mod support;

use adv_cpu::{CompileStrategy, CpuState, EXEC, READ, Region, RunOutcome, WRITE};
use iced_x86::code_asm::*;
use iced_x86::{Decoder, DecoderOptions, Mnemonic, OpKind};
use support::Executor;

const CODE: u32 = 0x10000;
const STOP: u32 = 0x18000;
const DATA: u32 = 0x20000;
const STACK: u32 = 0x2f800;

fn code(build: impl FnOnce(&mut CodeAssembler)) -> Vec<u8> {
    let mut assembler = CodeAssembler::new(32).unwrap();
    build(&mut assembler);
    assembler.assemble(u64::from(CODE)).unwrap()
}

fn arithmetic_oracle(bytes: &[u8]) -> Vec<u8> {
    let mut result = bytes.to_vec();
    for instruction in Decoder::with_ip(32, bytes, u64::from(CODE), DecoderOptions::NONE) {
        if instruction.has_lock_prefix() {
            let start = (instruction.ip32() - CODE) as usize;
            let lock = bytes[start..start + instruction.len()]
                .iter()
                .position(|&byte| byte == 0xf0)
                .unwrap();
            // Unicorn 2.1.5 miscomputes arithmetic flags for LOCK NEG. In this
            // single-thread arithmetic oracle, replace LOCK with a leading NOP
            // while retaining operand-size prefixes and all original addresses.
            // The JIT and interpreter still execute the actual LOCK instruction.
            result[start..=start + lock].rotate_right(1);
            result[start] = 0x90;
        }
    }
    result
}

fn machine(bytes: &[u8]) -> Executor {
    let mut machine = Executor::new(CODE, 0x20000).unwrap();
    machine.protect(CODE, 0x20000, READ | WRITE).unwrap();
    machine.write(CODE, bytes).unwrap();
    machine.protect(CODE, 4096, READ | EXEC).unwrap();
    machine.protect(STOP, 4096, READ | EXEC).unwrap();
    machine
}

fn compile(machine: &mut Executor, bytes: &[u8], strategy: CompileStrategy, entries: &[u32]) {
    let mut region = Region::new(CODE, bytes.len());
    region.compile_strategy = strategy;
    region.entries = entries.to_vec();
    machine.add_region(region).unwrap();
}

fn reset(machine: &mut Executor, accumulator: u32, flags: u32, address: u32) {
    machine.state = CpuState {
        regs: [
            accumulator,
            0x1357_2468,
            0x9876_5432,
            0xabcd_4321,
            STACK,
            DATA,
            address,
            DATA + 64,
        ],
        eflags: flags,
        eip: CODE,
        ..CpuState::default()
    };
    machine.write(STACK, &STOP.to_le_bytes()).unwrap();
}

fn compare(jit: &Executor, reference: &Executor, context: &str) {
    assert_eq!(jit.state.regs, reference.state.regs, "{context}");
    // NEG defines CF/PF/AF/ZF/SF/OF and preserves all other EFLAGS bits.
    assert_eq!(jit.state.eflags, reference.state.eflags, "{context}");
    assert_eq!(jit.state.eip, reference.state.eip);
    assert_eq!(jit.state.xmm, reference.state.xmm);
    assert_eq!(jit.state.mmx, reference.state.mmx);
    assert_eq!(jit.state.x87, reference.state.x87);
    assert_eq!(jit.state.mxcsr, reference.state.mxcsr);
    assert_eq!(
        jit.memory().read(DATA, 8192).unwrap(),
        reference.memory().read(DATA, 8192).unwrap()
    );
    assert_eq!(
        jit.memory().read(DATA + 8192, 4).unwrap(),
        reference.memory().read(DATA + 8192, 4).unwrap()
    );
}

