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

/// Input pixels a change reaches in the output of `choice`. Every variant of
/// a model keeps its receptive field.
pub fn reach(choice: Upscaler) -> u32 {
    models::REFERENCE[index(choice)].reach()
}

impl Model {
    /// Input pixels a change reaches in the output: every layer is a 3×3
    /// convolution, and the final layer also samples the input bilinearly.
    pub fn reach(&self) -> u32 {
        self.passes.iter().map(|p| p.radius).sum::<u32>() + 1
    }
}

impl Pass {
    /// Input pixels per workgroup cell along each axis.
    fn unit(&self) -> u32 {
        self.block / self.scale
    }
}

/// One pass's cells: columns and rows of its grid, and its whole-grid list.
struct Grid {
    columns: u32,
    rows: u32,
    whole: usize,
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
        let mut grids = Vec::new();
        let mut shapes: Vec<(u32, u32)> = Vec::new();
        let mut whole = Vec::new();
        for pass in model.passes {
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
            grids.push(Grid {
                columns: shape.0,
                rows: shape.1,
                whole: index,
            });
        }
        Ok(Self {
            model,
            size,
            input,
            tensors,
            pipelines,
            nearest: sampler(Filter::Nearest)?,
            linear: sampler(Filter::Linear)?,
            dimensions,
            grids,
            whole,
            align: device.caps().limits.storage_offset_alignment.max(4) / 4,
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
    /// cells of its grid that the next pass reads, so reads stay within values
    /// computed in this run, and disjoint changes stay disjoint (HOST-VIEW-10).
    /// Returns the input pixels the last pass recomputed. `Busy` leaves the
    /// encoder untouched.
    pub fn encode(
        &self,
        encoder: &mut Encoder,
        ring: &mut Ring,
        output: &ImageView,
        damage: Option<&[Rect]>,
    ) -> adv_gpu::Result<u64> {
        let passes = self.model.passes;
        let lists = match damage {
            Some(damage) => self.cells(damage),
            None => vec![None; passes.len()],
        };
        if lists.iter().any(|l| l.as_ref().is_some_and(Vec::is_empty)) {
            return Ok(0);
        }
        // Listed cells go to the ring, each list at an aligned offset.
        let padded = |l: &Vec<u32>| (l.len() as u64).next_multiple_of(u64::from(self.align));
        let entries: u64 = lists.iter().flatten().map(padded).sum();
        let mut span = None;
        let mut offsets = vec![0; passes.len()];
        if entries > 0 {
            let mut reserved = ring.reserve(entries * 4, u64::from(self.align) * 4)?;
            let origin = reserved.offset();
            let bytes = reserved.bytes();
            let mut at = 0;
            for (list, offset) in lists.iter().zip(&mut offsets) {
                if let Some(list) = list {
                    *offset = origin + at as u64;
                    bytes[at..at + list.len() * 4].copy_from_slice(&words(list.iter().copied()));
                    at += padded(list) as usize * 4;
                }
            }
            span = Some(reserved);
        }
        let chunk = DISPATCH / self.align * self.align;
        let mut compute = encoder.compute_pass(&ComputePassDesc {
            timestamps: None,
            label: "upscale",
        });
        for (i, (pass, pipeline)) in passes.iter().zip(&self.pipelines).enumerate() {
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
            let grid = &self.grids[i];
            let (buffer, base, count) = match (&lists[i], &span) {
                (Some(list), Some(span)) => (span.buffer(), offsets[i], list.len() as u32),
                _ => (&self.whole[grid.whole], 0, grid.columns * grid.rows),
            };
            for first in (0..count).step_by(chunk as usize) {
                let n = chunk.min(count - first);
                let start = base + u64::from(first) * 4;
                compute.bind(
                    0,
                    Bind::StorageBuffer(buffer.slice(start..start + u64::from(n) * 4)),
                );
                compute.dispatch(n, 1, 1);
            }
        }
        drop(compute);
        if let Some(span) = span {
            encoder.retain(span);
        }
        let last = passes.len() - 1;
        let cells = lists[last].as_ref().map_or(
            u64::from(self.grids[last].columns * self.grids[last].rows),
            |l| l.len() as u64,
        );
        Ok(cells * u64::from(passes[last].unit()).pow(2))
    }

    /// Each pass's cell list after a change within `damage`, or `None` where
    /// it is the whole grid. Walks back from the last pass: a pass's cells
    /// must cover what its successor reads, its radius beyond the cells.
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
        let mut needed: Vec<Rect> = damage
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
            for r in needed.iter().filter(|r| r.w > 0 && r.h > 0) {
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
            needed.clear();
            let mut list = Vec::new();
            for y in 0..grid.rows {
                let row = &bits[y as usize * words..][..words];
                let mut x = 0;
                while x < grid.columns {
                    let word = row[(x / 64) as usize] >> (x % 64);
                    if word == 0 {
                        x = (x / 64 + 1) * 64;
                        continue;
                    }
                    let start = x + word.trailing_zeros();
                    let mut end = start;
                    loop {
                        end += (row[(end / 64) as usize] >> (end % 64)).trailing_ones();
                        if !end.is_multiple_of(64) || end >= grid.columns {
                            break;
                        }
                    }
                    list.extend((start..end).map(|x| x | y << 16));
                    let run = Rect {
                        x: start * unit,
                        y: y * unit,
                        w: (end - start) * unit,
                        h: unit,
                    };
                    needed.push(grow(run, pass.radius));
                    x = end;
                }
            }
            lists[i] = (list.len() as u32 != grid.columns * grid.rows).then_some(list);
        }
        lists
    }
}

fn words(values: impl IntoIterator<Item = u32>) -> Vec<u8> {
    values.into_iter().flat_map(u32::to_le_bytes).collect()
}

#[cfg(test)]
mod precision_tests {
    use super::*;
    use std::time::Duration;

