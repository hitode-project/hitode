//! Precompile every host stage through the GPU layer's backend-free compiler.
use std::{env, fs, path::PathBuf};
#[path = "src/stages/bindings.rs"]
mod bindings;
fn main() {
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let read = |name: &str| {
        let path = format!("src/stages/{name}.wgsl");
        println!("cargo:rerun-if-changed={path}");
        fs::read_to_string(path).unwrap()
    };
    let software = read("software");
    for (name, wgsl, bindings) in [
        (
            "ingest",
            software.clone() + &read("ingest"),
            bindings::INGEST.to_vec(),
        ),
        (
            "video",
            software.clone() + &read("video") + &read("video_planes"),
            bindings::VIDEO
                .into_iter()
                .chain(bindings::INGEST)
                .collect(),
        ),
        (
            "video_converted",
            software + &read("video") + &read("video_converted"),
            bindings::VIDEO
                .into_iter()
                .filter(|b| {
                    matches!(
                        b.kind,
                        adv_gpu::pipeline::BindingKind::UniformBuffer
                            | adv_gpu::pipeline::BindingKind::StorageBuffer { .. }
                    )
                })
                .chain(bindings::INGEST)
                .collect(),
        ),
    ] {
        let compiled = adv_gpu::shader::precompile(&wgsl, &bindings).unwrap();
        fs::write(
            out.join(format!("{name}.spv.rs")),
            format!("{:?}", compiled.spirv),
        )
        .unwrap();
        fs::write(out.join(format!("{name}.msl")), compiled.metal).unwrap();
        fs::write(
            out.join(format!("{name}.identity.rs")),
            compiled.identity.to_string(),
        )
        .unwrap();
    }
}
