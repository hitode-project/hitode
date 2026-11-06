//! The upscale stage: the shipped models' compute layers over private
//! tensors, the final layer writing the doubled image into a presentable
//! buffer (HOST-VIEW-7, HOST-VIEW-10).
pub mod models;
mod table;

pub use table::{Model, OUTPUT, Pass, Role};

/// The format of the model's input, which composition writes.
pub const INPUT: Format = Format::Rgba16Float;

/// How composition populated the model input.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Input {
    /// Exact damage: the change itself.
    Exact,
    /// Conservative composed damage, which compare narrows.
    Composed,
}

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
    Buffer, ComputePipeline, Device, Encoder, Extent, Features, Format, Image, Region, Ring,
    Sampler,
};
use std::cell::{Cell, RefCell};

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
    /// The most source cells apart along an axis that one output cell's field
    /// meets: compare checks offsets this near the change and the votes.
    pub fn neighbourhood(&self) -> u32 {
        let unit = self.passes.last().unwrap().unit() as i32;
        let reach = self.reach() as i32;
        (0..8)
            .map(|j| {
                let first = (unit * j - reach).div_euclid(8);
                let last = (unit * j + unit - 1 + reach).div_euclid(8);
                (last - first) as u32
            })
            .max()
            .unwrap()
    }

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

/// One pass's grid of cells and where its list and indirect records live.
struct Grid {
    columns: u32,
    rows: u32,
    /// First list entry in the compacted buffer.
    compacted: u64,
    /// First indirect record, in words within an argument set.
    arguments: u64,
    /// Input pixels a change reaches in what this pass writes.
    #[cfg_attr(not(test), allow(dead_code))]
    affected: u32,
}

/// Bookkeeping tiles, in input pixels (upscale_filter.wgsl).
const TILE: u32 = 128;
/// Words of bookkeeping workgroup memory per bitmap kind.
const ARENA: u32 = 2048;
/// Source-cell words of the bookkeeping delta and decision bitmaps.
const DELTA_WORDS: u32 = 128;
/// Words of the frame header before its data (upscale_common.wgsl).
const FRAME_WORDS: usize = 8;
/// Words per source cell of the facts compare records.
const FACT_WORDS: u64 = 3;
/// Anchor windows' side; a change reaches anchors this far up and left, less one.
const WINDOW: u32 = 4;
/// Offset vote slots, and the words of one frame parity's hypotheses
/// (upscale_common.wgsl).
const VOTES: u64 = 64;
const HYPOTHESIS_WORDS: u64 = 11;

/// One model at one canvas size. Only the selected model is resident.
pub struct Upscaling {
    model: Model,
    size: (u32, u32),
    /// The last two encoded inputs, alternating, and the doubled output.
    inputs: [Image; 2],
    output: Image,
    /// The input the last encode read.
    newest: Cell<usize>,
    /// Producer damage of the last encode, which the older input lacks;
    /// `None` until both inputs hold a frame.
    stale: RefCell<Option<Region>>,
    /// Per source cell, what compare found (upscale_common.wgsl).
    facts: Buffer,
    /// Per source cell anchors, pending anchors and the anchor index.
    anchors: Buffer,
    epoch: Cell<u32>,
    anchor: ComputePipeline,
    hypotheses: ComputePipeline,
    compare: ComputePipeline,
    bookkeeping: ComputePipeline,
    gather: ComputePipeline,
    scatter: ComputePipeline,
    /// Copied output cells between gather and scatter.
    scratch: Image,
    /// Indirect sets, delta and validity.
    state: Buffer,
    metadata: Buffer,
    compacted: Buffer,
    layout: Layout,
    tensors: Vec<Image>,
    pipelines: Vec<ComputePipeline>,
    nearest: Sampler,
    linear: Sampler,
    /// Canvas and doubled dimensions, the uniform of every pass.
    dimensions: Buffer,
    grids: Vec<Grid>,
    /// The bookkeeping halo in input pixels.
    halo: u32,
    /// Storage offset alignment, in list entries.
    align: u32,
}

