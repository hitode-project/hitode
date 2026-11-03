//! Package C1: pipelines for fixed-function draws, programmable draws and
//! blits, keyed as the wgpu renderer keys them, and the disk cache
//! (GFX-CACHE-7, GFX-CACHE-8, GFX-CACHE-10) over `Device::pipeline_identity`.
//! Lookups run on the execution thread; record.rs asks for a pipeline per draw.

use super::super::{DrawState, Error, ProgrammableDraw};
use super::Gpu;
use std::path::Path;

/// Binding slots, the same for every pipeline: record.rs binds them, the
/// shaders declare them as `@binding(n)`. A pipeline lists only the slots its
/// shaders read.
pub(crate) mod slot {
    /// Fixed-function transform and lighting state, or vertex shader
    /// constants.
    pub const VERTEX_UNIFORMS: u32 = 0;
    /// Texture-stage state and fog, or pixel shader constants.
    pub const FRAGMENT_UNIFORMS: u32 = 1;
    /// D3D9 vertex streams `0..16`, one storage buffer each; shaders pull
    /// their attributes (adv-gpu has no vertex input state). Fixed-function
    /// draws use stream 0 only, as packed [`super::super::super::Vertex`]es.
    pub const STREAMS: u32 = 2;
    /// Sampled images: pixel samplers `s0..s15`, then vertex samplers
    /// `D3DVERTEXTEXTURESAMPLER0..3`; fixed-function stage `n` uses `n`.
    pub const TEXTURES: u32 = 18;
    /// Samplers, in the same order as [`TEXTURES`].
    pub const SAMPLERS: u32 = 38;
}

use super::super::{self as gpu, Attribute, AttributeFormat, Program};
use super::resources::gpu_error;
use adv_gpu::pipeline as p;
use adv_gpu::shader::{ShaderDesc, Source, Stage};
use adv_gpu::{Device, Format, RenderPipeline};
use gpu::compile::{self, Compile, CompileCounters};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};

use std::path::PathBuf;
use std::sync::Arc;

const MAX_PIPELINES: usize = 512;
const ARCHIVE_LIMIT: usize = 8 << 20;
const MAGIC: &[u8; 8] = b"ADVNAT01";

/// Vertex constants at slot 0 are followed immediately by the 640-byte
/// ProgrammableDraw::parameters block. Slot 1 contains pixel constants.
/// Fixed draws use the 32-byte fixed.wgsl Parameters block at slot 1;
/// alpha_op bits 31 and 30 select opaque and A8 sampling respectively.
const FIXED: &str = include_str!("../fixed.wgsl");

