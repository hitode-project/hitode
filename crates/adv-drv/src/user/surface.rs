//! Window surfaces. As Wine's Mac driver wraps its own pixels, the driver
//! gives win32u a surface over guest pixels it owns, so GDI draws a window
//! straight into what the host presents. win32u rounds a surface up to whole
//! tiles. The base window's frames are a store view of its client area,
//! presented without copying; other shown top-level windows show as planes
//! over it (WINE-GFX-10).
use std::collections::HashMap;

use adv_abi::{Conv, Env, Outcome, Registry};
use adv_cpu::backend::{GuestExt, Prot};
use adv_device::surface::{PixelFormat, Plane, Rect, SurfaceStore};
use adv_machine::Ext;
use adv_machine::outcome::Resume;

use super::{MODULE, is_desktop, surface_funcs, win32u, window};
use crate::{Provider, free_slot, slot, write_words};

/// The host functions of `struct window_surface_funcs`, in its order.
pub(super) const FUNCS: [&str; 3] = ["SurfaceSetClip", "SurfaceFlush", "SurfaceDestroy"];
/// `sizeof(struct window_surface)` (i386); the driver adds no fields.
const SURFACE_SIZE: u32 = 108;
const DCX_WINDOW: u32 = 1;
const WS_MINIMIZE: u32 = 0x2000_0000;
const WS_DISABLED: u32 = 0x0800_0000;
const SWP_NOSIZE: u32 = 0x1;
const SWP_NOZORDER: u32 = 0x4;
const SWP_NOACTIVATE: u32 = 0x10;
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
    /// The store surface over the window's part, with its size and first pixel.
    view: Option<(u32, u32, u32, u32)>,
    /// Whether win32u has flushed it: only drawn windows show.
    drawn: bool,
    /// Created with win32u's `layered` flag: `pixels` hold premultiplied alpha.
    layered: bool,
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
    let (hwnd, layered, bounds, output) = (env.arg(0), env.arg(1) != 0, env.arg(2), env.arg(3));
    let previous = check!(env.cpu.read_u32(output));
    if previous != 0 && env.cpu.read_u32(previous).ok() == Some(surface_funcs(env)) {
        if let Some(surface) = env.ext.get_or_default::<Surfaces>().0.get_mut(&previous) {
            surface.layered = layered;
        }
        return Outcome::Return(1);
    }
    // The desktop presents nothing, but its DCs span the virtual screen:
    // exactly, where win32u's own surface would round it up to whole tiles
    // and the 1×1 default would clip it.
    let bounds = if is_desktop(env, hwnd) {
        super::display::screen(env)
    } else {
        bounds
    };
    let [left, top, right, bottom] = check!(rect(env, bounds));
    let (width, height) = (right - left, bottom - top);
    if width <= 0 || height <= 0 {
        // Keep win32u's default surface.
        return Outcome::Return(1);
    }
    let (width, height) = (width as u32, height as u32);
    if previous == 0 {
        return wrap(env, hwnd, layered, bounds, output, width, height);
    }
    call(env, "window_surface_release", [previous], move |env, _| {
        check!(env.cpu.write_u32(output, 0));
        wrap(env, hwnd, layered, bounds, output, width, height)
    })
}

/// Bytes behind a surface: a flush presents the client area as a view of whole
/// pitches from its first pixel, which runs up to one pitch past the last row.
fn storage(pitch: u32, height: u32) -> u32 {
    pitch * (height + 1)
}

/// Wrap new store pixels in a bitmap and make the window surface over it.
fn wrap(
    env: &mut Env<'_>,
    hwnd: u32,
    layered: bool,
    bounds: u32,
    output: u32,
    width: u32,
    height: u32,
) -> Outcome {
    let pitch = width * 4;
    let pixels = check!(env.sys.vm.alloc(env.cpu, storage(pitch, height), Prot::RW));
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
                                layered,
                                bounds,
                                output,
                                (pixels, bitmap),
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
    layered: bool,
    bounds: u32,
    output: u32,
    (pixels, bitmap): (u32, u32),
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
    let funcs = surface_funcs(env);
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
            drawn: false,
            layered,
        };
        env.ext
            .get_or_default::<Surfaces>()
            .0
            .insert(created, surface);
        env.ext
            .get::<adv_nt::native::UserServices>()
            .surface(created, true);
        check!(env.cpu.write_u32(output, created));
        Outcome::Return(1)
    })
}

const CLR_INVALID: u32 = u32::MAX;
const EMPTY_BOUNDS: [u32; 4] = [
    i32::MAX as u32,
    i32::MAX as u32,
    i32::MIN as u32,
    i32::MIN as u32,
];
const OVERDUE: std::time::Duration = std::time::Duration::from_millis(50);

/// flush(surface, rect, dirty, color_info, color_bits, shape_changed, shape_info, shape_bits).
fn flush(env: &mut Env<'_>) -> Outcome {
    let (address, dirty) = (env.arg(0), env.arg(2));
    let Some(surface) = env.ext.get_or_default::<Surfaces>().0.get_mut(&address) else {
        return Outcome::Return(0);
    };
    surface.drawn = true;
    let hwnd = surface.hwnd;
    if let Some(users) = env.ext.try_get::<adv_nt::native::UserServices>() {
        users.mark_surface_dirty(address, None);
    }
    if !super::client_geometry(env.ext, hwnd).is_some_and(|(_, visible)| visible) {
        return Outcome::Return(1);
    }
    let dirty = check!(rect(env, dirty));
    if let Some(outcome) = check!(present_visible(env, address, dirty)) {
        return outcome;
    }
    Outcome::Return(1)
}

