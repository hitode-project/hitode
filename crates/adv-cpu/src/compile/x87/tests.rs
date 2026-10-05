use super::*;
use crate::{
    X87Register,
    interpreter::{Interpreter, Step},
};
use iced_x86::code_asm::*;

const CODE: u32 = 0x10000;
const DATA: u32 = 0x12000;

fn bytes(build: impl FnOnce(&mut CodeAssembler)) -> Vec<u8> {
    let mut a = CodeAssembler::new(32).unwrap();
    build(&mut a);
    a.assemble(u64::from(CODE)).unwrap()
}
fn memory(bytes: &[u8]) -> Memory {
    let mut m = Memory::new(CODE, 0x6000).unwrap();
    m.protect(CODE, 0x6000, READ | WRITE).unwrap();
    m.write(CODE, bytes).unwrap();
    m.protect(CODE, 4096, READ | EXEC).unwrap();
    m.write(DATA, &1.25f64.to_le_bytes()).unwrap();
    m
}
fn state() -> CpuState {
    let mut s = CpuState {
        eip: CODE,
        fuel: 100,
        ..CpuState::default()
    };
    s.x87
        .push(X87Register(crate::backend::x87::encode_f64(1.25)));
    s.x87
        .push(X87Register(crate::backend::x87::encode_f64(2.5)));
    s.refresh_mmx();
    s.regs[6] = DATA;
    s.eflags = 0xad7;
    s.xmm = std::array::from_fn(|i| std::array::from_fn(|n| (i * 17 + n) as u8));
    s.x87.ip = 0x11223344;
    s.x87.dp = 0x55667788;
    s.x87.cs = 0x1234;
    s.x87.ds = 0x5678;
    s
}

/// The reference executes decoded instructions, never the fast helper. Compare
/// every canonical field and every accessible byte, including fault rollback.
fn compare(bytes: &[u8], initial: CpuState, setup: impl Fn(&mut Memory), calls: Option<u64>) {
    let mut actual_memory = memory(bytes);
    let mut reference_memory = memory(bytes);
    setup(&mut actual_memory);
    setup(&mut reference_memory);
    let compiled = compile(&Region::new(CODE, bytes.len()), &actual_memory, false).unwrap();
    let mut expected = initial.clone();
    let mut actual = initial;
    let mut interpreter = Interpreter::new();
    let mut expected_exit = Exit::Dispatch;
    while expected.eip < CODE + bytes.len() as u32 {
        let epoch = reference_memory.epoch;
        match interpreter.step(&mut expected, &mut reference_memory) {
            Step::Executed => {
                if reference_memory.epoch != epoch || expected.x87.status & 0x80 != 0 {
                    expected_exit = Exit::Fallback;
                    break;
                }
            }
            Step::ReadFault { address, .. } => {
                expected.fault_addr = address;
                expected_exit = Exit::ReadFault;
                break;
            }
            Step::WriteFault { address, .. } => {
                expected.fault_addr = address;
                expected_exit = Exit::WriteFault;
                break;
            }
            Step::Trap { .. } => {
                expected_exit = Exit::Fallback;
                break;
            }
            other => panic!("unexpected reference exit {other:?}"),
        }
    }
    let before = testing::calls();
    assert_eq!(
        testing::run(&compiled, &mut actual, &mut actual_memory),
        expected_exit as u32,
        "{bytes:02x?}"
    );
    if let Some(calls) = calls {
        assert_eq!(testing::calls() - before, calls, "{bytes:02x?}");
    }
    expected.fuel = actual.fuel;
    let unspecified = crate::compile::flag_liveness::unspecified_at(bytes, CODE, actual.eip);
    actual.eflags &= !unspecified;
    expected.eflags &= !unspecified;
    assert_eq!(actual, expected, "{bytes:02x?}");
    assert_eq!(actual_memory.epoch, reference_memory.epoch);
    for page in 0..6 {
        let address = CODE + page * 4096;
        if let Ok(expected) = reference_memory.read_data(address, 4096) {
            assert_eq!(
                actual_memory.read_data(address, 4096).unwrap(),
                expected,
                "page {address:#x}"
            );
        }
    }
}

