//! D3D9 interface semantics over common COM identities and neutral GPU commands.
//! The runtime supplies only surface/window assembly callbacks. Native frames
//! use the same store and mailbox as GDI/DirectDraw, without a CPU round trip.
// Typed SDK signatures determine the parameter count, including COM `this`.
#![allow(clippy::too_many_arguments)]
use adv_abi::types::{D3DPRESENT_PARAMETERS, RECT};
use adv_abi::{GuestPtr, Handle, Registry, win32};
use adv_cpu::backend::{Backend, GuestExt, Prot};
use adv_device::gpu::{Error, Gpu, Target};
use adv_machine::{Env, Ext, Machine, Object, Objects, Outcome, Wait};
use adv_os::{
    Heaps,
    com::{Com, ComIdentity, Destroyed, Iid, QueryError},
};
use resources::Resource;
mod capabilities;
mod creation;
mod cursor;
mod dc;
mod dirty;
mod fixed;
mod format;
mod leaf_calls;
pub(crate) mod loading;
mod memory;
mod pacing;
mod presentation;
mod programmable;
mod readback;
mod recording;
mod residency;
pub(crate) mod saving;
mod stretch;
mod targets;
#[cfg(test)]
mod tests;
mod update_texture;
use std::collections::BTreeMap;
use std::sync::Arc;

const INVALID: u32 = 0x8876086c;
const UNAVAILABLE: u32 = 0x8876086a;
const DEVICE_LOST: u32 = 0x88760868;
const DEVICE_NOT_RESET: u32 = 0x88760869;
const OUT_OF_MEMORY: u32 = 0x8007000e;
const NO_INTERFACE: u32 = 0x80004002;
// Official SDK d3d9.h DEFINE_GUIDs in guest little-endian memory order.
const IID_FACTORY: Iid = [
    0xca, 0xcb, 0xbd, 0x81, 0xd4, 0x64, 0x6d, 0x42, 0xae, 0x8d, 0xad, 1, 0x47, 0xf4, 0x27, 0x5c,
];
const IID_DEVICE: Iid = [
    0x96, 0x3b, 0x22, 0xd0, 0x7a, 0xbf, 0xfd, 0x43, 0x92, 0xbd, 0xa4, 0x3b, 0x0d, 0x82, 0xb9, 0xeb,
];