fn present_visible(
    env: &mut Env<'_>,
    address: u32,
    dirty: [i32; 4],
) -> Result<Option<Outcome>, String> {
    let before = env.ext.get::<SurfaceStore>().stats.presentations;
    compose(env, Some((address, dirty)))?;
    let presented = env.ext.get::<SurfaceStore>().stats.presentations;
    if presented != before {
        env.sys.poll_work();
        let provider = *env.ext.get::<Provider>();
        if let Some(outcome) = (provider.completed)(env, presented) {
            return Ok(Some(outcome));
        }
    }
    Ok(None)
}

/// Flush dirty Wine window surfaces written by the waiting context (or overdue
/// past 50 ms) before parking, so GDI drawing from threads that wait without
/// pumping messages reaches the presenter without waiting for win32u's 50 ms
/// redraw gate (WINE-GFX-7, DEV-FRAME-8).
pub fn flush_on_wait(env: &mut Env<'_>) -> Option<Outcome> {
    let due = {
        let users = env.ext.try_get::<adv_nt::native::UserServices>()?;
        if !users.has_dirty_surfaces() {
            return None;
        }
        users.due_surfaces(env.context, OVERDUE)
    };
    for address in due {
        if adv_unix::mutex_locked(env.ext, address + adv_nt::native::WINDOW_SURFACE_MUTEX) {
            continue;
        }
        let Some(hwnd) = env
            .ext
            .get_or_default::<Surfaces>()
            .0
            .get(&address)
            .map(|surface| surface.hwnd)
        else {
            env.ext
                .get::<adv_nt::native::UserServices>()
                .mark_surface_dirty(address, None);
            continue;
        };
        let Ok(rect_sides) = rect(env, address + adv_nt::native::WINDOW_SURFACE_RECT) else {
            continue;
        };
        let Ok(bounds) = rect(env, address + adv_nt::native::WINDOW_SURFACE_BOUNDS) else {
            continue;
        };
        let Some(dirty) = adv_nt::native::window_surface_dirty(rect_sides, bounds) else {
            env.ext
                .get::<adv_nt::native::UserServices>()
                .mark_surface_dirty(address, None);
            continue;
        };
        // Only bypass win32u's window_surface_flush when it would perform no
        // color-key, alpha-mask or shape-bitmap conversion of its own.
        let plain = env
            .cpu
            .read_u32(address + adv_nt::native::WINDOW_SURFACE_COLOR_KEY)
            .ok()
            == Some(CLR_INVALID)
            && env
                .cpu
                .read_u32(address + adv_nt::native::WINDOW_SURFACE_ALPHA_MASK)
                .ok()
                == Some(0)
            && env
                .cpu
                .read_u32(address + adv_nt::native::WINDOW_SURFACE_SHAPE_REGION)
                .ok()
                == Some(0)
            && env
                .cpu
                .read_u32(address + adv_nt::native::WINDOW_SURFACE_SHAPE_BITMAP)
                .ok()
                == Some(0);
        if !plain {
            continue;
        }
        if let Err(error) = write_words(
            env,
            address + adv_nt::native::WINDOW_SURFACE_BOUNDS,
            &EMPTY_BOUNDS,
        ) {
            return Some(Outcome::Fail(format!("{MODULE}: {error}")));
        }
        env.ext
            .get::<adv_nt::native::UserServices>()
            .mark_surface_dirty(address, None);
        env.ext
            .get_mut::<Surfaces>()
            .0
            .get_mut(&address)
            .unwrap()
            .drawn = true;
        if !super::client_geometry(env.ext, hwnd).is_some_and(|(_, visible)| visible) {
            continue;
        }
        match present_visible(env, address, dirty) {
            Ok(Some(outcome)) => return Some(outcome),
            Ok(None) => {}
            Err(error) => return Some(Outcome::Fail(format!("{MODULE}: {error}"))),
        }
    }
    None
}

fn area(rect: [i32; 4]) -> i64 {
    i64::from(rect[2] - rect[0]).max(0) * i64::from(rect[3] - rect[1]).max(0)
}

/// Whether a window can show: a shown, unminimized top-level window that the
/// host does not show natively.
fn shows(ext: &Ext, hwnd: u32) -> bool {
    window(ext, hwnd).is_some_and(|w| w.visible && w.parent == 0 && w.style & WS_MINIMIZE == 0)
        && !super::project::projected(ext, hwnd)
}

/// Whether a child window can show as a layered overlay plane above its root.
fn shows_layered_child(ext: &Ext, hwnd: u32) -> bool {
    let Some(w) = window(ext, hwnd) else {
        return false;
    };
    w.parent != 0
        && w.style & WS_MINIMIZE == 0
        && !super::project::projected(ext, hwnd)
        && super::client_geometry(ext, hwnd).is_some_and(|(_, visible)| visible)
        && drawn_surface(ext, hwnd).is_some_and(|address| ext.get::<Surfaces>().0[&address].layered)
}

/// Whether `child` equals `ancestor` or has `ancestor` in its parent chain.
fn has_ancestor(ext: &Ext, mut child: u32, ancestor: u32) -> bool {
    let mut depth = 0;
    while child != 0 && depth < 64 {
        if child == ancestor {
            return true;
        }
        let Some(w) = window(ext, child) else {
            return false;
        };
        child = w.parent;
        depth += 1;
    }
    false
}

/// Whether changing position or visibility of `hwnd` can affect window planes.
pub(super) fn needs_compose(ext: &Ext, hwnd: u32) -> bool {
    ext.try_get::<Shown>().is_some_and(|shown| {
        shown
            .0
            .iter()
            .any(|&(child, _)| has_ancestor(ext, child, hwnd))
    }) || ext.try_get::<Surfaces>().is_some_and(|surfaces| {
        surfaces
            .0
            .values()
            .any(|s| s.layered && has_ancestor(ext, s.hwnd, hwnd))
    })
}

