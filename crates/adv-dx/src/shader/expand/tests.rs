use super::*;
use crate::shader::analysis::analyze;
use crate::shader::ir::{
    Components, Declaration, Index, Instruction, Model, Modifier, Operand, OperandValue, Precision,
    Register, SignatureElement, Signatures, SystemValue, ValueType,
};
use crate::shader::variant::DrawFacts;
use crate::shader::wgsl;
use adv_device::gpu::{DrawArguments, ModernColor, ShaderStage, test_support};
use adv_gpu::encode::{Load, Store};
use adv_gpu::pipeline::{self, Primitive};
use adv_gpu::resource::{BufferDesc, BufferUsage, Dimension, ImageDesc, Memory, Usage, ViewDesc};
use adv_gpu::{Buffer, Extent, Features, Format, Image, Rect};
use std::time::{Duration, Instant};

fn module(
    stage: Stage,
    input: Vec<SignatureElement>,
    output: Vec<SignatureElement>,
    body: Vec<Statement>,
) -> Module {
    Module {
        stage,
        model: Model::Sm50,
        signatures: Signatures {
            input,
            output,
            patch_constant: Vec::new(),
        },
        body,
        feature_flags: 0,
        statistics: Vec::new(),
        reflection: None,
    }
}

fn signature(
    value: SystemValue,
    register: u32,
    mask: u8,
    ty: ValueType,
    name: &str,
) -> SignatureElement {
    SignatureElement {
        semantic: name.into(),
        semantic_index: 0,
        system_value: value,
        value_type: ty,
        register,
        mask,
        used_mask: mask,
        stream: 0,
        min_precision: 0,
    }
}

fn operand(reg: Register, indices: Vec<Index>, components: Components) -> Operand {
    Operand {
        value: OperandValue::Register {
            register: reg,
            indices,
        },
        components,
        modifier: Modifier::None,
        precision: Precision::Default,
        non_uniform: false,
    }
}

fn immediate(data: Vector) -> Operand {
    Operand {
        value: OperandValue::Immediate(data),
        components: Components::Mask(15),
        modifier: Modifier::None,
        precision: Precision::Default,
        non_uniform: false,
    }
}

fn instruction(opcode: Opcode, destinations: Vec<Operand>, sources: Vec<Operand>) -> Statement {
    Statement::Execute(Instruction {
        opcode,
        destinations,
        sources,
        saturate: false,
        test_nonzero: false,
        precise: 0,
        controls: 0,
        resource: Default::default(),
        interface_function: None,
        flush_inputs: false,
        flush_outputs: false,
    })
}

fn mov(register: u64, source: Operand) -> Statement {
    instruction(
        Opcode::Mov,
        vec![operand(
            Register::Output,
            vec![Index::Direct(register)],
            Components::Mask(15),
        )],
        vec![source],
    )
}

fn declaration(opcode: Opcode, controls: u32, data: Vec<u32>) -> Statement {
    Statement::Declare(Declaration {
        opcode,
        operand: None,
        controls,
        data,
        binding: None,
        interpolation: None,
    })
}

fn position() -> SignatureElement {
    signature(
        SystemValue::Position,
        0,
        15,
        ValueType::Float,
        "SV_Position",
    )
}
fn varying() -> SignatureElement {
    signature(SystemValue::None, 1, 3, ValueType::Float, "TEXCOORD")
}

fn vertex(x0: f32, x1: f32, z: f32) -> Module {
    let id = operand(
        Register::Input,
        vec![Index::Direct(0)],
        Components::Select(0),
    );
    module(
        Stage::Vertex,
        vec![signature(
            SystemValue::VertexId,
            0,
            1,
            ValueType::Uint,
            "SV_VertexID",
        )],
        vec![position(), varying()],
        vec![
            Statement::ImmediateConstants(vec![
                [x0, -1.0, z, 1.0].map(f32::to_bits),
                [x0, 1.0, z, 1.0].map(f32::to_bits),
                [x1, -1.0, z, 1.0].map(f32::to_bits),
                [x1, 1.0, z, 1.0].map(f32::to_bits),
            ]),
            mov(
                0,
                operand(
                    Register::ImmediateConstantBuffer,
                    vec![Index::Relative {
                        base: 0,
                        register: Box::new(id),
                    }],
                    Components::Mask(15),
                ),
            ),
            mov(1, immediate([0.25, 1.0, 0.0, 1.0].map(f32::to_bits))),
        ],
    )
}

fn fragment() -> Module {
    module(
        Stage::Pixel,
        vec![varying()],
        vec![signature(
            SystemValue::Target,
            0,
            15,
            ValueType::Float,
            "SV_Target",
        )],
        vec![mov(
            0,
            operand(
                Register::Input,
                vec![Index::Direct(1)],
                Components::Swizzle([0, 1, 0, 1]),
            ),
        )],
    )
}

