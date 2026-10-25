use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=freetype");
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    // Keep the upstream source intact and select the distribution's scalar
    // conic path on every host. Standalone error values match FreeType's ABI.
    let source = fs::read_to_string("freetype/ftgrays.c").unwrap();
    let source = source
        .replace("#  define BEZIER_USE_DDA  1", "#  define BEZIER_USE_DDA  0")
        .replace(
            "Smooth_Err_Invalid_Outline      -1",
            "Smooth_Err_Invalid_Outline      20",
        )
        .replace(
            "Smooth_Err_Cannot_Render_Glyph  -2",
            "Smooth_Err_Cannot_Render_Glyph  19",
        )
        .replace(
            "Smooth_Err_Invalid_Argument     -3",
            "Smooth_Err_Invalid_Argument     6",
        )
        .replace(
            "Smooth_Err_Raster_Overflow      -4",
            "Smooth_Err_Raster_Overflow      98",
        )
        .replace(
            "static gray_TRaster  the_raster;",
            "gray_PRaster raster = (gray_PRaster)calloc(1, sizeof(gray_TRaster));",
        )
        .replace(
            "*araster = (FT_Raster)&the_raster;",
            "*araster = (FT_Raster)raster;\n    if (!raster) return 64;",
        )
        .replace("FT_ZERO( &the_raster );", "")
        .replacen("FT_UNUSED( raster );", "free(raster);", 1);
    fs::write(out.join("ftgrays-i386.c"), source).unwrap();
    cc::Build::new()
        .file("freetype/raster.c")
        .include("freetype")
        .include(out)
        .warnings(false)
        .compile("hitode_freetype_raster");
}
