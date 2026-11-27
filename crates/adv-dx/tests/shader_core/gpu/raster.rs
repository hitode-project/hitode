use super::*;
use adv_gpu::{
    Extent, Format, Rect,
    caps::FormatCaps,
    encode::{BufferCopy, ColorAttachment, ImageCopy, Load, RenderPassDesc, Store},
    pipeline::{ColorTarget, Compare, Cull, Primitive, RenderPipelineDesc, Topology},
    resource::{Address, BorderColor, Dimension, Filter, ImageDesc, SamplerDesc, Usage},
};
use half::f16;

const WIDTH: u32 = 8;
const HEIGHT: u32 = 4;
const MSAA_SAMPLES: u32 = 4;

// The triangle spans 16 by 8 pixels in the test viewport, so its interpolated
// values are exact before conversion to the multisample target format.
const VERTEX: &str = "
struct Vertex {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec4<f32>,
}
@vertex fn vertex_main(@builtin(vertex_index) i: u32) -> Vertex {
    let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    let q = p * 0.5 - 0.25;
    let uv = 0.5 + 2.0 * q;
    return Vertex(vec4<f32>(q * vec2(1.0, -1.0), 0.0, 1.0),
                  vec4<f32>(uv + vec2(0.0625, 0.03125), uv));
}
";

struct Pixel {
    uv: [f32; 4],
    samples: u32,
    fixture: &'static str,
    border: bool,
}

impl Pixel {
    fn sample_offset(&self, index: u32) -> [f32; 2] {
        let offsets: &[[i32; 2]] = match self.samples {
            4 => &[[-2, -6], [6, -2], [-6, 2], [2, 6]],
            8 => &[
                [1, -3],
                [-1, 3],
                [5, 1],
                [-3, -5],
                [-5, 5],
                [-7, -1],
                [3, 7],
                [7, -7],
            ],
            other => panic!("unexpected sample count {other}"),
        };
        let [x, y] = offsets.get(index as usize).copied().unwrap_or([0; 2]);
        [x as f32 / 16.0, y as f32 / 16.0]
    }
}

impl Resources for Pixel {
    fn read(&mut self, access: ResourceAccess, args: &[Vector]) -> Result<Vector, ir::Error> {
        let result = match access.instruction {
            Opcode::Resinfo => {
                let (width, height, levels) = if self.fixture == "multisample_queries" {
                    (WIDTH, HEIGHT, 1)
                } else {
                    let size = 8 >> args[0][0];
                    (size, size, 3)
                };
                return Ok([width, height, 0, levels]);
            }
            Opcode::Ld => {
                let [x, y, _, _] = args[0];
                if x >= 4 || y >= 4 {
                    [0.0; 4]
                } else {
                    [0.0, (x + 4 * y) as f32 / 64.0, 0.0, 1.0]
                }
            }
            Opcode::LdMs => [0.25, 0.5, 0.75, 1.0],
            Opcode::Sample | Opcode::SampleB | Opcode::SampleD | Opcode::SampleL => {
                let [u, v, ..] = args[0].map(f32::from_bits);
                let x = (u * 8.0).floor() as u32 % 8;
                let y = (v * 8.0).floor() as u32 % 8;
                [0.0, (x + 8 * y) as f32 / 64.0, 0.0, 1.0]
            }
            Opcode::SampleC | Opcode::SampleCLz => {
                [f32::from(f32::from_bits(args[1][0]) <= 0.5); 4]
            }
            Opcode::SamplePos => {
                let [x, y] = self.sample_offset(args[0][0]);
                [x, y, 0.0, 0.0]
            }
            Opcode::SampleInfo => [self.samples as f32, 0.0, 0.0, 0.0],
            Opcode::Lod => [0.75, 1.25, 0.0, 0.0],
            Opcode::Gather4 | Opcode::Gather4C | Opcode::Gather4Po | Opcode::Gather4PoC => {
                let uv = args[0].map(f32::from_bits);
                let dynamic = matches!(access.instruction, Opcode::Gather4Po | Opcode::Gather4PoC);
                let offset = if dynamic {
                    args[1].map(|word| (word << 26) as i32 >> 26)
                } else {
                    let [x, y, z] = access.operands.offset.unwrap_or_default();
                    [x, y, z, 0]
                };
                let x = (uv[0] * 8.0 - 0.5).floor() as i32 + offset[0];
                let y = (uv[1] * 8.0 - 0.5).floor() as i32 + offset[1];
                [(0, 1), (1, 1), (1, 0), (0, 0)].map(|(dx, dy)| {
                    let (x, y) = (x + dx, y + dy);
                    let value = if self.border && (!(0..8).contains(&x) || !(0..8).contains(&y)) {
                        if access.instruction == Opcode::Gather4PoC {
                            0.625
                        } else {
                            0.375
                        }
                    } else {
                        (x.rem_euclid(8) + 8 * y.rem_euclid(8)) as f32 / 64.0
                    };
                    match access.instruction {
                        Opcode::Gather4C => f32::from(f32::from_bits(args[1][0]) <= 0.5),
                        Opcode::Gather4PoC => f32::from(value >= f32::from_bits(args[2][0])),
                        _ => value,
                    }
                })
            }
            op => return Err(ir::Error::UnsupportedOpcode(op)),
        };
        Ok(result.map(f32::to_bits))
    }

