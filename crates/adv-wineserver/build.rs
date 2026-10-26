//! Compiles the vendored wineserver with Hitode's startup and host services.

use std::path::Path;

fn main() {
    println!("cargo:rerun-if-changed=c");
    println!("cargo:rerun-if-changed=../../vendor/wine");
    let apple = std::env::var("CARGO_CFG_TARGET_VENDOR").as_deref() == Ok("apple");
    if !apple && std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("android") {
        return;
    }
    let wine = Path::new("../../vendor/wine");
    let mut sources: Vec<_> = std::fs::read_dir(wine.join("server"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "c"))
        .collect();
    sources.sort();
    for source in &mut sources {
        if source.file_name().is_some_and(|name| name == "queue.c") {
            *source = "c/queue.c".into();
        }
    }
    cc::Build::new()
        .include("c")
        .include(wine.join("include"))
        .include(wine.join("server"))
        .flag("-include")
        .flag(Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("c/shim.h"))
        .define("__WINESRC__", None)
        .define("_REENTRANT", None)
        // The server resolves its data directory relative to its executable
        // path, which the host service reports inside the Wine installation.
        .define("BINDIR", "\"/\"")
        .define("DATADIR", "\"/share\"")
        .opt_level(2)
        .warnings(false)
        .files(sources)
        .file("c/main.c")
        .compile("adv_wineserver");
    cc::Build::new()
        .file("c/host.c")
        .opt_level(2)
        .flag("-Wno-unused-parameter")
        .compile("adv_wineserver_host");
}
