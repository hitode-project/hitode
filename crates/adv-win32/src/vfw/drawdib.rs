//! DrawDib aliases borrow the ordinary object table and the existing DIB raster.
#![allow(clippy::too_many_arguments)] // Fixed SDK signature.
use adv_abi::{GuestPtr, Handle, Registry, win32};
use adv_cpu::backend::{GuestExt, Prot};
use adv_machine::{Env, Namespace, Outcome};

use crate::{Win32, gdi32::raster::front, util::set_last_error};

struct DrawDib;

#[derive(Default)]
struct DrawDibs {
    // Only an alias view; the payload has one owner in Objects.
    namespace: Option<Namespace>,
}

fn valid(env: &Env<'_>, handle: Handle) -> bool {
    env.ext
        .try_get::<DrawDibs>()
        .and_then(|d| d.namespace)
        .and_then(|ns| env.sys.objects.lookup_alias::<DrawDib>(ns, handle.0))
        .is_some()
}

#[win32(msvfw32, name = "DrawDibOpen", level = state)]
fn open(env: &mut Env<'_>) -> Handle {
    let devices = env.ext.get_or_default::<DrawDibs>();
    if devices.namespace.is_none() {
        devices.namespace = env.sys.objects.new_namespace_in(0x0c00_0004, 0x0cff_fffc);
    }
    Handle(
        devices
            .namespace
            .and_then(|ns| env.sys.objects.insert_host_in(ns, DrawDib))
            .unwrap_or(0),
    )
}

#[win32(msvfw32, name = "DrawDibClose", level = state)]
fn close(env: &mut Env<'_>, handle: Handle) -> i32 {
    if !valid(env, handle) {
        return 0;
    }
    let namespace = env.ext.get::<DrawDibs>().namespace.unwrap();
    i32::from(env.sys.objects.close_alias(namespace, handle.0).is_some())
}

fn unsupported(env: &mut Env<'_>, reason: &str) -> Outcome {
    adv_abi::warn_unavailable(env, "msvfw32.dll!DrawDibDraw", reason);
    set_last_error(env, 50);
    Outcome::Return(0)
}

#[win32(msvfw32, name = "DrawDibDraw", level = implement)]
fn draw(
    env: &mut Env<'_>,
    handle: Handle,
    dc: Handle,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    info: GuestPtr<()>,
    bits: GuestPtr<()>,
    sx: i32,
    sy: i32,
    sw: i32,
    sh: i32,
    flags: u32,
) -> Outcome {
    if !valid(env, handle) || dc.0 == 0 {
        return Outcome::Return(0);
    }
    if flags != 0 {
        return unsupported(env, "only unbuffered DrawDib flags=0 is available");
    }
    if info.is_null() || adv_abi::guest::check_range(env.cpu, info.addr(), 40, Prot::READ).is_err()
    {
        return Outcome::Return(0);
    }
    let mut header = [0; 40];
    if env.cpu.read(info.addr(), &mut header).is_err() {
        return Outcome::Return(0);
    }
    let word = |at| u32::from_le_bytes(header[at..at + 4].try_into().unwrap());
    let bitmap_width = word(4) as i32;
    let bitmap_height = word(8) as i32;
    let bpp = u16::from_le_bytes(header[14..16].try_into().unwrap());
    if word(0) != 40 || !matches!(bpp, 8 | 32) || word(16) != 0 {
        return unsupported(env, "only 40-byte BI_RGB 8/32-bit DIBs are available");
    }
    if bitmap_width <= 0
        || bitmap_height <= 0
        || header[12..14] != 1u16.to_le_bytes()
        || adv_device::surface::size(bitmap_width as u32, bitmap_height as u32).is_err()
        || sx < 0
        || sy < 0
        || sw <= 0
        || sh <= 0
        || i64::from(sx) + i64::from(sw) > i64::from(bitmap_width)
        || i64::from(sy) + i64::from(sh) > i64::from(bitmap_height)
    {
        return Outcome::Return(0);
    }
    let colors = if bpp == 8 {
        if word(32) == 0 { 256 } else { word(32) }
    } else {
        0
    };
    if colors > 256 {
        return Outcome::Return(0);
    }
    let info_length = 40 + colors * 4;
    if adv_abi::guest::check_range(env.cpu, info.addr(), info_length, Prot::READ).is_err() {
        return Outcome::Return(0);
    }
    // Native accepts a contiguous DIB when lpBits is NULL. Its color table is
    // still decoded by the shared GDI DIB reader, never by a second rasterizer.
    let bits = if bits.is_null() {
        let Some(address) = info.addr().checked_add(info_length) else {
            return Outcome::Return(0);
        };
        GuestPtr::new(address)
    } else {
        bits
    };
    let width = if width == -1 { bitmap_width } else { width };
    let height = if height == -1 { bitmap_height } else { height };
    if width < 0 || height < 0 {
        return unsupported(env, "mirrored DrawDib destinations are unavailable");
    }
    let pitch = (bitmap_width as u32 * u32::from(bpp / 8) + 3) & !3;
    if width != 0
        && height != 0
        && adv_abi::guest::check_range(
            env.cpu,
            bits.addr(),
            pitch * bitmap_height as u32,
            Prot::READ,
        )
        .is_err()
    {
        return Outcome::Return(0);
    }
    if front::provider(env.ext).is_none()
        || env
            .ext
            .try_get::<adv_device::surface::SurfaceStore>()
            .is_none()
    {
        return unsupported(env, "the shared GDI raster backend is unavailable");
    }
    // Native flags=0 leaves COLORONCOLOR installed on the target DC. This is
    // another view of the same GDI owner, with no private device state.
    let gdi = env.ext.get::<Win32>().gdi.clone();
    if adv_os::gdi::DeviceContexts::new(gdi)
        .set_stretch_mode(&mut env.sys.objects, dc.0, 3)
        .is_none()
    {
        return Outcome::Return(0);
    }
    // DrawDib's source starts at the top-left; StretchDIBits uses the DIB's
    // bottom edge. BOOL conversion happens before the shared presentation hook.
    front::dib_transfer(
        env,
        dc,
        [x, y, width, height],
        [sx, bitmap_height - sy - sh, sw, sh],
        bits,
        info,
        0,
        0x00cc0020,
        "msvfw32.dll!DrawDibDraw",
        true,
    )
}

pub(super) fn register(registry: &mut Registry) {
    registry.register(open_ABI);
    registry.register(close_ABI);
    registry.register(draw_ABI);
}
