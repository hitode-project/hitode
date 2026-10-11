//! Owned presentation data and validated input for the platform dialog adapter.
//! Neither this snapshot nor a host event shares a guest payload across threads.
use adv_machine::{Env, Objects};

use super::super::window::{Rect, TrackbarState, User, Window, make_long, wm};
use super::super::{user, user_mut};
use crate::Win32;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DialogTarget {
    pub dialog: u32,
    pub hwnd: u32,
    pub parent: u32,
    pub id: u32,
    pub created_us: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DialogFont {
    pub face: String,
    pub height: i32,
    pub weight: i32,
    pub italic: bool,
    pub charset: u8,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DialogSet {
    pub dialogs: Vec<DialogWindow>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DialogWindow {
    pub target: DialogTarget,
    pub owner: u32,
    pub owner_enabled: bool,
    pub class: String,
    pub style: u32,
    pub ex_style: u32,
    pub text: String,
    /// Screen coordinates; child rectangles use their parent's client coordinates.
    pub rect: Rect,
    pub client_size: (i32, i32),
    pub visible: bool,
    pub enabled: bool,
    pub modal: bool,
    pub focus: u32,
    pub font: Option<DialogFont>,
    /// Parents precede descendants; preserve guest z-order within each parent.
    pub controls: Vec<DialogControl>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DialogControl {
    pub target: DialogTarget,
    pub class: String,
    pub style: u32,
    pub ex_style: u32,
    pub text: String,
    pub rect: Rect,
    pub visible: bool,
    pub enabled: bool,
    pub check: u32,
    pub items: Vec<String>,
    pub selection: i32,
    pub range: Option<(i32, i32)>,
    pub value: i32,
    pub font: Option<DialogFont>,
    pub supported: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DialogEvent {
    Click(DialogTarget),
    Select {
        target: DialogTarget,
        index: i32,
    },
    Track {
        target: DialogTarget,
        value: i32,
        code: u16,
    },
    Text {
        target: DialogTarget,
        text: String,
    },
    Focus(DialogTarget),
    Close(DialogTarget),
    Key {
        target: DialogTarget,
        key: u32,
        shift: bool,
    },
}

fn identity(dialog: u32, window: &Window) -> DialogTarget {
    DialogTarget {
        dialog,
        hwnd: window.hwnd,
        parent: window.parent,
        id: window.id,
        created_us: window.created_us,
    }
}

fn is_dialog(user: &User, objects: &Objects, window: &Window) -> bool {
    window.parent == 0
        && window.visible()
        && !window.destroying
        && window.control.dialog_end.is_none()
        && (user.classes[window.class]
            .name
            .eq_ignore_ascii_case("#32770")
            || is_control_panel(user, objects, window))
}

/// A custom top-level window built only from standard controls, with a push
/// button to answer it, is a dialog in all but class: engines create startup
/// prompts this way and pump them with IsDialogMessage. A game canvas has no
/// such children, so it is never projected.
fn is_control_panel(user: &User, objects: &Objects, window: &Window) -> bool {
    let mut push_button = false;
    !window.children.is_empty()
        && window.children.iter().all(|hwnd| {
            user.windows.get(objects, hwnd).is_some_and(|child| {
                let class = &user.classes[child.class].name;
                push_button |= class.eq_ignore_ascii_case("Button") && child.style & 0xf <= 1;
                supported(class, child.style)
            })
        })
        && push_button
}

fn font(win32: &Win32, objects: &Objects, window: &Window) -> Option<DialogFont> {
    let crate::gdi32::GdiObj::Font(source) = win32.gdi.get(objects, window.font)? else {
        return None;
    };
    Some(DialogFont {
        face: source.face.clone(),
        height: source.height,
        weight: source.weight,
        italic: source.italic,
        charset: source.charset,
    })
}

pub(super) fn supported(class: &str, style: u32) -> bool {
    // Owner-drawn buttons retain guest input/state; the platform adapter can
    // project their captions without inventing or running the guest painter.
    (class.eq_ignore_ascii_case("Button") && matches!(style & 0xf, 0..=9 | 11))
        || (class.eq_ignore_ascii_case("Static") && matches!(style & 0x1f, 0..=2 | 7..=9 | 11..=12))
        || class.eq_ignore_ascii_case("Edit")
        || (class.eq_ignore_ascii_case("ComboBox") && style & 3 == 3)
        || class.eq_ignore_ascii_case("msctls_trackbar32")
}

/// Cheap and allocation-free when no visible top-level dialog exists. Only the
/// window index is traversed; GDI handles are looked up individually.
pub fn presentation(win32: &Win32, objects: &Objects) -> Result<DialogSet, String> {
    let user = &win32.user;
    let mut result = DialogSet::default();
    let mut remaining = 16384;
    for &hwnd in &user.top {
        let Some(window) = user
            .windows
            .get(objects, &hwnd)
            .filter(|w| is_dialog(user, objects, w))
        else {
            continue;
        };
        if result.dialogs.len() == 256 {
            return Err("host dialogs exceed 256 windows".into());
        }
        let mut pending = window.children.clone();
        let mut controls = Vec::new();
        let mut index = 0;
        while index < pending.len() {
            if remaining == 0 {
                return Err("host dialogs exceed 16384 controls".into());
            }
            remaining -= 1;
            let control = pending[index];
            index += 1;
            let Some(child) = user.windows.get(objects, &control) else {
                continue;
            };
            if child.destroying {
                continue;
            }
            let class = &user.classes[child.class].name;
            pending.extend(child.children.iter().copied());
            let default_track = TrackbarState::default();
            let track = class
                .eq_ignore_ascii_case("msctls_trackbar32")
                .then(|| child.control.trackbar.as_ref().unwrap_or(&default_track));
            let choices = child.control.choices.as_ref();
            controls.push(DialogControl {
                target: identity(hwnd, child),
                class: class.clone(),
                style: child.style,
                ex_style: child.ex_style,
                text: child.text.clone(),
                rect: child.rect,
                visible: child.visible(),
                enabled: child.enabled(),
                check: child.control.button & 3,
                items: choices.map_or_else(Vec::new, |c| c.items.clone()),
                selection: choices.and_then(|c| c.selected).map_or(-1, |i| i as i32),
                range: track.map(|t| (t.min, t.max)),
                value: track.map_or(0, |t| t.value),
                font: font(win32, objects, child),
                supported: supported(class, child.style),
            });
        }
        result.dialogs.push(DialogWindow {
            target: identity(hwnd, window),
            owner: window.owner,
            owner_enabled: user
                .windows
                .get(objects, &window.owner)
                .is_none_or(Window::enabled),
            class: user.classes[window.class].name.clone(),
            style: window.style,
            ex_style: window.ex_style,
            text: window.text.clone(),
            rect: window.rect,
            client_size: window.client_size(),
            visible: true,
            enabled: window.enabled(),
            modal: window.control.modal,
            focus: user.queues.get(&window.thread).map_or(0, |q| q.focus),
            font: font(win32, objects, window),
            controls,
        });
    }
    Ok(result)
}

/// Reject delayed input after destruction, reparenting, disable, or hide. HWNDs
/// are monotonic, and the creation timestamp also protects snapshot identity.
fn valid(user: &User, objects: &Objects, target: DialogTarget) -> bool {
    let Some(dialog) = user.windows.get(objects, &target.dialog) else {
        return false;
    };
    if !is_dialog(user, objects, dialog) || !dialog.enabled() {
        return false;
    }
    let Some(window) = user.windows.get(objects, &target.hwnd) else {
        return false;
    };
    if window.parent != target.parent
        || window.id != target.id
        || window.created_us != target.created_us
        || window.thread != dialog.thread
    {
        return false;
    }
    let mut current = target.hwnd;
    for _ in 0..=user.windows.len() {
        let Some(window) = user.windows.get(objects, &current) else {
            return false;
        };
        if !window.visible() || !window.enabled() || window.destroying {
            return false;
        }
        if current == target.dialog {
            return true;
        }
        if window.parent == 0 {
            return false;
        }
        current = window.parent;
    }
    false
}

fn focus(env: &mut Env<'_>, target: DialogTarget) -> bool {
    let thread = user(env)
        .windows
        .get(&env.sys.objects, &target.hwnd)
        .unwrap()
        .thread;
    let queue = user_mut(env).queue(thread);
    let old = std::mem::replace(&mut queue.focus, target.hwnd);
    if old != target.hwnd {
        if old != 0 {
            super::super::message::post(env, old, wm::KILLFOCUS, target.hwnd, 0);
        }
        super::super::message::post(env, target.hwnd, wm::SETFOCUS, old, 0);
    }
    true
}

/// Runs solely on the execution thread. Native changes are input to the guest's
/// Win32 control state; only guest callbacks may alter the game's settings.
pub fn post_host_event(env: &mut Env<'_>, event: DialogEvent) -> bool {
    let target = match &event {
        DialogEvent::Click(t) | DialogEvent::Focus(t) | DialogEvent::Close(t) => *t,
        DialogEvent::Select { target, .. }
        | DialogEvent::Track { target, .. }
        | DialogEvent::Text { target, .. }
        | DialogEvent::Key { target, .. } => *target,
    };
    if !valid(user(env), &env.sys.objects, target) {
        return false;
    }
    let window = user(env)
        .windows
        .get(&env.sys.objects, &target.hwnd)
        .unwrap();
    let class = user(env).classes[window.class].name.clone();
    let style = window.style;
    let command = make_long(target.id as i32, 0);
    match event {
        DialogEvent::Click(_) if class.eq_ignore_ascii_case("Button") && style & 0xf != 7 => {
            super::super::message::post(env, target.hwnd, 0xf5, 0, 0)
        }
        DialogEvent::Select { index, .. }
            if class.eq_ignore_ascii_case("ComboBox") && style & 3 == 3 =>
        {
            let Some(choices) = window.control.choices.as_ref() else {
                return false;
            };
            if index < 0 || index as usize >= choices.items.len() {
                return false;
            }
            let selected_text = choices.items[index as usize].clone();
            let window = env
                .ext
                .get::<Win32>()
                .user
                .windows
                .get_mut(&mut env.sys.objects, &target.hwnd)
                .unwrap();
            window.control.choices.as_mut().unwrap().selected = Some(index as usize);
            window.text = selected_text;
            super::super::message::post(
                env,
                target.parent,
                wm::COMMAND,
                command | (1 << 16),
                target.hwnd,
            )
        }
        DialogEvent::Track { value, code, .. }
            if class.eq_ignore_ascii_case("msctls_trackbar32") =>
        {
            if code > 8 {
                return false;
            }
            let window = env
                .ext
                .get::<Win32>()
                .user
                .windows
                .get_mut(&mut env.sys.objects, &target.hwnd)
                .unwrap();
            let track = window
                .control
                .trackbar
                .get_or_insert_with(TrackbarState::default);
            let value = value.clamp(track.min.min(track.max), track.min.max(track.max));
            track.value = value;
            window.invalidate_client(true);
            super::super::message::post(
                env,
                target.parent,
                if style & 2 != 0 { 0x115 } else { 0x114 },
                make_long(i32::from(code), value),
                target.hwnd,
            )
        }
        DialogEvent::Text { text, .. }
            if class.eq_ignore_ascii_case("Edit") && style & 0x800 == 0 =>
        {
            if text.len() > 65536 {
                return false;
            }
            let window = env
                .ext
                .get::<Win32>()
                .user
                .windows
                .get_mut(&mut env.sys.objects, &target.hwnd)
                .unwrap();
            window.text = text;
            window.invalidate_client(true);
            super::super::message::post(
                env,
                target.parent,
                wm::COMMAND,
                command | (0x300 << 16),
                target.hwnd,
            )
        }
        DialogEvent::Focus(_) => focus(env, target),
        DialogEvent::Close(_) if target.hwnd == target.dialog => {
            super::super::message::post(env, target.dialog, wm::CLOSE, 0, 0)
        }
        DialogEvent::Key { key, shift, .. } => key_event(env, target.dialog, key, shift),
        _ => false,
    }
}

fn key_event(env: &mut Env<'_>, dialog: u32, key: u32, shift: bool) -> bool {
    let u = user(env);
    let window = u.windows.get(&env.sys.objects, &dialog).unwrap();
    let focus = u.queues.get(&window.thread).map_or(0, |q| q.focus);
    if key == 9 {
        let stops: Vec<_> = u
            .subtree(&env.sys.objects, dialog)
            .into_iter()
            .filter_map(|hwnd| u.windows.get(&env.sys.objects, &hwnd))
            .filter(|w| w.style & 0x10000 != 0 && valid(u, &env.sys.objects, identity(dialog, w)))
            .map(|w| identity(dialog, w))
            .collect();
        if stops.is_empty() {
            return false;
        }
        let next = stops.iter().position(|w| w.hwnd == focus).map_or(
            if shift { stops.len() - 1 } else { 0 },
            |i| {
                if shift {
                    (i + stops.len() - 1) % stops.len()
                } else {
                    (i + 1) % stops.len()
                }
            },
        );
        return self::focus(env, stops[next]);
    }
    if !matches!(key, 13 | 27) {
        return false;
    }
    let button = if key == 13 {
        let buttons: Vec<_> = window
            .children
            .iter()
            .filter_map(|hwnd| u.windows.get(&env.sys.objects, hwnd))
            .filter(|w| {
                u.classes[w.class].name.eq_ignore_ascii_case("Button")
                    && w.style & 0xf <= 1
                    && valid(u, &env.sys.objects, identity(dialog, w))
            })
            .collect();
        buttons
            .iter()
            .find(|w| w.hwnd == focus)
            .or_else(|| buttons.iter().find(|w| w.style & 0xf == 1))
            .map(|w| w.hwnd)
            .unwrap_or(super::item(u, &env.sys.objects, dialog, 1))
    } else {
        super::item(u, &env.sys.objects, dialog, 2)
    };
    if let Some(w) = u.windows.get(&env.sys.objects, &button) {
        if !valid(u, &env.sys.objects, identity(dialog, w)) {
            return false;
        }
        return super::super::message::post(env, button, 0xf5, 0, 0);
    }
    super::super::message::post(env, dialog, wm::COMMAND, if key == 13 { 1 } else { 2 }, 0)
}
