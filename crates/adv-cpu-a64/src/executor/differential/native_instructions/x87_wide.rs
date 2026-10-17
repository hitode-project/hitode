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
