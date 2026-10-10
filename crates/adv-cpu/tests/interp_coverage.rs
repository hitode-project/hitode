mod support;

use adv_cpu::{CpuState, EXEC, READ, Region, RunOutcome, WRITE};
use iced_x86::{Mnemonic, code_asm::*};
use support::Executor;

const CODE: u32 = 0x10000;
const STOP: u32 = 0x1f000;
const DATA: u32 = 0x20000;
const STACK: u32 = 0x3f000;

fn assemble(emit: impl FnOnce(&mut CodeAssembler)) -> Vec<u8> {
    let mut a = CodeAssembler::new(32).unwrap();
    emit(&mut a);
    a.assemble(CODE.into()).unwrap()
}

fn machine(code: &[u8], native: bool) -> Executor {
    let mut m = Executor::new(CODE, 0x30000).unwrap();
    m.protect(CODE, 0x30000, READ | WRITE).unwrap();
    m.write(CODE, code).unwrap();
    m.protect(CODE, 4096, READ | EXEC).unwrap();
    m.protect(STOP, 4096, READ | EXEC).unwrap();
    m.state = CpuState {
        eip: CODE,
        eflags: 0x8d7,
        mxcsr: 0x9fc0,
        ..CpuState::default()
    };
    m.state.regs[4] = STACK;
    m.write(STACK, &STOP.to_le_bytes()).unwrap();
    if native {
        m.set_region(Region::new(CODE, code.len())).unwrap();
    }
    m
}

fn equal_state(a: &Executor, b: &Executor) {
    assert_eq!(a.state.regs, b.state.regs);
    assert_eq!(a.state.eip, b.state.eip);
    assert_eq!(a.state.eflags & 0xcd5, b.state.eflags & 0xcd5);
    assert_eq!(a.state.xmm, b.state.xmm);
    // Unicorn omits precision flags for packed conversions. Its control bits remain
    // an independent oracle; compare our complete flags to the interpreter.
    assert_eq!(a.state.mxcsr & !0x3f, b.state.mxcsr & !0x3f);
    assert_eq!(
        a.memory().read(DATA, 4096).unwrap(),
        b.memory().read(DATA, 4096).unwrap()
    );
}

fn compare(machines: &mut [Executor; 3], expected: RunOutcome) {
    assert_eq!(machines[0].run(STOP, 1_000_000).unwrap(), expected);
    assert_eq!(machines[1].run(STOP, 1_000_000).unwrap(), expected);
    assert_eq!(machines[2].run_unicorn(STOP, 1_000_000).unwrap(), expected);
    assert_eq!(machines[0].state.mxcsr, machines[1].state.mxcsr);
    equal_state(&machines[0], &machines[1]);
    equal_state(&machines[0], &machines[2]);
}

#[test]
fn unsupported_sequence_returns_to_the_native_continuation() {
    let code = assemble(|a| {
        a.pause().unwrap();
        a.pause().unwrap();
        a.mov(dword_ptr(DATA), eax).unwrap();
        a.adc(eax, 0).unwrap();
        a.mov(dword_ptr(DATA + 4), eax).unwrap();
        a.ret().unwrap();
    });
    for value in [0, 0x7f, 0x80, 0xff, 0xabcd_1280] {
        let mut machines = [
            machine(&code, true),
            machine(&code, false),
            machine(&code, false),
        ];
        for m in &mut machines {
            m.state.regs[0] = value;
        }
        compare(&mut machines, RunOutcome::ReachedStop);
        assert_eq!(machines[0].stats.interpreter_instructions, 2);
        assert!(machines[0].stats.native_entries > 0);
    }
}

#[test]
fn exact_conversion_hands_canonical_mmx_state_to_native_continuation() {
    let code = assemble(|a| {
        // Keep testing a statically unsupported form after CVTPS2PI gains
        // native emission; dynamic guard exits have a different entry shape.
        a.cvtpd2pi(mm0, xmm0).unwrap();
        a.movq(qword_ptr(DATA), mm0).unwrap();
        a.emms().unwrap();
        a.mov(eax, 42).unwrap();
        a.ret().unwrap();
    });
    for rounding in [0, 0x2000, 0x4000, 0x6000] {
        let mut machines = [
            machine(&code, true),
            machine(&code, false),
            machine(&code, false),
        ];
        for m in &mut machines {
            m.state.mxcsr = 0x1f80 | rounding;
            m.state.xmm[0][..8].copy_from_slice(&1.75f64.to_le_bytes());
            m.state.xmm[0][8..].copy_from_slice(&(-2.25f64).to_le_bytes());
        }
        compare(&mut machines, RunOutcome::ReachedStop);
        assert_eq!(machines[0].stats.interpreter_instructions, 1);
        assert_ne!(machines[0].state.mxcsr & 0x20, 0);
    }
}

#[test]
fn unsupported_store_fault_does_not_enter_its_continuation() {
    let code = assemble(|a| {
        a.pop(dword_ptr(DATA)).unwrap();
        a.inc(eax).unwrap();
        a.jmp(STOP as u64).unwrap();
    });
    let mut machines = [
        machine(&code, true),
        machine(&code, false),
        machine(&code, false),
    ];
    for m in &mut machines {
        m.protect(DATA, 4096, READ).unwrap();
    }
    compare(
        &mut machines,
        RunOutcome::WriteFault {
            pc: CODE,
            address: DATA,
        },
    );
    assert_eq!(machines[0].state.regs[4], STACK);
    assert_eq!(machines[0].state.regs[0], 0);
}

