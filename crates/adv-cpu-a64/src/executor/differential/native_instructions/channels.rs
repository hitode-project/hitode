//! Authored planar-byte loops, independent of engines and library code.
use super::*;

const INPUTS: [u8; 4] = [3, 1, 2, 7];

fn program(channels: u32, variant: u32) -> Program {
    let registers = [eax, ecx, edx, ebx, esp, ebp, esi, edi];
    let inputs = if variant == 2 { [7, 2, 0, 3] } else { INPUTS };
    let mut p = fixture(|a| {
        let mut top = a.create_label();
        a.set_label(&mut top).unwrap();
        for lane in 0..channels {
            let source = registers[inputs[lane as usize] as usize];
            let position = if variant == 2 {
                channels - 1 - lane
            } else {
                lane
            };
            if variant == 1 {
                a.mov(ah, byte_ptr(source + 3)).unwrap();
                a.mov(byte_ptr(esi + position as i32 + 7), ah).unwrap();
                a.add(source, 1).unwrap();
            } else {
                if variant == 2 {
                    a.movsx(ecx, byte_ptr(source)).unwrap();
                } else {
                    a.movzx(eax, byte_ptr(source)).unwrap();
                }
                a.inc(source).unwrap();
                a.mov(
                    byte_ptr(esi + position as i32),
                    if variant == 2 { cl } else { al },
                )
                .unwrap();
            }
        }
        a.add(esi, channels as i32).unwrap();
        if variant == 3 {
            a.cmp(esi, DATA + 0x2000 + channels * 130).unwrap();
            a.jne(top).unwrap();
        } else {
            a.cmp(esi, dword_ptr(ebp - 4)).unwrap();
            a.jb(top).unwrap();
        }
    });
    for (n, b) in p.data.iter_mut().enumerate() {
        *b = (n * 73 + n / 13) as u8;
    }
    for (n, r) in inputs.into_iter().enumerate() {
        p.state.regs[r as usize] = DATA + n as u32 * 0x800 + 17;
    }
    p.state.regs[6] = DATA + 0x2000;
    p.state.regs[5] = STACK_TOP - 64;
    p
}

fn pair(p: &Program, config: Config, count: u32, channels: u32) -> (Executor, Executor) {
    let mut native = executor(p, config, false);
    let mut oracle = executor(p, config, true);
    let end = p.state.regs[6].wrapping_add(count * channels).to_le_bytes();
    for cpu in [&mut native, &mut oracle] {
        cpu.write(p.state.regs[5] - 4, &end).unwrap();
    }
    (native, oracle)
}

// spec: A64-TR-6, A64-CTL-7, A64-VER-1
#[test]
fn channel_batches_preserve_pixels_registers_flags_and_budget_cuts() {
    for channels in 2..=4 {
        for variant in 0..4 {
            let p = program(channels, variant);
            let size = channels as u64 * 3 + 3;
            for config in super::pixel_loops::configs() {
                for budget in [
                    1,
                    size * 64,
                    size * 65 - 1,
                    size * 65,
                    size * 65 + 1,
                    size * 130,
                    10000,
                ] {
                    let (mut native, mut oracle) = pair(&p, config, 130, channels);
                    assert_eq!(
                        native.run(p.stop, budget).unwrap(),
                        oracle.run(p.stop, budget).unwrap()
                    );
                    same(&native, &oracle);
                    if config.2 == Checks::Adaptive && !config.1 && budget >= size * 65 {
                        assert!(native.native.as_ref().unwrap().channel_batches() > 0);
                    }
                    assert_eq!(
                        native.run(p.stop, 10000).unwrap(),
                        oracle.run(p.stop, 10000).unwrap()
                    );
                    same(&native, &oracle);
                }
            }
        }
    }
}

// spec: A64-TR-6, A64-MEM-5, A64-MEM-10, A64-VER-1
#[test]
fn channel_batches_leave_the_exact_prefix_at_later_read_and_write_faults() {
    for store in [false, true] {
        let mut p = program(4, 0);
        p.state.regs[if store { 6 } else { 3 }] =
            DATA + DATA_LEN as u32 - 65 * if store { 4 } else { 1 };
        for flagm in [false, true] {
            let (mut native, mut oracle) =
                pair(&p, (Features { flagm }, false, Checks::Adaptive), 130, 4);
            let outcome = native.run(p.stop, 10000).unwrap();
            assert!(matches!(
                outcome,
                RunOutcome::ReadFault { .. } | RunOutcome::WriteFault { .. }
            ));
            assert_eq!(outcome, oracle.run(p.stop, 10000).unwrap());
            same(&native, &oracle);
            assert!(native.native.as_ref().unwrap().channel_batches() > 0);
            for cpu in [&mut native, &mut oracle] {
                cpu.protect(DATA + DATA_LEN as u32, 0x4000, 3).unwrap();
                cpu.write(DATA + DATA_LEN as u32, &[0x53; 0x4000]).unwrap();
            }
            assert_eq!(
                native.run(p.stop, 10000).unwrap(),
                oracle.run(p.stop, 10000).unwrap()
            );
            same(&native, &oracle);
            assert_eq!(
                native
                    .memory()
                    .read(DATA + DATA_LEN as u32, 0x4000)
                    .unwrap(),
                oracle
                    .memory()
                    .read(DATA + DATA_LEN as u32, 0x4000)
                    .unwrap()
            );
        }
    }
}