    fn derivative(
        &mut self,
        instruction: &ir::Instruction,
        _: Vector,
    ) -> Result<Vector, ir::Error> {
        let source = &instruction.sources[0];
        assert!(matches!(
            source.register(),
            Some((Register::Input, [ir::Index::Direct(0)]))
        ));
        let step = if matches!(
            instruction.opcode,
            Opcode::DerivRtx | Opcode::DerivRtxCoarse | Opcode::DerivRtxFine
        ) {
            [1.0 / WIDTH as f32, 0.0, 1.0 / WIDTH as f32, 0.0]
        } else {
            [0.0, 1.0 / HEIGHT as f32, 0.0, 1.0 / HEIGHT as f32]
        };
        Ok(source
            .components
            .swizzle()
            .map(|lane| step[lane as usize].to_bits()))
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
    let caps = gpu.device.caps();
    // The multisample fixtures' values are exact in RGBA16Float, which
    // supports 4x rendering and resolve on Apple5 and Apple6.
    let msaa_format = [Format::Rgba16Float, Format::Rgba32Float]
        .into_iter()
        .find(|&format| {
            caps.format(format)
                .contains(FormatCaps::COLOR | FormatCaps::SAMPLED)
                && caps.format_sample_counts(format) & MSAA_SAMPLES != 0
        })
        .expect("4x multisampled float color format");
    let native_8x = caps.format_sample_counts(msaa_format) & 8 != 0
        && caps
            .features
            .contains(adv_gpu::Features::STANDARD_SAMPLE_LOCATIONS);
    for (identity, (name, border, samples)) in [
        "raster_queries",
        "raster_samples",
        "level_query",
        "dynamic_gather",
        "dynamic_gather_compare",
        "derivative_queries",
        "derivatives_4_0",
        "multisample_queries",
        "sample_modes",
        "texture_ops",
    ]
    .into_iter()
    .map(|name| {
        let samples = if matches!(name, "raster_queries" | "raster_samples") {
            MSAA_SAMPLES
        } else {
            1
        };
        (name, false, samples)
    })
    .chain([
        ("dynamic_gather", true, 1),
        ("dynamic_gather_compare", true, 1),
    ])
    .chain(
        native_8x
            .then_some([("raster_queries", false, 8), ("raster_samples", false, 8)])
            .into_iter()
            .flatten(),
    )
    .enumerate()
    {
        let module = module(name);
        let slot = u32::from(name == "dynamic_gather_compare");
        let border_samplers = [wgsl::BorderSampler {
            slot,
            axes: [true; 3],
            color: [0.625, 0.375, 0., 1.],
            linear: [true; 3],
            lod: wgsl::LodSampler {
                bias: 0.,
                min: 0.,
                max: 16.,
                max_anisotropy: 16,
                round_mip: false,
                point: false,
                linear_comparison: slot == 1,
            },
            comparison: 4,
        }];
        let border_textures = [wgsl::BorderTexture {
            slot,
            channels: [wgsl::BorderChannel::Float; 4],
        }];
        let target_format = if samples > 1 {
            msaa_format
        } else {
            Format::Rgba32Float
        };
        let translation = wgsl::emit(
            &module,
            wgsl::Options {
                raster_samples: samples,
                border_samplers: if border { &border_samplers } else { &[] },
                border_textures: if border { &border_textures } else { &[] },
                lod_samplers: &[(
                    0,
                    wgsl::LodSampler {
                        bias: 0.25,
                        min: 0.0,
                        max: 0.75,
                        max_anisotropy: 1,
                        round_mip: false,
                        point: false,
                        linear_comparison: false,
                    },
                )],
                ..Default::default()
            },
        )
        .unwrap();
        let supplemental = identity >= 5;
        // Keep point samples and integer texel addresses away from rounding boundaries.
        let vertex_source = supplemental.then(|| VERTEX.replace("0.0625", "0.09375"));
        let vertex = gpu
            .device
            .create_shader(ShaderDesc {
                source: Source::Wgsl(vertex_source.as_deref().unwrap_or(VERTEX)),
                label: "shader oracle vertex",
                identity: u64::from(supplemental),
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

        let multisampled = name == "multisample_queries";
        let mut bindings = Vec::new();
        let mut encoder = gpu.queue.encoder(name);
        let mut images = [None, None];
        let mut samplers = [None, None];
        for location in &translation.locations {
            let slot = location.slot as usize;
            let depth = slot == 1;
            let kind = match location.register {
                Register::Resource => {
                    let format = if depth {
                        Format::Depth32Float
                    } else if multisampled {
                        msaa_format
                    } else {
                        Format::Rgba32Float
                    };
                    let mips = if !depth && matches!(name, "level_query" | "texture_ops") {
                        3
                    } else {
                        1
                    };
                    let size = if multisampled {
                        extent(WIDTH, HEIGHT)
                    } else {
                        extent(8, 8)
                    };
                    let image = gpu
                        .device
                        .create_image(&ImageDesc {
                            dimension: Dimension::D2,
                            size,
                            format,
                            mips,
                            samples: if multisampled { MSAA_SAMPLES } else { 1 },
                            usage: Usage::SAMPLED
                                | if multisampled {
                                    Usage::COLOR
                                } else {
                                    Usage::COPY_DST
                                }
                                | if depth {
                                    Usage::DEPTH_STENCIL
                                } else {
                                    Usage::empty()
                                },
                            view_formats: &[],
                            label: name,
                        })
                        .unwrap();
                    if multisampled {
                        let _clear = encoder.render_pass(&RenderPassDesc {
                            colors: [ColorAttachment {
                                view: image.view(),
                                load: Load::Clear([0.25, 0.5, 0.75, 1.0]),
                                store: Store::Store,
                                resolve: None,
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
                    } else if name != "level_query" {
                        for mip in 0..mips {
                            let width = 8 >> mip;
                            let values: Vec<_> = (0..width * width)
                                .flat_map(|i| {
                                    let value = i as f32 / 64.0;
                                    if depth {
                                        if name == "dynamic_gather_compare" {
                                            [value, 0.0, 0.0, 0.0]
                                        } else {
                                            [0.5, 0.0, 0.0, 0.0]
                                        }
                                    } else {
                                        [0.0, value, 0.0, 1.0]
                                    }
                                })
                                .collect();
                            let compact: Vec<_> = if depth {
                                values.into_iter().step_by(4).collect()
                            } else {
                                values
                            };
                            let upload = gpu.buffer(
                                (compact.len() * 4) as u64,
                                BufferUsage::COPY_SRC,
                                Memory::Upload,
                            );
                            // The upload is mapped before the encoder references it.
                            unsafe { &mut *upload.mapped().unwrap().as_ptr() }
                                .copy_from_slice(bytemuck::cast_slice(&compact));
                            encoder.copy_buffer_to_image(
                                BufferCopy {
                                    buffer: &upload,
                                    offset: 0,
                                    row_pitch: 0,
                                    rows: 0,
                                },
                                ImageCopy {
                                    mip,
                                    ..image_copy(&image)
                                },
                                extent(width, width),
                            );
                        }
                    }
                    images[slot] = Some(image);
                    GpuBinding::SampledImage {
                        dimension: Dimension::D2,
                        depth,
                        multisampled,
                    }
                }
                Register::Sampler => {
                    samplers[slot] = Some(
                        gpu.device
                            .sampler(&SamplerDesc {
                                min: if border {
                                    Filter::Linear
                                } else {
                                    Filter::Nearest
                                },
                                mag: if border {
                                    Filter::Linear
                                } else {
                                    Filter::Nearest
                                },
                                mip: if name == "sample_modes" {
                                    Filter::Nearest
                                } else {
                                    Filter::Linear
                                },
                                address: [if border {
                                    Address::Border
                                } else {
                                    Address::Repeat
                                }; 3],
                                border: BorderColor::TransparentBlack,
                                lod_bias: 0,
                                lod_min: 0,
                                lod_max: u16::MAX,
                                anisotropy: if border { 16 } else { 1 },
                                compare: depth.then_some(Compare::LessEqual),
                            })
                            .unwrap(),
                    );
                    GpuBinding::Sampler { comparison: depth }
                }
                register => panic!("unexpected register {register:?}"),
            };
            bindings.push(Binding {
                slot: location.binding,
                kind,
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
                        format: target_format,
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
                format: target_format,
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
                    format: target_format,
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
                        Bind::Sampled(images[location.slot as usize].as_ref().unwrap().view()),
                    );
                } else {
                    pass.bind(
                        location.binding,
                        Bind::Sampler(samplers[location.slot as usize].as_ref().unwrap()),
                    );
                }
            }
            pass.draw(0..3, 0..1);
        }
        let half_target = target_format == Format::Rgba16Float;
        let pixel_bytes = if half_target { 8 } else { 16 };
        let output = gpu.buffer(
            WIDTH as u64 * HEIGHT as u64 * pixel_bytes as u64,
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
        let values = unsafe { &*output.mapped().unwrap().as_ptr() };
        let oracle: Vec<Vec<f32>> = include_str!("../../fixtures/shader-core/raster.tsv")
            .lines()
            .map(|line| line.split_once('\t').unwrap())
            .filter(|&(fixture, _)| !border && samples != 8 && fixture == name)
            .map(|(_, row)| {
                row.split('\t')
                    .skip(1)
                    .map(|bits| f32::from_bits(u32::from_str_radix(bits, 16).unwrap()))
                    .collect()
            })
            .collect();
        if !supplemental || !oracle.is_empty() {
            assert_eq!(oracle.len(), HEIGHT as usize, "{name}");
        }
        for (i, pixel) in values.chunks_exact(pixel_bytes).enumerate() {
            let actual = if half_target {
                bytemuck::pod_read_unaligned::<[u16; 4]>(pixel)
                    .map(|bits| f16::from_bits(bits).to_f32())
            } else {
                bytemuck::pod_read_unaligned::<[f32; 4]>(pixel)
            };
            let x = (i as u32 % WIDTH) as f32 + 0.5;
            let y = (i as u32 / WIDTH) as f32 + 0.5;
            let u = x / WIDTH as f32;
            let v = y / HEIGHT as f32;
            let u_offset = if supplemental { 0.09375 } else { 0.0625 };
            let uv = [u + u_offset, v + 0.03125, u, v];
            let mut inputs = Inputs::default();
            for element in &module.signatures.input {
                let value = match element.system_value {
                    ir::SystemValue::Position => [x, y, 0.0, 1.0].map(f32::to_bits),
                    ir::SystemValue::None => uv.map(f32::to_bits),
                    ir::SystemValue::SampleIndex => [0; 4],
                    value => panic!("unexpected system value {value:?}"),
                };
                inputs
                    .registers
                    .insert((Register::Input, element.register), value);
            }
            if name == "raster_samples" {
                inputs
                    .registers
                    .insert((Register::InputCoverageMask, 0), [(1 << samples) - 1; 4]);
            }
            let result = run(
                &module,
                &inputs,
                &Constants::new(),
                &mut Pixel {
                    uv,
                    samples: if multisampled { MSAA_SAMPLES } else { samples },
                    fixture: name,
                    border,
                },
            )
            .unwrap();
            if name == "raster_samples" {
                assert_eq!(
                    result.registers[&(Register::OutputCoverageMask, 0)][0],
                    (1 << samples) - 1
                );
            }
            let expected = result.registers[&(Register::Output, 0)].map(f32::from_bits);
            if let Some(row) = oracle.get(i / WIDTH as usize) {
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
            }
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
