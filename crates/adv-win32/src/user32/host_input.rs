//! Physical input from the host frontend: route it to the window that would receive it
//! on Windows and keep the key state the guest reads in step.
use adv_machine::Env;

use super::User;
use super::window::make_long;
use crate::Win32;

/// Post input to the foremost visible window's thread. Keyboard messages use
/// its valid focus window; mouse messages use its valid capture window.
pub fn post(env: &mut Env<'_>, message: u32, wparam: u32, lparam: u32) -> bool {
    let user = &mut env.ext.get_mut::<Win32>().user;
    let Some((top, hwnd)) = target(user, message) else {
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
        let (ox, oy) = user.client_origin(top);
        user.cursor_pos = (x + ox, y + oy);
        if message == 0x20a || message == 0x20e {
            lparam = make_long(x + ox, y + oy);
        } else if hwnd != top {
            let (tx, ty) = user.client_origin(hwnd);
            lparam = make_long(x + ox - tx, y + oy - ty);
        }
        for (down, up, vk) in [(0x201, 0x202, 1), (0x204, 0x205, 2), (0x207, 0x208, 4)] {
            if message == down {
                user.keys[vk] |= 0x80;
            }
            if message == up {
                user.keys[vk] &= 0x7f;
            }
        }
    }
    super::message::post_input(env, hwnd, message, wparam, lparam)
}

/// Release keys/buttons when the native host loses focus. Preserve toggle
/// bits and enqueue releases so guest procedures cannot retain a held key.
pub fn release(env: &mut Env<'_>) -> usize {
    let (pressed, position) = {
        let user = &mut env.ext.get_mut::<Win32>().user;
        let pressed = user.keys;
        for key in &mut user.keys {
            *key &= 0x7f;
        }
        let position = target(user, 0x200).map_or(0, |(top, _)| {
            let (ox, oy) = user.client_origin(top);
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

/// (top-level window, target window) for `message`.
fn target(user: &User, message: u32) -> Option<(u32, u32)> {
    let top = user.top.iter().copied().find(|h| {
        user.windows
            .get(h)
            .is_some_and(|w| w.visible() && !w.destroying)
    })?;
    let thread = user.windows[&top].thread;
    let queue = user.queues.get(&thread);
    let candidate = if (0x100..=0x109).contains(&message) {
        queue.map(|q| q.focus)
    } else if (0x200..=0x20e).contains(&message) {
        queue.map(|q| q.capture)
    } else {
        None
    };
    let target = candidate
        .filter(|&hwnd| valid_window(user, hwnd, thread))
        .unwrap_or(top);
    Some((top, target))
}

fn valid_window(user: &User, hwnd: u32, thread: adv_machine::CtxId) -> bool {
    if !user.windows.get(&hwnd).is_some_and(|w| w.thread == thread) {
        return false;
    }
    let mut current = hwnd;
    // Bound malformed parent chains as well as checking inherited visibility.
    for _ in 0..=user.windows.len() {
        let Some(window) = user.windows.get(&current) else {
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