    /// The case a child process compiles; unset in the parent.
    const CASE: &str = "ADV_UPSCALE_COMPILE_CASE";
    /// The pass whose forms the variant cases rewrite.
    const SUBJECT: &str = "Anime4K_Upscale_Denoise_L/1.wgsl";

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

    /// `f16 <shader>` and `f32 <shader>` for every pass this device runs,
    /// then rewrites of `SUBJECT` that each remove one construct.
    fn cases(device: &Device) -> Vec<String> {
        let mut cases = vec![];
        for half in [true, false] {
            for choice in [
                Upscaler::Anime4k,
                Upscaler::Cunny,
                Upscaler::Artcnn,
                Upscaler::K7,
            ] {
                for pass in model(device, choice).passes {
                    let (_, h, _) = SHADERS.iter().find(|(p, ..)| *p == pass.shader).unwrap();
                    let case = format!("{} {}", if half { "f16" } else { "f32" }, pass.shader);
                    if !(half && h.is_none()) && !cases.contains(&case) {
                        cases.push(case);
                    }
                }
            }
        }
        cases.extend(["stored-nulls", "uninitialized", "sampled"].map(String::from));
        cases
    }

    /// Function variables of array type lose their null initializer; with
    /// `store`, the entry block stores the null instead.
    fn without_null_arrays(spirv: &[u32], store: bool) -> Vec<u32> {
        let instructions = || {
            let mut rest = &spirv[5..];
            std::iter::from_fn(move || {
                let (head, tail) = rest.split_at_checked((*rest.first()? >> 16) as usize)?;
                rest = tail;
                Some(head)
            })
        };
        let pointers: Vec<u32> = {
            let arrays: Vec<u32> = instructions()
                .filter(|i| i[0] & 0xffff == 28)
                .map(|i| i[1])
                .collect();
            instructions()
                .filter(|i| i[0] & 0xffff == 32 && arrays.contains(&i[3]))
                .map(|i| i[1])
                .collect()
        };
        let mut out = spirv[..5].to_vec();
        let mut pending = vec![];
        for i in instructions() {
            let op = i[0] & 0xffff;
            if op == 59 && i.len() == 5 && i[3] == 7 && pointers.contains(&i[1]) {
                out.extend_from_slice(&[(4 << 16) | 59, i[1], i[2], 7]);
                pending.push([(3 << 16) | 62, i[2], i[4]]);
                continue;
            }
            if op != 59 && !pending.is_empty() {
                if store {
                    out.extend(pending.iter().flatten());
                }
                pending.clear();
            }
            out.extend_from_slice(i);
        }
        out
    }

