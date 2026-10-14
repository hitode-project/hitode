//! Drives the x64 executor, so it runs only on x86_64 hosts; the benchmark
//! itself is in x64/topology_bench.rs.
#[cfg(target_arch = "x86_64")]
#[path = "x64/topology_bench.rs"]
mod bench;

fn main() {
    #[cfg(target_arch = "x86_64")]
    bench::main();
    #[cfg(not(target_arch = "x86_64"))]
    eprintln!("topology_bench drives the x64 executor; run it on an x86_64 host");
}
