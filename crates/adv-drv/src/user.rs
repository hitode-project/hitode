//! The minimal Wine user driver. win32u reports where each window is, and the
//! assembly answers window geometry from those reports, so a guest window is
//! associated with what the host presents.
//!
//! Wine loads the driver by the name in the display device's
//! `GraphicsDriver` value. Its DllMain hands win32u the driver's functions;
//! unset ones keep Wine's null driver behavior. Without explorer's process,
//! the driver also starts the desktop as explorer would.
use std::collections::HashMap;

use adv_abi::dll::ModuleImage;
use adv_abi::{Conv, Env, Outcome, Registry};
use adv_cpu::backend::{Backend, GuestExt, Prot};
use adv_machine::Machine;
use adv_machine::layout::PAGE;
use adv_nt::process::{Started, module_base, module_export};
use adv_unix::loader::Libraries;

mod display;
pub mod input;
pub mod project;
mod surface;
pub use display::DESKTOP;

/// The driver's file name, the display device's `GraphicsDriver`.
pub const MODULE: &str = "wineadv.drv";
/// `WINE_GDI_DRIVER_VERSION` of the vendored Wine.
const VERSION: u32 = 108;
/// Offsets in `struct user_driver_funcs` (i386), which is 604 bytes.
const FUNCS: [(u32, &str); 7] = [
    (468, "UpdateDisplayDevices"),
    (476, "CreateWindow"),
    (484, "DestroyWindow"),
    (496, "ProcessEvents"),
    (512, "SetDesktopWindow"),
    (572, "CreateWindowSurface"),
    (580, "WindowPosChanged"),
];
/// Where `struct window_surface_funcs` follows in the same page.
const SURFACE_FUNCS: u32 = 1024;
/// Rectangles in `struct window_rects`: window, client, visible.
const CLIENT_RECT: u32 = 16;
const VISIBLE_RECT: u32 = 32;
const SWP_SHOWWINDOW: u32 = 0x40;
const SWP_HIDEWINDOW: u32 = 0x80;
const DESKTOP_CLASS_ATOM: u32 = 32769;
/// WS_POPUP, WS_CLIPSIBLINGS and WS_CLIPCHILDREN, as explorer creates them.
const DESKTOP_STYLE: u32 = 0x8600_0000;
/// `NtUserCallTwoParam_GetVirtualScreenRect`, with `MDT_DEFAULT` (0).
const GET_VIRTUAL_SCREEN_RECT: u32 = 8;

/// What win32u last reported about a window.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Window {
    /// Left, top, right, bottom, in the parent's client coordinates.
    pub client: [i32; 4],
    /// The part of the window its surface holds, in the same coordinates.
    pub extent: [i32; 4],
    pub visible: bool,
    /// Client origin on screen for top-level windows.
    pub origin: (i32, i32),
    parent: u32,
    /// The guest context that owns the window.
    context: adv_machine::CtxId,
}

/// The windows win32u reported, by handle.
#[derive(Default)]
struct Windows(HashMap<u32, Window>);

/// The guest copy of the driver's function table.
struct Funcs(u32);

pub(super) fn register(registry: &mut Registry) {
    surface::register(registry);
    input::register(registry);
    display::register(registry);
    registry.add(MODULE, "DllMain", Conv::Stdcall { argc: 3 }, attach);
    registry.add(
        MODULE,
        "CreateWindow",
        Conv::Cdecl { argc: 1 },
        |env| match input::init(env) {
            Ok(()) => {
                let hwnd = env.arg(0);
                env.ext
                    .get_or_default::<Windows>()
                    .0
                    .entry(hwnd)
                    .or_default()
                    .context = env.context;
                Outcome::Return(1)
            }
            Err(error) => Outcome::Fail(error),
        },
    );
    registry.add(MODULE, "DestroyWindow", Conv::Cdecl { argc: 1 }, |env| {
        let hwnd = env.arg(0);
        env.ext.get_or_default::<Windows>().0.remove(&hwnd);
        project::forget(env.ext, hwnd);
        Outcome::Return(0)
    });
    registry.add(
        MODULE,
        "SetDesktopWindow",
        Conv::Cdecl { argc: 1 },
        set_desktop_window,
    );
    registry.add(
        MODULE,
        "WindowPosChanged",
        Conv::Cdecl { argc: 6 },
        position_changed,
    );
}

