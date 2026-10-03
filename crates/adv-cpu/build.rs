//! Portable exact guest floating-point arithmetic.
fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("x86_64")
        && std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc")
    {
        cc::Build::new()
            .file("src/host_fault.c")
            .opt_level(3)
            .compile("adv_host_fault");
        println!("cargo:rerun-if-changed=src/host_fault.c");
    }
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
