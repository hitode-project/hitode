//! Isolate scheduler scaling with independent, precompiled guest work.
//! Diagnostic only: this does not establish game throughput or playability.
#[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
fn main() {
    use adv_abi::Registry;
    use adv_cpu::{
        CompileStrategy, JitCpu, Region,
        backend::{Guest, Prot, Reg, asm::Asm},
    };
    use adv_machine::{Clock, Conv, Machine, Outcome, RunResult, SliceResult, layout::PARK_ADDR};
    use std::time::{Duration, Instant};

    const CODE: u32 = 0x400000;
    const CONTEXTS: u32 = 4;
    const ITERATIONS: u32 = 50_000_000;
    println!("workers,seconds,completed,leases,overlap_ms,queue_ms");
    for workers in [1, 2, 2, 1] {
        let mut registry = Registry::default();
        registry.add("bench", "done", Conv::Stdcall { argc: 0 }, |e| {
            assert_eq!(e.cpu.reg(Reg::Ecx), 0);
            assert_eq!(e.cpu.reg(Reg::Ebx), e.ctx().teb);
            e.ext.get_mut::<Vec<u32>>().push(e.context);
            Outcome::ExitThread(0)
        });
        let mut cpu = JitCpu::new(PARK_ADDR, true).unwrap();
        cpu.set_guest_workers(workers).unwrap();
        cpu.executor_mut().set_tiered_compilation(false);
        cpu.set_auto_compile_strategy(CompileStrategy::Fast);
        let mut machine = Machine::new(cpu, registry, Clock::real()).unwrap();
        machine.ext.insert(Vec::<u32>::new());
        machine.set_frame_handoff(false);
        machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
        let done = machine.resolve_import("bench", "done").unwrap();
        for id in 0..CONTEXTS {
            let address = CODE + id * 256;
            let mut code = Asm::new(address);
            code.mov_eax_fs(0x18).mov_ebx_eax().mov_ecx_imm(ITERATIONS);
            let top = code.here();
            code.dec_ecx().jnz(top).call(done).hlt();
            machine.cpu.write(address, &code.bytes).unwrap();
            let mut region = Region::new(address, code.bytes.len());
            region.compile_strategy = CompileStrategy::Fast;
            machine.cpu.executor_mut().add_region(region).unwrap();
            machine.spawn("compute", address, &[], 65536).unwrap();
        }
        let started = Instant::now();
        loop {
            assert!(started.elapsed() < Duration::from_secs(30));
            match machine.run_slice(Duration::from_millis(8)) {
                SliceResult::Complete(RunResult::Exited(_)) => break,
                SliceResult::Complete(RunResult::Failed(error)) => panic!("{error}"),
                _ => {}
            }
        }
        let seconds = started.elapsed().as_secs_f64();
        let mut completed = machine.ext.get::<Vec<u32>>().clone();
        completed.sort();
        assert_eq!(completed, (0..CONTEXTS).collect::<Vec<_>>());
        let stats = machine.cpu.parallel_stats();
        assert_eq!(stats.submitted, stats.completed);
        println!(
            "{workers},{seconds:.6},{CONTEXTS},{},{:.3},{:.3}",
            stats.completed,
            stats.overlap_ns as f64 / 1e6,
            stats.queue_wait_ns as f64 / 1e6
        );
    }
}

#[cfg(not(all(windows, target_arch = "x86_64", target_env = "msvc")))]
fn main() {
    eprintln!("N=2 currently requires Windows x64 MSVC");
}
