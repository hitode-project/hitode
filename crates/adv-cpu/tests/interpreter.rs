use adv_cpu::interpreter::{Interpreter, Step};
use adv_cpu::{CpuState, EXEC, Memory, READ, WRITE};
use iced_x86::code_asm::*;
use unicorn_engine::{
    RegisterX86 as R, Unicorn,
    unicorn_const::{Arch, Mode, Prot},
};

const CODE: u32 = 0x10000;
const DATA: u32 = 0x20000;
const STACK: u32 = 0x2f800;
const ARITH: u32 = 0x8d5;
const REGS: [R; 8] = [
    R::EAX,
    R::ECX,
    R::EDX,
    R::EBX,
    R::ESP,
    R::EBP,
    R::ESI,
    R::EDI,
];

fn code(build: impl FnOnce(&mut CodeAssembler)) -> Vec<u8> {
    let mut assembler = CodeAssembler::new(32).unwrap();
    build(&mut assembler);
    assembler.assemble(u64::from(CODE)).unwrap()
}

fn memory(bytes: &[u8]) -> Memory {
    let mut memory = Memory::new(0, 0x30000).unwrap();
    memory.protect(0, 0x30000, READ | WRITE).unwrap();
    memory.write(CODE, bytes).unwrap();
    memory.protect(CODE, 4096, READ | EXEC).unwrap();
    memory
}

fn state() -> CpuState {
    CpuState {
        regs: [
            0x1234_5678,
            0x89ab_cdef,
            0xabcd_3210,
            0x7788_9900,
            STACK,
            STACK + 64,
            DATA + 32,
            DATA + 4094,
        ],
        eip: CODE,
        eflags: 0xed7,
        xmm: std::array::from_fn(|r| std::array::from_fn(|b| (r * 31 + b * 7) as u8)),
        mmx: std::array::from_fn(|r| 0x9876_5432_10ab_cdefu64.rotate_left(r as u32 * 3)),
        mxcsr: 0x3f80,
        fuel: 59,
        fault_addr: 0x1234_5678,
        ..CpuState::default()
    }
}

struct Reference {
    uc: Unicorn<'static, ()>,
    memory: Memory,
    interpreter: Interpreter,
}

impl Reference {
    fn new(bytes: &[u8]) -> Self {
        let mut uc = Unicorn::new(Arch::X86, Mode::MODE_32).unwrap();
        uc.mem_map(0, 0x30000, Prot::ALL).unwrap();
        uc.mem_write(u64::from(CODE), bytes).unwrap();
        Self {
            uc,
            memory: memory(bytes),
            interpreter: Interpreter::new(),
        }
    }
    fn put(&mut self, address: u32, bytes: &[u8]) {
        self.memory.write(address, bytes).unwrap();
        self.uc.mem_write(u64::from(address), bytes).unwrap();
    }
    fn check(&mut self, initial: &CpuState, flags: u32) -> CpuState {
        for (r, value) in REGS.into_iter().zip(initial.regs) {
            self.uc.reg_write(r, u64::from(value)).unwrap();
        }
        self.uc
            .reg_write(R::EFLAGS, u64::from(initial.eflags))
            .unwrap();
        self.uc.reg_write(R::EIP, u64::from(initial.eip)).unwrap();
        let mut actual = initial.clone();
        assert_eq!(
            self.interpreter.step(&mut actual, &mut self.memory),
            Step::Executed
        );
        self.uc
            .emu_start(u64::from(initial.eip), 0xffff_ffff, 0, 1)
            .unwrap();
        for (index, r) in REGS.into_iter().enumerate() {
            assert_eq!(
                actual.regs[index],
                self.uc.reg_read(r).unwrap() as u32,
                "{r:?}, input {:?}",
                initial.regs
            );
        }
        assert_eq!(actual.eip, self.uc.reg_read(R::EIP).unwrap() as u32);
        assert_eq!(
            actual.eflags & flags,
            self.uc.reg_read(R::EFLAGS).unwrap() as u32 & flags,
            "input {:?}, EFLAGS {:#x}",
            initial.regs,
            initial.eflags
        );
        assert_eq!(actual.xmm, initial.xmm);
        assert_eq!(actual.mmx, initial.mmx);
        assert_eq!(actual.x87, initial.x87);
        assert_eq!(actual.mxcsr, initial.mxcsr);
        assert_eq!(actual.mmx_mode, initial.mmx_mode);
        assert_eq!(actual.fuel, initial.fuel);
        assert_eq!(actual.fault_addr, initial.fault_addr);
        for (at, len) in [(DATA, 96), (DATA + 4088, 16), (STACK - 64, 160)] {
            assert_eq!(
                self.memory.read(at, len).unwrap(),
                self.uc.mem_read_as_vec(u64::from(at), len).unwrap()
            );
        }
        actual
    }
}

#[test]
fn integer_alu_all_widths_and_carry_inputs_match_unicorn() {
    let values = [
        0,
        1,
        0x0f,
        0x7f,
        0xff,
        0x7fff,
        0xffff,
        0x7fff_ffff,
        u32::MAX,
    ];
    for width in [1, 2, 4] {
        for operation in 0..10 {
            let bytes = code(|a| {
                macro_rules! form {
                    ($dst:expr, $src:expr) => {{
                        match operation {
                            0 => a.add($dst, $src),
                            1 => a.adc($dst, $src),
                            2 => a.sub($dst, $src),
                            3 => a.sbb($dst, $src),
                            4 => a.cmp($dst, $src),
                            5 => a.and($dst, $src),
                            6 => a.or($dst, $src),
                            7 => a.xor($dst, $src),
                            8 => a.inc($dst),
                            _ => a.neg($dst),
                        }
                        .unwrap();
                    }};
                }
                match width {
                    1 => form!(ah, cl),
                    2 => form!(ax, cx),
                    _ => form!(eax, ecx),
                }
            });
            let mut reference = Reference::new(&bytes);
            for (index, value) in values.into_iter().enumerate() {
                for carry in [0, 1] {
                    let mut initial = state();
                    initial.regs[0] = value;
                    initial.regs[1] = values[(index * 5 + 3) % values.len()];
                    initial.eflags = (initial.eflags & !1) | carry;
                    reference.check(
                        &initial,
                        if (5..=7).contains(&operation) {
                            !0x10
                        } else {
                            u32::MAX
                        },
                    );
                }
            }
        }
    }
}

