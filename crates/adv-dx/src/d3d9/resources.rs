//! Guest resource locks and descriptions; all lifetime storage is in Objects.
use super::*;
use adv_abi::types::{D3DLOCKED_RECT, D3DSURFACE_DESC};
use adv_device::surface::Rect;
pub(super) const IID_RESOURCE: Iid = [
    0x5d, 0xc0, 0xee, 5, 0x7d, 0x8f, 0x62, 0x43, 0xb9, 0x99, 0xd1, 0xba, 0xf3, 0x57, 0xc7, 4,
];
const IID_BASE_TEXTURE: Iid = [
    0x7e, 0xa8, 0x0c, 0x58, 0x3c, 0x1d, 0x54, 0x4d, 0x99, 0x1d, 0xb7, 0xd3, 0xe3, 0xc2, 0x98, 0xce,
];
const IID_TEXTURE: Iid = [
    0x27, 0x12, 0xc3, 0x85, 0xe5, 0x3d, 0, 0x4f, 0x9b, 0x3a, 0xf1, 0x1a, 0xc3, 0x8c, 0x18, 0xb5,
];
pub(super) const IID_SURFACE: Iid = [
    0x3a, 0xaf, 0xfb, 0x0c, 0xf6, 0x9f, 0x9a, 0x42, 0x99, 0xb3, 0xa2, 0x79, 0x6a, 0xf8, 0xb8, 0x9b,
];
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Texture,
    Surface,
}
pub(super) struct Resource {
    pub pointer: u32,
    pub device: Id,
    pub kind: Kind,
    pub desc: D3DSURFACE_DESC,
    pub depth: Option<adv_device::gpu::Depth>,
    pub surface: Option<u32>,
    pub storage: u32,
    pub owns_surface: bool,
    /// CPU access requested for a standalone render target.
    pub lockable: bool,
    pub lock: Option<Lock>,
    /// Texture levels borrow the parent's storage and delegate public counts.
    pub container: Option<Id>,
    /// The texture owns all level storage and interface metadata. Borrowed
    /// surfaces select `level` here instead of duplicating lock/owner state.
    pub levels: Vec<Level>,
    pub level: u32,
    pub lod: u32,
}
#[derive(Clone, Copy)]
pub(super) struct Level {
    pub desc: D3DSURFACE_DESC,
    pub offset: u32,
    pub surface: Option<u32>,
    pub interface: Option<Id>,
    pub lock: Option<Lock>,
    /// Pending CPU-to-GPU synchronization: the bounds of the changed texels.
    pub dirty: Option<Rect>,
}
impl Level {
    pub(super) fn whole(&self) -> Rect {
        Rect {
            x: 0,
            y: 0,
            w: self.desc.Width,
            h: self.desc.Height,
        }
    }
    pub(super) fn mark(&mut self, rect: Rect) {
        self.dirty = Some(self.dirty.map_or(rect, |dirty| dirty.union(rect)));
    }
    pub(super) fn mark_all(&mut self) {
        self.mark(self.whole());
    }
    /// Mark a level-zero rectangle on this level, rounded outward as D3D9
    /// scales dirty regions to the sublevels.
    pub(super) fn mark_scaled(&mut self, rect: Rect, index: u32) {
        let x = (rect.x >> index).min(self.desc.Width - 1);
        let y = (rect.y >> index).min(self.desc.Height - 1);
        let right = (rect.x + rect.w).div_ceil(1 << index).min(self.desc.Width);
        let bottom = (rect.y + rect.h).div_ceil(1 << index).min(self.desc.Height);
        self.mark(Rect {
            x,
            y,
            w: right.max(x + 1) - x,
            h: bottom.max(y + 1) - y,
        });
    }
}
impl Resource {
    pub(super) fn new_depth(
        device: Id,
        desc: D3DSURFACE_DESC,
        depth: adv_device::gpu::Depth,
    ) -> Self {
        Self {
            pointer: 0,
            device,
            kind: Kind::Surface,
            desc,
            depth: Some(depth),
            surface: None,
            storage: 0,
            owns_surface: false,
            lockable: false,
            lock: None,
            container: None,
            levels: Vec::new(),
            level: 0,
            lod: 0,
        }
    }
}
#[derive(Clone, Copy)]
pub(super) struct Lock {
    pub rectangle: Rect,
    pub flags: u32,
    /// Pending readback/upload keeps the surface exclusive, but exposes no
    /// writable guest pointer until its Machine continuation completes.
    pub ready: bool,
}
pub(super) fn register(registry: &mut Registry) {
    for api in [
        surface_query_ABI,
        surface_add_ref_ABI,
        surface_release_ABI,
        texture_query_ABI,
        texture_add_ref_ABI,
        texture_release_ABI,
        back_buffer_ABI,
        create_texture_ABI,
        surface_desc_ABI,
        texture_desc_ABI,
        texture_lock_ABI,
        texture_unlock_ABI,
        super::unchanged::unless_unchanged!(set_texture_ABI, super::unchanged::binding),
        get_texture_ABI,
        texture_levels_ABI,
        texture_set_lod_ABI,
        texture_get_lod_ABI,
        texture_surface_ABI,
        surface_container_ABI,
        surface_device_ABI,
        texture_device_ABI,
        surface_type_ABI,
        texture_type_ABI,
    ] {
        registry.register(api);
    }
}
unknown!(
    surface_query,
    surface_add_ref,
    surface_release,
    "IDirect3DSurface9::QueryInterface",
    "IDirect3DSurface9::AddRef",
    "IDirect3DSurface9::Release"
);
unknown!(
    texture_query,
    texture_add_ref,
    texture_release,
    "IDirect3DTexture9::QueryInterface",
    "IDirect3DTexture9::AddRef",
    "IDirect3DTexture9::Release"
);
pub(super) fn resource<'a>(env: &'a Env<'_>, pointer: u32) -> Option<&'a Resource> {
    match com(env).get::<Payload>(&env.sys.objects, pointer)? {
        Payload::Resource(r) => Some(r),
        _ => None,
    }
}
pub(super) fn container_pointer(env: &Env<'_>, pointer: u32) -> Option<u32> {
    let id = resource(env, pointer)?.container?;
    match com(env).get_identity(&env.sys.objects, id)? {
        Payload::Resource(r) if r.kind == Kind::Texture => Some(r.pointer),
        _ => None,
    }
}
pub(super) fn container_level(env: &Env<'_>, pointer: u32) -> Option<(u32, u32)> {
    Some((
        container_pointer(env, pointer)?,
        resource(env, pointer)?.level,
    ))
}
/// Resolve a borrowed surface through its sole pixel owner. The view itself
/// never caches a heap address that could outlive its controlling texture.
pub(super) fn storage(env: &Env<'_>, r: &Resource) -> Option<u32> {
    if let Some(container) = r.container {
        let Some(Payload::Resource(parent)) = com(env).get_identity(&env.sys.objects, container)
        else {
            return None;
        };
        let selected = parent.levels.get(r.level as usize)?;
        (parent.storage != 0).then(|| parent.storage + selected.offset)
    } else {
        (r.storage != 0).then_some(r.storage)
    }
}
/// A texture lock only lends its CPU storage; the GPU image keeps the last
/// unlocked contents, so draws sample those as native D3D9 does. GDI leases
/// and texture transfers still change the image under the draw.
pub(super) fn sampling_blocked(env: &Env<'_>, r: &Resource) -> bool {
    dc::leased(env, r) || update_texture::reserved(env, r)
}
pub(super) fn locked(env: &Env<'_>, r: &Resource) -> bool {
    if dc::leased(env, r) || update_texture::reserved(env, r) {
        return true;
    }
    if let Some(container) = r.container {
        return match com(env).get_identity(&env.sys.objects, container) {
            Some(Payload::Resource(parent)) => parent
                .levels
                .get(r.level as usize)
                .is_none_or(|level| level.lock.is_some()),
            _ => true,
        };
    }
    r.lock.is_some() || r.levels.iter().any(|level| level.lock.is_some())
}
/// Bindings keep the texture's single backing alive when they refer to a level.
pub(super) fn binding_owner(env: &Env<'_>, id: Id) -> Id {
    match com(env).get_identity(&env.sys.objects, id) {
        Some(Payload::Resource(r)) => r.container.unwrap_or(id),
        _ => id,
    }
}

