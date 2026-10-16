use super::*;

const TABLE: u32 = CODE + 0x4000;
const HANDLERS: u32 = CODE + 0x1000;

fn program(byte_bound: bool, clobber: bool) -> Program {
    let mut p = fixture(|a| {
        a.movzx(eax, byte_ptr(esi)).unwrap();
        a.inc(esi).unwrap();
        if !byte_bound {
            a.cmp(eax, 7).unwrap();
            a.ja(CODE as u64 + 0x800).unwrap();
        }
        if clobber {
            a.mov(eax, 7).unwrap();
        }
        a.jmp(dword_ptr(eax * 4 + TABLE as i32)).unwrap();
    });
    p.code.resize(0x5000, 0x90);
    p.stop = CODE + 0x800;
    p.state.regs[6] = DATA;
    for i in 0..8 {
        let mut a = CodeAssembler::new(32).unwrap();
        a.add(edx, i + 1).unwrap();
        a.jmp(CODE as u64).unwrap();
        let bytes = a.assemble(u64::from(HANDLERS) + i as u64 * 32).unwrap();
        let offset = 0x1000 + i as usize * 32;
        p.code[offset..offset + bytes.len()].copy_from_slice(&bytes);
    }
    for i in 0..256 {
        let target = if i < 8 { HANDLERS + i * 32 } else { p.stop };
        p.code[0x4000 + i as usize * 4..0x4004 + i as usize * 4]
            .copy_from_slice(&target.to_le_bytes());
    }
    p.data[..9].copy_from_slice(&[0, 1, 2, 3, 4, 5, 6, 7, 255]);
    p
}

fn configured(p: &Program, config: Config, interpreted: bool) -> Executor {
    let mut e = executor(p, config, interpreted);
    e.memory_mut().protect(TABLE, 4096, 1).unwrap();
    e.images.borrow_mut().push((CODE, 0x5000));
    e
}

fn configs() -> impl Iterator<Item = Config> {
    [false, true].into_iter().flat_map(|flagm| {
        [false, true].into_iter().flat_map(move |first| {
            [Checks::Adaptive, Checks::All, Checks::Slow]
                .into_iter()
                .map(move |checks| (Features { flagm }, first, checks))
        })
    })
}

// spec: A64-CTL-6, A64-CTL-7, A64-VER-1
#[test]
fn switch_tables_match_all_handlers_default_and_budget_cuts() {
    for byte_bound in [false, true] {
        let p = program(byte_bound, false);
        for config in configs() {
            let mut native = configured(&p, config, false);
            let mut oracle = configured(&p, config, true);
            for budget in [1, 2, 3, 4, 5, 7, 17, 63, 256] {
                for e in [&mut native, &mut oracle] {
                    e.state = p.state.clone();
                }
                assert_eq!(
                    native.run(p.stop, budget).unwrap(),
                    oracle.run(p.stop, budget).unwrap()
                );
                same(&native, &oracle);
                assert_eq!(
                    native.run(p.stop, 256).unwrap(),
                    oracle.run(p.stop, 256).unwrap()
                );
                same(&native, &oracle);
            }
            let (rows, resolved) = native.native.as_ref().unwrap().switch_counts();
            assert!(rows >= if byte_bound { 256 } else { 8 });
            assert!(resolved >= 8, "resolved hot path must actually run");
        }
    }
}

// spec: A64-CTL-6, A64-CTL-10, A64-VER-1
#[test]
fn switch_table_writes_and_writable_protection_drop_the_source() {
    {
        for config in configs() {
            let p = program(false, false);
            let mut native = configured(&p, config, false);
            let mut oracle = configured(&p, config, true);
            for _ in 0..2 {
                for e in [&mut native, &mut oracle] {
                    e.state = p.state.clone();
                }
                assert_eq!(
                    native.run(p.stop, 256).unwrap(),
                    oracle.run(p.stop, 256).unwrap()
                );
                same(&native, &oracle);
            }
            let dropped = native.native.as_ref().unwrap().diagnostics.dropped_units;
            for e in [&mut native, &mut oracle] {
                e.memory_mut().protect(TABLE, 4096, 3).unwrap();
                e.state = p.state.clone();
            }
            // VirtualProtect alone must retire the read-only proof.
            assert_eq!(
                native.run(p.stop, 4).unwrap(),
                oracle.run(p.stop, 4).unwrap()
            );
            same(&native, &oracle);
            assert!(native.native.as_ref().unwrap().diagnostics.dropped_units > dropped);
            for e in [&mut native, &mut oracle] {
                e.memory_mut()
                    .write(TABLE, &(HANDLERS + 7 * 32).to_le_bytes())
                    .unwrap();
                e.state = p.state.clone();
            }
            assert_eq!(
                native.run(p.stop, 256).unwrap(),
                oracle.run(p.stop, 256).unwrap()
            );
            same(&native, &oracle);
            // Re-protect, compile again, then exercise watched-byte writes.
            for e in [&mut native, &mut oracle] {
                e.memory_mut().protect(TABLE, 4096, 1).unwrap();
                e.memory_mut().protect(CODE, 4096, 5).unwrap();
                e.state = p.state.clone();
            }
            assert_eq!(
                native.run(p.stop, 256).unwrap(),
                oracle.run(p.stop, 256).unwrap()
            );
            same(&native, &oracle);
            for e in [&mut native, &mut oracle] {
                // Memory::write is also the host/loader alias write path.
                e.memory_mut()
                    .write(TABLE, &(HANDLERS + 3 * 32).to_le_bytes())
                    .unwrap();
                e.state = p.state.clone();
            }
            assert_eq!(
                native.run(p.stop, 256).unwrap(),
                oracle.run(p.stop, 256).unwrap()
            );
            same(&native, &oracle);
        }
    }
}

