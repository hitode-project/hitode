//! Physical input from the host frontend: route it to the window that would receive it
//! on Windows and keep the key state the guest reads in step.
use adv_machine::Env;

use super::User;
use super::window::{WS_MINIMIZE, make_long, wm};
use crate::Win32;

/// Involutive mapping between physical L/R and logical primary/secondary.
/// The platform adapter uses it once to undo host mapping; this frontend uses
/// it once to apply the guest mapping. Keyboard/Ctrl and wheel high bits stay intact.
pub fn mouse_mapping(message: u32, buttons: u32, swapped: bool) -> (u32, u32) {
    if !swapped || !(0x200..=0x20e).contains(&message) {
        return (message, buttons);
    }
    let message = match message {
        0x201..=0x203 => message + 3,
        0x204..=0x206 => message - 3,
        _ => message,
    };
    (
        message,
        (buttons & !3) | ((buttons & 1) << 1) | ((buttons & 2) >> 1),
    )
}

/// Post input to the foremost visible window's thread. Keyboard messages use
/// its valid focus window, as do wheel messages; other mouse messages use capture.
pub fn post(env: &mut Env<'_>, message: u32, wparam: u32, lparam: u32) -> bool {
    post_at(env, message, wparam, lparam, std::time::Instant::now())
}

fn post_at(
    env: &mut Env<'_>,
    message: u32,
    wparam: u32,
    lparam: u32,
    time: std::time::Instant,
) -> bool {
    if message == wm::ACTIVATEAPP {
        return activate_app(env, wparam != 0);
    }
    if let Some(posted) = restore_minimized(env, message, wparam) {
        return posted;
    }
    let user = &mut env.ext.get_mut::<Win32>().user;
    let Some((top, mut hwnd)) = target(user, &env.sys.objects, message) else {
        return false;
    };
    let mut lparam = lparam;
    if matches!(message, 0x100 | 0x104 | 0x101 | 0x105) && wparam < 256 {
        let key = &mut user.keys[wparam as usize];
        if matches!(message, 0x100 | 0x104) {
            *key |= 0x80;
        } else {
            *key &= 0x7f;
        }
    }
    if (0x200..=0x20e).contains(&message) {
        let x = (lparam as u16 as i16) as i32;
        let y = ((lparam >> 16) as u16 as i16) as i32;
        // Frontend coordinates describe the displayed top-level client.
        // A captured child receives coordinates in its own client area.
        let (ox, oy) = user.client_origin(&env.sys.objects, top);
        let (cw, ch) = user
            .windows
            .get(&env.sys.objects, &top)
            .map_or((0, 0), |window| window.client_size());
        let (sx, sy) = super::input::set_host_cursor_position(
            user,
            (x.saturating_add(ox), y.saturating_add(oy)),
            super::window::Rect::new(ox, oy, cw, ch),
        );
        let thread = user.windows.get(&env.sys.objects, &top).unwrap().thread;
        let captured = user
            .queues
            .get(&thread)
            .is_some_and(|q| valid_window(user, &env.sys.objects, q.capture, thread));
        if !captured && message != 0x20a && message != 0x20e {
            // The host supplies top-level client coordinates. Ordinary mouse
            // input belongs to the deepest child at that point, not its parent.
            hwnd = super::hit_test::descendant(user, &env.sys.objects, top, (sx, sy));
            if hwnd == 0 {
                return false;
            }
        }
        if message == 0x20a || message == 0x20e {
            lparam = make_long(sx, sy);
        } else {
            let Some((x, y)) =
                super::geometry::client_point(user, &env.sys.objects, hwnd, (sx, sy))
            else {
                return false;
            };
            lparam = make_long(x, y);
        }
        for (down, up, vk) in [(0x201, 0x202, 1), (0x204, 0x205, 2), (0x207, 0x208, 4)] {
            if message == down || message == down + 2 {
                user.keys[vk] |= 0x80;
            }
            if message == up {
                user.keys[vk] &= 0x7f;
            }
        }
    }
    let (mut message, wparam) = mouse_mapping(message, wparam, user.swap_buttons);
    if (0x200..=0x20e).contains(&message) {
        let style = user
            .windows
            .get(&env.sys.objects, &hwnd)
            .map_or(0, |window| user.classes[window.class].style);
        message = user
            .double_click
            .message(hwnd, style, message, wparam, user.cursor_pos, time);
    }
    super::mouse_tracking::poll(env);
    super::message::post_input(env, hwnd, message, wparam, lparam)
}

const SIZE_MINIMIZED: u32 = 1;
const SC_RESTORE: u32 = 0xf120;

