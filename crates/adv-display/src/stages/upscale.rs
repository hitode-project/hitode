//! The upscale stage: the shipped models' compute layers over private
//! tensors, the final layer writing the doubled image into a presentable
//! buffer (HOST-VIEW-7, HOST-VIEW-10).
pub mod models;
mod table;

pub use table::{Model, OUTPUT, Pass, Role};

/// The format of the model's input, which composition writes.
pub const INPUT: Format = Format::Rgba16Float;

/// One precision of a precompiled upscale shader.
struct Compiled {
    spirv: &'static [u32],
    metal: &'static [u8],
    identity: u64,
    entries: &'static [adv_gpu::shader::PrecompiledEntry<'static>],
}

/// Each shader path with its half- and single-precision forms; models without
/// FP16 have only the latter.
static SHADERS: &[(&str, Option<Compiled>, Option<Compiled>)] =
    include!(concat!(env!("OUT_DIR"), "/upscale.rs"));

use crate::Rect;
use crate::plan::Upscaler;
use adv_gpu::encode::{Bind, ComputePassDesc};
use adv_gpu::pipeline::ComputePipelineDesc;
use adv_gpu::resource::{
    Address, BorderColor, BufferDesc, BufferUsage, Dimension, Filter, ImageDesc, Memory,
    SamplerDesc, Usage,
};
use adv_gpu::shader::{ShaderDesc, Source, Stage};
use adv_gpu::{
    Buffer, ComputePipeline, Device, Encoder, Extent, Features, Format, Image, ImageView, Ring,
    Sampler,
};
use std::cell::Cell;

/// Workgroups per dispatch, below the portable 65535 per dimension. Larger
/// lists split into dispatches over aligned slices.
const DISPATCH: u32 = 65535;

/// The selected model's tables, in `Upscaler` order.
fn index(choice: Upscaler) -> usize {
    match choice {
        Upscaler::Anime4k => 0,
        Upscaler::Cunny => 1,
        Upscaler::Artcnn => 2,
        Upscaler::K7 => 3,
        Upscaler::Off => unreachable!("no model runs when upscaling is off"),
    }
}

/// The model a device runs: matrix input convolutions where it runs half
/// precision and has 8×8 matrices of it, the fused layers otherwise.
pub fn model(device: &Device, choice: Upscaler, half: bool) -> Model {
    let matrix = Features::MATRIX_F16_8X8 | Features::SHADER_F16 | Features::SUBGROUP;
    if half && device.caps().features.contains(matrix) {
        models::MATRIX[index(choice)]
    } else {
        models::FUSED[index(choice)]
    }
}

/// Input pixels a change reaches in the output of `choice`. Every variant of
/// a model keeps its receptive field.
pub fn reach(choice: Upscaler) -> u32 {
    models::REFERENCE[index(choice)].reach()
}

impl Model {
    /// Input-pixel radius bounding a change in the output. Reach follows the data:
    /// a pass's outputs reach its radius beyond the farthest input it reads,
    /// so passes of one layer add theirs once. The final layer's bilinear
    /// sample of the input reaches one pixel beside that chain.
    pub fn reach(&self) -> u32 {
        let mut tensors = vec![0; self.tensors.len()];
        let mut output = 0;
        for pass in self.passes {
            let reach = pass.reads().map(|t| tensors[t]).max().unwrap_or(0) + pass.radius;
            for written in pass.writes() {
                match written {
                    Some(t) => tensors[t] = reach,
                    None => output = reach,
                }
            }
        }
        output.max(1)
    }

    /// A cell-to-delta distance bounding the backwards dependency closure.
    /// Rounding a later reader adds at most unit-1 pixels on either side.
    /// Taking the largest branch also covers shared tensor storage.
    fn filter_reaches(&self) -> Vec<u32> {
        let output = self.tensors.len();
        let mut needed = vec![0; output + 1];
        needed[output] = self.reach();
        let mut reaches = vec![0; self.passes.len()];
        for (i, pass) in self.passes.iter().enumerate().rev() {
            let reach = pass
                .writes()
                .map(|t| std::mem::take(&mut needed[t.unwrap_or(output)]))
                .max()
                .unwrap_or(0);
            reaches[i] = reach;
            for t in pass.reads() {
                needed[t] = needed[t].max(reach + pass.unit() - 1 + pass.radius);
            }
        }
        reaches
    }
}

impl Pass {
    /// Input pixels per workgroup cell along each axis.
    fn unit(&self) -> u32 {
        self.block / self.scale
    }

    /// The tensors this pass reads.
    fn reads(&self) -> impl Iterator<Item = usize> {
        self.bindings.iter().filter_map(|(_, role)| match role {
            Role::Tensor(t) => Some(*t),
            _ => None,
        })
    }

    /// The tensors this pass writes, `None` for the output.
    fn writes(&self) -> impl Iterator<Item = Option<usize>> {
        self.bindings.iter().filter_map(|(_, role)| match role {
            Role::Store(t) => Some(Some(*t)),
            Role::Output => Some(None),
            _ => None,
        })
    }
}

