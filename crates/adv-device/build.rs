//! Build the host Theora decoder and XAudio2 reverb DSP.
fn main() {
    if std::env::var_os("CARGO_FEATURE_MEDIA").is_some()
        && matches!(
            std::env::var("CARGO_CFG_TARGET_OS").as_deref(),
            Ok("macos" | "ios")
        )
    {
        // Static FFmpeg's VideoToolbox code also needs CoreVideo, which the
        // wgpu Metal backend used to link for it.
        for framework in ["VideoToolbox", "CoreMedia", "CoreVideo"] {
            println!("cargo:rustc-link-lib=framework={framework}");
        }
        link_clang_runtime();
    }
    cc::Build::new()
        .file("src/xaudio2/reverb.c")
        .define("NDEBUG", None)
        .warnings(false)
        .opt_level(3)
        .compile("adv_xaudio_reverb");
    println!("cargo:rerun-if-changed=src/xaudio2/reverb.c");
    let root = "../../vendor/libtheora";
    let mut build = cc::Build::new();
    build
        .include(format!("{root}/include"))
        .include(format!("{root}/lib"))
        .opt_level(3)
        .warnings(false)
        .flag_if_supported("-fno-strict-aliasing")
        .flag_if_supported("-fwrapv");
    for name in [
        "bitpack", "decode", "dequant", "fragment", "huffdec", "idct", "internal", "quant", "state",
    ] {
        build.file(format!("{root}/lib/{name}.c"));
    }
    if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("aarch64") {
        build
            .define("OC_STATE_USE_VTABLE", None)
            .define("ADV_THEORA_NEON", None);
        build.file("src/theora/neon.c");
    }
    build.file("src/theora/bridge.c").compile("adv_theora");
    println!("cargo:rerun-if-changed={root}");
    println!("cargo:rerun-if-changed=src/theora");
}

/// Static FFmpeg checks the OS version with `@available`, which calls
/// `__isPlatformVersionAtLeast` from clang's runtime library; rustc links
/// Apple targets with `-nodefaultlibs`, which leaves that library out.
fn link_clang_runtime() {
    let compiler = cc::Build::new().get_compiler();
    let Ok(output) = std::process::Command::new(compiler.path())
        .args(compiler.args())
        .arg("-print-libgcc-file-name")
        .output()
    else {
        return;
    };
    let path = std::path::PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    let (Some(dir), Some(name)) = (
        path.parent(),
        path.file_stem()
            .and_then(|stem| stem.to_str())
            .and_then(|stem| stem.strip_prefix("lib")),
    ) else {
        return;
    };
    if output.status.success() && path.is_file() {
        println!("cargo:rustc-link-search=native={}", dir.display());
        println!("cargo:rustc-link-lib=static={name}");
    }
}