/// The windows shown as planes, bottom first, with their screen rectangles
/// (WINE-IN-1).
#[derive(Default)]
struct Shown(Vec<(u32, [i32; 4])>);

#[cfg(test)]
pub(super) fn show_plane(ext: &mut Ext, hwnd: u32, rect: [i32; 4]) {
    ext.get_or_default::<Shown>().0.push((hwnd, rect));
}

/// The plane at screen point (`x`, `y`), if any.
pub(super) fn plane_at(ext: &Ext, x: i32, y: i32) -> Option<u32> {
    ext.try_get::<Shown>()?
        .0
        .iter()
        .rev()
        .find(|(_, [l, t, r, b])| (*l..*r).contains(&x) && (*t..*b).contains(&y))
        .map(|&(hwnd, _)| hwnd)
}

/// The screen rectangle of plane `hwnd`, if it shows.
pub(super) fn plane(ext: &Ext, hwnd: u32) -> Option<[i32; 4]> {
    let shown = ext.try_get::<Shown>()?;
    shown
        .0
        .iter()
        .find(|(h, _)| *h == hwnd)
        .map(|&(_, rect)| rect)
}

/// The address of `hwnd`'s surface, if it has been drawn.
fn drawn_surface(ext: &Ext, hwnd: u32) -> Option<u32> {
    ext.get::<Surfaces>()
        .0
        .iter()
        .find(|(_, surface)| surface.hwnd == hwnd && surface.drawn)
        .map(|(&address, _)| address)
}

/// Place every window by one rule (WINE-GFX-10). The base is the largest
/// window that shows and has been drawn, or the current base, which wins a
/// tie; owners and show order say nothing, as toolkits own every window by
/// one hidden application window. A modal window disables the windows it
/// stands over, so while one of them shows, only a disabled window is the
/// base. Every other drawn window that shows in front of the base is a plane. `drawn` is the surface a flush changed and
/// its changed part.
pub(super) fn compose(env: &mut Env<'_>, drawn: Option<(u32, [i32; 4])>) -> Result<(), String> {
    // A presenter may draw into a child window: its top-level window holds the base.
    let current =
        super::input::frame_window(env.ext).map(|(hwnd, _)| super::input::top_level(env.ext, hwnd));
    let source = env.ext.get::<SurfaceStore>().base_source();
    let mut front = super::input::front_window(env.ext, source);
    env.ext.get_or_default::<Surfaces>();
    let ext = &*env.ext;
    let base = ext
        .get::<Surfaces>()
        .0
        .values()
        .filter(|surface| surface.drawn)
        .map(|surface| surface.hwnd)
        .chain(current)
        .filter(|&hwnd| shows(ext, hwnd))
        .max_by_key(|&hwnd| {
            let window = window(ext, hwnd).unwrap();
            let disabled = window.style & WS_DISABLED != 0;
            (disabled, area(window.client), Some(hwnd) == current)
        });
    let Some(base) = base else {
        env.ext.get_or_default::<Shown>().0.clear();
        return Ok(());
    };
    // Only a fresh draw into shared screen content transfers ownership.
    // Plane/geometry recomposition and nonclient dirt keep the current frame.
    if front == Some(base)
        && super::input::front_shares_gdi(env.ext)
        && drawn.is_some_and(|(address, [left, top, right, bottom])| {
            let surface = &env.ext.get::<Surfaces>().0[&address];
            surface.hwnd == base
                && client_part(surface, &window(env.ext, base).unwrap()).is_some_and(
                    |(x, y, w, h)| {
                        i64::from(left).max(i64::from(x)) < i64::from(right).min(i64::from(x + w))
                            && i64::from(top).max(i64::from(y))
                                < i64::from(bottom).min(i64::from(y + h))
                    },
                )
        })
    {
        super::input::front_released(env.ext, source.unwrap());
        front = None;
    }
    // A new base presents whole; the current one presents what its flush changed.
    // A front keeps its frame until a shared-content draw takes over.
    let address = drawn_surface(env.ext, base).filter(|_| Some(base) != front);
    let dirty = match drawn {
        _ if Some(base) != current => Some([0, 0, i32::MAX, i32::MAX]),
        Some((at, dirty)) if Some(at) == address => Some(dirty),
        _ => None,
    };
    let view = match (address, dirty) {
        (Some(address), Some(dirty)) => attach(env, address, dirty)?,
        _ => None,
    };
    let moves = place_planes(env, base, drawn)?;
    let provider = *env.ext.get::<Provider>();
    let memory = (provider.memory)(env.cpu);
    let store = env.ext.get_mut::<SurfaceStore>();
    if let Some((sid, damage)) = view {
        let before = store.stats.presentations;
        store.present_from(env.context, sid, &memory, damage)?;
        if store.stats.presentations != before || Some(base) == current {
            super::input::presented(env.ext);
        }
    }
    env.ext
        .get_mut::<SurfaceStore>()
        .present_planes(env.context, &memory)?;
    for (hwnd, x, y) in moves {
        super::input::schedule(
            env.ext,
            hwnd,
            Box::new(move |env, done| {
                let target = adv_nt::process::module_base(env.cpu, env.sys.peb, "user32.dll")
                    .and_then(|base| adv_nt::process::module_export(env.cpu, base, "SetWindowPos"));
                let Some(target) = target else {
                    return Outcome::Fail("user32: missing SetWindowPos".into());
                };
                let flags = SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE;
                let args = [hwnd, 0, x as u32, y as u32, 0, 0, flags];
                Outcome::call(target, args, move |env, _| done(env))
            }),
        );
    }
    Ok(())
}

