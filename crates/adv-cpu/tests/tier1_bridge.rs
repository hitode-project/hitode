//! Authored bridge fixtures; native evidence requires Linux/Android ARM64.
use adv_cpu::{Executor, Tier1Mode};

#[test]
fn mode_names_and_unsupported_platform_are_explicit() {
    for (name, mode) in [
        ("off", Tier1Mode::Off),
        ("exclusive", Tier1Mode::Exclusive),
        ("bridge", Tier1Mode::Bridge),
    ] {
        assert_eq!(name.parse::<Tier1Mode>().unwrap(), mode);
    }
    assert!("auto".parse::<Tier1Mode>().is_err());
    let mut m = Executor::new(0, 4096).unwrap();
    assert_eq!(m.tier1_mode(), Tier1Mode::Off);
    if !cfg!(all(
        target_arch = "aarch64",
        any(target_os = "linux", target_os = "android")
    )) {
        for mode in [Tier1Mode::Exclusive, Tier1Mode::Bridge] {
            assert!(m.set_tier1_mode(mode).is_err());
            assert_eq!(m.tier1_mode(), Tier1Mode::Off);
        }
    }
}

#[cfg(all(
    target_arch = "aarch64",
    any(target_os = "linux", target_os = "android")
))]
mod native {
    use super::*;
    use adv_cpu::{CpuState, EXEC, READ, Region, RunOutcome, WRITE};
    use iced_x86::code_asm::*;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use std::time::{Duration, Instant};
    const CODE: u32 = 0x10000;
    const STOP: u32 = 0x1f000;
    const DATA: u32 = 0x20000;
    const STACK: u32 = 0x3f000;

    fn assemble(f: impl FnOnce(&mut CodeAssembler)) -> Vec<u8> {
        let mut a = CodeAssembler::new(32).unwrap();
        f(&mut a);
        a.assemble(u64::from(CODE)).unwrap()
    }
    fn machine(code: &[u8], mode: Tier1Mode) -> Executor {
        let mut m = Executor::new(CODE, 0x30000).unwrap();
        m.protect(CODE, 0x30000, READ | WRITE).unwrap();
        m.write(CODE, code).unwrap();
        m.protect(CODE, 4096, READ | EXEC).unwrap();
        m.protect(STOP, 4096, READ | EXEC).unwrap();
        m.set_tier1_mode(mode).unwrap();
        reset(&mut m);
        m
    }
    fn reset(m: &mut Executor) {
        m.state = CpuState::default();
        m.state.regs[4] = STACK;
        m.state.eip = CODE;
        m.write(STACK, &STOP.to_le_bytes()).unwrap();
    }
    fn equal(a: &Executor, b: &Executor) {
        assert_eq!(a.state.regs, b.state.regs);
        assert_eq!(a.state.eip, b.state.eip);
        assert_eq!(a.state.eflags, b.state.eflags);
        assert_eq!(a.state.x87, b.state.x87);
        assert_eq!(a.state.xmm, b.state.xmm);
        assert_eq!(a.state.mxcsr, b.state.mxcsr);
        assert_eq!(a.state.fs_base, b.state.fs_base);
        for page in [CODE, DATA, STACK] {
            assert_eq!(
                a.memory().read_data(page, 4096).unwrap(),
                b.memory().read_data(page, 4096).unwrap()
            );
        }
    }
    fn loop_code() -> Vec<u8> {
        assemble(|a| {
            let mut top = a.create_label();
            a.set_label(&mut top).unwrap();
            a.inc(eax).unwrap();
            a.mov(dword_ptr(DATA), eax).unwrap();
            a.dec(ecx).unwrap();
            a.jne(top).unwrap();
            a.ret().unwrap();
        })
    }