#[test]
fn executable_fallback_write_revalidates_the_continuation() {
    // POP absolute has a six-byte encoding; obtain the replacement from our
    // assembler too, so no sample bytes or instruction addresses are involved.
    let prefix = assemble(|a| {
        a.pop(dword_ptr(CODE)).unwrap();
    });
    let next = CODE + prefix.len() as u32;
    let code = assemble(|a| {
        a.pop(dword_ptr(next)).unwrap();
        for _ in 0..4 {
            a.inc(eax).unwrap();
        }
        a.jmp(STOP as u64).unwrap();
    });
    let replacement = assemble(|a| {
        for _ in 0..4 {
            a.nop().unwrap();
        }
    });
    let mut machines = [
        machine(&code, true),
        machine(&code, false),
        machine(&code, false),
    ];
    for m in &mut machines {
        m.protect(CODE, 4096, READ | WRITE | EXEC).unwrap();
        m.write(STACK, &replacement).unwrap();
    }
    compare(&mut machines, RunOutcome::ReachedStop);
    assert_eq!(machines[0].state.regs[0], 0);
    assert!(machines[0].stats.invalidations > 0);
}

#[test]
fn xmm_word_extraction_masks_all_immediates_and_zero_extends() {
    let code = assemble(|a| {
        for immediate in 0..=255 {
            a.pextrw(eax, xmm7, immediate).unwrap();
            a.mov(dword_ptr(DATA + immediate as u32 * 4), eax).unwrap();
        }
        a.ret().unwrap();
    });
    let mut machines = [
        machine(&code, true),
        machine(&code, false),
        machine(&code, false),
    ];
    for m in &mut machines {
        m.state.regs[0] = u32::MAX;
        m.state.xmm[7] = [
            0, 0, 1, 0, 0xff, 0x7f, 0, 0x80, 0xff, 0xff, 5, 0x80, 6, 0x80, 7, 0x80,
        ];
    }
    compare(&mut machines, RunOutcome::ReachedStop);
    assert_eq!(
        machines[0].stats.cliffs.fallbacks.get(&Mnemonic::Pextrw),
        None
    );
    assert_eq!(machines[0].stats.interpreter_instructions, 0);
}

#[test]
fn xmm_quadword_shifts_use_the_whole_low_count_and_preserve_high_count_bits() {
    for left in [false, true] {
        for source in 0..3 {
            let code = assemble(|a| {
                match (left, source) {
                    (true, 0) => a.psllq(xmm0, xmm1),
                    (true, 1) => a.psllq(xmm0, xmmword_ptr(DATA)),
                    (true, _) => a.psllq(xmm0, xmm0),
                    (false, 0) => a.psrlq(xmm0, xmm1),
                    (false, 1) => a.psrlq(xmm0, xmmword_ptr(DATA)),
                    (false, _) => a.psrlq(xmm0, xmm0),
                }
                .unwrap();
                a.ret().unwrap();
            });
            for count in [
                0,
                1,
                31,
                32,
                63,
                64,
                65,
                255,
                256,
                1u64 << 32,
                1 << 63,
                u64::MAX,
            ] {
                let mut machines = [
                    machine(&code, true),
                    machine(&code, false),
                    machine(&code, false),
                ];
                for m in &mut machines {
                    m.state.xmm[0] = [
                        0xff, 0, 0xab, 0x98, 0x76, 0x54, 0x32, 0x80, 0, 1, 0, 0, 0, 0, 0, 0xff,
                    ];
                    m.state.xmm[1][..8].copy_from_slice(&count.to_le_bytes());
                    m.state.xmm[1][8..].fill(0xff);
                    if source == 2 {
                        m.state.xmm[0] = m.state.xmm[1];
                    }
                    let count_bytes = m.state.xmm[1];
                    m.write(DATA, &count_bytes).unwrap();
                }
                compare(&mut machines, RunOutcome::ReachedStop);
                assert_eq!(machines[0].stats.interpreter_instructions, 0);
            }
        }
    }
}

#[test]
fn xmm_quadword_immediate_shifts_zero_at_lane_width() {
    for count in [0, 1, 31, 32, 63, 64, 65, 127, 255] {
        let code = assemble(|a| {
            a.psllq(xmm0, count).unwrap();
            a.psrlq(xmm1, count).unwrap();
            a.ret().unwrap();
        });
        let mut machines = [
            machine(&code, true),
            machine(&code, false),
            machine(&code, false),
        ];
        for m in &mut machines {
            m.state.xmm[0].fill(0xff);
            m.state.xmm[1].fill(0x81);
        }
        compare(&mut machines, RunOutcome::ReachedStop);
        assert_eq!(machines[0].stats.interpreter_instructions, 0);
    }
}

#[test]
fn xmm_quadword_memory_shift_preserves_destination_on_alignment_or_read_fault() {
    for offset in [1, 4096] {
        let code = assemble(|a| {
            a.psllq(xmm0, xmmword_ptr(DATA + offset)).unwrap();
            a.ret().unwrap();
        });
        let mut machines = [
            machine(&code, true),
            machine(&code, false),
            machine(&code, false),
        ];
        for m in &mut machines {
            m.state.xmm[0].fill(0x81);
            m.protect(DATA + 4096, 4096, 0).unwrap();
        }
        // Unicorn does not model every legacy-SSE alignment trap. The local
        // interpreter remains the precise reference for that guarded path.
        let native = machines[0].run(STOP, 100).unwrap();
        let reference = machines[1].run(STOP, 100).unwrap();
        assert_eq!(native, reference);
        if offset == 4096 {
            assert_eq!(machines[2].run_unicorn(STOP, 100).unwrap(), reference);
            equal_state(&machines[0], &machines[2]);
        }
        equal_state(&machines[0], &machines[1]);
        assert_eq!(machines[0].state.xmm[0], [0x81; 16]);
        assert_eq!(machines[0].state.eip, CODE);
    }
}