#[test]
fn mov_extensions_lea_and_exchange_preserve_aliases_and_flags() {
    for form in 0..9 {
        let bytes = code(|a| {
            match form {
                0 => a.mov(ah, cl),
                1 => a.mov(ax, cx),
                2 => a.movzx(eax, ah),
                3 => a.movsx(eax, cl),
                4 => a.movsx(eax, word_ptr(edi)),
                5 => a.movzx(ax, byte_ptr(edi)),
                6 => a.lea(eax, dword_ptr(ebx + ecx * 4 - 127)),
                7 => a.xchg(dword_ptr(esp), esp),
                _ => a.xchg(ah, cl),
            }
            .unwrap();
        });
        let mut reference = Reference::new(&bytes);
        for value in [0, 0x7f, 0x80, 0xff, 0x7fff, 0x8000, 0xffff, u32::MAX] {
            let mut initial = state();
            initial.regs[1] = value;
            reference.put(DATA + 4094, &value.to_le_bytes());
            reference.put(STACK, &DATA.to_le_bytes());
            reference.check(&initial, u32::MAX);
        }
    }
}

#[test]
fn shifts_and_rotates_match_unicorn_on_defined_flags() {
    for width in [1, 2, 4] {
        for operation in 0..7 {
            let bytes = code(|a| {
                macro_rules! form {
                    ($dst:expr) => {{
                        match operation {
                            0 => a.shl($dst, cl),
                            1 => a.shr($dst, cl),
                            2 => a.sar($dst, cl),
                            3 => a.rol($dst, cl),
                            4 => a.ror($dst, cl),
                            5 => a.rcl($dst, cl),
                            _ => a.rcr($dst, cl),
                        }
                        .unwrap();
                    }};
                }
                match width {
                    1 => form!(al),
                    2 => form!(ax),
                    _ => form!(eax),
                }
            });
            let mut reference = Reference::new(&bytes);
            for count in [0u32, 1, 7, 8, 9, 15, 16, 17, 31, 32, 33, 255] {
                for carry in [0, 1] {
                    let mut initial = state();
                    initial.regs[0] = 0x8192_83f1;
                    initial.regs[1] = count;
                    initial.eflags = (initial.eflags & !1) | carry;
                    let masked = count & 31;
                    let flags = if masked == 0 {
                        u32::MAX
                    } else {
                        let mut flags = !0x10;
                        if masked != 1 {
                            flags &= !(1 << 11);
                        }
                        if operation < 2 && masked >= width * 8 {
                            flags &= !1;
                        }
                        flags
                    };
                    reference.check(&initial, flags);
                }
            }
        }
    }
}

#[test]
fn multiply_and_divide_all_widths_match_reference_without_flag_assumptions() {
    for width in [1, 2, 4] {
        for operation in 0..4 {
            let bytes = code(|a| {
                macro_rules! form {
                    ($src:expr) => {{
                        match operation {
                            0 => a.mul($src),
                            1 => a.imul($src),
                            2 => a.div($src),
                            _ => a.idiv($src),
                        }
                        .unwrap();
                    }};
                }
                match width {
                    1 => form!(cl),
                    2 => form!(cx),
                    _ => form!(ecx),
                }
            });
            let mut reference = Reference::new(&bytes);
            for (low, high, divisor) in [
                (12u32, 0u32, 3u32),
                (127, 0, 7),
                (15, 0, 127),
                (0xffff_ffff, 0xffff_ffff, 0xffff_fff9),
            ] {
                if operation == 2 && high != 0 {
                    continue;
                }
                let mut initial = state();
                initial.regs[0] = low;
                initial.regs[2] = high;
                initial.regs[1] = divisor;
                reference.check(
                    &initial,
                    if operation < 2 {
                        !ARITH | 1 | (1 << 11)
                    } else {
                        !ARITH
                    },
                );
            }
        }
    }
    for form in 0..2 {
        let bytes = code(|a| {
            if form == 0 {
                a.imul_2(eax, ecx).unwrap();
            } else {
                a.imul_3(eax, dword_ptr(edi), -127).unwrap();
            }
        });
        let mut reference = Reference::new(&bytes);
        for (left, right) in [
            (0, 0),
            (1, -1i32 as u32),
            (i32::MAX as u32, 127),
            (0x8000_0000, u32::MAX),
        ] {
            let mut initial = state();
            initial.regs[0] = left;
            initial.regs[1] = right;
            reference.put(DATA + 4094, &right.to_le_bytes());
            reference.check(&initial, !ARITH | 1 | (1 << 11));
        }
    }
}

#[test]
fn stack_call_return_leave_and_esp_relative_pop_match_reference() {
    for form in 0..7 {
        let bytes = code(|a| {
            match form {
                0 => a.push(esp),
                1 => a.push(dword_ptr(esp)),
                2 => a.pop(esp),
                3 => a.pop(dword_ptr(esp + 4)),
                4 => a.call(dword_ptr(esp)),
                5 => a.ret_1(12u32),
                _ => a.leave(),
            }
            .unwrap();
        });
        let mut reference = Reference::new(&bytes);
        for offset in [0, 1, 3] {
            let mut initial = state();
            initial.regs[4] = STACK + offset;
            initial.regs[5] = STACK + 64 + offset;
            reference.put(STACK + offset, &(CODE + 128).to_le_bytes());
            reference.put(STACK + 64 + offset, &0x8765_4321u32.to_le_bytes());
            reference.check(&initial, u32::MAX);
        }
    }
}

