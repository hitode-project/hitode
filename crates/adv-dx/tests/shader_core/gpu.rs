use super::module;
#[path = "gpu/alu.rs"]
mod alu;
use adv_device::gpu::{ConstantBlock, DrawState, Program, ProgrammableDraw};
use adv_dx::shader::{
    ir::{self, BindingKind, Opcode, Register, Vector, interpreter::*},
    wgsl,
};
use adv_gpu::{
    Buffer, Device, Queue,
    encode::{Bind, ComputePassDesc},
    pipeline::{Binding, BindingKind as GpuBinding, ComputePipelineDesc, Stages},
    queue::DeviceOptions,
    resource::{BufferDesc, BufferUsage, Memory},
    shader::{ShaderDesc, Source, Stage},
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

struct Compute {
    device: Device,
    queue: Queue,
}

impl Compute {
    fn open() -> Self {
        let (device, queue) = Device::open(&DeviceOptions {
            software: !cfg!(any(target_vendor = "apple", target_os = "android")),
            validation: !cfg!(target_os = "android"),
            disable: adv_gpu::caps::Modern::from_test_env(),
            label: "shader core",
            ..Default::default()
        })
        .unwrap();
        Self { device, queue }
    }

    fn buffer(&self, size: u64, usage: BufferUsage, memory: Memory) -> Buffer {
        self.device
            .create_buffer(&BufferDesc {
                size,
                usage,
                memory,
                label: "shader oracle",
            })
            .unwrap()
    }

    fn run(
        &mut self,
        name: &str,
        module: &ir::Module,
        words: &[u32],
        output_count: usize,
        groups: u32,
    ) -> Vec<u32> {
        let translation = wgsl::emit(module, wgsl::Options::default())
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        let shader = self
            .device
            .create_shader(ShaderDesc {
                source: Source::Wgsl(&translation.source),
                identity: std::hash::BuildHasher::hash_one(
                    &std::hash::RandomState::new(),
                    &translation.source,
                ),
                label: "shader oracle",
            })
            .unwrap_or_else(|error| panic!("{name}: {error}\n{}", translation.source));
        let bindings = translation
            .locations
            .iter()
            .map(|location| {
                let kind = translation
                    .reflection
                    .bindings
                    .iter()
                    .find(|b| (b.register, b.slot) == (location.register, location.slot))
                    .unwrap()
                    .kind;
                assert!(matches!(
                    kind,
                    BindingKind::Raw { .. } | BindingKind::Structured { .. }
                ));
                Binding {
                    slot: location.binding,
                    kind: GpuBinding::StorageBuffer {
                        writable: location.register == Register::UnorderedAccessView,
                    },
                    stages: Stages::COMPUTE,
                }
            })
            .collect();
        let pipeline = self
            .device
            .create_compute_pipeline(
                &ComputePipelineDesc {
                    entry: shader
                        .entry(translation.entry_point, Stage::Compute)
                        .unwrap(),
                    bindings,
                    label: "shader oracle",
                },
                None,
            )
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        let input_size = std::mem::size_of_val(words).max(4) as u64;
        let input = self.buffer(input_size, BufferUsage::STORAGE, Memory::Upload);
        // The input is mapped by the owning test and has no pending submissions.
        let input_bytes = unsafe { &mut *input.mapped().unwrap().as_ptr() };
        input_bytes[..std::mem::size_of_val(words)].copy_from_slice(bytemuck::cast_slice(words));
        let output_size = (output_count * 4) as u64;
        let output = self.buffer(
            output_size,
            BufferUsage::STORAGE | BufferUsage::COPY_SRC,
            Memory::Device,
        );
        let readback = self.buffer(output_size, BufferUsage::COPY_DST, Memory::Readback);
        let mut encoder = self.queue.encoder("shader oracle");
        {
            let mut pass = encoder.compute_pass(&ComputePassDesc {
                timestamps: None,
                label: "shader oracle",
            });
            pass.set_pipeline(&pipeline);
            for location in translation.locations {
                let buffer = if location.register == Register::UnorderedAccessView {
                    output.slice(0..output_size)
                } else {
                    input.slice(0..input_size)
                };
                pass.bind(location.binding, Bind::StorageBuffer(buffer));
            }
            pass.dispatch(groups, 1, 1);
        }
        encoder.copy_buffer(output.slice(0..output_size), &readback, 0);
        let token = self.queue.submit(encoder).unwrap();
        assert!(self.queue.wait(token, Duration::from_secs(20)).unwrap());
        // The queue has completed the copy and keeps no writer to the readback mapping.
        let bytes = unsafe { &*readback.mapped().unwrap().as_ptr() };
        bytes
            .as_chunks::<4>()
            .0
            .iter()
            .copied()
            .map(u32::from_le_bytes)
            .collect()
    }
}

struct Buffers<'a> {
    input: &'a [u32],
    output: Vec<u32>,
}

