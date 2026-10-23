//! Window surfaces. As Wine's Mac driver wraps its own pixels, the driver
//! gives win32u a surface over guest pixels it owns, so GDI draws a window
//! straight into what the host presents. win32u rounds a surface up to whole
//! tiles; a flush presents a store view of the window's part of it, without
//! copying.
use std::collections::HashMap;

use adv_abi::{Conv, Env, Outcome, Registry};
use adv_cpu::backend::{GuestExt, Prot};
use adv_device::surface::{PixelFormat, Rect, SurfaceStore};
use adv_machine::outcome::Resume;

use super::driver::{self, MODULE, win32u};
use super::{free_slot, slot, write_words};
use crate::graphics::{frame_completed, guest_memory};

/// The host functions of `struct window_surface_funcs`, in its order.
pub(super) const FUNCS: [&str; 3] = ["SurfaceSetClip", "SurfaceFlush", "SurfaceDestroy"];
/// `sizeof(struct window_surface)` (i386); the driver adds no fields.
const SURFACE_SIZE: u32 = 108;
const DCX_WINDOW: u32 = 1;
const DCX_CACHE: u32 = 2;
const D3DDDIFMT_A8R8G8B8: u32 = 21;
/// hDc and hBitmap in `D3DKMT_CREATEDCFROMMEMORY` (i386).
const CREATED_DC: u32 = 28;
const CREATED_BITMAP: u32 = 32;

/// A window surface's pixels and the store view that presents them.
struct Surface {
    hwnd: u32,
    pixels: u32,
    width: u32,
    height: u32,
    /// The store surface over the window's part, with its size.
    view: Option<(u32, u32, u32)>,
}

/// The driver's window surfaces, by address.
#[derive(Default)]
struct Surfaces(HashMap<u32, Surface>);

pub(super) fn register(registry: &mut Registry) {
    registry.add(
        MODULE,
        "CreateWindowSurface",
        Conv::Cdecl { argc: 4 },
        create,
    );
    registry.add(MODULE, "SurfaceSetClip", Conv::Cdecl { argc: 3 }, |_| {
        Outcome::Return(0)
    });
    registry.add(MODULE, "SurfaceFlush", Conv::Cdecl { argc: 8 }, flush);
    registry.add(MODULE, "SurfaceDestroy", Conv::Cdecl { argc: 1 }, destroy);
}

/// Unwrap, or end the call with the error.
macro_rules! check {
    ($result:expr) => {
        match $result {
            Ok(value) => value,
            Err(error) => return Outcome::Fail(format!("{MODULE}: {error}")),
        }
    };
}

fn rect(env: &Env<'_>, at: u32) -> Result<[i32; 4], String> {
    let mut sides = [0; 4];
    for (index, side) in sides.iter_mut().enumerate() {
        *side = env
            .cpu
            .read_u32(at + 4 * index as u32)
            .map_err(|e| e.to_string())? as i32;
    }
    Ok(sides)
}

