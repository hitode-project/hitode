//! Synthetic polling benchmark: `cargo run -p adv-cpu --release --example safepoint_bench`.
//! Inputs: ADV_SAFEPOINT (unset or poll), ADV_SAFEPOINT_BENCH_TIER (0/1/2),
//! ADV_SAFEPOINT_BENCH_LOOPS (default 200000), ADV_SAFEPOINT_BENCH_SAMPLES
//! (default 7). Tier 1 requires a supported ARM64 host; the default tier is 2.
//! Output: anonymous CSV ns/loop and notification-to-return median/max us.
//! Compilation, warmup, producer sleep, and result validation are not timed.
use adv_cpu::{CpuState, EXEC, Executor, READ, Region, RunOutcome, WRITE};
use iced_x86::code_asm::*;
use std::hint::black_box;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const CODE: u32 = 0x10000;
const STOP: u32 = 0x18000;
const DATA: u32 = 0x20000;
const STACK: u32 = 0x2f800;
const SEED: u32 = 0x1357_9bdf;

fn option(name: &str, default: u32, maximum: u32) -> u32 {
    let value = std::env::var(name).map_or(default, |s| s.parse().expect("integer option"));
    assert!(
        value > 0 && value <= maximum,
        "option outside bounded range"
    );
    value
}

fn program() -> Vec<u8> {
    let mut a = CodeAssembler::new(32).unwrap();
    let mut top = a.create_label();
    a.set_label(&mut top).unwrap();
    // A load/store recurrence and independent memory counter prevent a closed
    // form replacement. Both results are checked against the host recurrence.
    a.mov(eax, dword_ptr(DATA)).unwrap();
    a.imul_3(eax, eax, 1_664_525).unwrap();
    a.add(eax, 1_013_904_223).unwrap();
    a.xor(eax, edx).unwrap();
    a.rol(edx, 3).unwrap();
    a.mov(dword_ptr(DATA), eax).unwrap();
    a.inc(dword_ptr(DATA + 4)).unwrap();
    a.dec(ecx).unwrap();
    a.jnz(top).unwrap();
    a.ret().unwrap();
    a.assemble(u64::from(CODE)).unwrap()
}

fn machine(tier: u8) -> Executor {
    let mut m = Executor::new(CODE, 0x20000).unwrap();
    m.disable_row_batching();
    m.enable_auto_jit(false);
    m.set_async_compilation(false);
    m.set_tiered_compilation(false);
    m.set_tier_verification(None);
    m.protect(CODE, 0x20000, READ | WRITE).unwrap();
    let bytes = program();
    m.write(CODE, &bytes).unwrap();
    m.protect(CODE, 4096, READ | EXEC).unwrap();
    m.protect(STOP, 4096, READ | EXEC).unwrap();
    match tier {
        0 => {}
        1 => m.set_arm64_tier1(true).expect("Tier 1 host support"),
        2 => m.add_region(Region::new(CODE, bytes.len())).unwrap(),
        _ => panic!("tier must be 0, 1, or 2"),
    }
    m
}

fn reset(m: &mut Executor, loops: u32) {
    m.write(DATA, &1u32.to_le_bytes()).unwrap();
    m.write(DATA + 4, &0u32.to_le_bytes()).unwrap();
    m.write(STACK, &STOP.to_le_bytes()).unwrap();
    m.state = CpuState {
        eip: CODE,
        eflags: 0x202,
        regs: [1, loops, SEED, 0, STACK, 0, 0, 0],
        ..CpuState::default()
    };
}

fn reference(loops: u32) -> (u32, u32) {
    let (mut word, mut salt) = (1u32, SEED);
    for _ in 0..loops {
        word = word.wrapping_mul(1_664_525).wrapping_add(1_013_904_223) ^ salt;
        salt = salt.rotate_left(3);
    }
    (word, salt)
}

fn word(m: &Executor, at: u32) -> u32 {
    u32::from_le_bytes(m.memory().read(at, 4).unwrap().try_into().unwrap())
}

fn verify(m: &Executor, completed: u32, expected: (u32, u32)) {
    assert!(word(m, DATA) == expected.0, "anonymous recurrence mismatch");
    assert!(
        word(m, DATA + 4) == completed,
        "anonymous loop counter mismatch"
    );
    assert!(
        m.state.regs[0] == expected.0,
        "anonymous accumulator mismatch"
    );
    assert!(m.state.regs[2] == expected.1, "anonymous salt mismatch");
    black_box((&m.state, expected));
}

