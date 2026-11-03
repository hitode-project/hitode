//! The upscale stage: the shipped models' compute layers over private
//! tensors, the final layer writing the doubled image into a presentable
//! buffer (HOST-VIEW-7, HOST-VIEW-10).
pub mod models;
mod table;

pub use table::{Model, Pass, Role};

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
use adv_gpu::Extent;
use adv_gpu::encode::{Bind, ComputePassDesc};
use adv_gpu::pipeline::ComputePipelineDesc;
use adv_gpu::resource::{Address, BorderColor, Dimension, Filter, ImageDesc, SamplerDesc, Usage};
use adv_gpu::shader::{ShaderDesc, Source, Stage};
use adv_gpu::{
    ComputePipeline, Device, Encoder, Features, Format, Image, ImageView, Ring, Sampler,
};

/// Bytes of one pass's uniform: dimensions, then the region origin.
const PARAMS: u64 = 32;

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

/// The model a device runs: matrix input convolutions where it has half
/// precision 8×8 matrices, the fused layers otherwise.
pub fn model(device: &Device, choice: Upscaler) -> Model {
    let matrix = Features::MATRIX_F16_8X8 | Features::SHADER_F16 | Features::SUBGROUP;
    if device.caps().features.contains(matrix) {
        models::MATRIX[index(choice)]
    } else {
        models::FUSED[index(choice)]
    }
}

impl Model {
    /// Input pixels a change reaches in the output: every layer is a 3×3
    /// convolution, and the final layer also samples the input bilinearly.
    pub fn reach(&self) -> u32 {
        self.passes.iter().map(|p| p.radius).sum::<u32>() + 1
    }
}

/// One model at one canvas size. Only the selected model is resident.
pub struct Upscaling {
    model: Model,
    size: (u32, u32),
    /// The composed canvas in half precision, written by composition.
    input: Image,
    tensors: Vec<Image>,
    pipelines: Vec<ComputePipeline>,
    nearest: Sampler,
    linear: Sampler,
    /// Uniform offset alignment of the device.
    align: u64,
}

impl Upscaling {
    /// Blocking pipeline creation; run it on the compile pool.
    pub fn new(device: &Device, model: Model, size: (u32, u32)) -> adv_gpu::Result<Self> {
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
            Format::Rgba16Float,
            Usage::SAMPLED | Usage::COLOR | Usage::COPY_DST,
            "upscale input",
        )?;
        let tensors = model
            .tensors
            .iter()
            .map(|&format| image(format, Usage::SAMPLED | Usage::STORAGE, "upscale tensor"))
            .collect::<adv_gpu::Result<_>>()?;
        let half = device.caps().features.contains(Features::SHADER_F16);
        let pipelines = model
            .passes
            .iter()
            .map(|pass| {
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
                        bindings: pass.layout(&model),
                        label: "upscale pass",
                    },
                    None,
                )
            })
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
        Ok(Self {
            model,
            size,
            input,
            tensors,
            pipelines,
            nearest: sampler(Filter::Nearest)?,
            linear: sampler(Filter::Linear)?,
            align: u64::from(device.caps().limits.uniform_offset_alignment),
        })
    }

    pub fn size(&self) -> (u32, u32) {
        self.size
    }
    pub fn model(&self) -> &Model {
        &self.model
    }
    /// The half-precision canvas the model reads; composition writes it.
    pub fn input(&self) -> &Image {
        &self.input
    }

    /// Recompute after the input changed within `damage`, or entirely, into
    /// `output`, a doubled-size view with storage usage. Each pass covers the
    /// next pass's workgroup-rounded region plus its radius, so reads stay
    /// within values computed in this run (HOST-VIEW-10). Returns the input
    /// pixels the last pass recomputed. `Busy` leaves the encoder untouched.
    pub fn encode(
        &self,
        encoder: &mut Encoder,
        ring: &mut Ring,
        output: &ImageView,
        damage: Option<Rect>,
    ) -> adv_gpu::Result<u64> {
        let full = Rect {
            x: 0,
            y: 0,
            w: self.size.0,
            h: self.size.1,
        };
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
        let passes = self.model.passes;
        let mut regions = vec![full; passes.len()];
        if let Some(damage) = damage {
            let mut needed = grow(damage, self.model.reach());
            for (pass, region) in passes.iter().zip(&mut regions).rev() {
                let unit = pass.block / pass.scale;
                *region = Rect {
                    w: needed.w.next_multiple_of(unit),
                    h: needed.h.next_multiple_of(unit),
                    ..needed
                };
                needed = grow(*region, pass.radius);
            }
        }
        if regions.iter().any(|r| r.w == 0 || r.h == 0) {
            return Ok(0);
        }
        let stride = PARAMS.next_multiple_of(self.align);
        let mut span = ring.reserve(stride * passes.len() as u64, self.align)?;
        let (w, h) = self.size;
        for (slot, region) in span.bytes().chunks_mut(stride as usize).zip(&regions) {
            let (x, y) = if damage.is_some() {
                (region.x, region.y)
            } else {
                (0, 0)
            };
            let values = [w, h, w * 2, h * 2, x, y, x * 2, y * 2];
            for (out, value) in slot.as_chunks_mut::<4>().0.iter_mut().zip(values) {
                out.copy_from_slice(&value.to_le_bytes());
            }
        }
        for (i, ((pass, pipeline), region)) in
            passes.iter().zip(&self.pipelines).zip(&regions).enumerate()
        {
            let mut compute = encoder.compute_pass(&ComputePassDesc {
                timestamps: None,
                label: "upscale",
            });
            compute.set_pipeline(pipeline);
            for &(slot, role) in pass.bindings {
                compute.bind(
                    slot,
                    match role {
                        Role::Uniform => Bind::Uniform(span.buffer().slice(
                            span.offset() + stride * i as u64
                                ..span.offset() + stride * i as u64 + PARAMS,
                        )),
                        Role::Source => Bind::Sampled(self.input.view()),
                        Role::Tensor(t) => Bind::Sampled(self.tensors[t].view()),
                        Role::Store(t) => Bind::StorageImage(self.tensors[t].view()),
                        Role::Output => Bind::StorageImage(output),
                        Role::Nearest => Bind::Sampler(&self.nearest),
                        Role::Linear => Bind::Sampler(&self.linear),
                    },
                );
            }
            compute.dispatch(
                (region.w * pass.scale).div_ceil(pass.block),
                (region.h * pass.scale).div_ceil(pass.block),
                1,
            );
        }
        encoder.retain(span);
        let last = regions[regions.len() - 1];
        Ok(u64::from(last.w) * u64::from(last.h))
    }
}
