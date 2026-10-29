//! x87 arithmetic and compares on full 64-bit significands against the
//! interpreter.
use super::*;
use adv_cpu_core::X87Register;

/// An ordinary extended value near `other`, sometimes a zero, an exact
/// double, an equal magnitude or a far exponent, to reach every path.
fn operand(rng: &mut Rng, other: (u64, u16)) -> (u64, u16) {
    let sign = if rng.below(2) == 0 { 0 } else { 0x8000 };
    let exponent = (0x3fff + rng.below(81) as i32 - 40) as u16;
    let random = rng.next() | 1 << 63;
    match rng.below(12) {
        0 => (0, sign),
        1 => (random & !0x7ff, sign | exponent),
        2 => (other.0, other.1 ^ 0x8000),
        3 => (other.0, other.1),
        4 => (u64::MAX, sign | exponent),
        5 => (1 << 63, sign | exponent),
        6 => {
            let base = if other.0 == 0 {
                0x3fff
            } else {
                other.1 & 0x7fff
            };
            let far = rng.pick(&[1, 63, 64, 65, 66, 127, 128, 200]);
            (random, sign | (base - far))
        }
        7 if other.0 != 0 => (other.0 ^ (1 << rng.below(3)), other.1 ^ 0x8000),
        _ => (random, sign | exponent),
    }
}

// spec: A64F-FP-1 A64F-FP-2 A64F-FP-3
#[test]
fn x87_arithmetic_and_compares_take_full_significands_under_every_control() {
    let mut rng = Rng(0x5eed_87a1_7b3c_9d01);
    let controls = [
        0x037f, 0x077f, 0x0b7f, 0x0f7f, 0x027f, 0x0e7f, 0x007f, 0x047f, 0x1332, 0x035f,
    ];
    for case in 0..240 {
        let form = case % 23;
        let seed = rng.next() | 1 << 63;
        let a = operand(&mut rng, (seed, 0x3fff));
        let b = operand(&mut rng, a);
        let memory: [u8; 8] = match form {
            12..=16 => (f64::from_bits(rng.next() >> 2 | 0x3000_0000_0000_0000)
                * if rng.below(2) == 0 { 1.0 } else { -1.0 })
            .to_le_bytes(),
            17 | 18 => {
                let value = f32::from_bits(rng.next() as u32 >> 2 | 0x2000_0000);
                let mut bytes = [0; 8];
                bytes[..4].copy_from_slice(&value.to_le_bytes());
                bytes
            }
            _ => (rng.next() as i32 >> rng.below(31))
                .to_le_bytes()
                .repeat(2)
                .try_into()
                .unwrap(),
        };
        let mut program = fixture(|code| {
            code.add(eax, ecx).unwrap();
            match form {
                0 => code.fadd_2(st0, st1),
                1 => code.fsub_2(st0, st1),
                2 => code.fmul_2(st0, st1),
                3 => code.fdiv_2(st0, st1),
                4 => code.fsubr_2(st0, st1),
                5 => code.fdivr_2(st0, st1),
                6 => code.faddp(st1, st0),
                7 => code.fsubp(st1, st0),
                8 => code.fdivrp(st1, st0),
                9 => code.fmulp(st1, st0),
                10 => code.fadd_2(st1, st0),
                11 => code.fdiv_2(st1, st0),
                12 => code.fadd(qword_ptr(ebx)),
                13 => code.fsub(qword_ptr(ebx)),
                14 => code.fmul(qword_ptr(ebx)),
                15 => code.fdiv(qword_ptr(ebx)),
                16 => code.fdivr(qword_ptr(ebx)),
                17 => code.fsubr(dword_ptr(ebx)),
                18 => code.fmul(dword_ptr(ebx)),
                19 => code.fiadd(dword_ptr(ebx)),
                20 => code.fidiv(word_ptr(ebx)),
                21 => code.fucomp(st0, st1),
                _ => code.fcom_2(st0, st1),
            }
            .unwrap();
        });
        program.data[..8].copy_from_slice(&memory);
        program.state.x87.push(X87Register::from_parts(b.0, b.1));
        program.state.x87.push(X87Register::from_parts(a.0, a.1));
        if case % 3 == 0 {
            program.state.x87.status |= 0x220;
        }
        // A zero divisor raises an exception the interpreter owns.
        let divisor = match form {
            3 | 8 => b.0,
            5 | 11 | 16 => a.0,
            15 => u64::from(f64::from_le_bytes(memory) != 0.0),
            20 => u64::from(i16::from_le_bytes([memory[0], memory[1]]) != 0),
            _ => 1,
        };
        for control in controls {
            program.state.x87.set_control(control);
            for flagm in [false, true] {
                let config = (Features { flagm }, false, Checks::Adaptive);
                let mut native = executor(&program, config, false);
                let mut oracle = executor(&program, config, true);
                assert_eq!(
                    native.run(program.stop, 100).unwrap(),
                    oracle.run(program.stop, 100).unwrap()
                );
                same(&native, &oracle);
                if control & 0x20 != 0 && divisor != 0 {
                    assert_eq!(
                        native.native.as_ref().unwrap().diagnostics.helpers,
                        0,
                        "form={form}, case={case}, control={control:#x}, a={a:x?}, b={b:x?}"
                    );
                }
            }
        }
    }
}