#[test]
fn conditional_branches_sets_and_moves_match_every_condition_flag_combination() {
    for form in 0..3 {
        for condition in 0..16 {
            let bytes = code(|a| {
                let target = u64::from(CODE + 128);
                macro_rules! forms {
                    ($j:ident, $s:ident, $c:ident) => {{
                        match form {
                            0 => a.$j(target),
                            1 => a.$s(ah),
                            _ => a.$c(eax, dword_ptr(edi)),
                        }
                        .unwrap();
                    }};
                }
                match condition {
                    0 => forms!(jo, seto, cmovo),
                    1 => forms!(jno, setno, cmovno),
                    2 => forms!(jb, setb, cmovb),
                    3 => forms!(jae, setae, cmovae),
                    4 => forms!(je, sete, cmove),
                    5 => forms!(jne, setne, cmovne),
                    6 => forms!(jbe, setbe, cmovbe),
                    7 => forms!(ja, seta, cmova),
                    8 => forms!(js, sets, cmovs),
                    9 => forms!(jns, setns, cmovns),
                    10 => forms!(jp, setp, cmovp),
                    11 => forms!(jnp, setnp, cmovnp),
                    12 => forms!(jl, setl, cmovl),
                    13 => forms!(jge, setge, cmovge),
                    14 => forms!(jle, setle, cmovle),
                    _ => forms!(jg, setg, cmovg),
                }
            });
            let mut reference = Reference::new(&bytes);
            reference.put(DATA + 4094, &0x8765_4321u32.to_le_bytes());
            for combination in 0..32 {
                let mut initial = state();
                initial.eflags = 2;
                for (index, bit) in [1u32, 4, 64, 128, 2048].into_iter().enumerate() {
                    if combination & (1 << index) != 0 {
                        initial.eflags |= bit;
                    }
                }
                reference.check(&initial, u32::MAX);
            }
        }
    }
}

#[test]
fn faulting_instruction_does_not_commit_registers_flags_or_partial_writes() {
    for form in 0..6 {
        let bytes = code(|a| {
            match form {
                0 => a.add(dword_ptr(edi), eax),
                1 => a.xchg(dword_ptr(edi), edi),
                2 => a.mov(dword_ptr(edi), eax),
                3 => a.pop(dword_ptr(esp)),
                4 => a.leave(),
                _ => a.cmove(eax, dword_ptr(edi)),
            }
            .unwrap();
        });
        let mut memory = memory(&bytes);
        let mut interpreter = Interpreter::new();
        let mut initial = state();
        initial.eflags &= !64; // CMOV's false condition still requires its source.
        if form == 3 {
            initial.regs[4] = DATA + 4090;
        }
        if form == 4 {
            initial.regs[5] = DATA + 4094;
        }
        memory.write(DATA + 4088, &[0x5a; 16]).unwrap();
        memory.protect(DATA + 4096, 4096, 0).unwrap();
        let before = initial.clone();
        let bytes_before = memory.read(DATA + 4088, 16).unwrap().to_vec();
        let outcome = interpreter.step(&mut initial, &mut memory);
        assert!(
            matches!(
                outcome,
                Step::ReadFault { pc: CODE, .. } | Step::WriteFault { pc: CODE, .. }
            ),
            "{outcome:?}"
        );
        assert_eq!(initial, before);
        assert_eq!(memory.read(DATA + 4088, 16).unwrap(), bytes_before);
    }
    // Readable, read-only RMW source fails its write after calculating flags.
    let bytes = code(|a| {
        a.adc(dword_ptr(edi), eax).unwrap();
    });
    let mut memory = memory(&bytes);
    memory.protect(DATA, 8192, READ).unwrap();
    let mut state = state();
    let before = state.clone();
    assert_eq!(
        Interpreter::new().step(&mut state, &mut memory),
        Step::WriteFault {
            pc: CODE,
            address: DATA + 4094
        }
    );
    assert_eq!(state, before);
}

#[test]
fn division_traps_are_precise_for_zero_and_signed_or_unsigned_overflow() {
    for signed in [false, true] {
        for width in [1, 2, 4] {
            let bytes = code(|a| {
                macro_rules! form {
                    ($src:expr) => {
                        if signed {
                            a.idiv($src).unwrap();
                        } else {
                            a.div($src).unwrap();
                        }
                    };
                }
                match width {
                    1 => form!(cl),
                    2 => form!(cx),
                    _ => form!(ecx),
                }
            });
            let mut memory = memory(&bytes);
            for (low, high, divisor) in [
                (1u32, 0u32, 0u32),
                (0, 0x8000_0000, u32::MAX),
                (u32::MAX, u32::MAX, 1),
            ] {
                let mut initial = state();
                if !signed && divisor == u32::MAX {
                    initial.regs[0] = u32::MAX;
                    initial.regs[2] = u32::MAX;
                } else if divisor == u32::MAX && width == 1 {
                    initial.regs[0] = 0x8000;
                } else if divisor == u32::MAX && width == 2 {
                    initial.regs[0] = 0;
                    initial.regs[2] = 0x8000;
                } else {
                    initial.regs[0] = low;
                    initial.regs[2] = high;
                }
                initial.regs[1] = divisor;
                if signed && divisor == 1 {
                    initial.regs[0] = if width == 1 { 0x7fff } else { u32::MAX };
                    initial.regs[2] = if width == 2 { 0x7fff } else { 0x7fff_ffff };
                }
                let before = initial.clone();
                assert_eq!(
                    Interpreter::new().step(&mut initial, &mut memory),
                    Step::Trap {
                        pc: CODE,
                        vector: 0
                    }
                );
                assert_eq!(initial, before);
            }
        }
    }
    let bytes = code(|a| {
        a.idiv(dword_ptr(edi)).unwrap();
    });
    let mut memory = memory(&bytes);
    memory.protect(DATA + 4096, 4096, 0).unwrap();
    let mut initial = state();
    let before = initial.clone();
    assert_eq!(
        Interpreter::new().step(&mut initial, &mut memory),
        Step::ReadFault {
            pc: CODE,
            address: DATA + 4094
        }
    );
    assert_eq!(initial, before);
}

