use super::*;
use std::collections::BTreeMap;

#[derive(Default)]
struct Views {
    sampled: Vec<u32>,
    storage: BTreeMap<u32, Vec<u32>>,
    counters: BTreeMap<u32, u32>,
    packed: bool,
}

impl Resources for Views {
    fn read(&mut self, access: ResourceAccess, arguments: &[Vector]) -> Result<Vector, ir::Error> {
        let (register, slot) = access.binding;
        let index = arguments.first().map_or(0, |args| args[0] as usize);
        match (access.instruction, register) {
            (Opcode::Bufinfo, Register::Resource) => Ok([(self.sampled.len() / 2) as u32; 4]),
            (Opcode::Ld, Register::Resource) if self.packed => {
                Ok([(self.sampled[0] >> (index * 8)) & 0xff, 0, 0, 1])
            }
            (Opcode::Ld, Register::Resource) => {
                let x = self.sampled.get(2 * index).copied().unwrap_or(0);
                let y = self.sampled.get(2 * index + 1).copied().unwrap_or(0);
                Ok([x, y, 0, 1])
            }
            (Opcode::LdStructured, Register::Resource) => Ok(std::array::from_fn(|lane| {
                self.sampled.get(index + lane).copied().unwrap_or(0)
            })),
            (Opcode::LdStructured | Opcode::LdUavTyped, Register::UnorderedAccessView) => {
                let x = self.storage[&slot].get(index).copied().unwrap_or(0);
                Ok([x, 0, 0, 1])
            }
            (op, _) => Err(ir::Error::UnsupportedOpcode(op)),
        }
    }

    fn atomic(&mut self, access: ResourceAccess, args: &[Vector]) -> Result<Vector, ir::Error> {
        let (_, slot) = access.binding;
        let result = match access.instruction {
            Opcode::ImmAtomicAlloc => {
                let counter = self.counters.get_mut(&slot).unwrap();
                let old = *counter;
                *counter = old.wrapping_add(1);
                old
            }
            Opcode::ImmAtomicConsume => {
                let counter = self.counters.get_mut(&slot).unwrap();
                *counter = counter.wrapping_sub(1);
                *counter
            }
            op => {
                let word = &mut self.storage.get_mut(&slot).unwrap()[args[0][0] as usize];
                let old = *word;
                let rhs = args[1][0];
                *word = match op {
                    Opcode::AtomicIadd | Opcode::ImmAtomicIadd => old.wrapping_add(rhs),
                    Opcode::AtomicAnd | Opcode::ImmAtomicAnd => old & rhs,
                    Opcode::AtomicOr | Opcode::ImmAtomicOr => old | rhs,
                    Opcode::AtomicXor | Opcode::ImmAtomicXor => old ^ rhs,
                    Opcode::AtomicUmin | Opcode::ImmAtomicUmin => old.min(rhs),
                    Opcode::AtomicUmax | Opcode::ImmAtomicUmax => old.max(rhs),
                    Opcode::AtomicImin | Opcode::ImmAtomicImin => {
                        (old as i32).min(rhs as i32) as u32
                    }
                    Opcode::AtomicImax | Opcode::ImmAtomicImax => {
                        (old as i32).max(rhs as i32) as u32
                    }
                    Opcode::AtomicCmpStore => {
                        if old == rhs {
                            args[2][0]
                        } else {
                            old
                        }
                    }
                    Opcode::ImmAtomicExch => rhs,
                    op => return Err(ir::Error::UnsupportedOpcode(op)),
                };
                old
            }
        };
        Ok([result; 4])
    }

    fn write(&mut self, access: ResourceAccess, _: u8, args: &[Vector]) -> Result<(), ir::Error> {
        assert!(matches!(
            access.instruction,
            Opcode::StoreStructured | Opcode::StoreUavTyped
        ));
        let storage = self.storage.get_mut(&access.binding.1).unwrap();
        let index = args[0][0] as usize;
        let value = args.last().unwrap()[0];
        if self.packed {
            let shift = index * 8;
            storage[0] = (storage[0] & !(0xff << shift)) | (value << shift);
        } else {
            storage[index] = value;
        }
        Ok(())
    }
}