/// One pass's cells: columns and rows of its grid, and its whole-grid list.
struct Grid {
    columns: u32,
    rows: u32,
    whole: usize,
    compacted: Buffer,
    parameters: Buffer,
    /// First aligned indirect record for this pass.
    arguments: u64,
}

/// One model at one canvas size. Only the selected model is resident.
pub struct Upscaling {
    model: Model,
    size: (u32, u32),
    /// The last encoded composition, in half precision.
    input: Image,
    scratch: Image,
    initialized: Cell<bool>,
    delta: Buffer,
    delta_dimensions: Buffer,
    compare: ComputePipeline,
    initialize: ComputePipeline,
    filter: ComputePipeline,
    /// Filter and model indirect records, one per CPU-list slice.
    gates: Buffer,
    arguments: Buffer,
    argument_stride: u64,
    source_whole: usize,
    tensors: Vec<Image>,
    pipelines: Vec<ComputePipeline>,
    nearest: Sampler,
    linear: Sampler,
    /// Canvas and doubled dimensions, the uniform of every pass.
    dimensions: Buffer,
    grids: Vec<Grid>,
    /// Every cell of each distinct grid, so whole runs upload nothing.
    whole: Vec<Buffer>,
    /// Storage offset alignment, in list entries.
    align: u32,
}

/// One pass's pipeline, in half precision where `half` and the pass has that
/// form.
fn pipeline(
    device: &Device,
    model: &Model,
    pass: &Pass,
    half: bool,
) -> adv_gpu::Result<ComputePipeline> {
    let (_, h, s) = SHADERS
        .iter()
        .find(|(path, ..)| *path == pass.shader)
        .expect("every table shader is precompiled");
    let compiled = if half {
        h.as_ref().or(s.as_ref())
    } else {
        s.as_ref()
    }
    .expect("a single-precision form exists");
    let shader = device.create_shader(ShaderDesc {
        source: Source::Precompiled {
            spirv: compiled.spirv,
            metal: compiled.metal,
            entries: compiled.entries,
        },
        identity: compiled.identity,
        label: "upscale pass",
    })?;
    device.create_compute_pipeline(
        &ComputePipelineDesc {
            entry: shader.entry("main", Stage::Compute)?,
            bindings: pass.layout(model),
            label: "upscale pass",
        },
        None,
    )
}

impl Upscaling {
    /// Blocking pipeline creation; run it on the compile pool.
    pub fn new(device: &Device, model: Model, size: (u32, u32)) -> adv_gpu::Result<Self> {
        let half = device.caps().features.contains(Features::SHADER_F16);
        Self::with_precision(device, model, size, half)
    }