    #[test]
    fn bridge_hands_off_to_async_cranelift_and_both_verify() {
        for strict in [false, true] {
            let code = loop_code();
            let mut bridge = machine(&code, Tier1Mode::Bridge);
            bridge.set_tier_verification(strict.then_some(true));
            bridge.enable_auto_jit(true);
            bridge.set_async_compilation(true);
            bridge.set_compile_threads(1).unwrap();
            // Completion wakeups are controller events; this fixture drives the
            // executor directly and tests host interrupts separately below.
            bridge.set_host_interrupts(false);
            bridge.state.regs[1] = u32::MAX;
            assert_eq!(bridge.run(STOP, 256).unwrap(), RunOutcome::Budget);
            assert_eq!(bridge.tier1_stats().bridge_units, 256);
            assert_eq!(bridge.stats.compilations, 0);
            let until = Instant::now() + Duration::from_secs(20);
            while bridge.stats.compilations == 0 {
                assert!(Instant::now() < until, "asynchronous publication timed out");
                assert_eq!(bridge.run(STOP, 256).unwrap(), RunOutcome::Budget);
                std::thread::yield_now();
            }
            assert!(bridge.stats.compile_requests > 0);
            assert_eq!(bridge.stats.compile_failures, 0);
            let taken = bridge.tier1_stats().bridge_units;
            let checks = bridge.stats.verification.verified;
            bridge.set_tier_verification(strict.then_some(true));
            let mut reference = machine(&code, Tier1Mode::Off);
            let mut cranelift = machine(&code, Tier1Mode::Off);
            cranelift.set_region(Region::new(CODE, code.len())).unwrap();
            for m in [&mut reference, &mut cranelift, &mut bridge] {
                reset(m);
                m.state.regs[1] = 40;
                assert_eq!(m.run(STOP, 10000).unwrap(), RunOutcome::ReachedStop);
            }
            equal(&reference, &cranelift);
            equal(&reference, &bridge);
            assert_eq!(
                bridge.tier1_stats().bridge_units,
                taken,
                "published Cranelift must take priority"
            );
            assert_eq!(bridge.stats.cliffs.tier1_bridge.instructions, taken);
            assert_eq!(
                bridge
                    .stats
                    .cliffs
                    .tier1_bridge_rows()
                    .iter()
                    .map(|(_, n)| n.instructions)
                    .sum::<u64>(),
                taken
            );
            assert_eq!(bridge.stats.verification.mismatches, 0);
            if strict {
                assert!(checks > 0);
                assert!(
                    bridge.stats.verification.verified > checks,
                    "Cranelift must also be verified"
                );
            }
        }
    }

    #[test]
    fn bridge_cached_cfg_yields_before_a_published_interior_entry() {
        let prefix = assemble(|a| {
            a.inc(ebx).unwrap();
            a.jmp(u64::from(CODE + 128)).unwrap();
        });
        let body = assemble(|a| {
            a.inc(eax).unwrap();
            a.ret().unwrap();
        });
        let mut machines = [
            machine(&prefix, Tier1Mode::Off),
            machine(&prefix, Tier1Mode::Off),
            machine(&prefix, Tier1Mode::Bridge),
        ];
        for m in &mut machines {
            m.set_tier_verification(Some(true));
            m.write(CODE + 128, &body).unwrap();
            assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
            reset(m);
        }
        let taken = machines[2].tier1_stats().bridge_units;
        for m in &mut machines[1..] {
            m.add_region(Region::new(CODE + 128, body.len())).unwrap();
        }
        for m in &mut machines {
            assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
            assert_eq!(m.stats.verification.mismatches, 0);
        }
        equal(&machines[0], &machines[1]);
        equal(&machines[0], &machines[2]);
        assert_eq!(machines[2].tier1_stats().bridge_units - taken, 2);
        assert!(machines[2].tier1_stats().cache_evictions > 0);
    }

    #[test]
    fn publication_keeps_disjoint_cfgs_and_retires_every_overlapping_owner() {
        let prefix = assemble(|a| {
            a.inc(ebx).unwrap();
            a.jmp(u64::from(CODE + 128)).unwrap();
        });
        let mut side = CodeAssembler::new(32).unwrap();
        side.inc(ecx).unwrap();
        side.jmp(u64::from(CODE + 128)).unwrap();
        let side = side.assemble(u64::from(CODE + 64)).unwrap();
        let leaf = assemble(|a| {
            a.inc(eax).unwrap();
            a.ret().unwrap();
        });
        let mut m = machine(&prefix, Tier1Mode::Bridge);
        m.write(CODE + 64, &side).unwrap();
        for offset in [128, 256, 512] {
            m.write(CODE + offset, &leaf).unwrap();
        }
        m.set_tier_verification(Some(true));
        // Two CFGs own the shared leaf; the later one overwrites its entry-map
        // slot. A third CFG is independent and must survive both publications.
        for offset in [0, 64, 256] {
            reset(&mut m);
            m.state.eip = CODE + offset;
            assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
        }
        let before = m.tier1_stats();
        assert_eq!(before.compilations, 3);
        m.add_region(Region::new(CODE + 512, leaf.len())).unwrap();
        reset(&mut m);
        m.state.eip = CODE + 256;
        assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
        let unrelated = m.tier1_stats();
        assert_eq!(unrelated.compilations, before.compilations);
        assert_eq!(unrelated.cache_evictions, before.cache_evictions);
        assert_eq!(unrelated.resident_bytes, before.resident_bytes);

        m.add_region(Region::new(CODE + 128, leaf.len())).unwrap();
        reset(&mut m);
        m.state.eip = CODE + 256;
        assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
        let conflict = m.tier1_stats();
        assert_eq!(conflict.compilations, before.compilations);
        assert_eq!(conflict.cache_evictions - before.cache_evictions, 2);
        assert!(conflict.resident_bytes < before.resident_bytes);
        assert_eq!(conflict.publication_flushes, 0);
        for offset in [0, 64] {
            reset(&mut m);
            m.state.eip = CODE + offset;
            let units = m.tier1_stats().bridge_units;
            assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
            assert_eq!(m.tier1_stats().bridge_units - units, 2);
            assert_eq!(m.state.regs[0], 1);
        }
        assert_eq!(m.stats.verification.mismatches, 0);
    }

