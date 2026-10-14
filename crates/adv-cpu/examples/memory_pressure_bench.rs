//! Drives the x64 executor, so it runs only on x86_64 hosts; the benchmark
//! itself is in x64/memory_pressure_bench.rs.
#[cfg(target_arch = "x86_64")]
#[path = "x64/memory_pressure_bench.rs"]
mod bench;

fn main() {
    #[cfg(target_arch = "x86_64")]
    bench::main();
    #[cfg(not(target_arch = "x86_64"))]
    eprintln!("memory_pressure_bench drives the x64 executor; run it on an x86_64 host");
}