#[win32(d3d9,name="IDirect3DTexture9::GetSurfaceLevel",level=implement)]
fn texture_surface(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    level: u32,
    output: GuestPtr<u32>,
) -> u32 {
    if output.write(env.cpu, 0).is_err() {
        return INVALID;
    }
    let Some(r) = resource(env, this.addr()).filter(|r| r.kind == Kind::Texture) else {
        return INVALID;
    };
    let Some(selected) = r.levels.get(level as usize).copied() else {
        return INVALID;
    };
    let container = com(env)
        .identity::<Payload>(&env.sys.objects, this.addr())
        .unwrap();
    let level_id = if let Some(id) = selected.interface {
        id
    } else {
        // Only interface metadata is new. Storage and lock state remain owned
        // by the texture; the level neither copies pixels nor owns a parent ref.
        let value = Resource {
            pointer: 0,
            device: r.device,
            kind: Kind::Surface,
            desc: selected.desc,
            depth: None,
            surface: selected.surface,
            storage: 0,
            owns_surface: false,
            lockable: false,
            lock: None,
            container: Some(container),
            levels: Vec::new(),
            level,
            lod: 0,
        };
        let Some((pointer, id)) = object(
            env,
            "IDirect3DSurface9",
            IID_SURFACE,
            Payload::Resource(value),
        ) else {
            return OUT_OF_MEMORY;
        };
        if let Some(Payload::Resource(r)) = com(env).get_identity_mut(&mut env.sys.objects, id) {
            r.pointer = pointer;
        }
        assert!(com(env).register_interface(&mut env.sys.objects, id, IID_RESOURCE, pointer));
        if !com(env).retain_internal(&mut env.sys.objects, id) {
            if let Some(released) = com(env).release_identity(&mut env.sys.objects, id)
                && let Some(dead) = released.destroyed
            {
                super::destroy(env, dead);
            }
            return OUT_OF_MEMORY;
        }
        // This is the interface metadata's initial owner, not a public view
        // Release (which delegates to the texture's public reference count).
        assert!(
            com(env)
                .release_identity(&mut env.sys.objects, id)
                .is_some()
        );
        if let Some(Payload::Resource(r)) =
            com(env).get_identity_mut(&mut env.sys.objects, container)
        {
            r.levels[level as usize].interface = Some(id);
        }
        id
    };
    let Some(Payload::Resource(r)) = com(env).get_identity(&env.sys.objects, level_id) else {
        return INVALID;
    };
    let pointer = r.pointer;
    if add_ref(env, pointer).is_none() {
        return OUT_OF_MEMORY;
    }
    if output.write(env.cpu, pointer).is_err() {
        release(env, pointer);
        return INVALID;
    }
    0
}

#[win32(d3d9,name="IDirect3DSurface9::GetContainer",level=implement)]
fn surface_container(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    iid: GuestPtr<Iid>,
    output: GuestPtr<u32>,
) -> Outcome {
    if output.write(env.cpu, 0).is_err() {
        return Outcome::Return(INVALID);
    }
    let Some(r) = resource(env, this.addr()).filter(|r| r.kind == Kind::Surface) else {
        return Outcome::Return(INVALID);
    };
    let parent = if let Some(container) = r.container {
        container
    } else if r.owns_surface || r.depth.is_some() || r.desc.Pool >= 2 {
        r.device
    } else {
        // Back buffers have a swap-chain container, not a device container.
        return Outcome::Return(UNAVAILABLE);
    };
    let pointer = match com(env).get_identity(&env.sys.objects, parent) {
        Some(Payload::Resource(r)) => r.pointer,
        Some(Payload::Device(d)) => d.pointer,
        _ => return Outcome::Return(INVALID),
    };
    query(env, GuestPtr::new(pointer), iid, output)
}