// spec: A64-TR-6, A64-MEM-4, A64-MEM-8, A64-VER-1
#[test]
fn channel_aliases_bound_writes_page_crossings_and_code_barriers_use_scalar_semantics() {
    for case in 0..8 {
        let mut p = program(4, 0);
        match case {
            0 => p.state.regs[6] = p.state.regs[3],
            1 => p.state.regs[6] = p.state.regs[3] + 1,
            2 => p.state.regs[6] = p.state.regs[3] - 1,
            3 => p.state.regs[6] = p.state.regs[5] - 4,
            4 => p.state.regs[3] = DATA + 0x1000 - 33,
            _ => {}
        }
        let (mut native, mut oracle) = pair(
            &p,
            (Features { flagm: true }, false, Checks::Adaptive),
            130,
            4,
        );
        for cpu in [&mut native, &mut oracle] {
            match case {
                5 => cpu.protect(DATA + 0x2000, 0x1000, 7).unwrap(),
                6 => cpu.memory_mut().watch_code(DATA + 0x2000, 4),
                7 => cpu.protect(DATA + 0x3000, 0x1000, 1).unwrap(),
                _ => {}
            }
        }
        assert_eq!(
            native.run(p.stop, 975).unwrap(),
            oracle.run(p.stop, 975).unwrap()
        );
        same(&native, &oracle);
        if case <= 6 && case != 5 {
            assert_eq!(
                native.native.as_ref().unwrap().channel_batches(),
                0,
                "case {case}"
            );
        }
    }
}

// spec: A64-TR-6, A64-CTL-1, A64-VER-1
#[test]
fn channel_code_changes_and_stops_disable_invalid_batch_plans() {
    let p = program(4, 0);
    let (mut native, mut oracle) = pair(
        &p,
        (Features { flagm: false }, false, Checks::Adaptive),
        130,
        4,
    );
    assert_eq!(
        native.run(p.stop, 10000).unwrap(),
        oracle.run(p.stop, 10000).unwrap()
    );
    same(&native, &oracle);
    let stop = iced_x86::Decoder::with_ip(32, &p.code, u64::from(CODE), 0)
        .iter()
        .nth(2)
        .unwrap()
        .ip32();
    for cpu in [&mut native, &mut oracle] {
        cpu.state = p.state.clone();
    }
    let before = native.native.as_ref().unwrap().channel_batches();
    assert_eq!(
        native.run(stop, 10000).unwrap(),
        oracle.run(stop, 10000).unwrap()
    );
    same(&native, &oracle);
    assert_eq!(native.native.as_ref().unwrap().channel_batches(), before);
    let changed = program(4, 1);
    for cpu in [&mut native, &mut oracle] {
        cpu.protect(CODE, 0x10000, 7).unwrap();
        cpu.write(CODE, &changed.code).unwrap();
        cpu.protect(CODE, 0x10000, 5).unwrap();
        cpu.state = changed.state.clone();
    }
    assert_eq!(
        native.run(changed.stop, 10000).unwrap(),
        oracle.run(changed.stop, 10000).unwrap()
    );
    same(&native, &oracle);
    assert!(native.stats.invalidations > 0);
}

// spec: A64-TR-6, A64-MEM-5, A64-VER-1
#[test]
fn a_faulting_loop_bound_is_read_only_after_the_original_pixel_stores() {
    let mut p = program(4, 0);
    p.state.regs[5] = DATA + DATA_LEN as u32 + 2;
    let config = (Features { flagm: true }, false, Checks::Adaptive);
    let mut native = executor(&p, config, false);
    let mut oracle = executor(&p, config, true);
    let outcome = native.run(p.stop, 10000).unwrap();
    assert!(matches!(outcome, RunOutcome::ReadFault { .. }));
    assert_eq!(outcome, oracle.run(p.stop, 10000).unwrap());
    same(&native, &oracle);
    assert_eq!(native.native.as_ref().unwrap().channel_batches(), 0);
}