/// A window surface's pixels for text extraction (WINE-UNIX-8): a top-down
/// 32-bit plane holding the window's whole extent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowPlane {
    pub hwnd: u32,
    pub bits: u32,
    pub stride: u32,
    pub size: [u32; 2],
    /// The client area's part of the plane, as x, y, width and height.
    pub client: Option<[u32; 4]>,
}

/// The window surfaces, by their pixels' address.
pub fn window_planes(ext: &Ext) -> Vec<WindowPlane> {
    let Some(surfaces) = ext.try_get::<Surfaces>() else {
        return Vec::new();
    };
    let mut planes: Vec<_> = surfaces
        .0
        .values()
        .map(|surface| WindowPlane {
            hwnd: surface.hwnd,
            bits: surface.pixels,
            stride: surface.width * 4,
            size: [surface.width, surface.height],
            client: window(ext, surface.hwnd)
                .and_then(|w| client_part(surface, &w))
                .map(|(x, y, width, height)| [x, y, width, height]),
        })
        .collect();
    planes.sort_unstable_by_key(|plane| plane.bits);
    planes
}

/// The client area's place in its surface: offset and size.
fn client_part(surface: &Surface, window: &super::Window) -> Option<(u32, u32, u32, u32)> {
    let full = (surface.width, surface.height);
    let [left, top, right, bottom] = window.client;
    let x = (left - window.extent[0]).clamp(0, full.0 as i32) as u32;
    let y = (top - window.extent[1]).clamp(0, full.1 as i32) as u32;
    let width = ((right - left).max(0) as u32).min(full.0 - x);
    let height = ((bottom - top).max(0) as u32).min(full.1 - y);
    (width != 0 && height != 0).then_some((x, y, width, height))
}

/// Make the client area of the surface at `address` the base: a store view,
/// with the frame sized to it, and `dirty`, in surface coordinates, as its
/// damage in view coordinates.
fn attach(env: &mut Env<'_>, address: u32, dirty: [i32; 4]) -> Result<Option<(u32, Rect)>, String> {
    let surface = &env.ext.get::<Surfaces>().0[&address];
    let (hwnd, pixels, full, view) = (
        surface.hwnd,
        surface.pixels,
        (surface.width, surface.height),
        surface.view,
    );
    let window = window(env.ext, hwnd).ok_or("presented window is unknown")?;
    let Some((x, y, width, height)) = client_part(surface, &window) else {
        return Ok(None);
    };
    let start = pixels + y * full.0 * 4 + x * 4;
    let [left, top, right, bottom] = dirty;
    let (left, top) = ((left - x as i32).max(0), (top - y as i32).max(0));
    let (right, bottom) = (
        (right - x as i32).min(width as i32),
        (bottom - y as i32).min(height as i32),
    );
    let damage = Rect {
        x: left as u32,
        y: top as u32,
        w: (right - left).max(0) as u32,
        h: (bottom - top).max(0) as u32,
    };
    super::input::prepare(env.ext, hwnd, (window.client[0], window.client[1]));
    let provider = *env.ext.get::<Provider>();
    let mut memory = (provider.memory)(env.cpu);
    let store = env.ext.get_mut::<SurfaceStore>();
    let sid = match view {
        Some((sid, w, h, at)) if (w, h, at) == (width, height, start) => sid,
        _ => {
            if let Some((old, _, _, _)) = view {
                store.delete(old);
            }
            let part = adv_device::surface::Surface {
                pixels: start,
                width,
                height,
                pitch: full.0 * 4,
                bottom_up: false,
                locked: None,
                version: 0,
                format: PixelFormat::Bgra32,
                palette: None,
            };
            let sid = store.import_guest(part, &mut memory)?;
            env.ext
                .get_mut::<Surfaces>()
                .0
                .get_mut(&address)
                .unwrap()
                .view = Some((sid, width, height, start));
            sid
        }
    };
    let store = env.ext.get_mut::<SurfaceStore>();
    store.sync_window_size(width, height)?;
    Ok(Some((sid, damage)))
}

