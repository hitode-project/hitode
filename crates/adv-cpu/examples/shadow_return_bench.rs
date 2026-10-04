//! Same-feature alternating on/off return-prediction probes. No device, window
//! or game. Full guest CPU and checked stack work are included in every sample.
#[path = "region_link_bench.rs"]
#[allow(dead_code)]
mod base;

use adv_cpu::{EXEC, READ, Region, RunOutcome, WRITE};
use base::oracle_support::Executor;
use iced_x86::code_asm::*;
use std::time::Instant;

fn local(shadow: bool) -> Executor {
    let mut m = Executor::new(base::CODE, 0x30000).unwrap();
    m.set_shadow_returns(shadow).unwrap();
    m.protect(base::CODE, 0x30000, READ | WRITE).unwrap();
    let mut bytes = base::code(base::CODE, |a| {
        let mut again = a.create_label();
        a.set_label(&mut again).unwrap();
        a.call(u64::from(base::CODE + 512)).unwrap();
        a.dec(ecx).unwrap();
        a.jnz(again).unwrap();
        a.ret().unwrap();
    });
    bytes.resize(512, 0xcc);
    bytes.extend(base::code(base::CODE + 512, |a| {
        a.add(eax, 1).unwrap();
        a.xor(ebx, eax).unwrap();
        a.paddb(xmm0, xmm1).unwrap();
        a.ret().unwrap();
    }));
    m.write(base::CODE, &bytes).unwrap();
    m.protect(base::CODE, 4096, READ | EXEC).unwrap();
    m.protect(base::STOP, 4096, READ | EXEC).unwrap();
    let mut region = Region::new(base::CODE, bytes.len());
    region.reachable = true;
    m.add_region(region).unwrap();
    m
}

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    values[values.len() / 2]
}

fn make(case: &str, enabled: bool) -> Executor {
    if case == "local_hint" {
        local(enabled)
    } else {
        base::setup(if case == "cross_1" { 1 } else { 64 }, enabled)
    }
}

fn main() {
    base::pin();
    let samples: usize = std::env::args().nth(1).map_or(21, |a| a.parse().unwrap());
    let iterations: u32 = std::env::args()
        .nth(2)
        .map_or(1_000_000, |a| a.parse().unwrap());
    assert!(samples > 0 && iterations > 0);
    println!(
        "case,shadow,iterations,ns_per_loop,ns_per_entry,native_entries,code_bytes,compile_p50_ms,compile_p95_ms,committed,reserved,matches,hits,misses,overflows"
    );
    for case in ["local_hint", "cross_1", "cross_64"] {
        let mut machines: Vec<_> = [false, true]
            .into_iter()
            .map(|enabled| make(case, enabled))
            .collect();
        base::seed(&mut machines[0], iterations);
        assert_eq!(
            machines[0].run_unicorn_unbounded(base::STOP).unwrap(),
            RunOutcome::ReachedStop
        );
        let expected = machines[0].state.clone();
        let mut times: [Vec<f64>; 2] = Default::default();
        let mut entries = [0; 2];
        let mut counts = [(0, 0, 0, 0); 2];
        for round in 0..=samples {
            for offset in 0..2 {
                let variant = (round + offset) % 2;
                let m = &mut machines[variant];
                base::seed(m, iterations);
                let before = m.stats.native_entries;
                let prior = (
                    m.stats.shadow_matches,
                    m.stats.shadow_hits,
                    m.stats.shadow_misses,
                    m.stats.shadow_overflows,
                );
                let started = Instant::now();
                assert_eq!(
                    m.run(base::STOP, u64::from(iterations) * 8 + 100).unwrap(),
                    RunOutcome::ReachedStop
                );
                let elapsed = started.elapsed().as_secs_f64();
                entries[variant] = m.stats.native_entries - before;
                counts[variant] = (
                    m.stats.shadow_matches - prior.0,
                    m.stats.shadow_hits - prior.1,
                    m.stats.shadow_misses - prior.2,
                    m.stats.shadow_overflows - prior.3,
                );
                assert_eq!(m.state.regs, expected.regs);
                assert_eq!(m.state.eflags, expected.eflags);
                assert_eq!(m.state.xmm, expected.xmm);
                assert_eq!(m.stats.interpreter_entries, 0);
                if case == "local_hint" {
                    assert_eq!(entries[variant], 1);
                    assert_eq!(
                        counts[variant].1, 0,
                        "local hint was turned into a cross-region Tail call"
                    );
                } else if variant == 1 {
                    assert!(counts[variant].1 > u64::from(iterations) * 99 / 100);
                }
                if round > 0 {
                    times[variant].push(elapsed);
                }
            }
        }
        // Warm both codegen modes above, then alternate cold construction too.
        // Exclude Unicorn/memory fixture setup from the reported CPU compile_time.
        let mut cold: [Vec<f64>; 2] = Default::default();
        for round in 0..9 {
            for offset in 0..2 {
                let variant = (round + offset) % 2;
                let m = make(case, variant == 1);
                cold[variant].push(m.stats.compile_time.as_secs_f64() * 1e3);
            }
        }
        for variant in 0..2 {
            let elapsed = median(&mut times[variant]);
            let compilation = median(&mut cold[variant]);
            let compile_p95 = cold[variant][8];
            let m = &machines[variant];
            #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
            let (committed, reserved) = m.native_code_storage_bytes().unwrap();
            #[cfg(not(all(windows, target_arch = "x86_64", target_env = "msvc")))]
            let (committed, reserved) = (0, 0);
            println!(
                "{case},{variant},{iterations},{:.3},{:.3},{},{},{compilation:.3},{compile_p95:.3},{committed},{reserved},{},{},{},{}",
                elapsed * 1e9 / f64::from(iterations),
                elapsed * 1e9 / entries[variant] as f64,
                entries[variant],
                m.stats.native_code_bytes,
                counts[variant].0,
                counts[variant].1,
                counts[variant].2,
                counts[variant].3
            );
        }
    }
}
