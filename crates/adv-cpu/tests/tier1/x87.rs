//! Authored x87 fixtures; Tier 0/Cranelift compare full state, with Unicorn as
//! an additional independent oracle for exact cases it represents faithfully.
use super::*;
use adv_cpu::X87Register as Ext;
#[path = "../support/mod.rs"]
mod reference;

type Asm = fn(&mut CodeAssembler) -> Result<(), IcedError>;

fn programs() -> Vec<Asm> {
    vec![
        |a| a.fld(dword_ptr(esi)),
        |a| a.fld(qword_ptr(esi)),
        |a| a.fld(st2),
        |a| a.fst(dword_ptr(esi)),
        |a| a.fst(qword_ptr(esi)),
        |a| a.fstp(dword_ptr(esi)),
        |a| a.fstp(qword_ptr(esi)),
        |a| a.fstp(st2),
        |a| a.fadd(dword_ptr(esi)),
        |a| a.fadd(qword_ptr(esi)),
        |a| a.fadd_2(st0, st2),
        |a| a.fadd_2(st2, st0),
        |a| a.fsub(dword_ptr(esi)),
        |a| a.fsub(qword_ptr(esi)),
        |a| a.fsub_2(st0, st2),
        |a| a.fsub_2(st2, st0),
        |a| a.fsubr(dword_ptr(esi)),
        |a| a.fsubr(qword_ptr(esi)),
        |a| a.fsubr_2(st0, st2),
        |a| a.fsubr_2(st2, st0),
        |a| a.fmul(dword_ptr(esi)),
        |a| a.fmul(qword_ptr(esi)),
        |a| a.fmul_2(st0, st2),
        |a| a.fmul_2(st2, st0),
        |a| a.fxch(st0, st2),
    ]
}

fn seed(m: &mut Executor, control: u16, top: usize) {
    reset(m, [3, 5, 7, 11, 0, 13, DATA, 17], 0xcd7);
    m.state.x87.control = control;
    m.state.x87.set_top(top);
    for logical in 0..3 {
        let slot = m.state.x87.physical_index(logical);
        m.state.x87.regs[slot] = Ext::from_parts(1 << 63, 0x4001 - logical as u16);
        m.state.x87.set_physical_tag(slot, 0);
    }
    m.state.x87.status |= 0x4500;
    m.state.xmm = [[0xa5; 16]; 8];
    m.state.refresh_mmx();
}

fn three(code: &[u8]) -> [Executor; 3] {
    let mut machines = [machine(code, 0), machine(code, 1), machine(code, 2)];
    machines[2].set_tier_verification(Some(true));
    machines
}

fn compare(machines: &mut [Executor; 3]) -> RunOutcome {
    let outcomes: Vec<_> = machines
        .iter_mut()
        .map(|m| m.run(STOP, 100).unwrap())
        .collect();
    assert_eq!(outcomes[0], outcomes[1]);
    assert_eq!(outcomes[0], outcomes[2]);
    equivalent(&machines[0], &machines[1], u32::MAX);
    equivalent(&machines[0], &machines[2], u32::MAX);
    assert_eq!(machines[0].state.mmx, machines[2].state.mmx);
    assert_eq!(machines[0].state.mmx_mode, machines[2].state.mmx_mode);
    assert_eq!(machines[2].stats.verification.mismatches, 0);
    outcomes.into_iter().next().unwrap()
}

