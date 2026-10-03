use adv_jit::interpreter::{Interpreter, Step};
use adv_jit::{CpuState, EXEC, Memory, READ, WRITE, X87Register};
use iced_x86::code_asm::*;
const CODE: u32 = 0x10000;
const DATA: u32 = 0x20000;
fn code(f: impl FnOnce(&mut CodeAssembler)) -> Vec<u8> {
    let mut a = CodeAssembler::new(32).unwrap();
    f(&mut a);
    a.assemble(CODE as u64).unwrap()
}
fn run(bytes: &[u8], state: &mut CpuState, memory: &mut Memory) -> Step {
    state.eip = CODE;
    memory.write(CODE, bytes).unwrap();
    let mut interpreter = Interpreter::new();
    while state.eip < CODE + bytes.len() as u32 {
        let result = interpreter.step(state, memory);
        if result != Step::Executed {
            return result;
        }
    }
    Step::Executed
}
fn memory() -> Memory {
    let mut m = Memory::new(CODE, 0x20000).unwrap();
    m.protect(CODE, 0x20000, READ | WRITE).unwrap();
    m.protect(CODE, 4096, READ | EXEC).unwrap();
    m
}
#[test]
fn extended_precision_and_control_word_rounding_are_not_binary64_substitutes() {
    let bytes = code(|a| {
        a.fld(tbyte_ptr(DATA as i32)).unwrap();
        a.fld1().unwrap();
        a.faddp(st1, st0).unwrap();
        a.fstp(tbyte_ptr(DATA as i32 + 16)).unwrap();
    });
    for (control, expected) in [
        (0x37f, 0x8000_0000_0000_0001u64),
        (0x27f, 0x8000_0000_0000_0000),
    ] {
        let mut s = CpuState::default();
        s.x87.control = control;
        let mut m = memory();
        m.write(DATA, &X87Register::from_parts(1 << 63, 0x3fc0).0)
            .unwrap();
        assert_eq!(run(&bytes, &mut s, &mut m), Step::Executed);
        assert_eq!(
            m.read(DATA + 16, 10).unwrap(),
            X87Register::from_parts(expected, 0x3fff).0
        );
        assert_eq!(s.x87.tag, 0xffff);
    }
}
#[test]
fn x87_pcm_conversion_uses_each_guest_rounding_mode() {
    let bytes = code(|a| {
        a.fld(dword_ptr(DATA as i32)).unwrap();
        a.fmul(dword_ptr(DATA as i32 + 4)).unwrap();
        a.fistp(dword_ptr(DATA as i32 + 8)).unwrap();
    });
    for (mode, expected) in [(0, 2i32), (1, 1), (2, 2), (3, 1)] {
        let mut s = CpuState::default();
        s.x87.control = 0x27f | (mode << 10);
        let mut m = memory();
        m.write(DATA, &(3f32 / 65536f32).to_le_bytes()).unwrap();
        m.write(DATA + 4, &32768f32.to_le_bytes()).unwrap();
        assert_eq!(run(&bytes, &mut s, &mut m), Step::Executed);
        assert_eq!(m.read(DATA + 8, 4).unwrap(), expected.to_le_bytes());
        assert_ne!(s.x87.status & 0x20, 0);
    }
}
#[test]
fn unmasked_x87_exception_defers_mf_and_preserves_stack_until_wait() {
    let bytes = code(|a| {
        a.fld1().unwrap();
        a.fldz().unwrap();
        a.fdivp(st1, st0).unwrap();
        a.wait().unwrap();
    });
    let mut s = CpuState::default();
    s.x87.control &= !4;
    let mut m = memory();
    let result = run(&bytes, &mut s, &mut m);
    assert!(matches!(result, Step::Trap { vector: 16, .. }));
    assert_eq!(s.x87.top(), 6);
    assert_eq!(s.x87.status & 0x8084, 0x8084);
    assert_eq!(s.x87.regs[7], X87Register::from_parts(1 << 63, 0x3fff));
}
#[test]
fn extended_store_fault_does_not_pop_or_modify_any_memory() {
    let bytes = code(|a| {
        a.fstp(tbyte_ptr(DATA as i32 + 4091)).unwrap();
    });
    let mut s = CpuState::default();
    s.x87
        .push(X87Register::from_parts(0xc000_1234_5678_9abc, 0x7fff));
    s.refresh_mmx();
    let before = s.clone();
    let mut m = memory();
    m.protect(DATA + 4096, 4096, READ).unwrap();
    assert!(matches!(
        run(&bytes, &mut s, &mut m),
        Step::WriteFault { .. }
    ));
    assert_eq!(s.x87, before.x87);
    assert_eq!(m.read(DATA + 4091, 10).unwrap(), [0; 10]);
}