/// Record the planes over `base` and what `drawn` changed in them. Returns
/// the planes outside the frame, with the positions that move
/// them into it, as a window manager keeps windows on screen; the host shows
/// nothing outside the base.
fn place_planes(
    env: &mut Env<'_>,
    base: u32,
    drawn: Option<(u32, [i32; 4])>,
) -> Result<Vec<(u32, i32, i32)>, String> {
    let base_window = window(env.ext, base).ok_or("base window is unknown")?;
    let [base_left, base_top, base_right, base_bottom] = base_window.client;
    let canvas = (
        (base_right - base_left).max(0) as u32,
        (base_bottom - base_top).max(0) as u32,
    );
    let order = env
        .ext
        .try_get::<super::Order>()
        .map_or(&[][..], |order| &order.0[..]);
    let front = order
        .iter()
        .position(|&hwnd| hwnd == base)
        .map_or(order, |at| &order[..at]);
    let mut emit = Vec::new();
    let layered_children = |root: u32, emit: &mut Vec<(u32, bool)>| {
        for &child in order.iter().rev() {
            if child != root
                && super::input::top_level(env.ext, child) == root
                && shows_layered_child(env.ext, child)
            {
                emit.push((child, false));
            }
        }
    };
    layered_children(base, &mut emit);
    for &hwnd in front.iter().rev() {
        if !shows(env.ext, hwnd) {
            continue;
        }
        if drawn_surface(env.ext, hwnd).is_some() {
            emit.push((hwnd, true));
        }
        layered_children(hwnd, &mut emit);
    }
    let (mut planes, mut damage, mut moves, mut shown) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for (hwnd, top_level) in emit {
        let Some(address) = drawn_surface(env.ext, hwnd) else {
            continue;
        };
        let surface = &env.ext.get::<Surfaces>().0[&address];
        let w = window(env.ext, hwnd).unwrap();
        let [ex_l, ex_t, ex_r, ex_b] = w.extent;
        let (width, height) = (ex_r - ex_l, ex_b - ex_t);
        let (left, top) = if top_level {
            (ex_l, ex_t)
        } else if let Some((origin, _)) = super::client_geometry(env.ext, hwnd) {
            (origin.0 + ex_l - w.client[0], origin.1 + ex_t - w.client[1])
        } else {
            continue;
        };
        let (right, bottom) = (left + width, top + height);
        if top_level {
            let x = left.clamp(base_left, (base_right - width).max(base_left));
            let y = top.clamp(base_top, (base_bottom - height).max(base_top));
            if (x, y) != (left, top) {
                moves.push((hwnd, x, y));
            }
        }
        let plane = Plane {
            id: hwnd,
            pixels: surface.pixels,
            pitch: surface.width * 4,
            x: left - base_left,
            y: top - base_top,
            width: (width.max(0) as u32).min(surface.width),
            height: (height.max(0) as u32).min(surface.height),
            layered: surface.layered,
        };
        if let Some((_, [l, t, r, b])) = drawn.filter(|&(at, _)| at == address) {
            let side = |at: i32, from: i32| (from + at).max(0) as u32;
            let (l, t) = (side(l, plane.x), side(t, plane.y));
            let (r, b) = (side(r, plane.x), side(b, plane.y));
            damage.push(Rect {
                x: l,
                y: t,
                w: r.saturating_sub(l),
                h: b.saturating_sub(t),
            });
        }
        shown.push((hwnd, [left, top, right, bottom]));
        planes.push(plane);
    }
    *env.ext.get_or_default::<Shown>() = Shown(shown);
    let provider = *env.ext.get::<Provider>();
    let memory = (provider.memory)(env.cpu);
    env.ext
        .get_mut::<SurfaceStore>()
        .set_planes(planes, &damage, canvas, &memory)?;
    Ok(moves)
}

fn destroy(env: &mut Env<'_>) -> Outcome {
    let address = env.arg(0);
    if let Some(surface) = env.ext.get_or_default::<Surfaces>().0.remove(&address) {
        env.ext
            .get::<adv_nt::native::UserServices>()
            .surface(address, false);
        if let Some((sid, _, _, _)) = surface.view {
            env.ext.get_mut::<SurfaceStore>().delete(sid);
        }
        let _ = env.sys.vm.free(env.cpu, surface.pixels);
    }
    Outcome::Return(0)
}

#[cfg(test)]
mod tests {
    // spec: WINE-GFX-4 WINE-GFX-10 WINE-GFX-11
    #[test]
    fn fresh_base_content_resumes_after_front_presentation() {
        use adv_abi::{Env, Registry};
        use adv_cpu::{
            JitCpu,
            backend::{Guest, Prot},
        };
        use adv_device::surface::{
            MemoryOperations, MemoryView, PixelFormat, Snapshot, SurfaceStore,
        };
        use adv_machine::{Clock, Machine, layout::PARK_ADDR};
        use std::borrow::Cow;
        fn memory(cpu: &mut dyn Guest) -> MemoryView<'_, dyn Guest + '_> {
            MemoryView::new(
                cpu,
                MemoryOperations {
                    read: |cpu, at, len| {
                        let mut bytes = vec![0; len];
                        cpu.read(at, &mut bytes).map_err(|e| e.to_string())?;
                        Ok(Cow::Owned(bytes))
                    },
                    write: |cpu, at, bytes| cpu.write(at, bytes).map_err(|e| e.to_string()),
                    writable: |cpu, at, len| {
                        adv_abi::guest::check_range(cpu, at, len as u32, Prot::WRITE)
                            .map_err(|e| e.to_string())
                    },
                    copy: |cpu, src, dst, len| {
                        cpu.copy_memory(src, dst, len).map_err(|e| e.to_string())
                    },
                },
            )
        }
        let mut m = Machine::new(
            JitCpu::new(PARK_ADDR, false).unwrap(),
            Registry::default(),
            Clock::virtual_time(),
        )
        .unwrap();
        m.cpu.map(0x410000, 4096, Prot::RW).unwrap();
        m.cpu.map(0x500000, 65536, Prot::RW).unwrap();
        m.ext.insert(crate::Provider {
            memory,
            completed: |_, _| None,
        });
        let mut frames = adv_device::surface::PresentedFrames::new();
        let mut store = SurfaceStore::new(4, 2, 0x500000, 65536, Snapshot::default()).unwrap();
        let video = store
            .create(4, 2, false, PixelFormat::Bgra32, &mut memory(&mut m.cpu))
            .unwrap();
        let video_pixels = [10, 20, 30, 255].repeat(8);
        m.cpu.write(store[video].pixels, &video_pixels).unwrap();
        store
            .present(video, &memory(&mut m.cpu), store[video].rect())
            .unwrap();
        frames.update(&mut store);
        m.ext.insert(store);
        let hwnd = 7;
        m.ext.get_or_default::<super::super::Windows>().0.insert(
            hwnd,
            super::super::Window {
                visible: true,
                client: [0, 1, 4, 3],
                extent: [0, 0, 4, 3],
                origin: (0, 1),
                ..Default::default()
            },
        );
        let address = 0x2000;
        m.ext.get_or_default::<super::Surfaces>().0.insert(
            address,
            super::Surface {
                hwnd,
                pixels: 0x410000,
                width: 4,
                height: 3,
                view: None,
                drawn: true,
                layered: false,
            },
        );
        m.ext.get_or_default::<super::super::input::Presentation>();
        super::super::input::shared_front_frame(&m.ext, hwnd, video);
        super::super::input::presented(&m.ext);
        let gdi_pixels = [40, 50, 60, 255].repeat(12);
        m.cpu.write(0x410000, &gdi_pixels).unwrap();
        let mut compose = |m: &mut Machine<JitCpu>, drawn| {
            super::compose(
                &mut Env {
                    context: m.sys.current,
                    cpu: &mut m.cpu,
                    sys: &mut m.sys,
                    ext: &mut m.ext,
                    thunk: 0,
                },
                drawn,
            )
            .unwrap();
            frames.update(m.ext.get_mut::<SurfaceStore>());
        };
        // Geometry-only recomposition and nonclient dirt keep the current video.
        compose(&mut m, None);
        compose(&mut m, Some((address, [0, 1, 0, 1])));
        compose(&mut m, Some((address, [0, 0, 4, 1])));
        assert_eq!(m.ext.get::<SurfaceStore>().presenter.bgra, video_pixels);
        let before = m.ext.get::<SurfaceStore>().stats.presentations;
        compose(&mut m, Some((address, [0, 1, 4, 3])));
        assert!(m.ext.get::<SurfaceStore>().stats.presentations > before);
        assert_eq!(m.ext.get::<SurfaceStore>().presenter.bgra, gdi_pixels[16..]);
        assert_eq!(super::super::input::front_window(&m.ext, Some(video)), None);
        // An exclusive front (such as D3D9) still ignores GDI surface flushes.
        super::super::input::front_frame(&m.ext, hwnd, video);
        let store = m.ext.get_mut::<SurfaceStore>();
        store
            .present(video, &memory(&mut m.cpu), store[video].rect())
            .unwrap();
        compose(&mut m, None);
        super::super::input::shared_front_frame(&m.ext, 0, video + 1);
        compose(&mut m, Some((address, [0, 1, 4, 3])));
        assert_eq!(m.ext.get::<SurfaceStore>().presenter.bgra, video_pixels);
        super::super::input::shared_front_frame(&m.ext, hwnd, video);
        assert_eq!(
            super::super::input::front_window(&m.ext, Some(video)),
            Some(hwnd)
        );
    }