#[test]
fn x87_all_forms_all_top_pc_rc_match_interpreter_cranelift_and_unicorn() {
    for build in programs() {
        let code = assemble(|a| {
            build(a).unwrap();
            a.ret().unwrap();
        });
        let instruction = iced_x86::Decoder::new(32, &code, 0).decode();
        let data = if instruction.memory_size().size() == 4 {
            2f32.to_le_bytes().to_vec()
        } else {
            2f64.to_le_bytes().to_vec()
        };
        let mut machines = three(&code);
        let mut oracle = reference::Executor::new(CODE, 0x30000).unwrap();
        oracle.protect(CODE, 0x30000, READ | WRITE).unwrap();
        oracle.write(CODE, &code).unwrap();
        oracle.protect(CODE, 4096, READ | EXEC).unwrap();
        oracle.protect(STOP, 4096, READ | EXEC).unwrap();
        oracle.write(STACK, &STOP.to_le_bytes()).unwrap();
        for pc in [0, 1, 2, 3] {
            for rc in 0..4 {
                for top in 0..8 {
                    let control = 0x7f | (pc << 8) | (rc << 10);
                    for m in &mut machines {
                        seed(m, control, top);
                        m.write(DATA, &data).unwrap();
                    }
                    oracle.state = machines[0].state.clone();
                    oracle.write(DATA, &data).unwrap();
                    assert_eq!(compare(&mut machines), RunOutcome::ReachedStop);
                    assert_eq!(
                        oracle.run_unicorn(STOP, 100).unwrap(),
                        RunOutcome::ReachedStop
                    );
                    let actual = &machines[2].state;
                    assert_eq!(actual.regs, oracle.state.regs);
                    assert_eq!(actual.eflags, oracle.state.eflags);
                    assert_eq!(actual.x87.regs, oracle.state.x87.regs);
                    assert_eq!(actual.x87.tag, oracle.state.x87.tag);
                    assert_eq!(actual.x87.control, oracle.state.x87.control);
                    assert_eq!(actual.x87.status, oracle.state.x87.status);
                    // Unicorn does not maintain the same FOP/FDP metadata;
                    // these remain fully compared against Tier 0 above.
                    assert_eq!(
                        machines[2].memory().read_data(DATA, 8).unwrap(),
                        oracle.memory().read_data(DATA, 8).unwrap()
                    );
                }
            }
        }
        // RC=nearest exact values must have real helper successes, even though
        // unsupported PC/RC combinations deliberately exercised Tier 0 too.
        assert!(machines[2].tier1_stats().native_instructions > 128);
    }
}

#[test]
fn x87_special_values_narrowing_and_rounding_keep_full_transaction_semantics() {
    let values = [
        Ext::from_parts(0, 0),
        Ext::from_parts(0, 0x8000),
        Ext::from_parts(1, 0),                          // Extended denormal.
        Ext::from_parts(1 << 63, 0x7fff),               // Infinity.
        Ext::from_parts(0xc000_0000_0000_1234, 0x7fff), // QNaN.
        Ext::from_parts(0x8000_0000_0000_1234, 0x7fff), // SNaN.
        Ext::from_parts(u64::MAX, 0x7ffe),              // Extended overflow boundary.
        Ext::from_parts(1 << 63, 0x3bcd),               // Below binary64 normal range.
        Ext::from_parts(0x8000_0000_0000_0001, 0x3fff), // Beyond binary64 precision.
        Ext::from_parts(0x8000_0080_0000_0000, 0x3fff), // binary32 tie.
    ];
    for build in programs() {
        let code = assemble(|a| {
            build(a).unwrap();
            a.ret().unwrap();
        });
        let mut machines = three(&code);
        for pc in [0, 2, 3] {
            for rc in 0..4 {
                for (case, value) in values.into_iter().enumerate() {
                    for m in &mut machines {
                        seed(m, 0x7f | (pc << 8) | (rc << 10), case & 7);
                        for logical in [0, 2] {
                            let slot = m.state.x87.physical_index(logical);
                            m.state.x87.regs[slot] = value;
                            m.state.x87.set_physical_tag(slot, value.tag());
                        }
                        m.state.refresh_mmx();
                        // Alternate finite, denormal, NaN and infinity memory operands.
                        let words = [
                            1u64,
                            0x7ff0_0000_0000_0001,
                            0x7ff8_0000_0000_1234,
                            0x7ff0_0000_0000_0000,
                            0x3ff0_0000_0000_0000,
                        ];
                        let singles = [1u32, 0x7f80_0001, 0x7fc0_1234, 0x7f80_0000, 0x3f80_0000];
                        let width = iced_x86::Decoder::new(32, &code, 0)
                            .decode()
                            .memory_size()
                            .size();
                        let data = if width == 4 {
                            singles[case % 5].to_le_bytes().to_vec()
                        } else {
                            words[case % 5].to_le_bytes().to_vec()
                        };
                        m.write(DATA, &data).unwrap();
                    }
                    assert_eq!(
                        compare(&mut machines),
                        RunOutcome::ReachedStop,
                        "case {case}"
                    );
                }
            }
        }
        assert!(machines[2].tier1_stats().fallback_instructions > 0);
    }
}