// spec: A64-MEM-7
#[test]
fn checked_x87_accesses_complete_and_refuse_without_instruction_helpers() {
    let forms: [fn(&mut CodeAssembler); 16] = [
        |a| a.fld(dword_ptr(eax)).unwrap(),
        |a| a.fld(qword_ptr(eax)).unwrap(),
        |a| a.fild(word_ptr(eax)).unwrap(),
        |a| a.fild(dword_ptr(eax)).unwrap(),
        |a| a.fild(qword_ptr(eax)).unwrap(),
        |a| a.fstp(dword_ptr(eax)).unwrap(),
        |a| a.fst(qword_ptr(eax)).unwrap(),
        |a| a.fistp(dword_ptr(eax)).unwrap(),
        |a| a.fistp(qword_ptr(eax)).unwrap(),
        |a| a.fadd(qword_ptr(eax)).unwrap(),
        |a| a.fmul(dword_ptr(eax)).unwrap(),
        |a| a.fiadd(dword_ptr(eax)).unwrap(),
        |a| a.fcomp(qword_ptr(eax)).unwrap(),
        |a| a.fldcw(word_ptr(eax)).unwrap(),
        |a| a.fnstcw(word_ptr(eax)).unwrap(),
        |a| a.fnstsw(word_ptr(eax)).unwrap(),
    ];
    for (n, form) in forms.into_iter().enumerate() {
        for address in [DATA + 128, DATA + 4096 - 2, HOLE] {
            let mut program = fixture(|a| {
                // Lazy flags stay live across the access.
                a.add(edx, 17).unwrap();
                a.mov(eax, address).unwrap();
                form(a);
                a.adc(edx, 0).unwrap();
            });
            // Normals as binary32 and binary64, and a control word that
            // masks every exception.
            if address != HOLE {
                let at = (address - DATA) as usize;
                program.data[at..at + 8].copy_from_slice(&0x3ff0_0000_3f80_027fu64.to_le_bytes());
            }
            program
                .state
                .x87
                .push(X87Register::from_parts(3 << 62, 0x3fff));
            program
                .state
                .x87
                .push(X87Register::from_parts(1 << 63, 0x4000));
            for flagm in [false, true] {
                let config = (Features { flagm }, false, Checks::Slow);
                let mut native = executor(&program, config, false);
                let mut oracle = executor(&program, config, true);
                let outcome = native.run(program.stop, 100).unwrap();
                assert_eq!(outcome, oracle.run(program.stop, 100).unwrap(), "form {n}");
                same(&native, &oracle);
                let diagnostics = &native.native.as_ref().unwrap().diagnostics;
                assert!(diagnostics.accesses > 0, "form {n} uses access");
                assert_eq!(diagnostics.helpers, 0, "form {n} avoids interpret");
                assert_eq!(diagnostics.refused, u64::from(address == HOLE), "form {n}");
            }
        }
    }
}