fn geometry() -> Module {
    let mut body = vec![
        declaration(
            Opcode::DclGsOutputPrimitiveTopology,
            encode_gs_output_primitive_topology(D3D10_SB_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP),
            vec![],
        ),
        declaration(Opcode::DclMaxOutputVertexCount, 0, vec![8]),
        declaration(Opcode::DclGsInstanceCount, 0, vec![2]),
    ];
    for _ in 0..2 {
        for vertex in [0, 1, 2, 0] {
            body.push(mov(
                0,
                operand(
                    Register::Input,
                    vec![Index::Direct(vertex), Index::Direct(0)],
                    Components::Mask(15),
                ),
            ));
            body.push(mov(
                1,
                operand(Register::InputGsInstanceId, vec![], Components::Select(0)),
            ));
            body.push(instruction(Opcode::Emit, vec![], vec![]));
        }
        body.push(instruction(Opcode::Cut, vec![], vec![]));
    }
    module(
        Stage::Geometry,
        vec![position()],
        vec![position(), varying()],
        body,
    )
}

fn gpu() -> Gpu {
    test_support::request(!cfg!(any(target_vendor = "apple", target_os = "android"))).unwrap()
}

fn buffer(gpu: &Gpu, words: &[u32], usage: BufferUsage, memory: Memory) -> BufferRange {
    let size = std::mem::size_of_val(words) as u64;
    let buffer = gpu
        .device()
        .create_buffer(&BufferDesc {
            size,
            usage,
            memory,
            label: "expansion test",
        })
        .unwrap();
    // No work referencing the buffer has been submitted.
    unsafe { &mut *buffer.mapped().unwrap().as_ptr() }.copy_from_slice(bytemuck::cast_slice(words));
    BufferRange {
        buffer,
        range: 0..size,
    }
}

fn upload(gpu: &Gpu, words: &[u32]) -> BufferRange {
    buffer(gpu, words, BufferUsage::STORAGE, Memory::Upload)
}

fn words(buffer: &Buffer) -> Vec<u32> {
    // The final readback already completed the submission.
    bytemuck::cast_slice(unsafe { buffer.mapped().unwrap().as_ref() }).to_vec()
}

fn image(gpu: &Gpu, samples: u32) -> Image {
    gpu.device()
        .create_image(&ImageDesc {
            dimension: Dimension::D2,
            size: Extent {
                width: 64,
                height: 64,
                depth_or_layers: 1,
            },
            format: Format::Rgba8Unorm,
            mips: 1,
            samples,
            usage: Usage::COLOR | Usage::COPY_SRC,
            view_formats: &[],
            label: "expansion oracle",
        })
        .unwrap()
}

fn wait(gpu: &mut Gpu) {
    let fence = gpu.target(1, 1, false).unwrap();
    gpu.clear(&fence, Some([0.; 4]), None).unwrap();
    let mut pending = gpu.readback(&fence.image, None).unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(result) = pending.poll() {
            result.unwrap();
            break;
        }
        gpu.check().unwrap();
        assert!(Instant::now() < deadline, "GPU readback timed out");
        std::thread::sleep(Duration::from_millis(1));
    }
    gpu.check().unwrap();
}

fn pixels(gpu: &mut Gpu, image: &Image) -> Vec<u32> {
    layer_pixels(gpu, image, 0)
}

fn layer_pixels(gpu: &mut Gpu, image: &Image, layer: u32) -> Vec<u32> {
    let read = buffer(gpu, &[0; 4096], BufferUsage::COPY_DST, Memory::Readback);
    gpu.copy_image_to_buffer(image, 0, layer, read.buffer.clone(), 256)
        .unwrap();
    wait(gpu);
    words(&read.buffer)
}

fn shader(translation: wgsl::Translation, stage: ShaderStage) -> Arc<ModernShader> {
    Arc::new(
        ModernShader::wgsl(
            xxhash_rust::xxh3::xxh3_64(translation.source.as_bytes()),
            &translation.source,
            translation.entry_point,
            stage,
        )
        .unwrap_or_else(|e| panic!("{e}\n{}", translation.source)),
    )
}

fn draw(image: &Image, vertex: Arc<ModernShader>, fragment: Arc<ModernShader>) -> ModernDraw {
    let area = Rect {
        x: 0,
        y: 0,
        w: 64,
        h: 64,
    };
    ModernDraw {
        color_slots: None,
        state_ids: [0; 4],
        vertex,
        fragment: Some(fragment),
        colors: [ModernColor {
            view: image.view().clone(),
            blend: None,
            write_mask: 15,
            load: Load::Clear([0., 0., 0., 0.]),
            store: Store::Store,
        }]
        .into_iter()
        .collect(),
        depth: None,
        samples: image.desc().samples,
        alpha_to_coverage: false,
        primitive: Primitive {
            topology: pipeline::Topology::TriangleStrip,
            cull: pipeline::Cull::None,
            front_ccw: true,
            wireframe: false,
            unclipped_depth: false,
        },
        area,
        viewport: [0., 0., 64., 64., 0., 1.],
        scissor: area,
        stencil_reference: 0,
        blend_constant: [0.; 4],
        depth_bias: [0.; 3],
        arguments: DrawArguments::Direct {
            vertices: 0..4,
            instances: 0..1,
            base_vertex: 0,
        },
        index: None,
        occlusion: None,
        predicate: None,
    }
}

fn inputs(gpu: &Gpu, index: &[u32]) -> Inputs {
    Inputs {
        primitives: index.len() as u32 / 3,
        vertices: Some(upload(
            gpu,
            &[0, 0, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0],
        )),
        indices: Some(upload(gpu, index)),
        vertex: SlotBindings::default(),
        geometry: SlotBindings::default(),
        hull: SlotBindings::default(),
        domain: SlotBindings::default(),
        indirect: None,
        stream_statistics: None,
        statistics: None,
        stream_output: Vec::new(),
    }
}

