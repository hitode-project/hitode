use adv_dx::shader::{Shader, analysis::analyze, dxbc, ir, wgsl};

const CORPUS: &[&str] = &[
    "vs_4_0",
    "vs_4_1",
    "vs_5_0",
    "ps_4_0",
    "ps_4_1",
    "ps_5_0",
    "gs_4_0",
    "gs_4_1",
    "gs_5_0",
    "cs_4_0",
    "cs_4_1",
    "cs_5_0",
    "hs_5_0",
    "ds_5_0",
    "vs_4_0_level_9_1",
    "vs_4_0_level_9_3",
    "ps_4_0_level_9_1",
    "ps_4_0_level_9_3",
    "arithmetic",
    "control",
    "color",
    "discard_pixel",
    "typed_uav",
    "doubles",
    "encoded_dmov",
    "atomics",
    "raw",
    "texture_ops",
    "buffer_views",
    "counters",
    "texture_atomic",
    "texture_atomic_return",
    "raster_queries",
    "raster_samples",
    "dynamic_gather",
    "dynamic_gather_compare",
    "level_query",
    "derivative_queries",
    "multisample_queries",
    "integer_ops",
    "atomic_modes",
    "sample_modes",
    "coverage",
    "early_depth",
    "vertex_sample",
    "packed_buffer",
    "float_ops",
    "bit_ops",
    "branches",
    "indexed_inputs",
    "derivatives_4_0",
    "residency",
    "linkage",
];

fn fixture(name: &str, extension: &str) -> Vec<u8> {
    std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/shader-core")
            .join(format!("{name}.{extension}")),
    )
    .unwrap()
}

fn module(name: &str) -> ir::Module {
    dxbc::parse(&fixture(name, "dxbc")).unwrap_or_else(|error| panic!("{name}: {error:?}"))
}

#[test]
fn windows_containers_round_trip_and_disassemble() {
    let mut mismatches = Vec::new();
    for entry in std::fs::read_dir(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/shader-core"
    ))
    .unwrap()
    {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|ext| ext != "asm") {
            continue;
        }
        let name = path.file_stem().unwrap().to_str().unwrap();
        let bytes = fixture(name, "dxbc");
        if name == "diagnostic_pixel" {
            assert_eq!(dxbc::parse(&bytes), Err(dxbc::ParseError::Checksum));
            continue;
        }
        let container =
            dxbc::Container::decode(&bytes).unwrap_or_else(|error| panic!("{name}: {error:?}"));
        assert_eq!(container.encode().unwrap(), bytes, "{name}");
        let expected = fixture(name, "asm");
        let expected = std::str::from_utf8(&expected).unwrap();
        let actual = dxbc::disassemble(&bytes).unwrap_or_else(|error| panic!("{name}: {error:?}"));
        let actual: Vec<_> = actual.lines().map(str::trim_end).collect();
        let expected: Vec<_> = expected.lines().collect();
        for (index, (used, wanted)) in actual.iter().zip(&expected).enumerate() {
            if used != wanted {
                mismatches.push(format!("{name}:{}: {used:?} != {wanted:?}", index + 1));
            }
        }
        assert_eq!(actual.len(), expected.len(), "{name}");
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
    let mut bytes = fixture("vs_5_0", "dxbc");
    bytes[4] ^= 1;
    assert_eq!(dxbc::parse(&bytes), Err(dxbc::ParseError::Checksum));
}

#[test]
fn profile_metadata_and_semantic_passes_verify() {
    use ir::{BindingKind, Model, Register};
    assert_eq!(module("vs_4_0_level_9_1").model, Model::Level91);
    assert_eq!(module("ps_4_0_level_9_3").model, Model::Level93);
    assert!(!module("hs_5_0").signatures.patch_constant.is_empty());
    let vs = module("vs_5_0");
    let reflection = analyze(&vs);
    assert_eq!(
        reflection.bindings[0].kind,
        BindingKind::ConstantBuffer {
            bytes: 32,
            dynamic_indexing: false
        }
    );
    assert_eq!(reflection.reads[&(Register::Input, Some(1))], 3);
    assert_eq!(
        reflection.signatures.output[0].system_value,
        ir::SystemValue::Position
    );
    let declarations = vs.reflection.unwrap();
    assert_eq!(declarations.buffers[0].variables[1].byte_offset, 16);
    assert!(!vs.statistics.is_empty());
    let mut cs = module("arithmetic");
    let reflection = analyze(&cs);
    assert_eq!(reflection.workgroup_size, Some([8, 1, 1]));
    ir::passes::flush_denormals(&mut cs, &reflection);
    ir::verify(&cs).unwrap();
    ir::passes::lower_half_conversion(&mut cs, &reflection);
    ir::verify(&cs).unwrap();
    assert!(cs.body.iter().any(
        |stmt| matches!(stmt, ir::Statement::Execute(op) if op.opcode == ir::Opcode::F32ToF16Rtz)
    ));
}