impl Resources for Buffers<'_> {
    fn read(&mut self, access: ResourceAccess, arguments: &[Vector]) -> Result<Vector, ir::Error> {
        assert_eq!(access.binding, (Register::Resource, 0));
        let base = match access.instruction {
            Opcode::LdStructured => arguments[0][0] + arguments[1][0] / 4,
            Opcode::LdRaw => arguments[0][0] / 4,
            other => return Err(ir::Error::UnsupportedOpcode(other)),
        } as usize;
        Ok(std::array::from_fn(|lane| {
            self.input.get(base + lane).copied().unwrap_or(0)
        }))
    }

    fn atomic(
        &mut self,
        access: ResourceAccess,
        arguments: &[Vector],
    ) -> Result<Vector, ir::Error> {
        assert_eq!(access.binding, (Register::UnorderedAccessView, 0));
        let index = arguments[0][0] as usize;
        let stored = &mut self.output[index];
        let old = *stored;
        match access.instruction {
            Opcode::ImmAtomicIadd => *stored = stored.wrapping_add(arguments[1][0]),
            Opcode::ImmAtomicCmpExch if old == arguments[1][0] => *stored = arguments[2][0],
            Opcode::ImmAtomicCmpExch => {}
            other => return Err(ir::Error::UnsupportedOpcode(other)),
        }
        Ok([old; 4])
    }

    fn write(
        &mut self,
        access: ResourceAccess,
        mask: u8,
        arguments: &[Vector],
    ) -> Result<(), ir::Error> {
        assert_eq!(access.binding, (Register::UnorderedAccessView, 0));
        let (base, value) = match access.instruction {
            Opcode::StoreStructured => (arguments[0][0] + arguments[1][0] / 4, arguments[2]),
            Opcode::StoreRaw => (arguments[0][0] / 4, arguments[1]),
            other => return Err(ir::Error::UnsupportedOpcode(other)),
        };
        for (lane, word) in value.into_iter().enumerate() {
            if mask & (1 << lane) != 0
                && let Some(slot) = self.output.get_mut(base as usize + lane)
            {
                *slot = word;
            }
        }
        Ok(())
    }
}

fn interpret_compute(
    module: &ir::Module,
    data: &[u32],
    output_count: usize,
    invocations: u32,
) -> Vec<u32> {
    let mut buffers = Buffers {
        input: data,
        output: vec![0; output_count],
    };
    let constants = Constants::new();
    let mut inputs = Inputs::default();
    for invocation in 0..invocations {
        inputs
            .registers
            .insert((Register::InputThreadId, 0), [invocation, 0, 0, 0]);
        run(module, &inputs, &constants, &mut buffers).unwrap();
    }
    buffers.output
}

#[test]
// spec: GFX-NUM-1
fn native_compute_matches_scalar_ir() {
    let mut gpu = Compute::open();
    let arithmetic = [
        0x00000001, 0x80000001, 0x7f7fffff, 0x387fc000, 0x3f803000, 0x477ff000, 0x40200000,
        0xc0200000, 0, 0x80000000, 0x3f800000, 33, 32, 0x3f000000, 0x40000000, 0x40400000,
    ];
    let control = [0, 1, 2, 3, 4, 5, 6, 7];
    for (name, data, outputs) in [
        ("arithmetic", arithmetic.as_slice(), 32),
        ("control", control.as_slice(), 8),
        ("atomics", control.as_slice(), 16),
        ("raw", arithmetic.as_slice().split_at(8).0, 8),
    ] {
        let module = module(name);
        let expected = interpret_compute(&module, data, outputs, 8);
        assert_eq!(gpu.run(name, &module, data, outputs, 1), expected, "{name}");
    }
    let shared = module("cs_5_0");
    assert_eq!(
        gpu.run("shared memory", &shared, &[], 8, 1),
        [49, 36, 25, 16, 9, 4, 1, 0]
    );
}