    #[test]
    fn bridge_self_modifying_code_invalidates_both_caches() {
        let body = assemble(|a| {
            a.mov(eax, 1).unwrap();
            a.ret().unwrap();
        });
        let replacement = assemble(|a| {
            a.mov(eax, 7).unwrap();
            a.ret().unwrap();
        });
        let prefix = assemble(|a| {
            a.mov(dword_ptr(CODE + 129), 7u32).unwrap();
            a.jmp(u64::from(CODE + 128)).unwrap();
        });
        let mut machines = [
            machine(&prefix, Tier1Mode::Off),
            machine(&prefix, Tier1Mode::Off),
            machine(&prefix, Tier1Mode::Bridge),
        ];
        for m in &mut machines {
            m.write(CODE + 128, &body).unwrap();
            m.protect(CODE, 4096, READ | WRITE | EXEC).unwrap();
            // Populate the bridge cache before publishing Cranelift for the body.
            m.state.eip = CODE + 128;
            assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
            reset(m);
        }
        for m in &mut machines[1..] {
            m.add_region(Region::new(CODE + 128, body.len())).unwrap();
        }
        for m in &mut machines {
            assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
            assert_eq!(m.state.regs[0], 7);
        }
        equal(&machines[0], &machines[1]);
        equal(&machines[0], &machines[2]);
        assert!(machines[2].stats.invalidations > 0);
        // Re-enter an unowned bridge entry after both host writes and protection changes.
        for m in &mut machines {
            m.write(CODE, &replacement).unwrap();
            reset(m);
            assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
        }
        equal(&machines[0], &machines[2]);
        assert!(machines[2].tier1_stats().invalidations > 0);
        machines[2].protect(CODE, 4096, READ).unwrap();
        reset(&mut machines[2]);
        assert!(matches!(
            machines[2].run(STOP, 100).unwrap(),
            RunOutcome::FetchFault { .. }
        ));
    }

    #[test]
    fn bridge_budget_rep_stop_and_interrupt_match_tier0() {
        let code = assemble(|a| {
            a.rep().movsb().unwrap();
            a.inc(eax).unwrap();
            a.ret().unwrap();
        });
        let mut pair = [
            machine(&code, Tier1Mode::Off),
            machine(&code, Tier1Mode::Bridge),
        ];
        let interrupt = Arc::new(AtomicBool::new(true));
        for m in &mut pair {
            m.set_tier_verification(Some(true));
            m.state.regs[1] = 11;
            m.state.regs[6] = DATA;
            m.state.regs[7] = DATA + 64;
            m.write(DATA, &[3; 11]).unwrap();
            m.set_host_interrupt_flag(interrupt.clone());
            assert_eq!(m.run(STOP, 20).unwrap(), RunOutcome::Interrupted);
            assert_eq!(m.state.eip, CODE);
        }
        interrupt.store(false, Ordering::Release);
        for budget in [0, 1, 3, 2, 5] {
            for m in &mut pair {
                assert_eq!(m.run(STOP, budget).unwrap(), RunOutcome::Budget);
            }
            equal(&pair[0], &pair[1]);
        }
        for m in &mut pair {
            assert_eq!(m.run(STOP, 20).unwrap(), RunOutcome::ReachedStop);
        }
        equal(&pair[0], &pair[1]);
        assert_eq!(pair[1].tier1_stats().bridge_units, 13);
        assert_eq!(pair[1].stats.verification.mismatches, 0);
    }