#[test]
fn cache_tracks_code_writes_permissions_tail_failures_and_different_memories() {
    let bytes = code(|a| {
        a.mov(eax, 1u32).unwrap();
    });
    let mut memory = memory(&bytes);
    let mut interpreter = Interpreter::with_capacity(2);
    let mut initial = state();
    assert_eq!(interpreter.step(&mut initial, &mut memory), Step::Executed);
    initial.eip = CODE;
    assert_eq!(interpreter.step(&mut initial, &mut memory), Step::Executed);
    assert_eq!(interpreter.decode_count(), 1);
    memory.write(CODE + 1, &2u32.to_le_bytes()).unwrap();
    initial.eip = CODE;
    assert_eq!(interpreter.step(&mut initial, &mut memory), Step::Executed);
    assert_eq!(initial.regs[0], 2);
    assert_eq!(interpreter.decode_count(), 2);
    memory.protect(CODE, 4096, READ).unwrap();
    initial.eip = CODE;
    let before = initial.clone();
    assert_eq!(
        interpreter.step(&mut initial, &mut memory),
        Step::FetchFault {
            pc: CODE,
            address: CODE
        }
    );
    assert_eq!(initial, before);
    memory.protect(CODE, 4096, EXEC).unwrap();
    assert_eq!(interpreter.step(&mut initial, &mut memory), Step::Executed);
    assert_eq!(
        initial.regs[0], 2,
        "execute-only code does not require READ"
    );
    let other_bytes = code(|a| {
        a.mov(eax, 73u32).unwrap();
    });
    let mut other = self::memory(&other_bytes);
    initial.eip = CODE;
    assert_eq!(interpreter.step(&mut initial, &mut other), Step::Executed);
    assert_eq!(initial.regs[0], 73);
    assert!(interpreter.cached_instructions() <= 2);

    // An incomplete instruction at the page tail negatively depends on the
    // following page. Granting EXEC there must retry the previous fetch fault.
    let tail = CODE + 4094;
    memory.write(tail, &[0xb8, 0x56]).unwrap();
    memory.write(tail + 2, &[0x34, 0x12, 0x00]).unwrap();
    initial.eip = tail;
    assert_eq!(
        interpreter.step(&mut initial, &mut memory),
        Step::FetchFault {
            pc: tail,
            address: tail + 2
        }
    );
    memory.protect(CODE + 4096, 4096, EXEC).unwrap();
    assert_eq!(interpreter.step(&mut initial, &mut memory), Step::Executed);
    assert_eq!(initial.regs[0], 0x123456);
    assert_eq!(initial.eip, tail + 5);
}

#[test]
fn self_modifying_store_invalidates_a_cached_target_and_completed_writes_do_not_replay() {
    let bytes = code(|a| {
        a.mov(dword_ptr(CODE + 33), 91u32).unwrap();
        a.mov(eax, dword_ptr(edi)).unwrap();
    });
    let mut memory = memory(&bytes);
    let target = code(|a| {
        a.mov(eax, 3u32).unwrap();
    });
    memory.write(CODE + 32, &target).unwrap();
    let mut interpreter = Interpreter::new();
    let mut initial = state();
    initial.eip = CODE + 32;
    assert_eq!(interpreter.step(&mut initial, &mut memory), Step::Executed);
    assert_eq!(initial.regs[0], 3);
    memory.protect(CODE, 4096, READ | WRITE | EXEC).unwrap();
    initial.eip = CODE;
    assert_eq!(interpreter.step(&mut initial, &mut memory), Step::Executed);
    let fault_pc = initial.eip;
    memory.protect(DATA + 4096, 4096, 0).unwrap();
    assert_eq!(
        interpreter.step(&mut initial, &mut memory),
        Step::ReadFault {
            pc: fault_pc,
            address: DATA + 4094
        }
    );
    memory.write(CODE + 33, &57u32.to_le_bytes()).unwrap();
    memory.protect(DATA + 4096, 4096, READ | WRITE).unwrap();
    assert_eq!(interpreter.step(&mut initial, &mut memory), Step::Executed);
    assert_eq!(
        memory.read(CODE + 33, 4).unwrap(),
        57u32.to_le_bytes(),
        "restarting a later fault must not replay the completed store"
    );
    initial.eip = CODE + 32;
    assert_eq!(interpreter.step(&mut initial, &mut memory), Step::Executed);
    assert_eq!(initial.regs[0], 57);
}

#[test]
fn software_trap_advances_eip_and_unsupported_or_ud2_preserve_state() {
    for (bytes, result, next) in [
        (
            vec![0xcd, 0x2e],
            Step::Trap {
                pc: CODE,
                vector: 0x2e,
            },
            CODE + 2,
        ),
        (
            vec![0xcc],
            Step::Trap {
                pc: CODE,
                vector: 3,
            },
            CODE + 1,
        ),
        (
            vec![0x0f, 0x0b],
            Step::Trap {
                pc: CODE,
                vector: 6,
            },
            CODE,
        ),
        (
            vec![0x0f, 0x01, 0xf9],
            Step::Unsupported {
                pc: CODE,
                code: iced_x86::Code::Rdtscp,
            },
            CODE,
        ),
    ] {
        let mut memory = memory(&bytes);
        let mut initial = state();
        let mut expected = initial.clone();
        expected.eip = next;
        assert_eq!(Interpreter::new().step(&mut initial, &mut memory), result);
        assert_eq!(initial, expected);
    }
}