impl Views {
    fn execute(&mut self, shader: &ir::Module, invocations: u32) {
        for invocation in 0..invocations {
            let inputs = Inputs {
                registers: [((Register::InputThreadId, 0), [invocation, 0, 0, 0])].into(),
            };
            run(shader, &inputs, &Constants::new(), self).unwrap();
        }
    }
}

fn compare_buffer_views(
    name: &'static str,
    mut views: Views,
    invocations: u32,
    options: wgsl::Options<'_>,
) {
    let mut gpu = Compute::open();
    let module = module(name);
    let translation = wgsl::emit(&module, options).unwrap();
    let shader = gpu
        .device
        .create_shader(ShaderDesc {
            source: Source::Wgsl(&translation.source),
            identity: 1,
            label: name,
        })
        .unwrap_or_else(|err| panic!("{err}\n{}", translation.source));
    let declared = translation.locations.iter().map(|location| {
        let storage = location.register == Register::UnorderedAccessView;
        (
            location.binding,
            storage,
            if storage {
                views.storage[&location.slot].as_slice()
            } else {
                assert_eq!(location.register, Register::Resource);
                views.sampled.as_slice()
            },
        )
    });
    let counters = translation
        .counters
        .iter()
        .map(|(slot, binding)| (*binding, true, std::slice::from_ref(&views.counters[slot])));
    let slots = declared.chain(counters).collect::<Vec<_>>();
    let bindings = slots
        .iter()
        .map(|&(slot, writable, _)| Binding {
            slot,
            kind: GpuBinding::StorageBuffer { writable },
            stages: Stages::COMPUTE,
        })
        .collect();
    let pipeline = gpu
        .device
        .create_compute_pipeline(
            &ComputePipelineDesc {
                entry: shader
                    .entry(translation.entry_point, Stage::Compute)
                    .unwrap(),
                bindings,
                label: name,
            },
            None,
        )
        .unwrap();
    let buffers: BTreeMap<_, _> = slots
        .iter()
        .map(|&(slot, writable, words)| {
            let size = std::mem::size_of_val(words) as u64;
            let input = gpu.buffer(
                size,
                BufferUsage::STORAGE | BufferUsage::COPY_SRC,
                Memory::Upload,
            );
            // No submission references this mapping yet.
            unsafe { &mut *input.mapped().unwrap().as_ptr() }
                .copy_from_slice(bytemuck::cast_slice(words));
            let output =
                writable.then(|| gpu.buffer(size, BufferUsage::COPY_DST, Memory::Readback));
            (slot, (input, output, size))
        })
        .collect();
    let mut encoder = gpu.queue.encoder(name);
    {
        let mut pass = encoder.compute_pass(&ComputePassDesc {
            timestamps: None,
            label: name,
        });
        pass.set_pipeline(&pipeline);
        for (&slot, (buffer, _, size)) in &buffers {
            pass.bind(slot, Bind::StorageBuffer(buffer.slice(0..*size)));
        }
        pass.dispatch(1, 1, 1);
    }
    for (buffer, readback, size) in buffers.values() {
        if let Some(readback) = readback {
            encoder.copy_buffer(buffer.slice(0..*size), readback, 0);
        }
    }
    let token = gpu.queue.submit(encoder).unwrap();
    assert!(gpu.queue.wait(token, Duration::from_secs(20)).unwrap());
    views.execute(&module, invocations);
    let words = |slot| {
        let readback = buffers[&slot].1.as_ref().unwrap();
        // The submission writing this readback has completed.
        bytemuck::cast_slice::<_, u32>(unsafe { &*readback.mapped().unwrap().as_ptr() })
    };
    for location in translation.locations {
        if location.register == Register::UnorderedAccessView {
            assert_eq!(
                words(location.binding),
                views.storage[&location.slot],
                "{name}"
            );
        }
    }
    for (slot, binding) in translation.counters {
        assert_eq!(
            words(binding),
            [views.counters[&slot]],
            "{name} counter {slot}"
        );
    }
}

