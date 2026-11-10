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
    let previous = check!(env.cpu.read_u32(output));
    if previous != 0 && env.cpu.read_u32(previous).ok() == Some(surface_funcs(env)) {
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
        return wrap(env, hwnd, bounds, output, width, height);
    }
    call(env, "window_surface_release", [previous], move |env, _| {
        check!(env.cpu.write_u32(output, 0));
        wrap(env, hwnd, bounds, output, width, height)
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

/// flush(surface, rect, dirty, color_info, color_bits, shape_changed, shape_info, shape_bits).
fn flush(env: &mut Env<'_>) -> Outcome {
    let (address, dirty) = (env.arg(0), env.arg(2));
    let Some(surface) = env.ext.get_or_default::<Surfaces>().0.get_mut(&address) else {
        return Outcome::Return(0);
    };
    surface.drawn = true;
    let hwnd = surface.hwnd;
    if !window(env.ext, hwnd).is_some_and(|window| window.visible) {
        return Outcome::Return(1);
    }
    let dirty = check!(rect(env, dirty));
    let before = env.ext.get::<SurfaceStore>().stats.presentations;
    check!(compose(env, Some((address, dirty))));
    let presented = env.ext.get::<SurfaceStore>().stats.presentations;
    if presented != before {
        env.sys.poll_work();
        let provider = *env.ext.get::<Provider>();
        if let Some(outcome) = (provider.completed)(env, presented) {
            return outcome;
        }
    }
    Outcome::Return(1)
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
    let front = super::input::front_window(env.ext);
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
    // A new base presents whole; the current one presents what its flush changed.
    // A front's window shows the front's frames, never its window surface.
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
    if (store.width, store.height) != (width, height) {
        store.configure_window(width, height, &mut memory)?;
    }
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
    let (mut planes, mut damage, mut moves, mut shown) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for &hwnd in front.iter().rev() {
        if !shows(env.ext, hwnd) {
            continue;
        }
        let Some(address) = drawn_surface(env.ext, hwnd) else {
            continue;
        };
        let surface = &env.ext.get::<Surfaces>().0[&address];
        let [left, top, right, bottom] = window(env.ext, hwnd).unwrap().extent;
        let (width, height) = (right - left, bottom - top);
        let x = left.clamp(base_left, (base_right - width).max(base_left));
        let y = top.clamp(base_top, (base_bottom - height).max(base_top));
        if (x, y) != (left, top) {
            moves.push((hwnd, x, y));
        }
        let plane = Plane {
            id: hwnd,
            pixels: surface.pixels,
            pitch: surface.width * 4,
            x: left - base_left,
            y: top - base_top,
            width: (width.max(0) as u32).min(surface.width),
            height: (height.max(0) as u32).min(surface.height),
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
}