#[test]
// spec: GFX-REC-4
fn programmable_pixels_match_scalar_ir() {
    const VERTEX: &str =
        "@vertex fn vertex_main(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
        let xy = array(vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0));
        return vec4<f32>(xy[i], 0.5, 1.0);
    }";
    let mut gpu = adv_device::gpu::test_support::request_backend(
        !cfg!(any(target_os = "macos", target_os = "android")),
        adv_device::gpu::GpuBackend::Automatic,
    )
    .expect("programmable GPU");
    for name in ["color", "discard_pixel"] {
        let module = module(name);
        let fragment = wgsl::emit(&module, wgsl::Options::default()).unwrap();
        let target = gpu.target(16, 8, false).unwrap();
        gpu.clear(&target, Some([0.0; 4]), None).unwrap();
        let program = Program::new(
            Arc::from(format!("{}\n{VERTEX}", fragment.source)),
            Vec::new(),
        );
        let draw = ProgrammableDraw {
            instance_count: 1,
            vertex_count: 3,
            program,
            streams: Vec::new(),
            indices: None,
            vertex_constants: ConstantBlock::default().into(),
            pixel_constants: ConstantBlock::default().into(),
            textures: Vec::new(),
            parameters: [[0; 4]; 40],
        };
        gpu.draw_programmable(&target, &draw, &DrawState::new(16, 8))
            .unwrap();
        let mut readback = gpu.readback(&target.image, None).unwrap();
        if let Some(job) = readback.take_completion_job() {
            job();
        }
        let deadline = Instant::now() + Duration::from_secs(20);
        let pixels = loop {
            gpu.poll().unwrap();
            if let Some(pixels) = readback.poll() {
                break pixels.unwrap();
            }
            assert!(Instant::now() < deadline, "{name} readback");
            std::thread::sleep(Duration::from_millis(1));
        };
        let mut inputs = Inputs::default();
        let constants = Constants::new();
        for (index, pixel) in pixels.as_chunks::<4>().0.iter().enumerate() {
            let x = (index % 16) as f32 + 0.5;
            let y = (index / 16) as f32 + 0.5;
            inputs
                .registers
                .insert((Register::Input, 0), [x, y, 0.5, 1.0].map(f32::to_bits));
            let result = run(&module, &inputs, &constants, &mut NoTextures).unwrap();
            let expected = if result.discarded {
                [0, 0, 0, 0]
            } else {
                result.registers[&(Register::Output, 0)]
                    .map(|bits| (f32::from_bits(bits) * 255.0).round() as u8)
            };
            let rgba = [pixel[2], pixel[1], pixel[0], pixel[3]];
            assert!(
                rgba.iter().zip(expected).all(|(a, b)| a.abs_diff(b) <= 1),
                "{name} ({x}, {y}): {rgba:?} vs {expected:?}"
            );
        }
    }
}

#[test]
fn typed_uav_uses_the_bound_storage_format() {
    use adv_gpu::{
        Extent, Format,
        encode::{BufferCopy, ImageCopy},
        resource::{Dimension, ImageDesc, Usage},
    };
    let mut gpu = Compute::open();
    let module = module("typed_uav");
    let translation = wgsl::emit(
        &module,
        wgsl::Options {
            storage_formats: &[(1, naga::StorageFormat::R32Uint)],
        },
    )
    .unwrap();
    let shader = gpu
        .device
        .create_shader(ShaderDesc {
            source: Source::Wgsl(&translation.source),
            identity: 1,
            label: "typed UAV",
        })
        .unwrap();
    let pipeline = gpu
        .device
        .create_compute_pipeline(
            &ComputePipelineDesc {
                entry: shader
                    .entry(translation.entry_point, Stage::Compute)
                    .unwrap(),
                bindings: vec![Binding {
                    slot: translation.locations[0].binding,
                    kind: GpuBinding::StorageImage {
                        format: Format::R32Uint,
                        writable: true,
                    },
                    stages: Stages::COMPUTE,
                }],
                label: "typed UAV",
            },
            None,
        )
        .unwrap();
    let size = Extent {
        width: 8,
        height: 1,
        depth_or_layers: 1,
    };
    let image = gpu
        .device
        .create_image(&ImageDesc {
            dimension: Dimension::D2,
            size,
            format: Format::R32Uint,
            mips: 1,
            samples: 1,
            usage: Usage::STORAGE | Usage::COPY_SRC | Usage::COPY_DST,
            view_formats: &[],
            label: "typed UAV",
        })
        .unwrap();
    let upload = gpu.buffer(32, BufferUsage::COPY_SRC, Memory::Upload);
    let readback = gpu.buffer(32, BufferUsage::COPY_DST, Memory::Readback);
    let initial: [u32; 8] = [1, 3, 11, 53, 17, 19, 21, u32::MAX];
    // No submission references this upload mapping.
    let upload_bytes = unsafe { &mut *upload.mapped().unwrap().as_ptr() };
    upload_bytes.copy_from_slice(bytemuck::cast_slice(&initial));
    let mut encoder = gpu.queue.encoder("typed UAV");
    let source = || ImageCopy {
        image: &image,
        mip: 0,
        layer: 0,
        origin: (0, 0, 0),
    };
    encoder.copy_buffer_to_image(
        BufferCopy {
            buffer: &upload,
            offset: 0,
            row_pitch: 0,
            rows: 0,
        },
        source(),
        size,
    );
    {
        let mut pass = encoder.compute_pass(&ComputePassDesc {
            timestamps: None,
            label: "typed UAV",
        });
        pass.set_pipeline(&pipeline);
        pass.bind(1, Bind::StorageImage(image.view()));
        pass.dispatch(1, 1, 1);
    }
    encoder.copy_image_to_buffer(
        source(),
        BufferCopy {
            buffer: &readback,
            offset: 0,
            row_pitch: 0,
            rows: 0,
        },
        size,
    );
    let token = gpu.queue.submit(encoder).unwrap();
    assert!(gpu.queue.wait(token, Duration::from_secs(20)).unwrap());
    // The readback copy has completed.
    let bytes = unsafe { &*readback.mapped().unwrap().as_ptr() };
    let actual: Vec<_> = bytes
        .as_chunks::<4>()
        .0
        .iter()
        .copied()
        .map(u32::from_le_bytes)
        .collect();
    let expected = std::array::from_fn::<_, 8, _>(|i| initial[i].wrapping_add(i as u32));
    assert_eq!(actual, expected);
}