    // spec: WINE-GFX-4 WINE-GFX-10 WINE-GFX-11
    #[test]
    fn gdi_flush_preserves_exclusive_display_mode_and_primary_geometry() {
        use adv_abi::{Env, Registry};
        use adv_cpu::{
            JitCpu,
            backend::{Guest, Prot},
        };
        use adv_device::surface::{
            MemoryOperations, MemoryView, PixelFormat, Snapshot, SurfaceStore,
        };
        use adv_machine::{Clock, Machine, layout::PARK_ADDR};
        use std::borrow::Cow;
        fn memory(cpu: &mut dyn Guest) -> MemoryView<'_, dyn Guest + '_> {
            MemoryView::new(
                cpu,
                MemoryOperations {
                    read: |cpu, at, len| {
                        let mut bytes = vec![0; len];
                        cpu.read(at, &mut bytes).map_err(|e| e.to_string())?;
                        Ok(Cow::Owned(bytes))
                    },
                    write: |cpu, at, bytes| cpu.write(at, bytes).map_err(|e| e.to_string()),
                    writable: |cpu, at, len| {
                        adv_abi::guest::check_range(cpu, at, len as u32, Prot::WRITE)
                            .map_err(|e| e.to_string())
                    },
                    copy: |cpu, src, dst, len| {
                        cpu.copy_memory(src, dst, len).map_err(|e| e.to_string())
                    },
                },
            )
        }
        let mut m = Machine::new(
            JitCpu::new(PARK_ADDR, false).unwrap(),
            Registry::default(),
            Clock::virtual_time(),
        )
        .unwrap();
        m.cpu.map(0x410000, 4096, Prot::RW).unwrap();
        m.cpu.map(0x500000, 65536, Prot::RW).unwrap();
        m.ext.insert(crate::Provider {
            memory,
            completed: |_, _| None,
        });
        let mut store = SurfaceStore::new(4, 2, 0x500000, 65536, Snapshot::default()).unwrap();
        store.set_exclusive_display_mode(true);
        m.ext.insert(store);
        let hwnd = 9;
        m.ext.get_or_default::<super::super::Windows>().0.insert(
            hwnd,
            super::super::Window {
                visible: true,
                client: [0, 0, 16, 8],
                extent: [0, 0, 16, 8],
                origin: (0, 0),
                ..Default::default()
            },
        );
        let address = 0x2200;
        m.ext.get_or_default::<super::Surfaces>().0.insert(
            address,
            super::Surface {
                hwnd,
                pixels: 0x410000,
                width: 16,
                height: 8,
                view: None,
                drawn: true,
                layered: false,
            },
        );
        m.ext.get_or_default::<super::super::input::Presentation>();
        let compose = |m: &mut Machine<JitCpu>, drawn| {
            super::compose(
                &mut Env {
                    context: m.sys.current,
                    cpu: &mut m.cpu,
                    sys: &mut m.sys,
                    ext: &mut m.ext,
                    thunk: 0,
                },
                drawn,
            )
            .unwrap();
        };
        // A GDI flush before primary creation keeps the exclusive display mode size.
        compose(&mut m, Some((address, [0, 0, 16, 8])));
        assert_eq!(
            (
                m.ext.get::<SurfaceStore>().width,
                m.ext.get::<SurfaceStore>().height
            ),
            (4, 2)
        );
        // Once a primary surface exists, GDI flushes never resize it.
        let primary = {
            let mut mem = memory(&mut m.cpu);
            let store = m.ext.get_mut::<SurfaceStore>();
            store.display_format = PixelFormat::Rgb565;
            store.primary(&mut mem).unwrap()
        };
        compose(&mut m, Some((address, [0, 0, 16, 8])));
        let store = m.ext.get::<SurfaceStore>();
        assert_eq!((store.width, store.height), (4, 2));
        assert_eq!((store[primary].width, store[primary].height), (4, 2));
        assert_eq!(store[primary].pitch, 8);
    }