fn resource_device(env: &mut Env<'_>, this: u32, output: GuestPtr<u32>) -> u32 {
    if output.write(env.cpu, 0).is_err() {
        return INVALID;
    }
    let Some(r) = resource(env, this) else {
        return INVALID;
    };
    let Some(Payload::Device(d)) = com(env).get_identity(&env.sys.objects, r.device) else {
        return INVALID;
    };
    let pointer = d.pointer;
    if add_ref(env, pointer).is_none() {
        return OUT_OF_MEMORY;
    }
    if output.write(env.cpu, pointer).is_err() {
        release(env, pointer);
        return INVALID;
    }
    0
}
#[win32(d3d9,name="IDirect3DSurface9::GetDevice",level=implement)]
fn surface_device(env: &mut Env<'_>, this: GuestPtr<()>, output: GuestPtr<u32>) -> u32 {
    resource_device(env, this.addr(), output)
}
#[win32(d3d9,name="IDirect3DTexture9::GetDevice",level=implement)]
fn texture_device(env: &mut Env<'_>, this: GuestPtr<()>, output: GuestPtr<u32>) -> u32 {
    resource_device(env, this.addr(), output)
}
#[win32(d3d9,name="IDirect3DSurface9::GetType",level=implement)]
fn surface_type(env: &mut Env<'_>, this: GuestPtr<()>) -> u32 {
    u32::from(resource(env, this.addr()).is_some_and(|r| r.kind == Kind::Surface))
}
#[win32(d3d9,name="IDirect3DTexture9::GetType",level=implement)]
fn texture_type(env: &mut Env<'_>, this: GuestPtr<()>) -> u32 {
    if resource(env, this.addr()).is_some_and(|r| r.kind == Kind::Texture) {
        3
    } else {
        0
    }
}
pub(super) fn parent_on_last_public_release(env: &Env<'_>, pointer: u32) -> Option<Id> {
    let r = resource(env, pointer)?;
    let id = com(env).identity::<Payload>(&env.sys.objects, pointer)?;
    (com(env).references(&env.sys.objects, id)? == 1).then_some(r.device)
}
pub(super) fn parent_on_first_public_retain(env: &Env<'_>, pointer: u32) -> Option<Id> {
    let r = resource(env, pointer)?;
    let id = com(env).identity::<Payload>(&env.sys.objects, pointer)?;
    (com(env).references(&env.sys.objects, id)? == 0).then_some(r.device)
}
pub(super) fn destroy(env: &mut Env<'_>, r: Resource) {
    for level in &r.levels {
        if let Some(interface) = level.interface
            && let Some(Some(dead)) = com(env).release_internal(&mut env.sys.objects, interface)
        {
            super::destroy(env, dead);
        }
    }
    if r.storage != 0 {
        free(env, r.storage);
    }
    if r.owns_surface {
        let provider = env.ext.get::<State>().provider;
        for surface in owned_surfaces(&r) {
            let _ = (provider.remove)(env.ext, env.cpu, surface);
        }
    }
}
fn owned_surfaces(r: &Resource) -> Vec<u32> {
    if !r.owns_surface {
        Vec::new()
    } else if r.levels.is_empty() {
        r.surface.into_iter().collect()
    } else {
        r.levels.iter().filter_map(|level| level.surface).collect()
    }
}
pub(super) fn make(env: &mut Env<'_>, mut r: Resource) -> Option<(u32, Id)> {
    let (interface, iid) = match r.kind {
        Kind::Surface => ("IDirect3DSurface9", IID_SURFACE),
        Kind::Texture => ("IDirect3DTexture9", IID_TEXTURE),
    };
    if com(env)
        .add_ref_identity(&mut env.sys.objects, r.device)
        .is_none()
    {
        destroy(env, r);
        return None;
    }
    let parent = r.device;
    let cleanup = (r.storage, owned_surfaces(&r));
    r.pointer = 0;
    let Some((pointer, id)) = object(env, interface, iid, Payload::Resource(r)) else {
        if cleanup.0 != 0 {
            free(env, cleanup.0);
        }
        for surface in cleanup.1 {
            let provider = env.ext.get::<State>().provider;
            let _ = (provider.remove)(env.ext, env.cpu, surface);
        }
        let _ = com(env).release_identity(&mut env.sys.objects, parent);
        return None;
    };
    if let Some(Payload::Resource(r)) = com(env).get_identity_mut(&mut env.sys.objects, id) {
        r.pointer = pointer;
    }
    assert!(com(env).register_interface(&mut env.sys.objects, id, IID_RESOURCE, pointer));
    if interface == "IDirect3DTexture9" {
        assert!(com(env).register_interface(&mut env.sys.objects, id, IID_BASE_TEXTURE, pointer));
    }
    Some((pointer, id))
}
pub(super) fn implicit_back_buffer(
    env: &mut Env<'_>,
    device: Id,
    p: D3DPRESENT_PARAMETERS,
    surface: u32,
) -> Option<Id> {
    let (pointer, id) = make(
        env,
        Resource {
            pointer: 0,
            device,
            kind: Kind::Surface,
            desc: D3DSURFACE_DESC {
                Format: p.BackBufferFormat,
                Type: 1,
                Usage: 1,
                Pool: 0,
                MultiSampleType: 0,
                MultiSampleQuality: 0,
                Width: p.BackBufferWidth,
                Height: p.BackBufferHeight,
            },
            depth: None,
            surface: Some(surface),
            storage: 0,
            owns_surface: false,
            lockable: false,
            lock: None,
            container: None,
            levels: Vec::new(),
            level: 0,
            lod: 0,
        },
    )?;
    if !com(env).retain_internal(&mut env.sys.objects, id) {
        release(env, pointer);
        return None;
    }
    assert_eq!(release(env, pointer), 0);
    Some(id)
}
#[win32(d3d9,name="IDirect3DDevice9::GetBackBuffer",level=implement)]
pub(super) fn back_buffer(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    swap: u32,
    index: u32,
    kind: i32,
    output: GuestPtr<u32>,
) -> Outcome {
    if output.write(env.cpu, 0).is_err() {
        return Outcome::Return(INVALID);
    }
    let Some(d) = device(env, this.addr()) else {
        return Outcome::Return(INVALID);
    };
    if swap != 0 || index != 0 || kind != 0 {
        return Outcome::Return(INVALID);
    }
    let Some(Payload::Resource(r)) =
        com(env).get_identity(&env.sys.objects, d.back_buffer.unwrap())
    else {
        return Outcome::Return(INVALID);
    };
    let pointer = r.pointer;
    if add_ref(env, pointer).is_none() {
        return Outcome::Return(OUT_OF_MEMORY);
    }
    if output.write(env.cpu, pointer).is_err() {
        release(env, pointer);
        return Outcome::Return(INVALID);
    }
    Outcome::Return(0)
}
#[win32(d3d9,name="IDirect3DDevice9::CreateTexture",level=implement)]
pub(super) fn create_texture(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    width: u32,
    height: u32,
    levels: u32,
    usage: u32,
    format: u32,
    pool: i32,
    output: GuestPtr<u32>,
    shared: GuestPtr<u32>,
) -> Outcome {
    if output.write(env.cpu, 0).is_err() {
        return Outcome::Return(INVALID);
    }
    if device(env, this.addr()).is_none() {
        return Outcome::Return(INVALID);
    }
    // Software vertex processing does not apply to textures; the runtime
    // accepts the flag in every pool.
    let usage = usage & !0x10;
    if !shared.is_null()
        || format::bytes(format).is_none()
        || !(0..=3).contains(&pool)
        || usage & !(1 | 0x200) != 0
        || (pool != 0 && usage != 0)
        || (usage & 1 != 0 && usage & 0x200 != 0)
        || (matches!(format, 23 | 26 | 28) && usage & 1 != 0)
    {
        let reason = if format::bytes(format).is_none() {
            "pixel format is unsupported (supported: 21/22/23/26/28)"
        } else if matches!(format, 23 | 26 | 28) && usage & 1 != 0 {
            "packed or alpha-only render targets are unavailable; formats 23/26/28 support sampled textures"
        } else {
            "usage/pool or shared-handle combination is unsupported"
        };
        adv_abi::warn_unavailable(
            env,
            "IDirect3DDevice9::CreateTexture",
            &format!("{reason}: format={format}, usage={usage:#x}, pool={pool}"),
        );
        return Outcome::Return(UNAVAILABLE);
    }
    let desc = D3DSURFACE_DESC {
        Format: format,
        Type: 1,
        Usage: usage,
        Pool: pool,
        MultiSampleType: 0,
        MultiSampleQuality: 0,
        Width: width,
        Height: height,
    };
    let Some((mut levels, len)) = level_layout(desc, levels) else {
        return Outcome::Return(INVALID);
    };
    // SYSTEMMEM/SCRATCH are guest storage, never sampleable GPU copies. The
    // default and managed pools retain one texture with shared mip views.
    if pool < 2 {
        let target = match env.ext.get::<State>().gpu.as_ref() {
            Some(g) => g.target_levels(width, height, levels.len() as u32),
            None => return Outcome::Return(UNAVAILABLE),
        };
        let target = match target {
            Ok(t) => t.with_alpha_mode(targets::alpha_mode(format)),
            Err(e) => return gpu_error(e),
        };
        let provider = env.ext.get::<State>().provider;
        for index in 0..levels.len() {
            let view = if index == 0 {
                target.clone()
            } else {
                target.mip_level(index as u32).unwrap()
            };
            match (provider.insert)(env.ext, view) {
                Ok(surface) => levels[index].surface = Some(surface),
                Err(_) => {
                    remove_levels(env, &levels);
                    return Outcome::Return(OUT_OF_MEMORY);
                }
            }
        }
    }
    let storage = if usage & 1 == 0 {
        alloc(env, len)
    } else {
        Some(0)
    };
    let Some(storage) = storage else {
        remove_levels(env, &levels);
        return Outcome::Return(OUT_OF_MEMORY);
    };
    let device = com(env)
        .identity::<Payload>(&env.sys.objects, this.addr())
        .unwrap();
    let Some((pointer, _)) = make(
        env,
        Resource {
            pointer: 0,
            device,
            kind: Kind::Texture,
            desc,
            depth: None,
            surface: levels[0].surface,
            storage,
            owns_surface: pool < 2,
            lockable: false,
            lock: None,
            container: None,
            levels,
            level: 0,
            lod: 0,
        },
    ) else {
        return Outcome::Return(OUT_OF_MEMORY);
    };
    if output.write(env.cpu, pointer).is_err() {
        release(env, pointer);
        return Outcome::Return(INVALID);
    }
    Outcome::Return(0)
}
/// A full 4096-square ARGB mip chain, the largest texture the caps advertise.
const CHAIN_BYTES: u32 = 89_478_484;
/// One checked guest allocation spans every level. GPU views and COM metadata
/// remain bounded by the full chain; they do not own separate pixel buffers.
fn level_layout(desc: D3DSURFACE_DESC, count: u32) -> Option<(Vec<Level>, u32)> {
    if desc.Width == 0 || desc.Height == 0 {
        return None;
    }
    let maximum = 32 - desc.Width.max(desc.Height).leading_zeros();
    let count = if count == 0 { maximum } else { count };
    if count > maximum {
        return None;
    }
    let mut length = 0u32;
    let mut levels = Vec::with_capacity(count as usize);
    for index in 0..count {
        let mut level_desc = desc;
        level_desc.Width = (desc.Width >> index).max(1);
        level_desc.Height = (desc.Height >> index).max(1);
        let bytes = format::length(level_desc.Format, level_desc.Width, level_desc.Height)?;
        let offset = length;
        length = length.checked_add(bytes).filter(|&n| n <= CHAIN_BYTES)?;
        levels.push(Level {
            desc: level_desc,
            offset,
            surface: None,
            interface: None,
            lock: None,
            dirty: Some(Rect {
                x: 0,
                y: 0,
                w: level_desc.Width,
                h: level_desc.Height,
            }),
        });
    }
    Some((levels, length))
}
fn remove_levels(env: &mut Env<'_>, levels: &[Level]) {
    let provider = env.ext.get::<State>().provider;
    for surface in levels.iter().filter_map(|level| level.surface) {
        let _ = (provider.remove)(env.ext, env.cpu, surface);
    }
}
#[win32(d3d9,name="IDirect3DSurface9::GetDesc",level=implement)]
fn surface_desc(env: &mut Env<'_>, this: GuestPtr<()>, output: GuestPtr<D3DSURFACE_DESC>) -> u32 {
    let Some(r) = resource(env, this.addr()).filter(|r| r.kind == Kind::Surface) else {
        return INVALID;
    };
    let desc = r.desc;
    if output.write(env.cpu, desc).is_ok() {
        0
    } else {
        INVALID
    }
}
#[win32(d3d9,name="IDirect3DTexture9::GetLevelDesc",level=implement)]
fn texture_desc(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    level: u32,
    output: GuestPtr<D3DSURFACE_DESC>,
) -> u32 {
    let Some(r) = resource(env, this.addr()).filter(|r| r.kind == Kind::Texture) else {
        return INVALID;
    };
    let Some(selected) = r.levels.get(level as usize) else {
        return INVALID;
    };
    let desc = selected.desc;
    if output.write(env.cpu, desc).is_ok() {
        0
    } else {
        INVALID
    }
}
#[win32(d3d9,name="IDirect3DTexture9::GetLevelCount",level=implement)]
fn texture_levels(env: &mut Env<'_>, this: GuestPtr<()>) -> u32 {
    resource(env, this.addr())
        .filter(|r| r.kind == Kind::Texture)
        .map_or(0, |r| r.levels.len() as u32)
}
#[win32(d3d9,name="IDirect3DTexture9::SetLOD",level=state)]
fn texture_set_lod(env: &mut Env<'_>, this: GuestPtr<()>, lod: u32) -> u32 {
    let Some(Payload::Resource(r)) = com(env).get_mut::<Payload>(&mut env.sys.objects, this.addr())
    else {
        return 0;
    };
    if r.kind != Kind::Texture || r.desc.Pool != 1 {
        return 0;
    }
    let old = r.lod;
    r.lod = lod.min((r.levels.len() as u32).saturating_sub(1));
    old
}
#[win32(d3d9,name="IDirect3DTexture9::GetLOD",level=implement)]
fn texture_get_lod(env: &mut Env<'_>, this: GuestPtr<()>) -> u32 {
    resource(env, this.addr())
        .filter(|r| r.kind == Kind::Texture && r.desc.Pool == 1)
        .map_or(0, |r| r.lod)
}
#[win32(d3d9,name="IDirect3DTexture9::LockRect",level=implement)]
pub(super) fn texture_lock(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    level: u32,
    output: GuestPtr<D3DLOCKED_RECT>,
    rectangle: GuestPtr<RECT>,
    flags: u32,
) -> u32 {
    if resource(env, this.addr())
        .is_some_and(|r| dc::leased(env, r) || update_texture::reserved(env, r))
    {
        return INVALID;
    }
    let Some(r) = resource(env, this.addr()).filter(|r| r.kind == Kind::Texture) else {
        return INVALID;
    };
    let Some(selected) = r.levels.get(level as usize).copied() else {
        return INVALID;
    };
    let flags = if r.desc.Usage & 0x200 == 0 {
        flags & !0x2000
    } else {
        flags
    };
    if selected.lock.is_some()
        || r.storage == 0
        || r.desc.Usage & 1 != 0
        || (r.desc.Pool == 0 && r.desc.Usage & 0x200 == 0)
        || flags & !(0x10 | 0x800 | 0x2000 | 0x8000) != 0
        || (flags & 0x2000 != 0
            && (r.desc.Usage & 0x200 == 0
                || level != 0
                || !rectangle.is_null()
                || flags & 0x10 != 0))
    {
        return INVALID;
    }
    let rect = if rectangle.is_null() {
        Rect {
            x: 0,
            y: 0,
            w: selected.desc.Width,
            h: selected.desc.Height,
        }
    } else {
        let Ok(rect) = rectangle.read(env.cpu) else {
            return INVALID;
        };
        if rect.left < 0
            || rect.top < 0
            || rect.right <= rect.left
            || rect.bottom <= rect.top
            || rect.right as u32 > selected.desc.Width
            || rect.bottom as u32 > selected.desc.Height
        {
            return INVALID;
        }
        Rect {
            x: rect.left as u32,
            y: rect.top as u32,
            w: (rect.right - rect.left) as u32,
            h: (rect.bottom - rect.top) as u32,
        }
    };
    let Some(pitch) = format::pitch(selected.desc.Format, selected.desc.Width) else {
        return INVALID;
    };
    let bytes = format::bytes(selected.desc.Format).unwrap();
    let bits = r.storage + selected.offset + rect.y * pitch + rect.x * bytes;
    if output
        .write(
            env.cpu,
            D3DLOCKED_RECT {
                Pitch: pitch as i32,
                pBits: bits,
            },
        )
        .is_err()
    {
        return INVALID;
    }
    if let Some(Payload::Resource(r)) =
        com(env).get_mut::<Payload>(&mut env.sys.objects, this.addr())
    {
        r.levels[level as usize].lock = Some(Lock {
            rectangle: rect,
            flags,
            ready: true,
        });
        if flags & 0x10 == 0 && (flags & 0x8000 == 0 || r.desc.Pool == 0) {
            if level == 0 {
                for (index, selected) in r.levels.iter_mut().enumerate() {
                    selected.mark_scaled(rect, index as u32);
                }
            } else {
                r.levels[level as usize].mark(rect);
            }
        }
    }
    0
}
#[win32(d3d9,name="IDirect3DTexture9::UnlockRect",level=implement)]
pub(super) fn texture_unlock(env: &mut Env<'_>, this: GuestPtr<()>, level: u32) -> Outcome {
    if resource(env, this.addr()).is_some_and(|r| dc::leased(env, r)) {
        return Outcome::Return(INVALID);
    }
    texture_unlock_internal(env, this, level)
}
pub(super) fn texture_unlock_internal(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    level: u32,
) -> Outcome {
    if resource(env, this.addr()).is_some_and(|r| update_texture::reserved(env, r)) {
        return Outcome::Return(INVALID);
    }
    let Some(r) = resource(env, this.addr()).filter(|r| r.kind == Kind::Texture) else {
        return Outcome::Return(INVALID);
    };
    let Some(selected) = r.levels.get(level as usize).copied() else {
        return Outcome::Return(INVALID);
    };
    let Some(lock) = selected.lock else {
        // The authored native oracle confirms repeated UnlockRect on a valid
        // texture level succeeds. Invalid interfaces/levels still fail above.
        return dirty::flush(env, this.addr());
    };
    if !lock.ready {
        return Outcome::Return(INVALID);
    }
    if let Some(Payload::Resource(r)) =
        com(env).get_mut::<Payload>(&mut env.sys.objects, this.addr())
    {
        r.levels[level as usize].lock = None;
    }
    dirty::flush(env, this.addr())
}
#[win32(d3d9,name="IDirect3DDevice9::SetTexture",level=state)]
pub(super) fn set_texture(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    stage: u32,
    texture: GuestPtr<()>,
) -> Outcome {
    let Some(device) = com(env)
        .identity::<Payload>(&env.sys.objects, this.addr())
        .filter(|_| super::device(env, this.addr()).is_some())
    else {
        return Outcome::Return(INVALID);
    };
    if super::programmable::sampler_slot(stage).is_none() {
        return Outcome::Return(INVALID);
    }
    if !texture.is_null()
        && resource(env, texture.addr()).is_some_and(|r| {
            r.kind == Kind::Texture
                && r.desc.Pool == 1
                && (residency::cold(env, texture.addr())
                    || r.levels.iter().any(|level| level.dirty.is_some()))
        })
    {
        let Some(r) = resource(env, texture.addr()).filter(|r| r.device == device) else {
            return Outcome::Return(INVALID);
        };
        // Binding a CPU-locked texture is valid. Unlock or DC release flushes it;
        // draws in between sample the last uploaded contents.
        if !locked(env, r) {
            let restored = residency::ensure(env, texture.addr());
            if restored != 0 {
                return Outcome::Return(restored);
            }
            let outcome = dirty::flush(env, texture.addr());
            return texture_restored(env, this.addr(), stage, texture.addr(), outcome);
        }
    }
    Outcome::Return(set_texture_binding(env, this, stage, texture))
}
fn texture_restored(
    env: &mut Env<'_>,
    device: u32,
    stage: u32,
    texture: u32,
    outcome: Outcome,
) -> Outcome {
    match outcome {
        Outcome::Return(0) => Outcome::Return(set_texture_binding(
            env,
            GuestPtr::new(device),
            stage,
            GuestPtr::new(texture),
        )),
        Outcome::Block {
            wait,
            then,
            allow_time_skip,
        } => Outcome::Block {
            wait,
            allow_time_skip,
            then: Box::new(move |env, resume| {
                let result = then(env, resume);
                texture_restored(env, device, stage, texture, result)
            }),
        },
        other => other,
    }
}
fn set_texture_binding(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    stage: u32,
    texture: GuestPtr<()>,
) -> u32 {
    let Some(d) = device(env, this.addr()) else {
        return INVALID;
    };
    let Some(slot) = super::programmable::sampler_slot(stage) else {
        return INVALID;
    };
    let old = if slot == 0 {
        d.texture
    } else {
        d.shaders.textures[slot]
    };
    let device_id = com(env)
        .identity::<Payload>(&env.sys.objects, this.addr())
        .unwrap();
    let candidate = if texture.is_null() {
        None
    } else {
        let Some(id) = com(env).identity::<Payload>(&env.sys.objects, texture.addr()) else {
            return INVALID;
        };
        Some(id)
    };
    if d.recording.is_none() && old == candidate {
        return 0;
    }
    let new = if texture.is_null() {
        None
    } else {
        let Some(Payload::Resource(r)) =
            candidate.and_then(|id| com(env).get_identity(&env.sys.objects, id))
        else {
            return INVALID;
        };
        if r.kind != Kind::Texture || r.device != device_id || r.desc.Pool > 1 {
            return INVALID;
        }
        candidate
    };
    if let Some(result) =
        super::shaders::stateblocks::record_texture_slot(env, device_id, slot, new)
    {
        return result;
    }
    if old == new {
        return 0;
    }
    if let Some(id) = new
        && !com(env).retain_internal(&mut env.sys.objects, id)
    {
        return OUT_OF_MEMORY;
    }
    if let Some(Payload::Device(d)) = com(env).get_mut::<Payload>(&mut env.sys.objects, this.addr())
    {
        if slot == 0 {
            d.texture = new;
        } else {
            d.shaders.textures[slot] = new;
        }
        d.touch(12);
    }
    if let Some(id) = old
        && let Some(Some(dead)) = com(env).release_internal(&mut env.sys.objects, id)
    {
        super::destroy(env, dead);
    }
    0
}