fn expected(name: &str) -> Vec<(u64, u64)> {
    include_str!("pipestats.tsv")
        .lines()
        .find_map(|line| line.strip_prefix(&format!("{name}\t")))
        .unwrap()
        .split('\t')
        .map(|entry| {
            let (lo, hi) = entry.split_once("..").unwrap_or((entry, entry));
            (lo.parse().unwrap(), hi.parse().unwrap())
        })
        .collect()
}

fn check_stats(name: &str, stats: statistics::PipelineStatistics) {
    let actual = [
        stats.ia_vertices,
        stats.ia_primitives,
        stats.vs_invocations,
        stats.gs_invocations,
        stats.gs_primitives,
        stats.c_invocations,
        stats.c_primitives,
        stats.ps_invocations,
        stats.hs_invocations,
        stats.ds_invocations,
        stats.cs_invocations,
    ];
    for (i, (observed, (lo, hi))) in actual.into_iter().zip(expected(name)).enumerate() {
        assert!(
            (lo..=hi).contains(&observed),
            "{name} counter {i}: {observed} outside {lo}..={hi}"
        );
    }
}

// spec: GFX-SHADER-6 GFX-SHADER-7
#[test]
fn native_vertex_and_expansion_render_identical_pixels() {
    let mut gpu = gpu();
    let mut caps = gpu.device().caps().clone();
    caps.features.remove(Features::PIPELINE_STATISTICS_QUERY);
    let ps = fragment();
    let ps_reflection = analyze(&ps);
    let ordinary_ps = Variant::select(&ps_reflection, &DrawFacts::default(), &caps);
    let ordinary_ps = shader(
        wgsl::emit_variant(&ps, &ordinary_ps, None, Options::default()).unwrap(),
        ShaderStage::Fragment,
    );
    let stats_facts = DrawFacts {
        statistics: true,
        ..Default::default()
    };
    let counting = Variant::select(&ps_reflection, &stats_facts, &caps);
    let counting = shader(
        wgsl::emit_variant(&ps, &counting, None, Options::default()).unwrap(),
        ShaderStage::Fragment,
    );
    let reference = image(&gpu, 1);
    let expanded = image(&gpu, 1);
    for (name, x0, x1, depth) in [
        ("full", -1.0, 1.0, 0.5),
        ("half", -2.0, 0.0, 0.5),
        ("offscreen", 2.0, 3.0, 0.5),
        ("far", -1.0, 1.0, 2.0),
    ] {
        let vs = vertex(x0, x1, depth);
        let reflection = analyze(&vs);
        let ordinary = DrawFacts {
            fragment: Some((1, &ps_reflection)),
            ..Default::default()
        };
        let variant = Variant::select(&reflection, &ordinary, &caps);
        let native = shader(
            wgsl::emit_variant(&vs, &variant, Some(&ps_reflection), Options::default()).unwrap(),
            ShaderStage::Vertex,
        );
        gpu.draw_modern(
            draw(&reference, native, ordinary_ps.clone()),
            &SlotBindings::default(),
            &SlotBindings::default(),
        )
        .unwrap();

        let expanded_facts = DrawFacts {
            statistics: true,
            ..ordinary
        };
        let variant = Variant::select(&reflection, &expanded_facts, &caps);
        let plan = Plan {
            source: Source::Vertex(Shader {
                ir: &vs,
                variant: &variant,
                options: Options::default(),
            }),
            topology: Topology::Triangle,
            input_vertices: 3,
            geometry: None,
            raster: Some(Raster {
                bins: &[&variant],
                fragment: Some(&ps_reflection),
                stream: 0,
                cull: Cull::None,
                front_ccw: true,
                depth_clip: true,
            }),
            stream_output: &[],
            stream_statistics: false,
        }
        .emit()
        .unwrap();
        let counters = buffer(
            &gpu,
            &[0; statistics::GPU_WORDS],
            BufferUsage::STORAGE,
            Memory::Readback,
        );
        let mut inputs = inputs(&gpu, &[0, 1, 2, 1, 3, 2]);
        inputs.statistics = Some(counters.clone());
        let bins = vec![BinDraw {
            draw: draw(&expanded, plan.vertex_shader(0).unwrap(), counting.clone()),
            fragment: SlotBindings::default(),
            statistics: None,
        }];
        let command = plan.command(&gpu, inputs, bins).unwrap();
        gpu.expand(command).unwrap();
        assert_eq!(
            pixels(&mut gpu, &expanded),
            pixels(&mut gpu, &reference),
            "{name}"
        );
        let mut stats = statistics::PipelineStatistics::default();
        stats.record_draw(
            statistics::Assembly {
                vertices: 4,
                primitives: 2,
                instances: 1,
            },
            None,
            None,
            statistics::Rasterization::Enabled,
        );
        stats.record_gpu(words(&counters.buffer).try_into().unwrap(), false);
        check_stats(name, stats);
    }
}

