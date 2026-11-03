//! Compile host WGSL for both backends; argument indices follow adv-gpu.
use std::{
    env, fs,
    hash::{DefaultHasher, Hasher},
    path::PathBuf,
};
fn main() {
    let source = "src/stages/ingest.wgsl";
    println!("cargo:rerun-if-changed={source}");
    let shared = "src/stages/software.wgsl";
    println!("cargo:rerun-if-changed={shared}");
    let wgsl = fs::read_to_string(shared).unwrap() + &fs::read_to_string(source).unwrap();
    let module = naga::front::wgsl::parse_str(&wgsl).unwrap();
    let info = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .unwrap();
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let mut identity = DefaultHasher::new();
    identity.write(wgsl.as_bytes());
    fs::write(
        out.join("ingest.identity.rs"),
        identity.finish().to_string(),
    )
    .unwrap();
    let spirv = naga::back::spv::write_vec(&module, &info, &Default::default(), None).unwrap();
    fs::write(out.join("ingest.spv.rs"), format!("{spirv:?}")).unwrap();
    let mut options = naga::back::msl::Options {
        lang_version: (2, 1),
        fake_missing_bindings: false,
        bounds_check_policies: naga::proc::BoundsCheckPolicies {
            index: naga::proc::BoundsCheckPolicy::Unchecked,
            buffer: naga::proc::BoundsCheckPolicy::Unchecked,
            ..Default::default()
        },
        ..Default::default()
    };
    for entry in &module.entry_points {
        let mut resources = naga::back::msl::EntryPointResources {
            sizes_buffer: Some(30),
            ..Default::default()
        };
        for slot in 0..2 {
            resources.resources.insert(
                naga::ResourceBinding {
                    group: 3,
                    binding: slot,
                },
                naga::back::msl::BindTarget {
                    buffer: Some(29 - slot as u8),
                    ..Default::default()
                },
            );
        }
        options
            .per_entry_point_map
            .insert(entry.name.clone(), resources);
    }
    let (msl, translation) =
        naga::back::msl::write_string(&module, &info, &options, &Default::default()).unwrap();
    for name in translation.entry_point_names {
        name.unwrap();
    }
    fs::write(out.join("ingest.msl"), msl).unwrap();
}
