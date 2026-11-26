//! Context state groups shared by immutable command snapshots.
mod hazard;

use super::abi::{D3D11_VIEWPORT, RECT};
use super::desc::{View, ViewKind};
use hazard::Index;
use std::sync::Arc;

const SRVS: usize = 128;
const SAMPLERS: usize = 16;
const CONSTANTS: usize = 14;
const RENDER_TARGETS: usize = 8;
const UAVS: usize = 8;
const STREAMS: usize = 32;
const VIEWPORTS: usize = 16;
const STREAM_OUTPUTS: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Vertex,
    Hull,
    Domain,
    Geometry,
    Pixel,
    Compute,
}

impl Stage {
    const COUNT: usize = 6;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Slot {
    Srv(Stage, usize),
    Rtv(usize),
    Dsv,
    OmUav(usize),
    CsUav(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ViewSlot(Slot);

impl ViewSlot {
    const COUNT: usize = Stage::COUNT * SRVS + RENDER_TARGETS + 1 + 2 * UAVS;

    pub fn srv(stage: Stage, slot: usize) -> Option<Self> {
        (slot < SRVS).then_some(Self(Slot::Srv(stage, slot)))
    }

    pub fn rtv(slot: usize) -> Option<Self> {
        (slot < RENDER_TARGETS).then_some(Self(Slot::Rtv(slot)))
    }

    pub const DSV: Self = Self(Slot::Dsv);

    pub fn om_uav(slot: usize) -> Option<Self> {
        (slot < UAVS).then_some(Self(Slot::OmUav(slot)))
    }

    pub fn cs_uav(slot: usize) -> Option<Self> {
        (slot < UAVS).then_some(Self(Slot::CsUav(slot)))
    }

    fn index(self) -> usize {
        const OM: usize = Stage::COUNT * SRVS;
        match self.0 {
            Slot::Srv(stage, slot) => stage as usize * SRVS + slot,
            Slot::Rtv(slot) => OM + slot,
            Slot::Dsv => OM + RENDER_TARGETS,
            Slot::OmUav(slot) => OM + RENDER_TARGETS + 1 + slot,
            Slot::CsUav(slot) => OM + RENDER_TARGETS + 1 + UAVS + slot,
        }
    }

    fn kind(self) -> ViewKind {
        match self.0 {
            Slot::Srv(..) => ViewKind::ShaderResource,
            Slot::Rtv(_) => ViewKind::RenderTarget,
            Slot::Dsv => ViewKind::DepthStencil,
            Slot::OmUav(_) | Slot::CsUav(_) => ViewKind::UnorderedAccess,
        }
    }
}

#[derive(Clone)]
pub struct BoundView {
    pub pointer: u32,
    pub resource: u32,
    pub desc: Arc<View>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VertexBuffer {
    pub buffer: u32,
    pub stride: u32,
    pub offset: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IndexBuffer {
    pub buffer: u32,
    pub format: u32,
    pub offset: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ConstantBuffer {
    pub buffer: u32,
    pub first: u32,
    pub count: u32,
}

#[derive(Clone, Default)]
pub struct InputAssembler {
    pub layout: u32,
    pub topology: u32,
    pub vertices: [VertexBuffer; STREAMS],
    pub indices: IndexBuffer,
}

#[derive(Clone)]
pub struct ShaderStage {
    pub shader: u32,
    pub constants: [ConstantBuffer; CONSTANTS],
    pub samplers: [u32; SAMPLERS],
    srvs: [Option<BoundView>; SRVS],
    uavs: [Option<BoundView>; UAVS],
}

impl Default for ShaderStage {
    fn default() -> Self {
        Self {
            shader: 0,
            constants: Default::default(),
            samplers: Default::default(),
            srvs: [const { None }; SRVS],
            uavs: Default::default(),
        }
    }
}

#[derive(Clone, Default)]
pub struct Rasterizer {
    pub state: u32,
    pub viewports: [D3D11_VIEWPORT; VIEWPORTS],
    pub viewport_count: usize,
    pub scissors: [RECT; VIEWPORTS],
    pub scissor_count: usize,
}

#[derive(Clone)]
pub struct OutputMerger {
    targets: [Option<BoundView>; RENDER_TARGETS],
    depth: Option<BoundView>,
    uavs: [Option<BoundView>; UAVS],
    pub blend: u32,
    pub blend_factor: [f32; 4],
    pub sample_mask: u32,
    pub depth_stencil: u32,
    pub stencil_reference: u32,
}

impl Default for OutputMerger {
    fn default() -> Self {
        Self {
            targets: Default::default(),
            depth: None,
            uavs: Default::default(),
            blend: 0,
            blend_factor: [1.0; 4],
            sample_mask: u32::MAX,
            depth_stencil: 0,
            stencil_reference: 0,
        }
    }
}

#[derive(Clone, Copy, Default)]
pub struct StreamOutput {
    pub buffers: [u32; STREAM_OUTPUTS],
    pub offsets: [u32; STREAM_OUTPUTS],
}

#[derive(Clone, Copy, Default)]
pub struct Predication {
    pub predicate: u32,
    pub value: bool,
}

#[derive(Clone, Default)]
pub struct Snapshot {
    pub ia: Arc<InputAssembler>,
    pub stages: [Arc<ShaderStage>; Stage::COUNT],
    pub rs: Arc<Rasterizer>,
    pub om: Arc<OutputMerger>,
    pub so: Arc<StreamOutput>,
    pub predication: Arc<Predication>,
}

impl Snapshot {
    pub fn view(&self, slot: ViewSlot) -> Option<&BoundView> {
        match slot.0 {
            Slot::Srv(stage, slot) => self.stages[stage as usize].srvs[slot].as_ref(),
            Slot::Rtv(slot) => self.om.targets[slot].as_ref(),
            Slot::Dsv => self.om.depth.as_ref(),
            Slot::OmUav(slot) => self.om.uavs[slot].as_ref(),
            Slot::CsUav(slot) => self.stages[Stage::Compute as usize].uavs[slot].as_ref(),
        }
    }

    fn replace_view(&mut self, slot: ViewSlot, new: Option<BoundView>) -> Option<BoundView> {
        let item = match slot.0 {
            Slot::Srv(stage, slot) => {
                &mut Arc::make_mut(&mut self.stages[stage as usize]).srvs[slot]
            }
            Slot::Rtv(slot) => &mut Arc::make_mut(&mut self.om).targets[slot],
            Slot::Dsv => &mut Arc::make_mut(&mut self.om).depth,
            Slot::OmUav(slot) => &mut Arc::make_mut(&mut self.om).uavs[slot],
            Slot::CsUav(slot) => {
                &mut Arc::make_mut(&mut self.stages[Stage::Compute as usize]).uavs[slot]
            }
        };
        std::mem::replace(item, new)
    }
}

pub struct ContextState {
    groups: Snapshot,
    defaults: Snapshot,
    index: Index,
}

impl Default for ContextState {
    fn default() -> Self {
        let defaults = Snapshot::default();
        Self {
            groups: defaults.clone(),
            defaults,
            index: Index::default(),
        }
    }
}

impl ContextState {
    pub fn snapshot(&self) -> Snapshot {
        self.groups.clone()
    }

    pub fn groups(&self) -> &Snapshot {
        &self.groups
    }

    pub fn clear(&mut self) -> Snapshot {
        self.index.clear();
        std::mem::replace(&mut self.groups, self.defaults.clone())
    }

    /// Changes one binding and reports each slot replacement for the object owner.
    pub fn bind_view(
        &mut self,
        slot: ViewSlot,
        mut new: Option<BoundView>,
        mut changed: impl FnMut(ViewSlot, Option<BoundView>, Option<u32>),
    ) {
        if let Some(binding) = &new {
            if slot.kind() != binding.desc.kind() {
                return;
            }
            let mut candidate = self.index.first(binding.resource);
            while let Some(other) = candidate {
                candidate = self.index.next(other);
                if slot != other && hazard::conflict(binding, self.groups.view(other).unwrap()) {
                    if slot.kind() == ViewKind::ShaderResource {
                        new = None;
                        break;
                    }
                    let old = self.replace(other, None);
                    changed(other, old, None);
                }
            }
        }
        let old = self.groups.view(slot).map(|view| view.pointer);
        let pointer = new.as_ref().map(|view| view.pointer);
        if old != pointer {
            let old = self.replace(slot, new);
            changed(slot, old, pointer);
        }
    }

    fn replace(&mut self, slot: ViewSlot, new: Option<BoundView>) -> Option<BoundView> {
        let resource = new.as_ref().map(|view| view.resource);
        let old = self.groups.replace_view(slot, new);
        if let Some(old) = &old {
            self.index.remove(old.resource, slot);
        }
        if let Some(resource) = resource {
            self.index.insert(resource, slot);
        }
        old
    }
}

#[cfg(test)]
mod tests;

impl ContextState {
    pub fn set_input_layout(&mut self, layout: u32) {
        if self.groups.ia.layout != layout {
            Arc::make_mut(&mut self.groups.ia).layout = layout;
        }
    }

    pub fn set_topology(&mut self, topology: u32) {
        if self.groups.ia.topology != topology {
            Arc::make_mut(&mut self.groups.ia).topology = topology;
        }
    }

    pub fn set_vertices(&mut self, first: usize, vertices: &[VertexBuffer]) {
        let range = first..first + vertices.len();
        if self.groups.ia.vertices[range.clone()] != *vertices {
            Arc::make_mut(&mut self.groups.ia).vertices[range].copy_from_slice(vertices);
        }
    }

    pub fn set_indices(&mut self, indices: IndexBuffer) {
        if self.groups.ia.indices != indices {
            Arc::make_mut(&mut self.groups.ia).indices = indices;
        }
    }

    pub fn set_shader(&mut self, stage: Stage, shader: u32) {
        let state = &mut self.groups.stages[stage as usize];
        if state.shader != shader {
            Arc::make_mut(state).shader = shader;
        }
    }

    pub fn set_constants(&mut self, stage: Stage, first: usize, constants: &[ConstantBuffer]) {
        let state = &mut self.groups.stages[stage as usize];
        let range = first..first + constants.len();
        if state.constants[range.clone()] != *constants {
            Arc::make_mut(state).constants[range].copy_from_slice(constants);
        }
    }

    pub fn set_samplers(&mut self, stage: Stage, first: usize, samplers: &[u32]) {
        let state = &mut self.groups.stages[stage as usize];
        let range = first..first + samplers.len();
        if state.samplers[range.clone()] != *samplers {
            Arc::make_mut(state).samplers[range].copy_from_slice(samplers);
        }
    }

    pub fn set_rasterizer(&mut self, rasterizer: u32) {
        if self.groups.rs.state != rasterizer {
            Arc::make_mut(&mut self.groups.rs).state = rasterizer;
        }
    }

    pub fn set_viewports(&mut self, viewports: &[D3D11_VIEWPORT]) {
        let state = &mut self.groups.rs;
        if bytemuck::cast_slice::<_, u32>(&state.viewports[..state.viewport_count])
            != bytemuck::cast_slice::<_, u32>(viewports)
        {
            let state = Arc::make_mut(state);
            state.viewport_count = viewports.len();
            state.viewports[..viewports.len()].copy_from_slice(viewports);
        }
    }

    pub fn set_scissors(&mut self, scissors: &[RECT]) {
        let state = &mut self.groups.rs;
        if bytemuck::cast_slice::<_, u32>(&state.scissors[..state.scissor_count])
            != bytemuck::cast_slice::<_, u32>(scissors)
        {
            let state = Arc::make_mut(state);
            state.scissor_count = scissors.len();
            state.scissors[..scissors.len()].copy_from_slice(scissors);
        }
    }

    pub fn set_blend(&mut self, blend: u32, factor: [f32; 4], mask: u32) {
        let state = &mut self.groups.om;
        if state.blend != blend
            || state.sample_mask != mask
            || state.blend_factor.map(f32::to_bits) != factor.map(f32::to_bits)
        {
            let state = Arc::make_mut(state);
            state.blend = blend;
            state.blend_factor = factor;
            state.sample_mask = mask;
        }
    }

    pub fn set_depth_stencil(&mut self, depth_stencil: u32, reference: u32) {
        let state = &mut self.groups.om;
        if state.depth_stencil != depth_stencil || state.stencil_reference != reference {
            let state = Arc::make_mut(state);
            state.depth_stencil = depth_stencil;
            state.stencil_reference = reference;
        }
    }

    pub fn set_predication(&mut self, predicate: u32, value: bool) {
        if self.groups.predication.predicate != predicate || self.groups.predication.value != value
        {
            *Arc::make_mut(&mut self.groups.predication) = Predication { predicate, value };
        }
    }
}
