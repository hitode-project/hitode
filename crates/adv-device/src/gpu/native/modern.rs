//! Draw and dispatch payloads for programmable front ends.

mod filter;

use super::resources::gpu_error;
use super::{BufferWindow, Error, Gpu, UniformWindow};
use adv_gpu::encode::{Bind, IndexFormat, Load, Store};
use adv_gpu::pipeline::{self as p, BindingKind, Stages};
use adv_gpu::shader::{ShaderDesc, Source, Stage};
use adv_gpu::{Buffer, ImageView, Rect, Sampler};
use arrayvec::ArrayVec;
use smallvec::SmallVec;
use std::ops::Range;
use std::sync::Arc;

#[derive(Clone)]
pub struct BufferRange {
    pub buffer: Buffer,
    pub range: Range<u64>,
}

#[derive(Clone)]
pub enum StorageBinding {
    Image(ImageView),
    Buffer(BufferRange),
    Window(BufferWindow),
    ImageAlias(ImageAlias),
}

/// A linear, writable copy of the image region for the duration of one command.
#[derive(Clone)]
pub struct ImageAlias {
    pub image: ImageRegion,
    pub linear: TransferBuffer,
}

impl ImageAlias {
    pub(super) fn copy(&self, encoder: &mut adv_gpu::encode::Encoder, to_image: bool) {
        if to_image {
            encoder.copy_buffer_to_image(self.linear.copy(), self.image.copy(), self.image.extent);
        } else {
            encoder.copy_image_to_buffer(self.image.copy(), self.linear.copy(), self.image.extent);
        }
    }
}

/// The stage's original register arrays, keyed by the shader's register
/// number. Bindings unused by the entry point are never visited.
#[derive(Clone, Default)]
pub struct SlotBindings {
    pub uniforms: Slots<BufferWindow>,
    pub images: Slots<ImageView>,
    pub samplers: Slots<Sampler>,
    pub storage: Slots<StorageBinding, 1>,
}

/// Bound registers in register order. Stages bind a few registers spread over
/// a wide range (pulled vertex streams sit above 192), so the table holds only
/// the bound ones, the first `N` without an allocation.
#[derive(Clone)]
pub struct Slots<T, const N: usize = 4>(SmallVec<[(u32, T); N]>);

impl<T, const N: usize> Default for Slots<T, N> {
    fn default() -> Self {
        Self(SmallVec::new())
    }
}

impl<T, const N: usize> Slots<T, N> {
    fn find(&self, register: usize) -> Result<usize, usize> {
        self.0.binary_search_by_key(&register, |&(n, _)| n as usize)
    }

    pub fn get(&self, register: usize) -> Option<&T> {
        self.find(register).ok().map(|n| &self.0[n].1)
    }

    /// Bind `value` at `register`, replacing any earlier binding there.
    pub fn set(&mut self, register: usize, value: T) {
        match self.find(register) {
            Ok(n) => self.0[n].1 = value,
            Err(n) => self.0.insert(n, (register as u32, value)),
        }
    }