    /// As [`Self::new`], in half precision only where `half` and the model
    /// declares it.
    pub fn with_precision(
        device: &Device,
        model: Model,
        size: (u32, u32),
        half: bool,
    ) -> adv_gpu::Result<Self> {
        let max = device.caps().limits.max_image_2d;
        if size.0 == 0 || size.1 == 0 || size.0 > max / 2 || size.1 > max / 2 {
            return Err(adv_gpu::Error::Invalid(
                "upscaled canvas exceeds the image limit".into(),
            ));
        }
        let image = |format, usage, label| {
            device.create_image(&ImageDesc {
                dimension: Dimension::D2,
                size: Extent {
                    width: size.0,
                    height: size.1,
                    depth_or_layers: 1,
                },
                format,
                mips: 1,
                samples: 1,
                usage,
                view_formats: &[],
                label,
            })
        };
        let input = image(
            INPUT,
            Usage::SAMPLED | Usage::COLOR | Usage::COPY_DST | Usage::STORAGE,
            "upscale input",
        )?;
        let scratch = image(
            INPUT,
            Usage::SAMPLED | Usage::COLOR | Usage::COPY_DST,
            "upscale scratch",
        )?;
        let tensors = model
            .tensors
            .iter()
            .map(|&format| image(format, Usage::SAMPLED | Usage::STORAGE, "upscale tensor"))
            .collect::<adv_gpu::Result<_>>()?;
        let pipelines = model
            .passes
            .iter()
            .map(|pass| pipeline(device, &model, pass, half))
            .collect::<adv_gpu::Result<_>>()?;
        let sampler = |filter| {
            device.sampler(&SamplerDesc {
                min: filter,
                mag: filter,
                mip: Filter::Nearest,
                address: [Address::Clamp; 3],
                border: BorderColor::TransparentBlack,
                lod_bias: 0,
                lod_min: 0,
                lod_max: 0,
                anisotropy: 1,
                compare: None,
            })
        };
        let upload = |bytes: &[u8], usage, label| {
            let buffer = device.create_buffer(&BufferDesc {
                size: bytes.len() as u64,
                usage,
                memory: Memory::Upload,
                label,
            })?;
            // SAFETY: the buffer is new and no GPU work refers to it yet.
            let mapped = unsafe { buffer.mapped().expect("upload memory").as_mut() };
            mapped[..bytes.len()].copy_from_slice(bytes);
            Ok::<_, adv_gpu::Error>(buffer)
        };
        let dimensions = upload(
            &words([size.0, size.1, size.0 * 2, size.1 * 2]),
            BufferUsage::UNIFORM,
            "upscale dimensions",
        )?;
        let align = device.caps().limits.storage_offset_alignment.max(4) / 4;
        let chunk = DISPATCH / align * align;
        let argument_stride = u64::from(align * 4).max(16);
        let reaches = model.filter_reaches();
        let mut argument_bytes = 0;
        let mut grids = Vec::new();
        let mut shapes: Vec<(u32, u32)> = Vec::new();
        let mut whole = Vec::new();
        for (pass, reach) in model.passes.iter().zip(reaches) {
            let shape = (size.0.div_ceil(pass.unit()), size.1.div_ceil(pass.unit()));
            let index = match shapes.iter().position(|&s| s == shape) {
                Some(index) => index,
                None => {
                    let cells: Vec<u32> = (0..shape.1)
                        .flat_map(|y| (0..shape.0).map(move |x| x | y << 16))
                        .collect();
                    whole.push(upload(
                        &words(cells),
                        BufferUsage::STORAGE,
                        "upscale cells",
                    )?);
                    shapes.push(shape);
                    shapes.len() - 1
                }
            };
            let count = shape.0 * shape.1;
            grids.push(Grid {
                columns: shape.0,
                rows: shape.1,
                whole: index,
                compacted: device.create_buffer(&BufferDesc {
                    size: u64::from(count) * 4,
                    usage: BufferUsage::STORAGE | BufferUsage::COPY_SRC,
                    memory: Memory::Device,
                    label: "upscale compacted cells",
                })?,
                parameters: upload(
                    &words([size.0, size.1, pass.unit(), reach]),
                    BufferUsage::UNIFORM,
                    "upscale filter parameters",
                )?,
                arguments: argument_bytes,
            });
            argument_bytes += u64::from(count.div_ceil(chunk)) * argument_stride;
        }
        let source_shape = (size.0.div_ceil(8), size.1.div_ceil(8));
        let source_whole = if let Some(i) = shapes.iter().position(|&s| s == source_shape) {
            i
        } else {
            whole.push(upload(
                &words(
                    (0..source_shape.1).flat_map(|y| (0..source_shape.0).map(move |x| x | y << 16)),
                ),
                BufferUsage::STORAGE,
                "upscale source cells",
            )?);
            whole.len() - 1
        };
        let buffer = |bytes, usage, label| {
            device.create_buffer(&BufferDesc {
                size: bytes,
                usage,
                memory: Memory::Device,
                label,
            })
        };
        let delta = buffer(
            u64::from(source_shape.0.div_ceil(32) * source_shape.1 + 1) * 4,
            BufferUsage::STORAGE | BufferUsage::COPY_DST,
            "upscale delta",
        )?;
        let delta_dimensions = upload(
            &words([
                size.0,
                size.1,
                argument_bytes as u32 / 4,
                argument_stride as u32 / 4,
            ]),
            BufferUsage::UNIFORM,
            "upscale delta dimensions",
        )?;
        let delta_shader = device.create_shader(ShaderDesc {
            source: Source::Precompiled {
                spirv: &include!(concat!(env!("OUT_DIR"), "/upscale_delta.spv.rs")),
                metal: include_bytes!(concat!(env!("OUT_DIR"), "/upscale_delta.msl")),
                entries: &include!(concat!(env!("OUT_DIR"), "/upscale_delta.entries.rs")),
            },
            identity: include!(concat!(env!("OUT_DIR"), "/upscale_delta.identity.rs")),
            label: "upscale delta",
        })?;
        let delta_pipeline = |entry| {
            device.create_compute_pipeline(
                &ComputePipelineDesc {
                    entry: delta_shader.entry(entry, Stage::Compute)?,
                    bindings: super::bindings::UPSCALE_DELTA.to_vec(),
                    label: "upscale delta",
                },
                None,
            )
        };
        let filter_shader = device.create_shader(ShaderDesc {
            source: Source::Precompiled {
                spirv: &include!(concat!(env!("OUT_DIR"), "/upscale_filter.spv.rs")),
                metal: include_bytes!(concat!(env!("OUT_DIR"), "/upscale_filter.msl")),
                entries: &include!(concat!(env!("OUT_DIR"), "/upscale_filter.entries.rs")),
            },
            identity: include!(concat!(env!("OUT_DIR"), "/upscale_filter.identity.rs")),
            label: "upscale filter",
        })?;
        let filter = device.create_compute_pipeline(
            &ComputePipelineDesc {
                entry: filter_shader.entry("main", Stage::Compute)?,
                bindings: super::bindings::UPSCALE_FILTER.to_vec(),
                label: "upscale filter",
            },
            None,
        )?;
        Ok(Self {
            model,
            scratch,
            initialized: Cell::new(false),
            delta,
            delta_dimensions,
            compare: delta_pipeline("compare")?,
            initialize: delta_pipeline("initialize")?,
            filter,
            gates: buffer(
                argument_bytes,
                BufferUsage::STORAGE
                    | BufferUsage::INDIRECT
                    | BufferUsage::COPY_SRC
                    | BufferUsage::COPY_DST,
                "upscale filter arguments",
            )?,
            arguments: buffer(
                argument_bytes,
                BufferUsage::STORAGE
                    | BufferUsage::INDIRECT
                    | BufferUsage::COPY_SRC
                    | BufferUsage::COPY_DST,
                "upscale model arguments",
            )?,
            argument_stride,
            source_whole,
            size,
            input,
            tensors,
            pipelines,
            nearest: sampler(Filter::Nearest)?,
            linear: sampler(Filter::Linear)?,
            dimensions,
            grids,
            whole,
            align,
        })
    }

