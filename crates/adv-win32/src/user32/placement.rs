//! WINDOWPLACEMENT uses the existing USER object and virtual desktop geometry.
use adv_abi::{Env, GuestPtr, Handle, Registry, win32};
use adv_cpu::backend::{GuestExt, Prot};
use adv_machine::Outcome;

use super::{
    user,
    window::{
        Rect, SW_MAXIMIZE, SW_MINIMIZE, SW_RESTORE, SW_SHOWMINIMIZED, SW_SHOWMINNOACTIVE,
        SW_SHOWNOACTIVATE, SW_SHOWNORMAL, WS_CAPTION, WS_DLGFRAME, WS_MAXIMIZE, WS_MINIMIZE,
        Window, nc_insets,
    },
};
use crate::util::set_last_error;

const WINDOWPLACEMENT_SIZE: u32 = 44;

/// Kept inside `Window`, so destroying its USER object retires all placement state.
#[derive(Clone, Copy)]
pub struct PlacementState {
    normal_rect: Rect,
    min_position: (i32, i32),
    restore_to_maximized: bool,
}

impl Default for PlacementState {
    fn default() -> Self {
        Self::new(Rect::default())
    }
}

impl PlacementState {
    pub fn new(rect: Rect) -> Self {
        Self {
            normal_rect: rect,
            min_position: (-1, -1),
            restore_to_maximized: false,
        }
    }
}

pub(super) fn position_changed(window: &mut Window) {
    if window.style & WS_MINIMIZE != 0 {
        window.placement.min_position = (window.rect.left, window.rect.top);
    } else if window.style & WS_MAXIMIZE == 0 {
        window.placement.normal_rect = window.rect;
    }
}

/// Returns the SIZE_* kind when ShowWindow changes the minimized/maximized state.
/// Notification delivery and activation remain in the existing ShowWindow path.
pub(super) fn show_state(
    window: &mut Window,
    command: u32,
    screen: (i32, i32),
    minimize_area: (i32, i32),
) -> Option<u32> {
    let minimized = window.style & WS_MINIMIZE != 0;
    let maximized = window.style & WS_MAXIMIZE != 0;
    match command {
        SW_MAXIMIZE if !maximized => {
            if !minimized {
                position_changed(window);
            }
            window.placement.restore_to_maximized = true;
            window.style = (window.style | WS_MAXIMIZE) & !WS_MINIMIZE;
            window.rect = Rect::new(0, 0, screen.0, screen.1 - 40);
            Some(2)
        }
        SW_MINIMIZE | SW_SHOWMINIMIZED | SW_SHOWMINNOACTIVE => {
            if !minimized {
                position_changed(window);
                window.placement.restore_to_maximized = maximized;
            }
            const WS_EX_TOOLWINDOW: u32 = 0x80;
            const WS_EX_APPWINDOW: u32 = 0x40000;
            let taskbar = window.parent == 0
                && (window.ex_style & WS_EX_APPWINDOW != 0
                    || (window.ex_style & WS_EX_TOOLWINDOW == 0 && window.owner == 0));
            if taskbar {
                window.placement.min_position = (-32000, -32000);
            } else if !minimized {
                // A non-taskbar window has a minimized caption in its parent's
                // client area (or the desktop work area), using our NC metrics.
                let (_, top, _, bottom) = nc_insets(WS_CAPTION | WS_DLGFRAME, 0);
                window.placement.min_position = (0, (minimize_area.1 - top - bottom).max(0));
            }
            window.style = (window.style | WS_MINIMIZE) & !WS_MAXIMIZE;
            Some(1)
        }
        SW_RESTORE | SW_SHOWNORMAL if minimized || maximized => {
            let restore_maximized =
                command == SW_RESTORE && minimized && window.placement.restore_to_maximized;
            window.style &= !(WS_MINIMIZE | WS_MAXIMIZE);
            if restore_maximized {
                window.style |= WS_MAXIMIZE;
                window.rect = Rect::new(0, 0, screen.0, screen.1 - 40);
                Some(2)
            } else {
                window.rect = window.placement.normal_rect;
                Some(0)
            }
        }
        SW_SHOWNOACTIVATE if minimized => {
            window.style &= !(WS_MINIMIZE | WS_MAXIMIZE);
            if window.placement.restore_to_maximized {
                window.style |= WS_MAXIMIZE;
                window.rect = Rect::new(0, 0, screen.0, screen.1 - 40);
                Some(2)
            } else {
                window.rect = window.placement.normal_rect;
                Some(0)
            }
        }
        _ => None,
    }
}