/// Thin assembly operations; no higher-layer payload is owned by this frontend.
#[derive(Clone, Copy)]
pub struct SurfaceProvider {
    /// Explicit CPU/GDI access, assembled by the runtime without a frontend
    /// dependency. The borrowed canvas is owned until `release_dc`.
    pub acquire_dc: fn(&mut Env<'_>, u32, u32, u32, u32) -> Result<u32, String>,
    pub writeback_dc: fn(&mut Env<'_>, u32, u32, u32) -> Result<(), String>,
    pub release_dc: fn(&mut Env<'_>, u32),
    pub insert: fn(&mut Ext, Target) -> Result<u32, String>,
    pub target: fn(&Ext, u32) -> Option<Target>,
    pub remove: fn(&mut Ext, &mut dyn adv_cpu::backend::Guest, u32) -> Result<(), String>,
    pub replace: fn(&mut Ext, u32, Target) -> Result<(), String>,
    pub present: fn(&mut Ext, u32, &mut Gpu) -> Result<u64, Error>,
    /// Snapshot a preserved COPY front without rotating its writable target.
    pub present_copy: fn(&mut Ext, u32, &mut Gpu) -> Result<u64, Error>,
    /// Complete a guest Present without publishing or executing a GPU command.
    pub repeat: fn(&mut Ext, u32) -> Result<u64, Error>,
    /// Shared serial includes other devices, GDI, DirectDraw and video fronts.
    pub presentation_serial: fn(&Ext) -> Option<u64>,
    pub window: fn(&Ext, &Objects, u32) -> Option<(u32, u32)>,
    pub visible: fn(&Ext, &Objects, u32) -> Option<bool>,
    /// Host input policy: bypass refresh pacing while physical Ctrl is held.
    pub fast_forward: fn(&Ext) -> bool,
    pub display: fn(&Ext) -> (u32, u32, u32),
    /// Apply guest display/window state only after device creation/reset succeeds.
    pub mode_changed: fn(&mut Env<'_>, u32, D3DPRESENT_PARAMETERS) -> Outcome,
    pub device_released: fn(&mut Env<'_>, u32),
    pub completed: fn(&mut Env<'_>, u64) -> Option<Outcome>,
}

type Id = ComIdentity<Payload>;
enum Payload {
    Factory,
    Device(Box<Device>),
    Resource(Resource),
    Buffer(buffers::Buffer),
    Declaration(declaration::Declaration),
    Shader(shaders::Shader),
    StateBlock(Box<shaders::stateblocks::StateBlock>),
}
struct Device {
    pointer: u32,
    factory: Id,
    surface: u32,
    /// Shared surface-store identity, owned exclusively by this device.
    front: Option<u32>,
    parameters: D3DPRESENT_PARAMETERS,
    creator: u32,
    preserve_fpu: bool,
    creation: creation::State,
    scene: bool,
    needs_reset: bool,
    back_buffer: Option<Id>,
    depth_stencil: Option<Id>,
    texture: Option<Id>,
    /// Borrowed surface identity; its controlling resource has one internal ref.
    render_target: Option<Id>,
    buffers: buffers::Bindings,
    fixed: fixed::Fixed,
    shaders: shaders::Bindings,
    recording: Option<shaders::stateblocks::Snapshot>,
    frame: recording::Recording,
    gpu_lost: bool,
}
pub struct State {
    com: Com,
    tables: BTreeMap<&'static str, u32>,
    gpu: Option<Gpu>,
    provider: SurfaceProvider,
    pending: Vec<Pending>,
    transfers: Vec<readback::PendingTransfer>,
    dcs: Vec<dc::Lease>,
    sender: Option<adv_sync::background::LocalSender>,
    wake: Option<std::sync::Arc<adv_sync::WakeEvent>>,
    /// Whether creation offers COPY presentation. Accepting a swap effect
    /// moves every engine that requests it onto its Direct3D path (SoftPAL
    /// then needs vertex declarations), so a profile can withhold COPY when
    /// that path regresses an engine that has a working fallback.
    copy: bool,
    shader_caps: ShaderCaps,
    shader_stats: ShaderStats,
    shader_translations: shaders::TranslationCache,
    programs: programmable::ProgramCache,
    shader_cache: Option<std::path::PathBuf>,
    frames: FrameCounters,
    verify_frames: bool,
}
/// Shader model capability gates. SM2 passed its differential fixtures and the
/// engine matrix and is reported by default; SM3 stays an explicit opt-in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShaderCaps {
    pub sm2: bool,
    pub sm3: bool,
}
impl Default for ShaderCaps {
    fn default() -> Self {
        Self {
            sm2: true,
            sm3: false,
        }
    }
}
pub fn set_shader_caps(ext: &mut Ext, caps: ShaderCaps) {
    ext.get_mut::<State>().shader_caps = caps;
}
/// Opt-in directory for derived WGSL. The frontend bounds and validates its
/// cache; no guest shader token stream is written here.
pub fn set_shader_cache(ext: &mut Ext, directory: Option<std::path::PathBuf>) {
    let state = ext.get_mut::<State>();
    if let Some(gpu) = &mut state.gpu {
        gpu.set_pipeline_cache_directory(directory.as_deref());
    }
    state.shader_cache = directory;
}
/// Counts shader models without retaining or exporting guest shader contents.
/// Array order: 1.1, 1.2, 1.3, 1.4, 2.0, 2.1, 3.0.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ShaderStats {
    pub vertex: [u64; 7],
    pub pixel: [u64; 7],
    pub malformed: u64,
    pub untranslated: u64,
}
#[derive(Clone, Copy, Debug)]
pub struct Diagnostics {
    pub live_interfaces: usize,
    pub pending_waits: usize,
    pub pending_readbacks: usize,
    pub gpu: Option<adv_device::gpu::Stats>,
    pub shaders: ShaderStats,
    pub frames: FrameCounters,
}
pub use recording::Counters as FrameCounters;
pub fn diagnostics(ext: &Ext, objects: &Objects) -> Diagnostics {
    let state = ext.get::<State>();
    Diagnostics {
        live_interfaces: objects.alias_count(state.com.namespace()),
        pending_waits: state.pending.len(),
        pending_readbacks: state.transfers.len(),
        gpu: state.gpu.as_ref().map(|g| g.stats()),
        shaders: state.shader_stats,
        frames: state.frames,
    }
}
// Keep ordinary hot commands inline. Only a parked command requires allocation.
#[allow(clippy::large_enum_variant)]
enum Operation {
    Clear {
        target: Target,
        rectangle: adv_device::surface::Rect,
        color: Option<[f64; 4]>,
        depth: Option<f32>,
        stencil: Option<u32>,
    },
    Present {
        owner: Id,
        token: u64,
        surface: u32,
        deadline: Option<u64>,
        copy: Option<(Id, presentation::Copy)>,
    },
    Upload {
        resource: Id,
        level: Option<u32>,
        target: Target,
        pixels: Arc<Vec<u8>>,
        pitch: u32,
        rectangle: adv_device::surface::Rect,
    },
    Copy {
        source: adv_device::gpu::Image,
        rectangle: adv_device::surface::Rect,
        destination: Target,
        region: adv_device::surface::Rect,
        filter: adv_device::gpu::Filter,
        resources: [Id; 2],
    },
    Draw(fixed::Draw),
    Flush {
        owner: Id,
        token: u64,
        reason: recording::Reason,
    },
    Consumed,
}
struct Pending {
    context: u32,
    event: u32,
    owner: Id,
    /// At most the two controlling surface identities of a pending blit.
    resources: [Option<Id>; 2],
    operation: Option<Box<Operation>>,
    result: Option<Result<Completion, Error>>,
}
enum Completion {
    Command,
    Presented(u64, Option<u64>),
}

pub fn register(registry: &mut Registry) {
    for api in [
        create_ABI,
        factory_query_ABI,
        factory_add_ref_ABI,
        factory_release_ABI,
        adapter_count_ABI,
        create_device_ABI,
        device_query_ABI,
        device_add_ref_ABI,
        device_release_ABI,
        cooperative_ABI,
        get_direct3d_ABI,
        begin_scene_ABI,
        end_scene_ABI,
        adv_abi::Api {
            env_leaf: Some(leaf_calls::draw),
            ..clear_ABI
        },
        present_ABI,
        reset_ABI,
    ] {
        registry.register(api);
    }
    resources::register(registry);
    fixed::register(registry);
    readback::register(registry);
    residency::register(registry);
    stretch::register(registry);
    capabilities::register(registry);
    targets::register(registry);
    buffers::register(registry);
    declaration::register(registry);
    creation::register(registry);
    depth::register(registry);
    memory::register(registry);
    cursor::register(registry);
    dirty::register(registry);
    dc::register(registry);
    update_texture::register(registry);
    shaders::register(registry);
    programmable::register(registry);
}

/// Install hookable, guest-resident vtables from generated COM slot metadata.
pub fn install<B: Backend>(
    machine: &mut Machine<B>,
    provider: SurfaceProvider,
) -> Result<(), String> {
    let mut tables = BTreeMap::new();
    for interface in [
        "IDirect3D9",
        "IDirect3DDevice9",
        "IDirect3DTexture9",
        "IDirect3DSurface9",
        "IDirect3DVertexBuffer9",
        "IDirect3DIndexBuffer9",
        "IDirect3DVertexDeclaration9",
        "IDirect3DVertexShader9",
        "IDirect3DPixelShader9",
        "IDirect3DStateBlock9",
    ] {
        let signatures: Vec<_> = adv_abi::signature::com_signatures()
            .iter()
            .filter(|s| s.module == interface)
            .collect();
        if signatures.is_empty() {
            return Err(format!("missing COM metadata: {interface}"));
        }
        let table = machine
            .sys
            .vm
            .alloc(&mut machine.cpu, signatures.len() as u32 * 4, Prot::RW)
            .map_err(|e| e.to_string())?;
        for signature in signatures {
            let method = format!("{interface}::{}", signature.name);
            let thunk = machine
                .sys
                .thunks
                .publish(&mut machine.cpu, "d3d9.dll", &method)
                .map_err(|e| e.to_string())?;
            machine
                .cpu
                .write_u32(table + u32::from(signature.com_slot.unwrap()) * 4, thunk)
                .map_err(|e| e.to_string())?;
        }
        tables.insert(interface, table);
    }
    let com = Com::new(&mut machine.sys.objects).ok_or("D3D9 COM namespace exhausted")?;
    machine.ext.insert(State {
        com,
        tables,
        gpu: None,
        provider,
        pending: Vec::new(),
        transfers: Vec::new(),
        dcs: Vec::new(),
        sender: None,
        wake: None,
        copy: true,
        shader_caps: ShaderCaps::default(),
        shader_stats: ShaderStats::default(),
        shader_translations: shaders::TranslationCache::default(),
        programs: programmable::ProgramCache::default(),
        shader_cache: None,
        frames: FrameCounters::default(),
        verify_frames: std::env::var("ADV_VERIFY_FRAMES").as_deref() == Ok("1"),
    });
    Ok(())
}
pub fn set_gpu(ext: &mut Ext, mut gpu: Gpu) -> Result<(), String> {
    let state = ext.get_mut::<State>();
    if state.gpu.is_some() {
        return Err("D3D9 GPU already installed".into());
    }
    gpu.set_pipeline_cache_directory(state.shader_cache.as_deref());
    state.gpu = Some(gpu);
    Ok(())
}
pub fn set_background(
    ext: &mut Ext,
    sender: adv_sync::background::LocalSender,
    wake: adv_sync::WakeEvent,
) {
    let state = ext.get_mut::<State>();
    state.sender = Some(sender);
    state.wake = Some(std::sync::Arc::new(wake));
}
/// Assemble an installed child frontend with the parent's host-device and
/// completion policy. Guest COM objects, vtables and pending work stay local.
pub fn fork_host_state(parent: &Ext, child: &mut Ext) -> Result<(), String> {
    let source = parent.get::<State>();
    let destination = child.get_mut::<State>();
    if destination.gpu.is_some() {
        return Err("D3D9 child GPU already installed".into());
    }
    let gpu = source
        .gpu
        .as_ref()
        .map(Gpu::fork)
        .transpose()
        .map_err(|error| error.to_string())?;
    destination.provider = source.provider;
    destination.sender = source.sender.clone();
    destination.wake = source.wake.clone();
    destination.copy = source.copy;
    destination.shader_caps = source.shader_caps;
    destination.shader_cache = source.shader_cache.clone();
    destination.verify_frames = source.verify_frames;
    if let Some(gpu) = gpu {
        set_gpu(child, gpu)?;
    }
    Ok(())
}
pub fn readback_image(
    ext: &mut Ext,
    image: &adv_device::gpu::Image,
) -> Result<adv_device::gpu::Readback, Error> {
    let state = ext.get_mut::<State>();
    state
        .gpu
        .as_mut()
        .ok_or_else(|| Error::Unavailable("D3D9 GPU absent".into()))?
        .readback(image, state.wake.clone())
}
fn com(env: &Env<'_>) -> Com {
    env.ext.get::<State>().com
}
fn alloc(env: &mut Env<'_>, len: u32) -> Option<u32> {
    let heaps = env.ext.get_mut::<Heaps>();
    heaps.alloc(env.cpu, &mut env.sys.vm, heaps.process_heap, len, true)
}
fn free(env: &mut Env<'_>, pointer: u32) {
    let heaps = env.ext.get_mut::<Heaps>();
    heaps.free(env.cpu, &mut env.sys.vm, heaps.process_heap, pointer);
}
fn object(
    env: &mut Env<'_>,
    interface: &'static str,
    iid: Iid,
    payload: Payload,
) -> Option<(u32, Id)> {
    let table = *env.ext.get::<State>().tables.get(interface)?;
    let pointer = alloc(env, 4)?;
    if env.cpu.write_u32(pointer, table).is_err() {
        free(env, pointer);
        return None;
    }
    let Some(id) = com(env).create(&mut env.sys.objects, pointer, iid, payload) else {
        free(env, pointer);
        return None;
    };
    Some((pointer, id))
}
fn destroy(env: &mut Env<'_>, destroyed: Destroyed<Payload>) {
    if let Payload::Device(device) = destroyed.value {
        let provider = env.ext.get::<State>().provider;
        (provider.device_released)(env, device.pointer);
        targets::release_binding(env, device.render_target);
        depth::release_binding(env, device.depth_stencil);
        buffers::release_bindings(env, device.buffers);
        shaders::release_bindings(env, device.shaders);
        if let Some(recording) = device.recording {
            shaders::stateblocks::release_snapshot(env, recording);
        }
        for binding in [device.texture, device.back_buffer].into_iter().flatten() {
            if let Some(Some(resource)) = com(env).release_internal(&mut env.sys.objects, binding) {
                destroy(env, resource);
            }
        }
        let _ = (provider.remove)(env.ext, env.cpu, device.surface);
        presentation::release(env, device.front);
        if let Some(released) = com(env).release_identity(&mut env.sys.objects, device.factory)
            && let Some(factory) = released.destroyed
        {
            destroy(env, factory);
        }
    } else if let Payload::Resource(resource) = destroyed.value {
        recording::forget(env, resource.device, resource.pointer);
        resources::destroy(env, resource);
    } else if let Payload::Buffer(buffer) = destroyed.value {
        buffers::destroy(env, buffer);
    } else if let Payload::StateBlock(block) = destroyed.value {
        shaders::stateblocks::destroy(env, *block);
    }
    for pointer in destroyed.interfaces {
        free(env, pointer);
    }
}
fn release_parent(env: &Env<'_>, pointer: u32) -> Option<Id> {
    let pointer = resources::container_pointer(env, pointer).unwrap_or(pointer);
    resources::parent_on_last_public_release(env, pointer)
        .or_else(|| buffers::parent_on_last_public_release(env, pointer))
        .or_else(|| declaration::parent_on_last_public_release(env, pointer))
        .or_else(|| shaders::parent_on_last_public_release(env, pointer))
}
fn release(env: &mut Env<'_>, pointer: u32) -> u32 {
    if let Some(parent) = resources::container_pointer(env, pointer) {
        return release(env, parent);
    }
    let parent = release_parent(env, pointer);
    let Some(released) = com(env).release::<Payload>(&mut env.sys.objects, pointer) else {
        return 0;
    };
    if let Some(dead) = released.destroyed {
        destroy(env, dead);
    }
    if let Some(parent) = parent
        && let Some(released) = com(env).release_identity(&mut env.sys.objects, parent)
        && let Some(dead) = released.destroyed
    {
        destroy(env, dead);
    }
    released.references
}
fn add_ref(env: &mut Env<'_>, pointer: u32) -> Option<u32> {
    if let Some(parent) = resources::container_pointer(env, pointer) {
        return add_ref(env, parent);
    }
    let parent = resources::parent_on_first_public_retain(env, pointer)
        .or_else(|| buffers::parent_on_first_public_retain(env, pointer))
        .or_else(|| declaration::parent_on_first_public_retain(env, pointer))
        .or_else(|| shaders::parent_on_first_public_retain(env, pointer));
    if let Some(parent) = parent {
        com(env).add_ref_identity(&mut env.sys.objects, parent)?;
    }
    let references = com(env).add_ref::<Payload>(&mut env.sys.objects, pointer);
    if references.is_none()
        && let Some(parent) = parent
    {
        let _ = com(env).release_identity(&mut env.sys.objects, parent);
    }
    references
}
fn query(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    iid: GuestPtr<Iid>,
    output: GuestPtr<u32>,
) -> Outcome {
    if output.write(env.cpu, 0).is_err() {
        return Outcome::Return(INVALID);
    }
    let Ok(iid) = iid.read(env.cpu) else {
        return Outcome::Return(INVALID);
    };
    if resources::container_pointer(env, this.addr()).is_some() {
        let pointer =
            match com(env).resolve_interface::<Payload>(&mut env.sys.objects, this.addr(), iid) {
                Ok(pointer) => pointer,
                Err(QueryError::NoInterface) => return Outcome::Return(NO_INTERFACE),
                Err(_) => return Outcome::Return(INVALID),
            };
        if add_ref(env, pointer).is_none() {
            return Outcome::Return(OUT_OF_MEMORY);
        }
        if output.write(env.cpu, pointer).is_err() {
            release(env, pointer);
            return Outcome::Return(INVALID);
        }
        return Outcome::Return(0);
    }
    let parent = resources::parent_on_first_public_retain(env, this.addr())
        .or_else(|| buffers::parent_on_first_public_retain(env, this.addr()))
        .or_else(|| declaration::parent_on_first_public_retain(env, this.addr()))
        .or_else(|| shaders::parent_on_first_public_retain(env, this.addr()));
    if let Some(parent) = parent
        && com(env)
            .add_ref_identity(&mut env.sys.objects, parent)
            .is_none()
    {
        return Outcome::Return(OUT_OF_MEMORY);
    }
    let query = com(env).query::<Payload>(&mut env.sys.objects, this.addr(), iid);
    if query.is_err()
        && let Some(parent) = parent
    {
        let _ = com(env).release_identity(&mut env.sys.objects, parent);
    }
    match query {
        Ok(pointer) => match output.write(env.cpu, pointer) {
            Ok(()) => Outcome::Return(0),
            Err(_) => {
                release(env, pointer);
                Outcome::Return(INVALID)
            }
        },
        Err(QueryError::NoInterface) => Outcome::Return(NO_INTERFACE),
        Err(QueryError::ReferenceLimit) => Outcome::Return(OUT_OF_MEMORY),
        Err(QueryError::InvalidObject) => Outcome::Return(INVALID),
    }
}
macro_rules! unknown {
    ($query:ident, $add:ident, $release:ident, $q:tt, $a:tt, $r:tt) => {
        #[win32(d3d9, name = $q, level = implement)]
        fn $query(
            env: &mut Env<'_>,
            this: GuestPtr<()>,
            iid: GuestPtr<Iid>,
            output: GuestPtr<u32>,
        ) -> Outcome {
            query(env, this, iid, output)
        }
        #[win32(d3d9, name = $a, level = implement)]
        fn $add(env: &mut Env<'_>, this: GuestPtr<()>) -> u32 {
            add_ref(env, this.addr()).unwrap_or(0)
        }
        #[win32(d3d9, name = $r, level = implement)]
        fn $release(env: &mut Env<'_>, this: GuestPtr<()>) -> Outcome {
            let Some(id) = com(env).identity::<Payload>(&env.sys.objects, this.addr()) else {
                return Outcome::Return(0);
            };
            let owner = recording::owner(env, id);
            // Commands own their data/images, so transient buffer, shader or
            // state-block releases need no flush. Guard parent device teardown.
            let refs = com(env).references(&env.sys.objects, owner);
            let stateblock_last = matches!(
                com(env).get_identity(&env.sys.objects, id),
                Some(Payload::StateBlock(_))
            ) && com(env).references(&env.sys.objects, id) == Some(1);
            if owner != id
                && refs != Some(0)
                && !(refs == Some(1)
                    && (stateblock_last || release_parent(env, this.addr()) == Some(owner)))
            {
                return Outcome::Return(release(env, this.addr()));
            }
            recording::barrier(env, owner, recording::Reason::Release, move |env| {
                Outcome::Return(release(env, this.addr()))
            })
        }
    };
}
mod buffers;
mod declaration;
mod depth;
mod resources;
mod shaders;
unknown!(
    factory_query,
    factory_add_ref,
    factory_release,
    "IDirect3D9::QueryInterface",
    "IDirect3D9::AddRef",
    "IDirect3D9::Release"
);
unknown!(
    device_query,
    device_add_ref,
    device_release,
    "IDirect3DDevice9::QueryInterface",
    "IDirect3DDevice9::AddRef",
    "IDirect3DDevice9::Release"
);

#[win32(d3d9, name = "Direct3DCreate9", level = implement)]
fn create(env: &mut Env<'_>, version: u32) -> GuestPtr<()> {
    if !matches!(version, 31 | 32) {
        return GuestPtr::new(0);
    }
    GuestPtr::new(object(env, "IDirect3D9", IID_FACTORY, Payload::Factory).map_or(0, |v| v.0))
}
#[win32(d3d9, name = "IDirect3D9::GetAdapterCount", level = implement)]
fn adapter_count(env: &mut Env<'_>, this: GuestPtr<()>) -> u32 {
    u32::from(matches!(
        com(env).get::<Payload>(&env.sys.objects, this.addr()),
        Some(Payload::Factory)
    ))
}
#[win32(d3d9, name = "IDirect3DDevice9::GetDirect3D", level = implement)]
fn get_direct3d(env: &mut Env<'_>, this: GuestPtr<()>, output: GuestPtr<u32>) -> u32 {
    if output.write(env.cpu, 0).is_err() {
        return INVALID;
    }
    let Some(factory) = device(env, this.addr()).map(|d| d.factory) else {
        return INVALID;
    };
    if !matches!(
        com(env).get_identity(&env.sys.objects, factory),
        Some(Payload::Factory)
    ) {
        return INVALID;
    }
    let Some(pointer) = com(env).canonical(&env.sys.objects, factory) else {
        return INVALID;
    };
    if com(env)
        .add_ref_identity(&mut env.sys.objects, factory)
        .is_none()
    {
        return OUT_OF_MEMORY;
    }
    if output.write(env.cpu, pointer).is_err() {
        release(env, pointer);
        return INVALID;
    }
    0
}
fn presentation_unavailable(parameters: D3DPRESENT_PARAMETERS, copy: bool) -> Option<String> {
    let mut reasons = Vec::new();
    for (name, value, supported) in [
        (
            "MultiSampleType",
            parameters.MultiSampleType as u32,
            parameters.MultiSampleType == 0,
        ),
        (
            "MultiSampleQuality",
            parameters.MultiSampleQuality,
            parameters.MultiSampleQuality == 0,
        ),
        (
            "SwapEffect",
            parameters.SwapEffect as u32,
            parameters.SwapEffect == 1 || copy && parameters.SwapEffect == 3,
        ),
        (
            "BackBufferCount",
            parameters.BackBufferCount,
            parameters.BackBufferCount <= 1,
        ),
        ("Flags", parameters.Flags, parameters.Flags & !3 == 0),
        (
            "AutoDepthStencilFormat",
            parameters.AutoDepthStencilFormat,
            parameters.EnableAutoDepthStencil == 0
                || depth::format(parameters.AutoDepthStencilFormat).is_some(),
        ),
        (
            "PresentationInterval",
            parameters.PresentationInterval,
            matches!(parameters.PresentationInterval, 0 | 1 | 0x80000000),
        ),
        (
            "BackBufferFormat",
            parameters.BackBufferFormat,
            matches!(parameters.BackBufferFormat, 0 | 21 | 22),
        ),
    ] {
        if !supported {
            reasons.push(format!("{name}={value:#x} ({value})"));
        }
    }
    (!reasons.is_empty()).then(|| {
        format!(
            "unsupported presentation parameters: {}",
            reasons.join(", ")
        )
    })
}
fn warn_create_unavailable(
    env: &mut Env<'_>,
    adapter: u32,
    kind: i32,
    focus: u32,
    flags: u32,
    input: Option<D3DPRESENT_PARAMETERS>,
    reason: &str,
) {
    let parameters = input.map_or_else(|| "parameters not read".into(), |p| format!(
        "requested BackBufferWidth={}, BackBufferHeight={}, BackBufferFormat={}, BackBufferCount={}, MultiSampleType={}, MultiSampleQuality={}, SwapEffect={}, hDeviceWindow={:#x}, Windowed={}, EnableAutoDepthStencil={}, AutoDepthStencilFormat={}, Flags={:#x}, FullScreen_RefreshRateInHz={}, PresentationInterval={:#x}",
        p.BackBufferWidth, p.BackBufferHeight, p.BackBufferFormat, p.BackBufferCount,
        p.MultiSampleType, p.MultiSampleQuality, p.SwapEffect, p.hDeviceWindow,
        p.Windowed, p.EnableAutoDepthStencil, p.AutoDepthStencilFormat, p.Flags,
        p.FullScreen_RefreshRateInHz, p.PresentationInterval,
    ));
    adv_abi::warn_unavailable(
        env,
        "IDirect3D9::CreateDevice",
        &format!(
            "{reason}; adapter={adapter}, device_type={kind}, focus={focus:#x}, behavior_flags={flags:#x}; {parameters}"
        ),
    );
}
fn normalize(
    env: &Env<'_>,
    mut parameters: D3DPRESENT_PARAMETERS,
    focus: u32,
) -> Result<D3DPRESENT_PARAMETERS, u32> {
    // COPY has one preserved back buffer and cannot use multisampling. Zero
    // count is the SDK spelling for one, normalized below for both swap effects.
    if parameters.SwapEffect == 3
        && (parameters.BackBufferCount > 1 || parameters.MultiSampleType != 0)
    {
        return Err(INVALID);
    }
    if presentation_unavailable(parameters, env.ext.get::<State>().copy).is_some() {
        return Err(UNAVAILABLE);
    }
    if parameters.hDeviceWindow == 0 {
        parameters.hDeviceWindow = focus;
    }
    let provider = env.ext.get::<State>().provider;
    let (width, height) =
        (provider.window)(env.ext, &env.sys.objects, parameters.hDeviceWindow).ok_or(INVALID)?;
    if parameters.BackBufferWidth == 0 {
        parameters.BackBufferWidth = width;
    }
    if parameters.BackBufferHeight == 0 {
        parameters.BackBufferHeight = height;
    }
    if parameters.BackBufferWidth == 0 || parameters.BackBufferHeight == 0 {
        return Err(INVALID);
    }
    if parameters.BackBufferFormat == 0 {
        parameters.BackBufferFormat = 22;
    }
    parameters.BackBufferCount = 1;
    Ok(parameters)
}
#[win32(d3d9, name = "IDirect3D9::CreateDevice", level = implement)]
fn create_device(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    adapter: u32,
    kind: i32,
    focus: Handle,
    flags: u32,
    parameters: GuestPtr<D3DPRESENT_PARAMETERS>,
    output: GuestPtr<u32>,
) -> Outcome {
    if output.write(env.cpu, 0).is_err() {
        return Outcome::Return(INVALID);
    }
    let Some(factory) = com(env).identity::<Payload>(&env.sys.objects, this.addr()) else {
        return Outcome::Return(INVALID);
    };
    if !matches!(
        com(env).get_identity(&env.sys.objects, factory),
        Some(Payload::Factory)
    ) || adapter != 0
        || kind != 1
    {
        return Outcome::Return(INVALID);
    }
    let creation = match creation::State::new(adapter, kind, focus.0, flags) {
        Ok(creation) => creation,
        Err(error) => {
            if error == UNAVAILABLE {
                warn_create_unavailable(
                    env,
                    adapter,
                    kind,
                    focus.0,
                    flags,
                    None,
                    "requested device creation behavior policy is unavailable",
                );
            }
            return Outcome::Return(error);
        }
    };
    let Ok(input) = parameters.read(env.cpu) else {
        return Outcome::Return(INVALID);
    };
    let normalized = match normalize(env, input, focus.0) {
        Ok(p) => p,
        Err(e) => {
            if e == UNAVAILABLE {
                let copy = env.ext.get::<State>().copy;
                let reason = presentation_unavailable(input, copy).unwrap();
                warn_create_unavailable(env, adapter, kind, focus.0, flags, Some(input), &reason);
            }
            return Outcome::Return(e);
        }
    };
    if adv_abi::guest::check_range(
        env.cpu,
        parameters.addr(),
        std::mem::size_of::<D3DPRESENT_PARAMETERS>() as u32,
        Prot::WRITE,
    )
    .is_err()
    {
        return Outcome::Return(INVALID);
    }
    let environment = match creation_environment(env.cpu, flags & 2 != 0) {
        Ok(environment) => environment,
        Err(error) => {
            warn_create_unavailable(
                env,
                adapter,
                kind,
                focus.0,
                flags,
                Some(input),
                &format!("CPU floating-point creation environment is unavailable: {error}"),
            );
            return Outcome::Return(UNAVAILABLE);
        }
    };
    let target = match env.ext.get::<State>().gpu.as_ref() {
        Some(gpu) => gpu.target(
            normalized.BackBufferWidth,
            normalized.BackBufferHeight,
            false,
        ),
        None => {
            warn_create_unavailable(
                env,
                adapter,
                kind,
                focus.0,
                flags,
                Some(input),
                "GPU backend is not installed",
            );
            return Outcome::Return(UNAVAILABLE);
        }
    };
    let target = match target {
        Ok(t) => t,
        Err(Error::Invalid(_)) => return Outcome::Return(INVALID),
        Err(_) => return Outcome::Return(DEVICE_LOST),
    };
    if com(env)
        .add_ref_identity(&mut env.sys.objects, factory)
        .is_none()
    {
        return Outcome::Return(OUT_OF_MEMORY);
    }
    let provider = env.ext.get::<State>().provider;
    let surface = match (provider.insert)(
        env.ext,
        target.with_alpha_mode(targets::alpha_mode(normalized.BackBufferFormat)),
    ) {
        Ok(s) => s,
        Err(_) => {
            let _ = com(env).release_identity(&mut env.sys.objects, factory);
            return Outcome::Return(OUT_OF_MEMORY);
        }
    };
    let mut fixed = fixed::Fixed::new(normalized.BackBufferWidth, normalized.BackBufferHeight);
    fixed.auto_depth(normalized.EnableAutoDepthStencil != 0);
    let device = Device {
        pointer: 0,
        factory,
        surface,
        front: None,
        parameters: normalized,
        creator: env.context,
        preserve_fpu: flags & 2 != 0,
        creation,
        scene: false,
        needs_reset: false,
        back_buffer: None,
        depth_stencil: None,
        texture: None,
        render_target: None,
        buffers: buffers::Bindings::default(),
        fixed,
        shaders: shaders::Bindings::default(),
        recording: None,
        frame: recording::Recording::default(),
        gpu_lost: false,
    };
    let Some((pointer, id)) = object(
        env,
        "IDirect3DDevice9",
        IID_DEVICE,
        Payload::Device(Box::new(device)),
    ) else {
        let _ = (provider.remove)(env.ext, env.cpu, surface);
        let _ = com(env).release_identity(&mut env.sys.objects, factory);
        return Outcome::Return(OUT_OF_MEMORY);
    };
    if let Some(Payload::Device(device)) = com(env).get_identity_mut(&mut env.sys.objects, id) {
        device.pointer = pointer;
    }
    let back_buffer = resources::implicit_back_buffer(env, id, normalized, surface);
    let Some(back_buffer) = back_buffer else {
        release(env, pointer);
        return Outcome::Return(OUT_OF_MEMORY);
    };
    if let Some(Payload::Device(device)) = com(env).get_identity_mut(&mut env.sys.objects, id) {
        device.back_buffer = Some(back_buffer);
    }
    let depth = match depth::implicit(env, id, normalized) {
        Ok(value) => value,
        Err(error) => {
            release(env, pointer);
            if error == UNAVAILABLE {
                warn_create_unavailable(
                    env,
                    adapter,
                    kind,
                    focus.0,
                    flags,
                    Some(input),
                    "automatic depth-stencil backend is unavailable",
                );
            }
            return Outcome::Return(error);
        }
    };
    if let Some(Payload::Device(device)) = com(env).get_identity_mut(&mut env.sys.objects, id) {
        device.depth_stencil = depth;
    }
    if let Some(environment) = environment
        && let Err(error) = env.cpu.set_fpu_environment(environment)
    {
        release(env, pointer);
        warn_create_unavailable(
            env,
            adapter,
            kind,
            focus.0,
            flags,
            Some(input),
            &format!("CPU floating-point creation environment could not be applied: {error}"),
        );
        return Outcome::Return(UNAVAILABLE);
    }
    if parameters.write(env.cpu, normalized).is_err() || output.write(env.cpu, pointer).is_err() {
        release(env, pointer);
        return Outcome::Return(INVALID);
    }
    (provider.mode_changed)(env, pointer, normalized)
}
fn device<'a>(env: &'a Env<'_>, pointer: u32) -> Option<&'a Device> {
    match com(env).get::<Payload>(&env.sys.objects, pointer)? {
        Payload::Device(d) => Some(d),
        _ => None,
    }
}
fn creation_environment(
    cpu: &mut dyn adv_cpu::backend::Guest,
    preserve: bool,
) -> Result<Option<adv_cpu::backend::FpuEnvironment>, adv_cpu::backend::FpuEnvironmentError> {
    if preserve {
        return Ok(None);
    }
    let mut environment = cpu.fpu_environment()?;
    // Native x86 CreateDevice/Reset: mask exceptions, PC=single, RC=nearest.
    // Infinity/reserved controls and the complete MXCSR state remain intact.
    environment.x87_control = (environment.x87_control & !0x0f3f) | 0x003f;
    Ok(Some(environment))
}
#[win32(d3d9, name = "IDirect3DDevice9::TestCooperativeLevel", level = implement)]
fn cooperative(env: &mut Env<'_>, this: GuestPtr<()>) -> u32 {
    if device(env, this.addr()).is_none() {
        return INVALID;
    }
    if device(env, this.addr()).unwrap().needs_reset {
        return DEVICE_NOT_RESET;
    }
    if device(env, this.addr()).unwrap().gpu_lost {
        return DEVICE_LOST;
    }
    env.ext
        .get::<State>()
        .gpu
        .as_ref()
        .map_or(
            UNAVAILABLE,
            |g| if g.check().is_ok() { 0 } else { DEVICE_LOST },
        )
}
#[win32(d3d9, name = "IDirect3DDevice9::Reset", level = implement)]
fn reset(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    parameters: GuestPtr<D3DPRESENT_PARAMETERS>,
) -> Outcome {
    if let Some(id) = com(env).identity::<Payload>(&env.sys.objects, this.addr())
        && recording::has_work(env, id)
    {
        return recording::barrier(env, id, recording::Reason::Reset, move |env| {
            reset(env, this, parameters)
        });
    }
    let Some(d) = device(env, this.addr()) else {
        return Outcome::Return(INVALID);
    };
    if d.creator != env.context || d.scene {
        return Outcome::Return(INVALID);
    }
    let focus = d.parameters.hDeviceWindow;
    let preserve_fpu = d.preserve_fpu;
    let surface = d.surface;
    let old_buffer = d.back_buffer;
    let old_depth = d.depth_stencil;
    let old_texture = d.texture;
    let old_front = d.front;
    let device_id = com(env)
        .identity::<Payload>(&env.sys.objects, this.addr())
        .unwrap();
    let com = com(env);
    let outstanding_default = env.sys.objects.hosts_in::<adv_os::com::ComObject<Payload>>(com.namespace())
        .any(|(id,_)| matches!(com.get_identity(&env.sys.objects,id), Some(Payload::Resource(r))
            if r.device == device_id && r.desc.Pool == 0 && com.references(&env.sys.objects,id).is_some_and(|n| n != 0)));
    if outstanding_default
        || buffers::has_outstanding_default(env, device_id)
        || shaders::stateblocks::has_outstanding(env, device_id)
    {
        if let Some(Payload::Device(d)) = com.get_identity_mut(&mut env.sys.objects, device_id) {
            d.needs_reset = true;
        }
        return Outcome::Return(INVALID);
    }
    let Ok(input) = parameters.read(env.cpu) else {
        return Outcome::Return(INVALID);
    };
    if adv_abi::guest::check_range(
        env.cpu,
        parameters.addr(),
        std::mem::size_of::<D3DPRESENT_PARAMETERS>() as u32,
        Prot::WRITE,
    )
    .is_err()
    {
        return Outcome::Return(INVALID);
    }
    let normalized = match normalize(env, input, focus) {
        Ok(p) => p,
        Err(e) => return Outcome::Return(e),
    };
    let environment = match creation_environment(env.cpu, preserve_fpu) {
        Ok(environment) => environment,
        Err(_) => return Outcome::Return(UNAVAILABLE),
    };
    let target = match env.ext.get::<State>().gpu.as_ref() {
        Some(g) => g.target(
            normalized.BackBufferWidth,
            normalized.BackBufferHeight,
            false,
        ),
        None => return Outcome::Return(UNAVAILABLE),
    };
    let target = match target {
        Ok(t) => t,
        Err(e) => return gpu_error(e),
    };
    let Some(new_buffer) = resources::implicit_back_buffer(env, device_id, normalized, surface)
    else {
        return Outcome::Return(OUT_OF_MEMORY);
    };
    let new_depth = match depth::implicit(env, device_id, normalized) {
        Ok(value) => value,
        Err(error) => {
            if let Some(Some(dead)) = com.release_internal(&mut env.sys.objects, new_buffer) {
                destroy(env, dead);
            }
            return Outcome::Return(error);
        }
    };
    let provider = env.ext.get::<State>().provider;
    if let Some(environment) = environment
        && env.cpu.set_fpu_environment(environment).is_err()
    {
        depth::release_binding(env, new_depth);
        if let Some(Some(dead)) = com.release_internal(&mut env.sys.objects, new_buffer) {
            destroy(env, dead);
        }
        return Outcome::Return(UNAVAILABLE);
    }
    if (provider.replace)(
        env.ext,
        surface,
        target.with_alpha_mode(targets::alpha_mode(normalized.BackBufferFormat)),
    )
    .is_err()
    {
        depth::release_binding(env, new_depth);
        if let Some(Some(dead)) = com.release_internal(&mut env.sys.objects, new_buffer) {
            destroy(env, dead);
        }
        return Outcome::Return(INVALID);
    }
    if let Some(Payload::Device(d)) = com.get_identity_mut(&mut env.sys.objects, device_id) {
        d.parameters = normalized;
        d.front = None;
        d.back_buffer = Some(new_buffer);
        d.depth_stencil = new_depth;
        d.texture = None;
        d.needs_reset = false;
        d.gpu_lost = false;
        d.frame = recording::Recording::default();
        d.creation.reset();
        d.fixed = fixed::Fixed::new(normalized.BackBufferWidth, normalized.BackBufferHeight);
        d.fixed.auto_depth(normalized.EnableAutoDepthStencil != 0);
    }
    depth::release_binding(env, old_depth);
    presentation::release(env, old_front);
    buffers::reset_bindings(env, device_id);
    shaders::reset_bindings(env, device_id);
    targets::reset_binding(env, device_id);
    for binding in [old_texture, old_buffer].into_iter().flatten() {
        if let Some(Some(dead)) = com.release_internal(&mut env.sys.objects, binding) {
            destroy(env, dead);
        }
    }
    if parameters.write(env.cpu, normalized).is_err() {
        return Outcome::Return(INVALID);
    }
    (provider.mode_changed)(env, this.addr(), normalized)
}
#[win32(d3d9, name = "IDirect3DDevice9::BeginScene", level = state, leaf)]
fn begin_scene(env: &mut Env<'_>, this: GuestPtr<()>) -> u32 {
    let Some(Payload::Device(d)) = com(env).get_mut::<Payload>(&mut env.sys.objects, this.addr())
    else {
        return INVALID;
    };
    if d.scene {
        return INVALID;
    }
    d.scene = true;
    0
}
#[win32(d3d9, name = "IDirect3DDevice9::EndScene", level = state, leaf)]
fn end_scene(env: &mut Env<'_>, this: GuestPtr<()>) -> u32 {
    let Some(Payload::Device(d)) = com(env).get_mut::<Payload>(&mut env.sys.objects, this.addr())
    else {
        return INVALID;
    };
    if !d.scene {
        return INVALID;
    }
    d.scene = false;
    0
}
fn execute_gpu(env: &mut Env<'_>, operation: &Operation) -> Result<Completion, Error> {
    let provider = env.ext.get::<State>().provider;
    let mut gpu = env
        .ext
        .get_mut::<State>()
        .gpu
        .take()
        .ok_or_else(|| Error::Unavailable("D3D9 GPU is absent".into()))?;
    let result = match operation {
        Operation::Clear {
            target,
            rectangle,
            color,
            depth,
            stencil,
        } => gpu
            .clear_region(target, *rectangle, *color, *depth, *stencil)
            .map(|()| Completion::Command),
        Operation::Present {
            surface,
            deadline,
            copy,
            ..
        } => {
            let result = if let Some((device, copy)) = copy {
                presentation::execute(env, &mut gpu, *device, *surface, *copy)
            } else {
                // DISCARD permits preserving the writable target. Snapshotting
                // keeps it consistent with the content identity after Present.
                (provider.present_copy)(env.ext, *surface, &mut gpu)
            };
            result.map(|serial| Completion::Presented(serial, *deadline))
        }
        Operation::Upload {
            resource: _,
            level: _,
            target,
            pixels,
            pitch,
            rectangle,
        } => gpu
            .upload(&target.image, *rectangle, *pitch, pixels)
            .map(|()| Completion::Command),
        Operation::Draw(draw) => {
            let result = if let Some(program) = &draw.programmable {
                gpu.draw_programmable(&draw.target, program, &draw.state)
            } else {
                gpu.draw(
                    &draw.target,
                    &draw.vertices,
                    draw.image.as_ref(),
                    draw.state,
                )
            };
            result.map(|()| Completion::Command)
        }
        Operation::Copy {
            source,
            rectangle,
            destination,
            region,
            filter,
            ..
        } => stretch::copy(&mut gpu, source, *rectangle, destination, *region, *filter)
            .map(|()| Completion::Command),
        Operation::Consumed => Ok(Completion::Command),
        Operation::Flush { .. } => unreachable!("flush is executed by the recording owner"),
    };
    env.ext.get_mut::<State>().gpu = Some(gpu);
    result
}
fn completed(env: &mut Env<'_>, result: Completion) -> Outcome {
    if let Completion::Presented(serial, deadline) = result {
        let provider = env.ext.get::<State>().provider;
        if let Some(outcome) = (provider.completed)(env, serial) {
            return outcome;
        }
        let fast_forward = (provider.fast_forward)(env.ext);
        // S_PRESENT_OCCLUDED belongs to D3D9Ex. Base Present still succeeds;
        // visibility only suppresses the refresh deadline calculated below.
        return pacing::finish(env, deadline, 0, fast_forward);
    }
    Outcome::Return(0)
}
fn gpu_error(error: Error) -> Outcome {
    match error {
        Error::Invalid(_) => Outcome::Return(INVALID),
        Error::Unavailable(_) => Outcome::Return(UNAVAILABLE),
        Error::Device(_) => Outcome::Return(DEVICE_LOST),
        Error::Busy => unreachable!("Busy has a machine continuation"),
    }
}
fn dispatch(env: &mut Env<'_>, owner: Id, operation: Operation) -> Outcome {
    recording::record(env, owner, operation)
}
fn execute(env: &mut Env<'_>, operation: &Operation) -> Result<Completion, Error> {
    match operation {
        Operation::Present { owner, .. } => {
            let result = recording::present(env, operation);
            if matches!(result, Err(Error::Device(_)))
                && let Some(Payload::Device(d)) =
                    com(env).get_identity_mut(&mut env.sys.objects, *owner)
            {
                d.gpu_lost = true;
            }
            result
        }
        Operation::Flush {
            owner,
            token,
            reason,
        } => {
            let result = recording::flush(env, *owner, *token, *reason);
            if !matches!(result, Err(Error::Busy)) {
                recording::unlock(env, *owner, *token);
            }
            result
        }
        _ => execute_gpu(env, operation),
    }
}
fn dispatch_now(env: &mut Env<'_>, owner: Id, operation: Operation) -> Outcome {
    match execute(env, &operation) {
        Ok(value) => completed(env, value),
        Err(Error::Busy) => {
            if !com(env).retain_internal(&mut env.sys.objects, owner) {
                readback::upload_failed(env, &operation, false);
                return Outcome::Return(OUT_OF_MEMORY);
            }
            let resources = match &operation {
                Operation::Copy { resources, .. } => resources.map(Some),
                _ => [None; 2],
            };
            let mut retained = [None; 2];
            for (index, resource) in resources.into_iter().enumerate() {
                if let Some(resource) = resource {
                    if !com(env).retain_internal(&mut env.sys.objects, resource) {
                        release_pending_owners(env, owner, retained);
                        return Outcome::Return(OUT_OF_MEMORY);
                    }
                    retained[index] = Some(resource);
                }
            }
            let event = env.sys.objects.insert(Object::Event {
                manual_reset: true,
                signaled: false,
            });
            if event == 0 {
                release_pending_owners(env, owner, retained);
                readback::upload_failed(env, &operation, false);
                return Outcome::Return(OUT_OF_MEMORY);
            }
            env.ext.get_mut::<State>().pending.push(Pending {
                context: env.context,
                event,
                owner,
                resources: retained,
                operation: Some(Box::new(operation)),
                result: None,
            });
            Outcome::block(
                Wait::Object {
                    handle: event,
                    deadline: None,
                },
                move |env, _| {
                    let index = env
                        .ext
                        .get::<State>()
                        .pending
                        .iter()
                        .position(|p| p.event == event)
                        .expect("owned GPU wait");
                    let pending = env.ext.get_mut::<State>().pending.swap_remove(index);
                    env.sys.objects.close(event);
                    release_pending_owners(env, pending.owner, pending.resources);
                    match pending
                        .result
                        .expect("GPU wait only signaled after completion")
                    {
                        Ok(value) => completed(env, value),
                        Err(error) => gpu_error(error),
                    }
                },
            )
        }
        Err(error) => {
            readback::upload_failed(env, &operation, false);
            gpu_error(error)
        }
    }
}
fn release_pending_owners(env: &mut Env<'_>, owner: Id, resources: [Option<Id>; 2]) {
    for id in resources
        .into_iter()
        .flatten()
        .chain(std::iter::once(owner))
    {
        if let Some(Some(dead)) = com(env).release_internal(&mut env.sys.objects, id) {
            destroy(env, dead);
        }
    }
}
/// Retry only when the execution owner runs after an actual external wake. No
/// fixed-period poll or guest sleep is inserted; successful work signals Machine.
pub fn pump<B: Backend>(machine: &mut Machine<B>) -> Result<bool, String> {
    let pending = std::mem::take(&mut machine.ext.get_mut::<State>().pending);
    let mut progress = false;
    for mut wait in pending {
        let mut env = Env {
            context: machine.sys.current,
            cpu: &mut machine.cpu,
            sys: &mut machine.sys,
            ext: &mut machine.ext,
            thunk: 0,
        };
        if env
            .sys
            .contexts
            .get(wait.context as usize)
            .is_none_or(|ctx| ctx.is_done())
        {
            if let Some(operation) = &wait.operation {
                readback::upload_failed(&mut env, operation, true);
            }
            env.sys.objects.close(wait.event);
            release_pending_owners(&mut env, wait.owner, wait.resources);
            continue;
        }
        if let Some(operation) = &wait.operation {
            match execute(&mut env, operation) {
                Err(Error::Busy) => {}
                result => {
                    if result.is_err() {
                        readback::upload_failed(&mut env, operation, false);
                    }
                    wait.operation = None;
                    wait.result = Some(result);
                    env.sys.set_event(wait.event);
                    progress = true;
                }
            }
        }
        env.ext.get_mut::<State>().pending.push(wait);
    }
    let mut env = Env {
        context: machine.sys.current,
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk: 0,
    };
    let readback = readback::pump(&mut env);
    dc::reap_cancelled(&mut env);
    update_texture::reap_cancelled(&mut env);
    loading::reap_cancelled(&mut env);
    crate::d3dx9::reap_file_exports(&mut env, false);
    Ok(readback || progress)
}
/// Process stop cancels map ownership and closes internal wait handles. Accepted
/// fence tasks still finish on the one service before the host window is destroyed.
pub fn cancel_pending<B: Backend>(machine: &mut Machine<B>) {
    let pending = std::mem::take(&mut machine.ext.get_mut::<State>().pending);
    let mut env = Env {
        context: machine.sys.current,
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk: 0,
    };
    for wait in pending {
        if let Some(operation) = &wait.operation {
            readback::upload_failed(&mut env, operation, true);
        }
        env.sys.objects.close(wait.event);
        release_pending_owners(&mut env, wait.owner, wait.resources);
    }
    readback::cancel_pending(&mut env);
    dc::cancel_all(&mut env);
    update_texture::cancel_all(&mut env);
    loading::cancel_all(&mut env);
    crate::d3dx9::reap_file_exports(&mut env, true);
}
#[win32(d3d9, name = "IDirect3DDevice9::Clear", level = implement)]
fn clear(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    count: u32,
    rectangles: GuestPtr<()>,
    flags: u32,
    color: u32,
    z: f32,
    stencil: u32,
) -> Outcome {
    if count != 0 || !rectangles.is_null() || flags == 0 || flags & !7 != 0 {
        return Outcome::Return(INVALID);
    }
    let target = match targets::draw_target(env, this.addr()) {
        Ok(target) => target,
        Err(error) => return Outcome::Return(error),
    };
    let target = if flags & 6 != 0 {
        if device(env, this.addr()).is_none_or(|d| d.depth_stencil.is_none())
            || (flags & 4 != 0 && !depth::has_stencil(env, this.addr()))
        {
            return Outcome::Return(INVALID);
        }
        match depth::attach(env, this.addr(), target) {
            Ok(target) => target,
            Err(error) => return Outcome::Return(error),
        }
    } else {
        target
    };
    let color = [
        ((color >> 16) & 255) as f64 / 255.,
        ((color >> 8) & 255) as f64 / 255.,
        (color & 255) as f64 / 255.,
        (color >> 24) as f64 / 255.,
    ];
    let id = com(env)
        .identity::<Payload>(&env.sys.objects, this.addr())
        .unwrap();
    dispatch(
        env,
        id,
        Operation::Clear {
            target,
            rectangle: device(env, this.addr()).unwrap().fixed.clear_rect(),
            color: (flags & 1 != 0).then_some(color),
            depth: (flags & 2 != 0).then_some(z),
            stencil: (flags & 4 != 0).then_some(stencil),
        },
    )
}
#[win32(d3d9, name = "IDirect3DDevice9::Present", level = implement)]
fn present(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    source: GuestPtr<RECT>,
    destination: GuestPtr<RECT>,
    override_window: Handle,
    dirty: GuestPtr<()>,
) -> Outcome {
    let Some(d) = device(env, this.addr()) else {
        return Outcome::Return(INVALID);
    };
    if d.scene {
        return Outcome::Return(INVALID);
    }
    let parameters = d.parameters;
    let copy = match presentation::validate(
        env,
        parameters,
        source,
        destination,
        override_window.0,
        dirty,
    ) {
        Ok(copy) => copy,
        Err(error) => return Outcome::Return(error),
    };
    let d = device(env, this.addr()).unwrap();
    if d.back_buffer.is_some_and(|id| {
        matches!(com(env).get_identity(&env.sys.objects, id),
            Some(Payload::Resource(r)) if resources::locked(env, r))
    }) {
        return Outcome::Return(INVALID);
    }
    let surface = d.surface;
    let provider = env.ext.get::<State>().provider;
    let occluded = (provider.visible)(env.ext, &env.sys.objects, d.parameters.hDeviceWindow)
        .is_some_and(|visible| !visible);
    let deadline = match pacing::deadline(
        env.sys.clock.now_us(),
        d.parameters.PresentationInterval,
        (provider.display)(env.ext).2,
        occluded,
    ) {
        Ok(deadline) => deadline,
        Err(()) => return Outcome::Return(UNAVAILABLE),
    };
    let id = com(env)
        .identity::<Payload>(&env.sys.objects, this.addr())
        .unwrap();
    if device(env, this.addr()).unwrap().gpu_lost {
        return Outcome::Return(DEVICE_LOST);
    }
    recording::present_operation(env, id, surface, deadline, copy.map(|copy| (id, copy)))
}