    pub fn size(&self) -> (u32, u32) {
        self.size
    }
    pub fn model(&self) -> &Model {
        &self.model
    }
    /// Composition target: the persistent input for exact damage, otherwise
    /// scratch for comparison against the last encoded input.
    pub fn input(&self, exact: bool) -> &Image {
        if exact { &self.input } else { &self.scratch }
    }

    /// Update after composition within `damage`, or the whole canvas. Exact
    /// damage writes the input directly; conservative damage writes scratch.
    /// Filters compact the CPU lists against GPU change cells (HOST-VIEW-10).
    /// Returns the last pass's CPU-list pixels, an upper bound on recomputed
    /// input pixels. `Busy` leaves the encoder untouched.
    pub fn encode(
        &self,
        encoder: &mut Encoder,
        ring: &mut Ring,
        output: &ImageView,
        damage: Option<&[Rect]>,
        exact: bool,
        timestamps: Option<(&adv_gpu::queue::QuerySet, u32)>,
    ) -> adv_gpu::Result<u64> {
        let passes = self.model.passes;
        let lists = match damage {
            Some(damage) => self.cells(damage),
            None => vec![None; passes.len()],
        };
        if lists.iter().any(|l| l.as_ref().is_some_and(Vec::is_empty)) {
            return Ok(0);
        }
        let source = if exact {
            None
        } else {
            damage.and_then(|rects| {
                let mut region = adv_gpu::Region::empty(self.size);
                for &rect in rects {
                    region.add(rect.into());
                }
                if region.is_whole() {
                    return None;
                }
                Some(
                    region
                        .rects()
                        .into_iter()
                        .flat_map(|r| {
                            (r.y / 8..(r.y + r.h).div_ceil(8)).flat_map(move |y| {
                                (r.x / 8..(r.x + r.w).div_ceil(8)).map(move |x| x | y << 16)
                            })
                        })
                        .collect::<Vec<_>>(),
                )
            })
        };
        // Reserve all uploads before recording any commands.
        let padded = |l: &Vec<u32>| (l.len() as u64).next_multiple_of(u64::from(self.align));
        let entries: u64 =
            lists.iter().flatten().map(padded).sum::<u64>() + source.as_ref().map_or(0, padded);
        let gate_start = entries * 4;
        let model_start = gate_start + self.gates.desc().size;
        let delta_start = model_start + self.arguments.desc().size;
        let mut span = ring.reserve(
            delta_start + if exact { self.delta.desc().size } else { 0 },
            u64::from(self.align) * 4,
        )?;
        let origin = span.offset();
        let mut offsets = vec![0; passes.len() + 1];
        let mut at = 0;
        for (list, offset) in lists
            .iter()
            .chain(std::iter::once(&source))
            .zip(&mut offsets)
        {
            if let Some(list) = list {
                *offset = origin + at as u64;
                span.bytes()[at..at + list.len() * 4].copy_from_slice(&words(list.iter().copied()));
                at += padded(list) as usize * 4;
            }
        }
        let chunk = DISPATCH / self.align * self.align;
        // Copy zero counts and unit y/z dimensions into the reusable indirect
        // records. Compare activates each filter only when delta is nonempty.
        let gate_data = &mut span.bytes()[gate_start as usize..model_start as usize];
        gate_data.fill(0);
        for (i, grid) in self.grids.iter().enumerate() {
            let count = lists[i]
                .as_ref()
                .map_or(grid.columns * grid.rows, |l| l.len() as u32);
            for (slice, first) in (0..count).step_by(chunk as usize).enumerate() {
                let offset = (grid.arguments + slice as u64 * self.argument_stride) as usize;
                gate_data[offset + 4..offset + 8].copy_from_slice(&1u32.to_le_bytes());
                gate_data[offset + 8..offset + 12].copy_from_slice(&1u32.to_le_bytes());
                gate_data[offset + 12..offset + 16]
                    .copy_from_slice(&chunk.min(count - first).to_le_bytes());
            }
        }
        span.bytes().copy_within(
            gate_start as usize..model_start as usize,
            model_start as usize,
        );
        encoder.copy_buffer(
            span.buffer()
                .slice(origin + model_start..origin + delta_start),
            &self.arguments,
            0,
        );
        if exact {
            // Exact publications already provide delta. Upload its bitmap and
            // activate filters without launching a comparison kernel.
            let bitmap = &mut span.bytes()[delta_start as usize..];
            bitmap.fill(0);
            let stride = self.size.0.div_ceil(8).div_ceil(32);
            let whole = [Rect {
                x: 0,
                y: 0,
                w: self.size.0,
                h: self.size.1,
            }];
            for r in damage
                .unwrap_or(&whole)
                .iter()
                .filter(|r| r.w > 0 && r.h > 0)
            {
                let (x0, x1) = (r.x / 8, (r.x + r.w).div_ceil(8));
                for y in r.y / 8..(r.y + r.h).div_ceil(8) {
                    for word in x0 / 32..=(x1 - 1) / 32 {
                        let lo = if word == x0 / 32 { x0 % 32 } else { 0 };
                        let hi = if word == (x1 - 1) / 32 {
                            (x1 - 1) % 32
                        } else {
                            31
                        };
                        let mask = (!0u32 << lo) & (!0u32 >> (31 - hi));
                        let at = ((y * stride + word) * 4) as usize;
                        let value =
                            u32::from_le_bytes(bitmap[at..at + 4].try_into().unwrap()) | mask;
                        bitmap[at..at + 4].copy_from_slice(&value.to_le_bytes());
                    }
                }
            }
            for record in span.bytes()[gate_start as usize..model_start as usize]
                .chunks_mut(self.argument_stride as usize)
            {
                let count = u32::from_le_bytes(record[12..16].try_into().unwrap());
                record[..4].copy_from_slice(&count.div_ceil(64).to_le_bytes());
            }
            encoder.copy_buffer(
                span.buffer()
                    .slice(origin + delta_start..origin + span.len()),
                &self.delta,
                0,
            );
        } else {
            encoder.fill_buffer(self.delta.slice(0..self.delta.desc().size), 0);
        }
        encoder.copy_buffer(
            span.buffer()
                .slice(origin + gate_start..origin + model_start),
            &self.gates,
            0,
        );
        let mut compute = encoder.compute_pass(&ComputePassDesc {
            timestamps,
            label: "upscale",
        });
        if !exact {
            compute.set_pipeline(if self.initialized.get() {
                &self.compare
            } else {
                &self.initialize
            });
            compute.bind(0, Bind::Uniform(self.delta_dimensions.slice(0..16)));
            compute.bind(0, Bind::Sampled(self.scratch.view()));
            compute.bind(0, Bind::StorageImage(self.input.view()));
            compute.bind(
                2,
                Bind::StorageBuffer(self.delta.slice(0..self.delta.desc().size)),
            );
            compute.bind(
                3,
                Bind::StorageBuffer(self.gates.slice(0..self.gates.desc().size)),
            );
            let (source_buffer, source_offset, source_count) = source.as_ref().map_or(
                (
                    &self.whole[self.source_whole],
                    0,
                    self.size.0.div_ceil(8) * self.size.1.div_ceil(8),
                ),
                |l| (span.buffer(), offsets[passes.len()], l.len() as u32),
            );
            for first in (0..source_count).step_by(chunk as usize) {
                let n = chunk.min(source_count - first);
                let start = source_offset + u64::from(first) * 4;
                compute.bind(
                    1,
                    Bind::StorageBuffer(source_buffer.slice(start..start + u64::from(n) * 4)),
                );
                compute.dispatch(n, 1, 1);
            }
        }
        let list = |i: usize| {
            let grid = &self.grids[i];
            match &lists[i] {
                Some(list) => (span.buffer(), offsets[i], list.len() as u32),
                None => (&self.whole[grid.whole], 0, grid.columns * grid.rows),
            }
        };
        for (i, (pass, pipeline)) in passes.iter().zip(&self.pipelines).enumerate() {
            let grid = &self.grids[i];
            let (buffer, base, count) = list(i);
            compute.set_pipeline(&self.filter);
            compute.bind(0, Bind::Uniform(grid.parameters.slice(0..16)));
            compute.bind(
                1,
                Bind::StorageBuffer(self.delta.slice(0..self.delta.desc().size)),
            );
            for (slice, first) in (0..count).step_by(chunk as usize).enumerate() {
                let n = chunk.min(count - first);
                let start = base + u64::from(first) * 4;
                let compacted = u64::from(first) * 4;
                let argument = grid.arguments + slice as u64 * self.argument_stride;
                compute.bind(
                    0,
                    Bind::StorageBuffer(buffer.slice(start..start + u64::from(n) * 4)),
                );
                compute.bind(
                    2,
                    Bind::StorageBuffer(
                        grid.compacted
                            .slice(compacted..compacted + u64::from(n) * 4),
                    ),
                );
                compute.bind(
                    3,
                    Bind::StorageBuffer(self.arguments.slice(argument..argument + 16)),
                );
                compute.dispatch_indirect(self.gates.slice(argument..argument + 12));
            }
            compute.set_pipeline(pipeline);
            for &(slot, role) in pass.bindings {
                let resource = match role {
                    Role::Uniform => Bind::Uniform(self.dimensions.slice(0..16)),
                    Role::Cells => continue,
                    Role::Source => Bind::Sampled(self.input.view()),
                    Role::Tensor(t) => Bind::Sampled(self.tensors[t].view()),
                    Role::Store(t) => Bind::StorageImage(self.tensors[t].view()),
                    Role::Output => Bind::StorageImage(output),
                    Role::Nearest => Bind::Sampler(&self.nearest),
                    Role::Linear => Bind::Sampler(&self.linear),
                };
                compute.bind(slot, resource);
            }
            for (slice, first) in (0..count).step_by(chunk as usize).enumerate() {
                let n = chunk.min(count - first);
                let start = u64::from(first) * 4;
                let argument = grid.arguments + slice as u64 * self.argument_stride;
                compute.bind(
                    0,
                    Bind::StorageBuffer(grid.compacted.slice(start..start + u64::from(n) * 4)),
                );
                compute.dispatch_indirect(self.arguments.slice(argument..argument + 16));
            }
        }
        drop(compute);
        encoder.retain(span);
        self.initialized.set(true);
        let last = passes.len() - 1;
        let cells = lists[last].as_ref().map_or(
            u64::from(self.grids[last].columns * self.grids[last].rows),
            |l| l.len() as u64,
        );
        Ok(cells * u64::from(passes[last].unit()).pow(2))
    }