#[test]
fn fs_and_word_addressing_resolve_before_guest_range_checks() {
    let mut memory = memory(&[0x64, 0xa1, 0x20, 0, 0, 0]); // mov eax,fs:[20h]
    memory
        .write(DATA + 32, &0x8765_4321u32.to_le_bytes())
        .unwrap();
    let mut initial = state();
    initial.fs_base = DATA;
    assert_eq!(
        Interpreter::new().step(&mut initial, &mut memory),
        Step::Executed
    );
    assert_eq!(initial.regs[0], 0x8765_4321);
    // mov eax,[bx+si+4], address size 16; truncate after displacement.
    memory.write(CODE, &[0x67, 0x8b, 0x40, 0x04]).unwrap();
    memory.write(2, &0x5566_7788u32.to_le_bytes()).unwrap();
    initial.eip = CODE;
    initial.regs[3] = 0x1234_fffe;
    initial.regs[6] = 0x8765_0000;
    assert_eq!(
        Interpreter::new().step(&mut initial, &mut memory),
        Step::Executed
    );
    assert_eq!(initial.regs[0], 0x5566_7788);
    memory.write(CODE, &[0x65, 0xa1, 0x20, 0, 0, 0]).unwrap(); // GS unsupported
    initial.eip = CODE;
    let before = initial.clone();
    assert!(matches!(
        Interpreter::new().step(&mut initial, &mut memory),
        Step::Unsupported { pc: CODE, .. }
    ));
    assert_eq!(initial, before);
}

#[test]
fn trap_flag_reports_debug_trap_after_the_instruction_commits() {
    let bytes = code(|a| {
        a.mov(dword_ptr(edi), eax).unwrap();
    });
    let mut memory = memory(&bytes);
    let mut initial = state();
    initial.eflags |= 1 << 8;
    let mut expected = initial.clone();
    expected.eip += bytes.len() as u32;
    assert_eq!(
        Interpreter::new().step(&mut initial, &mut memory),
        Step::Trap {
            pc: CODE,
            vector: 1
        }
    );
    assert_eq!(initial, expected);
    assert_eq!(
        memory.read(DATA + 4094, 4).unwrap(),
        expected.regs[0].to_le_bytes()
    );
}

#[test]
fn immediate_and_memory_integer_forms_match_deterministic_random_inputs() {
    let mut random = 0x7135_9bdfu32;
    for form in 0..12 {
        let bytes = code(|a| {
            match form {
                0 => a.adc(ah, -127i32),
                1 => a.sbb(ax, -128i32),
                2 => a.add(eax, -127i32),
                3 => a.sub(byte_ptr(edi), al),
                4 => a.adc(word_ptr(edi), ax),
                5 => a.sbb(dword_ptr(edi), eax),
                6 => a.and(eax, -127i32),
                7 => a.test(word_ptr(edi), ax),
                8 => a.dec(dword_ptr(edi)),
                9 => a.not(byte_ptr(edi)),
                10 => a.mov(byte_ptr(edi), ah),
                _ => a.add(ax, word_ptr(edi)),
            }
            .unwrap();
        });
        let mut reference = Reference::new(&bytes);
        for _ in 0..64 {
            let mut initial = state();
            for reg in &mut initial.regs[..4] {
                random ^= random << 13;
                random ^= random >> 17;
                random ^= random << 5;
                *reg = random;
            }
            initial.eflags = (initial.eflags & !ARITH) | (random & ARITH);
            reference.put(DATA + 4094, &initial.regs[3].to_le_bytes());
            reference.check(
                &initial,
                if matches!(form, 6 | 7) {
                    !0x10
                } else {
                    u32::MAX
                },
            );
        }
    }
}

#[test]
fn sign_extension_and_flag_helpers_match_reference_and_preserve_other_state() {
    for form in 0..11 {
        let bytes = code(|a| {
            match form {
                0 => a.cbw(),
                1 => a.cwde(),
                2 => a.cwd(),
                3 => a.cdq(),
                4 => a.clc(),
                5 => a.stc(),
                6 => a.cmc(),
                7 => a.cld(),
                8 => a.std(),
                9 => a.lahf(),
                _ => a.sahf(),
            }
            .unwrap();
        });
        let mut reference = Reference::new(&bytes);
        for value in [
            0,
            0x7f,
            0x80,
            0x7fff,
            0x8000,
            0x7fff_ffff,
            0x8000_0000,
            u32::MAX,
        ] {
            let mut initial = state();
            initial.regs[0] = value;
            reference.check(&initial, u32::MAX);
        }
    }
}

#[test]
fn lock_rmw_exchange_and_compare_exchange_match_reference() {
    for operation in 0..5 {
        let mut bytes = code(|a| {
            match operation {
                0 => a.adc(dword_ptr(edi), eax),
                1 => a.xadd(dword_ptr(edi), eax),
                2 => a.cmpxchg(dword_ptr(edi), ecx),
                3 => a.bts(dword_ptr(edi), ecx),
                _ => a.cmpxchg(ah, cl),
            }
            .unwrap();
        });
        if operation < 4 {
            bytes.insert(0, 0xf0);
        }
        let mut reference = Reference::new(&bytes);
        for (value, source) in [
            (0u32, 0u32),
            (u32::MAX, 1),
            (0x8000_0000, 17),
            (0x1234_5678, 0x1234_5678),
        ] {
            let mut initial = state();
            initial.regs[0] = value;
            initial.regs[1] = if operation == 3 { source & 31 } else { source };
            reference.put(DATA + 4094, &source.to_le_bytes());
            reference.check(&initial, if operation == 3 { !ARITH | 1 } else { u32::MAX });
        }
    }
    // Illegal LOCK on a register or read-only CMP must fault before mutation.
    for bytes in [vec![0xf0, 0x01, 0xc8], vec![0xf0, 0x39, 0x07]] {
        let mut memory = memory(&bytes);
        let mut initial = state();
        let before = initial.clone();
        assert_eq!(
            Interpreter::new().step(&mut initial, &mut memory),
            Step::InvalidInstruction { pc: CODE }
        );
        assert_eq!(initial, before);
    }
}