fn throughput(m: &mut Executor, loops: u32, samples: usize) -> (f64, f64) {
    let expected = reference(loops);
    let mut ns = Vec::with_capacity(samples);
    for _ in 0..samples {
        reset(m, loops);
        let start = Instant::now();
        let outcome = m.run(STOP, u64::from(loops) * 16 + 32).unwrap();
        let elapsed = start.elapsed();
        assert!(
            outcome == RunOutcome::ReachedStop,
            "throughput run did not finish"
        );
        assert!(m.state.regs[1] == 0, "throughput loop did not retire");
        verify(m, loops, expected);
        ns.push(elapsed.as_secs_f64() * 1e9 / f64::from(loops));
    }
    ns.sort_by(f64::total_cmp);
    (ns[samples / 2], ns[samples - 1])
}

fn latency(m: &mut Executor, tier: u8, samples: usize) -> (f64, f64) {
    let flag = Arc::new(AtomicBool::new(false));
    m.set_host_interrupt_flag(flag.clone());
    let mut replay = machine(tier);
    let mut us = Vec::with_capacity(samples);
    for _ in 0..samples {
        flag.store(false, Ordering::Release);
        reset(m, u32::MAX);
        let notify = flag.clone();
        let producer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(2));
            // Timestamp immediately before the release store, excluding sleep
            // and thread startup from notification-to-return measurement.
            let sent = Instant::now();
            notify.store(true, Ordering::Release);
            sent
        });
        let outcome = m.run(STOP, u64::MAX).unwrap();
        let returned = Instant::now();
        let sent = producer.join().unwrap();
        assert!(
            outcome == RunOutcome::Interrupted,
            "latency run did not interrupt"
        );
        let elapsed = returned.duration_since(sent);
        assert!(
            elapsed < Duration::from_secs(2),
            "notification exceeded functional bound"
        );
        // Tier 1 may pause inside the logical loop. Replay the exact charged
        // prefix in the same tier instead of equating block/instruction fuel.
        let consumed = u64::MAX - m.state.fuel;
        reset(&mut replay, u32::MAX);
        assert!(
            replay.run(STOP, consumed).unwrap() == RunOutcome::Budget,
            "anonymous prefix replay failed"
        );
        let mut expected = m.state.clone();
        expected.fuel = 0;
        assert!(replay.state == expected, "anonymous prefix CPU mismatch");
        assert!(
            replay.memory().read(DATA, 8).unwrap() == m.memory().read(DATA, 8).unwrap(),
            "anonymous prefix data mismatch"
        );
        us.push(elapsed.as_secs_f64() * 1e6);
    }
    flag.store(false, Ordering::Release);
    us.sort_by(f64::total_cmp);
    (us[samples / 2], us[samples - 1])
}

fn main() {
    let tier = std::env::var("ADV_SAFEPOINT_BENCH_TIER")
        .map_or(2, |s| s.parse::<u8>().expect("integer tier"));
    let loops = option("ADV_SAFEPOINT_BENCH_LOOPS", 200_000, 10_000_000);
    let samples = option("ADV_SAFEPOINT_BENCH_SAMPLES", 7, 31) as usize;
    let mode = if std::env::var("ADV_SAFEPOINT").is_ok_and(|s| s == "poll") {
        "poll"
    } else {
        "off"
    };
    let mut m = machine(tier);
    m.set_host_interrupt_flag(Arc::new(AtomicBool::new(false)));
    reset(&mut m, 32);
    assert!(
        m.run(STOP, 1000).unwrap() == RunOutcome::ReachedStop,
        "warmup failed"
    );
    verify(&m, 32, reference(32));
    let (median_ns, max_ns) = throughput(&mut m, loops, samples);
    let (median_us, max_us) = latency(&mut m, tier, samples);
    println!(
        "mode,tier,loops,samples,median_ns_per_loop,max_ns_per_loop,latency_median_us,latency_max_us"
    );
    println!(
        "{mode},{tier},{loops},{samples},{median_ns:.3},{max_ns:.3},{median_us:.3},{max_us:.3}"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_loop_retires_the_checked_memory_and_register_recurrence() {
        for tier in [0, 2] {
            let mut m = machine(tier);
            for loops in [1, 17, 1003] {
                reset(&mut m, loops);
                assert!(
                    m.run(STOP, u64::from(loops) * 16 + 32).unwrap() == RunOutcome::ReachedStop,
                    "bounded workload did not finish"
                );
                verify(&m, loops, reference(loops));
                assert!(
                    m.state.regs[1] == 0 && m.state.regs[4] == STACK + 4,
                    "workload exit state changed"
                );
            }
        }
    }
}
