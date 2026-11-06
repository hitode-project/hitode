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
    let common = read("upscale_common");
    for (name, wgsl, bindings) in [
        ("damage", read("damage"), bindings::DAMAGE.to_vec()),
        (
            "upscale_delta",
            common.clone() + &read("upscale_delta"),
            bindings::UPSCALE_DELTA.to_vec(),
        ),
        (
            "upscale_filter",
            common.clone() + &read("upscale_filter"),
            bindings::UPSCALE_FILTER.to_vec(),
        ),
        (
            "upscale_anchor",
            common.clone() + &read("upscale_anchor"),
            bindings::UPSCALE_ANCHOR.to_vec(),
        ),
        (
            "upscale_copy",
            common + &read("upscale_copy"),
            bindings::UPSCALE_COPY.to_vec(),
        ),
        (
            "output",
            software.clone() + &read("output"),
            bindings::OUTPUT
                .into_iter()
                .chain(bindings::INGEST)
                .collect(),
        ),
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
            // Passes at scale 1 sample texel centers and bind nearest there.
            let linear: Vec<u32> = pass
                .bindings
                .iter()
                .filter(|(_, role)| *role == table::Role::Linear && pass.scale != 1)
                .map(|&(slot, _)| slot)
                .collect();
            let source = snapped(&fs::read_to_string(&path).unwrap(), &linear);
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
/// Rewrites the samples through the samplers at `slots`, bound linear, as
/// loads weighed by phases snapped to 1/256. Normalized coordinates round
/// differently across a canvas and Vulkan quantizes their weights, so hardware
/// filtering could weigh one phase two ways; snapped, equal phases weigh alike
/// at every position and integer translations keep output bits (HOST-VIEW-13).
fn snapped(source: &str, slots: &[u32]) -> String {
    let names: Vec<&str> = source
        .lines()
        .filter(|line| line.contains(": sampler;"))
        .filter(|line| {
            slots
                .iter()
                .any(|slot| line.contains(&format!("@binding({slot})")))
        })
        .map(|line| {
            line.split("var ")
                .nth(1)
                .unwrap()
                .split(':')
                .next()
                .unwrap()
                .trim()
        })
        .collect();
    if names.is_empty() {
        return source.to_owned();
    }
    let call = "textureSampleLevel(";
    let mut out = String::with_capacity(source.len());
    // One helper per texture: SPIR-V from naga cannot pass textures.
    let mut textures: Vec<String> = vec![];
    let mut rest = source;
    while let Some(at) = rest.find(call) {
        out.push_str(&rest[..at]);
        // The call's four arguments, split at top-level commas.
        let body = &rest[at + call.len()..];
        let (mut depth, mut start, mut args, mut end) = (0, 0, vec![], 0);
        for (i, c) in body.char_indices() {
            match c {
                '(' => depth += 1,
                ')' if depth == 0 => {
                    args.push(body[start..i].trim());
                    end = i + 1;
                    break;
                }
                ')' => depth -= 1,
                ',' if depth == 0 => {
                    args.push(body[start..i].trim());
                    start = i + 1;
                }
                _ => {}
            }
        }
        let bare = |arg: &str| {
            arg.trim_start_matches('(')
                .trim_end_matches(')')
                .trim()
                .to_owned()
        };
        if args.len() == 4 && names.contains(&bare(args[1]).as_str()) {
            let texture = bare(args[0]);
            out.push_str(&format!("snapped_{texture}({})", args[2]));
            if !textures.contains(&texture) {
                textures.push(texture);
            }
        } else {
            out.push_str(&rest[at..at + call.len() + end]);
        }
        rest = &body[end..];
    }
    out.push_str(rest);
    for texture in textures {
        out += &SNAPPED_LINEAR.replace("TEXTURE", &texture);
    }
    out
}

const SNAPPED_LINEAR: &str = "
fn snapped_TEXTURE(uv: vec2<f32>) -> vec4<f32> {
    let size = textureDimensions(TEXTURE, 0);
    let p = round((uv * vec2<f32>(size) - 0.5) * 256.0) / 256.0;
    let base = floor(p);
    let f = p - base;
    let last = vec2<i32>(size) - 1;
    let a = clamp(vec2<i32>(base), vec2(0), last);
    if all(f == vec2(0.0)) { return textureLoad(TEXTURE, a, 0); }
    let b = clamp(vec2<i32>(base) + 1, vec2(0), last);
    let top = mix(textureLoad(TEXTURE, a, 0), textureLoad(TEXTURE, vec2(b.x, a.y), 0), f.x);
    let bottom = mix(textureLoad(TEXTURE, vec2(a.x, b.y), 0), textureLoad(TEXTURE, b, 0), f.x);
    return mix(top, bottom, f.y);
}
";

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