#[test]
fn bit_scans_strings_and_double_shifts_match_reference() {
    for form in 0..10 {
        let bytes = code(|a| {
            match form {
                0 => a.bsf(eax, ecx),
                1 => a.bsr(ax, cx),
                2 => a.bswap(eax),
                3 => a.bt(dword_ptr(edi), ecx),
                4 => a.btc(word_ptr(edi), cx),
                5 => a.btr(eax, ecx),
                6 => a.shld(eax, ebx, cl),
                7 => a.shrd(eax, ebx, cl),
                8 => a.shld(ax, bx, cl),
                _ => a.shrd(ax, bx, cl),
            }
            .unwrap();
        });
        let mut reference = Reference::new(&bytes);
        for value in [0u32, 1, 7, 15, 16, 31, 32, u32::MAX] {
            let mut initial = state();
            initial.regs[1] = value;
            reference.put(DATA + 4088, &[0x89; 16]);
            let flags = if form < 2 {
                !ARITH | 64
            } else if form == 2 {
                u32::MAX
            } else if form < 6 {
                !ARITH | 1
            } else if value & 31 == 0 {
                u32::MAX
            } else if form >= 8 && value & 31 > 16 {
                !ARITH
            } else {
                !0x10
                    & if value & 31 == 1 {
                        u32::MAX
                    } else {
                        !(1 << 11)
                    }
            };
            reference.check(&initial, flags);
        }
    }
}

#[test]
fn pushad_popad_word_stack_and_loop_forms_match_reference() {
    for form in 0..12 {
        let bytes = if form == 11 {
            vec![0x67, 0xe3, 0x40]
        } else {
            code(|a| {
                match form {
                    0 => a.pushad(),
                    1 => a.popad(),
                    2 => a.push(ax),
                    3 => a.pop(word_ptr(esp)),
                    4 => a.pushfd(),
                    5 => a.loop_(u64::from(CODE + 128)),
                    6 => a.loope(u64::from(CODE + 128)),
                    7 => a.loopne(u64::from(CODE + 128)),
                    8 => a.jecxz(u64::from(CODE + 128)),
                    9 => a.pusha(),
                    _ => a.popa(),
                }
                .unwrap();
            })
        };
        let mut reference = Reference::new(&bytes);
        for count in [0u32, 1, 2, 0xffff_0000] {
            let mut initial = state();
            initial.regs[1] = count;
            reference.put(STACK, &(0..32u8).collect::<Vec<_>>());
            reference.check(&initial, u32::MAX);
        }
    }
    let bytes = code(|a| {
        a.pushad().unwrap();
    });
    let mut memory = memory(&bytes);
    let mut initial = state();
    initial.regs[4] = DATA + 16;
    memory.protect(DATA - 4096, 4096, READ).unwrap();
    let before = initial.clone();
    let bytes_before = memory.read(DATA - 32, 64).unwrap().to_vec();
    assert!(matches!(
        Interpreter::new().step(&mut initial, &mut memory),
        Step::WriteFault { pc: CODE, .. }
    ));
    assert_eq!(initial, before);
    assert_eq!(memory.read(DATA - 32, 64).unwrap(), bytes_before);
}

#[test]
fn rep_chunk_progress_fault_and_resume_do_not_replay_elements() {
    use adv_cpu::interpreter::Execution;
    let bytes = [0xf3, 0xa4];
    let mut memory = memory(&bytes);
    memory.write(DATA, &[11, 22, 33, 44]).unwrap();
    let mut initial = state();
    initial.eflags &= !(1 << 10);
    initial.regs[6] = DATA;
    initial.regs[7] = DATA + 4094;
    initial.regs[1] = 4;
    let mut interpreter = Interpreter::new();
    memory.protect(DATA + 4096, 4096, READ).unwrap();
    assert_eq!(
        interpreter.run_chunk(&mut initial, &mut memory, 4),
        Execution {
            step: Step::WriteFault {
                pc: CODE,
                address: DATA + 4096
            },
            units: 2
        }
    );
    assert_eq!(initial.regs[1], 2);
    assert_eq!(initial.eip, CODE);
    assert_eq!(memory.read(DATA + 4094, 2).unwrap(), [11, 22]);
    memory.write(DATA, &[77, 88]).unwrap();
    memory.protect(DATA + 4096, 4096, READ | WRITE).unwrap();
    assert_eq!(interpreter.run_chunk(&mut initial, &mut memory, 1).units, 1);
    assert_eq!(initial.eip, CODE);
    assert_eq!(
        interpreter.run_chunk(&mut initial, &mut memory, 100).units,
        1
    );
    assert_eq!(initial.eip, CODE + 2);
    assert_eq!(initial.regs[1], 0);
    assert_eq!(memory.read(DATA + 4094, 4).unwrap(), [11, 22, 33, 44]);
}

