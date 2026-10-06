//! DirectDraw owns its ABI, window semantics and guest COM over the shared store.
//! The existing named-operation switch is a temporary behavior implementation;
//! registration never decodes an opaque descriptor or uses a hand-written arity.
#![allow(clippy::too_many_arguments)] // SDK COM signatures.
use super::*;
use adv_abi::{Env, GuestPtr, Handle, Outcome, Registry, win32};
use adv_cpu::backend::{Backend, Guest};
use adv_device::surface::{MemoryView, SurfaceStore};
use adv_machine::{Ext, Machine};
use adv_os::gdi::Gdi;
mod enumeration;
#[derive(Clone, Copy)]
pub struct WindowGeometry {
    pub width: u32,
    pub height: u32,
    pub origin: (i32, i32),
}
#[derive(Clone, Copy)]
pub struct Provider {
    pub memory: for<'a> fn(&'a mut (dyn Guest + 'a)) -> MemoryView<'a, dyn Guest + 'a>,
    pub window: fn(&Ext, &Objects, u32) -> Option<WindowGeometry>,
    pub completed: fn(&mut Env<'_>, u64) -> Option<Outcome>,
}
pub struct State {
    draw: DirectDraw,
    dcs: DeviceContexts,
    provider: Provider,
}
impl State {
    pub fn configure_clipper(&mut self, clipper: u32, origin: (i32, i32)) {
        self.draw.configure_clipper(clipper, origin);
    }
}

pub fn register(registry: &mut Registry) {
    enumeration::register(registry);
    for &(_, api) in BINDINGS {
        registry.register(api);
    }
    for api in [
        surface_queryinterface_ABI,
        surface_addref_ABI,
        surface_release_ABI,
        clipper_queryinterface_ABI,
        clipper_addref_ABI,
        clipper_release_ABI,
    ] {
        registry.register(api);
    }
}
pub fn install<B: Backend>(
    machine: &mut Machine<B>,
    gdi: Gdi,
    provider: Provider,
) -> Result<BTreeMap<u32, u32>, String> {
    let mut entries = BTreeMap::new();
    for (base, interface, methods) in [
        (400, "IDirectDraw7", DEVICE_METHODS),
        (500, "IDirectDrawSurface7", SURFACE_METHODS),
    ] {
        for (slot, method) in methods.iter().enumerate() {
            let entry = machine
                .sys
                .thunks
                .publish(
                    &mut machine.cpu,
                    "ddraw.dll",
                    &format!("{interface}::{method}"),
                )
                .map_err(|e| e.to_string())?;
            entries.insert(base + slot as u32, entry);
        }
    }
    for signature in adv_abi::signature::com_signatures()
        .iter()
        .filter(|s| s.module == "IDirectDrawClipper")
    {
        let slot = u32::from(
            signature
                .com_slot
                .expect("generated COM signature has a slot"),
        );
        let entry = machine
            .sys
            .thunks
            .publish(
                &mut machine.cpu,
                "ddraw.dll",
                &format!("IDirectDrawClipper::{}", signature.name),
            )
            .map_err(|e| e.to_string())?;
        entries.insert(600 + slot, entry);
    }
    for &(id, api) in BINDINGS {
        entries.insert(
            id,
            machine
                .sys
                .thunks
                .publish(&mut machine.cpu, api.module, api.name)
                .map_err(|e| e.to_string())?,
        );
    }
    machine.ext.insert(State {
        draw: DirectDraw::new(entries.clone()),
        dcs: DeviceContexts::new(gdi),
        provider,
    });
    Ok(entries)
}
fn call(env: &mut Env<'_>, id: u32, words: &[u32]) -> Outcome {
    let provider = env.ext.get::<State>().provider;
    let hwnd = match id {
        api::COOPERATIVE if words[2] == 8 => Some(words[1]),
        api::CLIPPER_SET_HWND => Some(words[2]),
        _ => None,
    };
    let geometry = hwnd.and_then(|window| (provider.window)(env.ext, &env.sys.objects, window));
    let (result, presented, changed) = {
        let (state, store) = env.ext.get2_installed_mut::<State, SurfaceStore>();
        let mut memory = (provider.memory)(env.cpu);
        if let Some(geometry) = geometry {
            if let Err(error) = store.configure_window(geometry.width, geometry.height, &mut memory)
            {
                return Outcome::Fail(format!("DirectDraw window: {error}"));
            }
            if id == api::CLIPPER_SET_HWND {
                state.draw.configure_clipper(words[0], geometry.origin);
            }
        }
        let before = store.stats.presentations;
        let result = state.draw.call(
            id,
            words,
            store,
            &mut state.dcs,
            &mut env.sys.objects,
            &mut memory,
        );
        (
            result,
            store.stats.presentations,
            store.stats.presentations != before,
        )
    };
    if changed
        && result.is_ok()
        && let Some(outcome) = (provider.completed)(env, presented)
    {
        return outcome;
    }
    match result {
        Ok(value) => Outcome::Return(value),
        Err(error) => Outcome::Fail(format!("DirectDraw: {error}")),
    }
}
// Static descriptors are verified by the ABI macro against generated SDK data.
pub const BINDINGS: &[(u32, adv_abi::Api)] = &[
    (api::CREATE_CLIPPER, create_clipper_ABI),
    (api::CLIPPER_SET_HWND, clipper_set_hwnd_ABI),
    (api::CLIPPER_GET_HWND, clipper_get_hwnd_ABI),
    (api::SET_CLIPPER, set_clipper_ABI),
    (api::CLIPPER_INITIALIZE, clipper_initialize_ABI),
    (api::DD_CREATE, dd_create_ABI),
    (api::QUERY, query_ABI),
    (api::ADDREF, addref_ABI),
    (api::RELEASE, release_ABI),
    (api::CREATE_SURFACE, create_surface_ABI),
    (api::COOPERATIVE, cooperative_ABI),
    (api::LOCK, lock_ABI),
    (api::UNLOCK, unlock_ABI),
    (api::BLT, blt_ABI),
    (api::GET_DESC, get_desc_ABI),
    (api::IS_LOST, is_lost_ABI),
    (api::DD_CREATE_OLD, dd_create_old_ABI),
    (api::DISPLAY_MODE, display_mode_ABI),
    (api::DISPLAY_MODE_OLD, display_mode_old_ABI),
    (api::RESTORE_MODE, restore_mode_ABI),
    (api::GET_DISPLAY_MODE, get_display_mode_ABI),
    (api::GET_CAPS, get_caps_ABI),
    (api::BLT_FAST, blt_fast_ABI),
    (api::FLIP, flip_ABI),
    (api::GET_ATTACHED, get_attached_ABI),
    (api::SURFACE_DC, surface_dc_ABI),
    (api::SURFACE_RELEASE_DC, surface_release_dc_ABI),
    (api::RESTORE, restore_ABI),
    (api::GET_PIXEL_FORMAT, get_pixel_format_ABI),
];
#[win32(ddraw, name = "IDirectDraw7::CreateClipper", level = implement)]
fn create_clipper(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    arg1: u32,
    arg2: GuestPtr<()>,
    arg3: GuestPtr<()>,
) -> Outcome {
    call(
        env,
        api::CREATE_CLIPPER,
        &[this.addr(), arg1, arg2.addr(), arg3.addr()],
    )
}
#[win32(ddraw, name = "IDirectDrawClipper::SetHWnd", level = implement)]
fn clipper_set_hwnd(env: &mut Env<'_>, this: GuestPtr<()>, arg1: u32, arg2: Handle) -> Outcome {
    call(env, api::CLIPPER_SET_HWND, &[this.addr(), arg1, arg2.0])
}
#[win32(ddraw, name = "IDirectDrawClipper::GetHWnd", level = implement)]
fn clipper_get_hwnd(env: &mut Env<'_>, this: GuestPtr<()>, arg1: GuestPtr<()>) -> Outcome {
    call(env, api::CLIPPER_GET_HWND, &[this.addr(), arg1.addr()])
}
#[win32(ddraw, name = "IDirectDrawSurface7::SetClipper", level = implement)]
fn set_clipper(env: &mut Env<'_>, this: GuestPtr<()>, arg1: GuestPtr<()>) -> Outcome {
    call(env, api::SET_CLIPPER, &[this.addr(), arg1.addr()])
}
#[win32(ddraw, name = "IDirectDrawClipper::Initialize", level = implement)]
fn clipper_initialize(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    arg1: GuestPtr<()>,
    arg2: u32,
) -> Outcome {
    call(
        env,
        api::CLIPPER_INITIALIZE,
        &[this.addr(), arg1.addr(), arg2],
    )
}
#[win32(ddraw, name = "DirectDrawCreateEx", level = implement)]
fn dd_create(
    env: &mut Env<'_>,
    arg0: GuestPtr<()>,
    arg1: GuestPtr<()>,
    arg2: GuestPtr<()>,
    arg3: GuestPtr<()>,
) -> Outcome {
    call(
        env,
        api::DD_CREATE,
        &[arg0.addr(), arg1.addr(), arg2.addr(), arg3.addr()],
    )
}
#[win32(ddraw, name = "IDirectDraw7::QueryInterface", level = implement)]
fn query(env: &mut Env<'_>, this: GuestPtr<()>, arg1: GuestPtr<()>, arg2: GuestPtr<()>) -> Outcome {
    call(env, api::QUERY, &[this.addr(), arg1.addr(), arg2.addr()])
}
#[win32(ddraw, name = "IDirectDraw7::AddRef", level = implement)]
fn addref(env: &mut Env<'_>, this: GuestPtr<()>) -> Outcome {
    call(env, api::ADDREF, &[this.addr()])
}
#[win32(ddraw, name = "IDirectDraw7::Release", level = implement)]
fn release(env: &mut Env<'_>, this: GuestPtr<()>) -> Outcome {
    call(env, api::RELEASE, &[this.addr()])
}
#[win32(ddraw, name = "IDirectDraw7::CreateSurface", level = implement)]
fn create_surface(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    arg1: GuestPtr<()>,
    arg2: GuestPtr<()>,
    arg3: GuestPtr<()>,
) -> Outcome {
    call(
        env,
        api::CREATE_SURFACE,
        &[this.addr(), arg1.addr(), arg2.addr(), arg3.addr()],
    )
}
#[win32(ddraw, name = "IDirectDraw7::SetCooperativeLevel", level = implement)]
fn cooperative(env: &mut Env<'_>, this: GuestPtr<()>, arg1: Handle, arg2: u32) -> Outcome {
    call(env, api::COOPERATIVE, &[this.addr(), arg1.0, arg2])
}
#[win32(ddraw, name = "IDirectDrawSurface7::Lock", level = implement)]
fn lock(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    arg1: GuestPtr<()>,
    arg2: GuestPtr<()>,
    arg3: u32,
    arg4: Handle,
) -> Outcome {
    call(
        env,
        api::LOCK,
        &[this.addr(), arg1.addr(), arg2.addr(), arg3, arg4.0],
    )
}
#[win32(ddraw, name = "IDirectDrawSurface7::Unlock", level = implement)]
fn unlock(env: &mut Env<'_>, this: GuestPtr<()>, arg1: GuestPtr<()>) -> Outcome {
    call(env, api::UNLOCK, &[this.addr(), arg1.addr()])
}
#[win32(ddraw, name = "IDirectDrawSurface7::Blt", level = implement)]
fn blt(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    arg1: GuestPtr<()>,
    arg2: GuestPtr<()>,
    arg3: GuestPtr<()>,
    arg4: u32,
    arg5: GuestPtr<()>,
) -> Outcome {
    call(
        env,
        api::BLT,
        &[
            this.addr(),
            arg1.addr(),
            arg2.addr(),
            arg3.addr(),
            arg4,
            arg5.addr(),
        ],
    )
}
#[win32(ddraw, name = "IDirectDrawSurface7::GetSurfaceDesc", level = implement)]
fn get_desc(env: &mut Env<'_>, this: GuestPtr<()>, arg1: GuestPtr<()>) -> Outcome {
    call(env, api::GET_DESC, &[this.addr(), arg1.addr()])
}
#[win32(ddraw, name = "IDirectDrawSurface7::IsLost", level = implement)]
fn is_lost(env: &mut Env<'_>, this: GuestPtr<()>) -> Outcome {
    call(env, api::IS_LOST, &[this.addr()])
}
#[win32(ddraw, name = "DirectDrawCreate", level = implement)]
fn dd_create_old(
    env: &mut Env<'_>,
    arg0: GuestPtr<()>,
    arg1: GuestPtr<()>,
    arg2: GuestPtr<()>,
) -> Outcome {
    call(
        env,
        api::DD_CREATE_OLD,
        &[arg0.addr(), arg1.addr(), arg2.addr()],
    )
}
#[win32(ddraw, name = "IDirectDraw7::SetDisplayMode", level = implement)]
fn display_mode(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    arg1: u32,
    arg2: u32,
    arg3: u32,
    arg4: u32,
    arg5: u32,
) -> Outcome {
    call(
        env,
        api::DISPLAY_MODE,
        &[this.addr(), arg1, arg2, arg3, arg4, arg5],
    )
}
#[win32(ddraw, name = "IDirectDraw::SetDisplayMode", level = implement)]
fn display_mode_old(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    arg1: u32,
    arg2: u32,
    arg3: u32,
) -> Outcome {
    call(env, api::DISPLAY_MODE_OLD, &[this.addr(), arg1, arg2, arg3])
}
#[win32(ddraw, name = "IDirectDraw7::RestoreDisplayMode", level = implement)]
fn restore_mode(env: &mut Env<'_>, this: GuestPtr<()>) -> Outcome {
    call(env, api::RESTORE_MODE, &[this.addr()])
}
#[win32(ddraw, name = "IDirectDraw7::GetDisplayMode", level = implement)]
fn get_display_mode(env: &mut Env<'_>, this: GuestPtr<()>, arg1: GuestPtr<()>) -> Outcome {
    call(env, api::GET_DISPLAY_MODE, &[this.addr(), arg1.addr()])
}
#[win32(ddraw, name = "IDirectDraw7::GetCaps", level = implement)]
fn get_caps(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    arg1: GuestPtr<()>,
    arg2: GuestPtr<()>,
) -> Outcome {
    call(env, api::GET_CAPS, &[this.addr(), arg1.addr(), arg2.addr()])
}
#[win32(ddraw, name = "IDirectDrawSurface7::BltFast", level = implement)]
fn blt_fast(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    arg1: u32,
    arg2: u32,
    arg3: GuestPtr<()>,
    arg4: GuestPtr<()>,
    arg5: u32,
) -> Outcome {
    call(
        env,
        api::BLT_FAST,
        &[this.addr(), arg1, arg2, arg3.addr(), arg4.addr(), arg5],
    )
}
#[win32(ddraw, name = "IDirectDrawSurface7::Flip", level = implement)]
fn flip(env: &mut Env<'_>, this: GuestPtr<()>, arg1: GuestPtr<()>, arg2: u32) -> Outcome {
    call(env, api::FLIP, &[this.addr(), arg1.addr(), arg2])
}
#[win32(ddraw, name = "IDirectDrawSurface7::GetAttachedSurface", level = implement)]
fn get_attached(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    arg1: GuestPtr<()>,
    arg2: GuestPtr<()>,
) -> Outcome {
    call(
        env,
        api::GET_ATTACHED,
        &[this.addr(), arg1.addr(), arg2.addr()],
    )
}
#[win32(ddraw, name = "IDirectDrawSurface7::GetDC", level = implement)]
fn surface_dc(env: &mut Env<'_>, this: GuestPtr<()>, arg1: GuestPtr<()>) -> Outcome {
    call(env, api::SURFACE_DC, &[this.addr(), arg1.addr()])
}
#[win32(ddraw, name = "IDirectDrawSurface7::ReleaseDC", level = implement)]
fn surface_release_dc(env: &mut Env<'_>, this: GuestPtr<()>, arg1: Handle) -> Outcome {
    call(env, api::SURFACE_RELEASE_DC, &[this.addr(), arg1.0])
}
#[win32(ddraw, name = "IDirectDrawSurface7::Restore", level = implement)]
fn restore(env: &mut Env<'_>, this: GuestPtr<()>) -> Outcome {
    call(env, api::RESTORE, &[this.addr()])
}
#[win32(ddraw, name = "IDirectDrawSurface7::GetPixelFormat", level = implement)]
fn get_pixel_format(env: &mut Env<'_>, this: GuestPtr<()>, arg1: GuestPtr<()>) -> Outcome {
    call(env, api::GET_PIXEL_FORMAT, &[this.addr(), arg1.addr()])
}
#[win32(ddraw, name = "IDirectDrawSurface7::QueryInterface", level = implement)]
fn surface_queryinterface(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    iid: GuestPtr<()>,
    output: GuestPtr<()>,
) -> Outcome {
    call(env, api::QUERY, &[this.addr(), iid.addr(), output.addr()])
}
#[win32(ddraw, name = "IDirectDrawSurface7::AddRef", level = implement)]
fn surface_addref(env: &mut Env<'_>, this: GuestPtr<()>) -> Outcome {
    call(env, api::ADDREF, &[this.addr()])
}
#[win32(ddraw, name = "IDirectDrawSurface7::Release", level = implement)]
fn surface_release(env: &mut Env<'_>, this: GuestPtr<()>) -> Outcome {
    call(env, api::RELEASE, &[this.addr()])
}
#[win32(ddraw, name = "IDirectDrawClipper::QueryInterface", level = implement)]
fn clipper_queryinterface(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    iid: GuestPtr<()>,
    output: GuestPtr<()>,
) -> Outcome {
    call(env, api::QUERY, &[this.addr(), iid.addr(), output.addr()])
}
#[win32(ddraw, name = "IDirectDrawClipper::AddRef", level = implement)]
fn clipper_addref(env: &mut Env<'_>, this: GuestPtr<()>) -> Outcome {
    call(env, api::ADDREF, &[this.addr()])
}
#[win32(ddraw, name = "IDirectDrawClipper::Release", level = implement)]
fn clipper_release(env: &mut Env<'_>, this: GuestPtr<()>) -> Outcome {
    call(env, api::RELEASE, &[this.addr()])
}