/// Write the function table and build the driver's image.
pub(super) fn image<B: Backend>(machine: &mut Machine<B>) -> Result<ModuleImage, String> {
    let funcs = machine
        .sys
        .vm
        .alloc(&mut machine.cpu, PAGE, Prot::RW)
        .map_err(|e| e.to_string())?;
    for (offset, name) in FUNCS {
        let entry = machine
            .sys
            .thunks
            .publish(&mut machine.cpu, MODULE, name)
            .map_err(|e| e.to_string())?;
        machine
            .cpu
            .write_u32(funcs + offset, entry)
            .map_err(|e| e.to_string())?;
    }
    for (index, name) in surface::FUNCS.into_iter().enumerate() {
        let entry = machine
            .sys
            .thunks
            .publish(&mut machine.cpu, MODULE, name)
            .map_err(|e| e.to_string())?;
        machine
            .cpu
            .write_u32(funcs + SURFACE_FUNCS + 4 * index as u32, entry)
            .map_err(|e| e.to_string())?;
    }
    machine.ext.insert(Funcs(funcs));
    machine.ext.insert(input::Presentation::default());
    display::install(machine)?;
    ModuleImage::build_with_entry(machine, MODULE, Some("DllMain"), &[], &[])
}

/// DllMain: on process attach, give win32u the function table.
fn attach(env: &mut Env<'_>) -> Outcome {
    const DLL_PROCESS_ATTACH: u32 = 1;
    if env.arg(1) != DLL_PROCESS_ATTACH {
        return Outcome::Return(1);
    }
    // Wine's drivers make this call from their Unix side; here win32u.so is
    // guest code like the image, so the call goes to it directly.
    let set = match win32u(env, "__wine_set_user_driver") {
        Ok(set) => set,
        Err(error) => return Outcome::Fail(error),
    };
    let funcs = env.ext.get::<Funcs>().0;
    Outcome::call_aligned(env, set, [funcs, VERSION], |_, _| Outcome::Return(1))
}

/// The address of win32u.so's export `name`, which the driver calls as its
/// Unix side would.
fn win32u(env: &Env<'_>, name: &str) -> Result<u32, String> {
    env.ext
        .try_get::<Libraries>()
        .and_then(|libraries| {
            libraries
                .0
                .iter()
                .find(|library| library.path.ends_with("win32u.so"))?
                .symbol(name)
        })
        .ok_or_else(|| format!("{MODULE}: win32u.so does not export {name}"))
}

/// The guest `struct window_surface_funcs` of the driver's surfaces.
fn surface_funcs(env: &Env<'_>) -> u32 {
    env.ext.get::<Funcs>().0 + SURFACE_FUNCS
}

/// What win32u last reported about `hwnd`.
pub fn window(ext: &adv_machine::Ext, hwnd: u32) -> Option<Window> {
    ext.try_get::<Windows>()?.0.get(&hwnd).copied()
}

/// Every window win32u reported, with the guest thread that owns it.
fn windows(ext: &adv_machine::Ext) -> Vec<(u32, adv_machine::CtxId)> {
    let mut windows: Vec<_> = ext
        .try_get::<Windows>()
        .into_iter()
        .flat_map(|windows| windows.0.iter())
        .map(|(&hwnd, window)| (hwnd, window.context))
        .collect();
    windows.sort_unstable();
    windows
}

/// The desktop window, which presents nothing itself.
struct Desktop(u32);

/// SetDesktopWindow(hwnd).
fn set_desktop_window(env: &mut Env<'_>) -> Outcome {
    let hwnd = env.arg(0);
    env.ext.insert(Desktop(hwnd));
    Outcome::Return(0)
}

fn is_desktop(env: &Env<'_>, hwnd: u32) -> bool {
    env.ext
        .try_get::<Desktop>()
        .is_some_and(|desktop| desktop.0 == hwnd)
}

/// Explorer's desktop startup, on the thread that asked for the desktop:
/// create the desktop and message windows, then show the desktop over the
/// virtual screen. The thread owns them as explorer's desktop thread would.
pub(super) fn start_desktop(env: &mut Env<'_>, then: Started) -> Outcome {
    let exports = module_base(env.cpu, env.sys.peb, "user32.dll").and_then(|base| {
        let export = |name| module_export(env.cpu, base, name);
        Some((export("CreateWindowExW")?, export("SetWindowPos")?))
    });
    let (Some((create, position)), Ok(measure)) = (exports, win32u(env, "NtUserCallTwoParam"))
    else {
        return Outcome::Fail(format!("{MODULE}: user32.dll and win32u.so are needed"));
    };
    let scratch = match crate::slot(env) {
        Ok(scratch) => scratch,
        Err(error) => return Outcome::Fail(error),
    };
    let message: Vec<u8> = "Message\0"
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    if let Err(error) = env.cpu.write(scratch, &message) {
        return Outcome::Fail(format!("{MODULE}: {error}"));
    }
    let window = |class, size| [0, class, 0, DESKTOP_STYLE, 0, 0, size, size, 0, 0, 0, 0];
    Outcome::call(create, window(DESKTOP_CLASS_ATOM, 0), move |_, desktop| {
        let desktop = desktop.value();
        Outcome::call(create, window(scratch, 100), move |env, _| {
            Outcome::call_aligned(
                env,
                measure,
                [scratch, 0, GET_VIRTUAL_SCREEN_RECT],
                move |env, _| {
                    let rect = read_rect(env, scratch);
                    crate::free_slot(env, scratch);
                    let [left, top, right, bottom] = match rect {
                        Ok(rect) => rect,
                        Err(error) => return Outcome::Fail(error),
                    };
                    let (width, height) = ((right - left) as u32, (bottom - top) as u32);
                    let args = [
                        desktop,
                        0,
                        left as u32,
                        top as u32,
                        width,
                        height,
                        SWP_SHOWWINDOW,
                    ];
                    Outcome::call(position, args, move |env, _| then(env))
                },
            )
        })
    })
}