#[test]
fn typed_buffers_and_buffer_backed_atomics_match_scalar_ir() {
    compare_buffer_views(
        "buffer_views",
        Views {
            sampled: vec![20, 7, 42, 100],
            storage: [(0, vec![0; 3])].into(),
            ..Default::default()
        },
        1,
        wgsl::Options {
            storage_formats: &[(0, naga::StorageFormat::R32Uint)],
            buffer_formats: &[(0, naga::StorageFormat::Rg32Uint)],
            ..Default::default()
        },
    );
    for name in ["texture_atomic", "texture_atomic_return"] {
        compare_buffer_views(
            name,
            Views {
                storage: [(1, vec![1, 3, 5, 9, 13, 33, 100, u32::MAX])].into(),
                ..Default::default()
            },
            8,
            wgsl::Options {
                storage_formats: &[(1, naga::StorageFormat::R32Uint)],
                storage_extents: &[(1, [8, 1, 1, 8])],
                ..Default::default()
            },
        );
    }
}

#[test]
fn hidden_uav_counters_match_scalar_ir() {
    compare_buffer_views(
        "counters",
        Views {
            storage: [
                (1, vec![93, 0, 0, 0]),
                (2, vec![3, 5, 7, 0]),
                (3, vec![10, 11, 0, 0]),
            ]
            .into(),
            counters: [(1, 1), (2, 3), (3, 2)].into(),
            ..Default::default()
        },
        1,
        Default::default(),
    );
}

fn compare_image(name: &'static str, compare_atomics: bool) {
    use adv_gpu::{
        Extent, Format,
        encode::{BufferCopy, ImageCopy},
        resource::{Dimension, ImageDesc, Usage},
    };
    let mut gpu = Compute::open();
    let module = module(name);
    let translation = wgsl::emit(
        &module,
        wgsl::Options {
            storage_formats: &[(1, naga::StorageFormat::R32Uint)],
            image_atomics: compare_atomics,
            ..Default::default()
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
        .unwrap_or_else(|error| panic!("{name}: {error}"));
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
    let mut views = Views {
        storage: [(1, initial.to_vec())].into(),
        ..Default::default()
    };
    views.execute(&module, 8);
    assert_eq!(actual, views.storage[&1]);
}

#[test]
fn typed_uav_uses_the_bound_storage_format() {
    compare_image("typed_uav", false);
}

#[test]
fn native_texture_atomics_match_scalar_ir() {
    compare_image("texture_atomic", true);
    compare_image("texture_atomic_return", true);
}

#[test]
// spec: GFX-NUM-1
fn packed_views_and_atomic_modes_match_scalar_ir() {
    compare_buffer_views(
        "packed_buffer",
        Views {
            sampled: vec![0x000003fa],
            storage: [(0, vec![0x65191919])].into(),
            packed: true,
            ..Default::default()
        },
        2,
        wgsl::Options {
            storage_formats: &[(0, naga::StorageFormat::R8Uint)],
            buffer_formats: &[(0, naga::StorageFormat::R8Uint)],
            buffer_elements: &[
                (Register::Resource, 0, 2),
                (Register::UnorderedAccessView, 0, 2),
            ],
            ..Default::default()
        },
    );
    compare_buffer_views(
        "branches",
        Views {
            sampled: vec![0, 1, 2, 3, 4, 5, 6, 7],
            storage: [(0, vec![0; 8])].into(),
            ..Default::default()
        },
        8,
        Default::default(),
    );
    compare_buffer_views(
        "atomic_modes",
        Views {
            sampled: vec![0xffffffe3],
            storage: [(0, vec![3; 16]), (1, vec![7])].into(),
            ..Default::default()
        },
        1,
        Default::default(),
    );
}
