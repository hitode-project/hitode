use super::*;
use adv_gpu::{
    Extent, Format, Rect,
    encode::{BufferCopy, ColorAttachment, ImageCopy, Load, RenderPassDesc, Store},
    pipeline::{ColorTarget, Compare, Cull, Primitive, RenderPipelineDesc, Topology},
    resource::{Address, BorderColor, Dimension, Filter, ImageDesc, SamplerDesc, Usage},
};

const WIDTH: u32 = 8;
const HEIGHT: u32 = 4;

const VERTEX: &str = "
struct Vertex {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec4<f32>,
}
@vertex fn vertex_main(@builtin(vertex_index) i: u32) -> Vertex {
    let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    let q = p * 0.6875 - 0.375;
    let uv = 0.5 + 2.0 * q;
    return Vertex(vec4<f32>(q * vec2(1.0, -1.0), 0.0, 1.0),
                  vec4<f32>(uv + vec2(0.0625, 0.03125), uv));
}
";

struct Pixel {
    uv: [f32; 4],
    samples: u32,
}

impl Pixel {
    fn sample_offset(&self, index: u32) -> [f32; 2] {
        assert_eq!(self.samples, 4);
        let [x, y] = [[-2, -6], [6, -2], [-6, 2], [2, 6]]
            .get(index as usize)
            .copied()
            .unwrap_or([0; 2]);
        [x as f32 / 16.0, y as f32 / 16.0]
    }
}

impl Resources for Pixel {
    fn read(&mut self, access: ResourceAccess, args: &[Vector]) -> Result<Vector, ir::Error> {
        let result = match access.instruction {
            Opcode::SamplePos => {
                let [x, y] = self.sample_offset(args[0][0]);
                [x, y, 0.0, 0.0]
            }
            Opcode::SampleInfo => [self.samples as f32, 0.0, 0.0, 0.0],
            Opcode::Lod => [0.75, 1.25, 0.0, 0.0],
            Opcode::Gather4Po | Opcode::Gather4PoC => {
                let uv = args[0].map(f32::from_bits);
                let offset = |word: u32| ((word << 26) as i32) >> 26;
                let x = (uv[0] * 8.0 - 0.5).floor() as i32 + offset(args[1][0]);
                let y = (uv[1] * 8.0 - 0.5).floor() as i32 + offset(args[1][1]);
                [(0, 1), (1, 1), (1, 0), (0, 0)].map(|(dx, dy)| {
                    let value = ((x + dx).rem_euclid(8) + 8 * (y + dy).rem_euclid(8)) as f32 / 64.0;
                    if access.instruction == Opcode::Gather4PoC {
                        f32::from(value >= f32::from_bits(args[2][0]))
                    } else {
                        value
                    }
                })
            }
            op => return Err(ir::Error::UnsupportedOpcode(op)),
        };
        Ok(result.map(f32::to_bits))
    }

    fn interpolate(
        &mut self,
        instruction: Opcode,
        input: u32,
        swizzle: [u8; 4],
        args: &[Vector],
    ) -> Result<Vector, ir::Error> {
        assert_eq!(input, 0);
        let mut uv = self.uv;
        let offset = match instruction {
            Opcode::EvalSnapped => [
                args[1][0] as i32 as f32 / 16.0,
                args[1][1] as i32 as f32 / 16.0,
            ],
            Opcode::EvalSampleIndex => self.sample_offset(args[1][0]),
            Opcode::EvalCentroid => [0.0; 2],
            _ => return Err(ir::Error::UnsupportedOpcode(instruction)),
        };
        uv[0] += offset[0] / WIDTH as f32;
        uv[1] += offset[1] / HEIGHT as f32;
        Ok(swizzle.map(|lane| uv[lane as usize].to_bits()))
    }

    fn atomic(&mut self, access: ResourceAccess, _: &[Vector]) -> Result<Vector, ir::Error> {
        Err(ir::Error::UnsupportedOpcode(access.instruction))
    }

    fn write(&mut self, access: ResourceAccess, _: u8, _: &[Vector]) -> Result<(), ir::Error> {
        Err(ir::Error::UnsupportedOpcode(access.instruction))
    }
}