#[test]
fn x87_fault_rolls_back_pending_mmx_and_preserves_the_complete_cpu() {
    for store in [false, true] {
        let bytes = code(|a| {
            if store {
                a.fstp(dword_ptr(DATA as i32 + 4094)).unwrap();
            } else {
                a.fld(dword_ptr(DATA as i32 + 4094)).unwrap();
            }
        });
        let mut s = CpuState {
            regs: [0x1234_5678; 8],
            eip: CODE,
            eflags: 0x8d7,
            fs_base: 0x12000,
            fault_addr: 0x7654_3210,
            fuel: 91,
            xmm: [[0xa5; 16]; 8],
            mmx: [0x8123_4567_89ab_cdef; 8],
            mmx_mode: 1,
            mxcsr: 0x3f80,
            ..CpuState::default()
        };
        s.segments.gdt_base = 0x23400;
        let before = s.clone();
        let mut m = memory();
        m.protect(DATA + 4096, 4096, if store { READ } else { WRITE })
            .unwrap();
        let outcome = run(&bytes, &mut s, &mut m);
        assert_eq!(
            outcome,
            if store {
                Step::WriteFault {
                    pc: CODE,
                    address: DATA + 4096,
                }
            } else {
                Step::ReadFault {
                    pc: CODE,
                    address: DATA + 4096,
                }
            }
        );
        assert_eq!(s, before, "store={store}");
        assert_eq!(m.read(DATA + 4094, 4).unwrap(), [0; 4]);
    }
}

#[test]
fn unmasked_comparison_suppresses_pop_and_integer_flags_without_losing_metadata() {
    let bytes = code(|a| a.fcomip(st0, st1).unwrap());
    let mut s = CpuState {
        eip: CODE,
        eflags: 0x8d7,
        xmm: [[0x69; 16]; 8],
        mxcsr: 0x5f80,
        ..CpuState::default()
    };
    s.x87.control &= !1;
    s.x87.push(X87Register::from_parts(1 << 63, 0x3fff));
    s.x87
        .push(X87Register::from_parts(0xc000_0000_0000_1234, 0x7fff));
    s.refresh_mmx();
    let mut expected = s.clone();
    expected.eip = CODE + bytes.len() as u32;
    expected.x87.status |= 0x8081;
    expected.x87.ip = CODE;
    expected.x87.opcode = ((bytes[0] as u16 & 7) << 8) | bytes[1] as u16;
    let mut m = memory();
    assert_eq!(run(&bytes, &mut s, &mut m), Step::Executed);
    assert_eq!(s, expected);
}

#[test]
fn cached_prefixed_x87_metadata_is_redecoded_after_code_changes() {
    let mut m = memory();
    let mut interpreter = Interpreter::new();
    for (bytes, opcode) in [([0x66, 0xd9, 0xe8], 0x1e8), ([0x66, 0xd9, 0xee], 0x1ee)] {
        m.write(CODE, &bytes).unwrap();
        let mut s = CpuState {
            eip: CODE,
            ..CpuState::default()
        };
        assert_eq!(interpreter.step(&mut s, &mut m), Step::Executed);
        assert_eq!(s.x87.ip, CODE);
        assert_eq!(s.x87.opcode, opcode);
    }
    assert_eq!(interpreter.decode_count(), 2);
}