#[test]
fn hot_operations_are_native_even_between_lazy_integer_flags() {
    let code = bytes(|a| {
        a.movdqu(xmm0, ptr(esi + 0x100)).unwrap();
        a.paddd(xmm0, xmm1).unwrap();
        a.add(eax, 13).unwrap();
        a.fld(qword_ptr(esi)).unwrap();
        a.fadd_2(st0, st1).unwrap();
        a.fmul_2(st0, st2).unwrap();
        a.fsubr(qword_ptr(esi)).unwrap();
        a.fdiv_2(st0, st1).unwrap();
        a.fxch(st0, st1).unwrap();
        a.fucom(st0, st1).unwrap();
        a.fchs().unwrap();
        a.fabs().unwrap();
        a.fst(st2).unwrap();
        a.fstp(qword_ptr(esi + 8)).unwrap();
        a.fnstsw(ax).unwrap();
        a.setb(dl).unwrap();
        a.setp(cl).unwrap();
        a.movdqu(ptr(esi + 0x110), xmm0).unwrap();
    });
    compare(
        &code,
        state(),
        |m| m.write(DATA + 0x100, &[0xa5; 16]).unwrap(),
        Some(0),
    );
    let compiled = compile(&Region::new(CODE, code.len()), &memory(&code), false).unwrap();
    // Runtime helper counts prove the exercised path is native; the disassembly
    // additionally protects against accidentally deleting arithmetic lowering.
    assert!(
        compiled.disasm.contains("addsd") || compiled.disasm.contains("vaddsd"),
        "{}",
        compiled.disasm
    );
    assert!(
        compiled.disasm.contains("divsd") || compiled.disasm.contains("vdivsd"),
        "{}",
        compiled.disasm
    );
}

#[test]
fn precision_rounding_pending_exceptions_and_mmx_take_the_helper() {
    let code = bytes(|a| {
        a.fmul_2(st0, st1).unwrap();
    });
    for precision in [0u16, 2, 3] {
        for rounding in 0..4 {
            let mut s = state();
            s.x87.control = 0x7f | precision << 8 | rounding << 10;
            let calls = u64::from(precision != 2 || rounding != 0);
            compare(&code, s, |_| {}, Some(calls));
        }
    }
    for status in [0x80, 0x8081] {
        let mut s = state();
        s.x87.status |= status;
        compare(&code, s, |_| {}, Some(1));
    }
    // Deferred EMMS is not an ordinary empty-stack image: the helper must first
    // commit it. This also guards the pre-existing helper's mmx_mode check.
    for mode in [1, 2, 3, 0x101] {
        let mut s = state();
        s.mmx_mode = mode;
        compare(&code, s, |_| {}, Some(1));
    }
    let code = bytes(|a| {
        a.movq(mm0, qword_ptr(esi)).unwrap();
        a.emms().unwrap();
        a.fld1().unwrap();
        a.fstp(qword_ptr(esi + 8)).unwrap();
    });
    compare(&code, state(), |_| {}, Some(2));
}

#[test]
fn exceptional_encodings_stack_faults_and_divide_by_zero_are_transactional() {
    let code = bytes(|a| {
        a.fld(st0).unwrap();
    });
    for value in [
        X87Register::from_parts(1 << 63, 0x43ff), // finite, outside binary64
        X87Register::from_parts(1 << 63, 0x3bff), // extended normal, double subnormal
        X87Register::from_parts((1 << 63) | 1, 0x3fff), // full 64-bit precision
        X87Register::from_parts(1 << 63, 0x7fff), // infinity
        X87Register::from_parts(0xc000_0000_0000_1234, 0x7fff), // qNaN
        X87Register::from_parts(0x8000_0000_0000_1234, 0x7fff), // sNaN
        X87Register::from_parts(1, 0),            // denormal
        X87Register::from_parts(0, 0x3fff),       // unsupported unnormal
    ] {
        let mut s = state();
        let slot = s.x87.top();
        s.x87.regs[slot] = value;
        s.x87.set_physical_tag(slot, value.tag());
        s.refresh_mmx();
        compare(&code, s, |_| {}, Some(1));
    }
    for tags in [0, 0xffff] {
        let mut s = state();
        s.x87.tag = tags;
        compare(&code, s, |_| {}, Some(1));
    }
    let code = bytes(|a| {
        a.fdiv(qword_ptr(esi)).unwrap();
    });
    for control in [0x027f, 0x027b] {
        let mut s = state();
        s.x87.control = control;
        compare(
            &code,
            s,
            |m| m.write(DATA, &0u64.to_le_bytes()).unwrap(),
            Some(1),
        );
    }
}