    fn compile(device: &Device, case: &str) {
        let (_, half, single) = SHADERS.iter().find(|(p, ..)| *p == SUBJECT).unwrap();
        let subject = half.as_ref().or(single.as_ref()).unwrap();
        let source = |spirv: &[u32]| {
            device
                .create_shader(ShaderDesc {
                    source: Source::Precompiled {
                        spirv,
                        metal: subject.metal,
                        entries: subject.entries,
                    },
                    identity: 0,
                    label: "compile case",
                })
                .unwrap()
        };
        let m = models::FUSED[index(Upscaler::Anime4k)];
        let pass = m.passes.iter().find(|p| p.shader == SUBJECT).unwrap();
        let build = |shader: adv_gpu::Shader| {
            device
                .create_compute_pipeline(
                    &ComputePipelineDesc {
                        entry: shader.entry("main", Stage::Compute).unwrap(),
                        bindings: pass.layout(&m),
                        label: "compile case",
                    },
                    None,
                )
                .unwrap();
        };
        match case {
            "stored-nulls" | "uninitialized" => {
                build(source(&without_null_arrays(
                    subject.spirv,
                    case == "stored-nulls",
                )));
            }
            "sampled" => {
                let mut wgsl = include_str!("upscale/Anime4K_Upscale_Denoise_L/1.wgsl").to_owned();
                for c in 0..3 {
                    wgsl = wgsl.replace(
                        &format!("textureGather((i32({c})), (INPUT_0), (sam_0), (tpos_0))"),
                        "textureSampleLevel(INPUT_0, sam_0, tpos_0, 0.0)",
                    );
                }
                assert!(!wgsl.contains("textureGather"));
                let compiled = adv_gpu::shader::precompile(&wgsl, &pass.layout(&m)).unwrap();
                build(source(&compiled.spirv));
            }
            _ => {
                let (form, shader) = case.split_once(' ').unwrap();
                let choice = [
                    Upscaler::Anime4k,
                    Upscaler::Cunny,
                    Upscaler::Artcnn,
                    Upscaler::K7,
                ]
                .into_iter()
                .map(|choice| model(device, choice))
                .find(|m| m.passes.iter().any(|p| p.shader == shader))
                .unwrap();
                let pass = choice.passes.iter().find(|p| p.shader == shader).unwrap();
                pipeline(device, &choice, pass, form == "f16").unwrap();
            }
        }
    }

    /// Cases compile in a child process that announces each one first; a
    /// driver crash names its case and a new child resumes after it, so every
    /// other case still reports. The rewrites of `SUBJECT` show which
    /// construct a compiler rejects.
    #[test]
    fn passes_compile_in_isolation() {
        use std::io::{BufRead, Write};
        let device = device();
        let cases = cases(&device);
        if let Ok(start) = std::env::var(CASE) {
            let mut out = std::io::stdout();
            for (i, case) in cases.iter().enumerate().skip(start.parse().unwrap()) {
                writeln!(out, "{i}").unwrap();
                out.flush().unwrap();
                compile(&device, case);
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
            eprintln!("{}: {status}", cases[i]);
            failed.push(format!("{} ({status})", cases[i]));
            start = i + 1;
        }
        assert!(failed.is_empty(), "failed: {}", failed.join(", "));
    }
}