// spec: GFX-SHADER-6 GFX-SHADER-7
#[test]
fn geometry_cuts_instances_and_stream_output_keep_primitive_order() {
    let mut gpu = gpu();
    let caps = gpu.device().caps();
    let vs = vertex(-1.0, 1.0, 0.5);
    let gs = geometry();
    let facts = DrawFacts {
        statistics: true,
        geometry: true,
        stream_output: true,
        rasterization_disabled: true,
        ..Default::default()
    };
    let source = Variant::select(&analyze(&vs), &facts, caps);
    let geometry = Variant::select(&analyze(&gs), &facts, caps);
    let plan = Plan {
        source: Source::Vertex(Shader {
            ir: &vs,
            variant: &source,
            options: Options::default(),
        }),
        topology: Topology::Triangle,
        input_vertices: 3,
        geometry: Some(Shader {
            ir: &gs,
            variant: &geometry,
            options: Options::default(),
        }),
        raster: None,
        stream_statistics: false,
        stream_output: &[StreamOutput {
            stream: 0,
            elements: vec![Some((0, 0)), Some((0, 1)), Some((1, 0)), None],
        }],
    }
    .emit()
    .unwrap();
    let target = buffer(
        &gpu,
        &[0xcccc_cccc; 104],
        BufferUsage::STORAGE,
        Memory::Readback,
    );
    let filled = buffer(&gpu, &[16], BufferUsage::STORAGE, Memory::Readback);
    let stats = buffer(
        &gpu,
        &[0; statistics::GPU_WORDS],
        BufferUsage::STORAGE,
        Memory::Readback,
    );
    let mut inputs = inputs(&gpu, &[0, 1, 2]);
    inputs.stream_output = vec![(target.clone(), filled.clone())];
    inputs.statistics = Some(stats.clone());
    let command = plan.command(&gpu, inputs, Vec::new()).unwrap();
    gpu.expand(command).unwrap();
    wait(&mut gpu);
    assert_eq!(words(&filled.buffer), [16 + 24 * 16]);
    let actual = words(&target.buffer);
    let positions = [[-1.0f32, -1.0], [-1.0, 1.0], [1.0, -1.0]];
    let mut expected = vec![0xcccc_cccc; 4];
    for instance in 0..2 {
        for _ in 0..2 {
            for vertex in [0, 1, 2, 1, 0, 2] {
                let [x, y] = positions[vertex].map(f32::to_bits);
                expected.extend([x, y, instance, 0xcccc_cccc]);
            }
        }
    }
    expected.extend([0xcccc_cccc; 4]);
    assert_eq!(actual, expected);
    assert_eq!(words(&stats.buffer), [0, 0, 8, 0, 0, 0, 0, 0]);
}

#[cfg(not(target_vendor = "apple"))]
#[test]
fn native_query_counts_only_raster() {
    let mut gpu = gpu();
    let caps = gpu.device().caps();
    if !caps.features.contains(Features::PIPELINE_STATISTICS_QUERY) {
        return;
    }
    let set = Arc::new(
        gpu.device()
            .create_query_set(adv_gpu::queue::QueryKind::PipelineStatistics, 1)
            .unwrap(),
    );
    let result = buffer(&gpu, &[0; 4], BufferUsage::COPY_DST, Memory::Readback);
    let vs = vertex(-1.0, 1.0, 0.5);
    let ps = fragment();
    let ps_reflection = analyze(&ps);
    let facts = DrawFacts {
        fragment: Some((1, &ps_reflection)),
        statistics: true,
        ..Default::default()
    };
    let vs_variant = Variant::select(&analyze(&vs), &facts, caps);
    let ps_variant = Variant::select(&ps_reflection, &facts, caps);
    assert!(!vs_variant.pulled());
    let vertex = shader(
        wgsl::emit_variant(&vs, &vs_variant, Some(&ps_reflection), Options::default()).unwrap(),
        ShaderStage::Vertex,
    );
    let fragment = shader(
        wgsl::emit_variant(&ps, &ps_variant, None, Options::default()).unwrap(),
        ShaderStage::Fragment,
    );
    let color = image(&gpu, 1);
    let slot = QuerySlot { set, index: 0 };
    gpu.draw_modern_statistics(
        draw(&color, vertex, fragment),
        &SlotBindings::default(),
        &SlotBindings::default(),
        slot.clone(),
    )
    .unwrap();
    gpu.read_query(slot, result.buffer.clone(), 0).unwrap();
    wait(&mut gpu);
    let result = words(&result.buffer);
    assert_eq!(result[0], 2);
    assert!(result[2] >= 4096);
    assert_eq!([result[1], result[3]], [0, 0]);
}

