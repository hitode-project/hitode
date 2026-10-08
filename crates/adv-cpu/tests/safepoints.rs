//! Synthetic off/poll differential checkpoints and restartable interrupt prefixes.
//! Run with `cargo test -p adv-cpu --test safepoints`. Each differential child
//! owns a fresh environment because the polling switch is process-wide.
use adv_cpu::{CompileStrategy, CpuState, EXEC, Executor, READ, Region, RunOutcome, WRITE};
use iced_x86::{Decoder, DecoderOptions, OpKind, code_asm::*};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const CODE: u32 = 0x10000;
const LEAF: u32 = 0x12000;
const STOP: u32 = 0x18000;
const DATA: u32 = 0x20000;
const SLOT: u32 = DATA + 4096;
const STACK: u32 = 0x2f800;
const CHILD: &str = "ADV_SAFEPOINT_TEST_CHILD";

#[derive(Clone, Copy)]
struct Backend {
    tier: u8,
    strategy: CompileStrategy,
    linked: bool,
}

fn backends() -> Vec<Backend> {
    let mut result = vec![Backend {
        tier: 0,
        strategy: CompileStrategy::Speed,
        linked: false,
    }];
    if cfg!(all(
        target_arch = "aarch64",
        any(
            target_os = "linux",
            target_os = "android",
            target_os = "macos"
        )
    )) {
        result.push(Backend {
            tier: 1,
            strategy: CompileStrategy::Speed,
            linked: false,
        });
    }
    for strategy in [CompileStrategy::Fast, CompileStrategy::Speed] {
        for linked in [false, true] {
            result.push(Backend {
                tier: 2,
                strategy,
                linked,
            });
        }
    }
    result
}

fn assemble(at: u32, build: impl FnOnce(&mut CodeAssembler)) -> Vec<u8> {
    let mut a = CodeAssembler::new(32).unwrap();
    build(&mut a);
    a.assemble(u64::from(at)).unwrap()
}

fn machine(programs: &[(u32, Vec<u8>)], backend: Backend) -> Executor {
    let mut m = Executor::new(CODE, 0x20000).unwrap();
    m.disable_row_batching();
    m.enable_auto_jit(false);
    m.set_async_compilation(false);
    m.set_tiered_compilation(false);
    m.set_tier_verification(None);
    m.set_region_linking(backend.linked);
    m.protect(CODE, 0x20000, READ | WRITE).unwrap();
    if backend.tier == 1 {
        m.set_arm64_tier1(true).unwrap();
    }
    for (at, bytes) in programs {
        m.write(*at, bytes).unwrap();
        m.protect(*at, 4096, READ | EXEC).unwrap();
        if backend.tier == 2 {
            let mut region = Region::new(*at, bytes.len());
            region.compile_strategy = backend.strategy;
            // Authored memory side entries let fault repair resume natively.
            region.entries.extend(
                Decoder::with_ip(32, bytes, u64::from(*at), DecoderOptions::NONE)
                    .into_iter()
                    .filter(|i| i.op_kinds().any(|kind| kind == OpKind::Memory))
                    .map(|i| i.ip32()),
            );
            m.add_region(region).unwrap();
        }
    }
    m.protect(STOP, 4096, READ | EXEC).unwrap();
    m.set_host_interrupt_flag(Arc::new(AtomicBool::new(false)));
    reset(&mut m, 19);
    m
}

fn reset(m: &mut Executor, iterations: u32) {
    m.write(DATA, &[0; 64]).unwrap();
    m.write(SLOT, &[0; 64]).unwrap();
    m.write(STACK - 1024, &[0; 1024]).unwrap();
    m.write(STACK, &STOP.to_le_bytes()).unwrap();
    m.state = CpuState {
        eip: CODE,
        eflags: 0x202,
        regs: [3, iterations, CODE, 11, STACK, iterations, DATA, 17],
        ..CpuState::default()
    };
    m.state.xmm[0] = [3; 16];
    m.state.xmm[1] = [1; 16];
}