    // spec: WINE-GFX-10
    #[test]
    fn z_order_follows_set_window_pos_placement() {
        let (main, popup, tool) = (0x10020, 0x10024, 0x10028);
        let mut order = super::super::Order::default();
        order.place(main, 0, 0);
        // A new window goes to the front even when its first move keeps the z-order.
        order.place(popup, 0, super::SWP_NOZORDER);
        assert_eq!(order.0, [popup, main]);
        order.place(main, 0, 0);
        assert_eq!(order.0, [main, popup]);
        order.place(main, popup, 0);
        assert_eq!(order.0, [popup, main]);
        order.place(tool, main, 0);
        order.place(popup, 0, super::SWP_NOZORDER);
        assert_eq!(order.0, [popup, main, tool]);
        // HWND_BOTTOM.
        order.place(popup, 1, 0);
        assert_eq!(order.0, [main, tool, popup]);
    }

    // spec: WINE-GFX-4
    #[test]
    fn a_client_view_of_whole_pitches_stays_in_storage() {
        let (width, height) = (1936u32, 1057u32);
        let pitch = width * 4;
        // The client area starts after an 8-pixel frame and a 31-pixel caption.
        let (x, y) = (8, 31);
        let start = y * pitch + x * 4;
        assert!(start + pitch * (height - y) <= super::storage(pitch, height));
    }

    // spec: WINE-GFX-10
    #[test]
    fn reparented_layered_child_visibility_follows_ancestor_and_surface_flag() {
        let (base, child) = (0x10030, 0x10040);
        let mut ext = adv_machine::Ext::default();
        let windows = &mut ext.get_or_default::<super::super::Windows>().0;
        windows.insert(
            base,
            super::super::Window {
                visible: true,
                origin: (10, 40),
                client: [10, 40, 810, 640],
                extent: [0, 0, 810, 640],
                ..Default::default()
            },
        );
        windows.insert(
            child,
            super::super::Window {
                visible: true,
                parent: base,
                client: [0, 0, 800, 600],
                extent: [0, 0, 800, 600],
                ..Default::default()
            },
        );
        ext.get_or_default::<super::Surfaces>().0.insert(
            0x2000,
            super::Surface {
                hwnd: child,
                pixels: 0x3000,
                width: 800,
                height: 600,
                view: None,
                drawn: true,
                layered: true,
            },
        );
        assert!(super::shows(&ext, base));
        assert!(!super::shows(&ext, child));
        assert!(super::shows_layered_child(&ext, child));
        assert_eq!(
            super::super::client_geometry(&ext, child),
            Some(((10, 40), true))
        );
        // Child geometry follows parent movement without a new child report.
        let windows = &mut ext.get_mut::<super::super::Windows>().0;
        windows.get_mut(&base).unwrap().origin = (-20, 80);
        windows.get_mut(&child).unwrap().client = [3, 5, 803, 605];
        assert_eq!(
            super::super::client_geometry(&ext, child),
            Some(((-17, 85), true))
        );
        ext.get_mut::<super::super::Windows>()
            .0
            .get_mut(&base)
            .unwrap()
            .visible = false;
        assert!(!super::shows_layered_child(&ext, child));
    }

    // spec: WINE-GFX-10
    #[test]
    fn intermediate_parent_change_triggers_compose_for_layered_descendant() {
        let (root, mid, child, sibling) = (0x10050, 0x10054, 0x10058, 0x1005c);
        let mut ext = adv_machine::Ext::default();
        let windows = &mut ext.get_or_default::<super::super::Windows>().0;
        for (hwnd, parent) in [(root, 0), (mid, root), (child, mid), (sibling, root)] {
            windows.insert(
                hwnd,
                super::super::Window {
                    visible: true,
                    parent,
                    ..Default::default()
                },
            );
        }
        ext.get_or_default::<super::Surfaces>().0.insert(
            0x2100,
            super::Surface {
                hwnd: child,
                pixels: 0x3100,
                width: 64,
                height: 64,
                view: None,
                drawn: true,
                layered: true,
            },
        );
        assert!(super::needs_compose(&ext, child));
        assert!(super::needs_compose(&ext, mid));
        assert!(super::needs_compose(&ext, root));
        assert!(!super::needs_compose(&ext, sibling));
    }