// spec: A64-CTL-6, A64-CTL-9, A64-VER-1
#[test]
fn discarded_switch_targets_restore_shared_stubs_and_relink() {
    let p = program(false, false);
    for config in configs() {
        let mut native = configured(&p, config, false);
        let mut oracle = configured(&p, config, true);
        for _ in 0..2 {
            for e in [&mut native, &mut oracle] {
                e.state = p.state.clone();
            }
            assert_eq!(
                native.run(p.stop, 256).unwrap(),
                oracle.run(p.stop, 256).unwrap()
            );
        }
        let n = native.native.as_mut().unwrap();
        let before = n.switch_counts().1;
        n.discard_switch_target(HANDLERS);
        assert!(n.switch_counts().1 < before);
        for e in [&mut native, &mut oracle] {
            e.state = p.state.clone();
        }
        assert_eq!(
            native.run(p.stop, 256).unwrap(),
            oracle.run(p.stop, 256).unwrap()
        );
        same(&native, &oracle);
        assert_eq!(native.native.as_ref().unwrap().switch_counts().1, before);
    }
}

// spec: A64-CTL-6, A64-CTL-11, A64-VER-1
#[test]
fn switch_thunks_api_entries_external_targets_and_stops_remain_unresolved() {
    for kind in 0..5 {
        let mut p = program(false, false);
        let target = CODE + 0x3000;
        p.code[0x4000..0x4004].copy_from_slice(&target.to_le_bytes());
        let mut a = CodeAssembler::new(32).unwrap();
        match kind {
            0 | 4 => {
                a.jmp(dword_ptr((DATA + 0x100) as i32)).unwrap();
            }
            1 => {
                a.push(0x1234).unwrap();
                a.int(0x2e).unwrap();
            }
            _ => {
                a.mov(edx, 99).unwrap();
                a.jmp(p.stop as u64).unwrap();
            }
        }
        let mut bytes = a.assemble(u64::from(target)).unwrap();
        if kind == 4 {
            // Authored CS overrides make the indirect jump eight bytes long.
            bytes.splice(0..0, [0x2e, 0x2e]);
        }
        p.code[0x3000..0x3000 + bytes.len()].copy_from_slice(&bytes);
        p.data[0x100..0x104].copy_from_slice(&p.stop.to_le_bytes());
        for config in configs() {
            let mut native = configured(&p, config, false);
            let mut oracle = configured(&p, config, true);
            if kind == 2 {
                *native.images.borrow_mut() = vec![(CODE, 0x3000), (TABLE, 4096)];
            }
            let stop = if kind == 3 { target } else { p.stop };
            for _ in 0..3 {
                for e in [&mut native, &mut oracle] {
                    e.state = p.state.clone();
                }
                assert_eq!(
                    native.run(stop, 256).unwrap(),
                    oracle.run(stop, 256).unwrap()
                );
                same(&native, &oracle);
            }
            assert_eq!(native.native.as_ref().unwrap().switch_counts().1, 0);
        }
    }
}

// spec: A64-CTL-6, A64-VER-1
#[test]
fn modified_index_and_non_image_or_writable_tables_use_the_original_probe() {
    for kind in 0..3 {
        let mut p = program(false, kind == 0);
        // A rewritten index is deliberately made terminating here.
        p.code[0x4000 + 7 * 4..0x4004 + 7 * 4].copy_from_slice(&p.stop.to_le_bytes());
        let config = (Features { flagm: false }, false, Checks::Adaptive);
        let mut native = configured(&p, config, false);
        let mut oracle = configured(&p, config, true);
        if kind == 1 {
            native.images.borrow_mut().clear();
        }
        if kind == 2 {
            for e in [&mut native, &mut oracle] {
                e.memory_mut().protect(TABLE, 4096, 3).unwrap();
            }
        }
        for _ in 0..3 {
            for e in [&mut native, &mut oracle] {
                e.state = p.state.clone();
            }
            assert_eq!(
                native.run(p.stop, 256).unwrap(),
                oracle.run(p.stop, 256).unwrap()
            );
            same(&native, &oracle);
        }
        assert_eq!(native.native.as_ref().unwrap().switch_counts().0, 0);
    }
}