// The two-entry A/B SCC is irreducible; neither member dominates the other.
fn program(case: &str) -> Vec<(u32, Vec<u8>)> {
    let first = assemble(CODE, |a| match case {
        "straight" => {
            for _ in 0..40 {
                let mut next = a.create_label();
                a.inc(eax).unwrap();
                a.jmp(next).unwrap();
                a.set_label(&mut next).unwrap();
            }
            a.ret().unwrap();
        }
        "large_block" => {
            for _ in 0..300 {
                a.inc(eax).unwrap();
            }
            a.ret().unwrap();
        }
        "direct" | "indirect" | "linked" => {
            let mut top = a.create_label();
            let mut done = a.create_label();
            a.set_label(&mut top).unwrap();
            a.inc(eax).unwrap();
            a.inc(dword_ptr(DATA)).unwrap();
            a.dec(ecx).unwrap();
            if case == "direct" {
                a.jnz(top).unwrap();
            } else {
                a.jz(done).unwrap();
                if case == "indirect" {
                    a.jmp(edx).unwrap();
                } else {
                    a.jmp(u64::from(LEAF)).unwrap();
                }
                a.set_label(&mut done).unwrap();
            }
            a.ret().unwrap();
        }
        "irreducible" => {
            let mut left = a.create_label();
            let mut right = a.create_label();
            let mut done = a.create_label();
            a.test(edx, edx).unwrap();
            a.jnz(right).unwrap();
            a.set_label(&mut left).unwrap();
            a.inc(eax).unwrap();
            a.inc(dword_ptr(DATA)).unwrap();
            a.dec(ecx).unwrap();
            a.jz(done).unwrap();
            a.jmp(right).unwrap();
            a.set_label(&mut right).unwrap();
            a.xor(ebx, eax).unwrap();
            a.inc(dword_ptr(DATA)).unwrap();
            a.dec(ecx).unwrap();
            a.jnz(left).unwrap();
            a.set_label(&mut done).unwrap();
            a.ret().unwrap();
        }
        "recursive" => {
            let mut top = a.create_label();
            a.set_label(&mut top).unwrap();
            a.mov(ecx, 8u32).unwrap();
            a.call(u64::from(LEAF)).unwrap();
            a.dec(ebp).unwrap();
            a.jnz(top).unwrap();
            a.ret().unwrap();
        }
        _ => unreachable!(),
    });
    let mut programs = vec![(CODE, first)];
    if case == "linked" {
        programs.push((LEAF, assemble(LEAF, |a| a.jmp(u64::from(CODE)).unwrap())));
    } else if case == "recursive" {
        programs.push((
            LEAF,
            assemble(LEAF, |a| {
                let mut base = a.create_label();
                a.dec(ecx).unwrap();
                a.jz(base).unwrap();
                a.inc(dword_ptr(DATA)).unwrap();
                a.call(u64::from(LEAF)).unwrap();
                a.set_label(&mut base).unwrap();
                a.inc(eax).unwrap();
                a.ret().unwrap();
            }),
        ));
    }
    programs
}

fn digest(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100_0000_01b3)
    })
}

fn checkpoint(m: &Executor, outcome: &RunOutcome) {
    // Include exact PC, fault provenance, fuel, indirect history, and all CPU
    // state, but emit only opaque digests. Compilation/dispatch stats may differ.
    let state = digest(format!("{:?}:{outcome:?}", m.state).as_bytes());
    let data = digest(m.memory().read(DATA, 64).unwrap());
    let slot = digest(m.memory().read(SLOT, 64).unwrap());
    let stack = digest(m.memory().read(STACK - 1024, 1028).unwrap());
    println!(
        "SPT_DIGEST {state:016x} {data:016x} {slot:016x} {stack:016x} {} {} {}",
        m.stats.native_blocks,
        m.stats.interpreter_instructions,
        m.tier1_stats().native_instructions
    );
}

fn run_checkpoint(m: &mut Executor, budget: u64) -> RunOutcome {
    let outcome = m.run(STOP, budget).unwrap();
    assert!(m.state.fuel <= budget, "fuel exceeded caller allowance");
    checkpoint(m, &outcome);
    outcome
}

