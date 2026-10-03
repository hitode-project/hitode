//! Synthetic scalar reads; no window, audio device, or guest application.
use adv_cpu::{CpuState, EXEC, Executor, READ, Region, RunOutcome, WRITE};
use iced_x86::code_asm::*;
use std::time::Instant;

const CODE: u32 = 0x10000;
const STOP: u32 = 0x11000;
const DATA: u32 = 0x20000;
const STACK: u32 = 0x40000;

fn main() {
    let samples: usize = std::env::args().nth(1).map_or(9, |s| s.parse().unwrap());
    #[cfg(windows)]
    {
        unsafe extern "system" {
            fn GetCurrentProcess() -> *mut std::ffi::c_void;
            fn SetProcessAffinityMask(process: *mut std::ffi::c_void, mask: usize) -> i32;
        }
        assert_ne!(unsafe { SetProcessAffinityMask(GetCurrentProcess(), 1) }, 0);
    }
    println!(
        "operation,policy,loads,median_ns_per_load,interpreted,native_entries,median_compile_ns,logical_code_bytes,code_xdata_committed,code_xdata_reserved"
    );
    for operation in ["entry_load", "loop_load"] {
        let count = if operation == "loop_load" { 16384 } else { 1 };
        let repeats = if count == 1 { 10000 } else { 50 };
        let mut a = CodeAssembler::new(32).unwrap();
        let mut top = a.create_label();
        a.set_label(&mut top).unwrap();
        a.mov(eax, dword_ptr(esi)).unwrap();
        a.add(eax, ebx).unwrap();
        if count != 1 {
            a.add(esi, 4).unwrap();
            a.dec(ecx).unwrap();
            a.jnz(top).unwrap();
        }
        a.ret().unwrap();
        let code = a.assemble(CODE as u64).unwrap();
        for policy in [false, true] {
            let mut cpu = Executor::address_space().unwrap();
            cpu.map(CODE, 4096, READ | EXEC).unwrap();
            cpu.map(STOP, 4096, READ | EXEC).unwrap();
            cpu.map(DATA, 65536, READ | WRITE).unwrap();
            cpu.map(STACK, 4096, READ | WRITE).unwrap();
            cpu.write(CODE, &code).unwrap();
            cpu.write(STACK, &STOP.to_le_bytes()).unwrap();
            #[cfg(windows)]
            if policy {
                cpu.enable_native_protection().unwrap();
            }
            cpu.add_region(Region::new(CODE, code.len())).unwrap();
            let mut times = Vec::new();
            let mut interpreted = 0;
            let mut entries = 0;
            for sample in 0..samples + 3 {
                let before = (cpu.stats.interpreter_instructions, cpu.stats.native_entries);
                let started = Instant::now();
                for _ in 0..repeats {
                    cpu.state = CpuState::default();
                    cpu.state.eip = CODE;
                    cpu.state.regs[1] = count;
                    cpu.state.regs[3] = 17;
                    cpu.state.regs[4] = STACK;
                    cpu.state.regs[6] = DATA;
                    assert_eq!(cpu.run(STOP, 100000).unwrap(), RunOutcome::ReachedStop);
                    assert_eq!(cpu.state.regs[0], 17);
                }
                if sample >= 3 {
                    times.push(
                        started.elapsed().as_nanos() as f64 / f64::from(count) / f64::from(repeats),
                    );
                    interpreted = cpu.stats.interpreter_instructions - before.0;
                    entries = cpu.stats.native_entries - before.1;
                }
            }
            times.sort_by(f64::total_cmp);
            let mut compile_times = Vec::with_capacity(samples);
            for sample in 0..samples + 3 {
                let before = cpu.stats.compile_time;
                cpu.set_region(Region::new(CODE, code.len())).unwrap();
                if sample >= 3 {
                    compile_times.push((cpu.stats.compile_time - before).as_nanos());
                }
            }
            compile_times.sort_unstable();
            #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
            let (committed, reserved) = cpu.native_code_storage_bytes().unwrap();
            #[cfg(not(all(windows, target_arch = "x86_64", target_env = "msvc")))]
            let (committed, reserved) = (0, 0);
            println!(
                "{operation},{policy},{},{:.3},{interpreted},{entries},{},{},{committed},{reserved}",
                count * repeats,
                times[times.len() / 2],
                compile_times[compile_times.len() / 2],
                cpu.stats.native_code_bytes,
            );
        }
    }
}