// spec: A64-MEM-7 A64F-FP-7
#[test]
fn cached_x87_runs_span_checked_accesses_and_publish_on_refusal() {
    use crate::jit_map::{Part, enable_memory, units_of};
    enable_memory();
    // Refuse an integer load, an x87 arithmetic source, or an x87 store
    // after the run has changed values, tags, TOP and status.
    for fault in [None, Some(0), Some(1), Some(2)] {
        let integer = if fault == Some(0) { HOLE } else { DATA + 64 };
        let source = if fault == Some(1) { HOLE } else { DATA + 16 };
        let dest = if fault == Some(2) { HOLE } else { DATA + 32 };
        let mut program = fixture(|a| {
            a.add(edx, 17).unwrap();
            a.fld(qword_ptr(ebx)).unwrap();
            a.fld(qword_ptr(ebx + 8)).unwrap();
            a.fadd_2(st0, st2).unwrap();
            a.fmul(qword_ptr(ebx + 16)).unwrap();
            a.fstp(qword_ptr(ebx + 24)).unwrap();
            a.fld(st0).unwrap();
            a.add(ecx, dword_ptr(integer as i32)).unwrap();
            a.fadd(qword_ptr(source as i32)).unwrap();
            a.fstp(qword_ptr(dest as i32)).unwrap();
            a.fld(dword_ptr(ebx + 48)).unwrap();
            a.fmul_2(st0, st1).unwrap();
            a.fstp(dword_ptr(ebx + 52)).unwrap();
            a.fstp(qword_ptr(ebx + 56)).unwrap();
            a.adc(edx, 0).unwrap();
        });
        for (at, value) in [(0, 2.5f64), (8, 1.25), (16, 3.0)] {
            program.data[at..at + 8].copy_from_slice(&value.to_le_bytes());
        }
        program.data[48..52].copy_from_slice(&1.5f32.to_le_bytes());
        program.data[64..68].copy_from_slice(&u32::MAX.to_le_bytes());
        program
            .state
            .x87
            .push(X87Register::from_parts(0xb000_0000_0000_0123, 0x3fff));
        program.state.x87.status |= 0x220;
        for checks in [Checks::All, Checks::Slow] {
            for flagm in [false, true] {
                let config = (Features { flagm }, false, checks);
                let mut native = executor(&program, config, false);
                let mut oracle = executor(&program, config, true);
                assert_eq!(
                    native.run(program.stop, 100).unwrap(),
                    oracle.run(program.stop, 100).unwrap(),
                    "{fault:?}, {checks:?}, flagm={flagm}"
                );
                same(&native, &oracle);
                let engine = native.native.as_ref().unwrap();
                assert_eq!(engine.diagnostics.helpers, 0, "avoids interpret");
                assert_eq!(engine.diagnostics.refused, u64::from(fault.is_some()));
                if checks == Checks::Slow {
                    assert!(engine.diagnostics.accesses > 0, "owner completes accesses");
                }
                let base = engine.unit_address(CODE).unwrap();
                let (_, marks) = units_of(CODE)
                    .into_iter()
                    .rfind(|&(unit, _)| unit == base)
                    .expect("the unit was recorded");
                assert_eq!(
                    marks.iter().filter(|m| m.part == Part::X87Entry).count(),
                    1,
                    "one cached run across checked accesses: {marks:?}"
                );
            }
        }
    }
}

// spec: A64F-FP-7
#[test]
fn cached_x87_runs_span_calls_stack_operations_and_sse() {
    use crate::jit_map::{Part, enable_memory, units_of};
    enable_memory();
    // A run through an inlined callee converting with SSE, as `_ftol2`
    // does; the SSE store may be refused, and the callee may change its
    // return address so that the inlined return misses.
    for variant in 0..3 {
        let store = if variant == 1 { HOLE } else { DATA + 48 };
        let mut program = fixture(|a| {
            let (mut f, mut done) = (a.create_label(), a.create_label());
            a.fld(qword_ptr(ebx)).unwrap();
            a.fld(qword_ptr(ebx + 8)).unwrap();
            a.call(f).unwrap();
            a.mov(dword_ptr(ebx + 40), eax).unwrap();
            a.push(ecx).unwrap();
            a.movsd_2(qword_ptr(store as i32), xmm1).unwrap();
            a.pop(edx).unwrap();
            a.faddp(st1, st0).unwrap();
            a.fstp(qword_ptr(ebx + 24)).unwrap();
            a.jmp(done).unwrap();
            a.set_label(&mut f).unwrap();
            a.push(ebp).unwrap();
            a.mov(ebp, esp).unwrap();
            a.sub(esp, 8).unwrap();
            a.and(esp, -8).unwrap();
            a.fld(st0).unwrap();
            a.fmul_2(st0, st2).unwrap();
            a.fstp(qword_ptr(esp)).unwrap();
            a.cvttsd2si(eax, qword_ptr(esp)).unwrap();
            if variant == 2 {
                // Return past the 3-byte store after the call.
                a.add(dword_ptr(ebp + 4), 3).unwrap();
            }
            a.leave().unwrap();
            a.ret().unwrap();
            a.set_label(&mut done).unwrap();
        });
        for (at, value) in [(0, 2.5f64), (8, 1.25)] {
            program.data[at..at + 8].copy_from_slice(&value.to_le_bytes());
        }
        program.state.xmm[1][..8].copy_from_slice(&4.0f64.to_le_bytes());
        for checks in [Checks::All, Checks::Slow] {
            let config = (Features { flagm: false }, false, checks);
            let mut native = executor(&program, config, false);
            let mut oracle = executor(&program, config, true);
            assert_eq!(
                native.run(program.stop, 100).unwrap(),
                oracle.run(program.stop, 100).unwrap(),
                "{variant}, {checks:?}"
            );
            same(&native, &oracle);
            let engine = native.native.as_ref().unwrap();
            assert_eq!(engine.diagnostics.helpers, 0, "avoids interpret");
            let base = engine.unit_address(CODE).unwrap();
            let (_, marks) = units_of(CODE)
                .into_iter()
                .rfind(|&(unit, _)| unit == base)
                .expect("the unit was recorded");
            assert_eq!(
                marks.iter().filter(|m| m.part == Part::X87Entry).count(),
                1,
                "one cached run through the callee: {marks:?}"
            );
        }
    }
}