/// A guest call to win32u.so's export `name`.
fn call(
    env: &mut Env<'_>,
    name: &str,
    args: impl Into<Vec<u32>>,
    then: impl FnOnce(&mut Env<'_>, Resume) -> Outcome + 'static,
) -> Outcome {
    Outcome::call_aligned(env, check!(win32u(env, name)), args, then)
}

/// CreateWindowSurface(hwnd, layered, surface_rect, surface).
fn create(env: &mut Env<'_>) -> Outcome {
    let (hwnd, bounds, output) = (env.arg(0), env.arg(2), env.arg(3));
    if driver::is_desktop(env, hwnd) {
        // Keep win32u's default surface.
        return Outcome::Return(1);
    }
    let previous = check!(env.cpu.read_u32(output));
    if previous != 0 && env.cpu.read_u32(previous).ok() == Some(driver::surface_funcs(env)) {
        return Outcome::Return(1);
    }
    let [left, top, right, bottom] = check!(rect(env, bounds));
    let (width, height) = (right - left, bottom - top);
    if width <= 0 || height <= 0 {
        // Keep win32u's default surface.
        return Outcome::Return(1);
    }
    let (width, height) = (width as u32, height as u32);
    if previous == 0 {
        return wrap(env, hwnd, bounds, output, width, height);
    }
    call(env, "window_surface_release", [previous], move |env, _| {
        check!(env.cpu.write_u32(output, 0));
        wrap(env, hwnd, bounds, output, width, height)
    })
}

/// Wrap new store pixels in a bitmap and make the window surface over it.
fn wrap(
    env: &mut Env<'_>,
    hwnd: u32,
    bounds: u32,
    output: u32,
    width: u32,
    height: u32,
) -> Outcome {
    let pitch = width * 4;
    let pixels = check!(env.sys.vm.alloc(env.cpu, pitch * height, Prot::RW));
    let flags = DCX_CACHE | DCX_WINDOW;
    call(
        env,
        "NtUserGetDCEx",
        [hwnd, 0, flags],
        move |env, device| {
            let device = device.value();
            let desc = check!(slot(env));
            let words = [
                pixels,
                D3DDDIFMT_A8R8G8B8,
                width,
                height,
                pitch,
                device,
                0,
                0,
                0,
            ];
            check!(write_words(env, desc, &words));
            call(
                env,
                "NtGdiDdDDICreateDCFromMemory",
                [desc],
                move |env, status| {
                    let created = check!(env.cpu.read_u32(desc + CREATED_DC));
                    let bitmap = check!(env.cpu.read_u32(desc + CREATED_BITMAP));
                    free_slot(env, desc);
                    if status.value() != 0 {
                        let _ = env.sys.vm.free(env.cpu, pixels);
                        return call(env, "NtUserReleaseDC", [hwnd, device], |_, _| {
                            Outcome::Return(1)
                        });
                    }
                    call(env, "NtGdiDeleteObjectApp", [created], move |env, _| {
                        call(env, "NtUserReleaseDC", [hwnd, device], move |env, _| {
                            surface(
                                env,
                                hwnd,
                                bounds,
                                output,
                                pixels,
                                bitmap,
                                (width, height, pitch),
                            )
                        })
                    })
                },
            )
        },
    )
}

fn surface(
    env: &mut Env<'_>,
    hwnd: u32,
    bounds: u32,
    output: u32,
    pixels: u32,
    bitmap: u32,
    (width, height, pitch): (u32, u32, u32),
) -> Outcome {
    // A top-down 32-bit BITMAPINFOHEADER: planes 1 and 32 bits share a word.
    let info = check!(slot(env));
    let header = [
        40,
        width,
        height.wrapping_neg(),
        1 | 32 << 16,
        0,
        pitch * height,
        0,
        0,
        0,
        0,
    ];
    check!(write_words(env, info, &header));
    let funcs = driver::surface_funcs(env);
    let args = [SURFACE_SIZE, funcs, hwnd, bounds, info, bitmap];
    call(env, "window_surface_create", args, move |env, created| {
        free_slot(env, info);
        let created = created.value();
        if created == 0 {
            let _ = env.sys.vm.free(env.cpu, pixels);
            return Outcome::Return(1);
        }
        let surface = Surface {
            hwnd,
            pixels,
            width,
            height,
            view: None,
        };
        env.ext
            .get_or_default::<Surfaces>()
            .0
            .insert(created, surface);
        check!(env.cpu.write_u32(output, created));
        Outcome::Return(1)
    })
}

/// flush(surface, rect, dirty, color_info, color_bits, shape_changed, shape_info, shape_bits).
fn flush(env: &mut Env<'_>) -> Outcome {
    let (address, dirty) = (env.arg(0), env.arg(2));
    let Some(surface) = env.ext.get_or_default::<Surfaces>().0.get(&address) else {
        return Outcome::Return(0);
    };
    let (hwnd, pixels, full, view) = (
        surface.hwnd,
        surface.pixels,
        (surface.width, surface.height),
        surface.view,
    );
    let Some(window) = driver::window(env, hwnd).filter(|window| window.visible) else {
        return Outcome::Return(1);
    };
    let [left, top, right, bottom] = window.extent;
    let width = ((right - left).max(0) as u32).min(full.0);
    let height = ((bottom - top).max(0) as u32).min(full.1);
    if width == 0 || height == 0 {
        return Outcome::Return(1);
    }
    let [left, top, right, bottom] = check!(rect(env, dirty));
    let damage = Rect {
        x: left.max(0) as u32,
        y: top.max(0) as u32,
        w: (right - left).max(0) as u32,
        h: (bottom - top).max(0) as u32,
    };
    let (changed, presented) = {
        let store = env.ext.get_mut::<SurfaceStore>();
        let mut memory = guest_memory(env.cpu);
        let sid = match view {
            Some((sid, w, h)) if (w, h) == (width, height) => sid,
            _ => {
                if let Some((old, _, _)) = view {
                    store.delete(old);
                }
                let part = adv_device::surface::Surface {
                    pixels,
                    width,
                    height,
                    pitch: full.0 * 4,
                    bottom_up: false,
                    locked: None,
                    version: 0,
                    format: PixelFormat::Bgra32,
                    palette: None,
                };
                let sid = check!(store.import_guest(part, &mut memory));
                env.ext
                    .get_mut::<Surfaces>()
                    .0
                    .get_mut(&address)
                    .unwrap()
                    .view = Some((sid, width, height));
                sid
            }
        };
        let store = env.ext.get_mut::<SurfaceStore>();
        if (store.width, store.height) != (width, height) {
            check!(store.configure_window(width, height, &mut memory));
        }
        let before = store.stats.presentations;
        check!(store.present_from(env.context, sid, &memory, damage));
        (
            store.stats.presentations != before,
            store.stats.presentations,
        )
    };
    if changed && let Some(outcome) = frame_completed(env, presented) {
        return outcome;
    }
    Outcome::Return(1)
}

fn destroy(env: &mut Env<'_>) -> Outcome {
    let address = env.arg(0);
    if let Some(surface) = env.ext.get_or_default::<Surfaces>().0.remove(&address) {
        if let Some((sid, _, _)) = surface.view {
            env.ext.get_mut::<SurfaceStore>().delete(sid);
        }
        let _ = env.sys.vm.free(env.cpu, surface.pixels);
    }
    Outcome::Return(0)
}