    /// Bound values in register order.
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.0.iter().map(|(_, value)| value)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Registers `0..M` in order.
impl<T, const N: usize, const M: usize> From<[T; M]> for Slots<T, N> {
    fn from(values: [T; M]) -> Self {
        Self(values.into_iter().zip(0..).map(|(v, n)| (n, v)).collect())
    }
}

pub struct ModernShader {
    identity: u64,
    module: naga::Module,
    name: String,
    stage: Stage,
    bindings: Vec<(u32, p::Binding)>,
    writes: Vec<u32>,
    per_sample_uav_effects: bool,
}

impl ModernShader {
    /// Admit one entry and compact its used registers. The identity includes
    /// any shader variant and identifies the selected entry point.
    pub fn new(
        identity: u64,
        mut module: naga::Module,
        name: &str,
        stage: Stage,
    ) -> Result<Self, Error> {
        let naga_stage = match stage {
            Stage::Vertex => naga::ShaderStage::Vertex,
            Stage::Fragment => naga::ShaderStage::Fragment,
            Stage::Compute => naga::ShaderStage::Compute,
            _ => return Err(Error::Invalid("modern shader stage".into())),
        };
        module
            .entry_points
            .retain(|e| e.name == name && e.stage == naga_stage);
        if module.entry_points.is_empty() {
            return Err(Error::Invalid(format!("missing shader entry {name}")));
        }
        naga::compact::compact(&mut module, naga::compact::KeepUnused::No);
        let info = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .map_err(|e| Error::Invalid(e.to_string()))?;
        let stages = match stage {
            Stage::Vertex => Stages::VERTEX,
            Stage::Fragment => Stages::FRAGMENT,
            Stage::Compute => Stages::COMPUTE,
            _ => unreachable!(),
        };
        let mut counts = [0; 4];
        let mut bindings = Vec::new();
        let mut writes = Vec::new();
        for (handle, var) in module.global_variables.iter_mut() {
            if info.get_entry_point(0)[handle].is_empty() {
                continue;
            }
            let Some(register) = &mut var.binding else {
                continue;
            };
            let (group, kind) = binding_kind(var.space, &module.types[var.ty].inner)?;
            if group != register.group {
                return Err(Error::Invalid("shader register group".into()));
            }
            let index = group as usize;
            let limit = p::VERTEX_BINDING_BASES[index];
            let mut slot = counts[index];
            if slot == limit {
                return Err(Error::Invalid(
                    "too many registers in one shader stage".into(),
                ));
            }
            counts[index] += 1;
            if stage == Stage::Vertex {
                slot += limit;
            }
            if group == 3
                && info.get_entry_point(0)[handle]
                    .intersects(naga::valid::GlobalUse::WRITE | naga::valid::GlobalUse::ATOMIC)
            {
                writes.push(register.binding);
            }
            bindings.push((register.binding, p::Binding { slot, kind, stages }));
            register.binding = slot;
        }
        Ok(Self {
            identity,
            module,
            name: name.to_owned(),
            stage,
            bindings,
            writes,
            per_sample_uav_effects: false,
        })
    }

    pub fn wgsl(identity: u64, source: &str, name: &str, stage: Stage) -> Result<Self, Error> {
        let module = naga::front::wgsl::parse_str(source)
            .map_err(|e| Error::Invalid(e.emit_to_string(source)))?;
        Self::new(identity, module, name, stage)
    }

    pub fn with_per_sample_uav_effects(mut self, effects: bool) -> Self {
        self.per_sample_uav_effects = effects;
        self
    }

    pub(super) fn has_per_sample_uav_effects(&self) -> bool {
        self.per_sample_uav_effects
    }

    pub(super) fn identity(&self) -> u64 {
        self.identity
    }

    pub(super) fn entry(
        &self,
        device: &adv_gpu::Device,
    ) -> adv_gpu::Result<adv_gpu::shader::Entry> {
        device
            .create_shader(ShaderDesc {
                source: Source::CompactIr(Box::new(self.module.clone())),
                identity: self.identity,
                label: "modern shader",
            })?
            .entry(&self.name, self.stage)
    }

    /// Storage register numbers written by this shader's entry point.
    pub fn storage_writes(&self) -> impl Iterator<Item = u32> + '_ {
        self.writes.iter().copied()
    }

    pub(super) fn layout(&self) -> impl Iterator<Item = p::Binding> + '_ {
        self.bindings.iter().map(|&(_, binding)| binding)
    }

    pub(super) fn compact(&self, slots: &SlotBindings) -> Result<BoundSlots, Error> {
        self.bindings
            .iter()
            .map(|&(register, binding)| {
                let i = register as usize;
                let resource = match binding.kind {
                    BindingKind::UniformBuffer => {
                        slots.uniforms.get(i).cloned().map(Bound::Uniform)
                    }
                    BindingKind::SampledImage { .. } => {
                        slots.images.get(i).cloned().map(Bound::Sampled)
                    }
                    BindingKind::Sampler { .. } => {
                        slots.samplers.get(i).cloned().map(Bound::Sampler)
                    }
                    BindingKind::StorageImage { .. } => match slots.storage.get(i).cloned() {
                        Some(StorageBinding::Image(view)) => Some(Bound::StorageImage(view)),
                        _ => None,
                    },
                    BindingKind::StorageBuffer { .. } => match slots.storage.get(i).cloned() {
                        Some(StorageBinding::Buffer(buffer)) => {
                            Some(Bound::StorageBuffer(buffer.into()))
                        }
                        Some(StorageBinding::Window(window)) => Some(Bound::StorageBuffer(window)),
                        Some(StorageBinding::ImageAlias(alias)) => Some(Bound::ImageAlias(alias)),
                        _ => None,
                    },
                    _ => None,
                };
                let resource = resource.ok_or_else(|| {
                    Error::Invalid(format!("missing {:?} register {register}", binding.kind))
                })?;
                Ok(BoundSlot {
                    slot: binding.slot,
                    resource,
                })
            })
            .collect()
    }
}