#[test]
fn rep_comparison_fault_restores_original_flags_across_saved_contexts() {
    let mut memory = memory(&[0xf3, 0xa6]);
    memory.write(DATA, &[1, 2, 3, 4]).unwrap();
    memory.write(DATA + 4094, &[1, 2, 3, 4]).unwrap();
    let mut initial = state();
    initial.eflags &= !(1 << 10);
    initial.regs[6] = DATA;
    initial.regs[7] = DATA + 4094;
    initial.regs[1] = 4;
    let flags = initial.eflags;
    let mut interpreter = Interpreter::new();
    assert_eq!(interpreter.run_chunk(&mut initial, &mut memory, 1).units, 1);
    assert_eq!(initial.regs[1], 3);
    let saved = initial.clone();
    let mut other = state();
    other.eip = CODE + 2;
    memory.write(CODE + 2, &[0x90]).unwrap();
    assert_eq!(interpreter.step(&mut other, &mut memory), Step::Executed);
    initial = saved;
    memory.protect(DATA + 4096, 4096, 0).unwrap();
    let result = interpreter.run_chunk(&mut initial, &mut memory, 4);
    assert_eq!(result.units, 1);
    assert_eq!(
        result.step,
        Step::ReadFault {
            pc: CODE,
            address: DATA + 4096
        }
    );
    assert_eq!(initial.regs[1], 2);
    assert_eq!(initial.eflags, flags);
    assert!(
        initial.rep.is_none(),
        "fault exposes a fresh restart context"
    );
    memory.protect(DATA + 4096, 4096, READ | WRITE).unwrap();
    assert_eq!(interpreter.run_chunk(&mut initial, &mut memory, 4).units, 2);
    assert_eq!(initial.eip, CODE + 2);
    assert!(initial.rep.is_none());
}

#[test]
fn rep_fault_restart_accepts_handler_flags_as_the_new_instruction_input() {
    let mut memory = memory(&[0xf3, 0xae]);
    memory.write(DATA + 4095, &[0x11; 8194]).unwrap();
    memory.protect(DATA + 4096, 8192, 0).unwrap();
    let mut initial = state();
    initial.eflags = 0x8d7;
    initial.regs[0] = 0x11;
    initial.regs[1] = 8194;
    initial.regs[7] = DATA + 4095;
    let mut interpreter = Interpreter::new();
    assert_eq!(interpreter.run_chunk(&mut initial, &mut memory, 1).units, 1);
    assert!(initial.rep.is_some());
    let fault = interpreter.run_chunk(&mut initial, &mut memory, 256);
    assert_eq!(fault.units, 0);
    assert_eq!(
        fault.step,
        Step::ReadFault {
            pc: CODE,
            address: DATA + 4096
        }
    );
    assert_eq!(initial.eflags, 0x8d7);
    assert!(initial.rep.is_none());

    // Model an exception handler changing the saved EFLAGS, then returning to
    // the same PC with the remaining count and indices. No completed byte repeats.
    let handler_flags = 0x282;
    initial.eflags = handler_flags;
    memory.write(DATA + 4095, &[0x12]).unwrap();
    memory.protect(DATA + 4096, 4096, READ | WRITE).unwrap();
    for _ in 0..16 {
        let result = interpreter.run_chunk(&mut initial, &mut memory, 256);
        assert_eq!(result.step, Step::Executed);
        assert_eq!(result.units, 256);
        assert_eq!(initial.eflags, 0x246);
        assert!(initial.rep.is_some());
    }
    let fault = interpreter.run_chunk(&mut initial, &mut memory, 256);
    assert_eq!(fault.units, 0);
    assert_eq!(
        fault.step,
        Step::ReadFault {
            pc: CODE,
            address: DATA + 8192
        }
    );
    assert_eq!(initial.regs[1], 8194 - 4097);
    assert_eq!(initial.regs[7], DATA + 8192);
    assert_eq!(initial.eip, CODE);
    assert_eq!(initial.eflags, handler_flags);
    assert!(initial.rep.is_none());
}

#[test]
fn rep_debug_exception_restart_uses_the_handler_context_flags() {
    let mut memory = memory(&[0xf3, 0xae]);
    memory.write(DATA + 4095, &[0x11, 0x11]).unwrap();
    memory.protect(DATA + 4096, 4096, 0).unwrap();
    let mut initial = state();
    initial.eflags = 0x9d7; // TF is set for the first element.
    initial.regs[0] = 0x11;
    initial.regs[1] = 2;
    initial.regs[7] = DATA + 4095;
    let mut interpreter = Interpreter::new();
    let trap = interpreter.run_chunk(&mut initial, &mut memory, 256);
    assert_eq!(trap.units, 1);
    assert_eq!(
        trap.step,
        Step::Trap {
            pc: CODE,
            vector: 1
        }
    );
    assert_eq!(initial.eip, CODE);
    assert_eq!(initial.regs[1], 1);
    assert_eq!(initial.eflags, 0x146);
    assert!(initial.rep.is_none());
    initial.eflags = 0x282; // A handler clears TF and changes the saved flags.
    assert_eq!(
        interpreter.step(&mut initial, &mut memory),
        Step::ReadFault {
            pc: CODE,
            address: DATA + 4096
        }
    );
    assert_eq!(initial.eflags, 0x282);
    assert_eq!(initial.regs[1], 1);
    assert_eq!(initial.regs[7], DATA + 4096);
    assert!(initial.rep.is_none());
}

