//! Depth surfaces use the same Resource/Com owner as color surfaces. The device
//! image has no color allocation and is attached without any pixel copy.
use super::*;
use adv_abi::types::D3DSURFACE_DESC;
use adv_device::gpu::DepthFormat;
use resources::{Kind, Resource};
#[cfg(test)]
mod tests;

pub(super) fn format(value: u32) -> Option<DepthFormat> {
    match value {
        75 | 77 => Some(DepthFormat::Depth24Stencil8),
        80 => Some(DepthFormat::Unorm16),
        _ => None,
    }
}
pub(super) fn register(registry: &mut Registry) {
    for api in [create_ABI, with_private_releases!(set_ABI), get_ABI] {
        registry.register(api);
    }
}
fn make(
    env: &mut Env<'_>,
    device: Id,
    width: u32,
    height: u32,
    value: u32,
) -> Result<(u32, Id), u32> {
    let format = format(value).ok_or(UNAVAILABLE)?;
    if width == 0
        || height == 0
        || width
            .checked_mul(height)
            .is_none_or(|n| n > 16 * 1024 * 1024)
    {
        return Err(INVALID);
    }
    let gpu = env.ext.get::<State>().gpu.as_ref().ok_or(UNAVAILABLE)?;
    let depth = gpu
        .depth(width, height, format)
        .map_err(|error| match error {
            Error::Invalid(_) => INVALID,
            Error::Busy => OUT_OF_MEMORY,
            _ => DEVICE_LOST,
        })?;
    resources::make(
        env,
        Resource::new_depth(
            device,
            D3DSURFACE_DESC {
                Format: value,
                Type: 1,
                Usage: 2,
                Pool: 0,
                MultiSampleType: 0,
                MultiSampleQuality: 0,
                Width: width,
                Height: height,
            },
            depth,
        ),
    )
    .ok_or(OUT_OF_MEMORY)
}
pub(super) fn implicit(
    env: &mut Env<'_>,
    device: Id,
    p: D3DPRESENT_PARAMETERS,
) -> Result<Option<Id>, u32> {
    if p.EnableAutoDepthStencil == 0 {
        return Ok(None);
    }
    let (pointer, id) = make(
        env,
        device,
        p.BackBufferWidth,
        p.BackBufferHeight,
        p.AutoDepthStencilFormat,
    )?;
    if !com(env).retain_internal(&mut env.sys.objects, id) {
        release(env, pointer);
        return Err(OUT_OF_MEMORY);
    }
    release(env, pointer);
    Ok(Some(id))
}
pub(super) fn release_binding(env: &mut Env<'_>, binding: Option<Id>) {
    if let Some(id) = binding
        && let Some(Some(dead)) = com(env).release_internal(&mut env.sys.objects, id)
    {
        super::destroy(env, dead);
    }
}
/// Bind the original depth owner, including a larger D3D depth image. The
/// shared renderer clips commands to the logical color view without resizing Z.
pub(super) fn attach(env: &Env<'_>, this: u32, target: Target) -> Result<Target, u32> {
    let device = device(env, this).ok_or(INVALID)?;
    let Some(id) = device.depth_stencil else {
        return Ok(target);
    };
    let Some(Payload::Resource(r)) = com(env).get_identity(&env.sys.objects, id) else {
        return Err(INVALID);
    };
    target.with_depth(r.depth.clone()).map_err(|_| UNAVAILABLE)
}
pub(super) fn has_stencil(env: &Env<'_>, this: u32) -> bool {
    device(env, this)
        .and_then(|d| d.depth_stencil)
        .and_then(|id| com(env).get_identity(&env.sys.objects, id))
        .is_some_and(|p| matches!(p, Payload::Resource(r) if r.desc.Format == 75))
}

#[win32(d3d9,name="IDirect3DDevice9::CreateDepthStencilSurface",level=implement)]
fn create(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    width: u32,
    height: u32,
    format: u32,
    samples: i32,
    quality: u32,
    _discard: i32,
    output: GuestPtr<u32>,
    shared: GuestPtr<u32>,
) -> u32 {
    if output.write(env.cpu, 0).is_err() || device(env, this.addr()).is_none() {
        return INVALID;
    }
    if samples != 0 || quality != 0 || !shared.is_null() {
        return UNAVAILABLE;
    }
    let device = com(env)
        .identity::<Payload>(&env.sys.objects, this.addr())
        .unwrap();
    let (pointer, _) = match make(env, device, width, height, format) {
        Ok(value) => value,
        Err(error) => return error,
    };
    if output.write(env.cpu, pointer).is_err() {
        release(env, pointer);
        return INVALID;
    }
    0
}
#[win32(d3d9,name="IDirect3DDevice9::SetDepthStencilSurface",level=state)]
fn set(env: &mut Env<'_>, this: GuestPtr<()>, pointer: GuestPtr<()>) -> u32 {
    let Some(d) = device(env, this.addr()) else {
        return INVALID;
    };
    let old = d.depth_stencil;
    let owner = com(env)
        .identity::<Payload>(&env.sys.objects, this.addr())
        .unwrap();
    let new = if pointer.is_null() {
        None
    } else {
        let Some(r) = resources::resource(env, pointer.addr()) else {
            return INVALID;
        };
        if r.device != owner || r.kind != Kind::Surface || r.depth.is_none() {
            return INVALID;
        }
        com(env).identity::<Payload>(&env.sys.objects, pointer.addr())
    };
    if old == new {
        return 0;
    }
    if new.is_some_and(|id| !com(env).retain_internal(&mut env.sys.objects, id)) {
        return OUT_OF_MEMORY;
    }
    if let Some(Payload::Device(d)) = com(env).get_identity_mut(&mut env.sys.objects, owner) {
        d.depth_stencil = new;
    }
    release_binding(env, old);
    0
}
#[win32(d3d9,name="IDirect3DDevice9::GetDepthStencilSurface",level=implement)]
fn get(env: &mut Env<'_>, this: GuestPtr<()>, output: GuestPtr<u32>) -> u32 {
    if output.write(env.cpu, 0).is_err() {
        return INVALID;
    }
    let Some(d) = device(env, this.addr()) else {
        return INVALID;
    };
    let Some(id) = d.depth_stencil else {
        return 0x88760866;
    }; // D3DERR_NOTFOUND
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