fn binding_kind(
    space: naga::AddressSpace,
    ty: &naga::TypeInner,
) -> Result<(u32, BindingKind), Error> {
    use adv_gpu::resource::Dimension;
    use naga::{AddressSpace, ImageClass, ImageDimension, StorageAccess, TypeInner};
    Ok(match (space, ty) {
        (AddressSpace::Uniform, _) => (0, BindingKind::UniformBuffer),
        (AddressSpace::Storage { access }, _) => (
            3,
            BindingKind::StorageBuffer {
                writable: access.intersects(StorageAccess::STORE | StorageAccess::ATOMIC),
            },
        ),
        (AddressSpace::Handle, TypeInner::Sampler { comparison }) => (
            2,
            BindingKind::Sampler {
                comparison: *comparison,
            },
        ),
        (AddressSpace::Handle, TypeInner::Image { dim, class, .. }) => {
            let dimension = match dim {
                ImageDimension::D1 => Dimension::D1,
                ImageDimension::D2 => Dimension::D2,
                ImageDimension::D3 => Dimension::D3,
                ImageDimension::Cube => Dimension::Cube,
            };
            match class {
                ImageClass::Sampled { multi, .. } | ImageClass::Depth { multi } => (
                    1,
                    BindingKind::SampledImage {
                        dimension,
                        depth: matches!(class, ImageClass::Depth { .. }),
                        multisampled: *multi,
                    },
                ),
                ImageClass::Storage { format, access } => (
                    3,
                    BindingKind::StorageImage {
                        format: storage_format(*format)?,
                        writable: access.intersects(StorageAccess::STORE | StorageAccess::ATOMIC),
                    },
                ),
                _ => return Err(Error::Invalid("modern shader image type".into())),
            }
        }
        _ => return Err(Error::Invalid("modern shader resource type".into())),
    })
}

fn storage_format(format: naga::StorageFormat) -> Result<adv_gpu::Format, Error> {
    use adv_gpu::Format as F;
    use naga::StorageFormat as S;
    Ok(match format {
        S::R8Unorm => F::R8Unorm,
        S::R8Snorm => F::R8Snorm,
        S::R8Uint => F::R8Uint,
        S::R8Sint => F::R8Sint,
        S::R16Uint => F::R16Uint,
        S::R16Sint => F::R16Sint,
        S::R16Float => F::R16Float,
        S::Rg8Unorm => F::Rg8Unorm,
        S::Rg8Snorm => F::Rg8Snorm,
        S::Rg8Uint => F::Rg8Uint,
        S::Rg8Sint => F::Rg8Sint,
        S::R32Uint => F::R32Uint,
        S::R32Sint => F::R32Sint,
        S::R32Float => F::R32Float,
        S::Rg16Uint => F::Rg16Uint,
        S::Rg16Sint => F::Rg16Sint,
        S::Rg16Float => F::Rg16Float,
        S::Rgba8Unorm => F::Rgba8Unorm,
        S::Rgba8Snorm => F::Rgba8Snorm,
        S::Rgba8Uint => F::Rgba8Uint,
        S::Rgba8Sint => F::Rgba8Sint,
        S::Bgra8Unorm => F::Bgra8Unorm,
        S::Rgb10a2Uint => F::Rgb10a2Uint,
        S::Rgb10a2Unorm => F::Rgb10a2Unorm,
        S::Rg11b10Ufloat => F::Rg11b10Float,
        S::R64Uint => return Err(Error::Invalid("64-bit storage image format".into())),
        S::Rg32Uint => F::Rg32Uint,
        S::Rg32Sint => F::Rg32Sint,
        S::Rg32Float => F::Rg32Float,
        S::Rgba16Uint => F::Rgba16Uint,
        S::Rgba16Sint => F::Rgba16Sint,
        S::Rgba16Float => F::Rgba16Float,
        S::Rgba32Uint => F::Rgba32Uint,
        S::Rgba32Sint => F::Rgba32Sint,
        S::Rgba32Float => F::Rgba32Float,
        S::R16Unorm => F::R16Unorm,
        S::R16Snorm => F::R16Snorm,
        S::Rg16Unorm => F::Rg16Unorm,
        S::Rg16Snorm => F::Rg16Snorm,
        S::Rgba16Unorm => F::Rgba16Unorm,
        S::Rgba16Snorm => F::Rgba16Snorm,
    })
}

