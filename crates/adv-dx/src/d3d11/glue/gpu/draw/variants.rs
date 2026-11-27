use super::*;
use crate::d3d11::desc::Shader;
use crate::shader::ir::passes::InterfaceBinding;
use adv_device::gpu::Dispatch;
use rustc_hash::FxHashMap;
use smallvec::SmallVec;
use std::{collections::hash_map::Entry, hash::Hash, rc::Rc};

#[derive(Clone, PartialEq, Eq, Hash)]
pub(super) struct ShaderIdentity {
    object: u32,
    specialization: Option<Arc<[InterfaceBinding]>>,
}

impl ShaderIdentity {
    pub(super) fn new(snapshot: &Snapshot, stage: Stage, shader: &Shader) -> Self {
        Self {
            object: snapshot.stages[stage as usize].shader,
            specialization: shader.specialization.clone(),
        }
    }
}

#[derive(PartialEq, Eq, Hash)]
pub(super) struct StageInput {
    pub shader: ShaderIdentity,
    pub variant: Variant,
    pub options: StageOptions,
}

impl StageInput {
    pub(super) fn expansion<'a>(&'a self, shader: &'a Shader) -> expand::Shader<'a> {
        expand::Shader {
            ir: shader.module(),
            variant: &self.variant,
            options: self.options.wgsl(),
        }
    }
}

#[derive(PartialEq, Eq, Hash)]
pub(super) struct StageOptions {
    pub sampling: Sampling,
    pub views: ViewOptions,
    pub raster_samples: u32,
    pub raster_sample_mask: u32,
    pub alpha_targets: u32,
    pub image_atomics: bool,
}

impl Default for StageOptions {
    fn default() -> Self {
        Self {
            sampling: Sampling::default(),
            views: ViewOptions::default(),
            raster_samples: 1,
            raster_sample_mask: u32::MAX,
            alpha_targets: 0,
            image_atomics: false,
        }
    }
}

impl StageOptions {
    pub(super) fn wgsl(&self) -> Options<'_> {
        Options {
            raster_samples: self.raster_samples,
            raster_sample_mask: self.raster_sample_mask,
            alpha_targets: self.alpha_targets,
            image_atomics: self.image_atomics,
            ..self.views.options(self.sampling.options())
        }
    }
}

pub(super) type Fetches = SmallVec<[FetchLayout; 8]>;

#[derive(PartialEq, Eq, Hash)]
pub(super) struct VertexKey {
    pub input: StageInput,
    pub fragment: Option<ShaderIdentity>,
    pub fetches: Fetches,
    pub start: DrawStart,
}

#[derive(PartialEq, Eq, Hash)]
pub(super) struct FragmentKey {
    pub input: StageInput,
    pub expanded: Option<(ShaderIdentity, expand::FragmentLayout)>,
}

#[derive(PartialEq, Eq, Hash)]
pub(super) struct ExpansionKey {
    pub vertex: StageInput,
    pub geometry: Option<StageInput>,
    pub hull: Option<StageInput>,
    pub domain: Option<StageInput>,
    pub fetches: Fetches,
    pub assembly: expand::Assembly,
    pub tessellation: Option<expand::Tessellation>,
    pub topology: expand::Topology,
    pub input_vertices: u32,
    pub raster: Option<RasterKey>,
    pub stream_shader: u32,
    pub stream_output: Vec<expand::StreamOutput>,
    pub stream_statistics: u32,
}

#[derive(PartialEq, Eq, Hash)]
pub(super) struct RasterKey {
    pub bins: Vec<Variant>,
    pub fragment: Option<ShaderIdentity>,
    pub stream: u32,
    pub cull: expand::Cull,
    pub front_ccw: bool,
    pub depth_clip: bool,
}

pub(super) struct Translated {
    pub code: Translation,
    pub shader: Arc<ModernShader>,
}

impl Translated {
    pub(super) fn new(code: Translation, stage: adv_gpu::shader::Stage) -> Result<Self, Error> {
        let id = xxhash_rust::xxh3::xxh3_64(code.source.as_bytes());
        let shader = ModernShader::wgsl(id, &code.source, code.entry_point, stage)?
            .with_per_sample_uav_effects(code.per_sample_uav_effects);
        Ok(Self {
            code,
            shader: Arc::new(shader),
        })
    }
}

#[derive(Default)]
pub(in crate::d3d11) struct ShaderVariants {
    pub(super) vertex: FxHashMap<VertexKey, Rc<Translated>>,
    pub(super) fragment: FxHashMap<FragmentKey, Rc<Translated>>,
    pub(super) compute: FxHashMap<StageInput, Rc<(Translated, Arc<Dispatch>)>>,
    pub(super) expansion: FxHashMap<ExpansionKey, Rc<expand::Programs>>,
    pub(super) masks: FxHashMap<u32, Arc<ModernShader>>,
}

impl ShaderVariants {
    pub(in crate::d3d11) fn release(&mut self, interfaces: &[u32]) {
        let retired = |shader: &ShaderIdentity| interfaces.contains(&shader.object);
        self.vertex.retain(|key, _| {
            !(retired(&key.input.shader) || key.fragment.as_ref().is_some_and(retired))
        });
        self.fragment.retain(|key, _| {
            !(retired(&key.input.shader)
                || key
                    .expanded
                    .as_ref()
                    .is_some_and(|(shader, _)| retired(shader)))
        });
        self.compute.retain(|key, _| !retired(&key.shader));
        self.expansion.retain(|key, _| {
            !(interfaces.contains(&key.stream_shader)
                || retired(&key.vertex.shader)
                || [&key.geometry, &key.hull, &key.domain]
                    .into_iter()
                    .flatten()
                    .any(|input| retired(&input.shader))
                || key
                    .raster
                    .as_ref()
                    .and_then(|r| r.fragment.as_ref())
                    .is_some_and(retired))
        });
    }
}

pub(super) fn cached<K: Eq + Hash, V: Clone>(
    entries: &mut FxHashMap<K, V>,
    key: K,
    _execution: &Execution,
    translate: impl FnOnce(&K) -> Result<V, Error>,
) -> Result<V, Error> {
    match entries.entry(key) {
        Entry::Occupied(entry) => {
            #[cfg(feature = "d3d9-counters")]
            _execution.variant_counts(true);
            Ok(entry.get().clone())
        }
        Entry::Vacant(entry) => {
            #[cfg(feature = "d3d9-counters")]
            _execution.variant_counts(false);
            let value = translate(entry.key())?;
            Ok(entry.insert(value).clone())
        }
    }
}