#[win32(d3d9,name="IDirect3DDevice9::GetTexture",level=implement)]
fn get_texture(env: &mut Env<'_>, this: GuestPtr<()>, stage: u32, output: GuestPtr<u32>) -> u32 {
    if output.write(env.cpu, 0).is_err() {
        return INVALID;
    }
    let Some(d) = device(env, this.addr()) else {
        return INVALID;
    };
    let Some(slot) = super::programmable::sampler_slot(stage) else {
        return INVALID;
    };
    let Some(id) = (if slot == 0 {
        d.texture
    } else {
        d.shaders.textures[slot]
    }) else {
        return 0;
    };
    let Some(Payload::Resource(r)) = com(env).get_identity(&env.sys.objects, id) else {
        return INVALID;
    };
    let pointer = r.pointer;
    if add_ref(env, pointer).is_none() {
        return OUT_OF_MEMORY;
    }
    if output.write(env.cpu, pointer).is_err() {
        release(env, pointer);
        return INVALID;
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mip_layout_bounds_the_complete_chain_and_halves_rectangular_npot_dimensions() {
        let desc = D3DSURFACE_DESC {
            Format: 21,
            Width: 3,
            Height: 5,
            ..D3DSURFACE_DESC::default()
        };
        let (levels, length) = level_layout(desc, 0).unwrap();
        assert_eq!(length, 72);
        assert_eq!(
            levels
                .iter()
                .map(|l| (l.desc.Width, l.desc.Height, l.offset))
                .collect::<Vec<_>>(),
            [(3, 5, 0), (1, 2, 60), (1, 1, 68)]
        );
        assert!(level_layout(desc, 4).is_none());
        assert_eq!(level_layout(desc, 2).unwrap().1, 68);
        assert!(level_layout(D3DSURFACE_DESC { Width: 0, ..desc }, 0).is_none());
        let limit = D3DSURFACE_DESC {
            Width: 4096,
            Height: 4096,
            ..desc
        };
        assert_eq!(level_layout(limit, 1).unwrap().1, 64 * 1024 * 1024);
        assert_eq!(level_layout(limit, 0).unwrap().1, CHAIN_BYTES);
        assert!(
            level_layout(
                D3DSURFACE_DESC {
                    Width: 8192,
                    ..limit
                },
                1
            )
            .is_none(),
            "one level stays within the 64 MiB level budget"
        );
        assert!(
            level_layout(
                D3DSURFACE_DESC {
                    Width: u32::MAX,
                    Height: u32::MAX,
                    ..desc
                },
                0
            )
            .is_none()
        );
    }

    #[test]
    fn level_reference_limit_and_invalid_output_preserve_parent_and_single_backing() {
        let mut m = super::super::tests::machine();
        m.ext
            .get_mut::<State>()
            .tables
            .insert("IDirect3DTexture9", 0);
        let mut env = Env {
            context: m.sys.current,
            cpu: &mut m.cpu,
            sys: &mut m.sys,
            ext: &mut m.ext,
            thunk: 0,
        };
        let (_, device, _, original) = super::super::tests::setup(&mut env);
        if let Some(Some(dead)) = com(&env).release_internal(&mut env.sys.objects, original) {
            super::super::destroy(&mut env, dead);
        }
        let (texture, identity) = make(
            &mut env,
            Resource {
                pointer: 0,
                device,
                kind: Kind::Texture,
                desc: D3DSURFACE_DESC {
                    Width: 2,
                    Height: 2,
                    Pool: 1,
                    Type: 1,
                    Format: 21,
                    ..D3DSURFACE_DESC::default()
                },
                depth: None,
                surface: None,
                storage: 0,
                owns_surface: false,
                lockable: false,
                lock: None,
                container: None,
                levels: level_layout(
                    D3DSURFACE_DESC {
                        Width: 2,
                        Height: 2,
                        Pool: 1,
                        Type: 1,
                        Format: 21,
                        ..D3DSURFACE_DESC::default()
                    },
                    1,
                )
                .unwrap()
                .0,
                level: 0,
                lod: 0,
            },
        )
        .unwrap();
        let output = GuestPtr::new(alloc(&mut env, 4).unwrap());
        let count = env.sys.objects.object_count();
        assert_eq!(
            texture_surface(&mut env, GuestPtr::new(texture), 0, GuestPtr::new(0)),
            INVALID
        );
        assert_eq!(env.sys.objects.object_count(), count);
        assert_eq!(add_ref(&mut env, texture), Some(2));
        assert_eq!(
            texture_surface(&mut env, GuestPtr::new(texture), 0, output),
            OUT_OF_MEMORY
        );
        assert_eq!(output.read(env.cpu).unwrap(), 0);
        assert_eq!(com(&env).references(&env.sys.objects, identity), Some(2));
        assert_eq!(release(&mut env, texture), 1);
        assert_eq!(
            texture_surface(&mut env, GuestPtr::new(texture), 0, output),
            0
        );
        let surface = output.read(env.cpu).unwrap();
        let level = com(&env)
            .identity::<Payload>(&env.sys.objects, surface)
            .unwrap();
        assert_eq!(binding_owner(&env, level), identity);
        assert_eq!(com(&env).references(&env.sys.objects, level), Some(0));
        assert_eq!(add_ref(&mut env, surface), None);
        assert_eq!(release(&mut env, surface), 1);
        assert_eq!(release(&mut env, texture), 0);
        assert!(com(&env).get_identity(&env.sys.objects, level).is_none());
        assert!(com(&env).get_identity(&env.sys.objects, identity).is_none());
        assert_eq!(env.sys.objects.object_count(), count - 1);
    }

    #[test]
    fn system_memory_levels_share_offset_storage_and_pending_readback_retains_the_actual_parent() {
        struct TestTarget(Target);
        let mut m = super::super::tests::machine();
        m.sys.objects = Objects::default();
        m.ext.get_mut::<State>().com = Com::new(&mut m.sys.objects).unwrap();
        m.ext
            .get_mut::<State>()
            .tables
            .insert("IDirect3DTexture9", 0);
        let gpu = pollster::block_on(Gpu::request(!cfg!(any(
            target_os = "macos",
            target_os = "android"
        ))))
        .unwrap();
        m.ext.insert(TestTarget(gpu.target(4, 2, false).unwrap()));
        m.ext.get_mut::<State>().provider.target =
            |ext, id| (id == 7).then(|| ext.get::<TestTarget>().0.clone());
        let wake = adv_sync::WakeEvent::new().unwrap();
        let mut builder = adv_sync::background::Builder::new(wake.clone()).unwrap();
        let sender = builder.sender(1, 1).unwrap().into_local();
        let _service = builder.start().unwrap();
        m.ext.get_mut::<State>().sender = Some(sender);
        m.ext.get_mut::<State>().wake = Some(std::sync::Arc::new(wake));
        let mut env = Env {
            context: m.sys.current,
            cpu: &mut m.cpu,
            sys: &mut m.sys,
            ext: &mut m.ext,
            thunk: 0,
        };
        let (device_pointer, device, _, original) = super::super::tests::setup(&mut env);
        if let Some(Some(dead)) = com(&env).release_internal(&mut env.sys.objects, original) {
            super::super::destroy(&mut env, dead);
        }
        let (source, _) = make(
            &mut env,
            Resource {
                pointer: 0,
                device,
                kind: Kind::Surface,
                desc: D3DSURFACE_DESC {
                    Width: 4,
                    Height: 2,
                    Format: 21,
                    Type: 1,
                    Usage: 1,
                    ..D3DSURFACE_DESC::default()
                },
                depth: None,
                surface: Some(7),
                storage: 0,
                owns_surface: false,
                lockable: false,
                lock: None,
                container: None,
                levels: Vec::new(),
                level: 0,
                lod: 0,
            },
        )
        .unwrap();
        let output = GuestPtr::new(alloc(&mut env, 4).unwrap());
        assert!(matches!(
            create_texture(
                &mut env,
                GuestPtr::new(device_pointer),
                8,
                4,
                0,
                0,
                21,
                2,
                output,
                GuestPtr::new(0)
            ),
            Outcome::Return(0)
        ));
        let texture = output.read(env.cpu).unwrap();
        let texture_id = com(&env)
            .identity::<Payload>(&env.sys.objects, texture)
            .unwrap();
        let parent = resource(&env, texture).unwrap();
        let base = parent.storage;
        assert!(
            parent.surface.is_none() && parent.levels.iter().all(|l| l.surface.is_none()),
            "system memory never calls the GPU surface provider"
        );
        assert_eq!(texture_set_lod(&mut env, GuestPtr::new(texture), 3), 0);
        assert_eq!(texture_get_lod(&mut env, GuestPtr::new(texture)), 0);
        assert_eq!(
            texture_surface(&mut env, GuestPtr::new(texture), 1, output),
            0
        );
        let surface = output.read(env.cpu).unwrap();
        let surface_id = com(&env)
            .identity::<Payload>(&env.sys.objects, surface)
            .unwrap();
        assert_eq!(
            storage(&env, resource(&env, surface).unwrap()),
            Some(base + 128)
        );
        let locked_rect = GuestPtr::new(alloc(&mut env, 8).unwrap());
        assert_eq!(
            texture_lock(
                &mut env,
                GuestPtr::new(texture),
                1,
                locked_rect,
                GuestPtr::new(0),
                0x800
            ),
            0
        );
        let lock = locked_rect.read(env.cpu).unwrap();
        assert_eq!((lock.Pitch, lock.pBits), (16, base + 128));
        assert!(
            matches!(
                super::super::readback::transfer_detached(
                    &mut env,
                    GuestPtr::new(device_pointer),
                    GuestPtr::new(source),
                    GuestPtr::new(surface)
                ),
                Outcome::Return(INVALID)
            ),
            "the borrowed destination sees its parent's level lock"
        );
        env.cpu.write_u32(lock.pBits, 0xff00ffff).unwrap();
        assert!(
            matches!(
                texture_unlock(&mut env, GuestPtr::new(texture), 1),
                Outcome::Return(0)
            ),
            "system-memory write unlock needs no GPU upload"
        );
        assert_eq!(env.cpu.read_u32(base + 128).unwrap(), 0xff00ffff);
        assert!(matches!(
            super::super::readback::transfer_detached(
                &mut env,
                GuestPtr::new(device_pointer),
                GuestPtr::new(source),
                GuestPtr::new(surface)
            ),
            Outcome::Block { .. }
        ));
        assert_eq!(env.ext.get::<State>().transfers.len(), 1);
        assert_eq!(release(&mut env, surface), 1);
        assert_eq!(release(&mut env, texture), 0);
        assert_eq!(com(&env).references(&env.sys.objects, texture_id), Some(0));
        assert!(
            com(&env)
                .get_identity(&env.sys.objects, surface_id)
                .is_some()
        );
        assert!(env.ext.get::<Heaps>().owner(env.cpu, base).is_some());
        super::super::readback::cancel_pending(&mut env);
        assert!(
            com(&env)
                .get_identity(&env.sys.objects, texture_id)
                .is_none()
        );
        assert!(
            com(&env)
                .get_identity(&env.sys.objects, surface_id)
                .is_none()
        );
        assert!(env.ext.get::<Heaps>().owner(env.cpu, base).is_none());
        release(&mut env, source);
    }
}