    /// Each pass's cell list after a change within `damage`, or `None` where
    /// it is the whole grid. Walks back from the last pass: a pass's cells
    /// cover what later readers need of the tensors it writes, and it needs
    /// its inputs its radius beyond those cells.
    fn cells(&self, damage: &[Rect]) -> Vec<Option<Vec<u32>>> {
        let grow = |r: Rect, by: u32| {
            let x = r.x.saturating_sub(by);
            let y = r.y.saturating_sub(by);
            Rect {
                x,
                y,
                w: (r.x + r.w + by).min(self.size.0).saturating_sub(x),
                h: (r.y + r.h + by).min(self.size.1).saturating_sub(y),
            }
        };
        let reach = self.model.reach();
        // What later readers need of each tensor, the output last.
        let output = self.model.tensors.len();
        let mut needed = vec![Vec::new(); output + 1];
        needed[output] = damage
            .iter()
            .filter(|r| r.w > 0 && r.h > 0)
            .map(|&r| grow(r, reach))
            .collect();
        let mut lists = vec![None; self.model.passes.len()];
        for (i, pass) in self.model.passes.iter().enumerate().rev() {
            let grid = &self.grids[i];
            let unit = pass.unit();
            let words = grid.columns.div_ceil(64) as usize;
            let mut bits = vec![0u64; words * grid.rows as usize];
            // Earlier writers of these tensors feed no later reader.
            let wanted: Vec<Rect> = pass
                .writes()
                .flat_map(|t| std::mem::take(&mut needed[t.unwrap_or(output)]))
                .collect();
            for r in wanted.iter().filter(|r| r.w > 0 && r.h > 0) {
                let (x0, x1) = (r.x / unit, (r.x + r.w - 1) / unit);
                for y in r.y / unit..=(r.y + r.h - 1) / unit {
                    let row = &mut bits[y as usize * words..][..words];
                    for word in x0 / 64..=x1 / 64 {
                        let lo = if word == x0 / 64 { x0 % 64 } else { 0 };
                        let hi = if word == x1 / 64 { x1 % 64 } else { 63 };
                        row[word as usize] |= (!0u64 << lo) & (!0u64 >> (63 - hi));
                    }
                }
            }
            let mut list = Vec::new();
            for y in 0..grid.rows {
                let row = &bits[y as usize * words..][..words];
                for (start, end) in runs(row, grid.columns) {
                    list.extend((start..end).map(|x| x | y << 16));
                    let run = Rect {
                        x: start * unit,
                        y: y * unit,
                        w: (end - start) * unit,
                        h: unit,
                    };
                    let input = grow(run, pass.radius);
                    for t in pass.reads() {
                        needed[t].push(input);
                    }
                }
            }
            lists[i] = (list.len() as u32 != grid.columns * grid.rows).then_some(list);
        }
        lists
    }
}