#[test]
fn x87_stack_faults_unmasked_exceptions_and_pending_mmx_exit_precisely() {
    for build in programs() {
        let code = assemble(|a| {
            build(a).unwrap();
            a.ret().unwrap();
        });
        let mut machines = three(&code);
        for case in 0..7 {
            for mask in [0x3f, 0x3e, 0x1f, 0] {
                for m in &mut machines {
                    seed(m, 0x340 | mask, 7);
                    m.write(DATA, &0x7ff0_0000_0000_0001u64.to_le_bytes())
                        .unwrap();
                    match case {
                        0 => m.state.x87.tag = 0xffff,
                        1 => m.state.x87.tag = 0,
                        2 => m.state.x87.status |= 0x8081,
                        3 => m.state.mmx_mode = 1,
                        4 => m.state.mmx_mode = 2,
                        5 => {
                            let slot = m.state.x87.top();
                            m.state.x87.regs[slot] = Ext::from_parts(u64::MAX, 0x7ffe);
                        }
                        _ => {
                            let slot = m.state.x87.top();
                            m.state.x87.regs[slot] = Ext::from_parts(1, 0);
                            m.state.x87.set_physical_tag(slot, 2);
                        }
                    }
                    if case < 3 || case > 4 {
                        m.state.refresh_mmx();
                    }
                }
                compare(&mut machines);
            }
        }
    }
}

#[test]
fn x87_pc64_residual_and_budgeted_mixed_integer_path_do_not_lose_precision() {
    let code = assemble(|a| {
        a.mov(esi, DATA).unwrap();
        a.add(esi, 8).unwrap();
        a.fld(qword_ptr(esi)).unwrap();
        a.fadd(qword_ptr(esi + 8)).unwrap();
        a.fstp(qword_ptr(esi + 16)).unwrap();
        a.adc(eax, 7).unwrap();
        a.ret().unwrap();
    });
    let mut machines = three(&code);
    for m in &mut machines {
        seed(m, 0x37f, 1);
        m.write(DATA + 8, &1f64.to_le_bytes()).unwrap();
        m.write(DATA + 16, &2f64.powi(-60).to_le_bytes()).unwrap();
    }
    // Both exact-count lanes must retire exactly the same prefix at each call.
    for _ in 0..4 {
        for index in [0, 2] {
            assert_eq!(machines[index].run(STOP, 1).unwrap(), RunOutcome::Budget);
        }
        equivalent(&machines[0], &machines[2], u32::MAX);
    }
    let st = machines[2].state.x87.physical_index(0);
    assert_eq!(
        machines[2].state.x87.regs[st],
        Ext::from_parts((1 << 63) | 8, 0x3fff)
    );
    assert_eq!(machines[2].tier1_stats().fallback_instructions, 1);
    assert_eq!(compare(&mut machines), RunOutcome::ReachedStop);
}