#[test]
fn programmable_corpus_emits_valid_wgsl() {
    for &name in CORPUS {
        let module = module(name);
        if matches!(name, "doubles" | "encoded_dmov") {
            assert!(analyze(&module).doubles);
            assert!(matches!(
                wgsl::emit(&module, wgsl::Options::default()),
                Err(ir::Error::DoublePrecision)
            ));
            assert!(matches!(
                ir::interpreter::run(
                    &module,
                    &Default::default(),
                    &Default::default(),
                    &mut ir::interpreter::NoTextures
                ),
                Err(ir::Error::DoublePrecision)
            ));
        } else if let Some(error) = match name {
            "residency" => Some(ir::Error::UnsupportedOpcode(ir::Opcode::LdMsFeedback)),
            "linkage" => Some(ir::Error::UnsupportedRegister(ir::Register::Interface)),
            _ => None,
        } {
            let options = wgsl::Options {
                storage_formats: &[(1, naga::StorageFormat::R32Uint)],
                ..Default::default()
            };
            assert!(
                matches!(wgsl::emit(&module, options), Err(actual) if actual == error),
                "{name}"
            );
        } else if name == "typed_uav" {
            assert!(matches!(
                wgsl::emit(&module, wgsl::Options::default()),
                Err(ir::Error::MissingStorageFormat(1))
            ));
            let translation = wgsl::emit(
                &module,
                wgsl::Options {
                    storage_formats: &[(1, naga::StorageFormat::R32Uint)],
                    ..Default::default()
                },
            )
            .unwrap();
            Shader::validate_wgsl(&translation.source).unwrap();
        } else if matches!(
            module.stage,
            ir::Stage::Hull | ir::Stage::Domain | ir::Stage::Geometry
        ) {
            assert!(matches!(
                wgsl::emit(&module, wgsl::Options::default()),
                Err(ir::Error::UnsupportedStage(_))
            ));
        } else {
            let options = match name {
                "buffer_views" => wgsl::Options {
                    storage_formats: &[(0, naga::StorageFormat::R32Uint)],
                    buffer_formats: &[(0, naga::StorageFormat::Rg32Uint)],
                    ..Default::default()
                },
                "packed_buffer" => wgsl::Options {
                    storage_formats: &[(0, naga::StorageFormat::R8Uint)],
                    buffer_formats: &[(0, naga::StorageFormat::R8Uint)],
                    buffer_elements: &[
                        (ir::Register::Resource, 0, 2),
                        (ir::Register::UnorderedAccessView, 0, 2),
                    ],
                    ..Default::default()
                },
                "texture_atomic" => wgsl::Options {
                    storage_formats: &[(1, naga::StorageFormat::R32Uint)],
                    image_atomics: true,
                    ..Default::default()
                },
                "texture_atomic_return" => wgsl::Options {
                    storage_formats: &[(1, naga::StorageFormat::R32Uint)],
                    storage_extents: &[(1, [8, 1, 1])],
                    ..Default::default()
                },
                _ => Default::default(),
            };
            let translation =
                wgsl::emit(&module, options).unwrap_or_else(|error| panic!("{name}: {error:?}"));
            Shader::validate_wgsl(&translation.source)
                .unwrap_or_else(|error| panic!("{name}: {error}"));
        }
    }
}

#[cfg(feature = "gpu")]
#[path = "shader_core/gpu.rs"]
mod gpu;

#[test]
// spec: GFX-REC-4
fn scalar_vertex_position_from_constant_bytes() {
    use ir::Register;
    use ir::interpreter::{Constants, Inputs, NoTextures, run};
    let module = module("vs_5_0");
    let mut inputs = Inputs::default();
    inputs.registers.insert(
        (Register::Input, 0),
        [1.0, 2.0, -3.0, 1.0].map(f32::to_bits),
    );
    inputs.registers.insert(
        (Register::Input, 1),
        [0.75, 0.25, 0.0, 0.0].map(f32::to_bits),
    );
    let constants = Constants::from([(
        0,
        vec![
            [2.0, 3.0, -1.0, 1.0].map(f32::to_bits),
            [-1.0, 1.0, 0.0, 0.0].map(f32::to_bits),
        ],
    )]);
    let outputs = run(&module, &inputs, &constants, &mut NoTextures).unwrap();
    assert_eq!(
        outputs.registers[&(Register::Output, 0)].map(f32::from_bits),
        [1.0, 7.0, 3.0, 1.0]
    );
    assert_eq!(
        outputs.registers[&(Register::Output, 1)].map(f32::from_bits),
        [0.75, 0.25, 0.0, 0.0]
    );
}