#[test]
fn address_generation_and_faults_preserve_the_exact_committed_prefix() {
    // FS plus SIB and wraparound are evaluated with guest u32 arithmetic.
    let code = bytes(|a| {
        a.fld(qword_ptr(esi + ecx * 4 + 12).fs()).unwrap();
    });
    let mut s = state();
    s.fs_base = 0x1000;
    s.regs[6] = u32::MAX - 3;
    s.regs[1] = (DATA - 0x1000 - 8) / 4;
    compare(&code, s, |_| {}, Some(0));
    let code = bytes(|a| {
        a.fld(qword_ptr(bx + si)).unwrap();
    });
    let mut s = state();
    s.regs[3] = 0x100;
    s.regs[6] = 0x200;
    compare(&code, s, |_| {}, Some(1));
    for store in [false, true] {
        let code = bytes(|a| {
            a.add(eax, 17).unwrap();
            a.fchs().unwrap();
            if store {
                a.fstp(qword_ptr(esi)).unwrap();
            } else {
                a.fld(qword_ptr(esi)).unwrap();
            }
        });
        for (address, permissions) in [
            (DATA + 4092, READ | WRITE),
            (DATA + 4092, 0),
            (DATA, READ),
            (u32::MAX - 3, READ | WRITE),
        ] {
            let mut s = state();
            s.regs[6] = address;
            let expected_calls = u64::from(address != DATA || store);
            compare(
                &code,
                s,
                |m| {
                    m.protect(DATA + u32::from(address != DATA) * 4096, 4096, permissions)
                        .unwrap()
                },
                Some(expected_calls),
            );
        }
    }
    let code = bytes(|a| {
        a.fstp(qword_ptr(esi)).unwrap();
    });
    compare(
        &code,
        state(),
        |m| m.protect(DATA, 4096, READ | WRITE | EXEC).unwrap(),
        Some(1),
    );
}

fn file_memory(code: &[u8], page: &std::rc::Rc<crate::backend::FilePage>, cow: bool) -> Memory {
    let mut m = Memory::reserve(CODE, 0x6000).unwrap();
    m.map(CODE, 4096, READ | WRITE).unwrap();
    m.write(CODE, code).unwrap();
    m.protect(CODE, 4096, READ | EXEC).unwrap();
    m.map_file_pages(DATA, std::slice::from_ref(page), READ | WRITE, cow)
        .unwrap();
    m
}

#[test]
fn file_backed_operands_and_copy_on_write_use_transactional_helpers() {
    let code = bytes(|a| {
        a.fld(qword_ptr(esi)).unwrap();
        a.fmul(qword_ptr(esi + 8)).unwrap();
        a.fstp(qword_ptr(esi + 16)).unwrap();
    });
    let mut data = [0; 4096];
    data[..8].copy_from_slice(&1.3f64.to_le_bytes());
    data[8..16].copy_from_slice(&0.7f64.to_le_bytes());
    for cow in [false, true] {
        let page = crate::backend::FilePage::new(data);
        let oracle = crate::backend::FilePage::new(data);
        let mut actual_memory = file_memory(&code, &page, cow);
        let mut reference_memory = file_memory(&code, &oracle, cow);
        let compiled = compile(&Region::new(CODE, code.len()), &actual_memory, false).unwrap();
        let mut actual = state();
        let mut expected = actual.clone();
        let mut interpreter = Interpreter::new();
        for _ in 0..3 {
            assert!(matches!(
                interpreter.step(&mut expected, &mut reference_memory),
                Step::Executed
            ));
        }
        let before = testing::calls();
        let reason = testing::run(&compiled, &mut actual, &mut actual_memory);
        assert!(reason == Exit::Dispatch as u32 || reason == Exit::Fallback as u32);
        assert_eq!(testing::calls() - before, 3);
        expected.fuel = actual.fuel;
        assert_eq!(actual, expected);
        assert_eq!(
            actual_memory.read_data(DATA, 4096).unwrap(),
            reference_memory.read_data(DATA, 4096).unwrap()
        );
        assert_eq!(page.snapshot().unwrap(), oracle.snapshot().unwrap());
        assert_eq!(page.dirty(), !cow);
        assert_eq!(
            actual_memory.file_page_copy_on_write(DATA),
            reference_memory.file_page_copy_on_write(DATA)
        );
    }
}