#[win32(user32, name = "GetWindowPlacement", level = state)]
fn get_window_placement(env: &mut Env<'_>, hwnd: Handle, output: GuestPtr<()>) -> i32 {
    let Some(window) = user(env).windows.get(&env.sys.objects, &hwnd.0) else {
        set_last_error(env, 1400); // ERROR_INVALID_WINDOW_HANDLE takes precedence.
        return 0;
    };
    if adv_abi::guest::check_range(env.cpu, output.addr(), WINDOWPLACEMENT_SIZE, Prot::WRITE)
        .is_err()
    {
        set_last_error(env, 998); // ERROR_NOACCESS
        return 0;
    }
    let show = if window.style & WS_MINIMIZE != 0 {
        SW_SHOWMINIMIZED
    } else if window.style & WS_MAXIMIZE != 0 {
        SW_MAXIMIZE
    } else {
        SW_SHOWNORMAL
    };
    let normal = if window.style & (WS_MINIMIZE | WS_MAXIMIZE) == 0 {
        window.rect
    } else {
        window.placement.normal_rect
    };
    // The single virtual monitor has workspace origin (0,0), with a bottom
    // taskbar. Thus top-level workspace and tool-window screen coordinates
    // coincide. Native child placement retains parent-client coordinates,
    // matching Window::rect; GetWindowRect performs their screen conversion.
    let fields = [
        WINDOWPLACEMENT_SIZE,
        if window.placement.restore_to_maximized {
            2
        } else {
            0
        },
        show,
        window.placement.min_position.0 as u32,
        window.placement.min_position.1 as u32,
        u32::MAX,
        u32::MAX,
        normal.left as u32,
        normal.top as u32,
        normal.right as u32,
        normal.bottom as u32,
    ];
    let mut bytes = [0; WINDOWPLACEMENT_SIZE as usize];
    for (slot, field) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(fields) {
        *slot = field.to_le_bytes();
    }
    // SDK-authored Windows probes accept every input length and overwrite it
    // with 44. Do not inspect an input member or require read permission.
    if env.cpu.write(output.addr(), &bytes).is_err() {
        set_last_error(env, 998);
        return 0;
    }
    1
}

pub(super) fn register(registry: &mut Registry) {
    registry.register(get_window_placement_ABI);
    registry.register(set_window_placement_ABI);
}

#[win32(user32, name = "SetWindowPlacement", level = state)]
fn set_window_placement(env: &mut Env<'_>, hwnd: Handle, input: GuestPtr<()>) -> Outcome {
    let Some(window) = user(env).windows.get(&env.sys.objects, &hwnd.0) else {
        set_last_error(env, 1400);
        return Outcome::Return(0);
    };
    let thread = window.thread;
    let normal_now = window.style & (WS_MINIMIZE | WS_MAXIMIZE) == 0;
    let tool = window.ex_style & 0x80 != 0;
    if adv_abi::guest::check_range(env.cpu, input.addr(), WINDOWPLACEMENT_SIZE, Prot::READ).is_err()
    {
        set_last_error(env, 998);
        return Outcome::Return(0);
    }
    let fields: [u32; 11] =
        std::array::from_fn(|i| env.cpu.read_u32(input.addr() + i as u32 * 4).unwrap());
    if fields[0] != WINDOWPLACEMENT_SIZE {
        set_last_error(env, 87);
        return Outcome::Return(0);
    }
    if fields[1] & 4 != 0 && thread != env.context {
        adv_abi::warn_unavailable(
            env,
            "SetWindowPlacement",
            "asynchronous placement across input queues is unsupported",
        );
        set_last_error(env, 50);
        return Outcome::Return(0);
    }
    let mut normal = Rect {
        left: fields[7] as i32,
        top: fields[8] as i32,
        right: fields[9] as i32,
        bottom: fields[10] as i32,
    };
    let width = i64::from(normal.right) - i64::from(normal.left);
    let height = i64::from(normal.bottom) - i64::from(normal.top);
    if !(0..=i64::from(i32::MAX)).contains(&width) || !(0..=i64::from(i32::MAX)).contains(&height) {
        set_last_error(env, 87);
        return Outcome::Return(0);
    }
    // Tool windows retain screen coordinates, including intentional offscreen
    // placement. Ordinary windows are recovered into the virtual work area.
    if !tool {
        let (sw, sh) = user(env).screen;
        let work_height = (sh - 40).max(0);
        if normal.left >= sw || normal.right <= 0 || normal.top >= work_height || normal.bottom <= 0
        {
            let x = normal.left.clamp(0, (sw - width as i32).max(0));
            let y = normal.top.clamp(0, (work_height - height as i32).max(0));
            normal = Rect::new(x, y, width as i32, height as i32);
        }
    }
    let command = match fields[2] {
        10 => SW_SHOWNORMAL,
        11 => SW_MINIMIZE,
        other => other,
    };
    let flags = fields[1];
    let min_position = (fields[3] as i32, fields[4] as i32);
    let apply = move |env: &mut Env<'_>| {
        let Some(window) = env
            .ext
            .get_mut::<crate::Win32>()
            .user
            .windows
            .get_mut(&mut env.sys.objects, &hwnd.0)
        else {
            return Outcome::Return(0);
        };
        window.placement.normal_rect = if normal_now { window.rect } else { normal };
        if normal_now || flags & 1 != 0 {
            window.placement.min_position = min_position;
        }
        window.placement.restore_to_maximized = false;
        let complete = move |env: &mut Env<'_>| {
            let Some(window) = env
                .ext
                .get_mut::<crate::Win32>()
                .user
                .windows
                .get_mut(&mut env.sys.objects, &hwnd.0)
            else {
                return Outcome::Return(0);
            };
            if flags & 1 != 0 {
                window.placement.min_position = min_position;
            }
            window.placement.restore_to_maximized =
                window.style & WS_MAXIMIZE != 0 || (command == SW_SHOWMINIMIZED && flags & 2 != 0);
            if command > 11 {
                set_last_error(env, 1449);
            }
            Outcome::Return(1)
        };
        if command > 11 {
            return complete(env);
        }
        super::window::show_window(env, hwnd.0, command, move |env, _| complete(env))
    };
    if normal_now {
        super::window::set_window_pos(
            env,
            hwnd.0,
            0,
            normal,
            super::window::SWP_NOZORDER | super::window::SWP_NOACTIVATE,
            move |env, success| {
                if success {
                    apply(env)
                } else {
                    Outcome::Return(0)
                }
            },
        )
    } else {
        apply(env)
    }
}

#[cfg(test)]
mod tests;