/// The host has no taskbar button for a window the guest minimized itself.
/// Restoring the host window, or the first press in it, does what clicking that
/// button does on Windows: WM_SYSCOMMAND SC_RESTORE. That press and its release
/// reach no window, and the host's minimize report for it is not repeated.
fn restore_minimized(env: &mut Env<'_>, message: u32, wparam: u32) -> Option<bool> {
    let user = &mut env.ext.get_mut::<Win32>().user;
    if user.restore_release == Some(message) {
        user.restore_release = None;
        return Some(false);
    }
    let release = match message {
        0x201 | 0x203 => 0x202,
        0x204 | 0x206 => 0x205,
        0x207 | 0x209 => 0x208,
        wm::SIZE => 0,
        _ => return None,
    };
    let (top, _) = target(user, &env.sys.objects, message)?;
    if !minimized_window(user, &env.sys.objects, top) {
        return None;
    }
    if message == wm::SIZE && wparam == SIZE_MINIMIZED {
        return Some(false);
    }
    if release != 0 {
        user.restore_release = Some(release);
    }
    super::message::post_input(env, top, wm::SYSCOMMAND, SC_RESTORE, 0);
    Some(false)
}

fn minimized_window(user: &User, objects: &adv_machine::Objects, hwnd: u32) -> bool {
    user.windows
        .get(objects, &hwnd)
        .is_some_and(|window| window.style & WS_MINIMIZE != 0)
}

/// Whether the displayed top-level window is minimized, which a host with
/// minimizable windows mirrors.
pub fn minimized(user: &User, objects: &adv_machine::Objects) -> bool {
    target(user, objects, 0).is_some_and(|(top, _)| minimized_window(user, objects, top))
}

/// The host application gained or lost activation. Windows tells the active
/// window in this order; some programs re-enable input only on WM_ACTIVATE.
fn activate_app(env: &mut Env<'_>, active: bool) -> bool {
    let user = &env.ext.get::<Win32>().user;
    let Some((top, _)) = target(user, &env.sys.objects, wm::ACTIVATEAPP) else {
        return false;
    };
    let thread = user.windows.get(&env.sys.objects, &top).unwrap().thread;
    let hwnd = user
        .queues
        .get(&thread)
        .map(|queue| queue.active)
        .filter(|&hwnd| valid_window(user, &env.sys.objects, hwnd, thread))
        .unwrap_or(top);
    let flag = u32::from(active);
    let messages = if active {
        [
            (wm::ACTIVATEAPP, flag),
            (NCACTIVATE, flag),
            (wm::ACTIVATE, flag),
        ]
    } else {
        [
            (NCACTIVATE, flag),
            (wm::ACTIVATE, flag),
            (wm::ACTIVATEAPP, flag),
        ]
    };
    messages
        .into_iter()
        .all(|(message, wparam)| super::message::post_input(env, hwnd, message, wparam, 0))
}

const NCACTIVATE: u32 = 0x86;

/// Text committed by a host input method, delivered the way an IME hands its
/// result to a program without IME support: one WM_IME_CHAR per character to
/// the keyboard focus, encoded for that window procedure (UTF-16 units, or a
/// packed ANSI DBCS character), which DefWindowProc turns into WM_CHAR.
/// Returns how many characters were posted; ones the guest ANSI code page
/// cannot represent are skipped.
pub fn post_text(env: &mut Env<'_>, text: &str) -> usize {
    let (hwnd, wide) = {
        let user = &env.ext.get::<Win32>().user;
        let Some((_, hwnd)) = target(user, &env.sys.objects, super::window::wm::CHAR) else {
            return 0;
        };
        let wide = user
            .windows
            .get(&env.sys.objects, &hwnd)
            .is_some_and(|w| w.unicode);
        (hwnd, wide)
    };
    let mut posted = 0;
    let mut units = [0; 2];
    for character in text.chars() {
        let units = character.encode_utf16(&mut units);
        let parameters: Vec<u32> = if wide {
            units.iter().map(|&unit| u32::from(unit)).collect()
        } else {
            let (bytes, lossy) = env.ext.get::<Win32>().codec.from_utf16(0, units);
            match bytes[..] {
                [byte] if !lossy => vec![u32::from(byte)],
                [lead, trail] if !lossy => vec![u32::from(lead) << 8 | u32::from(trail)],
                _ => continue,
            }
        };
        for parameter in parameters {
            if !super::message::post(env, hwnd, super::window::wm::IME_CHAR, parameter, 1) {
                return posted;
            }
        }
        posted += 1;
    }
    posted
}