    #[test]
    fn bridge_observes_interrupt_during_a_native_loop() {
        let code = assemble(|a| {
            let mut top = a.create_label();
            a.set_label(&mut top).unwrap();
            a.inc(eax).unwrap();
            a.jmp(top).unwrap();
        });
        let mut m = machine(&code, Tier1Mode::Bridge);
        m.set_tier_verification(None);
        assert_eq!(m.run(STOP, 256).unwrap(), RunOutcome::Budget);
        let flag = Arc::new(AtomicBool::new(false));
        m.set_host_interrupt_flag(flag.clone());
        let thread = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(10));
            flag.store(true, Ordering::Release);
        });
        assert_eq!(m.run(STOP, u64::MAX).unwrap(), RunOutcome::Interrupted);
        thread.join().unwrap();
        assert_eq!(m.tier1_stats().bridge_units % 256, 0);
    }

    #[test]
    fn bridge_forced_steps_and_pending_fpu_never_enter_native() {
        for pending in [false, true] {
            let code = assemble(|a| {
                a.inc(eax).unwrap();
                a.wait().unwrap();
                a.ret().unwrap();
            });
            let mut pair = [
                machine(&code, Tier1Mode::Off),
                machine(&code, Tier1Mode::Bridge),
            ];
            for m in &mut pair {
                if pending {
                    m.state.x87.status |= 0x81;
                    m.state.x87.control &= !1;
                } else {
                    m.state.eflags |= 0x100;
                }
            }
            assert_eq!(
                pair[0].run(STOP, 100).unwrap(),
                pair[1].run(STOP, 100).unwrap()
            );
            equal(&pair[0], &pair[1]);
            assert_eq!(pair[1].tier1_stats().native_entries, 0);
            for m in &mut pair {
                m.state.eflags &= !0x100;
                m.state.x87.status = 0;
                m.state.x87.control |= 1;
                assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
            }
            equal(&pair[0], &pair[1]);
            assert!(pair[1].tier1_stats().bridge_units > 0);
        }
    }

    #[test]
    fn bridge_checked_memory_and_fallback_work_with_direct_reservation() {
        // CI runs this whole suite with ADV_DIRECT_MEMORY off and on. Explicit
        // enablement also exercises selection after Bridge, before execution.
        let code = assemble(|a| {
            a.mov(dword_ptr(DATA), 71u32).unwrap();
            a.cbw().unwrap();
            a.mov(ebx, dword_ptr(DATA)).unwrap();
            a.ret().unwrap();
        });
        let mut pair = [
            machine(&code, Tier1Mode::Off),
            machine(&code, Tier1Mode::Bridge),
        ];
        pair[1].set_direct_memory(true).unwrap();
        for m in &mut pair {
            m.set_tier_verification(Some(true));
            m.state.regs[0] = 0x80;
            assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
        }
        equal(&pair[0], &pair[1]);
        // Re-selecting after execution succeeds only if the reservation is
        // already active; switching a used checked owner would be rejected.
        assert!(pair[1].set_direct_memory(true).is_ok());
        assert!(pair[1].tier1_stats().bridge_units > 0);
        assert!(pair[1].stats.interpreter_instructions > 0);
        assert_eq!(pair[1].stats.verification.mismatches, 0);
        assert!(pair[1].set_guest_workers(2).is_err());
    }

    #[test]
    fn bridge_cache_limits_evict_and_reject_without_losing_state() {
        let code = assemble(|a| {
            a.inc(eax).unwrap();
            a.ret().unwrap();
        });
        let mut pair = [
            machine(&code, Tier1Mode::Off),
            machine(&code, Tier1Mode::Bridge),
        ];
        pair[1].set_tier1_cache_limits(2, 65536);
        for m in &mut pair {
            for offset in [0, 128, 256] {
                m.write(CODE + offset, &code).unwrap();
            }
        }
        for offset in [0, 128, 256, 0] {
            for m in &mut pair {
                reset(m);
                m.state.eip += offset;
                assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
            }
            equal(&pair[0], &pair[1]);
            assert!(pair[1].tier1_stats().resident_bytes <= 65536);
        }
        assert!(pair[1].tier1_stats().cache_evictions >= 3);
        // Reducing a live cache to zero also retires its final RX mapping.
        pair[1].set_tier1_cache_limits(0, 0);
        assert_eq!(pair[1].tier1_stats().resident_bytes, 0);
        assert!(pair[1].tier1_stats().cache_evictions > 0);
        for m in &mut pair {
            reset(m);
            assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
        }
        equal(&pair[0], &pair[1]);
        assert!(pair[1].tier1_stats().cache_rejections > 0);
    }
}
