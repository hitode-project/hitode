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
    pub lock: Option<Lock>,
    /// Texture levels borrow the parent's storage and delegate public counts.
    pub container: Option<Id>,
    /// The texture owns this interface metadata through an internal Objects ref.
    pub level_surface: Option<Id>,
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
            lock: None,
            container: None,
            level_surface: None,
        }
    }
}
#[derive(Clone, Copy)]
pub(super) struct Lock {
    pub rectangle: Rect,
    pub flags: u32,
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
        set_texture_ABI,
        get_texture_ABI,
        texture_levels_ABI,
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
    let Some(r) = resource(env, this.addr()).filter(|r| r.kind == Kind::Texture && level == 0)
    else {
        return INVALID;
    };
    let container = com(env)
        .identity::<Payload>(&env.sys.objects, this.addr())
        .unwrap();
    let level_id = if let Some(id) = r.level_surface {
        id
    } else {
        // Only interface metadata is new. Storage and lock state remain owned
        // by the texture; the level neither copies pixels nor owns a parent ref.
        let value = Resource {
            pointer: 0,
            device: r.device,
            kind: Kind::Surface,
            desc: r.desc,
            depth: None,
            surface: r.surface,
            storage: 0,
            owns_surface: false,
            lock: None,
            container: Some(container),
            level_surface: None,
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
            r.level_surface = Some(id);
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
    if let Some(level) = r.level_surface
        && let Some(Some(dead)) = com(env).release_internal(&mut env.sys.objects, level)
    {
        super::destroy(env, dead);
    }
    if r.storage != 0 {
        free(env, r.storage);
    }
    if r.owns_surface
        && let Some(surface) = r.surface
    {
        let provider = env.ext.get::<State>().provider;
        let _ = (provider.remove)(env.ext, env.cpu, surface);
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
    let cleanup = (r.storage, r.owns_surface, r.surface);
    r.pointer = 0;
    let Some((pointer, id)) = object(env, interface, iid, Payload::Resource(r)) else {
        if cleanup.0 != 0 {
            free(env, cleanup.0);
        }
        if cleanup.1
            && let Some(surface) = cleanup.2
        {
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
            lock: None,
            container: None,
            level_surface: None,
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
fn back_buffer(
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
fn create_texture(
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
    if !shared.is_null()
        || levels != 1
        || !matches!(format, 21 | 22)
        || !(0..=3).contains(&pool)
        || usage & !(1 | 0x200) != 0
        || (pool != 0 && usage != 0)
        || (usage & 1 != 0 && usage & 0x200 != 0)
    {
        return Outcome::Return(UNAVAILABLE);
    }
    let Some(len) = width
        .checked_mul(height)
        .and_then(|n| n.checked_mul(4))
        .filter(|&n| n > 0 && n <= 64 * 1024 * 1024)
    else {
        return Outcome::Return(INVALID);
    };
    let target = match env.ext.get::<State>().gpu.as_ref() {
        Some(g) => g.target(width, height, false),
        None => return Outcome::Return(UNAVAILABLE),
    };
    let target = match target {
        Ok(t) => t,
        Err(e) => return gpu_error(e),
    };
    let provider = env.ext.get::<State>().provider;
    let surface =
        match (provider.insert)(env.ext, target.with_alpha_mode(targets::alpha_mode(format))) {
            Ok(s) => s,
            Err(_) => return Outcome::Return(OUT_OF_MEMORY),
        };
    let storage = if usage & 1 == 0 {
        alloc(env, len)
    } else {
        Some(0)
    };
    let Some(storage) = storage else {
        let _ = (provider.remove)(env.ext, env.cpu, surface);
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
            desc: D3DSURFACE_DESC {
                Format: format,
                Type: 1,
                Usage: usage,
                Pool: pool,
                MultiSampleType: 0,
                MultiSampleQuality: 0,
                Width: width,
                Height: height,
            },
            depth: None,
            surface: Some(surface),
            storage,
            owns_surface: true,
            lock: None,
            container: None,
            level_surface: None,
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
    let Some(r) = resource(env, this.addr()).filter(|r| r.kind == Kind::Texture && level == 0)
    else {
        return INVALID;
    };
    let desc = r.desc;
    if output.write(env.cpu, desc).is_ok() {
        0
    } else {
        INVALID
    }
}
#[win32(d3d9,name="IDirect3DTexture9::GetLevelCount",level=implement)]
fn texture_levels(env: &mut Env<'_>, this: GuestPtr<()>) -> u32 {
    u32::from(resource(env, this.addr()).is_some_and(|r| r.kind == Kind::Texture))
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
    let Some(r) = resource(env, this.addr()).filter(|r| r.kind == Kind::Texture && level == 0)
    else {
        return INVALID;
    };
    if r.lock.is_some()
        || r.storage == 0
        || r.desc.Usage & 1 != 0
        || (r.desc.Pool == 0 && r.desc.Usage & 0x200 == 0)
        || flags & !(0x10 | 0x2000 | 0x8000) != 0
        || (flags & 0x2000 != 0
            && (r.desc.Usage & 0x200 == 0 || !rectangle.is_null() || flags & 0x10 != 0))
    {
        return INVALID;
    }
    let rect = if rectangle.is_null() {
        Rect {
            x: 0,
            y: 0,
            w: r.desc.Width,
            h: r.desc.Height,
        }
    } else {
        let Ok(rect) = rectangle.read(env.cpu) else {
            return INVALID;
        };
        if rect.left < 0
            || rect.top < 0
            || rect.right <= rect.left
            || rect.bottom <= rect.top
            || rect.right as u32 > r.desc.Width
            || rect.bottom as u32 > r.desc.Height
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
    let pitch = r.desc.Width * 4;
    let bits = r.storage + rect.y * pitch + rect.x * 4;
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
        r.lock = Some(Lock {
            rectangle: rect,
            flags,
        });
    }
    0
}
#[win32(d3d9,name="IDirect3DTexture9::UnlockRect",level=implement)]
pub(super) fn texture_unlock(env: &mut Env<'_>, this: GuestPtr<()>, level: u32) -> Outcome {
    let Some(r) = resource(env, this.addr()).filter(|r| r.kind == Kind::Texture && level == 0)
    else {
        return Outcome::Return(INVALID);
    };
    let Some(lock) = r.lock else {
        return Outcome::Return(INVALID);
    };
    if lock.flags & 0x10 != 0 {
        if let Some(Payload::Resource(r)) =
            com(env).get_mut::<Payload>(&mut env.sys.objects, this.addr())
        {
            r.lock = None;
        }
        return Outcome::Return(0);
    }
    let provider = env.ext.get::<State>().provider;
    let Some(target) = r.surface.and_then(|s| (provider.target)(env.ext, s)) else {
        return Outcome::Return(INVALID);
    };
    let pitch = r.desc.Width * 4;
    let size = (lock.rectangle.h - 1) * pitch + lock.rectangle.w * 4;
    let address = r.storage + lock.rectangle.y * pitch + lock.rectangle.x * 4;
    let format = r.desc.Format;
    let mut pixels = vec![0; size as usize];
    if env.cpu.read(address, &mut pixels).is_err() {
        return Outcome::Return(INVALID);
    }
    if format == 22 {
        for y in 0..lock.rectangle.h as usize {
            for x in 0..lock.rectangle.w as usize {
                pixels[y * pitch as usize + x * 4 + 3] = 255;
            }
        }
    }
    let id = com(env)
        .identity::<Payload>(&env.sys.objects, this.addr())
        .unwrap();
    dispatch(
        env,
        id,
        Operation::Upload {
            resource: id,
            target,
            pixels,
            pitch,
            rectangle: lock.rectangle,
        },
    )
}
#[win32(d3d9,name="IDirect3DDevice9::SetTexture",level=state)]
fn set_texture(env: &mut Env<'_>, this: GuestPtr<()>, stage: u32, texture: GuestPtr<()>) -> u32 {
    let Some(d) = device(env, this.addr()) else {
        return INVALID;
    };
    if stage != 0 {
        return INVALID;
    }
    let old = d.texture;
    let device_id = com(env)
        .identity::<Payload>(&env.sys.objects, this.addr())
        .unwrap();
    let new = if texture.is_null() {
        None
    } else {
        let Some(r) = resource(env, texture.addr()) else {
            return INVALID;
        };
        if r.kind != Kind::Texture || r.device != device_id || r.desc.Pool > 1 {
            return INVALID;
        }
        Some(
            com(env)
                .identity::<Payload>(&env.sys.objects, texture.addr())
                .unwrap(),
        )
    };
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
        d.texture = new;
    }
    if let Some(id) = old
        && let Some(Some(dead)) = com(env).release_internal(&mut env.sys.objects, id)
    {
        super::destroy(env, dead);
    }
    0
}
#[win32(d3d9,name="IDirect3DDevice9::GetTexture",level=implement)]
fn get_texture(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    stage: u32,
    output: GuestPtr<u32>,
) -> Outcome {
    if output.write(env.cpu, 0).is_err() {
        return Outcome::Return(INVALID);
    }
    let Some(d) = device(env, this.addr()) else {
        return Outcome::Return(INVALID);
    };
    if stage != 0 {
        return Outcome::Return(INVALID);
    }
    let Some(id) = d.texture else {
        return Outcome::Return(0);
    };
    let Some(Payload::Resource(r)) = com(env).get_identity(&env.sys.objects, id) else {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_reference_limit_and_invalid_output_preserve_parent_and_single_backing() {
        let mut m = super::super::tests::machine();
        m.ext
            .get_mut::<State>()
            .tables
            .insert("IDirect3DTexture9", 0);
        let mut env = Env {
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
                lock: None,
                container: None,
                level_surface: None,
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
}