/// The runs of set bits in a row of `columns` cells, as `start..end`.
fn runs(row: &[u64], columns: u32) -> impl Iterator<Item = (u32, u32)> + '_ {
    let mut x = 0;
    std::iter::from_fn(move || {
        while x < columns {
            let word = row[(x / 64) as usize] >> (x % 64);
            if word == 0 {
                x = (x / 64 + 1) * 64;
                continue;
            }
            let start = x + word.trailing_zeros();
            let mut end = start;
            // A run continues into the next word only from its first bit.
            while end < columns {
                let ones = (row[(end / 64) as usize] >> (end % 64)).trailing_ones();
                end += ones;
                if ones == 0 || !end.is_multiple_of(64) {
                    break;
                }
            }
            x = end.min(columns);
            return Some((start, x));
        }
        None
    })
}

fn words(values: impl IntoIterator<Item = u32>) -> Vec<u8> {
    values.into_iter().flat_map(u32::to_le_bytes).collect()
}

/// `k / 256` as IEEE half-precision bits; exact for k up to 256.
fn half_bits(k: u32) -> [u8; 2] {
    if k == 0 {
        return [0, 0];
    }
    let exponent = 31 - k.leading_zeros();
    let mantissa = ((k << (10 - exponent)) & 0x3ff) as u16;
    (((exponent + 7) as u16) << 10 | mantissa).to_le_bytes()
}

