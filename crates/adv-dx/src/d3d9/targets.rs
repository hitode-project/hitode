//! Render-target bindings borrow identities in the common COM and surface stores.
use super::*;
use adv_abi::types::D3DSURFACE_DESC;
use resources::{Kind, Resource};

pub(super) fn alpha_mode(format: u32) -> adv_device::gpu::AlphaMode {
    if format == 22 {
        adv_device::gpu::AlphaMode::Opaque
    } else {
        adv_device::gpu::AlphaMode::Stored
    }
}

pub(super) fn register(registry: &mut Registry) {
    for api in [create_ABI, set_ABI, get_ABI] {
        registry.register(api);
    }
}

fn current(env: &Env<'_>, this: u32) -> Result<Id, u32> {
    let d = device(env, this).ok_or(INVALID)?;
    d.render_target.or(d.back_buffer).ok_or(INVALID)
}

pub(super) fn draw_target(env: &Env<'_>, this: u32) -> Result<Target, u32> {
    let id = current(env, this)?;
    let Some(Payload::Resource(r)) = com(env).get_identity(&env.sys.objects, id) else {
        return Err(INVALID);
    };
    // Authored native D3D9 permits Clear/draw while a lockable back buffer is
    // locked. Other default-pool resource locks still prohibit GPU drawing.
    if dc::leased(env, r)
        || (resources::locked(env, r) && !readback::lockable_back_buffer(env, r.pointer, r))
    {
        return Err(INVALID);
    }
    let provider = env.ext.get::<State>().provider;
    r.surface
        .and_then(|s| (provider.target)(env.ext, s))
        .ok_or(INVALID)
}

pub(super) fn release_binding(env: &mut Env<'_>, binding: Option<Id>) {
    if let Some(id) = binding {
        let owner = resources::binding_owner(env, id);
        if let Some(Some(dead)) = com(env).release_internal(&mut env.sys.objects, owner) {
            super::destroy(env, dead);
        }
    }
}

pub(super) fn reset_binding(env: &mut Env<'_>, device: Id) {
    let old = match com(env).get_identity_mut(&mut env.sys.objects, device) {
        Some(Payload::Device(d)) => d.render_target.take(),
        _ => None,
    };
    release_binding(env, old);
}

#[win32(d3d9,name="IDirect3DDevice9::CreateRenderTarget",level=implement)]
fn create(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    width: u32,
    height: u32,
    format: u32,
    samples: i32,
    quality: u32,
    lockable: i32,
    output: GuestPtr<u32>,
    shared: GuestPtr<u32>,
) -> Outcome {
    if output.write(env.cpu, 0).is_err() || device(env, this.addr()).is_none() {
        return Outcome::Return(INVALID);
    }
    if width == 0
        || height == 0
        || width
            .checked_mul(height)
            .is_none_or(|n| n > 16 * 1024 * 1024)
    {
        return Outcome::Return(INVALID);
    }
    if !matches!(format, 21 | 22) || samples != 0 || quality != 0 || !shared.is_null() {
        return Outcome::Return(UNAVAILABLE);
    }
    let target = match env.ext.get::<State>().gpu.as_ref() {
        Some(gpu) => match gpu.target(width, height, false) {
            Ok(target) => target,
            Err(error) => return gpu_error(error),
        },
        None => return Outcome::Return(UNAVAILABLE),
    };
    let provider = env.ext.get::<State>().provider;
    let surface = match (provider.insert)(env.ext, target.with_alpha_mode(alpha_mode(format))) {
        Ok(surface) => surface,
        Err(_) => return Outcome::Return(OUT_OF_MEMORY),
    };
    let owner = com(env)
        .identity::<Payload>(&env.sys.objects, this.addr())
        .unwrap();
    let Some((pointer, _)) = resources::make(
        env,
        Resource {
            pointer: 0,
            device: owner,
            kind: Kind::Surface,
            desc: D3DSURFACE_DESC {
                Format: format,
                Type: 1,
                Usage: 1,
                Pool: 0,
                MultiSampleType: 0,
                MultiSampleQuality: 0,
                Width: width,
                Height: height,
            },
            depth: None,
            surface: Some(surface),
            storage: 0,
            owns_surface: true,
            lockable: lockable != 0,
            lock: None,
            container: None,
            levels: Vec::new(),
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

#[win32(d3d9,name="IDirect3DDevice9::SetRenderTarget",level=state)]
fn set(env: &mut Env<'_>, this: GuestPtr<()>, index: u32, surface: GuestPtr<()>) -> u32 {
    let Some(d) = device(env, this.addr()) else {
        return INVALID;
    };
    if index != 0 || surface.is_null() {
        return INVALID;
    }
    let old = d.render_target;
    let device_id = com(env)
        .identity::<Payload>(&env.sys.objects, this.addr())
        .unwrap();
    let Some(r) = resources::resource(env, surface.addr()) else {
        return INVALID;
    };
    if r.kind != Kind::Surface || r.device != device_id || r.desc.Usage & 1 == 0 || r.desc.Pool != 0
    {
        return INVALID;
    }
    let (width, height) = (r.desc.Width, r.desc.Height);
    let new = com(env)
        .identity::<Payload>(&env.sys.objects, surface.addr())
        .unwrap();
    if old != Some(new) {
        let owner = resources::binding_owner(env, new);
        if !com(env).retain_internal(&mut env.sys.objects, owner) {
            return OUT_OF_MEMORY;
        }
    }
    if let Some(Payload::Device(d)) = com(env).get_identity_mut(&mut env.sys.objects, device_id) {
        d.render_target = Some(new);
        d.versions[3] += 1;
        d.versions[4] += 1;
        d.fixed.target_changed(width, height);
    }
    if old != Some(new) {
        release_binding(env, old);
    }
    0
}

#[win32(d3d9,name="IDirect3DDevice9::GetRenderTarget",level=implement)]
fn get(env: &mut Env<'_>, this: GuestPtr<()>, index: u32, output: GuestPtr<u32>) -> u32 {
    if output.write(env.cpu, 0).is_err() || index != 0 {
        return INVALID;
    }
    let id = match current(env, this.addr()) {
        Ok(id) => id,
        Err(error) => return error,
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