#[test]
fn packed_and_storage_varyings_match() {
    let mut gpu = gpu();
    let mut caps = gpu.device().caps().clone();
    let mut vs = vertex(-1.0, 1.0, 0.5);
    let mut ps = fragment();
    for register in 2..31 {
        let name = format!("TEXCOORD{register}");
        let element = signature(SystemValue::None, register, 15, ValueType::Float, &name);
        vs.signatures.output.push(element.clone());
        ps.signatures.input.push(element);
        vs.body
            .push(mov(u64::from(register), immediate([0.75f32.to_bits(); 4])));
    }
    // Read a high numbered input so the packed and storage forms both transport it.
    ps.body[0] = mov(
        0,
        operand(
            Register::Input,
            vec![Index::Direct(30)],
            Components::Mask(15),
        ),
    );
    let vertex_reflection = analyze(&vs);
    let pixel_reflection = analyze(&ps);
    let facts = DrawFacts {
        fragment: Some((2, &pixel_reflection)),
        producer_identity: Some(1),
        ..Default::default()
    };
    caps.limits.max_fragment_input_components = 128;
    let vertex_native = Variant::select(&vertex_reflection, &facts, &caps);
    let pixel_native = Variant::select(&pixel_reflection, &facts, &caps);
    assert!(!vertex_native.pulled());
    let reference = image(&gpu, 1);
    gpu.draw_modern(
        draw(
            &reference,
            shader(
                wgsl::emit_variant(
                    &vs,
                    &vertex_native,
                    Some(&pixel_reflection),
                    Options::default(),
                )
                .unwrap(),
                ShaderStage::Vertex,
            ),
            shader(
                wgsl::emit_variant(&ps, &pixel_native, None, Options::default()).unwrap(),
                ShaderStage::Fragment,
            ),
        ),
        &SlotBindings::default(),
        &SlotBindings::default(),
    )
    .unwrap();

    caps.limits.max_fragment_input_components = 112;
    let vs_variant = Variant::select(&vertex_reflection, &facts, &caps);
    let ps_variant = Variant::select(&pixel_reflection, &facts, &caps);
    assert!(vs_variant.pulled());
    assert!(ps_variant.storage_varyings());
    let plan = Plan {
        source: Source::Vertex(Shader {
            ir: &vs,
            variant: &vs_variant,
            options: Options::default(),
        }),
        topology: Topology::Triangle,
        input_vertices: 3,
        geometry: None,
        raster: Some(Raster {
            bins: &[&vs_variant],
            fragment: Some(&pixel_reflection),
            stream: 0,
            cull: Cull::None,
            front_ccw: true,
            depth_clip: true,
        }),
        stream_output: &[],
        stream_statistics: false,
    }
    .emit()
    .unwrap();
    let expanded = image(&gpu, 1);
    let draw = draw(
        &expanded,
        plan.vertex_shader(0).unwrap(),
        plan.fragment_shader(&ps, &ps_variant, &vertex_reflection, 2, Options::default())
            .unwrap(),
    );
    let command = plan
        .command(
            &gpu,
            inputs(&gpu, &[0, 1, 2, 1, 3, 2]),
            vec![BinDraw {
                draw,
                fragment: SlotBindings::default(),
                statistics: None,
            }],
        )
        .unwrap();
    gpu.expand(command).unwrap();
    let expanded = pixels(&mut gpu, &expanded);
    assert!(expanded.iter().all(|&pixel| pixel != 0));
    assert_eq!(expanded, pixels(&mut gpu, &reference));
}

#[test]
fn domain_points_are_shaded_once_and_streamed_as_a_list() {
    use tessellator::SurfaceOutput;
    let mut gpu = gpu();
    let caps = gpu.device().caps();
    let module = module(
        Stage::Domain,
        vec![],
        vec![varying()],
        vec![mov(
            1,
            operand(Register::InputDomainPoint, vec![], Components::Mask(15)),
        )],
    );
    let facts = DrawFacts {
        tessellated: true,
        stream_output: true,
        rasterization_disabled: true,
        ..Default::default()
    };
    let variant = Variant::select(&analyze(&module), &facts, caps);
    let domain = Domain::Tri {
        edges: [2.; 3],
        inside: 2.,
        output: SurfaceOutput::TriangleCcw,
    };
    let (vertices, indices) =
        Source::domain_inputs(domain, Partitioning::Integer, 0, &[[0; 4]], &[]);
    let plan = Plan {
        source: Source::Domain {
            shader: Shader {
                ir: &module,
                variant: &variant,
                options: Options::default(),
            },
            control_points: 1,
            control_registers: 1,
            patch_registers: 0,
        },
        topology: Topology::Triangle,
        input_vertices: 3,
        geometry: None,
        raster: None,
        stream_statistics: false,
        stream_output: &[StreamOutput {
            stream: 0,
            elements: vec![Some((1, 0)), Some((1, 1)), Some((1, 2))],
        }],
    }
    .emit()
    .unwrap();

    let mesh = tessellator::tessellate(domain, Partitioning::Integer);
    assert_eq!(vertices.len() / 3, mesh.points.len());
    let expected: Vec<_> = mesh
        .indices
        .iter()
        .flat_map(|&index| mesh.points[index as usize].map(f32::to_bits))
        .collect();
    let target = buffer(
        &gpu,
        &vec![0; expected.len()],
        BufferUsage::STORAGE,
        Memory::Readback,
    );
    let filled = buffer(&gpu, &[0], BufferUsage::STORAGE, Memory::Readback);
    let input = Inputs {
        primitives: indices.len() as u32 / 3,
        vertices: Some(upload(&gpu, bytemuck::cast_slice(&vertices))),
        indices: Some(upload(&gpu, &indices)),
        vertex: SlotBindings::default(),
        geometry: SlotBindings::default(),
        hull: SlotBindings::default(),
        domain: SlotBindings::default(),
        indirect: None,
        stream_statistics: None,
        statistics: None,
        stream_output: vec![(target.clone(), filled.clone())],
    };
    let command = plan.command(&gpu, input, vec![]).unwrap();
    assert_eq!(command.prelude.as_ref().unwrap().groups, [1, 1, 1]);
    gpu.expand(command).unwrap();
    wait(&mut gpu);
    assert_eq!(words(&target.buffer), expected);
    assert_eq!(words(&filled.buffer), [expected.len() as u32 * 4]);
}

