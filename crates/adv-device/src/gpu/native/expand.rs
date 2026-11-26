//! Expansion commands use consecutive chunks of a device-local ring. A chunk
//! is written by compute and consumed by indirect draws in submission order.

use super::modern::{
    self, BufferRange, Dispatch, DrawArguments, ModernDraw, SlotBindings, StorageBinding,
};
use super::{Error, Gpu};
use adv_gpu::encode::ComputePassDesc;
use adv_gpu::resource::{BufferDesc, BufferUsage, Memory};
use adv_gpu::{Buffer, Device, Encoder, Limits, RenderPipeline};
use std::ops::Range;

const CHUNK_BYTES: u64 = 4 * 1024 * 1024;
const RING_CHUNKS: u64 = 4;

/// Footprint of one region in each scratch chunk.
#[derive(Clone, Copy, Debug)]
pub struct ExpansionRegion {
    pub bytes_per_primitive: u32,
    pub fixed_bytes: u32,
    /// Byte stride of the storage binding's WGSL array.
    pub element_stride: u32,
}

impl ExpansionRegion {
    fn alignment(self, offset_alignment: u64) -> u64 {
        let stride = u64::from(self.element_stride);
        let (mut a, mut b) = (offset_alignment, stride);
        while b != 0 {
            (a, b) = (b, a % b);
        }
        offset_alignment / a * stride
    }
}

/// Slots in a shader reserved for expansion data.
#[derive(Clone, Copy, Debug)]
pub enum ExpansionBinding {
    Parameters { slot: u32 },
    Scratch { slot: u32, region: usize },
}

#[derive(Clone)]
pub struct ExpansionDispatch {
    pub dispatch: Dispatch,
    pub slots: SlotBindings,
    pub expansion: Vec<ExpansionBinding>,
    /// One invocation per primitive or a single invocation for the chunk.
    pub workgroup_size: Option<u32>,
    /// Upper bound on shader invocations per input primitive in the chunk.
    pub items_per_primitive: u32,
    /// Region and byte offset of the three dispatch groups for this program.
    pub indirect: Option<(usize, u32)>,
}

#[derive(Clone)]
pub struct ExpansionDraw {
    pub draw: ModernDraw,
    pub vertex: SlotBindings,
    pub fragment: SlotBindings,
    pub vertex_expansion: Vec<ExpansionBinding>,
    pub fragment_expansion: Vec<ExpansionBinding>,
    /// The region and byte offset of this bin's four indirect arguments.
    pub indirect: (usize, u32),
    /// Consecutive query indices are used for consecutive chunks.
    pub statistics: Option<modern::QuerySlot>,
}

/// Compute once before chunking (for example, one invocation per domain point).
#[derive(Clone)]
pub struct ExpansionPrelude {
    pub dispatch: Dispatch,
    pub slots: SlotBindings,
    pub groups: [u32; 3],
}

#[derive(Clone)]
pub struct Expand {
    pub primitives: u32,
    pub prelude: Option<ExpansionPrelude>,
    pub regions: Vec<ExpansionRegion>,
    pub programs: Vec<ExpansionDispatch>,
    pub bins: Vec<ExpansionDraw>,
}

impl Expand {
    pub fn primitives_per_chunk(&self, limits: &Limits) -> Result<u32, Error> {
        let alignment = u64::from(limits.storage_offset_alignment.max(4));
        let (fixed, stride) = self.regions.iter().fold((0, 0), |(fixed, stride), region| {
            (
                fixed + u64::from(region.fixed_bytes) + region.alignment(alignment) - 1,
                stride + u64::from(region.bytes_per_primitive),
            )
        });
        let available =
            CHUNK_BYTES.min(limits.max_buffer_size / RING_CHUNKS) / alignment * alignment;
        let count = if stride == 0 && fixed <= available {
            u64::from(u32::MAX)
        } else {
            available.saturating_sub(fixed) / stride.max(1)
        };
        // Bound one-dimensional dispatches by the D3D11/Vulkan guaranteed limit.
        let work_limit = self
            .programs
            .iter()
            .filter_map(|p| p.workgroup_size)
            .min()
            .map_or(u32::MAX, |size| size * 65_535);
        let count = (count as u32).min(work_limit);
        if count == 0 {
            return Err(Error::Invalid(
                "one primitive exceeds the expansion chunk".into(),
            ));
        }
        Ok(count)
    }
}