#[test]
fn x87_cross_page_faults_preserve_stack_and_memory_then_resume() {
    for build in [
        programs()[0],
        programs()[1],
        programs()[3],
        programs()[4],
        programs()[5],
        programs()[6],
        programs()[8],
        programs()[21],
    ] {
        let code = assemble(|a| {
            a.inc(ebx).unwrap();
            build(a).unwrap();
            a.ret().unwrap();
        });
        let mut machines = three(&code);
        for m in &mut machines {
            seed(m, 0x27f, 7);
            m.state.regs[6] = DATA + 4094;
            m.write(DATA + 4094, &2f64.to_le_bytes()).unwrap();
            m.protect(DATA + 4096, 4096, 0).unwrap();
        }
        let before = machines[0].state.x87.clone();
        assert!(matches!(
            compare(&mut machines),
            RunOutcome::ReadFault { .. } | RunOutcome::WriteFault { .. }
        ));
        assert_eq!(machines[2].state.x87, before);
        assert_eq!(machines[2].state.regs[3], 12);
        assert_eq!(machines[2].tier1_stats().native_instructions, 1);
        for m in &mut machines {
            m.protect(DATA + 4096, 4096, READ | WRITE).unwrap();
        }
        assert_eq!(compare(&mut machines), RunOutcome::ReachedStop);
    }
}

#[test]
fn x87_fs_sib_address_and_same_page_data_store_dispatch_with_live_state() {
    let code = assemble(|a| {
        a.mov(esi, DATA - 128).unwrap();
        a.mov(ecx, 2).unwrap();
        a.fld(qword_ptr(esi + ecx * 4 + 120).fs()).unwrap();
        a.fstp(qword_ptr(edi)).unwrap();
        a.ret().unwrap();
    });
    let mut machines = three(&code);
    for m in &mut machines {
        seed(m, 0x27f, 3);
        m.state.fs_base = 64;
        m.state.regs[7] = CODE + 256;
        m.write(DATA + 64, &3f64.to_le_bytes()).unwrap();
        m.protect(CODE, 4096, READ | WRITE | EXEC).unwrap();
    }
    assert_eq!(compare(&mut machines), RunOutcome::ReachedStop);
    for m in &machines {
        assert_eq!(
            m.memory().read_data(CODE + 256, 8).unwrap().as_ref(),
            3f64.to_le_bytes()
        );
    }
    assert_eq!(machines[2].tier1_stats().fallback_instructions, 0);
    // The store targets padding beyond RET. Event validation retains the
    // original unit after checking its unchanged decoded input.
    if std::env::var("ADV_CODE_VALIDITY").as_deref() == Ok("event") {
        assert_eq!(machines[2].tier1_stats().invalidations, 0);
        assert_eq!(machines[2].tier1_stats().compilations, 1);
    } else {
        assert!(machines[2].tier1_stats().invalidations > 0);
    }
}

#[test]
fn x87_store_patches_next_native_immediate_without_replaying_live_state() {
    let code = assemble(|a| {
        a.fld(qword_ptr(DATA as i32)).unwrap();
        a.fstp(dword_ptr(edi)).unwrap();
        a.mov(eax, 0x1234).unwrap();
        a.ret().unwrap();
    });
    let target =
        iced_x86::Decoder::with_ip(32, &code, u64::from(CODE), iced_x86::DecoderOptions::NONE)
            .into_iter()
            .find(|i| i.mnemonic() == iced_x86::Mnemonic::Mov)
            .unwrap();
    let mut machines = three(&code);
    for m in &mut machines {
        seed(m, 0x27f, 3);
        m.state.regs[7] = target.ip32() + 1;
        m.write(DATA, &3f64.to_le_bytes()).unwrap();
        m.protect(CODE, 4096, READ | WRITE | EXEC).unwrap();
    }
    assert_eq!(compare(&mut machines), RunOutcome::ReachedStop);
    assert_eq!(machines[2].state.regs[0], 3f32.to_bits());
    assert_eq!(machines[2].tier1_stats().fallback_instructions, 0);
    assert!(machines[2].tier1_stats().invalidations > 0);
}
