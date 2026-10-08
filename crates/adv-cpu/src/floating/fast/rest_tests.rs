//! Full-state differential matrix for raw80 transfers, one-round stores,
//! quotient guards and helper-only forms. Fixtures are entirely synthetic.
use super::*;
use crate::compile::x87::testing;

fn forms() -> Vec<Asm> {
    vec![
        |a, s| a.fld(s),
        |a, s| a.fst(s),
        |a, s| a.fstp(s),
        |a, s| a.fxch(st0, s),
        |a, _| a.fchs(),
        |a, _| a.fabs(),
        |a, _| a.fst(dword_ptr(esi)),
        |a, _| a.fstp(dword_ptr(esi)),
        |a, _| a.fst(qword_ptr(esi)),
        |a, _| a.fstp(qword_ptr(esi)),
        |a, _| a.fist(dword_ptr(esi)),
        |a, _| a.fistp(dword_ptr(esi)),
        |a, _| a.fistp(qword_ptr(esi)),
        |a, s| a.fcom_2(st0, s),
        |a, s| a.fcomp_2(st0, s),
        |a, _| a.fcompp(),
        |a, s| a.fucom(st0, s),
        |a, s| a.fucomp(st0, s),
        |a, _| a.fucompp(),
        |a, s| a.fdiv_2(st0, s),
        |a, s| a.fdivr_2(st0, s),
        |a, s| a.fdivp(s, st0),
        |a, s| a.fdivrp(s, st0),
        |a, _| a.fdiv(qword_ptr(esi)),
        |a, _| a.fdivr(dword_ptr(esi)),
        |a, _| a.fidiv(dword_ptr(esi)),
        |a, _| a.fld(tbyte_ptr(esi)),
        |a, _| a.fstp(tbyte_ptr(esi)),
        |a, _| a.fild(qword_ptr(esi)),
        |a, _| a.fxam(),
        |a, _| a.fnclex(),
        |a, s| a.fcomi(st0, s),
        |a, s| a.fcomip(st0, s),
        |a, s| a.fucomi(st0, s),
        |a, s| a.fucomip(st0, s),
        |a, _| a.fld1(),
        |a, _| a.fldz(),
        |a, _| a.fnstsw(ax),
    ]
}

fn edge_values() -> Vec<Ext> {
    vec![
        extended(0.0),
        extended(-0.0),
        extended(1.0),
        extended(-1.0),
        extended(3.0),
        extended(0.5),
        extended(-0.5),
        Ext::from_parts((1 << 63) | 1, 0x3ffe),
        Ext::from_parts((1 << 63) - 1, 0x3fff), // Unsupported encoding.
        Ext::from_parts((1 << 63) | 1, 0x3fff),
        Ext::from_parts((1 << 63) | (1 << 10), 0x3fff), // m64 halfway.
        Ext::from_parts((1 << 63) | (1 << 39) | 1, 0x3fff), // m32 above halfway.
        Ext::from_parts(u64::MAX, 0x3fff),              // Rounding carry.
        extended(2f64.powi(31)),
        extended(-2f64.powi(31)),
        extended(2f64.powi(63)),
        extended(-2f64.powi(63)),
        extended(2f64.powi(400)),
        extended(2f64.powi(-400)),
        extended(2f64.powi(401)),
        extended(2f64.powi(-401)),
        extended(f64::MIN_POSITIVE),
        extended(f64::MAX),
        Ext::from_parts(1 << 63, 1),
        Ext::from_parts(u64::MAX, 0x7ffe),
        Ext::from_parts(1, 0),
        Ext::from_parts(1 << 63, 0), // Pseudo-denormal.
        Ext::from_parts(1 << 63, 0x7fff),
        Ext::from_parts(1 << 63, 0xffff),
        Ext::from_parts(3 << 62, 0x7fff),       // QNaN.
        Ext::from_parts((1 << 63) | 1, 0xffff), // SNaN.
    ]
}

#[test]
fn remaining_helpers_preserve_complete_state_or_reject_atomically() {
    matrix(0);
}

#[test]
fn remaining_native_forms_and_recoveries_match_software_extended() {
    matrix(1);
}

#[test]
fn remaining_compiled_helper_only_forms_match_software_extended() {
    matrix(2);
}