#[test]
// spec: GFX-NUM-1
fn raster_queries_and_dynamic_gathers_match_scalar_ir() {
    let mut gpu = Compute::open();
    for (identity, name) in [
        "raster_queries",
        "raster_samples",
        "level_query",
        "dynamic_gather",
        "dynamic_gather_compare",
    ]
    .into_iter()
    .enumerate()
    {
        let module = module(name);
        let samples = if matches!(name, "raster_queries" | "raster_samples") {
            4
        } else {
            1
        };
        let translation = wgsl::emit(
            &module,
            wgsl::Options {
                raster_samples: samples,
                lod_samplers: &[(
                    0,
                    wgsl::LodSampler {
                        bias: 0.25,
                        min: 0.0,
                        max: 0.75,
                        max_anisotropy: 1,
                    },
                )],
                ..Default::default()
            },
        )
        .unwrap();
        let vertex = gpu
            .device
            .create_shader(ShaderDesc {
                source: Source::Wgsl(VERTEX),
                label: "shader oracle vertex",
                identity: 1,
            })
            .unwrap();
        let pixel = gpu
            .device
            .create_shader(ShaderDesc {
                source: Source::Wgsl(&translation.source),
                label: name,
                identity: 2 + identity as u64,
            })
            .unwrap_or_else(|err| panic!("{name}: {err}\n{}", translation.source));

        let depth = name == "dynamic_gather_compare";
        let depth_format = if depth {
            Format::Depth32Float
        } else {
            Format::Rgba32Float
        };
        let mut bindings = Vec::new();
        let mut encoder = gpu.queue.encoder(name);
        let texture = if translation
            .locations
            .iter()
            .any(|location| location.register == Register::Resource)
        {
            let image = gpu
                .device
                .create_image(&ImageDesc {
                    dimension: Dimension::D2,
                    size: extent(8, 8),
                    format: depth_format,
                    mips: if name == "level_query" { 3 } else { 1 },
                    samples: 1,
                    usage: Usage::SAMPLED
                        | Usage::COPY_DST
                        | if depth {
                            Usage::DEPTH_STENCIL
                        } else {
                            Usage::empty()
                        },
                    view_formats: &[],
                    label: name,
                })
                .unwrap();
            if name != "level_query" {
                let values = (0..64)
                    .flat_map(|i| {
                        let value = i as f32 / 64.0;
                        if depth {
                            [value, 0.0, 0.0, 0.0]
                        } else {
                            [0.0, value, 0.0, 1.0]
                        }
                    })
                    .collect::<Vec<_>>();
                let compact = if depth {
                    values
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .map(|pixel| pixel[0])
                        .collect::<Vec<_>>()
                } else {
                    values
                };
                let upload = gpu.buffer(
                    (compact.len() * 4) as u64,
                    BufferUsage::COPY_SRC,
                    Memory::Upload,
                );
                // No pending copy references the upload.
                unsafe { &mut *upload.mapped().unwrap().as_ptr() }
                    .copy_from_slice(bytemuck::cast_slice(&compact));
                encoder.copy_buffer_to_image(
                    BufferCopy {
                        buffer: &upload,
                        offset: 0,
                        row_pitch: 0,
                        rows: 0,
                    },
                    image_copy(&image),
                    extent(8, 8),
                );
            }
            Some(image)
        } else {
            None
        };
        let sampler = texture.as_ref().map(|_| {
            gpu.device
                .sampler(&SamplerDesc {
                    min: Filter::Nearest,
                    mag: Filter::Nearest,
                    mip: Filter::Linear,
                    address: [Address::Repeat; 3],
                    border: BorderColor::TransparentBlack,
                    lod_bias: 0,
                    lod_min: 0,
                    lod_max: u16::MAX,
                    anisotropy: 1,
                    compare: depth.then_some(Compare::LessEqual),
                })
                .unwrap()
        });
        for location in &translation.locations {
            bindings.push(Binding {
                slot: location.binding,
                kind: match location.register {
                    Register::Resource => GpuBinding::SampledImage {
                        dimension: Dimension::D2,
                        depth,
                        multisampled: false,
                    },
                    Register::Sampler => GpuBinding::Sampler { comparison: depth },
                    register => panic!("unexpected register {register:?}"),
                },
                stages: Stages::FRAGMENT,
            });
        }
        let pipeline = gpu
            .device
            .create_render_pipeline(
                &RenderPipelineDesc {
                    vertex: vertex.entry("vertex_main", Stage::Vertex).unwrap(),
                    fragment: Some(
                        pixel
                            .entry(translation.entry_point, Stage::Fragment)
                            .unwrap(),
                    ),
                    bindings,
                    primitive: Primitive {
                        topology: Topology::Triangles,
                        cull: Cull::None,
                        front_ccw: false,
                        wireframe: false,
                        unclipped_depth: false,
                    },
                    colors: [ColorTarget {
                        format: Format::Rgba32Float,
                        blend: None,
                        write_mask: 15,
                    }]
                    .into_iter()
                    .collect(),
                    depth_stencil: None,
                    samples,
                    alpha_to_coverage: false,
                    label: name,
                },
                None,
            )
            .unwrap();
        let target = gpu
            .device
            .create_image(&ImageDesc {
                dimension: Dimension::D2,
                size: extent(WIDTH, HEIGHT),
                format: Format::Rgba32Float,
                mips: 1,
                samples: 1,
                usage: Usage::COLOR | Usage::COPY_SRC,
                view_formats: &[],
                label: name,
            })
            .unwrap();
        let msaa = (samples > 1).then(|| {
            gpu.device
                .create_image(&ImageDesc {
                    dimension: Dimension::D2,
                    size: extent(WIDTH, HEIGHT),
                    format: Format::Rgba32Float,
                    mips: 1,
                    samples,
                    usage: Usage::COLOR,
                    view_formats: &[],
                    label: name,
                })
                .unwrap()
        });
        {
            let mut pass = encoder.render_pass(&RenderPassDesc {
                colors: [ColorAttachment {
                    view: msaa.as_ref().unwrap_or(&target).view(),
                    load: Load::DontCare,
                    store: Store::Store,
                    resolve: msaa.as_ref().map(|_| target.view()),
                }]
                .into_iter()
                .collect(),
                depth_stencil: None,
                area: Rect {
                    x: 0,
                    y: 0,
                    w: WIDTH,
                    h: HEIGHT,
                },
                timestamps: None,
                label: name,
            });
            pass.set_pipeline(&pipeline);
            // Keep the tested pixels inside one unclipped triangle.
            pass.set_viewport(-12.0, -6.0, 32.0, 16.0, 0.0, 1.0);
            for location in &translation.locations {
                if location.register == Register::Resource {
                    pass.bind(
                        location.binding,
                        Bind::Sampled(texture.as_ref().unwrap().view()),
                    );
                } else {
                    pass.bind(location.binding, Bind::Sampler(sampler.as_ref().unwrap()));
                }
            }
            pass.draw(0..3, 0..1);
        }
        let output = gpu.buffer(
            (WIDTH * HEIGHT * 16) as u64,
            BufferUsage::COPY_DST,
            Memory::Readback,
        );
        encoder.copy_image_to_buffer(
            image_copy(&target),
            BufferCopy {
                buffer: &output,
                offset: 0,
                row_pitch: 0,
                rows: 0,
            },
            extent(WIDTH, HEIGHT),
        );
        let token = gpu.queue.submit(encoder).unwrap();
        assert!(gpu.queue.wait(token, Duration::from_secs(20)).unwrap());
        // The render and readback copy have completed.
        let values: &[[f32; 4]] =
            bytemuck::cast_slice(unsafe { &*output.mapped().unwrap().as_ptr() });
        let oracle: Vec<Vec<f32>> = include_str!("../../fixtures/shader-core/raster.tsv")
            .lines()
            .map(|line| line.split_once('\t').unwrap())
            .filter(|&(fixture, _)| fixture == name)
            .map(|(_, row)| {
                row.split('\t')
                    .skip(1)
                    .map(|bits| f32::from_bits(u32::from_str_radix(bits, 16).unwrap()))
                    .collect()
            })
            .collect();
        assert_eq!(oracle.len(), HEIGHT as usize, "{name}");
        for (i, actual) in values.iter().enumerate() {
            let x = (i as u32 % WIDTH) as f32 + 0.5;
            let y = (i as u32 / WIDTH) as f32 + 0.5;
            let u = x / WIDTH as f32;
            let v = y / HEIGHT as f32;
            let uv = [u + 0.0625, v + 0.03125, u, v];
            let mut inputs = Inputs::default();
            for element in &module.signatures.input {
                let value = match element.system_value {
                    ir::SystemValue::Position => [x, y, 0.0, 1.0],
                    ir::SystemValue::None => uv,
                    value => panic!("unexpected system value {value:?}"),
                };
                inputs
                    .registers
                    .insert((Register::Input, element.register), value.map(f32::to_bits));
            }
            if name == "raster_samples" {
                inputs
                    .registers
                    .insert((Register::InputCoverageMask, 0), [15; 4]);
            }
            let result = run(
                &module,
                &inputs,
                &Constants::new(),
                &mut Pixel { uv, samples },
            )
            .unwrap();
            if name == "raster_samples" {
                assert_eq!(result.registers[&(Register::OutputCoverageMask, 0)][0], 15);
            }
            let expected = result.registers[&(Register::Output, 0)].map(f32::from_bits);
            let row = &oracle[i / WIDTH as usize];
            let column = i % WIDTH as usize * 4;
            let hardware = &row[column..][..4];
            let warp = &row[(WIDTH as usize * 4 + column)..][..4];
            assert!(
                [hardware, warp].into_iter().any(|windows| expected
                    .iter()
                    .zip(windows)
                    .all(|(a, b)| (a - b).abs() < 1e-5)),
                "{name} ({x}, {y}): scalar {expected:?}, Windows {hardware:?} / {warp:?}"
            );
            assert!(
                actual
                    .iter()
                    .zip(expected)
                    .all(|(a, b)| (a - b).abs() < 1e-5),
                "{name} ({x}, {y}): {actual:?} expected {expected:?}"
            );
        }
    }
}

fn extent(width: u32, height: u32) -> Extent {
    Extent {
        width,
        height,
        depth_or_layers: 1,
    }
}

fn image_copy(image: &adv_gpu::Image) -> ImageCopy<'_> {
    ImageCopy {
        image,
        mip: 0,
        layer: 0,
        origin: (0, 0, 0),
    }
}