/// Word offsets into the state buffer.
struct Layout {
    /// Compare's indirect record, in state words.
    compare_record: u64,
    arguments: [u64; 2],
    set_words: u64,
    records: u64,
    delta: u64,
    chunk: u32,
    /// The copy list in the compacted buffer, and its first indirect record.
    copies: u64,
    copy_arguments: u64,
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

/// A precompiled host shader of this stage.
macro_rules! stage_shader {
    ($device:expr, $name:literal) => {
        $device.create_shader(ShaderDesc {
            source: Source::Precompiled {
                spirv: &include!(concat!(env!("OUT_DIR"), "/", $name, ".spv.rs")),
                metal: include_bytes!(concat!(env!("OUT_DIR"), "/", $name, ".msl")),
                entries: &include!(concat!(env!("OUT_DIR"), "/", $name, ".entries.rs")),
            },
            identity: include!(concat!(env!("OUT_DIR"), "/", $name, ".identity.rs")),
            label: $name,
        })
    };
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
        let (columns, rows) = (size.0.div_ceil(8), size.1.div_ceil(8));
        if size.0 == 0 || size.1 == 0 || size.0 > max / 2 || size.1 > max / 2 {
            return Err(adv_gpu::Error::Invalid(
                "upscaled canvas exceeds the image limit".into(),
            ));
        }
        let image = |size: (u32, u32), format, usage, label| {
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
        let input = || {
            image(
                size,
                INPUT,
                Usage::SAMPLED | Usage::COLOR | Usage::COPY_DST,
                "upscale input",
            )
        };
        let output = {
            image(
                (size.0 * 2, size.1 * 2),
                OUTPUT,
                Usage::STORAGE | Usage::SAMPLED | Usage::COPY_SRC,
                "upscaled",
            )?
        };
        let tensors = model
            .tensors
            .iter()
            .map(|&format| {
                image(
                    size,
                    format,
                    Usage::SAMPLED | Usage::STORAGE,
                    "upscale tensor",
                )
            })
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
        let buffer = |bytes, usage, label| {
            device.create_buffer(&BufferDesc {
                size: bytes,
                usage,
                memory: Memory::Device,
                label,
            })
        };
        let dimensions = upload(
            &words([size.0, size.1, size.0 * 2, size.1 * 2]),
            BufferUsage::UNIFORM,
            "upscale dimensions",
        )?;
        let unsupported =
            |what: &str| adv_gpu::Error::Invalid(format!("upscale bookkeeping: {what}"));
        let passes = model.passes;
        let last = passes.last().unwrap();
        if passes.len() > 32
            || passes.iter().any(|p| ![4, 8, 16].contains(&p.unit()))
            || last.unit() > 8
        {
            return Err(unsupported("pass units"));
        }
        let unit_max = passes.iter().map(Pass::unit).max().unwrap();
        let reaches = model.filter_reaches();
        let halo = (reaches[0] + unit_max).next_multiple_of(16);
        let span = |unit: u32| (TILE + 2 * halo) / unit;
        let region_words = |unit: u32| span(unit).div_ceil(32) * span(unit);
        // What each pass writes reaches this far from changed input, and
        // the passes whose results it reads.
        let mut writers = vec![None; model.tensors.len()];
        let mut consumers = vec![0u32; passes.len()];
        let mut producers = vec![0u32; passes.len()];
        let mut affected = vec![0; model.tensors.len()];
        let mut reach = vec![0; passes.len()];
        for (i, pass) in passes.iter().enumerate() {
            for t in pass.reads() {
                if let Some(writer) = writers[t] {
                    consumers[writer] |= 1 << i;
                    producers[i] |= 1u32 << writer;
                }
            }
            reach[i] = pass.reads().map(|t| affected[t]).max().unwrap_or(0) + pass.radius;
            for t in pass.writes().flatten() {
                writers[t] = Some(i);
                affected[t] = reach[i];
            }
        }
        let output_reach = reach[passes.len() - 1];
        // Input pixels by which the output cells' need grows back to each
        // pass's results: exact, so a pass computes only cells meeting it.
        let mut margins = vec![0u32; passes.len()];
        for i in (0..passes.len()).rev() {
            margins[i] = (i + 1..passes.len())
                .filter(|&j| consumers[i] & 1 << j != 0)
                .map(|j| margins[j] + passes[j].radius)
                .max()
                .unwrap_or(0);
        }
        if output_reach > 16
            || (TILE + 2 * (halo + 32)).div_ceil(8).div_ceil(32) * (TILE + 2 * (halo + 32)) / 8
                > DELTA_WORDS
        {
            return Err(unsupported("model reach"));
        }
        let align = device.caps().limits.storage_offset_alignment.max(4) / 4;
        let chunk = DISPATCH / align * align;
        let mut grids = Vec::new();
        let mut metadata = Vec::new();
        let (mut arena, mut valid, mut entries, mut records) = (0, 0u32, 0u64, 0u64);
        for (i, pass) in passes.iter().enumerate() {
            let shape = (size.0.div_ceil(pass.unit()), size.1.div_ceil(pass.unit()));
            let count = shape.0 * shape.1;
            let aliases = passes.iter().enumerate().fold(0u32, |mask, (j, other)| {
                let shared = pass
                    .writes()
                    .flatten()
                    .any(|t| other.writes().any(|u| u == Some(t)));
                if i != j && shared {
                    mask | 1 << j
                } else {
                    mask
                }
            });
            metadata.push([
                shape.0,
                shape.1,
                pass.unit(),
                reach[i],
                pass.radius,
                producers[i],
                aliases,
                valid,
                entries as u32,
                records as u32 * 4,
                0,
                arena,
                margins[i],
            ]);
            grids.push(Grid {
                columns: shape.0,
                rows: shape.1,
                compacted: entries,
                arguments: records * 4,
                affected: reach[i],
            });
            arena += region_words(pass.unit());
            valid += shape.0.div_ceil(32) * shape.1;
            entries = (entries + u64::from(count)).next_multiple_of(u64::from(align));
            records += u64::from(count.div_ceil(chunk));
        }
        if arena > ARENA {
            return Err(unsupported("bookkeeping memory"));
        }
        // The copy list: at most every last-pass cell.
        let copies = entries;
        let copy_arguments = records * 4;
        let last_cells = {
            let grid = grids.last().unwrap();
            grid.columns * grid.rows
        };
        entries += u64::from(last_cells);
        records += u64::from(last_cells.div_ceil(chunk));
        // Records first, then one counter per pass and the copies'.
        let record_words = records * 4;
        for (i, row) in metadata.iter_mut().enumerate() {
            row[10] = (record_words + i as u64) as u32;
        }
        let copy_counter = record_words + passes.len() as u64;
        let set_words = copy_counter + 1;
        let delta_words = u64::from(columns.div_ceil(32) * rows);
        let arguments = [0, set_words];
        // The damage overlay binds delta as storage.
        let delta = (2 * set_words).next_multiple_of(u64::from(align));
        let valid_start = delta + delta_words;
        let hypotheses = valid_start + 2 * u64::from(valid);
        let votes = hypotheses + 2 * HYPOTHESIS_WORDS;
        let cells = u64::from(columns * rows);
        // Compare's indirect record.
        let compare_record = votes + 2 * VOTES;
        let state_words = compare_record + 4;
        let index_slots = cells.next_power_of_two();
        let shape = [
            size.0,
            size.1,
            passes.len() as u32,
            halo,
            columns,
            rows,
            delta as u32,
            valid_start as u32,
            (valid_start + u64::from(valid)) as u32,
            arguments[0] as u32,
            arguments[1] as u32,
            set_words as u32,
            record_words as u32,
            chunk,
            model.reach(),
            hypotheses as u32,
            votes as u32,
            copies as u32,
            copy_arguments as u32,
            copy_counter as u32,
            (index_slots - 1) as u32,
            model.neighbourhood(),
            compare_record as u32,
            0,
        ];
        let metadata = upload(
            &words(shape.into_iter().chain(metadata.into_iter().flatten())),
            BufferUsage::STORAGE,
            "upscale pass graph",
        )?;
        let stage = |shader: adv_gpu::Shader, entry, bindings: &[adv_gpu::pipeline::Binding]| {
            device.create_compute_pipeline(
                &ComputePipelineDesc {
                    entry: shader.entry(entry, Stage::Compute)?,
                    bindings: bindings.to_vec(),
                    label: "upscale stage",
                },
                None,
            )
        };
        Ok(Self {
            anchor: stage(
                stage_shader!(device, "upscale_anchor")?,
                "main",
                &super::bindings::UPSCALE_ANCHOR,
            )?,
            gather: stage(
                stage_shader!(device, "upscale_copy")?,
                "gather",
                &super::bindings::UPSCALE_COPY,
            )?,
            scatter: stage(
                stage_shader!(device, "upscale_copy")?,
                "scatter",
                &super::bindings::UPSCALE_COPY,
            )?,
            scratch: image(
                (size.0 * 2, size.1 * 2),
                OUTPUT,
                Usage::STORAGE | Usage::SAMPLED,
                "upscale copies",
            )?,
            hypotheses: stage(
                stage_shader!(device, "upscale_delta")?,
                "hypotheses",
                &super::bindings::UPSCALE_DELTA,
            )?,
            compare: stage(
                stage_shader!(device, "upscale_delta")?,
                "compare",
                &super::bindings::UPSCALE_DELTA,
            )?,
            bookkeeping: stage(
                stage_shader!(device, "upscale_filter")?,
                "main",
                &super::bindings::UPSCALE_FILTER,
            )?,
            model,
            size,
            inputs: [input()?, input()?],
            output,
            newest: Cell::new(0),
            stale: RefCell::new(None),
            facts: buffer(
                cells * FACT_WORDS * 4,
                // COPY_SRC: tests read it back.
                BufferUsage::STORAGE | BufferUsage::COPY_DST | BufferUsage::COPY_SRC,
                "upscale compare facts",
            )?,
            anchors: buffer(
                (6 * cells + 2 * index_slots) * 4,
                BufferUsage::STORAGE | BufferUsage::COPY_DST,
                "upscale anchors",
            )?,
            epoch: Cell::new(0),
            state: buffer(
                state_words * 4,
                // COPY_SRC: tests read it back.
                BufferUsage::STORAGE
                    | BufferUsage::INDIRECT
                    | BufferUsage::COPY_DST
                    | BufferUsage::COPY_SRC,
                "upscale state",
            )?,
            metadata,
            compacted: buffer(
                entries * 4,
                BufferUsage::STORAGE | BufferUsage::COPY_SRC,
                "upscale compacted cells",
            )?,
            layout: Layout {
                arguments,
                set_words,
                records: record_words,
                compare_record,
                delta,
                chunk,
                copies,
                copy_arguments,
            },
            tensors,
            pipelines,
            nearest: sampler(Filter::Nearest)?,
            linear: sampler(Filter::Linear)?,
            dimensions,
            grids,
            halo,
            align,
        })
    }

    pub fn size(&self) -> (u32, u32) {
        self.size
    }
    pub fn model(&self) -> &Model {
        &self.model
    }

    /// The image composition writes before the next encode.
    pub fn input(&self) -> &Image {
        &self.inputs[1 - self.newest.get()]
    }

    /// The doubled image of the last encode.
    pub fn output(&self) -> &Image {
        &self.output
    }

    /// What composition writes into [`Self::input`] for a change within
    /// `damage`: it holds the frame before last, so the last encode's change
    /// too. `None` is the whole canvas.
    pub fn composition(&self, damage: Option<&[Rect]>) -> Option<Vec<Rect>> {
        let stale = self.stale.borrow();
        let (Some(stale), Some(damage)) = (stale.as_ref(), damage) else {
            return None;
        };
        let mut region = stale.clone();
        for &r in damage {
            region.add(r.into());
        }
        Some(region.rects().into_iter().map(Into::into).collect())
    }

    /// Update after composition within `damage`, or the whole canvas, wrote
    /// [`Self::input`] as [`Self::composition`] asks. Exact damage is the
    /// change itself; otherwise compare finds it. Bookkeeping then selects
    /// every pass's cells on the GPU (HOST-VIEW-10). Returns the input pixels
    /// the change reaches in the output, an upper bound on recomputed work.
    pub fn encode(
        &self,
        gpu: (&mut Encoder, &mut Ring),
        damage: Option<&[Rect]>,
        input: Input,
        timestamps: Option<(&adv_gpu::queue::QuerySet, u32)>,
    ) -> adv_gpu::Result<u64> {
        let (encoder, ring) = gpu;
        let initialized = self.epoch.get() != 0;
        let mut region = Region::empty(self.size);
        match damage.filter(|_| initialized) {
            Some(rects) => rects.iter().for_each(|&r| region.add(r.into())),
            None => region = Region::whole(self.size),
        }
        if region.is_empty() {
            return Ok(0);
        }
        let exact = input == Input::Exact || !initialized;
        let (columns, rows) = (self.size.0.div_ceil(8), self.size.1.div_ceil(8));
        let rects: Vec<Rect> = region.rects().into_iter().map(Into::into).collect();
        // The bookkeeping tiles: wherever this change or the last one reaches.
        let tile_columns = self.size.0.div_ceil(TILE);
        let tile_rows = self.size.1.div_ceil(TILE);
        let mut tiles = vec![false; (tile_columns * tile_rows) as usize];
        let stale = self.stale.borrow();
        let reached = rects.iter().copied().chain(
            stale
                .as_ref()
                .map(Region::rects)
                .unwrap_or_default()
                .into_iter()
                .map(Into::into),
        );
        for r in reached {
            let x0 = r.x.saturating_sub(self.halo) / TILE;
            let y0 = r.y.saturating_sub(self.halo) / TILE;
            let x1 = (r.x + r.w + self.halo).min(self.size.0).div_ceil(TILE);
            let y1 = (r.y + r.h + self.halo).min(self.size.1).div_ceil(TILE);
            for y in y0..y1 {
                for x in x0..x1 {
                    tiles[(y * tile_columns + x) as usize] = true;
                }
            }
        }
        drop(stale);
        let tile_list: Vec<u32> = (0..tile_rows)
            .flat_map(|y| (0..tile_columns).map(move |x| (x, y)))
            .filter(|&(x, y)| tiles[(y * tile_columns + x) as usize])
            .map(|(x, y)| x | y << 16)
            .collect();
        // Without exact damage, the anchor pass also finds the change among
        // the producer's cells. Compare checks them and every cell sharing an
        // output field with them under the offset hypotheses. A whole-canvas
        // change walks every cell instead of listing them; a first frame has
        // nothing to compare against. Offsets are sought only for changes
        // over an eighth of the canvas's cells: smaller ones skip the four
        // stages that find and copy them.
        let whole = region.rects() == Region::whole(self.size).rects();
        let cells: u32 = rects
            .iter()
            .map(|r| r.w.div_ceil(8) * r.h.div_ceil(8))
            .sum();
        let motion = initialized && cells >= (columns * rows).div_ceil(8);
        let (mut compared, mut anchored) = (vec![], vec![]);
        if !whole {
            let mut seen = vec![0u64; (columns * rows).div_ceil(64) as usize];
            if motion {
                compared = self.cells(&rects, 0, 0, &mut seen);
                let ring = 8 * self.model.neighbourhood();
                compared.extend(self.cells(&rects, ring, ring, &mut seen));
            }
            // Encodes seeking no offsets hash no anchors: the anchor pass
            // then only finds the change among the producer's cells.
            seen.fill(0);
            if motion {
                anchored = self.cells(&rects, WINDOW - 1, 0, &mut seen);
            } else if !exact {
                anchored = self.cells(&rects, 0, 0, &mut seen);
            }
        }
        let all = (columns * rows) as usize;
        let compare_count = if whole && motion { all } else { compared.len() };
        let anchor_count = if whole { all } else { anchored.len() };
        let listed = |at: usize, every: bool| if every { u32::MAX } else { at as u32 };
        let stride = columns.div_ceil(32);
        let bitmap_words = if exact { stride * rows } else { 0 };
        let list = 0;
        let tile_at = list + compared.len();
        let bitmap = tile_at + tile_list.len();
        let anchor_at = bitmap + bitmap_words as usize;
        let data_words = anchor_at + anchored.len();
        let align = u64::from(self.align) * 4;
        let mut span = ring.reserve(((FRAME_WORDS + data_words) * 4) as u64, align)?;
        let epoch = self.epoch.get() + 1;
        {
            let bytes = span.bytes();
            bytes.fill(0);
            let mut put = |at: usize, value: u32| {
                bytes[at * 4..at * 4 + 4].copy_from_slice(&value.to_le_bytes());
            };
            put(0, epoch);
            put(1, u32::from(exact));
            put(2, compare_count as u32);
            put(3, listed(list, whole));
            put(4, tile_at as u32);
            put(5, bitmap as u32);
            put(6, anchor_count as u32);
            put(7, listed(anchor_at, whole));
            for (i, &cell) in anchored.iter().enumerate() {
                put(FRAME_WORDS + anchor_at + i, cell);
            }
            for (i, &cell) in compared.iter().enumerate() {
                put(FRAME_WORDS + list + i, cell);
            }
            for (i, &tile) in tile_list.iter().enumerate() {
                put(FRAME_WORDS + tile_at + i, tile);
            }
            if exact {
                for r in &rects {
                    for y in r.y / 8..(r.y + r.h).div_ceil(8) {
                        for x in r.x / 8..(r.x + r.w).div_ceil(8) {
                            let at =
                                (FRAME_WORDS + bitmap) * 4 + ((y * stride + x / 32) * 4) as usize;
                            let word = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
                            bytes[at..at + 4]
                                .copy_from_slice(&(word | 1 << (x % 32)).to_le_bytes());
                        }
                    }
                }
            }
        }
        let (frame_at, frame_len) = (span.offset(), span.len());
        if !initialized {
            // The only clears: both argument sets start as (0, 1, 1, 0) records.
            encoder.fill_buffer(self.state.slice(0..self.state.desc().size), 0);
            encoder.fill_buffer(self.facts.slice(0..self.facts.desc().size), 0);
            encoder.fill_buffer(self.anchors.slice(0..self.anchors.desc().size), 0);
            let mut init = ring.reserve(self.layout.set_words * 8, align)?;
            let bytes = init.bytes();
            bytes.fill(0);
            for set in 0..2 {
                for record in 0..self.layout.records / 4 {
                    let at = ((set * self.layout.set_words + record * 4) * 4) as usize;
                    bytes[at + 4..at + 8].copy_from_slice(&1u32.to_le_bytes());
                    bytes[at + 8..at + 12].copy_from_slice(&1u32.to_le_bytes());
                }
            }
            encoder.copy_buffer(
                init.buffer()
                    .slice(init.offset()..init.offset() + init.len()),
                &self.state,
                self.layout.arguments[0] * 4,
            );
            encoder.retain(init);
        }
        let (newest, written) = (self.newest.get(), 1 - self.newest.get());
        let state = || Bind::StorageBuffer(self.state.slice(0..self.state.desc().size));
        let facts = || Bind::StorageBuffer(self.facts.slice(0..self.facts.desc().size));
        let anchors = || Bind::StorageBuffer(self.anchors.slice(0..self.anchors.desc().size));
        let frame = || Bind::StorageBuffer(span.buffer().slice(frame_at..frame_at + frame_len));
        let metadata = || Bind::StorageBuffer(self.metadata.slice(0..self.metadata.desc().size));
        let mut compute = encoder.compute_pass(&ComputePassDesc {
            timestamps,
            label: "upscale",
        });
        let inputs = |compute: &mut adv_gpu::encode::ComputePass<'_>| {
            compute.bind(0, Bind::Sampled(self.inputs[written].view()));
            compute.bind(1, Bind::Sampled(self.inputs[newest].view()));
        };
        if anchor_count != 0 {
            compute.set_pipeline(&self.anchor);
            inputs(&mut compute);
            compute.bind(0, frame());
            compute.bind(1, anchors());
            compute.bind(2, state());
            compute.bind(3, metadata());
            compute.bind(4, facts());
            let n = anchor_count as u32;
            compute.dispatch(n.min(DISPATCH), n.div_ceil(DISPATCH), 1);
        }
        if compare_count != 0 {
            // The hypotheses once, then compare under them as they size it.
            let delta = |compute: &mut adv_gpu::encode::ComputePass<'_>, pipeline| {
                compute.set_pipeline(pipeline);
                inputs(compute);
                compute.bind(0, frame());
                compute.bind(1, facts());
                compute.bind(2, state());
                compute.bind(3, metadata());
                compute.bind(4, anchors());
            };
            delta(&mut compute, &self.hypotheses);
            compute.dispatch(1, 1, 1);
            delta(&mut compute, &self.compare);
            let at = self.layout.compare_record * 4;
            compute.dispatch_indirect(self.state.slice(at..at + 12));
        }
        compute.set_pipeline(&self.bookkeeping);
        compute.bind(0, frame());
        compute.bind(1, facts());
        compute.bind(2, state());
        compute.bind(3, metadata());
        compute.bind(
            4,
            Bind::StorageBuffer(self.compacted.slice(0..self.compacted.desc().size)),
        );
        compute.bind(5, anchors());
        let n = tile_list.len() as u32;
        compute.dispatch(n.min(DISPATCH), n.div_ceil(DISPATCH), 1);
        let set = self.layout.arguments[(epoch % 2) as usize];
        let chunk = u64::from(self.layout.chunk);
        // Copies read the previous output before any pass writes it.
        let last = self.grids.last().unwrap();
        let copies = u64::from(last.columns * last.rows);
        if motion {
            for (pipeline, read, write) in [
                (&self.gather, &self.output, &self.scratch),
                (&self.scatter, &self.scratch, &self.output),
            ] {
                compute.set_pipeline(pipeline);
                compute.bind(0, Bind::Sampled(read.view()));
                compute.bind(1, frame());
                compute.bind(2, state());
                compute.bind(3, metadata());
                compute.bind(4, Bind::StorageImage(write.view()));
                for (slice, first) in (0..copies).step_by(chunk as usize).enumerate() {
                    let n = chunk.min(copies - first);
                    let start = (self.layout.copies + first) * 4;
                    compute.bind(
                        0,
                        Bind::StorageBuffer(self.compacted.slice(start..start + n * 4)),
                    );
                    let at = (set + self.layout.copy_arguments + slice as u64 * 4) * 4;
                    compute.dispatch_indirect(self.state.slice(at..at + 12));
                }
            }
        }
        for (pass, (pipeline, grid)) in self
            .model
            .passes
            .iter()
            .zip(self.pipelines.iter().zip(&self.grids))
        {
            compute.set_pipeline(pipeline);
            for &(slot, role) in pass.bindings {
                let resource = match role {
                    Role::Uniform => Bind::Uniform(self.dimensions.slice(0..16)),
                    Role::Cells => continue,
                    Role::Source => Bind::Sampled(self.inputs[written].view()),
                    Role::Tensor(t) => Bind::Sampled(self.tensors[t].view()),
                    Role::Store(t) => Bind::StorageImage(self.tensors[t].view()),
                    Role::Output => Bind::StorageImage(self.output.view()),
                    Role::Nearest => Bind::Sampler(&self.nearest),
                    // At scale 1 linear samples land on texel centers, where
                    // nearest returns the same texel and rounds robustly.
                    Role::Linear if pass.scale == 1 => Bind::Sampler(&self.nearest),
                    Role::Linear => Bind::Sampler(&self.linear),
                };
                compute.bind(slot, resource);
            }
            let count = u64::from(grid.columns * grid.rows);
            for (slice, first) in (0..count).step_by(chunk as usize).enumerate() {
                let n = chunk.min(count - first);
                let start = (grid.compacted + first) * 4;
                compute.bind(
                    0,
                    Bind::StorageBuffer(self.compacted.slice(start..start + n * 4)),
                );
                let at = (set + grid.arguments + slice as u64 * 4) * 4;
                compute.dispatch_indirect(self.state.slice(at..at + 12));
            }
        }
        drop(compute);
        encoder.retain(span);
        self.epoch.set(epoch);
        self.newest.set(written);
        *self.stale.borrow_mut() = Some(region);
        Ok(self.reached(&rects))
    }

    /// The source cells meeting `rects` grown by `before` pixels up and left
    /// and `after` down and right, packed, skipping and marking those `seen`.
    fn cells(&self, rects: &[Rect], before: u32, after: u32, seen: &mut [u64]) -> Vec<u32> {
        let columns = self.size.0.div_ceil(8);
        let mut found = Vec::new();
        for r in rects {
            let (x0, y0) = (
                r.x.saturating_sub(before) / 8,
                r.y.saturating_sub(before) / 8,
            );
            let x1 = (r.x + r.w + after).min(self.size.0).div_ceil(8);
            let y1 = (r.y + r.h + after).min(self.size.1).div_ceil(8);
            for y in y0..y1 {
                for x in x0..x1 {
                    let i = (y * columns + x) as usize;
                    if seen[i / 64] & 1 << (i % 64) == 0 {
                        seen[i / 64] |= 1 << (i % 64);
                        found.push(x | y << 16);
                    }
                }
            }
        }
        found
    }

    /// Input pixels a change within `damage` reaches in the output, in cells.
    pub(crate) fn reached(&self, damage: &[Rect]) -> u64 {
        let reach = self.model.reach();
        let mut reached = Region::empty(self.size);
        for r in damage.iter().filter(|r| r.w > 0 && r.h > 0) {
            let x = r.x.saturating_sub(reach);
            let y = r.y.saturating_sub(reach);
            reached.add(adv_gpu::Rect {
                x,
                y,
                w: (r.x + r.w + reach).min(self.size.0) - x,
                h: (r.y + r.h + reach).min(self.size.1) - y,
            });
        }
        reached
            .rects()
            .iter()
            .map(|r| u64::from(r.w) * u64::from(r.h))
            .sum()
    }

    /// Diagnostic readers of the existing GPU result; no extra upscale work.
    pub(crate) fn damage_delta(&self) -> adv_gpu::resource::BufferSlice<'_> {
        let start = self.layout.delta * 4;
        let words = u64::from(self.size.0.div_ceil(8).div_ceil(32) * self.size.1.div_ceil(8));
        self.state.slice(start..start + words * 4)
    }

    pub(crate) fn damage_unit(&self) -> u32 {
        self.model.passes.last().unwrap().unit()
    }

    /// The last pass's list slices with their indirect records, of the last encode.
    pub(crate) fn damage_cells(
        &self,
    ) -> impl Iterator<
        Item = (
            adv_gpu::resource::BufferSlice<'_>,
            adv_gpu::resource::BufferSlice<'_>,
        ),
    > {
        let grid = self.grids.last().unwrap();
        let chunk = u64::from(self.layout.chunk);
        let count = u64::from(grid.columns * grid.rows);
        let set = self.layout.arguments[(self.epoch.get() % 2) as usize];
        (0..count)
            .step_by(chunk as usize)
            .enumerate()
            .map(move |(slice, first)| {
                let n = chunk.min(count - first);
                let start = (grid.compacted + first) * 4;
                let at = (set + grid.arguments + slice as u64 * 4) * 4;
                (
                    self.compacted.slice(start..start + n * 4),
                    self.state.slice(at..at + 12),
                )
            })
    }
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
    /// Half, then single precision.
    runs: [Upscaling; 2],
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
        let run = |half| Upscaling::with_precision(device, model, CHECK, half);
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
        for (offset, upscaling) in (0..).step_by(Self::BYTES as usize).zip(&self.runs) {
            let input = upscaling.input();
            let pixels = BufferCopy {
                buffer: &self.pixels,
                offset: 0,
                row_pitch: 0,
                rows: 0,
            };
            encoder.copy_buffer_to_image(pixels, at(input), input.desc().size);
            upscaling.encode((encoder, ring), None, Input::Exact, None)?;
            let readback = BufferCopy {
                buffer: &self.readback,
                offset,
                row_pitch: 0,
                rows: 0,
            };
            let output = upscaling.output();
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
