//! Build the host Theora decoder and XAudio2 reverb DSP.
fn main() {
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