#[test]
fn a_lazy_file_fault_is_not_retried_inside_the_same_x87_instruction() {
    use std::{cell::Cell, rc::Rc};
    for store in [false, true] {
        let code = bytes(|a| {
            a.fchs().unwrap();
            if store {
                a.fstp(qword_ptr(esi)).unwrap();
            } else {
                a.fld(qword_ptr(esi)).unwrap();
            }
        });
        let attempts = Rc::new(Cell::new(0));
        let count = attempts.clone();
        let page = crate::backend::FilePage::lazy(move || {
            count.set(count.get() + 1);
            if count.get() == 1 {
                Err("injected first-touch failure".into())
            } else {
                Ok([0; 4096])
            }
        });
        let mut memory = file_memory(&code, &page, store);
        let compiled = compile(&Region::new(CODE, code.len()), &memory, false).unwrap();
        let mut actual = state();
        let before = testing::calls();
        let reason = testing::run(&compiled, &mut actual, &mut memory);
        assert_eq!(
            reason,
            if store {
                Exit::WriteFault
            } else {
                Exit::ReadFault
            } as u32
        );
        assert_eq!(
            attempts.get(),
            1,
            "one failing guest access must perform exactly one initializer attempt"
        );
        assert_eq!(testing::calls() - before, 1);
        let mut expected = state();
        let slot = expected.x87.top();
        expected.x87.regs[slot] = X87Register(crate::backend::x87::encode_f64(-2.5));
        expected.x87.ip = CODE;
        expected.x87.opcode = 0x1e0; // FCHS D9 E0
        expected.x87.status &= !0x200;
        expected.refresh_mmx();
        expected.eip = CODE + 2;
        expected.fault_addr = DATA;
        expected.fuel = actual.fuel;
        assert_eq!(
            actual, expected,
            "the completed FCHS prefix must not be replayed or rolled back"
        );
    }
}

#[test]
#[cfg(windows)]
fn pure_x87_shared_audio_accesses_never_use_ordinary_loads_or_stores() {
    let code = bytes(|a| {
        a.fld(qword_ptr(esi)).unwrap();
        a.fadd(qword_ptr(esi)).unwrap();
        a.fstp(qword_ptr(esi + 8)).unwrap();
    });
    let mut memory = Memory::reserve(CODE, 0x6000).unwrap();
    memory.map(CODE, 4096, READ | WRITE).unwrap();
    memory.write(CODE, &code).unwrap();
    memory.protect(CODE, 4096, READ | EXEC).unwrap();
    let shared = memory.map_shared_audio(DATA, 4096).unwrap();
    shared.store_bytes(0, &1.25f64.to_le_bytes());
    let compiled = compile(&Region::new(CODE, code.len()), &memory, false).unwrap();
    let mut actual = state();
    let before = testing::calls();
    assert_eq!(
        testing::run(&compiled, &mut actual, &mut memory),
        Exit::Dispatch as u32
    );
    assert_eq!(testing::calls() - before, 3);
    let result: [u8; 8] = std::array::from_fn(|n| shared.load_u8(n + 8));
    assert_eq!(f64::from_le_bytes(result), 2.5);
    assert_eq!(actual.x87.top(), state().x87.top());
}

#[test]
fn helper_only_compilation_retains_a_reproducible_comparison_lane() {
    let code = bytes(|a| {
        a.fld(qword_ptr(esi)).unwrap();
        a.fadd_2(st0, st1).unwrap();
        a.fstp(qword_ptr(esi + 8)).unwrap();
    });
    testing::HELPER_ONLY.set(true);
    compare(&code, state(), |_| {}, Some(3));
    testing::HELPER_ONLY.set(false);
    compare(&code, state(), |_| {}, Some(0));
}