/// Translate the stream's shared slot space to adv-gpu's per-kind registers.
/// The owner applies this once when encoding a stream binding.
pub(super) fn binding_slot(shared: u32) -> u32 {
    match shared {
        slot::VERTEX_UNIFORMS | slot::FRAGMENT_UNIFORMS => shared,
        slot::STREAMS..slot::TEXTURES => shared - slot::STREAMS,
        slot::TEXTURES..slot::SAMPLERS => shared - slot::TEXTURES,
        slot::SAMPLERS..58 => shared - slot::SAMPLERS,
        _ => unreachable!("executor binding slot"),
    }
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, Serialize, Deserialize)]
struct Raster {
    topology: gpu::Topology,
    cull: gpu::Cull,
    color_blend: Option<gpu::Blend>,
    alpha_blend: Option<gpu::Blend>,
    depth: Option<gpu::Compare>,
    depth_write: bool,
    stencil: Option<gpu::Stencil>,
    color_write: u8,
}
impl From<&DrawState> for Raster {
    fn from(s: &DrawState) -> Self {
        Self {
            topology: s.topology,
            cull: s.cull,
            color_blend: s.blend,
            alpha_blend: s.alpha_blend,
            depth: s.depth,
            depth_write: s.depth_write,
            stencil: s.stencil.map(|mut stencil| {
                stencil.read_mask &= 255;
                stencil.write_mask &= 255;
                stencil
            }),
            color_write: s.color_write,
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
enum Kind {
    Fixed,
    Program {
        source: Arc<str>,
        textures: Vec<bool>,
        modes: Vec<u8>,
        streams: Vec<(u32, Vec<Attribute>, bool)>,
    },
    Blit(gpu::Filter),
}
#[derive(Clone, Serialize, Deserialize)]
struct Recipe {
    kind: Kind,
    raster: Raster,
    colors: Vec<u32>,
    depth: Option<u32>,
}
// Stable archive codes; these are not backend enum discriminants.
const FORMATS: &[Format] = &[
    Format::Bgra8Unorm,
    Format::Rgba8Unorm,
    Format::Depth16Unorm,
    Format::Depth32Float,
    Format::Depth24UnormStencil8,
    Format::Depth32FloatStencil8,
];
fn format_code(format: Format) -> u32 {
    FORMATS
        .iter()
        .position(|f| *f == format)
        .expect("executor attachment format") as u32
}
fn digest(bytes: &[u8]) -> u64 {
    u64::from_le_bytes(Sha256::digest(bytes)[..8].try_into().unwrap())
}
#[derive(Clone, PartialEq, Eq, Hash)]
enum DrawKind {
    Fixed,
    Program(u64),
    Blit(gpu::Filter),
}
#[derive(Clone, PartialEq, Eq, Hash)]
struct StreamKey {
    stride: u32,
    attributes: arrayvec::ArrayVec<Attribute, 16>,
    instance: bool,
}
#[derive(Clone, PartialEq, Eq, Hash)]
struct Key {
    kind: DrawKind,
    raster: Raster,
    colors: arrayvec::ArrayVec<Format, 8>,
    depth: Option<Format>,
    streams: arrayvec::ArrayVec<StreamKey, 16>,
    textures: arrayvec::ArrayVec<(bool, u8), 20>,
}

#[derive(Default)]
pub(super) struct Pipelines {
    compiler: Compiler,
    entries: HashMap<Key, RenderPipeline>,
    order: VecDeque<Key>,
    jobs: HashMap<[u8; 32], Compile<Result<Built, Error>>>,
    cache_hits: u64,
    batch: Option<Compile<()>>,
    cache: Option<Arc<p::PipelineCache>>,
    store: Option<Archive>,
    save: Option<Compile<u64>>,
    counters: CompileCounters,
    hits: u64,
    misses: u64,
    discarded: u64,
    persisted: u64,
}

/// A sendable shader-preparation endpoint for the front end's compile jobs.
/// Complete vertex layouts arrive with draws; their pipelines are prepared by
/// the recipe prebuilds and shared with demand.
#[derive(Clone, Default)]
pub struct Compiler;
impl Compiler {
    pub fn prepare_program(&self, program: Program) -> Result<Program, Error> {
        gpu::validate_program(&program)?;
        Ok(program)
    }
}

impl Pipelines {
    pub fn fixed(
        &mut self,
        device: &Device,
        state: &DrawState,
        colors: &[Format],
        depth: Option<Format>,
    ) -> Result<RenderPipeline, Error> {
        let raster = Raster::from(state);
        let key = Key {
            kind: DrawKind::Fixed,
            raster,
            colors: colors.iter().copied().collect(),
            depth,
            streams: arrayvec::ArrayVec::new(),
            textures: arrayvec::ArrayVec::new(),
        };
        if let Some(pipeline) = self.entries.get(&key) {
            self.cache_hits += 1;
            return Ok(pipeline.clone());
        }
        self.demand(
            device,
            key,
            Recipe {
                kind: Kind::Fixed,
                raster,
                colors: colors.iter().copied().map(format_code).collect(),
                depth: depth.map(format_code),
            },
        )
    }
    pub fn programmable(
        &mut self,
        device: &Device,
        draw: &ProgrammableDraw,
        state: &DrawState,
        colors: &[Format],
        depth: Option<Format>,
    ) -> Result<RenderPipeline, Error> {
        let raster = Raster::from(state);
        let key = Key {
            kind: DrawKind::Program(draw.program.id),
            raster,
            colors: colors.iter().copied().collect(),
            depth,
            streams: draw
                .streams
                .iter()
                .map(|s| StreamKey {
                    stride: s.stride,
                    attributes: s.attributes.iter().cloned().collect(),
                    instance: s.instance,
                })
                .collect(),
            textures: draw
                .program
                .textures
                .iter()
                .zip(&draw.textures)
                .map(|(&vs, t)| (vs, sampling_mode(t.image.as_ref())))
                .collect(),
        };
        if let Some(pipeline) = self.entries.get(&key) {
            self.cache_hits += 1;
            return Ok(pipeline.clone());
        }
        gpu::validate_program(&draw.program)?;
        self.demand(
            device,
            key,
            Recipe {
                kind: Kind::Program {
                    source: draw.program.source.clone(),
                    textures: draw.program.textures.clone(),
                    modes: draw
                        .textures
                        .iter()
                        .map(|t| sampling_mode(t.image.as_ref()))
                        .collect(),
                    streams: draw
                        .streams
                        .iter()
                        .map(|s| (s.stride, s.attributes.clone(), s.instance))
                        .collect(),
                },
                raster,
                colors: colors.iter().copied().map(format_code).collect(),
                depth: depth.map(format_code),
            },
        )
    }
    pub fn blit(
        &mut self,
        device: &Device,
        filter: gpu::Filter,
        color: Format,
    ) -> Result<RenderPipeline, Error> {
        let mut state = DrawState::new(1, 1);
        let key = Key {
            kind: DrawKind::Blit(filter),
            raster: Raster::from(&state),
            colors: [color].into_iter().collect(),
            depth: None,
            streams: arrayvec::ArrayVec::new(),
            textures: arrayvec::ArrayVec::new(),
        };
        if let Some(pipeline) = self.entries.get(&key) {
            self.cache_hits += 1;
            return Ok(pipeline.clone());
        }
        state.texture_op = gpu::TextureOp::Texture;
        state.alpha_texture_op = gpu::TextureOp::Texture;
        self.demand(
            device,
            key,
            Recipe {
                kind: Kind::Blit(filter),
                raster: Raster::from(&state),
                colors: vec![format_code(color)],
                depth: None,
            },
        )
    }
    fn demand(
        &mut self,
        device: &Device,
        key: Key,
        recipe: Recipe,
    ) -> Result<RenderPipeline, Error> {
        let encoded = serde_json::to_vec(&recipe).unwrap();
        let recipe_key: [u8; 32] = Sha256::digest(&encoded).into();
        let task = if let Some(job) = self.jobs.get(&recipe_key) {
            self.hits += 1;
            job.clone()
        } else {
            self.misses += 1;
            job(
                device,
                recipe.clone(),
                self.cache.clone(),
                self.counters.clone(),
            )
        };
        let built = task.wait().clone().map_err(Error::Device)??;
        let pipeline = built.pipeline;
        if self.entries.len() == MAX_PIPELINES {
            self.entries.remove(&self.order.pop_front().unwrap());
        }
        self.entries.insert(key.clone(), pipeline.clone());
        self.order.push_back(key);
        if let Some(store) = &mut self.store
            && store.remember(recipe, recipe_key, encoded.len(), built.identity)
        {
            self.save();
        }
        Ok(pipeline)
    }
    fn save(&mut self) {
        if let Some(job) = &self.save {
            match job.ready() {
                None => return,
                Some(Ok(count)) => self.persisted = *count,
                Some(Err(_)) => {}
            }
        }
        let Some(store) = &self.store else {
            return;
        };
        let (path, identity, recipes, cache) = (
            store.path.clone(),
            store.identity.clone(),
            store.recipes.clone(),
            self.cache.clone().unwrap(),
        );
        self.save = Some(Compile::spawn(
            ARCHIVE_LIMIT,
            CompileCounters::default(),
            move || {
                save_archive(&path, &identity, &recipes, &cache).map_err(|e| e.to_string())?;
                Ok(recipes.len() as u64)
            },
        ));
    }
    fn open(&mut self, device: &Device, directory: Option<&Path>) {
        self.flush();
        self.jobs.clear();
        self.batch = None;
        self.cache = None;
        self.store = None;
        let Some(directory) = directory else {
            return;
        };
        let backend = if cfg!(target_vendor = "apple") {
            "metal"
        } else {
            "vulkan"
        };
        let identity = format!(
            "native-1/{backend}/{}/{}/{:?}/{}",
            device.caps().adapter,
            device.caps().driver,
            Format::Bgra8Unorm,
            std::env::consts::ARCH
        );
        let path = directory.join(format!("{:016x}.native", digest(identity.as_bytes())));
        let archive = load_archive(&path, &identity);
        self.discarded += u64::from(path.exists() && archive.is_none());
        let (recipes, driver) = archive.unwrap_or_default();
        self.persisted = recipes.len() as u64;
        let cache = Arc::new(p::PipelineCache::load(&driver));
        let bytes = serde_json::to_vec(&recipes).unwrap().len();
        for recipe in &recipes {
            self.jobs.insert(
                Sha256::digest(serde_json::to_vec(recipe).unwrap()).into(),
                job(
                    device,
                    recipe.clone(),
                    Some(cache.clone()),
                    self.counters.clone(),
                ),
            );
        }
        self.batch = Some(compile::prebuild(
            bytes,
            self.jobs.values().cloned().collect(),
        ));
        self.cache = Some(cache);
        let encoded: Vec<_> = recipes
            .iter()
            .map(|r| serde_json::to_vec(r).unwrap())
            .collect();
        let sizes: VecDeque<_> = encoded.iter().map(Vec::len).collect();
        let bytes = sizes.iter().sum();
        let order: VecDeque<[u8; 32]> = encoded.iter().map(|r| Sha256::digest(r).into()).collect();
        let identities = order.iter().map(|&key| (key, None)).collect();
        self.store = Some(Archive {
            path,
            identity,
            recipes,
            identities,
            order,
            sizes,
            bytes,
        });
    }
    pub(super) fn stats(&self) -> gpu::Stats {
        let (waits, ns) = self.counters.snapshot();
        gpu::Stats {
            pipeline_cache_hits: self.cache_hits,
            compile_waits: waits,
            compile_wait_ns: ns,
            prebuild_hits: self.hits,
            prebuild_misses: self.misses,
            persisted_pipelines: self
                .save
                .as_ref()
                .and_then(Compile::ready)
                .and_then(|result| result.as_ref().ok())
                .copied()
                .unwrap_or(self.persisted),
            discarded_pipelines: self.discarded,
            ..Default::default()
        }
    }
    fn flush(&mut self) {
        if let Some(job) = self.save.take()
            && let Ok(count) = job.resolve()
        {
            self.persisted = *count;
        }
        if let (Some(store), Some(cache)) = (&self.store, &self.cache)
            && save_archive(&store.path, &store.identity, &store.recipes, cache).is_ok()
        {
            self.persisted = store.recipes.len() as u64;
        }
    }
}
impl Drop for Pipelines {
    fn drop(&mut self) {
        self.flush();
    }
}
#[derive(Clone)]
struct Built {
    pipeline: RenderPipeline,
    identity: u64,
}
fn job(
    device: &Device,
    recipe: Recipe,
    cache: Option<Arc<p::PipelineCache>>,
    counters: CompileCounters,
) -> Compile<Result<Built, Error>> {
    let device = device.clone();
    Compile::spawn(usize::MAX, counters, move || {
        Ok(build(&device, &recipe, cache.as_deref()))
    })
}
fn sampling_mode(image: Option<&super::Image>) -> u8 {
    image.map_or(0, |image| {
        if image.texel_format() == gpu::TexelFormat::A8 {
            2
        } else {
            u8::from(image.alpha_mode() == gpu::AlphaMode::Opaque)
        }
    })
}

pub(super) fn fixed_sampling_flags(image: Option<&super::Image>) -> u32 {
    match sampling_mode(image) {
        1 => 1 << 31,
        2 => 1 << 30,
        _ => 0,
    }
}

impl Gpu {
    pub fn compile_handle(&self) -> Compiler {
        self.pipelines.compiler.clone()
    }
    pub fn set_pipeline_cache_directory(&mut self, directory: Option<&Path>) {
        self.pipelines.open(&self.device, directory);
    }
    pub fn flush_pipeline_cache(&mut self) {
        self.pipelines.flush();
        let cache = self.pipelines.stats();
        self.stats.pipeline_cache_hits = cache.pipeline_cache_hits;
        self.stats.compile_waits = cache.compile_waits;
        self.stats.compile_wait_ns = cache.compile_wait_ns;
        self.stats.prebuild_hits = cache.prebuild_hits;
        self.stats.prebuild_misses = cache.prebuild_misses;
        self.stats.persisted_pipelines = cache.persisted_pipelines;
        self.stats.discarded_pipelines = cache.discarded_pipelines;
    }
}

struct Archive {
    path: PathBuf,
    identity: String,
    recipes: VecDeque<Recipe>,
    identities: HashMap<[u8; 32], Option<u64>>,
    order: VecDeque<[u8; 32]>,
    sizes: VecDeque<usize>,
    bytes: usize,
}
impl Archive {
    fn remember(&mut self, recipe: Recipe, key: [u8; 32], size: usize, identity: u64) -> bool {
        if let Some(stored) = self.identities.get_mut(&key) {
            *stored = Some(identity);
            return false;
        }
        if size > ARCHIVE_LIMIT / 2
            || self
                .identities
                .values()
                .any(|&stored| stored == Some(identity))
        {
            return false;
        }
        while self.recipes.len() == MAX_PIPELINES || self.bytes + size > ARCHIVE_LIMIT / 2 {
            self.recipes.pop_front().unwrap();
            self.identities.remove(&self.order.pop_front().unwrap());
            self.bytes -= self.sizes.pop_front().unwrap();
        }
        self.recipes.push_back(recipe);
        self.identities.insert(key, Some(identity));
        self.order.push_back(key);
        self.sizes.push_back(size);
        self.bytes += size;
        true
    }
}
#[derive(Serialize, Deserialize)]
struct Disk {
    version: u32,
    identity: String,
    recipes: VecDeque<Recipe>,
    driver: Vec<u8>,
}
fn load_archive(path: &Path, identity: &str) -> Option<(VecDeque<Recipe>, Vec<u8>)> {
    use std::io::Read;
    let file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    if !(40..=ARCHIVE_LIMIT as u64).contains(&len) {
        return None;
    }
    let mut bytes = Vec::with_capacity(len as usize);
    file.take(ARCHIVE_LIMIT as u64 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 != len
        || &bytes[..8] != MAGIC
        || Sha256::digest(&bytes[40..]).as_slice() != &bytes[8..40]
    {
        return None;
    }
    let disk: Disk = serde_json::from_slice(&bytes[40..]).ok()?;
    if disk.version != 1
        || disk.identity != identity
        || disk.recipes.len() > MAX_PIPELINES
        || disk.recipes.iter().any(|r| !valid_recipe(r))
    {
        return None;
    }
    Some((disk.recipes, disk.driver))
}
fn valid_recipe(recipe: &Recipe) -> bool {
    !recipe.colors.is_empty()
        && recipe.colors.len() <= p::MAX_COLOR_TARGETS
        && recipe.colors.iter().all(|&f| f < 2)
        && recipe
            .depth
            .is_none_or(|f| (2..FORMATS.len() as u32).contains(&f))
        && recipe.raster.stencil.is_none_or(|_| {
            recipe
                .depth
                .is_some_and(|f| FORMATS[f as usize].has_stencil())
        })
        && recipe.raster.color_write <= 15
        && match &recipe.kind {
            Kind::Program {
                source,
                textures,
                modes,
                streams,
            } => {
                gpu::program_module(source).is_ok_and(|module| {
                    module.entry_points.iter().any(|e| {
                        e.name == "vertex_main" && e.stage == wgpu::naga::ShaderStage::Vertex
                    }) && module.entry_points.iter().any(|e| {
                        e.name == "fragment_main" && e.stage == wgpu::naga::ShaderStage::Fragment
                    })
                }) && textures.iter().filter(|&&vs| vs).count() <= 4
                    && textures.iter().filter(|&&vs| !vs).count() <= 16
                    && modes.len() == textures.len()
                    && modes.iter().all(|&m| m <= 2)
                    && streams.len() <= 16
                    && streams.iter().map(|s| s.1.len()).sum::<usize>() <= 16
                    && streams.iter().all(|(stride, attrs, _)| {
                        *stride <= 2048
                            && stride.is_multiple_of(4)
                            && !attrs.is_empty()
                            && attrs.iter().all(|a| {
                                a.location < 16
                                    && a.offset.is_multiple_of(4)
                                    && a.offset
                                        .checked_add(a.format.size())
                                        .is_some_and(|end| *stride == 0 || end <= *stride)
                            })
                    })
            }
            _ => true,
        }
}
fn save_archive(
    path: &Path,
    identity: &str,
    recipes: &VecDeque<Recipe>,
    cache: &p::PipelineCache,
) -> std::io::Result<()> {
    use std::io::Write;
    let disk = Disk {
        version: 1,
        identity: identity.to_owned(),
        recipes: recipes.clone(),
        driver: cache.serialize().map_err(std::io::Error::other)?,
    };
    let bytes = serde_json::to_vec(&disk)?;
    if bytes.len() + 40 > ARCHIVE_LIMIT {
        return Err(std::io::ErrorKind::InvalidData.into());
    }
    let directory = path.parent().unwrap();
    std::fs::create_dir_all(directory)?;
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let temporary = directory.join(format!(
        ".native-{}-{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(MAGIC)?;
        file.write_all(&Sha256::digest(&bytes))?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result?;
    let mut partitions: Vec<_> = std::fs::read_dir(directory)?
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|s| s == "native"))
        .collect();
    partitions.sort_by_key(|entry| entry.metadata().and_then(|m| m.modified()).ok());
    let remove = partitions.len().saturating_sub(4);
    for old in partitions.into_iter().take(remove) {
        let _ = std::fs::remove_file(old.path());
    }
    Ok(())
}

fn build(
    device: &Device,
    recipe: &Recipe,
    cache: Option<&p::PipelineCache>,
) -> Result<Built, Error> {
    let source = match &recipe.kind {
        Kind::Fixed | Kind::Blit(_) => fixed_source()?,
        Kind::Program {
            source,
            textures,
            modes,
            streams,
        } => program_source(source, textures, modes, streams)?,
    };
    let bindings = bindings(&source)?;
    let shader = device
        .create_shader(ShaderDesc {
            source: Source::Wgsl(&source),
            identity: digest(source.as_bytes()),
            label: "native executor",
        })
        .map_err(gpu_error)?;
    let r = recipe.raster;
    let desc = p::RenderPipelineDesc {
        vertex: shader
            .entry("vertex_main", Stage::Vertex)
            .map_err(gpu_error)?,
        fragment: Some(
            shader
                .entry("fragment_main", Stage::Fragment)
                .map_err(gpu_error)?,
        ),
        bindings,
        primitive: p::Primitive {
            topology: match r.topology {
                gpu::Topology::Points => p::Topology::Points,
                gpu::Topology::Lines => p::Topology::Lines,
                gpu::Topology::LineStrip => p::Topology::LineStrip,
                gpu::Topology::Triangles => p::Topology::Triangles,
                gpu::Topology::TriangleStrip => p::Topology::TriangleStrip,
            },
            cull: match r.cull {
                gpu::Cull::None => p::Cull::None,
                gpu::Cull::Clockwise => p::Cull::Back,
                gpu::Cull::CounterClockwise => p::Cull::Front,
            },
            front_ccw: true,
            wireframe: false,
            unclipped_depth: false,
        },
        colors: recipe
            .colors
            .iter()
            .map(|&f| p::ColorTarget {
                format: FORMATS[f as usize],
                blend: r
                    .color_blend
                    .map(|b| (blend(b), blend(r.alpha_blend.unwrap_or(b)))),
                write_mask: r.color_write,
            })
            .collect(),
        depth_stencil: recipe.depth.map(|f| p::DepthStencil {
            format: FORMATS[f as usize],
            depth_compare: compare(r.depth.unwrap_or(gpu::Compare::Always)),
            depth_write: r.depth_write && r.depth.is_some(),
            stencil: r.stencil.map(|s| (stencil(s.front), stencil(s.back))),
            stencil_read_mask: r.stencil.map_or(255, |s| s.read_mask as u8),
            stencil_write_mask: r.stencil.map_or(255, |s| s.write_mask as u8),
            depth_bias: false,
        }),
        samples: 1,
        alpha_to_coverage: false,
        label: "native executor",
    };
    // Backend identity removes dynamic state from its driver-object key; the
    // device shares that compilation while retaining each description's state.
    let identity = device.pipeline_identity(&desc);
    let pipeline = device
        .create_render_pipeline(&desc, cache)
        .map_err(gpu_error)?;
    Ok(Built { pipeline, identity })
}
fn compare(c: gpu::Compare) -> p::Compare {
    match c {
        gpu::Compare::Never => p::Compare::Never,
        gpu::Compare::Less => p::Compare::Less,
        gpu::Compare::Equal => p::Compare::Equal,
        gpu::Compare::LessEqual => p::Compare::LessEqual,
        gpu::Compare::Greater => p::Compare::Greater,
        gpu::Compare::NotEqual => p::Compare::NotEqual,
        gpu::Compare::GreaterEqual => p::Compare::GreaterEqual,
        gpu::Compare::Always => p::Compare::Always,
    }
}
fn stencil(s: gpu::StencilFace) -> p::StencilFace {
    let op = |op| match op {
        gpu::StencilOp::Keep => p::StencilOp::Keep,
        gpu::StencilOp::Zero => p::StencilOp::Zero,
        gpu::StencilOp::Replace => p::StencilOp::Replace,
        gpu::StencilOp::IncrementClamp => p::StencilOp::IncrementClamp,
        gpu::StencilOp::DecrementClamp => p::StencilOp::DecrementClamp,
        gpu::StencilOp::Invert => p::StencilOp::Invert,
        gpu::StencilOp::IncrementWrap => p::StencilOp::IncrementWrap,
        gpu::StencilOp::DecrementWrap => p::StencilOp::DecrementWrap,
    };
    p::StencilFace {
        compare: compare(s.compare),
        fail: op(s.fail),
        depth_fail: op(s.depth_fail),
        pass: op(s.pass),
    }
}
fn blend(b: gpu::Blend) -> p::BlendComponent {
    let factor = |f| match f {
        gpu::Factor::Zero => p::Factor::Zero,
        gpu::Factor::One => p::Factor::One,
        gpu::Factor::SourceColor => p::Factor::Src,
        gpu::Factor::OneMinusSourceColor => p::Factor::OneMinusSrc,
        gpu::Factor::SourceAlpha => p::Factor::SrcAlpha,
        gpu::Factor::OneMinusSourceAlpha => p::Factor::OneMinusSrcAlpha,
        gpu::Factor::DestinationColor => p::Factor::Dst,
        gpu::Factor::OneMinusDestinationColor => p::Factor::OneMinusDst,
        gpu::Factor::DestinationAlpha => p::Factor::DstAlpha,
        gpu::Factor::OneMinusDestinationAlpha => p::Factor::OneMinusDstAlpha,
        gpu::Factor::SourceAlphaSaturated => p::Factor::SrcAlphaSaturated,
        gpu::Factor::Constant => p::Factor::Constant,
        gpu::Factor::OneMinusConstant => p::Factor::OneMinusConstant,
    };
    p::BlendComponent {
        src: factor(b.source),
        dst: factor(b.destination),
        op: match b.operation {
            gpu::BlendOp::Add => p::BlendOp::Add,
            gpu::BlendOp::Subtract => p::BlendOp::Subtract,
            gpu::BlendOp::ReverseSubtract => p::BlendOp::ReverseSubtract,
            gpu::BlendOp::Minimum => p::BlendOp::Min,
            gpu::BlendOp::Maximum => p::BlendOp::Max,
        },
    }
}
fn fixed_source() -> Result<String, Error> {
    let source = format!("{}\n{}", border_source("image", ""), FIXED)
        .replace("adv_border_sample(image, ", "adv_border_sample(")
        .replace("@group(0) @binding(0)", &format!("@group(1) @binding({})", binding_slot(slot::TEXTURES)))
        .replace("@group(0) @binding(1)", &format!("@group(2) @binding({})", binding_slot(slot::SAMPLERS)))
        .replace("@group(0) @binding(2)", &format!("@group(0) @binding({})", slot::FRAGMENT_UNIFORMS))
        .replace("    if (parameters.alpha_op & 0x80000000u)", "    if (parameters.alpha_op & 0x40000000u) != 0u { texel = vec4<f32>(0.0,0.0,0.0,texel.r); }\n    if (parameters.alpha_op & 0x80000000u)")
        .replace("0x7fffffffu", "0x3fffffffu");
    pulling(
        &source,
        &[(
            40,
            vec![
                Attribute {
                    location: 0,
                    offset: 0,
                    format: AttributeFormat::Float4,
                },
                Attribute {
                    location: 1,
                    offset: 16,
                    format: AttributeFormat::Float4,
                },
                Attribute {
                    location: 2,
                    offset: 32,
                    format: AttributeFormat::Float2,
                },
            ],
            false,
        )],
    )
}

fn program_source(
    source: &str,
    textures: &[bool],
    modes: &[u8],
    streams: &[(u32, Vec<Attribute>, bool)],
) -> Result<String, Error> {
    let mut source = source.to_owned();
    // Frontend programs share parameters between stages. Append them to the
    // vertex constant block, preserving both public constant block layouts.
    let declaration = "@group(0) @binding(0) var<uniform> vs_constants:ShaderConstants;";
    if source.contains(declaration) {
        source = source.replace(declaration, "struct AdvVertexUniforms { constants:ShaderConstants, parameters:DrawParameters }\n@group(0) @binding(0) var<uniform> adv_vertex:AdvVertexUniforms;");
        source = source
            .replace(
                "@group(0) @binding(2) var<uniform> draw_parameters:DrawParameters;",
                "",
            )
            .replace("vs_constants.", "adv_vertex.constants.")
            .replace("draw_parameters.", "adv_vertex.parameters.");
    }
    let module = parse(&source)?;
    let mut replacements = Vec::new();
    let border = source
        .find(gpu::BORDER_WGSL)
        .map(|start| start..start + gpu::BORDER_WGSL.len());
    if let Some(range) = &border {
        replacements.push((range.clone(), String::new()));
    }
    let tokens = tokens(&source);
    let mut pixel = 0;
    let mut vertex = 16;
    for (ordinal, &vs) in textures.iter().enumerate() {
        let n = if vs {
            let n = vertex;
            vertex += 1;
            n
        } else {
            let n = pixel;
            pixel += 1;
            n
        };
        for (handle, global) in module.global_variables.iter() {
            let _ = handle;
            let Some(binding) = &global.binding else {
                continue;
            };
            if binding.group != 1 || binding.binding / 2 != ordinal as u32 {
                continue;
            }
            let group = if binding.binding % 2 == 0 { 1 } else { 2 };
            let slot = binding_slot(if group == 1 {
                slot::TEXTURES + n
            } else {
                slot::SAMPLERS + n
            });
            // Locate the binding attributes of this global in token space;
            // comments and whitespace have no semantic effect.
            let name = global.name.as_deref().unwrap();
            let index = tokens
                .windows(2)
                .position(|t| t[0].0 == "var" && t[1].0 == name)
                .unwrap()
                + 1;
            let start = tokens[..index]
                .windows(2)
                .rposition(|t| t[0].0 == "@" && t[1].0 == "group")
                .unwrap();
            let end = tokens[start..index]
                .iter()
                .position(|t| t.0 == "var")
                .map(|i| start + i)
                .unwrap();
            replacements.push((
                tokens[start].1..tokens[end].1,
                format!("@group({group}) @binding({slot}) "),
            ));
            if group == 1 && border.is_some() {
                let suffix = format!("_native_{ordinal}");
                for (i, token) in tokens.iter().enumerate() {
                    if token.0 == "adv_border_sample"
                        && tokens.get(i + 2).is_some_and(|t| t.0 == name)
                        && border.as_ref().is_none_or(|r| !r.contains(&token.1))
                    {
                        replacements.push((token.1..token.2, format!("adv_border_sample{suffix}")));
                        replacements.push((tokens[i + 2].1..tokens[i + 3].2, String::new()));
                    }
                }
                replacements.push((source.len()..source.len(), border_source(name, &suffix)));
            }
            if group == 1 && modes[ordinal] != 0 {
                let alpha = name.replace("_image_", "_alpha_");
                if source.contains(&format!("fn {alpha}(")) {
                    let body = function_body(&source, &alpha);
                    let value = if modes[ordinal] == 2 {
                        "vec4<f32>(0.0,0.0,0.0,value.r)"
                    } else {
                        "vec4<f32>(value.rgb,1.0)"
                    };
                    replacements.push((body, format!("return {value};")));
                } else {
                    // Hand-authored programs use ordinary texture operations.
                    for (i, token) in tokens.iter().enumerate() {
                        if (token.0.starts_with("textureSample") || token.0 == "textureLoad")
                            && tokens.get(i + 1).is_some_and(|t| t.0 == "(")
                            && tokens.get(i + 2).is_some_and(|t| t.0 == name)
                        {
                            let close = closing(&tokens, i + 1, "(", ")");
                            let range = token.1..tokens[close].2;
                            replacements.push((
                                range.clone(),
                                format!("adv_texel_{ordinal}({})", &source[range]),
                            ));
                        }
                    }
                    let value = if modes[ordinal] == 2 {
                        "vec4<f32>(0.0,0.0,0.0,value.r)"
                    } else {
                        "vec4<f32>(value.rgb,1.0)"
                    };
                    replacements.push((source.len()..source.len(), format!("\nfn adv_texel_{ordinal}(value:vec4<f32>)->vec4<f32>{{return {value};}}\n")));
                }
            }
        }
    }
    replace(&mut source, replacements);
    pulling(&source, streams)
}

// Resource arguments to shared border helpers produce invalid SPIR-V in the
// native translator. Specialize the same filtering code for each bound image.
fn border_source(image: &str, suffix: &str) -> String {
    let mut source = gpu::BORDER_WGSL
        .replace("image: texture_2d<f32>, ", "")
        .replace("adv_border_texel(image, ", "adv_border_texel(")
        .replace("adv_border_level(image, ", "adv_border_level(");
    let edits = tokens(&source)
        .into_iter()
        .filter_map(|(token, start, end)| {
            if token == "image" {
                Some((start..end, image.to_owned()))
            } else if [
                "adv_address_texel",
                "adv_border_texel",
                "adv_border_level",
                "adv_border_sample",
            ]
            .contains(&token)
            {
                Some((start..end, format!("{token}{suffix}")))
            } else {
                None
            }
        })
        .collect();
    replace(&mut source, edits);
    source
}

fn parse(source: &str) -> Result<wgpu::naga::Module, Error> {
    wgpu::naga::front::wgsl::parse_str(source).map_err(|e| Error::Invalid(e.emit_to_string(source)))
}
// The wrapper edits only the validated vertex entry's header. Its body and
// the frontend's attribute unpack expressions are retained verbatim.
fn pulling(source: &str, streams: &[(u32, Vec<Attribute>, bool)]) -> Result<String, Error> {
    use wgpu::naga::{Binding, BuiltIn, TypeInner};
    let module = parse(source)?;
    let entry = module
        .entry_points
        .iter()
        .find(|e| e.name == "vertex_main" && e.stage == wgpu::naga::ShaderStage::Vertex)
        .ok_or_else(|| Error::Invalid("program has no vertex_main".into()))?;
    let fetch = |binding: &Binding| -> Result<String, Error> {
        match binding {
            Binding::BuiltIn(BuiltIn::VertexIndex) => Ok("adv_vertex_index".into()),
            Binding::BuiltIn(BuiltIn::InstanceIndex) => Ok("adv_instance_index".into()),
            Binding::Location { location, .. } => {
                for (n, (stride, attrs, instance)) in streams.iter().enumerate() {
                    if let Some(a) = attrs.iter().find(|a| a.location == *location) {
                        if !stride.is_multiple_of(4)
                            || !a.offset.is_multiple_of(4)
                            || (*stride != 0
                                && a.offset
                                    .checked_add(a.format.size())
                                    .is_none_or(|end| end > *stride))
                        {
                            return Err(Error::Invalid(
                                "vertex attribute outside stream stride".into(),
                            ));
                        }
                        return Ok(fetch(n, *stride, a, *instance));
                    }
                }
                Err(Error::Invalid(
                    "vertex input has no stream attribute".into(),
                ))
            }
            _ => Err(Error::Invalid("unsupported vertex builtin".into())),
        }
    };
    let mut arguments = Vec::new();
    for argument in &entry.function.arguments {
        if let Some(binding) = &argument.binding {
            arguments.push(fetch(binding)?);
        } else if let TypeInner::Struct { members, .. } = &module.types[argument.ty].inner {
            let values = members
                .iter()
                .map(|m| fetch(m.binding.as_ref().unwrap()))
                .collect::<Result<Vec<_>, _>>()?;
            arguments.push(format!(
                "{}({})",
                module.types[argument.ty].name.as_ref().unwrap(),
                values.join(",")
            ));
        } else {
            return Err(Error::Invalid("unbound vertex input".into()));
        }
    }
    let tokens = tokens(source);
    let function = tokens
        .windows(2)
        .position(|t| t[0].0 == "fn" && t[1].0 == "vertex_main")
        .unwrap();
    let open = function + 2;
    let close = closing(&tokens, open, "(", ")");
    let body = close + tokens[close..].iter().position(|t| t.0 == "{").unwrap();
    let mut result = source.to_owned();
    let mut edits = vec![(
        tokens[function + 1].1..tokens[function + 1].2,
        "adv_guest_vertex".into(),
    )];
    let vertex = tokens[..function]
        .windows(2)
        .rposition(|t| t[0].0 == "@" && t[1].0 == "vertex")
        .unwrap();
    edits.push((tokens[vertex].1..tokens[vertex + 1].2, String::new()));
    let mut at = open;
    while at < body {
        if tokens[at].0 == "@" {
            let end = if tokens.get(at + 2).is_some_and(|t| t.0 == "(") {
                closing(&tokens, at + 2, "(", ")")
            } else {
                at + 1
            };
            edits.push((tokens[at].1..tokens[end].2, String::new()));
            at = end + 1;
        } else {
            at += 1;
        }
    }
    let output = source[tokens[close].2..tokens[body].1].trim();
    replace(&mut result, edits);
    for n in 0..streams.len() {
        result.push_str(&format!(
            "\n@group(3) @binding({}) var<storage,read> adv_stream_{n}:array<u32>;\n",
            binding_slot(slot::STREAMS + n as u32)
        ));
    }
    result.push_str(&format!("\n@vertex fn vertex_main(@builtin(vertex_index) adv_vertex_index:u32,@builtin(instance_index) adv_instance_index:u32) {output} {{ return adv_guest_vertex({}); }}\n", arguments.join(",")));
    Ok(result)
}
fn fetch(stream: usize, stride: u32, attribute: &Attribute, instance: bool) -> String {
    use AttributeFormat as A;
    let index = if instance {
        "adv_instance_index"
    } else {
        "adv_vertex_index"
    };
    let word = |n| {
        format!(
            "adv_stream_{stream}[{index}*{}u+{}u]",
            stride / 4,
            attribute.offset / 4 + n
        )
    };
    let floats = |n| {
        (0..n)
            .map(|i| format!("bitcast<f32>({})", word(i)))
            .collect::<Vec<_>>()
            .join(",")
    };
    match attribute.format {
        A::Float1 => format!("bitcast<f32>({})", word(0)),
        A::Float2 => format!("vec2<f32>({})", floats(2)),
        A::Float3 => format!("vec3<f32>({})", floats(3)),
        A::Float4 => format!("vec4<f32>({})", floats(4)),
        A::Bgra8Unorm | A::Unorm8x4 => format!("unpack4x8unorm({})", word(0)),
        A::Uint8x4 => format!(
            "vec4<u32>({w}&255u,({w}>>8u)&255u,({w}>>16u)&255u,{w}>>24u)",
            w = word(0)
        ),
        A::Sint16x2 | A::Sint16x4 => {
            let n = if attribute.format == A::Sint16x2 {
                2
            } else {
                4
            };
            let values = (0..n)
                .map(|i| {
                    if i % 2 == 0 {
                        format!("bitcast<i32>({}<<16u)>>16", word(i / 2))
                    } else {
                        format!("bitcast<i32>({})>>16", word(i / 2))
                    }
                })
                .collect::<Vec<_>>()
                .join(",");
            format!("vec{n}<i32>({values})")
        }
        A::Snorm16x2 => format!("unpack2x16snorm({})", word(0)),
        A::Snorm16x4 => format!(
            "vec4<f32>(unpack2x16snorm({}),unpack2x16snorm({}))",
            word(0),
            word(1)
        ),
        A::Unorm16x2 => format!("unpack2x16unorm({})", word(0)),
        A::Unorm16x4 => format!(
            "vec4<f32>(unpack2x16unorm({}),unpack2x16unorm({}))",
            word(0),
            word(1)
        ),
        A::Float16x2 => format!("unpack2x16float({})", word(0)),
        A::Float16x4 => format!(
            "vec4<f32>(unpack2x16float({}),unpack2x16float({}))",
            word(0),
            word(1)
        ),
        A::Uint10x3 | A::Snorm10x3 => word(0),
    }
}
fn bindings(source: &str) -> Result<Vec<p::Binding>, Error> {
    use wgpu::naga::{AddressSpace, TypeInner};
    let module = parse(source)?;
    let info = wgpu::naga::valid::Validator::new(
        wgpu::naga::valid::ValidationFlags::all(),
        wgpu::naga::valid::Capabilities::SHADER_FLOAT16_IN_FLOAT32,
    )
    .validate(&module)
    .map_err(|e| Error::Invalid(e.emit_to_string(source)))?;
    let mut bindings = Vec::new();
    for (handle, variable) in module.global_variables.iter() {
        let Some(binding) = &variable.binding else {
            continue;
        };
        let mut stages = p::Stages::empty();
        for (n, entry) in module.entry_points.iter().enumerate() {
            if !info.get_entry_point(n)[handle].is_empty() {
                stages |= match entry.stage {
                    wgpu::naga::ShaderStage::Vertex => p::Stages::VERTEX,
                    wgpu::naga::ShaderStage::Fragment => p::Stages::FRAGMENT,
                    _ => p::Stages::COMPUTE,
                };
            }
        }
        if stages.is_empty() {
            continue;
        }
        let (group, kind) = match variable.space {
            AddressSpace::Uniform => (0, p::BindingKind::UniformBuffer),
            AddressSpace::Storage { access }
                if !access.contains(wgpu::naga::StorageAccess::STORE) =>
            {
                (3, p::BindingKind::StorageBuffer { writable: false })
            }
            AddressSpace::Handle => match module.types[variable.ty].inner {
                TypeInner::Image {
                    dim: wgpu::naga::ImageDimension::D2,
                    arrayed: false,
                    class:
                        wgpu::naga::ImageClass::Sampled {
                            kind: wgpu::naga::ScalarKind::Float,
                            multi: false,
                        },
                } => (
                    1,
                    p::BindingKind::SampledImage {
                        dimension: adv_gpu::resource::Dimension::D2,
                        depth: false,
                        multisampled: false,
                    },
                ),
                TypeInner::Sampler { comparison: false } => {
                    (2, p::BindingKind::Sampler { comparison: false })
                }
                _ => return Err(Error::Invalid("unsupported pipeline binding".into())),
            },
            _ => return Err(Error::Invalid("unsupported pipeline binding".into())),
        };
        if group != binding.group || binding.binding >= [2, 20, 20, 16][group as usize] {
            return Err(Error::Invalid(
                "pipeline binding is outside executor slots".into(),
            ));
        }
        bindings.push(p::Binding {
            slot: binding.binding,
            kind,
            stages,
        });
    }
    Ok(bindings)
}
fn replace(source: &mut String, mut edits: Vec<(std::ops::Range<usize>, String)>) {
    edits.sort_by_key(|edit| std::cmp::Reverse(edit.0.start));
    for (range, text) in edits {
        source.replace_range(range, &text);
    }
}
// Token offsets let header edits respect arbitrary whitespace and comments.
fn tokens(source: &str) -> Vec<(&str, usize, usize)> {
    let bytes = source.as_bytes();
    let mut result = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at].is_ascii_whitespace() {
            at += 1;
            continue;
        }
        if source[at..].starts_with("//") {
            at = source[at..].find('\n').map_or(bytes.len(), |n| at + n);
            continue;
        }
        if source[at..].starts_with("/*") {
            let mut depth = 1;
            at += 2;
            while depth != 0 {
                if source[at..].starts_with("/*") {
                    depth += 1;
                    at += 2;
                } else if source[at..].starts_with("*/") {
                    depth -= 1;
                    at += 2;
                } else {
                    at += source[at..].chars().next().unwrap().len_utf8();
                }
            }
            continue;
        }
        let start = at;
        if bytes[at].is_ascii_alphanumeric() || bytes[at] == b'_' {
            while at < bytes.len() && (bytes[at].is_ascii_alphanumeric() || bytes[at] == b'_') {
                at += 1;
            }
        } else {
            at += source[at..].chars().next().unwrap().len_utf8();
        }
        result.push((&source[start..at], start, at));
    }
    result
}
fn closing(tokens: &[(&str, usize, usize)], open: usize, left: &str, right: &str) -> usize {
    let mut depth = 0;
    for (i, token) in tokens.iter().enumerate().skip(open) {
        if token.0 == left {
            depth += 1;
        } else if token.0 == right {
            depth -= 1;
            if depth == 0 {
                return i;
            }
        }
    }
    unreachable!("validated shader delimiter")
}
fn function_body(source: &str, name: &str) -> std::ops::Range<usize> {
    let tokens = tokens(source);
    let start = tokens
        .windows(2)
        .position(|t| t[0].0 == "fn" && t[1].0 == name)
        .unwrap();
    let open = start + tokens[start..].iter().position(|t| t.0 == "{").unwrap();
    tokens[open].2..tokens[closing(&tokens, open, "{", "}")].1
}

#[cfg(test)]
mod tests {
    use super::super::resources::tests::{open, render_stream, sample, upload_chunk};
    use super::*;
    use gpu::{ConstantBlock, ConstantSnapshot, VertexBytes, VertexStream};

    fn source(format: AttributeFormat) -> Arc<str> {
        format!(
            r#"
struct V {{ @builtin(position) position:vec4<f32>, @location(0) color:vec4<f32> }}
@vertex fn vertex_main(@builtin(vertex_index) n:u32, @location(0) a:{ty})->V {{
    let positions=array<vec2<f32>,3>(vec2<f32>(-1.0,-1.0),vec2<f32>(3.0,-1.0),vec2<f32>(-1.0,3.0));
    return V(vec4<f32>(positions[n],0.0,1.0),{value});
}}
@fragment fn fragment_main(v:V)->@location(0) vec4<f32> {{ return v.color; }}
"#,
            ty = format.wgsl_type(),
            value = format.unpack("a")
        )
        .into()
    }
    fn draw(format: AttributeFormat, bytes: Vec<u8>) -> ProgrammableDraw {
        ProgrammableDraw {
            instance_count: 1,
            program: Program::new(source(format), vec![]),
            streams: vec![VertexStream {
                instance: false,
                bytes: VertexBytes::from(bytes),
                stride: format.size(),
                attributes: vec![Attribute {
                    location: 0,
                    offset: 0,
                    format,
                }],
            }],
            indices: None,
            vertex_count: 3,
            vertex_constants: ConstantSnapshot::default(),
            pixel_constants: ConstantSnapshot::default(),
            textures: vec![],
            parameters: [[0; 4]; 40],
        }
    }
    // spec: GFX-INPUT-1, GFX-CACHE-8
    #[test]
    fn every_recipe_and_attribute_fetch_compiles_and_executes() {
        let Some((mut gpu, mut queue, consumer)) = open() else {
            return;
        };
        let image = gpu.image(1, 1).unwrap();
        gpu.upload(
            &image,
            gpu::Rect {
                x: 0,
                y: 0,
                w: 1,
                h: 1,
            },
            4,
            &[255; 4],
        )
        .unwrap();
        upload_chunk(&mut gpu, &mut queue, &consumer);
        assert_eq!(sample(&mut gpu, &mut queue, &image), [255; 4]);
        for filter in [gpu::Filter::Nearest, gpu::Filter::Linear] {
            let pipeline = gpu
                .pipelines
                .blit(&gpu.device, filter, Format::Bgra8Unorm)
                .unwrap();
            let target = gpu.target_levels(1, 1, 1).unwrap();
            assert_eq!(
                super::super::resources::tests::render(
                    &mut gpu,
                    &mut queue,
                    &target.image,
                    &pipeline,
                    &image,
                    bytemuck::cast_slice(&[1u32, 1, 7, 0, 0, 0, 0, 0])
                ),
                [255; 4]
            );
        }
        use AttributeFormat as A;
        for (format, packed, expected) in [
            (
                A::Float1,
                bytemuck::cast_slice(&[0.25f32]).to_vec(),
                [0, 0, 64, 255],
            ),
            (
                A::Float2,
                bytemuck::cast_slice(&[0.25f32, 0.5]).to_vec(),
                [0, 128, 64, 255],
            ),
            (
                A::Float3,
                bytemuck::cast_slice(&[0.25f32, 0.5, 0.75]).to_vec(),
                [191, 128, 64, 255],
            ),
            (
                A::Float4,
                bytemuck::cast_slice(&[0.25f32, 0.5, 0.75, 1.0]).to_vec(),
                [191, 128, 64, 255],
            ),
            (A::Bgra8Unorm, vec![11, 22, 33, 44], [11, 22, 33, 44]),
            (A::Uint8x4, vec![0, 1, 0, 1], [0, 255, 0, 255]),
            (
                A::Sint16x2,
                bytemuck::cast_slice(&[1i16, 0]).to_vec(),
                [0, 0, 255, 255],
            ),
            (
                A::Sint16x4,
                bytemuck::cast_slice(&[-1i16, 0, 1, 1]).to_vec(),
                [255, 0, 0, 255],
            ),
            (A::Unorm8x4, vec![11, 22, 33, 44], [33, 22, 11, 44]),
            (
                A::Snorm16x2,
                bytemuck::cast_slice(&[16384i16, -16384]).to_vec(),
                [0, 0, 128, 255],
            ),
            (
                A::Snorm16x4,
                bytemuck::cast_slice(&[16384i16, -16384, 32767, 32767]).to_vec(),
                [255, 0, 128, 255],
            ),
            (
                A::Unorm16x2,
                bytemuck::cast_slice(&[16384u16, 32768]).to_vec(),
                [0, 128, 64, 255],
            ),
            (
                A::Unorm16x4,
                bytemuck::cast_slice(&[16384u16, 32768, 49152, 65535]).to_vec(),
                [191, 128, 64, 255],
            ),
            (
                A::Uint10x3,
                (1u32 << 10).to_le_bytes().to_vec(),
                [0, 255, 0, 255],
            ),
            (
                A::Snorm10x3,
                (512u32 | (255 << 10) | (511 << 20)).to_le_bytes().to_vec(),
                [255, 127, 0, 255],
            ),
            (
                A::Float16x2,
                bytemuck::cast_slice(&[0x3400u16, 0x3800]).to_vec(),
                [0, 128, 64, 255],
            ),
            (
                A::Float16x4,
                bytemuck::cast_slice(&[0x3400u16, 0x3800, 0x3a00, 0x3c00]).to_vec(),
                [191, 128, 64, 255],
            ),
        ] {
            let stride = format.size() + 8;
            let mut raw = vec![0; stride as usize * 3];
            raw[..4].fill(0xa5);
            raw[4..4 + packed.len()].copy_from_slice(&packed);
            // With an instance stream, all three vertices must fetch the first
            // attribute, including its nonzero offset and padded stride.
            let mut draw = draw(format, raw.clone());
            draw.streams[0].instance = true;
            draw.streams[0].stride = stride;
            draw.streams[0].attributes[0].offset = 4;
            let pipeline = gpu
                .pipelines
                .programmable(
                    &gpu.device,
                    &draw,
                    &DrawState::new(1, 1),
                    &[Format::Bgra8Unorm],
                    None,
                )
                .unwrap();
            let target = gpu.target_levels(1, 1, 1).unwrap();
            let result = render_stream(
                &mut gpu,
                &mut queue,
                &target.image,
                &pipeline,
                &[&image],
                [&[], &[0; 32]],
                &raw,
            );
            assert_eq!(result, expected, "{format:?}");
        }
    }
    // spec: GFX-COPY-1
    #[test]
    fn programmable_constants_border_helpers_and_per_slot_adapters_execute() {
        let Some((mut gpu, mut queue, consumer)) = open() else {
            return;
        };
        let alpha = gpu.texture(1, 1, 1, gpu::TexelFormat::A8).unwrap();
        let opaque = gpu.texture(1, 1, 1, gpu::TexelFormat::Bgrx8).unwrap();
        let rect = gpu::Rect {
            x: 0,
            y: 0,
            w: 1,
            h: 1,
        };
        gpu.upload_alpha(&alpha, rect, 1, &[77]).unwrap();
        gpu.upload(&opaque, rect, 4, &[11, 22, 33, 0]).unwrap();
        upload_chunk(&mut gpu, &mut queue, &consumer);
        let mut source = String::from(
            r#"
struct ShaderConstants { f:array<vec4<f32>,256>, i:array<vec4<i32>,16>, b:array<vec4<i32>,16>, bump_matrix:array<vec4<f32>,8>, bump_luminance:array<vec4<f32>,8> }
struct DrawParameters { values:array<vec4<u32>,40> }
@group(0) @binding(0) var<uniform> vs_constants:ShaderConstants;
@group(0) @binding(1) var<uniform> ps_constants:ShaderConstants;
@group(0) @binding(2) var<uniform> draw_parameters:DrawParameters;
@group(1) @binding(0) var ps_image_0:texture_2d<f32>;
@group(1) @binding(1) var ps_sampler_0:sampler;
@group(1) @binding(2) var ps_image_1:texture_2d<f32>;
@group(1) @binding(3) var ps_sampler_1:sampler;
fn ps_alpha_0(value:vec4<f32>)->vec4<f32> { return value; }
fn ps_alpha_1(value:vec4<f32>)->vec4<f32> { return value; }
struct V { @builtin(position) position:vec4<f32>, @location(0) uv:vec2<f32> }
@vertex fn vertex_main(@location(0) position:vec4<f32>, @location(1) uv:vec2<f32>)->V {
    return V(position + vs_constants.f[0], uv + vec2<f32>(bitcast<f32>(draw_parameters.values[0].x)));
}
@fragment fn fragment_main(v:V)->@location(0) vec4<f32> {
    let a = ps_alpha_0(textureSample(ps_image_0,ps_sampler_0,v.uv));
    let c = ps_alpha_1(adv_border_sample(ps_image_1,v.uv,0.0,0u,0u,0u,0u));
    return vec4<f32>(c.rgb,a.a*c.a) * ps_constants.f[0];
}
"#,
        );
        source.push_str(gpu::BORDER_WGSL);
        let vertices = [
            gpu::Vertex {
                position: [-1., -1., 0., 1.],
                color: [1.; 4],
                uv: [0., 1.],
            },
            gpu::Vertex {
                position: [3., -1., 0., 1.],
                color: [1.; 4],
                uv: [2., 1.],
            },
            gpu::Vertex {
                position: [-1., 3., 0., 1.],
                color: [1.; 4],
                uv: [0., -1.],
            },
        ];
        let raw = bytemuck::cast_slice(&vertices);
        let mut draw = draw(AttributeFormat::Float4, raw.to_vec());
        draw.program = Program::new(source.into(), vec![false, false]);
        draw.streams[0].stride = 40;
        draw.streams[0].attributes = vec![
            Attribute {
                location: 0,
                offset: 0,
                format: AttributeFormat::Float4,
            },
            Attribute {
                location: 1,
                offset: 32,
                format: AttributeFormat::Float2,
            },
        ];
        let state = DrawState::new(1, 1);
        draw.textures = vec![
            gpu::ProgramTexture {
                image: Some(alpha.clone()),
                sampler: state.sampler,
            },
            gpu::ProgramTexture {
                image: Some(opaque.clone()),
                sampler: state.sampler,
            },
        ];
        let pipeline = gpu
            .pipelines
            .programmable(&gpu.device, &draw, &state, &[Format::Bgra8Unorm], None)
            .unwrap();
        let target = gpu.target_levels(1, 1, 1).unwrap();
        let mut vertex = bytemuck::bytes_of(&ConstantBlock::default()).to_vec();
        vertex.extend_from_slice(bytemuck::bytes_of(&draw.parameters));
        let mut pixel = ConstantBlock::default();
        pixel.floats[0] = [1.; 4];
        assert_eq!(
            render_stream(
                &mut gpu,
                &mut queue,
                &target.image,
                &pipeline,
                &[&alpha, &opaque],
                [&vertex, bytemuck::bytes_of(&pixel)],
                raw
            ),
            [11, 22, 33, 77]
        );
    }

    // spec: GFX-CACHE-7, GFX-CACHE-8, GFX-CACHE-9, GFX-CACHE-10
    #[test]
    fn archives_round_trip_share_demand_and_discard_corruption() {
        let Some((mut gpu, mut queue, consumer)) = open() else {
            return;
        };
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let directory = std::env::temp_dir().join(format!(
            "adv-native-cache-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        gpu.set_pipeline_cache_directory(Some(&directory));
        let image = gpu.image(1, 1).unwrap();
        gpu.upload(
            &image,
            gpu::Rect {
                x: 0,
                y: 0,
                w: 1,
                h: 1,
            },
            4,
            &[11, 22, 33, 44],
        )
        .unwrap();
        upload_chunk(&mut gpu, &mut queue, &consumer);
        assert_eq!(sample(&mut gpu, &mut queue, &image), [11, 22, 33, 44]);
        let draw = draw(AttributeFormat::Float4, vec![0; 48]);
        gpu.pipelines
            .programmable(
                &gpu.device,
                &draw,
                &DrawState::new(1, 1),
                &[Format::Bgra8Unorm],
                None,
            )
            .unwrap();
        gpu.flush_pipeline_cache();
        let store = gpu.pipelines.store.as_ref().unwrap();
        let (path, identity) = (store.path.clone(), store.identity.clone());
        let (loaded, driver) = load_archive(&path, &identity).unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(
            gpu.pipelines.stats().persisted_pipelines,
            loaded.len() as u64
        );
        assert!(load_archive(&path, "another driver").is_none());
        let mut warmed = Pipelines::default();
        warmed.open(&gpu.device, Some(&directory));
        assert_eq!(warmed.jobs.len(), 2);
        let task = warmed.jobs.values().next().unwrap().clone();
        let background = task.clone();
        let worker = std::thread::spawn(move || {
            background.resolve().as_ref().unwrap().as_ref().unwrap();
        });
        warmed
            .fixed(
                &gpu.device,
                &DrawState::new(1, 1),
                &[Format::Bgra8Unorm],
                None,
            )
            .unwrap();
        worker.join().unwrap();
        assert!(task.ready().unwrap().as_ref().unwrap().is_ok());
        assert_eq!(warmed.hits, 1);
        warmed.flush();
        drop(warmed);
        let mut bytes = std::fs::read(&path).unwrap();
        *bytes.last_mut().unwrap() ^= 1;
        std::fs::write(&path, bytes).unwrap();
        assert!(load_archive(&path, &identity).is_none());
        let mut malformed = gpu.pipelines.store.as_ref().unwrap().recipes.clone();
        let program = malformed
            .iter_mut()
            .find(|r| matches!(r.kind, Kind::Program { .. }))
            .unwrap();
        let Kind::Program { source, .. } = &mut program.kind else {
            unreachable!()
        };
        *source = "invalid WGSL".into();
        // Keep a valid driver blob and checksum while corrupting a recipe.
        let disk = Disk {
            version: 1,
            identity: identity.clone(),
            recipes: malformed,
            driver,
        };
        let payload = serde_json::to_vec(&disk).unwrap();
        let checksum = Sha256::digest(&payload);
        std::fs::write(
            &path,
            [MAGIC.as_slice(), checksum.as_slice(), &payload].concat(),
        )
        .unwrap();
        assert!(load_archive(&path, &identity).is_none());
        let mut rejected = Pipelines::default();
        rejected.open(&gpu.device, Some(&directory));
        assert_eq!(rejected.discarded, 1);
        assert!(rejected.jobs.is_empty());
        rejected
            .fixed(
                &gpu.device,
                &DrawState::new(1, 1),
                &[Format::Bgra8Unorm],
                None,
            )
            .unwrap();
        rejected.flush();
        drop(rejected);
        gpu.set_pipeline_cache_directory(None);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