impl Gpu {
    pub fn expansion_storage(&self, size: u64) -> Result<BufferRange, Error> {
        let buffer = self
            .device
            .create_buffer(&BufferDesc {
                size,
                usage: BufferUsage::STORAGE,
                memory: Memory::Device,
                label: "domain point results",
            })
            .map_err(super::resources::gpu_error)?;
        Ok(BufferRange {
            buffer,
            range: 0..size,
        })
    }

    /// Record expansion at the current position of the modern command stream.
    pub fn expand(&mut self, plan: Expand) -> Result<(), Error> {
        if plan.primitives == 0 {
            return Ok(());
        }
        let per_chunk = plan.primitives_per_chunk(&self.device.caps().limits)?;
        self.enqueue_modern(super::record::WorkerItem::Expand {
            facade: self.facade_id,
            plan: Box::new(plan),
            per_chunk,
        })
    }
}

pub(crate) struct Work {
    pub plan: Expand,
    pub per_chunk: u32,
    pub pipelines: Vec<RenderPipeline>,
}

#[derive(Default)]
pub(super) struct Ring {
    buffer: Option<Buffer>,
    next: u64,
}

impl Ring {
    pub fn begin_frame(&mut self) {
        self.next = 0;
    }

    pub fn encode(
        &mut self,
        device: &Device,
        encoder: &mut Encoder,
        work: Work,
    ) -> adv_gpu::Result<()> {
        let Work {
            mut plan,
            per_chunk,
            pipelines,
        } = work;
        if let Some(prelude) = &plan.prelude {
            let bindings = prelude
                .dispatch
                .shader
                .compact(&prelude.slots)
                .map_err(invalid)?;
            let mut pass = encoder.compute_pass(&ComputePassDesc {
                timestamps: None,
                label: "domain expansion",
            });
            pass.set_pipeline(&prelude.dispatch.pipeline);
            for binding in &bindings {
                pass.bind(binding.slot, binding.binding());
            }
            let [x, y, z] = prelude.groups;
            pass.dispatch(x, y, z);
        }
        let limits = &device.caps().limits;
        let align = u64::from(limits.storage_offset_alignment.max(4));
        let chunk_bytes = CHUNK_BYTES.min(limits.max_buffer_size / RING_CHUNKS) / align * align;
        if self.buffer.is_none() {
            self.buffer = Some(device.create_buffer(&BufferDesc {
                size: chunk_bytes * RING_CHUNKS,
                usage: BufferUsage::STORAGE | BufferUsage::INDIRECT,
                memory: Memory::Device,
                label: "expansion ring",
            })?);
        }
        let scratch = self.buffer.as_ref().unwrap();
        let chunk_count = plan.primitives.div_ceil(per_chunk);
        let param_stride = u64::from(limits.uniform_offset_alignment.max(16));
        let parameters = device.create_buffer(&BufferDesc {
            size: u64::from(chunk_count) * param_stride,
            usage: BufferUsage::UNIFORM,
            memory: Memory::Upload,
            label: "expansion parameters",
        })?;
        let mut regions = Vec::with_capacity(plan.regions.len());
        for chunk in 0..chunk_count {
            let start = chunk * per_chunk;
            let count = per_chunk.min(plan.primitives - start);
            let offset = u64::from(chunk) * param_stride;
            let words = [start, count, plan.primitives, 0];
            // Each chunk has a distinct immutable uniform range for this submission.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    words.as_ptr().cast::<u8>(),
                    parameters
                        .mapped()
                        .unwrap()
                        .as_ptr()
                        .cast::<u8>()
                        .add(offset as usize),
                    16,
                );
            }
            let parameters = BufferRange {
                buffer: parameters.clone(),
                range: offset..offset + 16,
            };
            let mut cursor = self.next * chunk_bytes;
            self.next = (self.next + 1) % RING_CHUNKS;
            regions.clear();
            for region in &plan.regions {
                let alignment = region.alignment(align);
                let start = cursor.div_ceil(alignment) * alignment;
                cursor = start
                    + u64::from(region.fixed_bytes)
                    + u64::from(count) * u64::from(region.bytes_per_primitive);
                regions.push(start..cursor);
            }

