//! Generated GDI entry points and Win32 ownership over the single device store.
#![allow(clippy::too_many_arguments)] // Fixed SDK signatures.
use super::*;
use crate::{Win32, util::set_last_error};
use adv_abi::{Env, GuestPtr, Handle, Outcome, Registry, win32};
use adv_cpu::backend::{Backend, Guest, Prot};
use adv_device::surface::{MemoryView, SurfaceStore};
use adv_machine::{Ext, Machine, Objects};

mod display;
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
}
pub fn install<B: Backend>(machine: &mut Machine<B>) {
    let gdi = machine.ext.get::<Win32>().gdi.clone();
    machine.ext.insert(State {
        raster: Raster::default(),
        dcs: DeviceContexts::new(gdi),
        provider: None,
        surface_dcs: Default::default(),
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
        Ok(0) if error != 0 => fail(env, error),
        Ok(value) => Outcome::Return(value),
        Err(error) => Outcome::Fail(format!("GDI raster: {error}")),
    }
}

pub(super) fn get_dc_value(env: &mut Env<'_>, hwnd: u32) -> Outcome {
    if !available(env) {
        return Outcome::Return(super::super::window_dc(env, hwnd));
    }
    let size = if hwnd == 0 {
        Some(env.ext.get::<Win32>().user.screen)
    } else {
        env.ext
            .get::<Win32>()
            .user
            .windows
            .get(&env.sys.objects, &hwnd)
            .map(|w| w.client_size())
    };
    let Some((width, height)) = size else {
        return fail(env, 1400);
    };
    let provider = env.ext.get::<State>().provider.unwrap();
    let primary = {
        let store = env.ext.get_mut::<SurfaceStore>();
        let mut memory = (provider.memory)(env.cpu);
        if width > 0
            && height > 0
            && let Err(error) = store.configure_window(width as u32, height as u32, &mut memory)
        {
            return Outcome::Fail(format!("GetDC resize: {error}"));
        }
        match store.primary(&mut memory) {
            Ok(id) => id,
            Err(error) => return Outcome::Fail(format!("GetDC: {error}")),
        }
    };
    let gdi = &mut env.ext.get_mut::<Win32>().gdi;
    if !gdi.register_external_dc(&mut env.sys.objects, WINDOW_DC, hwnd) {
        return fail(env, 8);
    }
    gdi.set_external_bitmap(&mut env.sys.objects, WINDOW_DC, primary);
    Outcome::Return(WINDOW_DC)
}
#[win32(user32, name = "GetDC", level = implement)]
fn get_dc(env: &mut Env<'_>, hwnd: Handle) -> Outcome {
    get_dc_value(env, hwnd.0)
}
#[win32(user32, name = "GetWindowDC", level = implement)]
fn get_window_dc(env: &mut Env<'_>, hwnd: Handle) -> Outcome {
    // The renderer currently models client pixels for both DC accessors.
    get_dc_value(env, hwnd.0)
}

#[win32(user32, name = "ReleaseDC", level = implement)]
fn release_dc(env: &mut Env<'_>, _hwnd: Handle, dc: Handle) -> Outcome {
    if available(env) {
        if dc.0 != WINDOW_DC {
            return Outcome::Return(0);
        }
        env.ext
            .get_mut::<Win32>()
            .gdi
            .remove_external_dc(&mut env.sys.objects, dc.0);
        Outcome::Return(1)
    } else {
        Outcome::Return(u32::from(super::super::release_dc(env, dc.0)))
    }
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
    let (width, height) = window.client_size();
    if let Err(error) = adv_abi::guest::check_range(env.cpu, paint.addr(), 64, Prot::WRITE) {
        return Outcome::Fail(format!("BeginPaint: {error}"));
    }
    let dc = match get_dc_value(env, hwnd.0) {
        Outcome::Return(0) => return Outcome::Return(0),
        Outcome::Return(dc) => dc,
        other => return other,
    };
    let mut bytes = [0u8; 64];
    bytes[..4].copy_from_slice(&dc.to_le_bytes());
    bytes[16..20].copy_from_slice(&width.to_le_bytes());
    bytes[20..24].copy_from_slice(&height.to_le_bytes());
    if let Err(error) = paint.write(env.cpu, bytes) {
        env.ext
            .get_mut::<Win32>()
            .gdi
            .remove_external_dc(&mut env.sys.objects, dc);
        return Outcome::Fail(format!("BeginPaint: {error}"));
    }
    crate::user32::message::validate(env, hwnd.0);
    Outcome::Return(dc)
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
    if section.0 != 0 {
        let _ = bits.write(env.cpu, 0);
        return fail(env, 87);
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
    raster_call(env, |r, s, d, o, m| {
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
    raster_call(env, |r, s, d, o, m| {
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
    if !available(env) {
        return fail(env, 6);
    }
    if adv_abi::guest::check_range(env.cpu, info.addr(), 40, Prot::READ).is_err() {
        return fail(env, 87);
    }
    raster_call(env, |r, s, d, o, m| {
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