enum Bound {
    Uniform(BufferWindow),
    Sampled(ImageView),
    Sampler(Sampler),
    StorageBuffer(BufferWindow),
    StorageImage(ImageView),
    ImageAlias(ImageAlias),
}

pub(crate) struct BoundSlot {
    pub slot: u32,
    resource: Bound,
}

/// A stage's bound resources in slot order; draws bind a few.
pub(crate) type BoundSlots = SmallVec<[BoundSlot; 4]>;

impl BoundSlot {
    pub(super) fn retain_uniform(&self, windows: &mut Vec<UniformWindow>) {
        if let Bound::Uniform(window) | Bound::StorageBuffer(window) = &self.resource {
            window.retain(windows);
        }
    }

    pub fn binding(&self) -> Bind<'_> {
        match &self.resource {
            Bound::Uniform(b) => Bind::Uniform(b.binding()),
            Bound::Sampled(view) => Bind::Sampled(view),
            Bound::Sampler(sampler) => Bind::Sampler(sampler),
            Bound::StorageBuffer(b) => Bind::StorageBuffer(b.binding()),
            Bound::StorageImage(view) => Bind::StorageImage(view),
            Bound::ImageAlias(alias) => Bind::StorageBuffer(
                alias
                    .linear
                    .buffer
                    .slice(0..alias.linear.buffer.desc().size),
            ),
        }
    }

    pub(super) fn image_alias(&self) -> Option<&ImageAlias> {
        match &self.resource {
            Bound::ImageAlias(alias) => Some(alias),
            _ => None,
        }
    }
}

#[derive(Clone)]
pub struct ModernColor {
    pub view: ImageView,
    pub blend: Option<(p::BlendComponent, p::BlendComponent)>,
    pub write_mask: u8,
    pub load: Load<[f64; 4]>,
    pub store: Store,
}

#[derive(Clone)]
pub struct ModernDepth {
    pub view: ImageView,
    pub state: p::DepthStencil,
    pub depth: (Load<f32>, Store),
    pub stencil: (Load<u32>, Store),
    pub read_only: bool,
}

#[derive(Clone)]
pub enum DrawArguments {
    Direct {
        vertices: Range<u32>,
        instances: Range<u32>,
        base_vertex: i32,
    },
    Indirect(BufferRange),
}

#[derive(Clone)]
pub enum DispatchArguments {
    Direct([u32; 3]),
    Indirect(BufferRange),
}

#[derive(Clone)]
pub struct ModernDraw {
    /// Immutable input assembly, rasterizer, blend and depth/stencil state IDs.
    pub state_ids: [u64; 4],
    pub vertex: Arc<ModernShader>,
    pub fragment: Option<Arc<ModernShader>>,
    pub colors: ArrayVec<ModernColor, { p::MAX_COLOR_TARGETS }>,
    /// Color locations in increasing slot order. None uses consecutive slots.
    pub color_slots: Option<u8>,
    pub depth: Option<ModernDepth>,
    pub samples: u32,
    pub sample_mask: u32,
    pub inexact_sample_mask: bool,
    pub alpha_to_coverage: bool,
    pub primitive: p::Primitive,
    pub area: Rect,
    pub viewport: [f32; 6],
    pub scissor: Rect,
    pub stencil_reference: u32,
    pub blend_constant: [f32; 4],
    pub depth_bias: [f32; 3],
    pub arguments: DrawArguments,
    pub index: Option<(BufferWindow, IndexFormat)>,
    pub occlusion: Option<QuerySlot>,
    pub predicate: Option<Predicate>,
}

#[derive(Clone)]
pub struct QuerySlot {
    pub set: Arc<adv_gpu::queue::QuerySet>,
    pub index: u32,
}