            for program in &mut plan.programs {
                bind(
                    &mut program.slots,
                    &program.expansion,
                    scratch,
                    &regions,
                    &parameters,
                );
                let bindings = program
                    .dispatch
                    .shader
                    .compact(&program.slots)
                    .map_err(invalid)?;
                let mut pass = encoder.compute_pass(&ComputePassDesc {
                    timestamps: None,
                    label: "expansion",
                });
                pass.set_pipeline(&program.dispatch.pipeline);
                for binding in &bindings {
                    pass.bind(binding.slot, binding.binding());
                }
                if let Some((region, offset)) = program.indirect {
                    let at = regions[region].start + u64::from(offset);
                    pass.dispatch_indirect(scratch.slice(at..at + 12));
                } else {
                    pass.dispatch(
                        program.workgroup_size.map_or(1, |size| {
                            (count * program.items_per_primitive).div_ceil(size)
                        }),
                        1,
                        1,
                    );
                }
            }

            for (bin, pipeline) in plan.bins.iter_mut().zip(&pipelines) {
                bind(
                    &mut bin.vertex,
                    &bin.vertex_expansion,
                    scratch,
                    &regions,
                    &parameters,
                );
                bind(
                    &mut bin.fragment,
                    &bin.fragment_expansion,
                    scratch,
                    &regions,
                    &parameters,
                );
                let vertex = bin.draw.vertex.compact(&bin.vertex).map_err(invalid)?;
                let fragment = bin
                    .draw
                    .fragment
                    .as_ref()
                    .map(|shader| shader.compact(&bin.fragment))
                    .transpose()
                    .map_err(invalid)?
                    .unwrap_or_default();
                let indirect_offset = regions[bin.indirect.0].start + u64::from(bin.indirect.1);
                let args = DrawArguments::Indirect(BufferRange {
                    buffer: scratch.clone(),
                    range: indirect_offset..indirect_offset + 16,
                });
                let statistics = bin.statistics.as_ref().map(|slot| modern::QuerySlot {
                    set: slot.set.clone(),
                    index: slot.index + chunk,
                });
                super::owner::draw_modern_pass(
                    encoder,
                    (&bin.draw, pipeline),
                    (&vertex, &fragment),
                    &args,
                    statistics.as_ref(),
                    (chunk == 0, chunk + 1 == chunk_count),
                );
            }
        }
        Ok(())
    }
}

fn bind(
    slots: &mut SlotBindings,
    expansion: &[ExpansionBinding],
    scratch: &Buffer,
    regions: &[Range<u64>],
    parameters: &BufferRange,
) {
    for binding in expansion {
        match *binding {
            ExpansionBinding::Parameters { slot } => {
                set(&mut slots.uniforms, slot, parameters.clone());
            }
            ExpansionBinding::Scratch { slot, region } => {
                set(
                    &mut slots.storage,
                    slot,
                    StorageBinding::Buffer(BufferRange {
                        buffer: scratch.clone(),
                        range: regions[region].clone(),
                    }),
                );
            }
        }
    }
}

fn set<T>(slots: &mut Vec<Option<T>>, slot: u32, value: T) {
    let slot = slot as usize;
    if slots.len() <= slot {
        slots.resize_with(slot + 1, || None);
    }
    slots[slot] = Some(value);
}

fn invalid(error: Error) -> adv_gpu::Error {
    adv_gpu::Error::Invalid(error.to_string())
}
