//! Guest fullscreen ownership. Never changes a host monitor or native window.
use std::collections::BTreeMap;

use adv_machine::{Env, Outcome};

use super::{
    message,
    window::{self, Rect},
};
use crate::Win32;

#[derive(Clone, Copy)]
struct SavedWindow {
    hwnd: u32,
    rect: Rect,
    style: u32,
    ex_style: u32,
    placement: super::PlacementState,
}
struct Fullscreen {
    saved: SavedWindow,
    size: (i32, i32),
}
#[derive(Default)]
struct Modes {
    desktop: Option<(i32, i32)>,
    owners: BTreeMap<u32, Fullscreen>,
    order: Vec<u32>,
}

fn screen(env: &mut Env<'_>, size: (i32, i32)) {
    let win32 = env.ext.get_mut::<Win32>();
    if win32.user.screen == size {
        return;
    }
    win32.user.screen = size;
    super::input::reconcile_cursor_clip(&mut win32.user);
    let desktop = win32.user.desktop;
    if let Some(window) = win32.user.windows.get_mut(&mut env.sys.objects, &desktop) {
        window.rect = Rect::new(0, 0, size.0, size.1);
    }
    let windows: Vec<_> = win32.user.top.iter().copied().collect();
    for hwnd in windows {
        // The display changed in the guest; notification belongs to its queue.
        message::post(env, hwnd, 0x7e, 32, window::make_long(size.0, size.1));
    }
}

fn retire(env: &mut Env<'_>, owner: u32) -> Option<SavedWindow> {
    let modes = env.ext.get_or_default::<Modes>();
    let removed = modes.owners.remove(&owner)?;
    modes.order.retain(|&id| id != owner);
    let size = modes
        .order
        .last()
        .and_then(|id| modes.owners.get(id))
        .map(|entry| entry.size)
        .or_else(|| modes.desktop.take())
        .unwrap();
    let remaining = modes.order.iter().rev().find_map(|id| {
        let entry = modes.owners.get(id)?;
        (entry.saved.hwnd == removed.saved.hwnd).then_some(entry.size)
    });
    screen(env, size);
    if let Some(size) = remaining {
        // Several devices can share a window. Retiring the latest one must
        // expose the surviving fullscreen geometry, not its windowed baseline.
        let window = env
            .ext
            .get::<Win32>()
            .user
            .windows
            .get(&env.sys.objects, &removed.saved.hwnd)?;
        Some(SavedWindow {
            rect: Rect::new(0, 0, size.0, size.1),
            style: window.style,
            ex_style: window.ex_style,
            ..removed.saved
        })
    } else {
        Some(removed.saved)
    }
}

fn restore_style(env: &mut Env<'_>, saved: SavedWindow) -> bool {
    let win32 = env.ext.get_mut::<Win32>();
    let Some(window) = win32
        .user
        .windows
        .get_mut(&mut env.sys.objects, &saved.hwnd)
    else {
        return false;
    };
    window.style = saved.style;
    window.ex_style = saved.ex_style;
    window.placement = saved.placement;
    true
}

/// A committed fullscreen mode uses the display origin and a borderless client.
pub fn fullscreen(env: &mut Env<'_>, owner: u32, hwnd: u32, width: u32, height: u32) -> Outcome {
    let same = env
        .ext
        .get_or_default::<Modes>()
        .owners
        .get(&owner)
        .is_some_and(|entry| entry.saved.hwnd == hwnd);
    if !same {
        release(env, owner);
    }
    let Some(window) = env
        .ext
        .get::<Win32>()
        .user
        .windows
        .get(&env.sys.objects, &hwnd)
    else {
        return Outcome::Return(0x8876086c);
    };
    let initial = SavedWindow {
        hwnd,
        rect: window.rect,
        style: window.style,
        ex_style: window.ex_style,
        placement: window.placement,
    };
    let desktop = env.ext.get::<Win32>().user.screen;
    let modes = env.ext.get_or_default::<Modes>();
    let saved = modes
        .owners
        .values()
        .find(|entry| entry.saved.hwnd == hwnd)
        .map_or(initial, |entry| entry.saved);
    modes.desktop.get_or_insert(desktop);
    modes.owners.insert(
        owner,
        Fullscreen {
            saved,
            size: (width as i32, height as i32),
        },
    );
    modes.order.retain(|&id| id != owner);
    modes.order.push(owner);
    screen(env, (width as i32, height as i32));
    let window = env
        .ext
        .get_mut::<Win32>()
        .user
        .windows
        .get_mut(&mut env.sys.objects, &hwnd)
        .unwrap();
    window.style = (window.style
        & !(window::WS_CAPTION
            | window::WS_THICKFRAME
            | window::WS_MINIMIZE
            | window::WS_MAXIMIZE))
        | window::WS_POPUP
        | window::WS_VISIBLE;
    window.ex_style &= !(window::WS_EX_DLGMODALFRAME | window::WS_EX_CLIENTEDGE);
    window::set_window_pos(
        env,
        hwnd,
        0,
        Rect::new(0, 0, width as i32, height as i32),
        window::SWP_NOACTIVATE | window::SWP_NOZORDER | window::SWP_FRAMECHANGED,
        |_, _| Outcome::Return(0),
    )
}

/// Restore the original window and desktop on a successful windowed reset.
pub fn windowed(env: &mut Env<'_>, owner: u32) -> Outcome {
    let Some(saved) = retire(env, owner) else {
        return Outcome::Return(0);
    };
    if !restore_style(env, saved) {
        return Outcome::Return(0);
    }
    window::set_window_pos(
        env,
        saved.hwnd,
        0,
        saved.rect,
        window::SWP_NOACTIVATE | window::SWP_NOZORDER | window::SWP_FRAMECHANGED,
        |_, _| Outcome::Return(0),
    )
}

/// Final COM destruction cannot retain a guest callback; restore state now and
/// queue its geometry notifications for the surviving window owner.
pub fn release(env: &mut Env<'_>, owner: u32) {
    let Some(saved) = retire(env, owner) else {
        return;
    };
    if !restore_style(env, saved) {
        return;
    }
    let user = &mut env.ext.get_mut::<Win32>().user;
    user.windows
        .get_mut(&mut env.sys.objects, &saved.hwnd)
        .unwrap()
        .rect = saved.rect;
    let messages = window::window_pos_changed_msgs(user, &env.sys.objects, saved.hwnd, 0);
    for [hwnd, message, wparam, lparam] in messages {
        message::post(env, hwnd, message, wparam, lparam);
    }
}

#[cfg(test)]
mod tests;