    // spec: WINE-GFX-7
    #[test]
    fn flush_on_wait_publishes_plain_dirty_window_surface_for_writer_only() {
        use adv_abi::Registry;
        use adv_cpu::{
            JitCpu,
            backend::{Guest, GuestExt, Prot},
        };
        use adv_device::surface::{MemoryOperations, MemoryView, Snapshot, SurfaceStore};
        use adv_machine::{Clock, Machine, layout::PARK_ADDR};
        use std::borrow::Cow;
        fn memory(cpu: &mut dyn Guest) -> MemoryView<'_, dyn Guest + '_> {
            MemoryView::new(
                cpu,
                MemoryOperations {
                    read: |cpu, at, len| {
                        let mut bytes = vec![0; len];
                        cpu.read(at, &mut bytes).map_err(|e| e.to_string())?;
                        Ok(Cow::Owned(bytes))
                    },
                    write: |cpu, at, bytes| cpu.write(at, bytes).map_err(|e| e.to_string()),
                    writable: |cpu, at, len| {
                        adv_abi::guest::check_range(cpu, at, len as u32, Prot::WRITE)
                            .map_err(|e| e.to_string())
                    },
                    copy: |cpu, src, dst, len| {
                        cpu.copy_memory(src, dst, len).map_err(|e| e.to_string())
                    },
                },
            )
        }
        let mut m = Machine::new(
            JitCpu::new(PARK_ADDR, false).unwrap(),
            Registry::default(),
            Clock::virtual_time(),
        )
        .unwrap();
        let (hwnd, pixels, address) = (0x10070, 0x410000, 0x420000);
        m.cpu.map(pixels, 4096, Prot::RW).unwrap();
        m.cpu.map(address, 4096, Prot::RW).unwrap();
        m.cpu.map(0x500000, 65536, Prot::RW).unwrap();
        let red: Vec<u8> = [0x00, 0x00, 0xff, 0xff].repeat(64 * 4);
        m.cpu.write(pixels, &red).unwrap();
        let write_surface =
            |cpu: &mut JitCpu, bounds: [i32; 4], color_key: u32, alpha_mask: u32| {
                let rect = [0i32, 0, 64, 4];
                for (i, v) in rect.into_iter().enumerate() {
                    cpu.write_u32(
                        address + adv_nt::native::WINDOW_SURFACE_RECT + 4 * i as u32,
                        v as u32,
                    )
                    .unwrap();
                }
                for (i, v) in bounds.into_iter().enumerate() {
                    cpu.write_u32(
                        address + adv_nt::native::WINDOW_SURFACE_BOUNDS + 4 * i as u32,
                        v as u32,
                    )
                    .unwrap();
                }
                cpu.write_u32(
                    address + adv_nt::native::WINDOW_SURFACE_COLOR_KEY,
                    color_key,
                )
                .unwrap();
                cpu.write_u32(
                    address + adv_nt::native::WINDOW_SURFACE_ALPHA_MASK,
                    alpha_mask,
                )
                .unwrap();
                cpu.write_u32(address + adv_nt::native::WINDOW_SURFACE_SHAPE_REGION, 0)
                    .unwrap();
                cpu.write_u32(address + adv_nt::native::WINDOW_SURFACE_SHAPE_BITMAP, 0)
                    .unwrap();
            };
        m.ext.insert(crate::Provider {
            memory,
            completed: |_, _| None,
        });
        let store = SurfaceStore::new(64, 4, 0x500000, 65536, Snapshot::default()).unwrap();
        m.ext.insert(store);
        m.ext.get_or_default::<super::super::input::Presentation>();
        m.ext
            .get_or_default::<adv_nt::native::UserServices>()
            .surface(address, true);
        m.ext.get_or_default::<super::super::Windows>().0.insert(
            hwnd,
            super::super::Window {
                visible: true,
                origin: (0, 0),
                client: [0, 0, 64, 4],
                extent: [0, 0, 64, 4],
                ..Default::default()
            },
        );
        m.ext.get_or_default::<super::Surfaces>().0.insert(
            address,
            super::Surface {
                hwnd,
                pixels,
                width: 64,
                height: 4,
                view: None,
                drawn: false,
                layered: false,
            },
        );
        let writer = m.spawn("writer", pixels, &[], 4096).unwrap();
        let other = m.spawn("other", pixels, &[], 4096).unwrap();
        let run_wait = |m: &mut Machine<JitCpu>, context: u32| {
            m.visit_context(Some(context), super::flush_on_wait)
                .unwrap()
        };

        // Non-writer wait must not flush before the 50 ms overdue threshold.
        write_surface(&mut m.cpu, [2, 1, 10, 3], super::CLR_INVALID, 0);
        m.ext
            .get::<adv_nt::native::UserServices>()
            .mark_surface_dirty(address, Some(writer));
        assert!(run_wait(&mut m, other).is_none());
        assert_eq!(m.ext.get::<SurfaceStore>().stats.presentations, 0);
        assert!(
            m.ext
                .get::<adv_nt::native::UserServices>()
                .has_dirty_surfaces()
        );

        // Color-keyed surfaces are left for Wine's window_surface_flush.
        write_surface(&mut m.cpu, [2, 1, 10, 3], 0x00ff00, 0);
        assert!(run_wait(&mut m, writer).is_none());
        assert_eq!(m.ext.get::<SurfaceStore>().stats.presentations, 0);
        assert_eq!(
            m.cpu
                .read_u32(address + adv_nt::native::WINDOW_SURFACE_BOUNDS)
                .unwrap(),
            2
        );

        // Writer wait flushes a plain surface and clears Wine's bounds to empty.
        write_surface(&mut m.cpu, [2, 1, 10, 3], super::CLR_INVALID, 0);
        assert!(run_wait(&mut m, writer).is_none());
        assert_eq!(m.ext.get::<SurfaceStore>().stats.presentations, 1);
        assert!(
            !m.ext
                .get::<adv_nt::native::UserServices>()
                .has_dirty_surfaces()
        );
        for (i, expected) in super::EMPTY_BOUNDS.into_iter().enumerate() {
            assert_eq!(
                m.cpu
                    .read_u32(address + adv_nt::native::WINDOW_SURFACE_BOUNDS + 4 * i as u32)
                    .unwrap(),
                expected
            );
        }
    }
}