fn matrix(mode: u8) {
    testing::SINGLE_ONLY.set(true);
    testing::HELPER_ONLY.set(mode == 2);
    let values = edge_values();
    let mut hits = vec![0; forms().len()];
    let mut rng = Rng(0x45ad_67c9_843e_91bf);
    for (form, assemble) in forms().into_iter().enumerate() {
        let mut asm = CodeAssembler::new(32).unwrap();
        assemble(&mut asm, st1).unwrap();
        let bytes = asm.assemble(u64::from(CODE)).unwrap();
        let instruction = iced_x86::Decoder::with_ip(32, &bytes, u64::from(CODE), 0).decode();
        let opcode = bytes
            .windows(2)
            .find(|b| (0xd8..=0xdf).contains(&b[0]))
            .map(|b| (u16::from(b[0] & 7) << 8) | u16::from(b[1]));
        let op = NativeX87 {
            instruction,
            opcode,
            fast: classify(&instruction),
        };
        let compiled = (mode != 0).then(|| {
            crate::compile::compile(
                &crate::Region::new(CODE, bytes.len()),
                &mut memory(0, &bytes),
                false,
            )
            .unwrap()
        });
        for pc in 0..4 {
            for rc in 0..4 {
                for case in 0..values.len() + 15 {
                    let mut initial = CpuState {
                        eip: CODE,
                        fuel: 1,
                        eflags: 0xad7,
                        ..CpuState::default()
                    };
                    initial.regs[0] = 0xabcd_9876;
                    initial.regs[6] = BASE + 0x10;
                    initial.x87.control = 0x7f | pc << 8 | rc << 10;
                    initial.x87.status = (rng.next() as u16 & 0x4700) | 0x3f;
                    initial.x87.set_top(case & 7);
                    let mut a = values[case % values.len()];
                    let mut b = if case % 3 == 0 { a } else { extended(3.0) };
                    match case.checked_sub(values.len()) {
                        Some(2) => a = Ext::from_parts((1 << 63) | 1, 0x7fff),
                        Some(3) => a = Ext::from_parts(1, 0),
                        Some(4) => {
                            a = extended(1.0);
                            b = extended(0.0);
                        }
                        Some(5) => {
                            a = Ext::from_parts(u64::MAX, 0x7ffe);
                            b = extended(0.5);
                        }
                        Some(6) => a = Ext::from_parts(1 << 63, 1),
                        Some(7) => a = extended(1.0),
                        _ => {}
                    }
                    for (logical, value) in [a, b].into_iter().enumerate() {
                        let slot = initial.x87.physical_index(logical);
                        initial.x87.regs[slot] = value;
                        initial.x87.set_physical_tag(slot, value.tag());
                    }
                    initial.x87.ip = CODE - 12;
                    initial.x87.dp = BASE + 0x28;
                    initial.x87.opcode = 0x321;
                    initial.x87.cs = 0x1234;
                    initial.x87.ds = 0x5678;
                    match case.checked_sub(values.len()) {
                        Some(0) => initial.x87.tag = 0xffff,
                        Some(1) => initial.x87.tag = 0,
                        Some(n @ 2..=7) => {
                            initial.x87.control &= !(1 << (n - 2));
                            initial.x87.status &= !0x3f;
                        }
                        Some(8) => initial.x87.status |= 0x8084,
                        Some(9) => initial.mmx_mode = 1,
                        Some(10) => initial.regs[6] = BASE + 0xffc,
                        Some(11) => initial.regs[6] = BASE + 0x3010,
                        Some(12) => {
                            initial.x87.tag = 0xffff;
                            initial.x87.control &= !1;
                            initial.x87.status &= !0x3f;
                        }
                        Some(13) => {
                            initial.x87.tag = 0;
                            initial.x87.control &= !1;
                            initial.x87.status &= !0x3f;
                        }
                        Some(14) => {
                            initial.x87.control &= !4;
                            initial.x87.status |= 0x8084;
                        }
                        _ => {}
                    }
                    if initial.mmx_mode == 0 {
                        initial.refresh_mmx();
                    }
                    let mut reference = memory(a.significand(), &bytes);
                    let mut actual_memory = memory(a.significand(), &bytes);
                    for mem in [&mut reference, &mut actual_memory] {
                        mem.write(BASE + 0x10, &a.0).unwrap();
                        if instruction.mnemonic() == M::Fidiv {
                            mem.write(BASE + 0x10, &3i32.to_le_bytes()).unwrap();
                        }
                        if mode != 0 {
                            mem.watch_code(CODE, bytes.len());
                        }
                    }
                    let epoch = actual_memory.epoch;
                    let original_bytes = actual_memory.read_data(BASE, 0x3000).unwrap().to_vec();
                    let mut expected = initial.clone();
                    let step = crate::floating::execute(
                        &instruction,
                        crate::floating::Operation::X87,
                        opcode,
                        &mut expected,
                        &mut reference,
                        true,
                    );
                    let mut actual = initial.clone();
                    if let Some(compiled) = &compiled {
                        let result = testing::run(compiled, &mut actual, &mut actual_memory);
                        let exit = match step {
                            Step::Executed
                                if reference.epoch != epoch || expected.x87.status & 0x80 != 0 =>
                            {
                                crate::Exit::Fallback
                            }
                            Step::Executed => crate::Exit::Dispatch,
                            Step::ReadFault { address, .. } => {
                                expected.fault_addr = address;
                                crate::Exit::ReadFault
                            }
                            Step::WriteFault { address, .. } => {
                                expected.fault_addr = address;
                                crate::Exit::WriteFault
                            }
                            Step::Trap { .. } => crate::Exit::Fallback,
                            _ => panic!("unexpected reference exit"),
                        };
                        assert_eq!(
                            result, exit as u32,
                            "form {form}, PC {pc}, RC {rc}, case {case}"
                        );
                        expected.fuel = actual.fuel;
                    } else {
                        let result = match op.fast {
                            Some(fast) => execute(&op, fast, &mut actual, &mut actual_memory),
                            None => execute_other(&op, &mut actual, &mut actual_memory),
                        };
                        if result.is_none() {
                            assert_eq!(actual, initial);
                            assert_eq!(actual_memory.epoch, epoch);
                            assert_eq!(
                                actual_memory.read_data(BASE, 0x3000).unwrap().as_ref(),
                                original_bytes
                            );
                            continue;
                        }
                        hits[form] += 1;
                        assert_eq!(step, Step::Executed);
                        assert_eq!(
                            result,
                            Some(if reference.epoch != epoch {
                                X87_EXIT
                            } else {
                                X87_DONE
                            })
                        );
                    }
                    assert_eq!(
                        actual, expected,
                        "form {form}, PC {pc}, RC {rc}, case {case}"
                    );
                    assert_eq!(actual_memory.epoch, reference.epoch);
                    assert_eq!(
                        actual_memory.read_data(BASE, 0x3000).unwrap(),
                        reference.read_data(BASE, 0x3000).unwrap()
                    );
                }
            }
        }
    }
    if mode == 0 {
        assert!(hits.iter().all(|n| *n > 0), "missing admissions: {hits:?}");
    }
    testing::SINGLE_ONLY.set(false);
    testing::HELPER_ONLY.set(false);
}