fn finished(jit: &mut Executor, interpreter: &mut Executor, unicorn: &mut Executor, context: &str) {
    assert_eq!(jit.run(STOP, 10000).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(
        interpreter.run(STOP, 10000).unwrap(),
        RunOutcome::ReachedStop
    );
    assert_eq!(
        unicorn.run_unicorn(STOP, 10000).unwrap(),
        RunOutcome::ReachedStop
    );
    compare(jit, interpreter, &format!("{context}, interpreter"));
    compare(jit, unicorn, &format!("{context}, Unicorn"));
}

#[test]
fn byte_neg_exhausts_all_values_and_word_neg_covers_flag_boundaries_natively() {
    let words = [
        0, 1, 2, 7, 8, 0x0f, 0x10, 0x11, 0x7f, 0x80, 0xff, 0x100, 0x101, 0x7ffe, 0x7fff, 0x8000,
        0x8001, 0x80f0, 0xff00, 0xff0f, 0xff10, 0xfff0, 0xfffe, 0xffff,
    ];
    let bytes: Vec<u32> = (0..=255).collect();
    for strategy in [CompileStrategy::Fast, CompileStrategy::Speed] {
        for form in 0..7 {
            let code = code(|assembler| {
                match form {
                    0 => assembler.neg(al),
                    1 => assembler.neg(ah),
                    2 => assembler.neg(ax),
                    3 => assembler.neg(byte_ptr(esi)),
                    4 => assembler.neg(word_ptr(esi)),
                    5 => assembler.lock().neg(byte_ptr(esi)),
                    _ => assembler.lock().neg(word_ptr(esi)),
                }
                .unwrap();
                // Consume lazy arithmetic flags before the final state export.
                assembler.setb(bl).unwrap();
                assembler.seto(bh).unwrap();
                assembler.sete(dl).unwrap();
                assembler.sets(dh).unwrap();
                assembler.setp(cl).unwrap();
                assembler.pushfd().unwrap();
                assembler.pop(edi).unwrap();
                assembler.ret().unwrap();
            });
            let mut jit = machine(&code);
            compile(&mut jit, &code, strategy, &[]);
            let mut interpreter = machine(&code);
            let mut unicorn = machine(&arithmetic_oracle(&code));
            let values: &[u32] = if matches!(form, 2 | 4 | 6) {
                &words
            } else {
                &bytes
            };
            for &value in values {
                for flags in [2, 0xed7] {
                    let accumulator = match form {
                        0 => 0x96bc_5200 | value,
                        1 => 0x96bc_0084 | (value << 8),
                        2 => 0x96bc_0000 | value,
                        _ => 0x96bc_5284,
                    };
                    let width = if matches!(form, 2 | 4 | 6) { 2 } else { 1 };
                    let mut memory = [0x5a, 0xa5, 0xb6, 0x6b];
                    memory[..width].copy_from_slice(&value.to_le_bytes()[..width]);
                    for machine in [&mut jit, &mut interpreter, &mut unicorn] {
                        reset(machine, accumulator, flags, DATA);
                        machine.write(DATA, &memory).unwrap();
                    }
                    let context =
                        format!("form {form}, value {value:#x}, flags {flags:#x}, {strategy:?}");
                    finished(&mut jit, &mut interpreter, &mut unicorn, &context);
                    let result = 0u32.wrapping_sub(value);
                    let expected = match form {
                        0 => (accumulator & !0xff) | (result & 0xff),
                        1 => (accumulator & !0xff00) | ((result & 0xff) << 8),
                        2 => (accumulator & !0xffff) | (result & 0xffff),
                        _ => accumulator,
                    };
                    assert_eq!(jit.state.regs[0], expected, "form {form}, value {value:#x}");
                    assert_eq!(jit.state.regs[7], jit.state.eflags);
                    if form >= 3 {
                        memory[..width].copy_from_slice(&result.to_le_bytes()[..width]);
                        assert_eq!(jit.memory().read(DATA, 4).unwrap(), memory);
                    }
                }
            }
            support::x64_shape!(assert_eq!(
                jit.stats.interpreter_entries, 0,
                "form {form}, strategy {strategy:?}"
            ));
        }
    }
}

#[test]
fn narrow_neg_memory_faults_preserve_prefix_flags_and_atomic_writes_then_resume() {
    for strategy in [CompileStrategy::Fast, CompileStrategy::Speed] {
        for width in [1, 2] {
            for locked in [false, true] {
                let bytes = code(|assembler| {
                    assembler.neg(ah).unwrap();
                    assembler.mov(dword_ptr(DATA as i32 + 8192), eax).unwrap();
                    match (width, locked) {
                        (1, false) => assembler.neg(byte_ptr(esi)),
                        (1, true) => assembler.lock().neg(byte_ptr(esi)),
                        (_, false) => assembler.neg(word_ptr(esi)),
                        _ => assembler.lock().neg(word_ptr(esi)),
                    }
                    .unwrap();
                    assembler.mov(eax, 0x1234_5678).unwrap();
                    assembler.ret().unwrap();
                });
                let fault_pc = Decoder::with_ip(32, &bytes, u64::from(CODE), DecoderOptions::NONE)
                    .into_iter()
                    .find(|instruction| {
                        instruction.mnemonic() == Mnemonic::Neg
                            && instruction.op0_kind() == OpKind::Memory
                    })
                    .unwrap()
                    .ip32();
                for fault in 0..3 {
                    if width == 1 && fault == 2 {
                        continue;
                    }
                    let mut jit = machine(&bytes);
                    compile(&mut jit, &bytes, strategy, &[CODE, fault_pc]);
                    let mut interpreter = machine(&bytes);
                    let mut unicorn = machine(&arithmetic_oracle(&bytes));
                    let address = if fault == 2 { DATA + 4095 } else { DATA + 31 };
                    for machine in [&mut jit, &mut interpreter, &mut unicorn] {
                        reset(machine, 0x96bc_8100, 0xed7, address);
                        machine.write(address, &[0x01, 0x80][..width]).unwrap();
                        if fault == 2 {
                            machine.protect(DATA + 4096, 4096, READ).unwrap();
                        } else {
                            machine
                                .protect(DATA, 4096, if fault == 0 { READ } else { WRITE })
                                .unwrap();
                        }
                    }
                    let expected = if fault == 1 {
                        RunOutcome::ReadFault {
                            pc: fault_pc,
                            address,
                        }
                    } else {
                        RunOutcome::WriteFault {
                            pc: fault_pc,
                            address,
                        }
                    };
                    assert_eq!(jit.run(STOP, 10000).unwrap(), expected);
                    assert_eq!(interpreter.run(STOP, 10000).unwrap(), expected);
                    assert_eq!(
                        unicorn.run_unicorn(fault_pc, 10000).unwrap(),
                        RunOutcome::ReachedStop
                    );
                    // Unicorn may partially commit a cross-page RMW, and its
                    // WRITE-only x86 pages are readable. Compare the fault with
                    // the exact independently executed Unicorn prefix instead.
                    let context =
                        format!("width {width}, locked {locked}, fault {fault}, {strategy:?}");
                    compare(&jit, &interpreter, &format!("{context}, interpreter"));
                    compare(&jit, &unicorn, &format!("{context}, Unicorn"));
                    assert_eq!(jit.state.regs[0], 0x96bc_7f00);
                    assert_eq!(jit.state.eflags, 0x613);
                    assert_eq!(
                        jit.memory().read(DATA + 8192, 4).unwrap(),
                        0x96bc_7f00u32.to_le_bytes()
                    );
                    assert_eq!(
                        jit.memory().read(address, width).unwrap(),
                        &[0x01, 0x80][..width]
                    );
                    let fallbacks = jit.stats.interpreter_entries;
                    for machine in [&mut jit, &mut interpreter, &mut unicorn] {
                        machine.protect(DATA, 8192, READ | WRITE).unwrap();
                    }
                    finished(&mut jit, &mut interpreter, &mut unicorn, &context);
                    support::x64_shape!(assert_eq!(jit.stats.interpreter_entries, fallbacks));
                }
            }
        }
    }
}