#[test]
fn indirect_recording_dispatch_counts() {
    use adv_device::gpu::DispatchArguments;
    let mut gpu = gpu();
    let arguments = upload(&gpu, &[3, 2, 1]);
    let counters = buffer(
        &gpu,
        &[0; statistics::INDIRECT_WORDS],
        BufferUsage::STORAGE,
        Memory::Readback,
    );
    let source = statistics::Indirect::Dispatch {
        threads_per_group: 32,
    }
    .compute();
    let shader =
        ModernShader::wgsl(42, &source, "indirect_statistics", ShaderStage::Compute).unwrap();
    let dispatch = gpu.prepare_dispatch(Arc::new(shader)).unwrap();
    let mut slots = SlotBindings::default();
    bind_storage(&mut slots, wgsl::STATISTICS_SLOT, counters.clone());
    bind_storage(&mut slots, statistics::INDIRECT_ARGUMENTS_SLOT, arguments);
    gpu.dispatch(&dispatch, &slots, DispatchArguments::Direct([1, 1, 1]))
        .unwrap();
    wait(&mut gpu);
    let output = words(&counters.buffer);
    let mut stats = statistics::PipelineStatistics::default();
    stats.record_indirect(output.try_into().unwrap());
    check_stats("dispatch", stats);
}

#[test]
fn recorded_counts_match_authored_statistics_shapes() {
    use statistics::{Assembly, PipelineStatistics as Stats, Rasterization};
    for (name, vertices, primitives, instances) in [
        ("instances", 3, 1, 5),
        ("incomplete", 5, 1, 1),
        ("index_instances", 54, 18, 2),
    ] {
        let mut stats = Stats::default();
        stats.record_draw(
            Assembly {
                vertices,
                primitives,
                instances,
            },
            None,
            None,
            Rasterization::Enabled,
        );
        let ranges = expected(name);
        assert_eq!(stats.ia_vertices, ranges[0].0);
        assert_eq!(stats.ia_primitives, ranges[1].0);
        assert_eq!(stats.vs_invocations, ranges[2].0);
        assert_eq!(stats.c_invocations, ranges[5].0);
    }
    let tess = tessellator::tessellate(
        Domain::Tri {
            edges: [2.; 3],
            inside: 2.,
            output: tessellator::SurfaceOutput::TriangleCcw,
        },
        Partitioning::Integer,
    );
    let mut stats = Stats::default();
    let (points, primitives) = Stats::tessellated(&[tess], Topology::Triangle);
    stats.record_draw(
        Assembly {
            vertices: 6,
            primitives: 2,
            instances: 1,
        },
        None,
        Some((points * 2, primitives * 2)),
        Rasterization::Enabled,
    );
    let rows = expected("tessellation");
    assert_eq!(
        [
            stats.c_invocations,
            stats.hs_invocations,
            stats.ds_invocations
        ],
        [rows[5].0, rows[8].0, rows[9].0]
    );

    let mut stats = Stats::default();
    stats.record_draw(
        Assembly {
            vertices: 4,
            primitives: 2,
            instances: 1,
        },
        Some(1),
        None,
        Rasterization::select(false, true, true),
    );
    stats.record_gpu([0, 0, 2, 0, 0, 0, 0, 0], true);
    check_stats("stream_only", stats);
    let mut stats = Stats::default();
    stats.record_draw(
        Assembly {
            vertices: 4,
            primitives: 2,
            instances: 1,
        },
        None,
        None,
        Rasterization::select(true, false, false),
    );
    assert_eq!(
        (
            stats.c_invocations,
            stats.c_primitives,
            stats.ps_invocations
        ),
        (0, 0, 0)
    );
}