#[test]
fn raw80_stores_round_once_in_100000_random_cases() {
    use crate::floating::softfloat as sf;
    let mut rng = Rng(0x479a_269d_80bd_7813);
    let mut hits = [[0; 4]; 4];
    for n in 0..100_000 {
        let kind = n % 4;
        let rc = (n / 4 % 4) as u16;
        let control = 0x7f | ((n / 16 % 4) as u16) << 8 | rc << 10;
        let integer = kind >= 2;
        let width = if kind % 2 == 0 { 4 } else { 8 };
        let exponent = match n % 3 {
            0 => rng.next() as i32 % 2000,
            1 => rng.next() as i32 % 65,
            _ => rng.next() as i32 % 130,
        };
        let value = Ext::from_parts(
            rng.next() | 1 << 63,
            (exponent + 0x3fff) as u16 | (rng.next() as u16 & 0x8000),
        );
        let Some((bits, exact, up)) = raw::store_bits(value, width, integer, control) else {
            continue;
        };
        hits[kind][rc as usize] += 1;
        sf::configure(control);
        let expected = sf::to(value, kind as u8, sf::rounding(rc));
        let mask = if width == 4 {
            u64::from(u32::MAX)
        } else {
            u64::MAX
        };
        assert_eq!(
            bits & mask,
            expected & mask,
            "kind {kind}, rc {rc}, case {n}"
        );
        assert_eq!(sf::flags(), if exact { 0 } else { PE });
        assert_eq!(sf::rounded_up(), up);
    }
    assert!(hits.iter().flatten().all(|&n| n > 100), "{hits:?}");
}
