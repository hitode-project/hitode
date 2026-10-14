//! Identical self-authored workloads for comparing automatic region discovery.
//! Save the baseline executable before modifying the implementation.
use adv_cpu::{EXEC, Executor, READ, RunOutcome, WRITE};
use iced_x86::code_asm::*;
use std::time::Instant;

const CODE: u32 = 0x10000;
const STOP: u32 = 0x28000;
const STACK: u32 = 0x2f800;

#[cfg(windows)]
fn pin() {
    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut std::ffi::c_void;
        fn SetProcessAffinityMask(process: *mut std::ffi::c_void, mask: usize) -> i32;
    }
    // SAFETY: CPU 0 is a valid affinity bit on the benchmark host.
    assert_ne!(unsafe { SetProcessAffinityMask(GetCurrentProcess(), 1) }, 0);
}
#[cfg(not(windows))]
fn pin() {}

fn code(at: u32, f: impl FnOnce(&mut CodeAssembler)) -> Vec<u8> {
    let mut a = CodeAssembler::new(32).unwrap();
    f(&mut a);
    a.assemble(u64::from(at)).unwrap()
}

fn machine() -> Executor {
    let mut m = Executor::new(CODE, 0x20000).unwrap();
    m.protect(CODE, 0x20000, READ | WRITE).unwrap();
    m.enable_auto_jit(true);
    // Isolate deterministic discovery and compilation cost from worker latency.
    // Production asynchronous submission consumes the same region snapshots.
    m.set_async_compilation(false);
    m
}

fn seed(m: &mut Executor, pc: u32, count: u32) {
    m.state.eip = pc;
    m.state.regs = [0, count, 0, 0, STACK, 0, 0, 0];
    m.state.eflags = 0x202;
    m.write(STACK, &STOP.to_le_bytes()).unwrap();
}

fn run(m: &mut Executor, pc: u32, count: u32, expected: u32) {
    seed(m, pc, count);
    assert_eq!(
        m.run(STOP, u64::from(count) * 100 + 100).unwrap(),
        RunOutcome::ReachedStop
    );
    assert_eq!(m.state.regs[0], expected);
    assert_eq!(m.state.regs[1], 0);
    assert_eq!(m.state.regs[4], STACK + 4);
}

fn trace(blocks: u32, stride: u32, samples: usize, iterations: u32) {
    let mut m = machine();
    for index in 0..blocks {
        let at = CODE + index * stride;
        let bytes = code(at, |a| {
            a.add(eax, index as i32 + 1).unwrap();
            a.xor(ebx, eax).unwrap();
            if index + 1 < blocks {
                a.jmp(u64::from(at + stride)).unwrap();
            } else {
                a.dec(ecx).unwrap();
                a.jnz(u64::from(CODE)).unwrap();
                a.ret().unwrap();
            }
        });
        m.write(at, &bytes).unwrap();
    }
    m.protect(CODE, 0x16000, READ | EXEC).unwrap();
    let expected = iterations.wrapping_mul(blocks * (blocks + 1) / 2);
    let cold = Instant::now();
    run(&mut m, CODE, iterations, expected);
    let cold_ms = cold.elapsed().as_secs_f64() * 1000.0;
    let before_i = m.stats.interpreter_instructions;
    let before_n = m.stats.native_entries;
    let mut times = Vec::new();
    for _ in 0..samples {
        let start = Instant::now();
        run(&mut m, CODE, iterations, expected);
        times.push(start.elapsed().as_secs_f64() * 1e9);
    }
    times.sort_by(f64::total_cmp);
    println!(
        "trace_{blocks}_{stride},{:.3},{:.3},{:.3},{},{},{},{},{}",
        times[samples / 2] / (f64::from(iterations) * f64::from(blocks)),
        cold_ms,
        m.stats.compile_time.as_secs_f64() * 1000.0,
        m.stats.compilations,
        m.stats.native_code_bytes,
        (m.stats.native_entries - before_n) / samples as u64,
        m.stats.interpreter_instructions - before_i,
        expected
    );
}

fn entries(samples: usize) {
    let mut cold_times = Vec::new();
    let mut compile_ms = 0.0;
    let mut compiled = 0;
    let mut bytes = 0;
    for _ in 0..samples {
        let mut m = machine();
        for index in 0..32u32 {
            let at = CODE + index * 8;
            m.write(
                at,
                &code(at, |a| {
                    a.mov(eax, index as i32).unwrap();
                    a.ret().unwrap();
                }),
            )
            .unwrap();
        }
        m.protect(CODE, 4096, READ | EXEC).unwrap();
        let start = Instant::now();
        for index in 0..32u32 {
            for _ in 0..33 {
                run(&mut m, CODE + index * 8, 0, index);
            }
        }
        cold_times.push(start.elapsed().as_secs_f64() * 1000.0);
        compile_ms = m.stats.compile_time.as_secs_f64() * 1000.0;
        compiled = m.stats.compilations;
        bytes = m.stats.native_code_bytes;
    }
    cold_times.sort_by(f64::total_cmp);
    println!(
        "entries_32,0,{:.3},{compile_ms:.3},{compiled},{bytes},0,0,31",
        cold_times[samples / 2]
    );
}

pub fn main() {
    pin();
    let samples = std::env::args()
        .nth(1)
        .map_or(7, |n| n.parse::<usize>().unwrap());
    let iterations = std::env::args()
        .nth(2)
        .map_or(200_000, |n| n.parse::<u32>().unwrap());
    assert!(samples > 0 && iterations > 32);
    println!(
        "workload,ns_per_guest_transfer,cold_ms,compile_ms,compilations,native_bytes,native_entries,interpreted_units,checksum"
    );
    trace(2, 512, samples, iterations);
    trace(16, 512, samples, iterations);
    trace(8, 1536, samples, iterations);
    trace(2, 20_480, samples, iterations);
    entries(samples);
}