#[test]
fn bins_draw_only_matching_primitives_across_chunks() {
    use crate::shader::variant::Bin;
    let mut gpu = gpu();
    let vs = {
        let mut vs = vertex(-1.0, 1.0, 0.5);
        vs.signatures.output.extend([
            signature(
                SystemValue::ViewportArrayIndex,
                2,
                1,
                ValueType::Uint,
                "SV_ViewportArrayIndex",
            ),
            signature(
                SystemValue::RenderTargetArrayIndex,
                3,
                1,
                ValueType::Uint,
                "SV_RenderTargetArrayIndex",
            ),
        ]);
        vs.body.push(mov(
            2,
            operand(
                Register::Input,
                vec![Index::Direct(0)],
                Components::Select(0),
            ),
        ));
        vs.body.push(mov(
            3,
            operand(
                Register::Input,
                vec![Index::Direct(0)],
                Components::Select(0),
            ),
        ));
        vs
    };
    let ps = fragment();
    let fragment = analyze(&ps);
    let producer = analyze(&vs);
    let caps = gpu.device().caps();
    let facts = DrawFacts {
        fragment: Some((1, &fragment)),
        viewport_count: 3,
        layer_count: 3,
        ..Default::default()
    };
    let variants: Vec<_> = (0..3)
        .map(|index| {
            Variant::select(
                &producer,
                &DrawFacts {
                    bin: Bin {
                        viewport: index,
                        first_layer: index,
                        layers: 1,
                    },
                    ..facts
                },
                caps,
            )
        })
        .collect();
    let references: Vec<_> = variants.iter().collect();
    let pixel = Variant::select(&fragment, &facts, caps);
    let pixel = shader(
        wgsl::emit_variant(&ps, &pixel, None, Options::default()).unwrap(),
        ShaderStage::Fragment,
    );
    let plan = Plan {
        source: Source::Vertex(Shader {
            ir: &vs,
            variant: &variants[0],
            options: Options::default(),
        }),
        topology: Topology::Triangle,
        input_vertices: 3,
        geometry: None,
        raster: Some(Raster {
            bins: &references,
            fragment: Some(&fragment),
            stream: 0,
            cull: Cull::None,
            front_ccw: true,
            depth_clip: true,
        }),
        stream_output: &[],
        stream_statistics: false,
    }
    .emit()
    .unwrap();
    let target = gpu
        .device()
        .create_image(&ImageDesc {
            size: Extent {
                width: 64,
                height: 64,
                depth_or_layers: 3,
            },
            dimension: Dimension::D2,
            format: Format::Rgba8Unorm,
            mips: 1,
            samples: 1,
            usage: Usage::COLOR | Usage::COPY_SRC,
            view_formats: &[],
            label: "expansion layer bins",
        })
        .unwrap();
    let bins = (0..3)
        .map(|i| {
            let mut draw = draw(
                &target,
                plan.vertex_shader(i as usize).unwrap(),
                pixel.clone(),
            );
            draw.colors[0].view = target
                .create_view(&ViewDesc {
                    format: None,
                    dimension: None,
                    mips: 0..1,
                    layers: i..i + 1,
                    plane: 0,
                })
                .unwrap();
            BinDraw {
                draw,
                fragment: SlotBindings::default(),
                statistics: None,
            }
        })
        .collect();
    let mut command = plan
        .command(&gpu, inputs(&gpu, &[0, 1, 2, 1, 3, 2]), bins)
        .unwrap();
    command.regions.push(ExpansionRegion {
        bytes_per_primitive: 2 * 1024 * 1024,
        fixed_bytes: 0,
        element_stride: 4,
    });
    assert_eq!(command.primitives_per_chunk(&caps.limits).unwrap(), 1);
    gpu.expand(command).unwrap();
    let first = layer_pixels(&mut gpu, &target, 0);
    let second = layer_pixels(&mut gpu, &target, 1);
    assert!(
        first
            .iter()
            .zip(&second)
            .all(|(&a, &b)| (a == 0) != (b == 0))
    );
    assert!(
        layer_pixels(&mut gpu, &target, 2)
            .iter()
            .all(|&pixel| pixel == 0)
    );
}

#[test]
fn clip_discard_matches_native_clip_distances() {
    let mut gpu = gpu();
    let mut caps = gpu.device().caps().clone();
    if !caps.features.contains(Features::CLIP_DISTANCES) {
        return;
    }
    let mut vs = vertex(-1.0, 1.0, 0.5);
    vs.signatures.output.push(signature(
        SystemValue::ClipDistance,
        2,
        1,
        ValueType::Float,
        "SV_ClipDistance",
    ));
    vs.body.push(mov(
        2,
        operand(
            Register::Output,
            vec![Index::Direct(0)],
            Components::Select(0),
        ),
    ));
    let ps = fragment();
    let vertex = analyze(&vs);
    let fragment = analyze(&ps);
    let facts = DrawFacts {
        fragment: Some((1, &fragment)),
        clip_components: 1,
        ..Default::default()
    };
    let native = image(&gpu, 1);
    let emulated = image(&gpu, 1);
    for target in [&native, &emulated] {
        let v = Variant::select(&vertex, &facts, &caps);
        let p = Variant::select(&fragment, &facts, &caps);
        let draw = draw(
            target,
            shader(
                wgsl::emit_variant(&vs, &v, Some(&fragment), Options::default()).unwrap(),
                ShaderStage::Vertex,
            ),
            shader(
                wgsl::emit_variant(&ps, &p, None, Options::default()).unwrap(),
                ShaderStage::Fragment,
            ),
        );
        gpu.draw_modern(draw, &SlotBindings::default(), &SlotBindings::default())
            .unwrap();
        caps.features.remove(Features::CLIP_DISTANCES);
    }
    let found = pixels(&mut gpu, &emulated);
    assert_eq!(found.iter().filter(|&&p| p != 0).count(), 2048);
    assert_eq!(found, pixels(&mut gpu, &native));
}

