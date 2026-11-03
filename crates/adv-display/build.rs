//! Precompile every host stage through the GPU layer's backend-free compiler.
use std::{env, fs, path::PathBuf};
#[path = "src/stages/bindings.rs"]
mod bindings;
#[path = "src/stages/upscale/models.rs"]
mod models;
// The build script reads only the layouts of the shared table types.
#[allow(dead_code)]
#[path = "src/stages/upscale/table.rs"]
mod table;
fn main() {
    upscale(&PathBuf::from(env::var_os("OUT_DIR").unwrap()));
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
        fs::write(out.join(format!("{name}.msl")), &compiled.metal).unwrap();
        fs::write(
            out.join(format!("{name}.identity.rs")),
            compiled.identity.to_string(),
        )
        .unwrap();
        fs::write(out.join(format!("{name}.entries.rs")), entries(&compiled)).unwrap();
    }
}

/// Precompile each upscale shader once per precision, keyed by its path. The
/// table `upscale.rs` lists them for `stages::upscale`.
fn upscale(out: &std::path::Path) {
    let mut done: Vec<(&str, Vec<adv_gpu::pipeline::Binding>)> = vec![];
    let mut table = String::from("&[\n");
    for model in models::REFERENCE
        .iter()
        .chain(&models::FUSED)
        .chain(&models::MATRIX)
    {
        for pass in model.passes {
            let layout = pass.layout(model);
            if let Some((_, seen)) = done.iter().find(|(shader, _)| *shader == pass.shader) {
                assert_eq!(*seen, layout, "{} keeps one layout", pass.shader);
                continue;
            }
            let path = format!("src/stages/upscale/{}", pass.shader);
            println!("cargo:rerun-if-changed={path}");
            let source = fs::read_to_string(&path).unwrap();
            let mut forms = vec![];
            for (half, wgsl) in [(true, source.clone()), (false, widen(&source))] {
                if half && !source.contains("enable f16;") {
                    forms.push("None".to_owned());
                    continue;
                }
                let compiled = adv_gpu::shader::precompile(&wgsl, &layout)
                    .unwrap_or_else(|e| panic!("{}: {e}", pass.shader));
                let stem = format!(
                    "upscale-{}-{}",
                    pass.shader.replace(['/', '.'], "-"),
                    if half { "half" } else { "single" }
                );
                fs::write(
                    out.join(format!("{stem}.spv.rs")),
                    format!("{:?}", compiled.spirv),
                )
                .unwrap();
                fs::write(out.join(format!("{stem}.msl")), &compiled.metal).unwrap();
                forms.push(format!(
                    "Some(Compiled {{ spirv: &include!(concat!(env!(\"OUT_DIR\"), \"/{stem}.spv.rs\")), \
                     metal: include_bytes!(concat!(env!(\"OUT_DIR\"), \"/{stem}.msl\")), \
                     identity: {}, entries: &{} }})",
                    compiled.identity,
                    entries(&compiled)
                ));
            }
            table += &format!("({:?}, {}, {}),\n", pass.shader, forms[0], forms[1]);
            done.push((pass.shader, layout));
        }
    }
    fs::write(out.join("upscale.rs"), table + "]").unwrap();
}

/// The entry table of [`adv_gpu::shader::Source::Precompiled`].
fn entries(compiled: &adv_gpu::shader::Precompiled) -> String {
    let entries: String = compiled
        .entries
        .iter()
        .map(|(name, function, workgroup, threadgroup)| {
            format!(
                "adv_gpu::shader::PrecompiledEntry {{ name: {name:?}, function: {function:?}, \
                 workgroup: {workgroup:?}, threadgroup: &{threadgroup:?} }},"
            )
        })
        .collect();
    format!("[{entries}]")
}

/// Single-precision form of a half-precision pass for devices without shader
/// f16. Weights keep their f16-rounded values.
fn widen(source: &str) -> String {
    let source = source.replacen("enable f16;", "", 1);
    let mut out = String::with_capacity(source.len());
    // Words include dots, so `1.5e-05h` ends in the word `05h`.
    for (index, word) in source
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '.'))
        .enumerate()
    {
        // Replacements keep lengths, so `out.len()` indexes the next separator.
        if index > 0 {
            out.push(source.as_bytes()[out.len()] as char);
        }
        if word == "f16" {
            out.push_str("f32");
        } else if word.starts_with(|c: char| c.is_ascii_digit()) && word.ends_with('h') {
            out.push_str(&word[..word.len() - 1]);
            out.push('f');
        } else {
            out.push_str(word);
        }
    }
    out
}