/// Release keys/buttons when the native host loses focus. Preserve toggle
/// bits and enqueue releases so guest procedures cannot retain a held key.
pub fn release(env: &mut Env<'_>) -> usize {
    let (pressed, position) = {
        let user = &mut env.ext.get_mut::<Win32>().user;
        let pressed = user.keys;
        user.double_click.reset();
        for key in &mut user.keys {
            *key &= 0x7f;
        }
        let position = target(user, &env.sys.objects, 0x200).map_or(0, |(top, _)| {
            let (ox, oy) = user.client_origin(&env.sys.objects, top);
            make_long(user.cursor_pos.0 - ox, user.cursor_pos.1 - oy)
        });
        (pressed, position)
    };
    let mut posted = 0;
    for (key, _) in pressed
        .into_iter()
        .enumerate()
        .filter(|(_, state)| state & 0x80 != 0)
    {
        let (message, wparam, lparam) = match key {
            1 => (0x202, 0, position),
            2 => (0x205, 0, position),
            4 => (0x208, 0, position),
            5 | 6 => (0x20c, ((key as u32 - 4) << 16), position),
            0x12 => (0x105, key as u32, 0xe0000001),
            _ => (0x101, key as u32, 0xc0000001),
        };
        posted += usize::from(post(env, message, wparam, lparam));
    }
    for queue in env.ext.get_mut::<Win32>().user.queues.values_mut() {
        queue.capture = 0;
    }
    posted
}

/// A guest window as the control channel reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowView {
    pub hwnd: u32,
    pub parent: u32,
    pub id: u32,
    pub class: String,
    pub text: String,
    pub visible: bool,
    pub enabled: bool,
}

/// What host input would address now: the displayed top-level window, its
/// thread's focus, active and capture windows, and every top-level window in
/// z-order. `top` is 0 when no guest window is displayed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InputView {
    pub top: u32,
    pub focus: u32,
    pub active: u32,
    pub capture: u32,
    pub windows: Vec<WindowView>,
}

fn window_view(user: &User, window: &super::window::Window) -> WindowView {
    WindowView {
        hwnd: window.hwnd,
        parent: window.parent,
        id: window.id,
        class: user.classes[window.class].name.clone(),
        text: window.text.clone(),
        visible: window.visible(),
        enabled: window.enabled(),
    }
}

pub fn view(user: &User, objects: &adv_machine::Objects) -> InputView {
    let windows = user
        .top
        .iter()
        .filter_map(|hwnd| user.windows.get(objects, hwnd))
        .filter(|window| window.hwnd != user.desktop && !window.destroying)
        .map(|window| window_view(user, window))
        .collect();
    let Some((top, _)) = target(user, objects, 0) else {
        return InputView {
            windows,
            ..InputView::default()
        };
    };
    let thread = user.windows.get(objects, &top).unwrap().thread;
    let queue = user.queues.get(&thread);
    InputView {
        top,
        focus: queue.map_or(0, |q| q.focus),
        active: queue.map_or(0, |q| q.active),
        capture: queue.map_or(0, |q| q.capture),
        windows,
    }
}

/// The window that ordinary mouse input at top-level client point `(x, y)`
/// reaches, as [`post`] routes it without capture.
pub fn window_at(
    user: &User,
    objects: &adv_machine::Objects,
    x: i32,
    y: i32,
) -> Option<WindowView> {
    let (top, _) = target(user, objects, 0)?;
    let (ox, oy) = user.client_origin(objects, top);
    let hwnd = super::hit_test::descendant(user, objects, top, (x + ox, y + oy));
    user.windows
        .get(objects, &hwnd)
        .map(|window| window_view(user, window))
}

/// (top-level window, target window) for `message`.
pub(super) fn target(
    user: &User,
    objects: &adv_machine::Objects,
    message: u32,
) -> Option<(u32, u32)> {
    let top = user.top.iter().copied().find(|h| {
        user.windows
            .get(objects, h)
            .is_some_and(|w| w.visible() && !w.destroying)
    })?;
    let thread = user.windows.get(objects, &top).unwrap().thread;
    let queue = user.queues.get(&thread);
    let candidate = if (0x100..=0x109).contains(&message) || matches!(message, 0x20a | 0x20e) {
        queue.map(|q| q.focus)
    } else if (0x200..=0x20e).contains(&message) {
        queue.map(|q| q.capture)
    } else {
        None
    };
    let target = candidate
        .filter(|&hwnd| valid_window(user, objects, hwnd, thread))
        .unwrap_or(top);
    Some((top, target))
}

fn valid_window(
    user: &User,
    objects: &adv_machine::Objects,
    hwnd: u32,
    thread: adv_machine::CtxId,
) -> bool {
    if !user
        .windows
        .get(objects, &hwnd)
        .is_some_and(|w| w.thread == thread)
    {
        return false;
    }
    let mut current = hwnd;
    // Bound malformed parent chains as well as checking inherited visibility.
    for _ in 0..=user.windows.len() {
        let Some(window) = user.windows.get(objects, &current) else {
            return false;
        };
        if !window.visible() || !window.enabled() || window.destroying {
            return false;
        }
        if window.parent == 0 {
            return true;
        }
        current = window.parent;
    }
    false
}

#[cfg(test)]
mod tests;
