//! Compiles the vendored wineserver with Hitode's startup and host services.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The server symbols that Rust uses. Every other server global is made local,
/// because names such as `release_object` also exist in other linked libraries.
const EXPORTS: &[&str] = &[
    "hitode_server_main",
    "hitode_server_key_state",
    "server_dir",
    "master_socket_timeout",
    "flush_registry",
];

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
    let mut server = cc::Build::new();
    server
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
        // Common symbols would stay global in the merged object.
        .flag("-fno-common")
        .files(sources)
        .file("c/main.c");
    link_server(&server, apple);
    cc::Build::new()
        .file("c/host.c")
        .opt_level(2)
        .flag("-Wno-unused-parameter")
        .compile("adv_wineserver_host");
}

/// Archives the server objects as one object that defines only `EXPORTS`.
fn link_server(build: &cc::Build, apple: bool) {
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let objects = build.compile_intermediates();
    let merged = out.join("adv_wineserver.o");
    if apple {
        let list = out.join("adv_wineserver.exports");
        let names: String = EXPORTS.iter().map(|name| format!("_{name}\n")).collect();
        std::fs::write(&list, names).unwrap();
        // Without -keep_private_externs, the hidden globals become local.
        run(Command::new("ld")
            .arg("-r")
            .arg("-exported_symbols_list")
            .arg(&list)
            .args(&objects)
            .arg("-o")
            .arg(&merged));
    } else {
        let tools = build.get_compiler().path().parent().unwrap().to_owned();
        run(Command::new(tools.join("ld.lld"))
            .arg("-r")
            .args(&objects)
            .arg("-o")
            .arg(&merged));
        let mut localize = Command::new(tools.join("llvm-objcopy"));
        for name in EXPORTS {
            localize.arg(format!("--keep-global-symbol={name}"));
        }
        run(localize.arg(&merged));
    }
    let archive = out.join("libadv_wineserver.a");
    let _ = std::fs::remove_file(&archive);
    run(build.get_archiver().arg("crs").arg(&archive).arg(&merged));
    println!("cargo:rustc-link-search=native={}", out.display());
    println!("cargo:rustc-link-lib=static=adv_wineserver");
}

fn run(command: &mut Command) {
    let status = command
        .status()
        .unwrap_or_else(|error| panic!("{command:?}: {error}"));
    assert!(status.success(), "{command:?} failed with {status}");
}