fn fuel_cases(backend: Backend) {
    for case in [
        "straight",
        "large_block",
        "direct",
        "irreducible",
        "indirect",
        "recursive",
        "linked",
    ] {
        let mut m = machine(&program(case), backend);
        for budget in [0, 1, 2, 7, 15, 16, 17, 31, 32, 33, 255] {
            println!(
                "SPT_DIGEST case={case} tier={} budget={budget}",
                backend.tier
            );
            reset(&mut m, 19);
            if case == "irreducible" {
                m.state.regs[2] = budget as u32 & 1;
            }
            let mut outcome = run_checkpoint(&mut m, budget);
            for round in 0..256 {
                if outcome == RunOutcome::ReachedStop {
                    break;
                }
                assert!(
                    outcome == RunOutcome::Budget,
                    "unexpected sliced exit in {case}"
                );
                assert!(round != 255, "bounded case did not finish: {case}");
                outcome = run_checkpoint(&mut m, [1, 2, 7, 16, 33][round % 5]);
            }
            assert!(m.state.eip == STOP, "stop PC mismatch in {case}");
            assert!(m.state.regs[4] == STACK + 4, "unbalanced stack in {case}");
            let count = u32::from_le_bytes(m.memory().read(DATA, 4).unwrap().try_into().unwrap());
            assert!(
                count
                    == match case {
                        "straight" | "large_block" => 0,
                        "recursive" => 19 * 7,
                        _ => 19,
                    },
                "loop side effects mismatch in {case}"
            );
        }
    }
}

fn stop_cases(backend: Backend) {
    let programs = program("straight");
    let targets = Decoder::with_ip(32, &programs[0].1, u64::from(CODE), DecoderOptions::NONE)
        .into_iter()
        .step_by(2)
        .map(|i| i.ip32())
        .collect::<Vec<_>>();
    for ordinal in [0, 1, 15, 16, 17, 39, 40] {
        let target = targets[ordinal];
        // Tier 2's public API requires stop outside every published region.
        // Compile the exact preceding prefix, leaving the stop as its frontier.
        let mut m = machine(
            &programs,
            if backend.tier == 2 {
                Backend { tier: 0, ..backend }
            } else {
                backend
            },
        );
        if backend.tier == 2 && target != CODE {
            let mut region = Region::new(CODE, (target - CODE) as usize);
            region.compile_strategy = backend.strategy;
            m.add_region(region).unwrap();
        }
        for budget in [0, 1, 15, 16, 17, 1000] {
            reset(&mut m, 19);
            println!(
                "SPT_DIGEST stop={ordinal} budget={budget} tier={}",
                backend.tier
            );
            let mut outcome = m.run(target, budget).unwrap();
            checkpoint(&m, &outcome);
            for round in 0..4 {
                if outcome == RunOutcome::ReachedStop {
                    break;
                }
                assert!(
                    outcome == RunOutcome::Budget,
                    "unexpected authored stop exit"
                );
                assert!(round != 3, "authored stop was missed");
                outcome = m.run(target, 1000).unwrap();
                checkpoint(&m, &outcome);
            }
            assert!(m.state.eip == target, "exact authored stop PC changed");
            assert!(
                m.state.regs[0] == 3 + ordinal as u32,
                "authored stop prefix changed"
            );
            assert!(m.state.regs[4] == STACK, "stop retired the return");
        }
    }
}

fn fault_cases(backend: Backend) {
    for kind in ["read", "write", "stack"] {
        let bytes = assemble(CODE, |a| {
            a.inc(eax).unwrap();
            a.mov(ah, 1).unwrap();
            a.paddd(xmm0, xmm1).unwrap();
            a.inc(dword_ptr(SLOT)).unwrap();
            match kind {
                "read" => a.mov(ebx, dword_ptr(DATA)).unwrap(),
                "write" => a.mov(dword_ptr(DATA), eax).unwrap(),
                "stack" => {
                    a.push(eax).unwrap();
                    a.pop(edx).unwrap();
                }
                _ => unreachable!(),
            }
            a.ret().unwrap();
        });
        let expected_pc = Decoder::with_ip(32, &bytes, u64::from(CODE), DecoderOptions::NONE)
            .into_iter()
            .nth(4)
            .unwrap()
            .ip32();
        let mut m = machine(&[(CODE, bytes)], backend);
        let bad_page = if kind == "stack" {
            (STACK - 4) & !4095
        } else {
            DATA
        };
        m.protect(bad_page, 4096, 0).unwrap();
        let outcome = m.run(STOP, 1000).unwrap();
        assert!(
            matches!(outcome, RunOutcome::ReadFault { pc, .. } | RunOutcome::WriteFault { pc, .. }
            if pc == expected_pc),
            "fault PC mismatch in {kind}"
        );
        assert!(m.state.eip == expected_pc, "fault restart PC mismatch");
        assert!(
            m.state.regs[0] == 0x104,
            "fault prefix register writes were lost"
        );
        assert!(
            m.state.xmm[0] == [4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4],
            "fault prefix vector write was lost"
        );
        // Restore permission before reading the data/stack digest; this does not
        // execute the faulting instruction or change the checkpointed CPU state.
        m.protect(bad_page, 4096, READ | WRITE).unwrap();
        println!("SPT_DIGEST fault={kind} tier={}", backend.tier);
        checkpoint(&m, &outcome);
        m.write(DATA, &123u32.to_le_bytes()).unwrap();
        assert!(
            run_checkpoint(&mut m, 1000) == RunOutcome::ReachedStop,
            "repaired fault did not finish"
        );
        assert!(m.state.regs[0] == 0x104, "repair replayed the prefix");
        assert!(
            m.memory().read(SLOT, 4).unwrap() == 1u32.to_le_bytes(),
            "repair replayed a completed store"
        );
        assert!(
            match kind {
                "read" => m.state.regs[3] == 123,
                "write" => m.memory().read(DATA, 4).unwrap() == 0x104u32.to_le_bytes(),
                "stack" => m.state.regs[2] == 0x104,
                _ => unreachable!(),
            },
            "repaired instruction did not commit its result"
        );
        assert!(
            m.state.regs[4] == STACK + 4,
            "fault repair unbalanced the stack"
        );
    }
}