#[test]
fn all_string_widths_directions_and_repeat_conditions_match_full_unicorn_execution() {
    for kind in 0..5 {
        for width in [1u32, 2, 4] {
            for reverse in [false, true] {
                for repeat in [0u8, 0xf2, 0xf3] {
                    let mut bytes = Vec::new();
                    if repeat != 0 {
                        bytes.push(repeat);
                    }
                    if width == 2 {
                        bytes.push(0x66);
                    }
                    bytes.push(match kind {
                        0 => {
                            if width == 1 {
                                0xa4
                            } else {
                                0xa5
                            }
                        }
                        1 => {
                            if width == 1 {
                                0xaa
                            } else {
                                0xab
                            }
                        }
                        2 => {
                            if width == 1 {
                                0xa6
                            } else {
                                0xa7
                            }
                        }
                        3 => {
                            if width == 1 {
                                0xae
                            } else {
                                0xaf
                            }
                        }
                        _ => {
                            if width == 1 {
                                0xac
                            } else {
                                0xad
                            }
                        }
                    });
                    let mut reference = Reference::new(&bytes);
                    let mut initial = state();
                    initial.regs[1] = 4;
                    initial.regs[6] = DATA + 32 + if reverse { 3 * width } else { 0 };
                    initial.regs[7] = DATA + 4094 + if reverse { 3 * width } else { 0 };
                    initial.eflags =
                        (initial.eflags & !(1 << 10)) | if reverse { 1 << 10 } else { 0 };
                    let mut payload = [0u8; 32];
                    for (index, byte) in payload.iter_mut().enumerate() {
                        *byte = if index < 8 { 0x78 } else { 0x12 };
                    }
                    reference.put(DATA + 32, &payload);
                    reference.put(DATA + 4094, &payload);
                    for (register, value) in REGS.into_iter().zip(initial.regs) {
                        reference.uc.reg_write(register, u64::from(value)).unwrap();
                    }
                    reference
                        .uc
                        .reg_write(R::EFLAGS, u64::from(initial.eflags))
                        .unwrap();
                    let mut actual = initial.clone();
                    let executed =
                        reference
                            .interpreter
                            .run_chunk(&mut actual, &mut reference.memory, 256);
                    assert_eq!(executed.step, Step::Executed);
                    reference
                        .uc
                        .emu_start(u64::from(CODE), u64::from(CODE) + bytes.len() as u64, 0, 0)
                        .unwrap();
                    for (index, register) in REGS.into_iter().enumerate() {
                        assert_eq!(
                            actual.regs[index],
                            reference.uc.reg_read(register).unwrap() as u32,
                            "kind {kind}, width {width}, reverse {reverse}, repeat {repeat:#x}, {register:?}"
                        );
                    }
                    assert_eq!(actual.eip, reference.uc.reg_read(R::EIP).unwrap() as u32);
                    assert_eq!(
                        actual.eflags,
                        reference.uc.reg_read(R::EFLAGS).unwrap() as u32
                    );
                    assert_eq!(
                        reference.memory.read(DATA + 4094, 32).unwrap(),
                        reference
                            .uc
                            .mem_read_as_vec(u64::from(DATA + 4094), 32)
                            .unwrap()
                    );
                    assert_eq!(actual.xmm, initial.xmm);
                    assert_eq!(actual.x87, initial.x87);
                    assert!(actual.rep.is_none());
                }
            }
        }
    }
}

#[test]
fn rep_overlap_word_addresses_zero_count_and_chunk_budget_are_exact() {
    let mut memory = memory(&[0xf3, 0xa4]);
    let mut initial = state();
    initial.eflags &= !(1 << 10);
    initial.regs[6] = DATA;
    initial.regs[7] = DATA + 1;
    initial.regs[1] = 4;
    memory.write(DATA, &[1, 2, 3, 4, 5]).unwrap();
    let mut interpreter = Interpreter::new();
    assert_eq!(
        interpreter.run_chunk(&mut initial, &mut memory, 256).units,
        4
    );
    assert_eq!(memory.read(DATA, 5).unwrap(), [1, 1, 1, 1, 1]);
    initial.eip = CODE;
    initial.regs[1] = 600;
    initial.regs[6] = DATA + 32;
    initial.regs[7] = DATA + 4094;
    assert_eq!(
        interpreter.run_chunk(&mut initial, &mut memory, 1000).units,
        256
    );
    assert_eq!(initial.regs[1], 344);
    assert_eq!(initial.eip, CODE);
    initial.regs[1] = 0;
    memory.protect(DATA, 8192, 0).unwrap();
    let before = initial.clone();
    assert_eq!(
        interpreter.run_chunk(&mut initial, &mut memory, 256).units,
        1
    );
    assert_eq!(initial.regs, before.regs);
    assert_eq!(initial.eflags, before.eflags);
    assert_eq!(initial.eip, CODE + 2);
    memory.protect(0, 4096, READ | WRITE).unwrap();
    memory.write(CODE, &[0x67, 0xf3, 0xa4]).unwrap();
    memory.write(0xfffe, &[11, 22]).unwrap();
    initial.eip = CODE;
    initial.regs[1] = 0xabcd_0002;
    initial.regs[6] = 0x5678_fffe;
    initial.regs[7] = 0x9876_0010;
    assert_eq!(
        interpreter.run_chunk(&mut initial, &mut memory, 256).units,
        2
    );
    assert_eq!(initial.regs[1], 0xabcd_0000);
    assert_eq!(initial.regs[6], 0x5678_0000);
    assert_eq!(initial.regs[7], 0x9876_0012);
    assert_eq!(memory.read(0x10, 2).unwrap(), [11, 22]);
}

#[test]
fn popfd_in_user_mode_preserves_privileged_bits_and_defers_new_tf_until_next_instruction() {
    let mut memory = memory(&[0x9d, 0x90]);
    let mut initial = state();
    initial.eflags = 0x202;
    memory.write(STACK, &0x0020_31d7u32.to_le_bytes()).unwrap();
    let mut interpreter = Interpreter::new();
    assert_eq!(interpreter.step(&mut initial, &mut memory), Step::Executed);
    assert_eq!(
        initial.eflags & 0x3200,
        0x200,
        "guest CPL3 cannot change IOPL/IF"
    );
    assert_ne!(initial.eflags & 0x100, 0);
    assert_eq!(initial.regs[4], STACK + 4);
    assert_eq!(
        interpreter.step(&mut initial, &mut memory),
        Step::Trap {
            pc: CODE + 1,
            vector: 1
        }
    );
    assert_eq!(initial.eip, CODE + 2);
}
