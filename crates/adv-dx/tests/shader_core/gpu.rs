use super::module;
#[path = "gpu/alu.rs"]
mod alu;
#[path = "gpu/raster.rs"]
mod raster;
#[path = "gpu/resources.rs"]
mod resources;
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
    shared: [u32; 4],
}

impl Resources for Buffers<'_> {
    fn read(&mut self, access: ResourceAccess, arguments: &[Vector]) -> Result<Vector, ir::Error> {
        let data = match access.binding {
            (Register::Resource, 0) => self.input,
            (Register::UnorderedAccessView, 0) => &self.output,
            (Register::ThreadGroupSharedMemory, 0) => &self.shared,
            binding => panic!("unexpected read binding {binding:?}"),
        };
        let base = match access.instruction {
            Opcode::LdStructured => arguments[0][0] + arguments[1][0] / 4,
            Opcode::LdRaw => arguments[0][0] / 4,
            other => return Err(ir::Error::UnsupportedOpcode(other)),
        } as usize;
        Ok(std::array::from_fn(|lane| {
            data.get(base + lane).copied().unwrap_or(0)
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
        let data: &mut [u32] = match access.binding {
            (Register::UnorderedAccessView, 0) => &mut self.output,
            (Register::ThreadGroupSharedMemory, 0) => &mut self.shared,
            binding => panic!("unexpected write binding {binding:?}"),
        };
        let (base, value) = match access.instruction {
            Opcode::StoreStructured => (arguments[0][0] + arguments[1][0] / 4, arguments[2]),
            Opcode::StoreRaw => (arguments[0][0] / 4, arguments[1]),
            other => return Err(ir::Error::UnsupportedOpcode(other)),
        };
        for (lane, word) in value.into_iter().enumerate() {
            if mask & (1 << lane) != 0
                && let Some(slot) = data.get_mut(base as usize + lane)
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
        shared: [0; 4],
    };
    let constants = Constants::new();
    let mut inputs = Inputs::default();
    for invocation in 0..invocations {
        buffers.shared = [0; 4];
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
        ("branches", control.as_slice(), 8),
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
// spec: GFX-SHADER-5
fn opcode_fixtures_match_windows_and_scalar_ir() {
    let mut gpu = Compute::open();
    let input = [
        0, 0, 1, 0, 2, 1, 1, 2, 0xffffffff, 1, 1, 0xffffffff, 0x80000000, 2, 0xffffffff, 0xffffffff,
    ];
    let rows = include_str!("../fixtures/shader-core/encoded-values.tsv");
    for (name, outputs) in [
        ("encoded_alu", 56),
        ("encoded_call", 8),
        ("encoded_callc", 8),
        ("encoded_empty_opcode", 8),
        ("encoded_empty_operand", 8),
        ("encoded_flow", 8),
        ("encoded_min10", 8),
        ("encoded_non_uniform", 8),
        ("encoded_shared_raw", 8),
        ("min_precision", 24),
        ("swap_values", 16),
    ] {
        let module = module(name);
        let native = gpu.run(name, &module, &input, outputs, 8);
        let scalar = interpret_compute(&module, &input, outputs, 8);
        assert_eq!(native, scalar, "{name}");
        let mut measured = 0;
        for line in rows.lines() {
            let mut columns = line.split('\t');
            let (kind, driver, fixture) = (
                columns.next().unwrap(),
                columns.next().unwrap(),
                columns.next().unwrap(),
            );
            if kind != "encoded" || name != fixture {
                continue;
            }
            let expected: Vec<_> = columns
                .map(|value| u32::from_str_radix(value, 16).unwrap())
                .collect();
            assert_eq!(native, expected, "{name} ({driver})");
            measured += 1;
        }
        assert_ne!(measured, 0, "{name} has no Windows readback");
    }
}