fn smc_case(backend: Backend) {
    let leaf = assemble(LEAF, |a| {
        a.mov(eax, 3u32).unwrap();
        a.ret().unwrap();
    });
    let root = assemble(CODE, |a| {
        a.mov(dword_ptr(LEAF + 1), 91u32).unwrap();
        a.inc(dword_ptr(SLOT)).unwrap();
        a.mov(ebx, dword_ptr(DATA)).unwrap();
        a.call(u64::from(LEAF)).unwrap();
        a.ret().unwrap();
    });
    let fault_pc = Decoder::with_ip(32, &root, u64::from(CODE), DecoderOptions::NONE)
        .into_iter()
        .nth(2)
        .unwrap()
        .ip32();
    let mut m = machine(&[(CODE, root), (LEAF, leaf)], backend);
    m.state.eip = LEAF;
    assert!(
        run_checkpoint(&mut m, 1000) == RunOutcome::ReachedStop,
        "SMC warmup failed"
    );
    reset(&mut m, 19);
    m.protect(LEAF, 4096, READ | WRITE | EXEC).unwrap();
    m.protect(DATA, 4096, 0).unwrap();
    let outcome = m.run(STOP, 1000).unwrap();
    assert!(
        matches!(outcome, RunOutcome::ReadFault { pc, address } if pc == fault_pc && address == DATA),
        "SMC suffix did not fault"
    );
    assert!(m.state.eip == fault_pc, "SMC fault restart PC changed");
    m.protect(DATA, 4096, READ | WRITE).unwrap();
    println!("SPT_DIGEST smc tier={}", backend.tier);
    checkpoint(&m, &outcome);
    // A host update after the fault must win over the already retired guest store.
    m.write(LEAF + 1, &57u32.to_le_bytes()).unwrap();
    assert!(
        run_checkpoint(&mut m, 1000) == RunOutcome::ReachedStop,
        "SMC repair failed"
    );
    assert!(
        m.state.regs[0] == 57,
        "SMC store replayed or stale code executed"
    );
    assert!(
        m.memory().read(SLOT, 4).unwrap() == 1u32.to_le_bytes(),
        "SMC prefix replayed"
    );
}

#[test]
fn differential_child() {
    if std::env::var_os(CHILD).is_none() {
        return;
    }
    for backend in backends() {
        fuel_cases(backend);
        stop_cases(backend);
        fault_cases(backend);
        smc_case(backend);
    }
}