#[test]
fn parallel_fast_compilation_preserves_native_and_transactional_x87_state() {
    use std::sync::Arc;

    let code = bytes(|a| {
        a.fld(qword_ptr(esi)).unwrap();
        a.fadd_2(st0, st1).unwrap();
        a.fstp(qword_ptr(esi + 8)).unwrap();
        a.fld(tbyte_ptr(esi + 16)).unwrap();
        a.fstp(tbyte_ptr(esi + 32)).unwrap();
    });
    let extended = X87Register::from_parts((1 << 63) | 1, 0x3fff);
    for control in [0x027f, 0x037f] {
        let mut reference_memory = memory(&code);
        let mut actual_memory = memory(&code);
        for m in [&mut reference_memory, &mut actual_memory] {
            m.write(DATA + 16, &extended.0).unwrap();
        }
        let fence = Arc::new(crate::parallel_memory::Fence::default());
        actual_memory.parallel = Some(fence.clone());
        // A parallel snapshot selects the Fast compilation strategy even when
        // the region requests Speed. The permit exercises foreign-memory helpers.
        let compiled = compile(&Region::new(CODE, code.len()), &actual_memory, false).unwrap();
        let mut actual = state();
        actual.x87.control = control;
        let mut expected = actual.clone();
        let mut interpreter = Interpreter::new();
        for _ in 0..5 {
            assert!(matches!(
                interpreter.step(&mut expected, &mut reference_memory),
                Step::Executed
            ));
        }
        expected.fuel -= 5;
        let permit = fence.enter();
        assert!(actual_memory.foreign_access_required());
        let before = testing::calls();
        assert_eq!(
            testing::run(&compiled, &mut actual, &mut actual_memory),
            Exit::Dispatch as u32
        );
        assert_eq!(
            testing::calls() - before,
            // A rejected segment resumes the remaining instruction at its
            // canonical cold helper entry before the two extended transfers.
            if control == 0x027f { 2 } else { 4 }
        );
        assert_eq!(actual, expected);
        assert_eq!(actual_memory.epoch, reference_memory.epoch);
        assert_eq!(
            actual_memory.read_data(DATA, 4096).unwrap(),
            reference_memory.read_data(DATA, 4096).unwrap()
        );
        drop(permit);
    }
}

#[path = "segment_tests.rs"]
mod segment_tests;

#[path = "hardware_tests.rs"]
mod hardware_tests;

#[test]
fn helper_effects_preserve_dirty_address_aliases_integer_flags_and_xmm() {
    for vectors in [false, true] {
        let code = bytes(|a| {
            a.mov(ebx, 0xabcd_fff0u32).unwrap();
            a.mov(esi, 0xffff_2028u32).unwrap();
            if vectors {
                a.movdqu(xmm0, ptr(DATA as i32 + 0x100)).unwrap();
                a.paddd(xmm0, xmm1).unwrap();
            }
            a.mov(eax, 0x1234_ffffu32).unwrap();
            a.add(eax, 1).unwrap();
            // addr16 wraps before FS addition; neither address register is current
            // in CpuState until the helper's declared inputs are synchronized.
            a.fld(tbyte_ptr(bx + si).fs()).unwrap();
            a.fstp(tbyte_ptr(bx + si + 16).fs()).unwrap();
            a.fnstsw(ax).unwrap();
            a.mov(edx, eax).unwrap(); // Observe the preserved upper EAX half.
            a.setb(cl).unwrap();
            a.setp(al).unwrap();
            if vectors {
                a.movdqu(ptr(DATA as i32 + 0x110), xmm0).unwrap();
            }
            a.mov(edi, u32::MAX - 3).unwrap();
            a.mov(ecx, (DATA - 0x10000 - 8) / 4).unwrap();
            a.fld(qword_ptr(edi + ecx * 4 + 12).fs()).unwrap();
        });
        let mut s = state();
        s.fs_base = 0x10000;
        testing::HELPER_ONLY.set(true);
        compare(
            &code,
            s,
            |m| {
                m.write(
                    DATA + 0x18,
                    &X87Register::from_parts((1 << 63) | 13, 0x4000).0,
                )
                .unwrap();
                m.write(DATA + 0x100, &[0x5a; 16]).unwrap();
            },
            Some(4),
        );
        testing::HELPER_ONLY.set(false);
    }
}

