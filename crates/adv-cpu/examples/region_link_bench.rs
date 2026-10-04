//! Full-state region boundary benchmark, including guest work and guarded stack
//! accesses. This is not the minimal Tail ABI microbenchmark in architecture.md.
#[path = "../tests/support/mod.rs"]
pub(crate) mod oracle_support;
use adv_cpu::{EXEC, READ, Region, RunOutcome, WRITE};
use iced_x86::code_asm::*;
use oracle_support::Executor;
use std::time::Instant;

pub(crate) const CODE: u32 = 0x10000;
const TABLE: u32 = 0x28000;
const STACK: u32 = 0x2f800;
pub(crate) const STOP: u32 = 0x30000;

#[cfg(windows)]
pub(crate) fn pin() {
    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut std::ffi::c_void;
        fn SetProcessAffinityMask(process: *mut std::ffi::c_void, mask: usize) -> i32;
    }
    // SAFETY: the pseudo-handle names this process; CPU 0 is an affinity bit.
    assert_ne!(unsafe { SetProcessAffinityMask(GetCurrentProcess(), 1) }, 0);
}
#[cfg(not(windows))]
pub(crate) fn pin() {}

pub(crate) fn code(at: u32, f: impl FnOnce(&mut CodeAssembler)) -> Vec<u8> {
    let mut a = CodeAssembler::new(32).unwrap();
    f(&mut a);
    a.assemble(at as u64).unwrap()
}

pub(crate) fn setup(targets: u32, shadow: bool) -> Executor {
    let mut m = Executor::new(CODE, 0x30000).unwrap();
    m.set_shadow_returns(shadow).unwrap();
    m.protect(CODE, 0x30000, READ | WRITE).unwrap();
    let main = code(CODE, |a| {
        let mut again = a.create_label();
        a.set_label(&mut again).unwrap();
        if targets == 1 {
            a.call(0x12000u64).unwrap();
        } else {
            a.mov(edx, ecx).unwrap();
            a.and(edx, (targets - 1) as i32).unwrap();
            a.call(dword_ptr(edx * 4 + TABLE)).unwrap();
        }
        a.dec(ecx).unwrap();
        a.jnz(again).unwrap();
        a.ret().unwrap();
    });
    let mut regions = vec![(CODE, main)];
    for index in 0..targets {
        let address = 0x12000 + index * 512;
        m.write(TABLE + index * 4, &address.to_le_bytes()).unwrap();
        regions.push((
            address,
            code(address, |a| {
                a.add(eax, index as i32 + 1).unwrap();
                a.xor(ebx, eax).unwrap();
                a.paddb(xmm0, xmm1).unwrap();
                a.ret().unwrap();
            }),
        ));
    }
    for (at, bytes) in &regions {
        m.write(*at, bytes).unwrap();
    }
    m.protect(CODE, 0x10000, READ | EXEC).unwrap();
    m.protect(STOP, 4096, READ | EXEC).unwrap();
    for (at, bytes) in regions {
        m.add_region(Region::new(at, bytes.len())).unwrap();
    }
    m
}

pub(crate) fn seed(m: &mut Executor, iterations: u32) {
    m.state.regs = [0, iterations, 0, 0, STACK, 0, 0, 0];
    m.state.eip = CODE;
    m.state.eflags = 0x202;
    m.state.xmm[0] = [0; 16];
    m.state.xmm[1] = [7; 16];
    m.write(STACK, &STOP.to_le_bytes()).unwrap();
}

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    values[values.len() / 2]
}

fn main() {
    pin();
    let samples: usize = std::env::args().nth(1).map_or(7, |a| a.parse().unwrap());
    let iterations: u32 = std::env::args()
        .nth(2)
        .map_or(200_000, |a| a.parse().unwrap());
    assert!(samples > 0 && iterations > 0);
    let shadow: bool = std::env::args().nth(3).is_some_and(|a| a == "1");
    println!(
        "targets,iterations,native_entries,dispatch_ms,tail_ms,dispatch_ns_per_entry,tail_ns_per_entry,speedup,code_bytes,compile_ms,committed,reserved,shadow,matches,hits,misses,overflows"
    );
    for targets in [1, 64] {
        let mut m = setup(targets, shadow);
        seed(&mut m, iterations);
        assert_eq!(
            m.run_unicorn_unbounded(STOP).unwrap(),
            RunOutcome::ReachedStop
        );
        let expected = m.state.clone();
        let mut timings: [Vec<f64>; 2] = Default::default();
        let mut entries = 0;
        let mut predictions = (0, 0, 0, 0);
        for round in 0..samples + 1 {
            for offset in 0..2 {
                let variant = (round + offset) % 2;
                m.set_region_linking(variant == 1);
                seed(&mut m, iterations);
                let before = m.stats.native_entries;
                let counts = (
                    m.stats.shadow_matches,
                    m.stats.shadow_hits,
                    m.stats.shadow_misses,
                    m.stats.shadow_overflows,
                );
                let start = Instant::now();
                let result = m.run(STOP, u64::from(iterations) * 8 + 100);
                let elapsed = start.elapsed().as_secs_f64();
                assert_eq!(result.unwrap(), RunOutcome::ReachedStop);
                entries = m.stats.native_entries - before;
                if variant == 1 {
                    predictions = (
                        m.stats.shadow_matches - counts.0,
                        m.stats.shadow_hits - counts.1,
                        m.stats.shadow_misses - counts.2,
                        m.stats.shadow_overflows - counts.3,
                    );
                }
                assert_eq!(m.state.regs, expected.regs);
                assert_eq!(m.state.eflags, expected.eflags);
                assert_eq!(m.state.xmm, expected.xmm);
                assert_eq!(m.stats.interpreter_entries, 0);
                if round != 0 {
                    timings[variant].push(elapsed);
                }
            }
        }
        let dispatch = median(&mut timings[0]);
        let tail = median(&mut timings[1]);
        #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
        let (committed, reserved) = m.native_code_storage_bytes().unwrap();
        #[cfg(not(all(windows, target_arch = "x86_64", target_env = "msvc")))]
        let (committed, reserved) = (0, 0);
        println!(
            "{targets},{iterations},{entries},{:.6},{:.6},{:.3},{:.3},{:.3},{},{:.3},{committed},{reserved},{},{},{},{},{}",
            dispatch * 1e3,
            tail * 1e3,
            dispatch * 1e9 / entries as f64,
            tail * 1e9 / entries as f64,
            dispatch / tail,
            m.stats.native_code_bytes,
            m.stats.compile_time.as_secs_f64() * 1000.0,
            u8::from(shadow),
            predictions.0,
            predictions.1,
            predictions.2,
            predictions.3
        );
    }
}