/// The canvas of the precision check.
const CHECK: (u32, u32) = (16, 16);

/// A model's half forms against its single forms on a small canvas of
/// pseudo-random pixels. Some drivers compute the half forms of some models
/// wrongly by whole channels (Adreno 6xx); healthy half precision stays
/// within 3 levels of single precision.
pub struct Check {
    /// Half, then single precision, each with its doubled output.
    runs: [(Upscaling, Image); 2],
    pixels: Buffer,
    readback: Buffer,
}

impl Check {
    /// Blocking pipeline creation, like [`Upscaling::new`]. `None` when
    /// `model` has no half form.
    pub fn new(device: &Device, model: Model) -> adv_gpu::Result<Option<Self>> {
        let halves = model.passes.iter().any(|pass| {
            SHADERS
                .iter()
                .any(|(path, h, _)| *path == pass.shader && h.is_some())
        });
        if !halves {
            return Ok(None);
        }
        let run = |half| {
            let output = device.create_image(&ImageDesc {
                dimension: Dimension::D2,
                size: Extent {
                    width: CHECK.0 * 2,
                    height: CHECK.1 * 2,
                    depth_or_layers: 1,
                },
                format: OUTPUT,
                mips: 1,
                samples: 1,
                usage: Usage::STORAGE | Usage::COPY_SRC,
                view_formats: &[],
                label: "upscale check",
            })?;
            Ok::<_, adv_gpu::Error>((
                Upscaling::with_precision(device, model, CHECK, half)?,
                output,
            ))
        };
        let mut seed = 0x2545_f491u32;
        let mut level = || {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
            half_bits((seed >> 16) % 257)
        };
        let bytes: Vec<u8> = (0..CHECK.0 * CHECK.1)
            .flat_map(|_| [level(), level(), level(), [0x00, 0x3c]])
            .flatten()
            .collect();
        let buffer = |size, usage, memory| {
            device.create_buffer(&BufferDesc {
                size,
                usage,
                memory,
                label: "upscale check",
            })
        };
        let pixels = buffer(bytes.len() as u64, BufferUsage::COPY_SRC, Memory::Upload)?;
        // SAFETY: the buffer is new and no GPU work refers to it yet.
        (unsafe { pixels.mapped().expect("upload memory").as_mut() })[..bytes.len()]
            .copy_from_slice(&bytes);
        Ok(Some(Self {
            runs: [run(true)?, run(false)?],
            pixels,
            readback: buffer(2 * Self::BYTES, BufferUsage::COPY_DST, Memory::Readback)?,
        }))
    }

    const BYTES: u64 = (CHECK.0 * CHECK.1 * 16) as u64;

    /// Both runs and their readback.
    pub fn encode(&self, encoder: &mut Encoder, ring: &mut Ring) -> adv_gpu::Result<()> {
        use adv_gpu::encode::{BufferCopy, ImageCopy};
        let at = |image| ImageCopy {
            image,
            mip: 0,
            layer: 0,
            origin: (0, 0, 0),
        };
        for (offset, (upscaling, output)) in (0..).step_by(Self::BYTES as usize).zip(&self.runs) {
            let input = upscaling.input(true);
            let pixels = BufferCopy {
                buffer: &self.pixels,
                offset: 0,
                row_pitch: 0,
                rows: 0,
            };
            encoder.copy_buffer_to_image(pixels, at(input), input.desc().size);
            upscaling.encode(encoder, ring, output.view(), None, true, None)?;
            let readback = BufferCopy {
                buffer: &self.readback,
                offset,
                row_pitch: 0,
                rows: 0,
            };
            encoder.copy_image_to_buffer(at(output), readback, output.desc().size);
        }
        Ok(())
    }

