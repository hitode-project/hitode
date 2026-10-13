use super::*;
use adv_device::gpu::*;

fn vertex() -> Shader {
    Shader::parse(&[
        0xfffe0101, 0x1f, 0x80000000, 0x900f0000, 0x1f, 0x8000000a, 0x900f0001, 1, 0xc00f0000,
        0x90e40000, 1, 0xd00f0000, 0x90e40001, 0xffff,
    ])
    .unwrap()
}
#[test]
// spec: GFX-INPUT-1
fn every_declaration_type_is_fetched_by_the_gpu_with_d3d_component_defaults() {
    let backend = match std::env::var("ADV_TEST_GPU_BACKEND").as_deref() {
        Ok("vulkan") => GpuBackend::Vulkan,
        Ok("gles") => GpuBackend::Gl,
        Ok("metal") => GpuBackend::Metal,
        _ => GpuBackend::Automatic,
    };
    let mut gpu = pollster::block_on(Gpu::request_backend(
        matches!(backend, GpuBackend::Automatic) && !cfg!(target_os = "macos"),
        backend,
    ))
    .unwrap();
    let disk_dir = matches!(backend, GpuBackend::Vulkan).then(|| {
        std::env::temp_dir().join(format!(
            "adv-vulkan-pipelines-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    });
    gpu.set_pipeline_cache_directory(disk_dir.as_deref());
    let target = gpu.target(8, 8, false).unwrap();
    let shader = vertex();
    let translated = shader.translate().unwrap();
    let float = |v: &[f32]| bytemuck::cast_slice(v).to_vec();
    let mut cases: Vec<(AttributeFormat, Vec<u8>, [f32; 4])> = vec![
        (AttributeFormat::Float1, float(&[0.25]), [0.25, 0., 0., 1.]),
        (
            AttributeFormat::Float2,
            float(&[0.25, 0.5]),
            [0.25, 0.5, 0., 1.],
        ),
        (
            AttributeFormat::Float3,
            float(&[0.25, 0.5, 0.75]),
            [0.25, 0.5, 0.75, 1.],
        ),
        (
            AttributeFormat::Float4,
            float(&[0.25, 0.5, 0.75, 1.]),
            [0.25, 0.5, 0.75, 1.],
        ),
        (
            AttributeFormat::Bgra8Unorm,
            vec![192, 128, 64, 255],
            [64. / 255., 128. / 255., 192. / 255., 1.],
        ),
        (AttributeFormat::Uint8x4, vec![0, 1, 0, 1], [0., 1., 0., 1.]),
        (
            AttributeFormat::Unorm8x4,
            vec![64, 128, 192, 255],
            [64. / 255., 128. / 255., 192. / 255., 1.],
        ),
        (
            AttributeFormat::Uint10x3,
            (1u32 << 10).to_le_bytes().to_vec(),
            [0., 1., 0., 1.],
        ),
        (
            AttributeFormat::Snorm10x3,
            (256u32 | 511u32 << 10 | 512u32 << 20)
                .to_le_bytes()
                .to_vec(),
            [256. / 511., 1., 0., 1.],
        ),
        (
            AttributeFormat::Float16x2,
            bytemuck::cast_slice(&[0x3400u16, 0x3800]).to_vec(),
            [0.25, 0.5, 0., 1.],
        ),
        (
            AttributeFormat::Float16x4,
            bytemuck::cast_slice(&[0x3400u16, 0x3800, 0x3a00, 0x3c00]).to_vec(),
            [0.25, 0.5, 0.75, 1.],
        ),
    ];
    for (kind, values, expected) in [
        (AttributeFormat::Sint16x2, vec![0i16, 1], [0., 1., 0., 1.]),
        (
            AttributeFormat::Sint16x4,
            vec![0, 1, 0, 1],
            [0., 1., 0., 1.],
        ),
        (
            AttributeFormat::Snorm16x2,
            vec![16384, 32767],
            [16384. / 32767., 1., 0., 1.],
        ),
        (
            AttributeFormat::Snorm16x4,
            vec![16384, 32767, -32768, 32767],
            [16384. / 32767., 1., 0., 1.],
        ),
    ] {
        cases.push((kind, bytemuck::cast_slice(&values).to_vec(), expected));
    }
    for (kind, values, expected) in [
        (
            AttributeFormat::Unorm16x2,
            vec![16384u16, 32768],
            [16384. / 65535., 32768. / 65535., 0., 1.],
        ),
        (
            AttributeFormat::Unorm16x4,
            vec![16384, 32768, 49152, 65535],
            [16384. / 65535., 32768. / 65535., 49152. / 65535., 1.],
        ),
    ] {
        cases.push((kind, bytemuck::cast_slice(&values).to_vec(), expected));
    }
    assert_eq!(cases.len(), 17);
    for (kind, bytes, expected) in cases {
        let inputs = vec![
            Input {
                stream: 0,
                offset: 0,
                format: AttributeFormat::Float4,
                register: 0,
            },
            Input {
                stream: 1,
                offset: 0,
                format: kind,
                register: 1,
            },
        ];
        let source = source::build(
            Some(&shader),
            None,
            Some(&translated),
            None,
            &inputs,
            &[(false, 0)],
        )
        .unwrap();
        let program = Program {
            source: source.into(),
            textures: vec![false],
        };
        validate_program(&program).unwrap_or_else(|e| panic!("{kind:?}: {e}"));
        let state = DrawState::new(8, 8);
        let mut parameters = [[0; 4]; 40];
        parameters[0] = [8f32.to_bits(), 8f32.to_bits(), 0, 0];
        parameters[1][2] = 7;
        let draw = ProgrammableDraw {
            program,
            streams: vec![
                VertexStream {
                    bytes: float(&[-1., 1., 0.5, 1., -1., -3., 0.5, 1., 3., 1., 0.5, 1.]),
                    stride: 16,
                    attributes: vec![Attribute {
                        location: 0,
                        offset: 0,
                        format: AttributeFormat::Float4,
                    }],
                },
                VertexStream {
                    bytes,
                    stride: 0,
                    attributes: vec![Attribute {
                        location: 1,
                        offset: 0,
                        format: kind,
                    }],
                },
            ],
            indices: None,
            vertex_count: 3,
            vertex_constants: ConstantBlock::default(),
            pixel_constants: ConstantBlock::default(),
            textures: vec![ProgramTexture {
                image: None,
                sampler: state.sampler,
            }],
            parameters,
        };
        gpu.draw_programmable(&target, &draw, &state)
            .unwrap_or_else(|e| panic!("{kind:?}: {e}"));
        let mut read = gpu.readback(&target.image, None).unwrap();
        read.take_completion_job().unwrap()();
        let pixels = read.poll().unwrap().unwrap();
        let got = &pixels[(4 * 8 + 4) * 4..(4 * 8 + 4) * 4 + 4];
        let want =
            [expected[2], expected[1], expected[0], expected[3]].map(|x| (x * 255.).round() as u8);
        assert!(
            got.iter().zip(want).all(|(&a, b)| a.abs_diff(b) <= 1),
            "{kind:?}: {got:?} expected {want:?}"
        );
    }
    if let Some(dir) = disk_dir {
        gpu.flush_pipeline_cache();
        let files: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        assert_eq!(
            files.len(),
            1,
            "one adapter cache, without stale temporary files"
        );
        assert!(std::fs::metadata(&files[0]).unwrap().len() > 40);
        let mut reopened =
            pollster::block_on(Gpu::request_backend(false, GpuBackend::Vulkan)).unwrap();
        reopened.set_pipeline_cache_directory(Some(&dir));
        reopened.check().unwrap();
        reopened.set_pipeline_cache_directory(None);
        gpu.set_pipeline_cache_directory(None);
        for file in files {
            std::fs::remove_file(file).unwrap();
        }
        std::fs::remove_dir(dir).unwrap();
    }
}