#[test]
fn scalar_sse_rounding_daz_ftz_and_unmasked_exceptions_commit_correctly() {
    let conversion = code(|a| {
        a.cvtss2si(eax, xmm0).unwrap();
    });
    for (mode, expected) in [(0, 2), (1, 1), (2, 2), (3, 1)] {
        let mut s = CpuState {
            mxcsr: 0x1f80 | (mode << 13),
            ..CpuState::default()
        };
        s.xmm[0][..4].copy_from_slice(&1.5f32.to_le_bytes());
        let mut m = memory();
        assert_eq!(run(&conversion, &mut s, &mut m), Step::Executed);
        assert_eq!(s.regs[0], expected);
        assert_eq!(s.mxcsr & 0x20, 0x20);
    }
    let multiply = code(|a| {
        a.mulss(xmm0, xmm1).unwrap();
    });
    let mut s = CpuState {
        mxcsr: 0x9fc0,
        ..CpuState::default()
    };
    s.xmm[0][..4].copy_from_slice(&f32::MIN_POSITIVE.to_le_bytes());
    s.xmm[1][..4].copy_from_slice(&0.5f32.to_le_bytes());
    let mut m = memory();
    assert_eq!(run(&multiply, &mut s, &mut m), Step::Executed);
    assert_eq!(&s.xmm[0][..4], &0f32.to_le_bytes());
    assert_eq!(s.mxcsr & 0x30, 0x30);
    let divide = code(|a| {
        a.divsd(xmm0, xmm1).unwrap();
    });
    let mut s = CpuState::default();
    s.mxcsr &= !(1 << 9);
    s.xmm[0][..8].copy_from_slice(&1f64.to_le_bytes());
    let before = s.xmm;
    assert!(matches!(
        run(&divide, &mut s, &mut m),
        Step::Trap { vector: 19, .. }
    ));
    assert_eq!(s.xmm, before);
    assert_eq!(s.mxcsr & 4, 4);
    assert_eq!(s.eip, CODE);
}

#[test]
fn fxsave_restore_retains_top_payloads_physical_tags_xmm_and_mxcsr() {
    let save = code(|a| {
        a.fxsave(ptr(DATA as i32)).unwrap();
    });
    let restore = code(|a| {
        a.fxrstor(ptr(DATA as i32)).unwrap();
    });
    let mut s = CpuState::default();
    s.x87.regs =
        std::array::from_fn(|n| X87Register::from_parts(0x8000_0000_0000_0000 + n as u64, 0x3fff));
    s.x87.set_top(5);
    s.x87.tag = 0x3030;
    s.x87.opcode = 0x321;
    s.x87.ip = 0x12345678;
    s.x87.dp = 0x76543210;
    s.mxcsr = 0xffc0;
    s.xmm = std::array::from_fn(|n| [n as u8; 16]);
    s.refresh_mmx();
    let before = s.clone();
    let mut m = memory();
    assert_eq!(run(&save, &mut s, &mut m), Step::Executed);
    s = CpuState::default();
    assert_eq!(run(&restore, &mut s, &mut m), Step::Executed);
    assert_eq!(s.x87, before.x87);
    assert_eq!(s.mxcsr, before.mxcsr);
    assert_eq!(s.xmm, before.xmm);
}

#[cfg(target_arch = "x86_64")]
#[test]
fn integer_store_result_and_c1_round_direction_match_host_x87() {
    #[repr(align(16))]
    struct Saved([u8; 512]);
    let bytes = code(|a| {
        a.fld(qword_ptr(DATA as i32)).unwrap();
        a.fistp(dword_ptr(DATA as i32 + 16)).unwrap();
    });
    for value in [1.5f64, 2.5, 1.6, -1.5, -2.5, -1.6] {
        for mode in 0..4u16 {
            let control = 0x37f | (mode << 10);
            let mut saved = Saved([0; 512]);
            let mut expected = 0i32;
            let status: u16;
            // SAFETY: save/restore the entire host FP/SIMD state around one bounded
            // x87 fixture; all pointers are live and aligned as required by FXSAVE.
            unsafe {
                std::arch::asm!("fxsave [{save}]","fninit","fldcw [{cw}]","fld qword ptr [{value}]","fistp dword ptr [{out}]","fnstsw ax","fxrstor [{save}]",save=in(reg)saved.0.as_mut_ptr(),cw=in(reg)&control,value=in(reg)&value,out=in(reg)&mut expected,out("ax")status,options(nostack));
            }
            let mut s = CpuState::default();
            s.x87.control = control;
            let mut m = memory();
            m.write(DATA, &value.to_le_bytes()).unwrap();
            assert_eq!(run(&bytes, &mut s, &mut m), Step::Executed);
            assert_eq!(
                m.read(DATA + 16, 4).unwrap(),
                expected.to_le_bytes(),
                "value {value} RC {mode}"
            );
            assert_eq!(
                s.x87.status & 0x23f,
                status & 0x23f,
                "value {value} RC {mode}"
            );
        }
    }
}
