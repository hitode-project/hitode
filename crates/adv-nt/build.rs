fn main() {
    for file in ["pixels.c", "bcdec.h", "stb_dxt.h"] {
        println!("cargo:rerun-if-changed=src/native/{file}");
    }
    cc::Build::new()
        .file("src/native/pixels.c")
        .opt_level(3)
        .flag_if_supported("-ffp-contract=off")
        .flag_if_supported("-frounding-math")
        .warnings(false)
        .compile("adv_pixels");
}
