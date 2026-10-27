//! Portable exact guest floating-point arithmetic and POSIX fault probes.

fn main() {
    cc::Build::new()
        .file("src/fault_recovery/posix.c")
        .file("src/fault_recovery/probe.S")
        .opt_level(3)
        .compile("adv_posix_fault");
    println!("cargo:rerun-if-changed=src/fault_recovery/posix.c");
    println!("cargo:rerun-if-changed=src/fault_recovery/probe.S");
    let mut build = cc::Build::new();
    build
        .include("softfloat/include")
        .define("SOFTFLOAT_FAST_INT64", None)
        .opt_level(3)
        .warnings(false)
        .file("softfloat/shim.c");
    let mut sources: Vec<_> = std::fs::read_dir("softfloat/source")
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "c"))
        .collect();
    sources.sort();
    build.files(sources).compile("adv_softfloat");
    println!("cargo:rerun-if-changed=softfloat");
}