#[test]
fn counted_fragments_obey_early_depth_and_sample_frequency() {
    use adv_device::gpu::ModernDepth;
    use adv_gpu::pipeline::{Compare, DepthStencil};
    let mut gpu = gpu();
    let mut caps = gpu.device().caps().clone();
    caps.features.remove(Features::PIPELINE_STATISTICS_QUERY);
    let vs = vertex(-1.0, 1.0, 0.5);
    let vs_reflection = analyze(&vs);
    for (name, samples, depth_output) in [
        ("depth_fail", 1, false),
        ("depth_output", 1, true),
        ("samples", 4, false),
    ] {
        let mut ps = fragment();
        if depth_output {
            ps.signatures.output.push(signature(
                SystemValue::Depth,
                u32::MAX,
                1,
                ValueType::Float,
                "SV_Depth",
            ));
            ps.body.push(instruction(
                Opcode::Mov,
                vec![operand(Register::OutputDepth, vec![], Components::Scalar)],
                vec![immediate([0.5f32.to_bits(); 4])],
            ));
        }
        if samples == 4 {
            ps.signatures.input.push(signature(
                SystemValue::SampleIndex,
                2,
                1,
                ValueType::Uint,
                "SV_SampleIndex",
            ));
            ps.body.push(instruction(
                Opcode::Utof,
                vec![operand(
                    Register::Output,
                    vec![Index::Direct(0)],
                    Components::Mask(1),
                )],
                vec![operand(
                    Register::Input,
                    vec![Index::Direct(2)],
                    Components::Select(0),
                )],
            ));
        }
        let ps_reflection = analyze(&ps);
        let facts = DrawFacts {
            fragment: Some((1, &ps_reflection)),
            statistics: true,
            ..Default::default()
        };
        let ps_variant = Variant::select(&ps_reflection, &facts, &caps);
        let v_variant = Variant::select(&vs_reflection, &facts, &caps);
        assert_eq!(ps_variant.early_tests(), !depth_output);
        let ps = shader(
            wgsl::emit_variant(
                &ps,
                &ps_variant,
                None,
                Options {
                    raster_samples: samples,
                    ..Options::default()
                },
            )
            .unwrap(),
            ShaderStage::Fragment,
        );
        let plan = Plan {
            source: Source::Vertex(Shader {
                ir: &vs,
                variant: &v_variant,
                options: Options::default(),
            }),
            topology: Topology::Triangle,
            input_vertices: 3,
            geometry: None,
            raster: Some(Raster {
                bins: &[&v_variant],
                fragment: Some(&ps_reflection),
                stream: 0,
                cull: Cull::None,
                front_ccw: true,
                depth_clip: true,
            }),
            stream_output: &[],
            stream_statistics: false,
        }
        .emit()
        .unwrap();
        let color = image(&gpu, samples);
        let mut draw = draw(&color, plan.vertex_shader(0).unwrap(), ps);
        if samples == 1 {
            let depth = gpu
                .device()
                .create_image(&ImageDesc {
                    dimension: Dimension::D2,
                    size: color.desc().size,
                    format: Format::Depth32Float,
                    mips: 1,
                    samples,
                    usage: Usage::DEPTH_STENCIL,
                    view_formats: &[],
                    label: "expansion statistics depth",
                })
                .unwrap();
            draw.depth = Some(ModernDepth {
                view: depth.view().clone(),
                state: DepthStencil {
                    format: depth.desc().format,
                    depth_compare: Compare::Less,
                    depth_write: false,
                    stencil: None,
                    stencil_read_mask: 0,
                    stencil_write_mask: 0,
                    depth_bias: false,
                },
                depth: (Load::Clear(0.0), Store::Store),
                stencil: (Load::None, Store::None),
                read_only: false,
            });
        }
        let counters = buffer(
            &gpu,
            &[0; statistics::GPU_WORDS],
            BufferUsage::STORAGE,
            Memory::Readback,
        );
        let mut inputs = inputs(&gpu, &[0, 1, 2, 1, 3, 2]);
        inputs.statistics = Some(counters.clone());
        let command = plan
            .command(
                &gpu,
                inputs,
                vec![BinDraw {
                    draw,
                    fragment: SlotBindings::default(),
                    statistics: None,
                }],
            )
            .unwrap();
        gpu.expand(command).unwrap();
        wait(&mut gpu);
        let mut stats = statistics::PipelineStatistics::default();
        stats.record_draw(
            statistics::Assembly {
                vertices: 4,
                primitives: 2,
                instances: 1,
            },
            None,
            None,
            statistics::Rasterization::Enabled,
        );
        stats.record_gpu(words(&counters.buffer).try_into().unwrap(), false);
        check_stats(name, stats);
        if name == "depth_fail" {
            assert_eq!(stats.ps_invocations, 0);
        }
    }
}

#[test]
fn typed_view_formats_specialize_only_declared_uav_slots() {
    let gpu = gpu();
    let ir = crate::shader::dxbc::parse(include_bytes!(
        "../../../tests/fixtures/shader-core/typed_uav.dxbc"
    ))
    .unwrap();
    let reflection = analyze(&ir);
    let select = |storage_formats: &[(u32, naga::StorageFormat)]| {
        Variant::select(
            &reflection,
            &DrawFacts {
                storage_formats,
                ..Default::default()
            },
            gpu.device().caps(),
        )
    };
    let format = naga::StorageFormat::R32Uint;
    let variant = select(&[(7, format), (1, format)]);
    assert_eq!(variant.storage_formats(), &[(1, format)]);
    assert_eq!(variant, select(&[(1, format)]));
    assert_ne!(variant.pipeline_identity(1), variant.pipeline_identity(2));
    assert_ne!(variant, select(&[(1, naga::StorageFormat::Rgba8Uint)]));
}