#[test]
fn helper_integer_flag_effects_cover_conditions_and_compare_rollback() {
    testing::HELPER_ONLY.set(true);
    for predicate in 0..8 {
        for value in [0u32, 1, u32::MAX] {
            let code = bytes(|a| {
                a.mov(eax, value).unwrap();
                a.add(eax, 1).unwrap();
                a.inc(ecx).unwrap(); // INC preserves ADD's lazy carry.
                match predicate {
                    0 => a.fcmovb(st0, st1),
                    1 => a.fcmovbe(st0, st1),
                    2 => a.fcmove(st0, st1),
                    3 => a.fcmovnb(st0, st1),
                    4 => a.fcmovnbe(st0, st1),
                    5 => a.fcmovne(st0, st1),
                    6 => a.fcmovnu(st0, st1),
                    _ => a.fcmovu(st0, st1),
                }
                .unwrap();
                a.setb(dl).unwrap();
                a.setp(bl).unwrap();
                a.sete(cl).unwrap();
            });
            compare(&code, state(), |_| {}, Some(1));
        }
    }
    for instruction in 0..4 {
        for bits in [
            0.5f64.to_bits(),
            1.25f64.to_bits(),
            2.5f64.to_bits(),
            0x7ff8_0000_0000_0001,
        ] {
            for masked in [false, true] {
                let code = bytes(|a| {
                    a.add(eax, -1).unwrap();
                    match instruction {
                        0 => a.fcomi(st0, st1),
                        1 => a.fcomip(st0, st1),
                        2 => a.fucomi(st0, st1),
                        _ => a.fucomip(st0, st1),
                    }
                    .unwrap();
                    a.setb(dl).unwrap();
                    a.setp(bl).unwrap();
                    a.sete(cl).unwrap();
                });
                let mut s = state();
                let top = s.x87.top();
                s.x87.regs[top] =
                    X87Register(crate::backend::x87::encode_f64(f64::from_bits(bits)));
                s.x87.set_physical_tag(top, s.x87.regs[top].tag());
                if !masked {
                    s.x87.control &= !1;
                }
                s.refresh_mmx();
                compare(&code, s, |_| {}, Some(1));
            }
        }
    }
    testing::HELPER_ONLY.set(false);
}

#[test]
fn helper_effects_keep_fuel_and_exact_state_across_budget_resumptions() {
    let code = bytes(|a| {
        for _ in 0..4 {
            a.inc(ebx).unwrap();
            a.fsin().unwrap();
            a.fnstsw(ax).unwrap();
        }
    });
    let mut actual_memory = memory(&code);
    let mut expected_memory = memory(&code);
    let mut region = Region::new(CODE, code.len());
    // Every instruction is a legal budget resumption entry.
    let instructions: Vec<_> =
        iced_x86::Decoder::with_ip(32, &code, CODE as u64, iced_x86::DecoderOptions::NONE)
            .into_iter()
            .collect();
    for i in &instructions {
        if i.ip32() != CODE {
            region.entries.push(i.ip32());
        }
    }
    let compiled = compile(&region, &actual_memory, false).unwrap();
    let mut actual = state();
    let mut expected = actual.clone();
    let mut interpreter = Interpreter::new();
    while actual.eip < CODE + code.len() as u32 {
        actual.fuel = 2;
        let before = actual.eip;
        let exit = testing::run(&compiled, &mut actual, &mut actual_memory);
        assert!(exit == Exit::Budget as u32 || exit == Exit::Dispatch as u32);
        let executed = instructions
            .iter()
            .filter(|i| i.ip32() >= before && i.ip32() < actual.eip)
            .count();
        assert!(executed > 0);
        assert_eq!(actual.fuel, 2 - executed as u64);
        for _ in 0..executed {
            assert!(matches!(
                interpreter.step(&mut expected, &mut expected_memory),
                Step::Executed
            ));
        }
        expected.fuel = 2 - executed as u64;
        assert_eq!(actual, expected);
    }
}
