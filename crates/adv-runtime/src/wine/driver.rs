//! The minimal Wine user driver. win32u reports where each window is, and the
//! assembly answers window geometry from those reports, so a guest window is
//! associated with what the host presents.
//!
//! Wine loads the driver by the name in the display device's
//! `GraphicsDriver` value. Its DllMain hands win32u the driver's functions;
//! unset ones keep Wine's null driver behavior.
use std::collections::HashMap;

use adv_abi::dll::ModuleImage;
use adv_abi::{Conv, Env, Outcome, Registry};
use adv_cpu::backend::{Backend, GuestExt, Prot};
use adv_machine::Machine;
use adv_machine::layout::PAGE;
use adv_unix::loader::Libraries;

/// The driver's file name, the display device's `GraphicsDriver`.
pub const MODULE: &str = "wineadv.drv";
/// `WINE_GDI_DRIVER_VERSION` of the vendored Wine.
const VERSION: u32 = 108;
/// Offsets in `struct user_driver_funcs` (i386), which is 604 bytes.
const FUNCS: [(u32, &str); 3] = [
    (476, "CreateWindow"),
    (484, "DestroyWindow"),
    (580, "WindowPosChanged"),
];
/// The client rectangle in `struct window_rects` (window, client, visible).
const CLIENT_RECT: u32 = 16;
const SWP_SHOWWINDOW: u32 = 0x40;
const SWP_HIDEWINDOW: u32 = 0x80;

/// What win32u last reported about a window.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Window {
    /// Left, top, right, bottom, in the parent's client coordinates.
    pub client: [i32; 4],
    pub visible: bool,
}

/// The windows win32u reported, by handle.
#[derive(Default)]
pub struct Windows(pub HashMap<u32, Window>);

/// The guest copy of the driver's function table.
struct Funcs(u32);

pub(super) fn register(registry: &mut Registry) {
    registry.add(MODULE, "DllMain", Conv::Stdcall { argc: 3 }, attach);
    registry.add(MODULE, "CreateWindow", Conv::Cdecl { argc: 1 }, |_| {
        Outcome::Return(1)
    });
    registry.add(MODULE, "DestroyWindow", Conv::Cdecl { argc: 1 }, |env| {
        let hwnd = env.arg(0);
        env.ext.get_or_default::<Windows>().0.remove(&hwnd);
        Outcome::Return(0)
    });
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
    machine.ext.insert(Funcs(funcs));
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
    let Some(set) = env.ext.try_get::<Libraries>().and_then(|libraries| {
        libraries
            .0
            .iter()
            .find(|library| library.path.ends_with("win32u.so"))?
            .symbol("__wine_set_user_driver")
    }) else {
        return Outcome::Fail(format!("{MODULE}: win32u.so is not loaded"));
    };
    let funcs = env.ext.get::<Funcs>().0;
    Outcome::call(set, [funcs, VERSION], |_, _| Outcome::Return(1))
}

/// WindowPosChanged(hwnd, insert_after, owner_hint, swp_flags, rects, surface).
fn position_changed(env: &mut Env<'_>) -> Outcome {
    let (hwnd, flags, rects) = (env.arg(0), env.arg(3), env.arg(4));
    let mut client = [0; 4];
    for (index, side) in client.iter_mut().enumerate() {
        match env.cpu.read_u32(rects + CLIENT_RECT + 4 * index as u32) {
            Ok(value) => *side = value as i32,
            Err(error) => return Outcome::Fail(format!("{MODULE}: {error}")),
        }
    }
    let window = env
        .ext
        .get_or_default::<Windows>()
        .0
        .entry(hwnd)
        .or_default();
    window.client = client;
    if flags & SWP_SHOWWINDOW != 0 {
        window.visible = true;
    }
    if flags & SWP_HIDEWINDOW != 0 {
        window.visible = false;
    }
    Outcome::Return(0)
}

/// The geometry DirectDraw sees for `hwnd`.
pub(super) fn geometry(
    ext: &adv_machine::Ext,
    _: &adv_machine::Objects,
    hwnd: u32,
) -> Option<adv_dx::ddraw::WindowGeometry> {
    let [left, top, right, bottom] = ext.try_get::<Windows>()?.0.get(&hwnd)?.client;
    let (width, height) = (right.checked_sub(left)?, bottom.checked_sub(top)?);
    (width > 0 && height > 0).then_some(adv_dx::ddraw::WindowGeometry {
        width: width as u32,
        height: height as u32,
        origin: (left, top),
    })
}
