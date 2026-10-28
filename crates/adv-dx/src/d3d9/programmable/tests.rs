use super::*;
use adv_cpu::backend::Guest;
use adv_device::gpu::*;
include!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/metal_compare.rs"
));

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
        matches!(backend, GpuBackend::Automatic)
            && !cfg!(any(target_os = "macos", target_os = "android")),
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
            (false, 0, false, 0),
        )
        .unwrap();
        let program = Program::new(source.into(), vec![false]);
        validate_program(&program).unwrap_or_else(|e| panic!("{kind:?}: {e}"));
        let state = DrawState::new(8, 8);
        let mut parameters = [[0; 4]; 40];
        parameters[0] = [8f32.to_bits(), 8f32.to_bits(), 0, 0];
        parameters[1][2] = 7;
        let draw = ProgrammableDraw {
            instance_count: 1,
            program,
            streams: vec![
                VertexStream {
                    instance: false,
                    bytes: float(&[-1., 1., 0.5, 1., -1., -3., 0.5, 1., 3., 1., 0.5, 1.]).into(),
                    stride: 16,
                    attributes: vec![Attribute {
                        location: 0,
                        offset: 0,
                        format: AttributeFormat::Float4,
                    }],
                },
                VertexStream {
                    instance: false,
                    bytes: bytes.into(),
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
            vertex_constants: ConstantBlock::default().into(),
            pixel_constants: ConstantBlock::default().into(),
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
        #[cfg(all(target_vendor = "apple", target_arch = "aarch64"))]
        emit_metal_pixels(&pixels);
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

// spec: GFX-METAL-1
#[cfg(all(target_vendor = "apple", target_arch = "aarch64"))]
#[test]
fn declaration_fixtures_match_native_metal_and_wgpu_exactly() {
    compare_metal_fixture(
        "every_declaration_type_is_fetched_by_the_gpu_with_d3d_component_defaults",
    );
}

// spec: GFX-INPUT-1
#[test]
fn fixed_pixel_stages_combine_a_second_texture_stage() {
    let mut gpu = pollster::block_on(Gpu::request_backend(
        !cfg!(any(target_os = "macos", target_os = "android")),
        GpuBackend::Automatic,
    ))
    .unwrap();
    let target = gpu.target(8, 8, false).unwrap();
    let texel = |bgra: [u8; 4]| {
        let image = gpu.image(1, 1).unwrap();
        let whole = adv_device::surface::Rect {
            x: 0,
            y: 0,
            w: 1,
            h: 1,
        };
        gpu.upload(&image, whole, 4, &bgra).unwrap();
        image
    };
    // Stage zero texel (0.5, 1, 1, 1), stage one (1, 0.5, 0.25, 0.5).
    let images = [texel([255, 255, 128, 255]), texel([64, 128, 255, 128])];
    let source = source::build(
        None,
        None,
        None,
        None,
        &[],
        &[(false, 0), (false, 1)],
        (true, 0, false, 0),
    )
    .unwrap();
    let program = Program::new(source.into(), vec![false, false]);
    validate_program(&program).unwrap();
    let state = DrawState::new(8, 8);
    let mut parameters = [[0; 4]; 40];
    parameters[0] = [8f32.to_bits(), 8f32.to_bits(), 0, 0];
    // Stage zero modulates diffuse by its texel; alpha always passes.
    parameters[1] = [2, 2, 7, 0];
    // Stage one: MODULATE(CURRENT, TEXTURE) for color, SELECTARG1(TEXTURE)
    // for alpha, from coordinate set zero.
    parameters[13] = [4, 1 | 2 << 8, 2 | 2 << 8 | 1 << 16, 0];
    for ordinal in 0..2 {
        parameters[16 + ordinal] = sampler_parameters(state.sampler);
    }
    let mut vertices = Vec::new();
    for position in [[-1f32, 1.], [-1., -3.], [3., 1.]] {
        let diffuse = [1f32, 1., 0.5, 1.];
        vertices.extend([position[0], position[1], 0.5, 1.]);
        vertices.extend(diffuse);
        vertices.extend([0.5, 0.5]);
    }
    let draw = ProgrammableDraw {
        instance_count: 1,
        program,
        streams: vec![VertexStream {
            instance: false,
            bytes: bytemuck::cast_slice(&vertices).to_vec().into(),
            stride: 40,
            attributes: [
                (0, 0, AttributeFormat::Float4),
                (1, 16, AttributeFormat::Float4),
                (2, 32, AttributeFormat::Float2),
            ]
            .map(|(location, offset, format)| Attribute {
                location,
                offset,
                format,
            })
            .to_vec(),
        }],
        indices: None,
        vertex_count: 3,
        vertex_constants: ConstantBlock::default().into(),
        pixel_constants: ConstantBlock::default().into(),
        textures: images
            .map(|image| ProgramTexture {
                image: Some(image),
                sampler: state.sampler,
            })
            .to_vec(),
        parameters,
    };
    gpu.draw_programmable(&target, &draw, &state).unwrap();
    let mut read = gpu.readback(&target.image, None).unwrap();
    read.take_completion_job().unwrap()();
    let pixels = read.poll().unwrap().unwrap();
    let got = &pixels[(4 * 8 + 4) * 4..(4 * 8 + 4) * 4 + 4];
    // Current (0.5, 1, 0.5, 1) times (1, 0.5, 0.25): (0.5, 0.5, 0.125), alpha 0.5.
    let want = [32u8, 128, 128, 128];
    assert!(
        got.iter().zip(want).all(|(&a, b)| a.abs_diff(b) <= 2),
        "{got:?} expected {want:?}"
    );
}

// spec: GFX-FIXED-2, GFX-CACHE-8
#[test]
fn fixed_completion_unlit_pipeline_identity() {
    let key = ProgramKey {
        vertex: None,
        pixel: None,
        inputs: Vec::new(),
        second: false,
        features: (0, false),
        fixed: 0,
    };
    assert_eq!(
        recording::hash(&key),
        recording::hash((key.vertex, key.pixel, &key.inputs, key.second, key.features))
    );
    let source = source::build(
        None,
        None,
        None,
        None,
        &[],
        &[(false, 0)],
        (false, 0, false, 0),
    )
    .unwrap();
    assert_eq!(
        xxhash_rust::xxh3::xxh3_128(source.as_bytes()),
        12730266951233098795822423985951590738
    );
    for fixed in [1, 2, 4, 8, 32] {
        let changed = ProgramKey {
            fixed,
            ..key.clone()
        };
        assert_ne!(recording::hash(&key), recording::hash(&changed));
    }
}

// spec: GFX-INPUT-7
#[test]
fn d3d9_package_d_instanced_offsets_match_two_draws() {
    use crate::d3d9::stretch::tests::Fixture;
    for native in [false, true] {
        let mut f = Fixture::with_native(adv_machine::ObjectLimits::default(), native);
        let output = f.packet(0u32);
        let tokens = f.packet([
            0xfffe0200u32,
            0x0200001f,
            0x80000000,
            0x900f0000,
            0x0200001f,
            0x80000005,
            0x900f0001,
            0x03000002,
            0xc00f0000,
            0x90e40000,
            0x90e40001,
            0x02000001,
            0xd00f0000,
            0xa0e40000,
            0xffff,
        ]);
        f.okay(
            "IDirect3DDevice9::CreateVertexShader",
            &[f.device, tokens, output],
        );
        let shader = f.m.cpu.read_u32(output).unwrap();
        f.okay("IDirect3DDevice9::SetVertexShader", &[f.device, shader]);
        let color = f.packet([1f32, 0.25, 0.5, 1.]);
        f.okay(
            "IDirect3DDevice9::SetVertexShaderConstantF",
            &[f.device, 0, color, 1],
        );
        let elements = f.packet([
            [0u8, 0, 0, 0, 3, 0, 0, 0],
            [1, 0, 0, 0, 3, 0, 5, 0],
            [255, 0, 0, 0, 17, 0, 0, 0],
        ]);
        f.okay(
            "IDirect3DDevice9::CreateVertexDeclaration",
            &[f.device, elements, output],
        );
        let declaration = f.m.cpu.read_u32(output).unwrap();
        f.okay(
            "IDirect3DDevice9::SetVertexDeclaration",
            &[f.device, declaration],
        );
        let positions = [
            [-0.85f32, 0.7, 0.5, 1.],
            [-0.85, -0.7, 0.5, 1.],
            [-0.15, 0.7, 0.5, 1.],
        ];
        let offsets = [[0f32, 0., 0., 0.], [1., 0., 0., 0.]];
        for (n, bytes) in [
            bytemuck::cast_slice(&positions),
            bytemuck::cast_slice(&offsets),
        ]
        .into_iter()
        .enumerate()
        {
            f.okay(
                "IDirect3DDevice9::CreateVertexBuffer",
                &[f.device, bytes.len() as u32, 0, 0, 1, output, 0],
            );
            let buffer = f.m.cpu.read_u32(output).unwrap();
            f.okay("IDirect3DVertexBuffer9::Lock", &[buffer, 0, 0, output, 0]);
            f.m.cpu
                .write(f.m.cpu.read_u32(output).unwrap(), bytes)
                .unwrap();
            f.okay("IDirect3DVertexBuffer9::Unlock", &[buffer]);
            f.okay(
                "IDirect3DDevice9::SetStreamSource",
                &[f.device, n as u32, buffer, 0, 16],
            );
        }
        f.okay(
            "IDirect3DDevice9::CreateIndexBuffer",
            &[f.device, 6, 0, 101, 1, output, 0],
        );
        let index = f.m.cpu.read_u32(output).unwrap();
        f.okay("IDirect3DIndexBuffer9::Lock", &[index, 0, 0, output, 0]);
        f.m.cpu
            .write(
                f.m.cpu.read_u32(output).unwrap(),
                bytemuck::cast_slice(&[0u16, 1, 2]),
            )
            .unwrap();
        f.okay("IDirect3DIndexBuffer9::Unlock", &[index]);
        f.okay("IDirect3DDevice9::SetIndices", &[f.device, index]);
        for (state, value) in [(7, 0), (22, 1)] {
            f.okay(
                "IDirect3DDevice9::SetRenderState",
                &[f.device, state, value],
            );
        }
        f.okay("IDirect3DDevice9::BeginScene", &[f.device]);
        f.okay(
            "IDirect3DDevice9::Clear",
            &[f.device, 0, 0, 1, 0xff000000, 1f32.to_bits(), 0],
        );
        f.okay(
            "IDirect3DDevice9::SetStreamSourceFreq",
            &[f.device, 0, buffers::INDEXED_DATA | 2],
        );
        f.okay(
            "IDirect3DDevice9::SetStreamSourceFreq",
            &[f.device, 1, buffers::INSTANCE_DATA | 1],
        );
        let before = f.m.ext.get::<State>().gpu.as_ref().unwrap().stats().draws;
        f.okay(
            "IDirect3DDevice9::DrawIndexedPrimitive",
            &[f.device, 4, 0, 0, 3, 0, 1],
        );
        let image = f.image(f.back);
        let actual = f.pixels(&image);
        assert_eq!(
            f.m.ext.get::<State>().gpu.as_ref().unwrap().stats().draws - before,
            1
        );
        f.okay(
            "IDirect3DDevice9::Clear",
            &[f.device, 0, 0, 1, 0xff000000, 1f32.to_bits(), 0],
        );
        for stream in [0, 1] {
            f.okay(
                "IDirect3DDevice9::SetStreamSourceFreq",
                &[f.device, stream, 1],
            );
        }
        let e = Env {
            context: f.m.sys.current,
            cpu: &mut f.m.cpu,
            sys: &mut f.m.sys,
            ext: &mut f.m.ext,
            thunk: 0,
        };
        let binding = device(&e, f.device).unwrap().buffers.streams[1];
        let Some(Payload::Buffer(buffer)) =
            com(&e).get_identity(&e.sys.objects, binding.buffer.unwrap())
        else {
            panic!()
        };
        let pointer = buffer.pointer;
        for offset in [0, 16] {
            f.okay(
                "IDirect3DDevice9::SetStreamSource",
                &[f.device, 1, pointer, offset, 0],
            );
            f.okay(
                "IDirect3DDevice9::DrawIndexedPrimitive",
                &[f.device, 4, 0, 0, 3, 0, 1],
            );
        }
        assert_eq!(actual, f.pixels(&image));
        assert!(
            actual
                .as_chunks::<4>()
                .0
                .iter()
                .enumerate()
                .any(|(n, p)| n % 16 < 8 && p[2] > 200)
        );
        assert!(
            actual
                .as_chunks::<4>()
                .0
                .iter()
                .enumerate()
                .any(|(n, p)| n % 16 >= 8 && p[2] > 200)
        );
        f.okay(
            "IDirect3DDevice9::Clear",
            &[f.device, 0, 0, 1, 0xff000000, 1f32.to_bits(), 0],
        );
        f.okay(
            "IDirect3DDevice9::SetStreamSource",
            &[f.device, 1, pointer, 0, 0],
        );
        f.okay(
            "IDirect3DDevice9::SetStreamSourceFreq",
            &[f.device, 0, buffers::INDEXED_DATA | 2],
        );
        f.okay(
            "IDirect3DDevice9::SetStreamSourceFreq",
            &[f.device, 1, buffers::INSTANCE_DATA | 1],
        );
        f.okay("IDirect3DDevice9::DrawPrimitive", &[f.device, 4, 0, 1]);
        let ignored = f.pixels(&image);
        f.okay(
            "IDirect3DDevice9::Clear",
            &[f.device, 0, 0, 1, 0xff000000, 1f32.to_bits(), 0],
        );
        for stream in [0, 1] {
            f.okay(
                "IDirect3DDevice9::SetStreamSourceFreq",
                &[f.device, stream, 1],
            );
        }
        f.okay("IDirect3DDevice9::DrawPrimitive", &[f.device, 4, 0, 1]);
        assert_eq!(ignored, f.pixels(&image));
        f.okay("IDirect3DDevice9::EndScene", &[f.device]);
    }
}

// spec: GFX-INPUT-7
#[test]
fn d3d9_package_d_instancing_fixed_and_fvf_match_pixels() {
    for programmable in [false, true] {
        for fvf in [false, true] {
            let mut f = crate::d3d9::stretch::tests::Fixture::new();
            let output = f.packet(0u32);
            if programmable {
                let tokens = f.packet([
                    0xfffe0101u32,
                    0x1f,
                    0x80000000,
                    0x900f0000,
                    0x1f,
                    0x8000000a,
                    0x900f0001,
                    1,
                    0xc00f0000,
                    0x90e40000,
                    1,
                    0xd00f0000,
                    0x90e40001,
                    0xffff,
                ]);
                f.okay(
                    "IDirect3DDevice9::CreateVertexShader",
                    &[f.device, tokens, output],
                );
                let shader = f.m.cpu.read_u32(output).unwrap();
                f.okay("IDirect3DDevice9::SetVertexShader", &[f.device, shader]);
            }
            let positions = [[-0.8f32, 0.8, 0.5], [-0.8, -0.8, 0.5], [0.8, 0.8, 0.5]];
            let vertices: Vec<u32> = positions
                .into_iter()
                .flat_map(|p| [p[0].to_bits(), p[1].to_bits(), p[2].to_bits(), 0xff00ff00])
                .collect();
            let colors = [0xffff0000u32, 0xff00ff00];
            for (stream, bytes, stride) in [
                (0, bytemuck::cast_slice(&vertices), 16),
                (1, bytemuck::cast_slice(&colors), 4),
            ] {
                f.okay(
                    "IDirect3DDevice9::CreateVertexBuffer",
                    &[f.device, bytes.len() as u32, 0, 0, 1, output, 0],
                );
                let buffer = f.m.cpu.read_u32(output).unwrap();
                f.okay("IDirect3DVertexBuffer9::Lock", &[buffer, 0, 0, output, 0]);
                f.m.cpu
                    .write(f.m.cpu.read_u32(output).unwrap(), bytes)
                    .unwrap();
                f.okay("IDirect3DVertexBuffer9::Unlock", &[buffer]);
                f.okay(
                    "IDirect3DDevice9::SetStreamSource",
                    &[f.device, stream, buffer, 0, stride],
                );
            }
            if fvf {
                f.okay("IDirect3DDevice9::SetFVF", &[f.device, 0x42]);
            } else {
                let elements = f.packet([
                    [0u8, 0, 0, 0, 2, 0, 0, 0],
                    [1, 0, 0, 0, 4, 0, 10, 0],
                    [255, 0, 0, 0, 17, 0, 0, 0],
                ]);
                f.okay(
                    "IDirect3DDevice9::CreateVertexDeclaration",
                    &[f.device, elements, output],
                );
                let declaration = f.m.cpu.read_u32(output).unwrap();
                f.okay(
                    "IDirect3DDevice9::SetVertexDeclaration",
                    &[f.device, declaration],
                );
            }
            f.okay(
                "IDirect3DDevice9::CreateIndexBuffer",
                &[f.device, 6, 0, 101, 1, output, 0],
            );
            let index = f.m.cpu.read_u32(output).unwrap();
            f.okay("IDirect3DIndexBuffer9::Lock", &[index, 0, 0, output, 0]);
            f.m.cpu
                .write(
                    f.m.cpu.read_u32(output).unwrap(),
                    bytemuck::cast_slice(&[0u16, 1, 2]),
                )
                .unwrap();
            f.okay("IDirect3DIndexBuffer9::Unlock", &[index]);
            f.okay("IDirect3DDevice9::SetIndices", &[f.device, index]);
            for (state, value) in [(7, 0), (22, 1), (137, 0)] {
                f.okay(
                    "IDirect3DDevice9::SetRenderState",
                    &[f.device, state, value],
                );
            }
            f.okay("IDirect3DDevice9::BeginScene", &[f.device]);
            f.okay(
                "IDirect3DDevice9::Clear",
                &[f.device, 0, 0, 1, 0xff000000, 1f32.to_bits(), 0],
            );
            f.okay(
                "IDirect3DDevice9::SetStreamSourceFreq",
                &[f.device, 0, buffers::INDEXED_DATA | 2],
            );
            f.okay(
                "IDirect3DDevice9::SetStreamSourceFreq",
                &[f.device, 1, buffers::INSTANCE_DATA | 1],
            );
            let before = f.m.ext.get::<State>().gpu.as_ref().unwrap().stats().draws;
            f.okay(
                "IDirect3DDevice9::DrawIndexedPrimitive",
                &[f.device, 4, 0, 0, 3, 0, 1],
            );
            let image = f.image(f.back);
            let actual = f.pixels(&image);
            assert_eq!(
                f.m.ext.get::<State>().gpu.as_ref().unwrap().stats().draws - before,
                1
            );
            f.okay(
                "IDirect3DDevice9::Clear",
                &[f.device, 0, 0, 1, 0xff000000, 1f32.to_bits(), 0],
            );
            for stream in [0, 1] {
                f.okay(
                    "IDirect3DDevice9::SetStreamSourceFreq",
                    &[f.device, stream, 1],
                );
            }
            f.okay("IDirect3DDevice9::SetFVF", &[f.device, 0x42]);
            let data = f.packet(<[u32; 12]>::try_from(vertices).unwrap());
            f.okay(
                "IDirect3DDevice9::DrawPrimitiveUP",
                &[f.device, 4, 1, data, 16],
            );
            assert_eq!(
                actual,
                f.pixels(&image),
                "programmable={programmable}, fvf={fvf}"
            );
            assert!(actual.as_chunks::<4>().0.iter().any(|p| p[1] > 200));
            f.okay("IDirect3DDevice9::EndScene", &[f.device]);
        }
    }
}
