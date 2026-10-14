//! Self-authored execution-boundary workloads, not a whole-game speedup claim.
//! Run the identical file on both revisions; compare medians on the same CPU.
use adv_cpu::{
    CompileStrategy, CpuState, EXEC, ExecutionTrap, Executor, Memory, READ, Region, RunOutcome,
    WRITE,
};
use iced_x86::code_asm::*;
use std::hint::black_box;
use std::time::Instant;

const CODE: u32 = 0x10000;
const STOP: u32 = 0x18000;
const APIS: u32 = 0x20000;
const STACK: u32 = 0x3f000;
const ITERATIONS: u32 = 100_000;

struct Services;
impl ExecutionTrap for Services {
    fn call(&mut self, state: &mut CpuState, _: &mut Memory, vector: u32) -> Result<bool, String> {
        assert_eq!(vector, 0x2e);
        state.regs[0] = 7;
        Ok(false)
    }
}

fn setup(bytes: &[u8], apis: u32, native: Option<CompileStrategy>, auto: bool) -> Executor {
    let mut m = Executor::new(CODE, 0x30000).unwrap();
    m.protect(CODE, 0x30000, READ | WRITE).unwrap();
    m.write(CODE, bytes).unwrap();
    for id in 0..apis {
        let mut thunk = [0x90; 16];
        thunk[..3].copy_from_slice(&[0xcd, 0x2e, 0xc3]);
        m.write(APIS + id * 16, &thunk).unwrap();
    }
    m.protect(CODE, 4096, READ | EXEC).unwrap();
    m.write(STACK, &STOP.to_le_bytes()).unwrap();
    if let Some(strategy) = native {
        let mut region = Region::new(CODE, bytes.len());
        region.compile_strategy = strategy;
        m.add_region(region).unwrap();
    }
    m.enable_auto_jit(auto);
    m.set_async_compilation(auto);
    m
}

fn run(m: &mut Executor) {
    m.state.eip = CODE;
    m.state.regs[4] = STACK;
    assert_eq!(
        m.run_with_traps(STOP, 100_000_000, &mut Services, false)
            .unwrap(),
        RunOutcome::ReachedStop
    );
    black_box(m.state.regs);
}

fn record(name: &str, samples: usize, repetitions: usize, m: &mut Executor, expected: u32) {
    // Equal priming on both versions; automatic compilation is included in cold_ms.
    let cold = Instant::now();
    run(m);
    assert_eq!(m.state.regs[0], expected);
    let cold_ms = cold.elapsed().as_secs_f64() * 1000.0;
    let mut settled = 0;
    for _ in 0..1024 {
        run(m);
        if m.stats.pending_compilations == 0 {
            settled += 1;
        } else {
            settled = 0;
        }
        if settled >= 8 {
            break;
        }
        std::thread::yield_now();
    }
    assert_eq!(
        m.stats.pending_compilations, 0,
        "compiler did not settle during warmup"
    );
    let mut timings = Vec::new();
    let before_i = m.stats.interpreter_instructions;
    let before_e = m.stats.interpreter_entries;
    for _ in 0..samples {
        let start = Instant::now();
        for _ in 0..repetitions {
            run(m);
        }
        timings.push(start.elapsed().as_secs_f64() * 1e9 / repetitions as f64);
        assert_eq!(m.state.regs[0], expected);
    }
    timings.sort_by(f64::total_cmp);
    println!(
        "{name},{samples},{repetitions},{:.2},{:.3},{},{},{},{},{}",
        timings[samples / 2],
        cold_ms,
        m.stats.interpreter_instructions - before_i,
        m.stats.interpreter_entries - before_e,
        m.stats.compilations,
        m.stats.native_code_bytes,
        expected
    );
}

pub fn main() {
    let samples: usize = std::env::args()
        .nth(1)
        .map_or(5, |s| s.parse().expect("sample count"));
    assert!(samples > 0);
    println!(
        "workload,samples,repetitions,median_ns_per_run,cold_ms,interpreted_units,interpreter_entries,compilations,native_bytes,checksum"
    );
    for width in [4, 32] {
        let mut a = CodeAssembler::new(32).unwrap();
        let mut top = a.create_label();
        a.xor(eax, eax).unwrap();
        a.mov(ecx, ITERATIONS).unwrap();
        a.set_label(&mut top).unwrap();
        for _ in 0..width {
            a.add(eax, 3).unwrap();
        }
        a.dec(ecx).unwrap();
        a.jnz(top).unwrap();
        a.ret().unwrap();
        let bytes = a.assemble(CODE as u64).unwrap();
        let expected = ITERATIONS * width * 3;
        let mut interp = setup(&bytes, 0, None, false);
        record(
            &format!("interpreter_alu_{width}"),
            samples,
            1,
            &mut interp,
            expected,
        );
        if width == 32 {
            let mut auto = setup(&bytes, 0, None, true);
            record("automatic_alu_32", samples, 1, &mut auto, expected);
            for (label, strategy) in [
                ("native_fast_32", CompileStrategy::Fast),
                ("native_speed_32", CompileStrategy::Speed),
            ] {
                let mut m = setup(&bytes, 0, Some(strategy), false);
                record(label, samples, 1, &mut m, expected);
            }
        }
    }
    let mut a = CodeAssembler::new(32).unwrap();
    let mut top = a.create_label();
    a.xor(eax, eax).unwrap();
    a.mov(ecx, ITERATIONS).unwrap();
    a.set_label(&mut top).unwrap();
    for _ in 0..8 {
        a.push(eax).unwrap();
        a.pop(edx).unwrap();
        a.lea(eax, ptr(edx + 1)).unwrap();
    }
    a.dec(ecx).unwrap();
    a.jnz(top).unwrap();
    a.ret().unwrap();
    let bytes = a.assemble(CODE as u64).unwrap();
    let mut m = setup(&bytes, 0, None, false);
    record("interpreter_stack", samples, 1, &mut m, ITERATIONS * 8);
    // Short executions model repeated API-bearing execution slices. Registration
    // itself is outside timing; this specifically measures the service boundary.
    for apis in [0, 512] {
        let mut m = setup(
            &[0xb8, 42, 0, 0, 0, 0xc3],
            apis,
            Some(CompileStrategy::Speed),
            false,
        );
        record(
            &format!("service_slices_{apis}"),
            samples,
            10_000,
            &mut m,
            42,
        );
    }
    // Collocated mutable data in an executable page: code bytes never change.
    // This measures needless recompilation, not removal of self-modification checks.
    let mut m = setup(
        &[0xb8, 42, 0, 0, 0, 0xc3],
        0,
        Some(CompileStrategy::Speed),
        false,
    );
    m.protect(CODE, 4096, READ | WRITE | EXEC).unwrap();
    run(&mut m); // Compile against the final permissions before measurement.
    let mut times = Vec::new();
    for sample in 0..samples {
        let start = Instant::now();
        for index in 0..100u32 {
            m.write(CODE + 0x800, &(index + sample as u32).to_le_bytes())
                .unwrap();
            run(&mut m);
        }
        times.push(start.elapsed().as_secs_f64() * 1e9 / 100.0);
        assert_eq!(m.state.regs[0], 42);
    }
    times.sort_by(f64::total_cmp);
    println!(
        "same_page_data_write,{samples},100,{:.2},0,0,0,{},{},42",
        times[samples / 2],
        m.stats.compilations,
        m.stats.native_code_bytes
    );
}