#[test]
fn off_and_poll_preserve_exact_checkpoints() {
    let executable = std::env::current_exe().unwrap();
    let capture = |poll| {
        let mut command = Command::new(&executable);
        command
            .args(["--exact", "differential_child", "--nocapture"])
            .env(CHILD, "1")
            .env_remove("ADV_SAFEPOINT");
        if poll {
            command.env("ADV_SAFEPOINT", "poll");
        }
        let output = command.output().unwrap();
        // Failed children may contain diagnostic PCs; keep their output private.
        assert!(
            output.status.success(),
            "anonymous differential child failed (poll={poll})"
        );
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .filter(|line| line.starts_with("SPT_DIGEST "))
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let off = capture(false);
    let poll = capture(true);
    assert!(!off.is_empty(), "missing differential checkpoints");
    assert!(off.len() == poll.len(), "checkpoint count changed");
    for (index, (off, poll)) in off.iter().zip(&poll).enumerate() {
        assert!(off == poll, "anonymous checkpoint {index} changed");
    }
}

#[test]
fn stop_budget_and_pending_interrupt_keep_owner_priority() {
    for backend in backends() {
        let mut m = machine(&program("direct"), backend);
        let flag = Arc::new(AtomicBool::new(true));
        m.set_host_interrupt_flag(flag.clone());
        m.state.eip = STOP;
        assert!(
            m.run(STOP, 0).unwrap() == RunOutcome::ReachedStop,
            "stop priority changed"
        );
        m.state.eip = CODE;
        assert!(
            m.run(STOP, 0).unwrap() == RunOutcome::Budget,
            "zero fuel priority changed"
        );
        m.state.fuel = 123;
        let before = m.state.clone();
        for _ in 0..2 {
            assert!(
                m.run(STOP, 123).unwrap() == RunOutcome::Interrupted,
                "pending request lost"
            );
            assert!(
                m.state == before,
                "pending request charged work or changed state"
            );
        }
        assert!(
            flag.load(Ordering::Acquire),
            "executor cleared the owner's flag"
        );
        flag.store(false, Ordering::Release);
        assert!(
            m.run(STOP, 10000).unwrap() == RunOutcome::ReachedStop,
            "clear did not resume"
        );
    }
}

#[test]
fn cyclic_shapes_interrupt_at_an_exact_replayable_prefix() {
    for backend in backends().into_iter().filter(|b| b.tier != 0) {
        for case in ["direct", "irreducible", "indirect", "recursive", "linked"] {
            let programs = program(case);
            let mut m = machine(&programs, backend);
            reset(&mut m, 100_000_000);
            let flag = Arc::new(AtomicBool::new(false));
            m.set_host_interrupt_flag(flag.clone());
            let notify = flag.clone();
            let producer = std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(2));
                let sent = Instant::now();
                notify.store(true, Ordering::Release);
                sent
            });
            let budget = 10_000_000_000;
            let outcome = m.run(STOP, budget).unwrap();
            let returned = Instant::now();
            let sent = producer.join().unwrap();
            assert!(
                outcome == RunOutcome::Interrupted,
                "loop did not interrupt: {case}"
            );
            assert!(
                returned.duration_since(sent) < Duration::from_secs(2),
                "interrupt exceeded the broad functional bound: {case}"
            );
            let consumed = budget - m.state.fuel;
            assert!(
                consumed
                    == m.stats.native_blocks
                        + m.stats.interpreter_instructions
                        + m.tier1_stats().native_instructions,
                "retired accounting changed: {case}"
            );
            let mut replay = machine(&programs, backend);
            reset(&mut replay, 100_000_000);
            assert!(
                replay.run(STOP, consumed).unwrap() == RunOutcome::Budget,
                "prefix replay did not reach a budget boundary: {case}"
            );
            let mut expected = m.state.clone();
            expected.fuel = 0;
            assert!(
                replay.state == expected,
                "interrupt prefix CPU mismatch: {case}"
            );
            assert!(
                replay.memory().read(DATA, 64).unwrap() == m.memory().read(DATA, 64).unwrap(),
                "interrupt prefix data mismatch: {case}"
            );
            assert!(
                replay.memory().read(STACK - 1024, 1028).unwrap()
                    == m.memory().read(STACK - 1024, 1028).unwrap(),
                "interrupt prefix stack mismatch: {case}"
            );
            flag.store(false, Ordering::Release);
            let resumed = m.run(STOP, 33).unwrap();
            assert!(
                resumed == RunOutcome::Budget,
                "cleared loop did not resume: {case}"
            );
            assert!(
                replay.run(STOP, 33).unwrap() == RunOutcome::Budget,
                "replay resume failed"
            );
            assert!(
                m.state == replay.state,
                "resume replayed interrupted work: {case}"
            );
        }
    }
}
