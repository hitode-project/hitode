//! Generated GDI entry points and Win32 ownership over the single device store.
#![allow(clippy::too_many_arguments)] // Fixed SDK signatures.
use super::*;
use crate::{Win32, util::set_last_error};
use adv_abi::{Env, GuestPtr, Handle, Outcome, Registry, win32};
use adv_cpu::backend::{Backend, Guest, GuestExt, Prot};
use adv_device::surface::{MemoryView, SurfaceStore};
use adv_machine::{Ext, Machine, Objects};
use adv_os::gdi::WINDOW_DC;

mod display;
mod mapped;
pub mod surface_dc;

#[derive(Clone, Copy)]
pub struct DrawingProvider {
    pub memory: for<'a> fn(&'a mut (dyn Guest + 'a)) -> MemoryView<'a, dyn Guest + 'a>,
    pub completed: fn(&mut Env<'_>, u64) -> Option<Outcome>,
}
pub struct State {
    raster: Raster,
    dcs: DeviceContexts,
    provider: Option<DrawingProvider>,
    /// Canvas bitmaps lent to another frontend, retired only by that owner.
    surface_dcs: std::collections::BTreeMap<u32, u32>,
    /// Balanced USER acquisitions over the existing GDI/surface owner. Paint
    /// leases belong to their PAINTSTRUCT, not to arbitrary guest HDC values.
    window_leases: Vec<WindowDcLease>,
    /// Only identities are retained here; all private DC attributes have the
    /// same GDI object owner as common display and compatible memory DCs.
    private_dcs: std::collections::BTreeMap<u32, u32>,
}
const MAX_WINDOW_LEASES: usize = 4096;
#[derive(Clone, Copy, PartialEq, Eq)]
enum WindowDcUse {
    Get,
    Window,
    Paint(u32),
    Erase,
}
struct WindowDcLease {
    context: u32,
    hwnd: u32,
    dc: u32,
    usage: WindowDcUse,
    paint_before: Option<Option<Vec<[i32; 4]>>>,
}
pub fn install<B: Backend>(machine: &mut Machine<B>) {
    let gdi = machine.ext.get::<Win32>().gdi.clone();
    machine.ext.insert(State {
        raster: Raster::default(),
        dcs: DeviceContexts::new(gdi),
        provider: None,
        surface_dcs: Default::default(),
        window_leases: Vec::new(),
        private_dcs: Default::default(),
    });
}
pub fn set_provider(ext: &mut Ext, provider: DrawingProvider) {
    ext.get_mut::<State>().provider = Some(provider);
}
pub(crate) fn provider(ext: &Ext) -> Option<DrawingProvider> {
    ext.try_get::<State>().and_then(|state| state.provider)
}
/// Stable diagnostic ids, independent of runtime ABI decoding.
pub const BINDINGS: &[(u32, adv_abi::Api)] = &[
    (api::GET_DC, get_dc_ABI),
    (api::RELEASE_DC, release_dc_ABI),
    (api::CREATE_DC, create_dc_ABI),
    (api::CREATE_DIB, create_dib_ABI),
    (api::SELECT, select_object_ABI),
    (api::DELETE_OBJECT, delete_object_ABI),
    (api::DELETE_DC, delete_dc_ABI),
    (api::BITBLT, bit_blt_ABI),
    (api::STRETCHBLT, stretch_blt_ABI),
    (api::STRETCH_DIB, stretch_dib_ABI),
    (api::SET_STRETCH_MODE, set_stretch_mode_ABI),
    (api::FLUSH, flush_ABI),
];
pub fn register(registry: &mut Registry) {
    display::register(registry);
    super::fill::register(registry);
    super::clip::register(registry);
    super::color_table::register(registry);
    for &(_, api) in BINDINGS {
        registry.register(api);
    }
    registry.register(begin_paint_ABI);
    registry.register(get_window_dc_ABI);
}
fn available(env: &Env<'_>) -> bool {
    env.ext.try_get::<SurfaceStore>().is_some()
        && env
            .ext
            .try_get::<State>()
            .is_some_and(|s| s.provider.is_some())
}
fn fail(env: &mut Env<'_>, code: u32) -> Outcome {
    set_last_error(env, code);
    Outcome::Return(0)
}
fn raster_call(
    env: &mut Env<'_>,
    call: impl FnOnce(
        &mut Raster,
        &mut SurfaceStore,
        &mut DeviceContexts,
        &mut Objects,
        &mut dyn SurfaceMemory,
    ) -> Result<u32, String>,
) -> Outcome {
    raster_call_named(env, None, call)
}
fn raster_call_named(
    env: &mut Env<'_>,
    api: Option<&str>,
    call: impl FnOnce(
        &mut Raster,
        &mut SurfaceStore,
        &mut DeviceContexts,
        &mut Objects,
        &mut dyn SurfaceMemory,
    ) -> Result<u32, String>,
) -> Outcome {
    let Some(provider) = env.ext.get::<State>().provider else {
        return Outcome::Fail("raster memory provider is not installed".into());
    };
    let (result, presented, error, changed) = {
        let (state, store) = env.ext.get2_installed_mut::<State, SurfaceStore>();
        state.raster.last_error = 0;
        let before = store.stats.presentations;
        let result = call(
            &mut state.raster,
            store,
            &mut state.dcs,
            &mut env.sys.objects,
            &mut (provider.memory)(env.cpu),
        );
        (
            result,
            store.stats.presentations,
            state.raster.last_error,
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
        Ok(0) if error != 0 => {
            if error == 50
                && let Some(api) = api
            {
                adv_abi::warn_unavailable(
                    env,
                    api,
                    "the coordinate mapping or translated device range is unavailable",
                );
            }
            fail(env, error)
        }
        Ok(value) => Outcome::Return(value),
        Err(error) => Outcome::Fail(format!("GDI raster: {error}")),
    }
}

fn acquire_window_dc(env: &mut Env<'_>, hwnd: u32, usage: WindowDcUse, api: &str) -> Outcome {
    let window = if hwnd == 0 {
        Some((env.ext.get::<Win32>().user.screen, 0))
    } else {
        env.ext
            .get::<Win32>()
            .user
            .windows
            .get(&env.sys.objects, &hwnd)
            .map(|w| (w.client_size(), w.dc_style))
    };
    let Some(((width, height), style)) = window else {
        return fail(env, 1400);
    };
    let private = usage != WindowDcUse::Window && style & 0x20 != 0;
    if usage != WindowDcUse::Window && !private && style & 0x40 != 0 {
        adv_abi::warn_unavailable(env, api, "class display DC sharing is unavailable");
        return fail(env, 50);
    }
    let raster = available(env);
    let state = env.ext.get_mut::<State>();
    if state.window_leases.len() == MAX_WINDOW_LEASES {
        adv_abi::warn_unavailable(env, api, "window DC acquisition capacity is exhausted");
        return fail(env, 50);
    }
    // The single display canvas cannot be rebound while another window or
    // thread still owns it. Same-window reentry retains its GDI state instead.
    if raster
        && state
            .window_leases
            .iter()
            .any(|lease| lease.hwnd != hwnd || lease.context != env.context)
    {
        adv_abi::warn_unavailable(
            env,
            api,
            "simultaneous display DC acquisitions for different windows or threads are unavailable",
        );
        return fail(env, 50);
    }
    if state.window_leases.try_reserve(1).is_err() {
        return fail(env, 8);
    }
    let dc = match acquire_window_dc_owner(env, hwnd, width, height, raster, private) {
        Outcome::Return(0) => return Outcome::Return(0),
        Outcome::Return(dc) => dc,
        result => return result,
    };
    env.ext
        .get_mut::<State>()
        .window_leases
        .push(WindowDcLease {
            context: env.context,
            hwnd,
            dc,
            usage,
            paint_before: None,
        });
    Outcome::Return(dc)
}
fn acquire_window_dc_owner(
    env: &mut Env<'_>,
    hwnd: u32,
    width: i32,
    height: i32,
    raster: bool,
    private: bool,
) -> Outcome {
    // Complete the fallible surface binding before publishing a new private
    // identity. A failed first acquisition must not retain a partial GDI DC.
    let primary = if raster {
        let provider = env.ext.get::<State>().provider.unwrap();
        let store = env.ext.get_mut::<SurfaceStore>();
        let mut memory = (provider.memory)(env.cpu);
        if width > 0
            && height > 0
            && let Err(error) = store.configure_window(width as u32, height as u32, &mut memory)
        {
            return Outcome::Fail(format!("GetDC resize: {error}"));
        }
        match store.primary(&mut memory) {
            Ok(id) => Some(id),
            Err(error) => return Outcome::Fail(format!("GetDC: {error}")),
        }
    } else {
        None
    };
    let dc = if private {
        if let Some(&dc) = env.ext.get::<State>().private_dcs.get(&hwnd) {
            dc
        } else {
            let dc = super::super::window_dc(env, hwnd);
            if dc == 0 {
                return fail(env, 8);
            }
            env.ext.get_mut::<State>().private_dcs.insert(hwnd, dc);
            dc
        }
    } else if raster {
        WINDOW_DC
    } else {
        super::super::window_dc(env, hwnd)
    };
    let Some(primary) = primary else {
        return Outcome::Return(dc);
    };
    let gdi = &mut env.ext.get_mut::<Win32>().gdi;
    if !gdi.register_external_dc(&mut env.sys.objects, dc, hwnd) {
        return fail(env, 8);
    }
    gdi.set_external_bitmap(&mut env.sys.objects, dc, primary);
    Outcome::Return(dc)
}
fn release_window_dc(env: &mut Env<'_>, hwnd: u32, dc: u32, usage: WindowDcUse) -> bool {
    let state = env.ext.get_mut::<State>();
    let Some(index) = state.window_leases.iter().rposition(|lease| {
        lease.context == env.context
            && lease.hwnd == hwnd
            && lease.dc == dc
            && (lease.usage == usage
                || (usage == WindowDcUse::Get && lease.usage == WindowDcUse::Window))
    }) else {
        return false;
    };
    let lease = state.window_leases.remove(index);
    if let Some(previous) = lease.paint_before
        && let Some(adv_os::gdi::GdiObj::Dc { clip, .. }) =
            env.ext.get::<Win32>().gdi.get_mut(&mut env.sys.objects, dc)
    {
        clip.replace_paint_region(previous);
    }
    let state = env.ext.get::<State>();
    if state.private_dcs.get(&hwnd) != Some(&dc)
        && !state.window_leases.iter().any(|lease| lease.dc == dc)
    {
        super::super::release_dc(env, dc);
    }
    true
}

/// Final USER removal follows WM_NCDESTROY callbacks, which may still use a DC.
pub(crate) fn retire_window(env: &mut Env<'_>, hwnd: u32) {
    let state = env.ext.get_mut::<State>();
    let private = state.private_dcs.remove(&hwnd);
    let mut retired = std::collections::BTreeSet::new();
    state.window_leases.retain(|lease| {
        if lease.hwnd == hwnd {
            retired.insert(lease.dc);
            false
        } else {
            true
        }
    });
    if let Some(dc) = private {
        retired.insert(dc);
    }
    retired.retain(|&dc| !state.window_leases.iter().any(|lease| lease.dc == dc));
    for dc in retired {
        env.ext
            .get_mut::<Win32>()
            .gdi
            .remove_external_dc(&mut env.sys.objects, dc);
    }
}
#[win32(user32, name = "GetDC", level = implement)]
fn get_dc(env: &mut Env<'_>, hwnd: Handle) -> Outcome {
    acquire_window_dc(env, hwnd.0, WindowDcUse::Get, "user32.dll!GetDC")
}
#[win32(user32, name = "GetWindowDC", level = implement)]
fn get_window_dc(env: &mut Env<'_>, hwnd: Handle) -> Outcome {
    // The renderer currently models client pixels for both DC accessors.
    acquire_window_dc(env, hwnd.0, WindowDcUse::Window, "user32.dll!GetWindowDC")
}

#[win32(user32, name = "ReleaseDC", level = implement)]
fn release_dc(env: &mut Env<'_>, hwnd: Handle, dc: Handle) -> Outcome {
    Outcome::Return(u32::from(release_window_dc(
        env,
        hwnd.0,
        dc.0,
        WindowDcUse::Get,
    )))
}
#[win32(user32, name = "BeginPaint", level = implement)]
fn begin_paint(env: &mut Env<'_>, hwnd: Handle, paint: GuestPtr<[u8; 64]>) -> Outcome {
    let Some(window) = env
        .ext
        .get::<Win32>()
        .user
        .windows
        .get(&env.sys.objects, &hwnd.0)
    else {
        return Outcome::Return(0);
    };
    let old = crate::user32::update::snapshot(window);
    if let Err(error) = adv_abi::guest::check_range(env.cpu, paint.addr(), 64, Prot::WRITE) {
        return Outcome::Fail(format!("BeginPaint: {error}"));
    }
    let dc = match acquire_window_dc(
        env,
        hwnd.0,
        WindowDcUse::Paint(paint.addr()),
        "user32.dll!BeginPaint",
    ) {
        Outcome::Return(0) => return Outcome::Return(0),
        Outcome::Return(dc) => dc,
        other => return other,
    };
    set_paint_layer(env, dc, old.rects.clone());
    let mut bytes = [0u8; 64];
    bytes[..4].copy_from_slice(&dc.to_le_bytes());
    let unerased = old.unerased && !old.rects.is_empty();
    bytes[4..8].copy_from_slice(&u32::from(unerased).to_le_bytes());
    for (field, value) in bytes[8..24]
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(old.bounds())
    {
        *field = value.to_le_bytes();
    }
    if let Err(error) = paint.write(env.cpu, bytes) {
        release_window_dc(env, hwnd.0, dc, WindowDcUse::Paint(paint.addr()));
        return Outcome::Fail(format!("BeginPaint: {error}"));
    }
    crate::user32::message::validate(env, hwnd.0);
    if old.erase && !old.rects.is_empty() {
        return crate::user32::message::send(
            env,
            hwnd.0,
            crate::user32::window::wm::ERASEBKGND,
            dc,
            0,
            move |env, handled| {
                crate::user32::update::erased(env, hwnd.0, &old, handled != 0);
                if !env
                    .ext
                    .get::<Win32>()
                    .user
                    .windows
                    .contains_key(&env.sys.objects, &hwnd.0)
                {
                    release_window_dc(env, hwnd.0, dc, WindowDcUse::Paint(paint.addr()));
                    return Outcome::Return(0);
                }
                if let Err(error) = GuestPtr::<u32>::new(paint.addr() + 4)
                    .write(env.cpu, u32::from(unerased && handled == 0))
                {
                    release_window_dc(env, hwnd.0, dc, WindowDcUse::Paint(paint.addr()));
                    return Outcome::Fail(format!("BeginPaint: {error}"));
                }
                Outcome::Return(dc)
            },
        );
    }
    Outcome::Return(dc)
}
fn set_paint_layer(env: &mut Env<'_>, dc: u32, region: Vec<[i32; 4]>) {
    let old = match env.ext.get::<Win32>().gdi.get_mut(&mut env.sys.objects, dc) {
        Some(adv_os::gdi::GdiObj::Dc { clip, .. }) => clip.replace_paint_region(Some(region)),
        _ => None,
    };
    env.ext
        .get_mut::<State>()
        .window_leases
        .last_mut()
        .unwrap()
        .paint_before = Some(old);
}
pub(crate) fn erase_update(
    env: &mut Env<'_>,
    hwnd: u32,
    old: crate::user32::update::Snapshot,
    then: impl FnOnce(&mut Env<'_>, bool) -> Outcome + 'static,
) -> Outcome {
    let dc = match acquire_window_dc(env, hwnd, WindowDcUse::Erase, "user32.dll!GetUpdateRect") {
        Outcome::Return(0) => return Outcome::Return(0),
        Outcome::Return(dc) => dc,
        other => return other,
    };
    set_paint_layer(env, dc, old.rects.clone());
    crate::user32::message::send(
        env,
        hwnd,
        crate::user32::window::wm::ERASEBKGND,
        dc,
        0,
        move |env, result| {
            crate::user32::update::erased(env, hwnd, &old, result != 0);
            release_window_dc(env, hwnd, dc, WindowDcUse::Erase);
            then(env, result != 0)
        },
    )
}
/// End only the acquisition represented by this PAINTSTRUCT. The documented
/// EndPaint return is always nonzero; an unmatched structure cannot retire a DC.
pub(crate) fn end_paint_value(env: &mut Env<'_>, hwnd: u32, paint: u32) -> Outcome {
    if let Err(error) = adv_abi::guest::check_range(env.cpu, paint, 64, Prot::READ) {
        return Outcome::Fail(format!("EndPaint: {error}"));
    }
    let dc = match env.cpu.read_u32(paint) {
        Ok(dc) => dc,
        Err(error) => return Outcome::Fail(format!("EndPaint: {error}")),
    };
    release_window_dc(env, hwnd, dc, WindowDcUse::Paint(paint));
    Outcome::Return(1)
}
#[win32(gdi32, name = "CreateCompatibleDC", level = implement)]
fn create_dc(env: &mut Env<'_>, source: Handle) -> Outcome {
    if !available(env) {
        return Outcome::Return(super::super::window_dc(env, 0));
    }
    raster_call(env, |r, s, d, o, m| r.create_dc(s, d, o, m, source.0))
}
#[win32(gdi32, name = "CreateDIBSection", level = implement)]
fn create_dib(
    env: &mut Env<'_>,
    dc: Handle,
    info: GuestPtr<()>,
    usage: u32,
    bits: GuestPtr<u32>,
    section: Handle,
    offset: u32,
) -> Outcome {
    if !available(env) {
        return fail(env, 50); // A raster store is required; no placeholder bitmap.
    }
    if adv_abi::guest::check_range(env.cpu, bits.addr(), 4, Prot::WRITE).is_err()
        || adv_abi::guest::check_range(env.cpu, info.addr(), 40, Prot::READ).is_err()
    {
        return fail(env, 87);
    }
    let mut header = [0; 40];
    if env.cpu.read(info.addr(), &mut header).is_err() {
        return fail(env, 87);
    }
    if header[16..20] == 3u32.to_le_bytes()
        && adv_abi::guest::check_range(env.cpu, info.addr(), 52, Prot::READ).is_err()
    {
        return fail(env, 87);
    }
    if header[14..16] == 8u16.to_le_bytes() {
        let used = u32::from_le_bytes(header[32..36].try_into().unwrap());
        let count = if used == 0 { 256 } else { used };
        if count > 256
            || adv_abi::guest::check_range(env.cpu, info.addr(), 40 + count * 4, Prot::READ)
                .is_err()
        {
            let _ = bits.write(env.cpu, 0);
            return fail(env, 87);
        }
    }
    let _ = (dc, offset); // An unbacked DIB does not retain its creation DC.
    let provider = env.ext.get::<State>().provider.unwrap();
    let decoded = match super::dib::read(&(provider.memory)(env.cpu), info.addr(), usage) {
        Ok(Some(dib)) => dib,
        _ => {
            let _ = bits.write(env.cpu, 0);
            return fail(env, 87);
        }
    };
    if bits.write(env.cpu, 0).is_err() {
        return fail(env, 87);
    }
    let Some(bytes) = decoded.pitch.checked_mul(decoded.height) else {
        return fail(env, 8);
    };
    if section.0 != 0 {
        return mapped::create(env, decoded, bits.addr(), section.0, offset);
    }
    let usage = env.ext.get::<SurfaceStore>().arena_usage();
    let Some(growth) = env.ext.get::<SurfaceStore>().arena_growth(bytes) else {
        adv_abi::warn_unavailable(
            env,
            "gdi32.dll!CreateDIBSection",
            &format!("out of surface storage: request {bytes}, {usage:?}"),
        );
        return fail(env, 8);
    };
    if growth != 0 {
        let address = match env.sys.vm.alloc(env.cpu, growth, Prot::RW) {
            Ok(address) => address,
            Err(error) => {
                adv_abi::warn_unavailable(
                    env,
                    "gdi32.dll!CreateDIBSection",
                    &format!("storage mapping failed: {error}; {usage:?}"),
                );
                return fail(env, 8);
            }
        };
        let length = env.sys.vm.region(address).unwrap();
        if let Err(error) = env
            .ext
            .get_mut::<SurfaceStore>()
            .extend_arena(address, length)
        {
            let _ = env.sys.vm.free(env.cpu, address);
            adv_abi::warn_unavailable(
                env,
                "gdi32.dll!CreateDIBSection",
                &format!("storage extension failed: {error}; {usage:?}"),
            );
            return fail(env, 8);
        }
        log::info!("CreateDIBSection surface pool +{length} bytes for {bytes}: {usage:?}");
    }
    raster_call(env, |r, s, d, o, m| {
        r.create_decoded_dib(s, d, o, m, decoded, bits.addr())
    })
}
#[win32(gdi32, name = "SelectObject", level = implement)]
fn select_object(env: &mut Env<'_>, dc: Handle, object: Handle) -> Outcome {
    if env
        .ext
        .try_get::<State>()
        .is_some_and(|s| s.surface_dcs.contains_key(&dc.0))
        && (object.0 == STOCK_BITMAP || env.ext.get::<SurfaceStore>().contains(object.0))
    {
        return fail(env, 87);
    }
    if available(env)
        && (object.0 == STOCK_BITMAP || env.ext.get::<SurfaceStore>().contains(object.0))
    {
        return raster_call(env, |r, s, d, o, m| {
            r.select_bitmap(s, d, o, m, dc.0, object.0)
        });
    }
    super::super::select_object_value(env, dc.0, object.0)
}
/// Reselect a bitmap through the drawing provider so its surface binding follows (RestoreDC).
pub(in crate::gdi32) fn reselect_bitmap(env: &mut Env<'_>, dc: u32, bitmap: u32) {
    let _ = select_object(env, Handle(dc), Handle(bitmap));
}
#[win32(gdi32, name = "DeleteObject", level = implement)]
fn delete_object(env: &mut Env<'_>, object: Handle) -> Outcome {
    if env
        .ext
        .try_get::<State>()
        .is_some_and(|s| s.surface_dcs.values().any(|&bitmap| bitmap == object.0))
    {
        return Outcome::Return(0);
    }
    if matches!(
        env.ext.get::<Win32>().gdi.get(&env.sys.objects, object.0),
        Some(adv_os::gdi::GdiObj::Dc { .. })
    ) {
        return Outcome::Return(0); // DCs are retired with DeleteDC/ReleaseDC.
    }
    if available(env) && env.ext.get::<SurfaceStore>().contains(object.0) {
        if let Some(outcome) = mapped::release(env, object.0) {
            return outcome;
        }
        return raster_call(env, |r, s, d, o, m| r.delete_bitmap(s, d, o, m, object.0));
    }
    if env
        .ext
        .get::<Win32>()
        .gdi
        .font_selected(&env.sys.objects, object.0)
    {
        return fail(env, 170);
    }
    super::super::delete_object_value(env, object.0)
}
#[win32(gdi32, name = "DeleteDC", level = implement)]
fn delete_dc(env: &mut Env<'_>, dc: Handle) -> Outcome {
    if env
        .ext
        .get::<State>()
        .private_dcs
        .values()
        .any(|&private| private == dc.0)
    {
        return Outcome::Return(0); // Private display DCs live until USER destroys their window.
    }
    if env
        .ext
        .get::<State>()
        .window_leases
        .iter()
        .any(|lease| lease.dc == dc.0)
    {
        return Outcome::Return(0); // USER leases are balanced by their accessors.
    }
    if available(env)
        && (dc.0 == WINDOW_DC || env.ext.get::<State>().surface_dcs.contains_key(&dc.0))
    {
        return Outcome::Return(0); // GetDC lends this DC; ReleaseDC returns it.
    }
    if available(env)
        && (dc.0 == WINDOW_DC || env.ext.get::<State>().dcs.contains(&env.sys.objects, dc.0))
    {
        return raster_call(env, |r, s, d, o, m| r.delete_dc(s, d, o, m, dc.0));
    }
    Outcome::Return(u32::from(super::super::release_dc(env, dc.0)))
}
#[win32(gdi32, name = "BitBlt", level = implement)]
fn bit_blt(
    env: &mut Env<'_>,
    dst: Handle,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    src: Handle,
    sx: i32,
    sy: i32,
    rop: u32,
) -> Outcome {
    if !available(env) {
        return fail(env, 6);
    }
    raster_call_named(env, Some("gdi32.dll!BitBlt"), |r, s, d, o, m| {
        r.bit_blt(
            s,
            d,
            o,
            m,
            dst.0,
            x as u32,
            y as u32,
            width as u32,
            height as u32,
            src.0,
            sx as u32,
            sy as u32,
            rop,
        )
    })
}
#[win32(gdi32, name = "StretchBlt", level = implement)]
fn stretch_blt(
    env: &mut Env<'_>,
    dst: Handle,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    src: Handle,
    sx: i32,
    sy: i32,
    sw: i32,
    sh: i32,
    rop: u32,
) -> Outcome {
    if !available(env) {
        return fail(env, 6);
    }
    raster_call_named(env, Some("gdi32.dll!StretchBlt"), |r, s, d, o, m| {
        r.stretch_blt(
            s,
            d,
            o,
            m,
            dst.0,
            x as u32,
            y as u32,
            width as u32,
            height as u32,
            src.0,
            sx as u32,
            sy as u32,
            sw as u32,
            sh as u32,
            rop,
        )
    })
}
#[win32(gdi32, name = "StretchDIBits", level = implement)]
fn stretch_dib(
    env: &mut Env<'_>,
    dst: Handle,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    sx: i32,
    sy: i32,
    sw: i32,
    sh: i32,
    bits: GuestPtr<()>,
    info: GuestPtr<()>,
    usage: u32,
    rop: u32,
) -> Outcome {
    dib_transfer(
        env,
        dst,
        [x, y, width, height],
        [sx, sy, sw, sh],
        bits,
        info,
        usage,
        rop,
        "gdi32.dll!StretchDIBits",
        false,
    )
}

pub(crate) fn dib_transfer(
    env: &mut Env<'_>,
    dst: Handle,
    [x, y, width, height]: [i32; 4],
    [sx, sy, sw, sh]: [i32; 4],
    bits: GuestPtr<()>,
    info: GuestPtr<()>,
    usage: u32,
    rop: u32,
    api: &str,
    boolean: bool,
) -> Outcome {
    if !available(env) {
        return fail(env, 6);
    }
    if adv_abi::guest::check_range(env.cpu, info.addr(), 40, Prot::READ).is_err() {
        return fail(env, 87);
    }
    raster_call_named(env, Some(api), |r, s, d, o, m| {
        r.stretch_dib(
            s,
            d,
            o,
            m,
            dst.0,
            x,
            y,
            width,
            height,
            sx,
            sy,
            sw,
            sh,
            bits.addr(),
            info.addr(),
            usage,
            rop,
        )
        .map(|value| {
            if boolean {
                u32::from(value != 0)
            } else {
                value
            }
        })
    })
}

#[win32(gdi32, name = "SetStretchBltMode", level = state)]
fn set_stretch_mode(env: &mut Env<'_>, dc: Handle, mode: i32) -> Outcome {
    if !available(env) {
        return fail(env, 6);
    }
    raster_call(env, |r, s, d, o, m| {
        r.set_stretch_mode(s, d, o, m, dc.0, mode as u32)
    })
}
#[win32(gdi32, name = "GdiFlush", level = state)]
fn flush(_env: &mut Env<'_>) -> i32 {
    1
}

#[cfg(test)]
mod tests;