fn read_rect(env: &Env<'_>, at: u32) -> Result<[i32; 4], String> {
    let mut sides = [0; 4];
    for (index, side) in sides.iter_mut().enumerate() {
        *side = env
            .cpu
            .read_u32(at + 4 * index as u32)
            .map_err(|e| format!("{MODULE}: {e}"))? as i32;
    }
    Ok(sides)
}

/// WindowPosChanged(hwnd, insert_after, owner_hint, swp_flags, rects, surface).
fn position_changed(env: &mut Env<'_>) -> Outcome {
    let (hwnd, flags, rects) = (env.arg(0), env.arg(3), env.arg(4));
    if is_desktop(env, hwnd) {
        return Outcome::Return(0);
    }
    let (client, extent) = match (
        read_rect(env, rects + CLIENT_RECT),
        read_rect(env, rects + VISIBLE_RECT),
    ) {
        (Ok(client), Ok(extent)) => (client, extent),
        (Err(error), _) | (_, Err(error)) => return Outcome::Fail(error),
    };
    let window = env
        .ext
        .get_or_default::<Windows>()
        .0
        .entry(hwnd)
        .or_default();
    window.client = client;
    window.extent = extent;
    if flags & SWP_SHOWWINDOW != 0 {
        window.visible = true;
    }
    if flags & SWP_HIDEWINDOW != 0 {
        window.visible = false;
    }
    let user32 = adv_nt::process::module_base(env.cpu, env.sys.peb, "user32.dll");
    let target = |name| user32.and_then(|base| adv_nt::process::module_export(env.cpu, base, name));
    let (Some(origin), Some(parent)) = (target("ClientToScreen"), target("GetAncestor")) else {
        return Outcome::Fail("user driver: missing window geometry exports".into());
    };
    let scratch = match crate::slot(env) {
        Ok(value) => value,
        Err(error) => return Outcome::Fail(error),
    };
    if let Err(error) = crate::write_words(env, scratch, &[0, 0]) {
        return Outcome::Fail(error);
    }
    Outcome::call(origin, [hwnd, scratch], move |env, _| {
        let point = (
            env.cpu.read_u32(scratch).unwrap_or(0) as i32,
            env.cpu.read_u32(scratch + 4).unwrap_or(0) as i32,
        );
        crate::free_slot(env, scratch);
        Outcome::call(parent, [hwnd, 1], move |env, result| {
            let parent = result.value();
            let parent = if is_desktop(env, parent) { 0 } else { parent };
            let window = env.ext.get_mut::<Windows>().0.get_mut(&hwnd).unwrap();
            window.origin = point;
            window.parent = parent;
            if flags & SWP_SHOWWINDOW != 0 {
                project::shown(env, hwnd)
            } else {
                Outcome::Return(0)
            }
        })
    })
}

/// Client origin and inherited visibility from the driver's window reports.
pub fn client_geometry(ext: &adv_machine::Ext, hwnd: u32) -> Option<((i32, i32), bool)> {
    let window = window(ext, hwnd)?;
    if window.parent == 0 {
        return Some((window.origin, window.visible));
    }
    let (origin, visible) = client_geometry(ext, window.parent)?;
    Some((
        (origin.0 + window.client[0], origin.1 + window.client[1]),
        visible && window.visible,
    ))
}
/// Post a host service notification through the window owner's Wine queue.
pub fn post_message(ext: &mut adv_machine::Ext, args: [u32; 4]) {
    input::schedule(
        ext,
        args[0],
        Box::new(move |env, done| {
            let target = adv_nt::process::module_base(env.cpu, env.sys.peb, "user32.dll")
                .and_then(|base| adv_nt::process::module_export(env.cpu, base, "PostMessageW"));
            match target {
                Some(target) => Outcome::call(target, args, move |env, _| done(env)),
                None => Outcome::Fail("user32: missing PostMessageW".into()),
            }
        }),
    );
}