/// Execute the draw if the occlusion result is nonzero (or zero when inverted).
#[derive(Clone)]
pub struct Predicate {
    pub query: QuerySlot,
    pub inverted: bool,
}

/// A draw with its compacted bindings. The submitting thread fills it once;
/// the worker adds the pipeline and passes the same allocation on.
pub(crate) struct Render {
    pub draw: ModernDraw,
    pub statistics: Option<QuerySlot>,
    pub vertex: BoundSlots,
    pub fragment: BoundSlots,
}

#[derive(Clone)]
pub struct Dispatch {
    pub(super) pipeline: adv_gpu::ComputePipeline,
    pub(super) shader: Arc<ModernShader>,
}

pub(crate) struct Compute {
    pub pipeline: adv_gpu::ComputePipeline,
    pub bindings: BoundSlots,
    pub arguments: DispatchArguments,
}

impl Gpu {
    pub fn draw_modern(
        &mut self,
        draw: ModernDraw,
        vertex: &SlotBindings,
        fragment: &SlotBindings,
    ) -> Result<(), Error> {
        self.record_modern_draw(draw, vertex, fragment, None)
    }

    /// The slot brackets only this draw's raster pass.
    pub fn draw_modern_statistics(
        &mut self,
        draw: ModernDraw,
        vertex: &SlotBindings,
        fragment: &SlotBindings,
        statistics: QuerySlot,
    ) -> Result<(), Error> {
        self.record_modern_draw(draw, vertex, fragment, Some(statistics))
    }

    fn record_modern_draw(
        &mut self,
        draw: ModernDraw,
        vertex: &SlotBindings,
        fragment: &SlotBindings,
        statistics: Option<QuerySlot>,
    ) -> Result<(), Error> {
        let vertex = draw.vertex.compact(vertex)?;
        let fragment = draw
            .fragment
            .as_ref()
            .map(|shader| shader.compact(fragment))
            .transpose()?
            .unwrap_or_default();
        self.enqueue_modern(super::record::WorkerItem::DrawModern {
            facade: self.facade_id,
            render: Box::new(Render {
                draw,
                statistics,
                vertex,
                fragment,
            }),
        })
    }

    pub fn prepare_dispatch(&self, shader: Arc<ModernShader>) -> Result<Dispatch, Error> {
        let pipeline = self
            .device
            .create_compute_pipeline(
                &p::ComputePipelineDesc {
                    entry: shader.entry(&self.device).map_err(gpu_error)?,
                    bindings: shader.layout().collect(),
                    label: "modern dispatch",
                },
                None,
            )
            .map_err(gpu_error)?;
        Ok(Dispatch { pipeline, shader })
    }

    pub fn dispatch(
        &mut self,
        dispatch: &Dispatch,
        slots: &SlotBindings,
        arguments: DispatchArguments,
    ) -> Result<(), Error> {
        let bindings = dispatch.shader.compact(slots)?;
        self.enqueue_modern(super::record::WorkerItem::Dispatch {
            facade: self.facade_id,
            work: Compute {
                pipeline: dispatch.pipeline.clone(),
                bindings,
                arguments,
            },
        })
    }

    pub fn resolve(&mut self, source: ImageView, destination: ImageView) -> Result<(), Error> {
        self.enqueue_modern(super::record::WorkerItem::Resolve {
            facade: self.facade_id,
            source,
            destination,
        })
    }

    pub fn generate_mips(&mut self, view: ImageView) -> Result<(), Error> {
        self.enqueue_modern(super::record::WorkerItem::GenerateMips {
            facade: self.facade_id,
            view,
        })
    }

    pub(super) fn enqueue_modern(&self, item: super::record::WorkerItem) -> Result<(), Error> {
        self.check()?;
        self.execution.worker.admit_cmd(0, self.wake.get())?;
        self.execution.worker.push(item, 0);
        Ok(())
    }
}

/// The timestamp counter has a stable period for the lifetime of the device.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TimestampDisjoint {
    pub frequency: u64,
    pub disjoint: bool,
}

impl Gpu {
    pub fn timestamp_disjoint(&self, set: Option<&adv_gpu::queue::QuerySet>) -> TimestampDisjoint {
        match set {
            Some(set) => TimestampDisjoint {
                frequency: (1_000_000_000.0 / set.period()).round() as u64,
                disjoint: false,
            },
            None => TimestampDisjoint {
                frequency: 0,
                disjoint: true,
            },
        }
    }

