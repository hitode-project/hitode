//! DirectDraw owns its ABI, window semantics and guest COM over the shared store.
//! The existing named-operation switch is a temporary behavior implementation;
//! registration never decodes an opaque descriptor or uses a hand-written arity.
#![allow(clippy::too_many_arguments)] // SDK COM signatures.
use super::*;
use crate::argument::CallError;
use crate::interop::{DcSurface, MemoryDc};
use adv_abi::{Env, GuestPtr, Handle, Outcome, Registry, win32};
use adv_cpu::backend::{Backend, Guest, Prot};
use adv_device::surface::{MemoryView, SurfaceStore};
use adv_machine::{Ext, Machine};
mod enumeration;
mod extras;
pub(crate) mod modes;
#[derive(Clone, Copy)]
pub struct WindowGeometry {
    pub hwnd: u32,
    pub width: u32,
    pub height: u32,
    pub origin: (i32, i32),
}
#[derive(Clone, Copy)]
pub struct Provider {
    pub dc: MemoryDc,
    pub memory: for<'a> fn(&'a mut (dyn Guest + 'a)) -> MemoryView<'a, dyn Guest + 'a>,
    /// Resolve a window; zero selects the current presented game window.
    pub window: fn(&Ext, &Objects, u32) -> Option<WindowGeometry>,
    pub submitted: fn(&Ext, u32, u32),
    pub completed: fn(&mut Env<'_>, u64) -> Option<Outcome>,
    /// The DirectDraw object is gone: its windows present their own surfaces.
    pub released: fn(&Ext, u32),
    pub vsync: fn(&Ext) -> Option<(std::time::Duration, std::time::Duration)>,
}
pub struct State {
    draw: DirectDraw,
    provider: Provider,
    binding: Option<(u32, u32)>,
    presented: Option<(u32, u32)>,
}
impl State {
    pub fn configure_clipper(&mut self, clipper: u32, origin: (i32, i32)) {
        self.draw.configure_clipper(clipper, origin);
    }

    fn submitted(ext: &mut Ext, surface: u32) {
        let state = ext.get_mut::<Self>();
        state.presented = state.binding.map(|(device, _)| (device, surface));
        let hwnd = state
            .draw
            .surface_clippers
            .get(&surface)
            .and_then(|clipper| state.draw.clipper_windows.get(clipper))
            .copied()
            .filter(|&hwnd| hwnd != 0)
            .or(state.binding.map(|(_, hwnd)| hwnd))
            .unwrap_or(0);
        (state.provider.submitted)(ext, hwnd, surface);
    }

    fn device(&self, objects: &Objects, pointer: u32) -> Option<u32> {
        let object = self.draw.com?.get::<ComState>(objects, pointer)?;
        matches!(object.kind, Kind::Device).then_some(object.modern)
    }
}

pub fn register(registry: &mut Registry) {
    enumeration::register(registry);
    extras::register(registry);
    registry.register(get_device_identifier_ABI);
    registry.register(get_fourcc_codes_ABI);
    registry.register(wait_for_vertical_blank_ABI);
    registry.register(get_scan_line_ABI);
    registry.register(get_vertical_blank_status_ABI);
    registry.register(get_monitor_frequency_ABI);
    registry.register(get_available_vid_mem_ABI);
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
    crate::refcounts::install(registry, "ddraw.dll");
}
pub fn install<B: Backend>(
    machine: &mut Machine<B>,
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
        provider,
        binding: None,
        presented: None,
    });
    Ok(entries)
}
/// CLSID_DirectDraw and CLSID_DirectDraw7, which assembly binds to [`activate`].
pub const CLASSES: [[u8; 16]; 2] = [
    [
        0xe0, 0x0e, 0xb7, 0xd7, 0x40, 0x43, 0xcf, 0x11, 0xb0, 0x63, 0, 0x20, 0xaf, 0xc2, 0xcd, 0x35,
    ],
    [
        0x96, 0x51, 0x30, 0x3c, 0xdb, 0x50, 0xd3, 0x11, 0x9c, 0xfe, 0, 0xc0, 0x4f, 0xd9, 0x30, 0xc5,
    ],
];
pub fn activate(env: &mut Env<'_>, iid: [u8; 16], outer: u32, output: u32) -> Outcome {
    let provider = env.ext.get::<State>().provider;
    let result = {
        let (state, store) = env.ext.get2_installed_mut::<State, SurfaceStore>();
        let mut memory = (provider.memory)(env.cpu);
        state
            .draw
            .activate(&mut env.sys.objects, store, iid, outer, output, &mut memory)
    };
    publish_counters(env);
    match result {
        Ok(hr) => Outcome::Return(hr),
        Err(error) => error.outcome(env),
    }
}
fn publish_counters(env: &mut Env<'_>) {
    let draw = &mut env.ext.get_mut::<State>().draw;
    let (com, pending) = (draw.com, std::mem::take(&mut draw.pending_guest));
    if let Some(com) = com {
        for id in pending {
            com.enable_guest(env, id);
        }
    }
}
/// Map more guest address space into the store before a surface it cannot
/// place: as on Windows, system-memory surfaces are bounded by the address
/// space, not by the store's first mapping. A primary may bring a back buffer.
fn reserve_surface(env: &mut Env<'_>, desc: u32) -> Result<(), u32> {
    const OUT_OF_MEMORY: u32 = 0x8007000e;
    let provider = env.ext.get::<State>().provider;
    let read = |env: &mut Env<'_>, offset| (provider.memory)(env.cpu).u32(desc + offset).ok();
    let (caps, width, height) = (read(env, 104), read(env, 12), read(env, 8));
    let store = env.ext.get::<SurfaceStore>();
    let pixels = match (caps, width, height) {
        (Some(caps), _, _) if caps & 0x200 != 0 => {
            2 * surface::size(store.width, store.height).unwrap_or(0)
        }
        (Some(_), Some(width), Some(height)) => surface::size(width, height).unwrap_or(0),
        _ => 0,
    };
    // Front objects for the new interfaces come from the same store.
    let Some(growth) = store
        .arena_growth(pixels + 4096)
        .filter(|&growth| growth != 0)
    else {
        return Ok(());
    };
    let address = env
        .sys
        .vm
        .alloc(env.cpu, growth, Prot::RW)
        .map_err(|_| OUT_OF_MEMORY)?;
    let length = env.sys.vm.region(address).unwrap();
    let store = env.ext.get_mut::<SurfaceStore>();
    if store.extend_arena(address, length).is_err() {
        let _ = env.sys.vm.free(env.cpu, address);
        return Err(OUT_OF_MEMORY);
    }
    log::info!(
        "DirectDraw surface storage grew by {length} bytes: {:?}",
        store.arena_usage()
    );
    Ok(())
}
fn call(env: &mut Env<'_>, id: u32, words: &[u32]) -> Outcome {
    let provider = env.ext.get::<State>().provider;
    if id == api::COOPERATIVE
        && words[2] == 0x80
        && (provider.window)(env.ext, &env.sys.objects, words[1]).is_none()
    {
        return Outcome::Return(INVALID);
    }
    if id == api::CREATE_SURFACE
        && let Err(hr) = reserve_surface(env, words[1])
    {
        return Outcome::Return(hr);
    }
    let binding = match id {
        api::COOPERATIVE if words[2] == 8 => Some((words[1], None)),
        api::CLIPPER_SET_HWND if words[2] != 0 => Some((words[2], Some(words[0]))),
        api::BLT => {
            let state = env.ext.get::<State>();
            let primary = env.ext.get::<SurfaceStore>().primary_id();
            state
                .draw
                .surface_id(&env.sys.objects, words[0])
                .filter(|&sid| Some(sid) == primary)
                .and_then(|sid| {
                    if let Some(&clipper) = state.draw.surface_clippers.get(&sid) {
                        return state
                            .draw
                            .clipper_windows
                            .get(&clipper)
                            .filter(|&&hwnd| hwnd != 0)
                            .map(|&hwnd| (hwnd, Some(clipper)));
                    }
                    // A window-backed primary still uses screen coordinates
                    // when the application has not attached a clipper.
                    let owner = state.draw.primary_owner?;
                    let device = state.draw.com?.get::<ComState>(&env.sys.objects, owner)?;
                    device.windowed.then_some((device.focus_window, None))
                })
        }
        _ => None,
    };
    let geometry = binding.and_then(|(window, clipper)| {
        (provider.window)(env.ext, &env.sys.objects, window).map(|geometry| (geometry, clipper))
    });
    let (result, presented, changed, submitted) = {
        let (state, store) = env.ext.get2_installed_mut::<State, SurfaceStore>();
        let mut memory = (provider.memory)(env.cpu);
        // Refresh for each operation; never retain an origin across movement,
        // a mode change, or a different primary owner.
        state.draw.primary_origin = if id == api::BLT {
            geometry.map(|(geometry, _)| geometry.origin)
        } else {
            None
        };
        if let Some((geometry, clipper)) = geometry {
            if let Err(error) = store.configure_window(geometry.width, geometry.height, &mut memory)
            {
                return Outcome::Fail(format!("DirectDraw window: {error}"));
            }
            if let Some(clipper) = clipper {
                state.draw.configure_clipper(clipper, geometry.origin);
            }
        }
        state.draw.context = env.context;
        let before = store.stats.presentations;
        let result = state
            .draw
            .call(id, words, store, &mut env.sys.objects, &mut memory);
        if matches!(result, Ok(0))
            && state.draw.submitted.is_some()
            && let Some((geometry, _)) = geometry
        {
            state.binding = state.draw.primary_owner.map(|owner| (owner, geometry.hwnd));
        }
        if id == api::COOPERATIVE && matches!(result, Ok(0)) {
            state.binding = state
                .device(&env.sys.objects, words[0])
                .map(|device| (device, words[1]));
        }
        (
            result,
            store.stats.presentations,
            store.stats.presentations != before,
            state.draw.submitted.take(),
        )
    };
    if let Some(surface) = submitted {
        State::submitted(env.ext, surface);
    }
    publish_counters(env);
    if changed
        && result.is_ok()
        && let Some(outcome) = (provider.completed)(env, presented)
    {
        return outcome;
    }
    match result {
        Ok(value) => Outcome::Return(value),
        Err(error) => error.outcome(env),
    }
}
// Static descriptors are verified by the ABI macro against generated SDK data.
pub const BINDINGS: &[(u32, adv_abi::Api)] = &[
    (api::CREATE_CLIPPER, create_clipper_ABI),
    (api::CLIPPER_SET_HWND, clipper_set_hwnd_ABI),
    (api::CLIPPER_GET_HWND, clipper_get_hwnd_ABI),
    (api::GET_CLIPPER, get_clipper_ABI),
    (api::GET_BLT_STATUS, get_blt_status_ABI),
    (api::GET_FLIP_STATUS, get_flip_status_ABI),
    (api::SET_CLIPPER, set_clipper_ABI),
    (api::CLIPPER_INITIALIZE, clipper_initialize_ABI),
    (api::CLIPPER_SET_CLIP_LIST, clipper_set_clip_list_ABI),
    (api::DD_CREATE, dd_create_ABI),
    (api::INITIALIZE, initialize_ABI),
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
    (api::FLIP_TO_GDI, flip_to_gdi_ABI),
    (api::GET_DISPLAY_MODE, get_display_mode_ABI),
    (api::ENUM_MODES, modes::enum_modes_ABI),
    (api::GET_CAPS, get_caps_ABI),
    (api::BLT_FAST, blt_fast_ABI),
    (api::FLIP, flip_ABI),
    (api::GET_ATTACHED, get_attached_ABI),
    (api::SURFACE_DC, surface_dc_ABI),
    (api::SURFACE_RELEASE_DC, surface_release_dc_ABI),
    (api::RESTORE, restore_ABI),
    (api::GET_PIXEL_FORMAT, get_pixel_format_ABI),
    (api::SET_COLOR_KEY, set_color_key_ABI),
    (api::GET_COLOR_KEY, get_color_key_ABI),
    (api::COMPACT, compact_ABI),
    (api::RESTORE_ALL_SURFACES, restore_all_surfaces_ABI),
    (api::TEST_COOPERATIVE_LEVEL, test_cooperative_level_ABI),
    (api::DD_CREATE_CLIPPER, dd_create_clipper_ABI),
    (api::GET_GDI_SURFACE, get_gdi_surface_ABI),
    (api::GET_DD_INTERFACE, get_dd_interface_ABI),
    (api::PAGE_LOCK, page_lock_ABI),
    (api::PAGE_UNLOCK, page_unlock_ABI),
    (api::GET_UNIQUENESS, get_uniqueness_ABI),
    (api::CHANGE_UNIQUENESS, change_uniqueness_ABI),
    (api::SURFACE_CAPS, surface_caps_ABI),
    (api::SURFACE_INITIALIZE, surface_initialize_ABI),
];
#[win32(ddraw, name = "DirectDrawCreateClipper", level = implement)]
fn dd_create_clipper(
    env: &mut Env<'_>,
    flags: u32,
    clipper: GuestPtr<()>,
    outer: GuestPtr<()>,
) -> Outcome {
    call(
        env,
        api::DD_CREATE_CLIPPER,
        &[flags, clipper.addr(), outer.addr()],
    )
}
#[win32(ddraw, name = "IDirectDraw7::GetGDISurface", level = implement)]
fn get_gdi_surface(env: &mut Env<'_>, this: GuestPtr<()>, surface: GuestPtr<()>) -> Outcome {
    call(env, api::GET_GDI_SURFACE, &[this.addr(), surface.addr()])
}
#[win32(ddraw, name = "IDirectDrawSurface7::GetDDInterface", level = implement)]
fn get_dd_interface(env: &mut Env<'_>, this: GuestPtr<()>, dd: GuestPtr<()>) -> Outcome {
    call(env, api::GET_DD_INTERFACE, &[this.addr(), dd.addr()])
}
#[win32(ddraw, name = "IDirectDrawSurface7::PageLock", level = implement)]
fn page_lock(env: &mut Env<'_>, this: GuestPtr<()>, flags: u32) -> Outcome {
    call(env, api::PAGE_LOCK, &[this.addr(), flags])
}
#[win32(ddraw, name = "IDirectDrawSurface7::PageUnlock", level = implement)]
fn page_unlock(env: &mut Env<'_>, this: GuestPtr<()>, flags: u32) -> Outcome {
    call(env, api::PAGE_UNLOCK, &[this.addr(), flags])
}
#[win32(ddraw, name = "IDirectDrawSurface7::GetUniquenessValue", level = implement)]
fn get_uniqueness(env: &mut Env<'_>, this: GuestPtr<()>, value: GuestPtr<()>) -> Outcome {
    call(env, api::GET_UNIQUENESS, &[this.addr(), value.addr()])
}
#[win32(ddraw, name = "IDirectDrawSurface7::ChangeUniquenessValue", level = implement)]
fn change_uniqueness(env: &mut Env<'_>, this: GuestPtr<()>) -> Outcome {
    call(env, api::CHANGE_UNIQUENESS, &[this.addr()])
}
#[win32(ddraw, name = "IDirectDrawSurface7::GetCaps", level = implement)]
fn surface_caps(env: &mut Env<'_>, this: GuestPtr<()>, caps: GuestPtr<()>) -> Outcome {
    call(env, api::SURFACE_CAPS, &[this.addr(), caps.addr()])
}
#[win32(ddraw, name = "IDirectDrawSurface7::Initialize", level = implement)]
fn surface_initialize(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    dd: GuestPtr<()>,
    desc: GuestPtr<()>,
) -> Outcome {
    call(
        env,
        api::SURFACE_INITIALIZE,
        &[this.addr(), dd.addr(), desc.addr()],
    )
}
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
#[win32(ddraw, name = "IDirectDrawClipper::SetClipList", level = implement)]
fn clipper_set_clip_list(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    arg1: GuestPtr<()>,
    arg2: u32,
) -> Outcome {
    call(
        env,
        api::CLIPPER_SET_CLIP_LIST,
        &[this.addr(), arg1.addr(), arg2],
    )
}
#[win32(ddraw, name = "IDirectDrawClipper::GetHWnd", level = implement)]
fn clipper_get_hwnd(env: &mut Env<'_>, this: GuestPtr<()>, arg1: GuestPtr<()>) -> Outcome {
    call(env, api::CLIPPER_GET_HWND, &[this.addr(), arg1.addr()])
}
#[win32(ddraw, name = "IDirectDrawSurface7::GetBltStatus", level = implement)]
fn get_blt_status(env: &mut Env<'_>, this: GuestPtr<()>, arg1: u32) -> Outcome {
    call(env, api::GET_BLT_STATUS, &[this.addr(), arg1])
}
#[win32(ddraw, name = "IDirectDrawSurface7::GetFlipStatus", level = implement)]
fn get_flip_status(env: &mut Env<'_>, this: GuestPtr<()>, arg1: u32) -> Outcome {
    call(env, api::GET_FLIP_STATUS, &[this.addr(), arg1])
}
#[win32(ddraw, name = "IDirectDrawSurface7::GetClipper", level = implement)]
fn get_clipper(env: &mut Env<'_>, this: GuestPtr<()>, arg1: GuestPtr<()>) -> Outcome {
    call(env, api::GET_CLIPPER, &[this.addr(), arg1.addr()])
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
    let state = env.ext.get::<State>();
    let device = state.device(&env.sys.objects, this.addr());
    let outcome = call(env, api::RELEASE, &[this.addr()]);
    if let Some(device) = device
        && matches!(outcome, Outcome::Return(0))
    {
        let state = env.ext.get_mut::<State>();
        if state.binding.is_some_and(|(id, _)| id == device) {
            state.binding = None;
        }
        if let Some((id, surface)) = state.presented
            && id == device
        {
            state.presented = None;
            (state.provider.released)(env.ext, surface);
        }
    }
    outcome
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
fn validate_device(env: &Env<'_>, this: u32) -> Result<(), u32> {
    let state = env.ext.get::<State>();
    let Some(object) = state
        .draw
        .com
        .as_ref()
        .and_then(|com| com.get::<ComState>(&env.sys.objects, this))
        .filter(|object| matches!(object.kind, Kind::Device))
    else {
        return Err(0x88760082); // DDERR_INVALIDOBJECT
    };
    if !object.initialized {
        return Err(0x800401f0); // DDERR_NOTINITIALIZED
    }
    Ok(())
}
fn writable_u32(env: &Env<'_>, output: GuestPtr<u32>) -> Result<(), u32> {
    if output.is_null()
        || adv_abi::guest::check_range(env.cpu, output.addr(), 4, adv_cpu::backend::Prot::WRITE)
            .is_err()
    {
        return Err(INVALID);
    }
    Ok(())
}
fn device_scanline(
    env: &mut Env<'_>,
    this: u32,
    output: GuestPtr<u32>,
) -> Result<(bool, u32), u32> {
    validate_device(env, this)?;
    writable_u32(env, output)?;
    let vsync = (env.ext.get::<State>().provider.vsync)(env.ext);
    let height = env.ext.get::<SurfaceStore>().height;
    let now = env.sys.clock.guest_now_us();
    Ok(crate::pacing::scanline(height, 60, now, vsync))
}
/// The virtual 60 Hz adapter scans the visible rows and one blanking row.
/// Querying the guest clock also advances virtual-time polling loops.
#[win32(ddraw, name = "IDirectDraw7::GetScanLine", level = state)]
fn get_scan_line(env: &mut Env<'_>, this: GuestPtr<()>, output: GuestPtr<u32>) -> Outcome {
    let (blank, line) = match device_scanline(env, this.addr(), output) {
        Ok(result) => result,
        Err(hr) => return Outcome::Return(hr),
    };
    if blank {
        return Outcome::Return(0x88760219); // DDERR_VERTICALBLANKINPROGRESS
    }
    Outcome::Return(output.write(env.cpu, line).map_or(INVALID, |()| 0))
}
#[win32(ddraw, name = "IDirectDraw7::GetVerticalBlankStatus", level = state)]
fn get_vertical_blank_status(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    output: GuestPtr<u32>,
) -> Outcome {
    let (blank, _) = match device_scanline(env, this.addr(), output) {
        Ok(result) => result,
        Err(hr) => return Outcome::Return(hr),
    };
    Outcome::Return(
        output
            .write(env.cpu, u32::from(blank))
            .map_or(INVALID, |()| 0),
    )
}
#[win32(ddraw, name = "IDirectDraw7::GetMonitorFrequency", level = state)]
fn get_monitor_frequency(env: &mut Env<'_>, this: GuestPtr<()>, output: GuestPtr<u32>) -> Outcome {
    if let Err(hr) = validate_device(env, this.addr()).and_then(|()| writable_u32(env, output)) {
        return Outcome::Return(hr);
    }
    Outcome::Return(output.write(env.cpu, 60).map_or(INVALID, |()| 0))
}
#[win32(ddraw, name = "IDirectDraw7::GetAvailableVidMem", level = state)]
fn get_available_vid_mem(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    caps: GuestPtr<u32>,
    total: GuestPtr<u32>,
    free: GuestPtr<u32>,
) -> Outcome {
    const DDSCAPS_SYSTEMMEMORY: u32 = 0x800;
    if let Err(hr) = validate_device(env, this.addr()) {
        return Outcome::Return(hr);
    }
    if caps.is_null() || (total.is_null() && free.is_null()) {
        return Outcome::Return(INVALID);
    }
    let Ok(dw_caps) = caps.read(env.cpu) else {
        return Outcome::Return(INVALID);
    };
    if dw_caps & DDSCAPS_SYSTEMMEMORY != 0 {
        return Outcome::Return(INVALID);
    }
    if (!total.is_null() && writable_u32(env, total).is_err())
        || (!free.is_null() && writable_u32(env, free).is_err())
    {
        return Outcome::Return(INVALID);
    }
    if !total.is_null() && total.write(env.cpu, 64 * 1024 * 1024).is_err() {
        return Outcome::Return(INVALID);
    }
    if !free.is_null() && free.write(env.cpu, 32 * 1024 * 1024).is_err() {
        return Outcome::Return(INVALID);
    }
    Outcome::Return(0)
}
#[win32(ddraw, name = "IDirectDraw7::Compact", level = implement)]
fn compact(env: &mut Env<'_>, this: GuestPtr<()>) -> Outcome {
    call(env, api::COMPACT, &[this.addr()])
}
#[win32(ddraw, name = "IDirectDraw7::RestoreAllSurfaces", level = implement)]
fn restore_all_surfaces(env: &mut Env<'_>, this: GuestPtr<()>) -> Outcome {
    call(env, api::RESTORE_ALL_SURFACES, &[this.addr()])
}
#[win32(ddraw, name = "IDirectDraw7::TestCooperativeLevel", level = implement)]
fn test_cooperative_level(env: &mut Env<'_>, this: GuestPtr<()>) -> Outcome {
    call(env, api::TEST_COOPERATIVE_LEVEL, &[this.addr()])
}
#[win32(ddraw, name = "IDirectDraw7::WaitForVerticalBlank", level = state)]
fn wait_for_vertical_blank(
    _env: &mut Env<'_>,
    _this: GuestPtr<()>,
    flags: u32,
    _event: Handle,
) -> Outcome {
    // Like Wine, the virtual adapter has no scanout to wait for.
    const DDWAITVB_BLOCKBEGINEVENT: u32 = 0x2;
    Outcome::Return(if flags & DDWAITVB_BLOCKBEGINEVENT != 0 {
        UNSUPPORTED
    } else {
        0
    })
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
#[win32(ddraw, name = "IDirectDrawSurface7::SetColorKey", level = implement)]
fn set_color_key(env: &mut Env<'_>, this: GuestPtr<()>, flags: u32, key: GuestPtr<()>) -> Outcome {
    call(env, api::SET_COLOR_KEY, &[this.addr(), flags, key.addr()])
}
#[win32(ddraw, name = "IDirectDrawSurface7::GetColorKey", level = implement)]
fn get_color_key(env: &mut Env<'_>, this: GuestPtr<()>, flags: u32, key: GuestPtr<()>) -> Outcome {
    call(env, api::GET_COLOR_KEY, &[this.addr(), flags, key.addr()])
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
#[win32(ddraw, name = "IDirectDraw7::FlipToGDISurface", level = implement)]
fn flip_to_gdi(env: &mut Env<'_>, this: GuestPtr<()>) -> Outcome {
    call(env, api::FLIP_TO_GDI, &[this.addr()])
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
    get_dc(env, this.addr(), arg1.addr())
}
#[win32(ddraw, name = "IDirectDrawSurface7::ReleaseDC", level = implement)]
fn surface_release_dc(env: &mut Env<'_>, this: GuestPtr<()>, arg1: Handle) -> Outcome {
    release_dc(env, this.addr(), arg1.0)
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

#[win32(ddraw, name = "IDirectDraw7::Initialize", level = state)]
fn initialize(env: &mut Env<'_>, this: GuestPtr<()>, guid: GuestPtr<()>) -> Outcome {
    call(env, api::INITIALIZE, &[this.addr(), guid.addr()])
}

#[win32(ddraw, name = "IDirectDraw7::GetDeviceIdentifier", level = state)]
fn get_device_identifier(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    output: GuestPtr<adv_abi::types::DDDEVICEIDENTIFIER2>,
    flags: u32,
) -> Outcome {
    let state = env.ext.get::<State>();
    let Some(object) = state
        .draw
        .com
        .and_then(|com| com.get::<ComState>(&env.sys.objects, this.addr()))
    else {
        return Outcome::Return(INVALID);
    };
    if !matches!(object.kind, Kind::Device) || flags & !1 != 0 {
        return Outcome::Return(INVALID);
    }
    if !object.initialized {
        return Outcome::Return(0x800401f0);
    }
    let mut value = adv_abi::types::DDDEVICEIDENTIFIER2::default();
    let driver = b"adv-ddraw";
    let description = b"ADV virtual DirectDraw adapter";
    for (out, byte) in value.szDriver.iter_mut().zip(driver) {
        *out = *byte as i8;
    }
    for (out, byte) in value.szDescription.iter_mut().zip(description) {
        *out = *byte as i8;
    }
    value.liDriverVersion = 1;
    // Identify our software adapter, without claiming host hardware or WHQL.
    value.guidDeviceIdentifier = [0x66445641, 0x4e87461b, 0x219a38aa, 0x015eda1e];
    match output.write(env.cpu, value) {
        Ok(()) => Outcome::Return(0),
        Err(_) => Outcome::Return(INVALID),
    }
}

/// The adapter offers no FourCC overlay formats, so video renderers fall
/// back to RGB surfaces.
#[win32(ddraw, name = "IDirectDraw7::GetFourCCCodes", level = state)]
fn get_fourcc_codes(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    count: GuestPtr<u32>,
    _codes: GuestPtr<u32>,
) -> Outcome {
    let state = env.ext.get::<State>();
    let device = state
        .draw
        .com
        .and_then(|com| com.get::<ComState>(&env.sys.objects, this.addr()))
        .filter(|object| matches!(object.kind, Kind::Device));
    if device.is_none() || count.addr() == 0 {
        return Outcome::Return(INVALID);
    }
    match count.write(env.cpu, 0) {
        Ok(()) => Outcome::Return(0),
        Err(_) => Outcome::Return(INVALID),
    }
}

fn dc_surface(env: &Env<'_>, sid: u32) -> DcSurface {
    let surface = &env.ext.get::<SurfaceStore>()[sid];
    DcSurface {
        surface: Some(sid),
        pixels: surface.pixels,
        width: surface.width,
        height: surface.height,
        pitch: surface.pitch,
        format: surface.format,
    }
}
fn get_dc(env: &mut Env<'_>, object: u32, output: u32) -> Outcome {
    let state = env.ext.get::<State>();
    let Some(sid) = state.draw.surface_id(&env.sys.objects, object) else {
        return Outcome::Return(INVALID);
    };
    if env.ext.get::<SurfaceStore>()[sid].locked.is_some() {
        return Outcome::Return(BUSY);
    }
    let provider = state.provider;
    if let Err(error) = (provider.memory)(env.cpu).writable(output, 4) {
        return CallError::Argument(error).outcome(env);
    }
    let surface = dc_surface(env, sid);
    (provider.dc.create)(
        env,
        surface,
        Box::new(move |env, result| {
            let dc = match result {
                Ok(dc) => dc,
                Err(_) => return Outcome::Return(0x8007000e),
            };
            let result = {
                let mut memory = (provider.memory)(env.cpu);
                memory
                    .writable(output, 4)
                    .and_then(|()| memory.put_u32(output, dc))
            };
            if let Err(error) = result {
                return (provider.dc.release)(
                    env,
                    dc,
                    Box::new(move |env, _| CallError::Argument(error).outcome(env)),
                );
            }
            env.ext.get_mut::<State>().draw.surface_dcs.insert(dc, sid);
            Outcome::Return(0)
        }),
    )
}
fn release_dc(env: &mut Env<'_>, object: u32, dc: u32) -> Outcome {
    let state = env.ext.get::<State>();
    let Some(sid) = state.draw.surface_id(&env.sys.objects, object) else {
        return Outcome::Return(INVALID);
    };
    let provider = state.provider;
    let surface = dc_surface(env, sid);
    (provider.dc.flush)(
        env,
        dc,
        surface,
        Box::new(move |env, result| {
            if result.is_err() {
                return Outcome::Return(INVALID);
            }
            (provider.dc.release)(
                env,
                dc,
                Box::new(move |env, result| {
                    if result.is_err() {
                        return Outcome::Return(INVALID);
                    }
                    env.ext.get_mut::<State>().draw.surface_dcs.remove(&dc);
                    let store = env.ext.get_mut::<SurfaceStore>();
                    if store.primary_id() == Some(sid) {
                        let memory = (provider.memory)(env.cpu);
                        if let Err(error) =
                            store.present_from(env.context, sid, &memory, store[sid].rect())
                        {
                            return Outcome::Fail(format!("DirectDraw: {error}"));
                        }
                        let serial = store.stats.presentations;
                        State::submitted(env.ext, sid);
                        if let Some(outcome) = (provider.completed)(env, serial) {
                            return outcome;
                        }
                    }
                    Outcome::Return(0)
                }),
            )
        }),
    )
}