    /// Whether the half forms computed what the single forms did.
    ///
    /// # Safety
    /// The work [`Self::encode`] recorded has completed.
    pub unsafe fn agrees(&self) -> bool {
        // SAFETY: the caller guarantees the copies completed.
        let bytes = unsafe { self.readback.mapped().expect("readback memory").as_ref() };
        let (half, single) = bytes[..2 * Self::BYTES as usize].split_at(Self::BYTES as usize);
        half.iter().zip(single).all(|(a, b)| a.abs_diff(*b) <= 8)
    }
}

#[cfg(test)]
mod run_tests {
    #[test]
    // spec: HOST-VIEW-10
    fn runs_end_at_word_boundaries_and_cross_them() {
        let runs = |row: &[u64], columns| super::runs(row, columns).collect::<Vec<_>>();
        // A run ending on the last bit of a word, before a clear next word.
        assert_eq!(runs(&[1 << 63, 0], 100), [(63, 64)]);
        assert_eq!(runs(&[!0, 1 << 1], 100), [(0, 64), (65, 66)]);
        // Runs spanning words, and one ending at the row's last cell.
        assert_eq!(runs(&[1 << 63, !0, 0b11], 130), [(63, 130)]);
        assert_eq!(runs(&[0, 1 << 3 | 1 << 5], 70), [(67, 68), (69, 70)]);
        assert_eq!(runs(&[!0], 64), [(0, 64)]);
        assert!(runs(&[0, 0], 128).is_empty());
    }
}

#[cfg(test)]
mod precision_tests {
    use super::*;
    use std::time::Duration;

    /// The first case a child process compiles; unset in the parent.
    const CASE: &str = "ADV_UPSCALE_COMPILE_CASE";

    fn device() -> Device {
        Device::open(&adv_gpu::queue::DeviceOptions {
            software: !cfg!(any(target_vendor = "apple", target_os = "android")),
            validation: !cfg!(target_os = "android"),
            disable: adv_gpu::Modern::from_test_env(),
            ..Default::default()
        })
        .unwrap()
        .0
    }

    /// Every pass this device runs, in each precision it has: the label, the
    /// model, the pass and whether it is the half-precision form.
    fn cases(device: &Device) -> Vec<(String, Model, &'static Pass, bool)> {
        let mut cases: Vec<(String, Model, &'static Pass, bool)> = vec![];
        for half in [true, false] {
            for choice in [
                Upscaler::Anime4k,
                Upscaler::Cunny,
                Upscaler::Artcnn,
                Upscaler::K7,
            ] {
                let m = model(device, choice, half);
                for pass in m.passes {
                    let (_, h, _) = SHADERS.iter().find(|(p, ..)| *p == pass.shader).unwrap();
                    let label = format!("{} {}", if half { "f16" } else { "f32" }, pass.shader);
                    if !(half && h.is_none()) && !cases.iter().any(|c| c.0 == label) {
                        cases.push((label, m, pass, half));
                    }
                }
            }
        }
        cases
    }

    /// Cases compile in a child process that announces each one first; a
    /// driver crash names its case and a new child resumes after it, so every
    /// other case still reports.
    #[test]
    fn passes_compile_in_isolation() {
        use std::io::{BufRead, Write};
        let device = device();
        let cases = cases(&device);
        if let Ok(start) = std::env::var(CASE) {
            let mut out = std::io::stdout();
            for (i, (_, m, pass, half)) in cases.iter().enumerate().skip(start.parse().unwrap()) {
                writeln!(out, "{i}").unwrap();
                out.flush().unwrap();
                pipeline(&device, m, pass, *half).unwrap();
            }
            return;
        }
        drop(device);
        let mut failed = vec![];
        let mut start = 0;
        while start < cases.len() {
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "stages::upscale::precision_tests::passes_compile_in_isolation",
                    "--exact",
                    "--nocapture",
                ])
                .env(CASE, start.to_string())
                .stdout(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            let (sender, announced) = std::sync::mpsc::channel();
            let stdout = child.stdout.take().unwrap();
            std::thread::spawn(move || {
                for line in std::io::BufReader::new(stdout).lines() {
                    if let Ok(i) = line.unwrap().parse::<usize>() {
                        sender.send(i).unwrap();
                    }
                }
            });
            let mut current = None;
            let status = loop {
                match announced.recv_timeout(Duration::from_secs(60)) {
                    Ok(i) => current = Some(i),
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                        break child.wait().unwrap().to_string();
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        child.kill().unwrap();
                        child.wait().unwrap();
                        break "timed out".to_owned();
                    }
                }
            };
            if status == "exit status: 0" {
                break;
            }
            let i = current.expect("the child failed before its first case");
            eprintln!("{}: {status}", cases[i].0);
            failed.push(format!("{} ({status})", cases[i].0));
            start = i + 1;
        }
        assert!(failed.is_empty(), "failed: {}", failed.join(", "));
    }
}

#[cfg(test)]
#[path = "upscale_tests.rs"]
mod delta_tests;