    /// Record a GPU timestamp after preceding work and before subsequent work.
    /// A marker uses two consecutive indices in the set, with the result in
    /// the second index.
    pub fn timestamp(&mut self, slot: QuerySlot) -> Result<(), Error> {
        self.enqueue_modern(super::record::WorkerItem::Timestamp {
            facade: self.facade_id,
            slot,
        })
    }

    /// Copy one native query result. A subsequent flush submits the copy.
    pub fn read_query(
        &mut self,
        slot: QuerySlot,
        destination: Buffer,
        offset: u64,
    ) -> Result<(), Error> {
        self.enqueue_modern(super::record::WorkerItem::ReadQuery {
            facade: self.facade_id,
            slot,
            destination,
            offset,
        })
    }
}

#[cfg(test)]
mod tests;

#[derive(Clone)]
pub struct ImageRegion {
    pub image: adv_gpu::Image,
    pub mip: u32,
    pub layer: u32,
    pub origin: (u32, u32, u32),
    pub extent: adv_gpu::Extent,
}

impl ImageRegion {
    pub(super) fn copy(&self) -> adv_gpu::encode::ImageCopy<'_> {
        adv_gpu::encode::ImageCopy {
            image: &self.image,
            mip: self.mip,
            layer: self.layer,
            origin: self.origin,
        }
    }
}

#[derive(Clone)]
pub struct TransferBuffer {
    pub buffer: Buffer,
    pub offset: u64,
    pub row_pitch: u32,
    pub rows: u32,
}

impl TransferBuffer {
    pub(super) fn copy(&self) -> adv_gpu::encode::BufferCopy<'_> {
        adv_gpu::encode::BufferCopy {
            buffer: &self.buffer,
            offset: self.offset,
            row_pitch: self.row_pitch,
            rows: self.rows,
        }
    }
}

#[derive(Clone)]
pub enum ModernTransfer {
    CopyImage {
        source: ImageRegion,
        destination: ImageRegion,
    },
    CopyImageBits {
        source: ImageRegion,
        scratch: TransferBuffer,
        destination: ImageRegion,
    },
    Upload {
        source: TransferBuffer,
        destination: ImageRegion,
    },
    UploadStaged {
        source: super::Staged,
        rows: u32,
        destination: ImageRegion,
    },
    UploadDepthStencil {
        depth: super::Staged,
        stencil: super::Staged,
        destination: ImageRegion,
    },
    Download {
        source: ImageRegion,
        destination: TransferBuffer,
    },
    DownloadDepthStencil {
        source: ImageRegion,
        depth: TransferBuffer,
        stencil: TransferBuffer,
    },
    CopyBuffer {
        source: BufferRange,
        destination: Buffer,
        offset: u64,
    },
    CopyBufferWindow {
        source: BufferWindow,
        destination: BufferWindow,
    },
    FillBuffer {
        destination: BufferRange,
        value: u32,
    },
    ClearColor {
        view: ImageView,
        value: [f64; 4],
        area: Option<Rect>,
    },
    ClearDepth {
        view: ImageView,
        depth: Option<f32>,
        stencil: Option<u32>,
    },
}

impl Gpu {
    pub fn transfer(&self, transfer: ModernTransfer) -> Result<(), Error> {
        self.enqueue_modern(super::record::WorkerItem::TransferImage {
            facade: self.facade_id,
            command: Box::new(super::stream::Command::ModernTransfer(Box::new(transfer))),
        })
    }

    pub fn completion(&self) -> Result<std::sync::mpsc::Receiver<()>, Error> {
        self.completion_with_wake(None)
    }

    pub fn completion_with_wake(
        &self,
        wake: Option<adv_sync::WakeEvent>,
    ) -> Result<std::sync::mpsc::Receiver<()>, Error> {
        let (send, receive) = std::sync::mpsc::channel();
        self.check()?;
        self.execution.worker.admit_cmd(0, self.wake.get())?;
        self.execution.worker.push_boundary(
            super::record::WorkerItem::TransferImage {
                facade: self.facade_id,
                command: Box::new(super::stream::Command::Complete(send, wake)),
            },
            0,
        );
        Ok(receive)
    }
}
