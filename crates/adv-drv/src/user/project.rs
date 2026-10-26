//! The host's native view of guest windows: dialogs, the menu bar and what
//! input reaches. Each guest thread reads its own windows through user32
//! when it next looks at its queue, and the host's answers go back to that
//! thread as the controls' own messages, so Wine's controls keep their state.
use std::collections::{BTreeSet, HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use adv_abi::{Env, Outcome};
use adv_cpu::backend::{GuestExt, Prot};
use adv_device::ui::{
    DialogControl, DialogEvent, DialogFont, DialogSet, DialogTarget, DialogWindow, InputView,
    MenuBar, MenuCommand, MenuEvent, MenuItem, MenuKey, PopupPresentation, Rect, WindowView,
    supported,
};
use adv_machine::{CtxId, Ext};
use adv_nt::process::{module_base, module_export};

use super::input::{self, Done};
use super::{MODULE, is_desktop};

const WS_CHILD: u32 = 0x4000_0000;
const WS_MINIMIZE: u32 = 0x2000_0000;
const WS_VISIBLE: u32 = 0x1000_0000;
const WS_DISABLED: u32 = 0x0800_0000;
const WS_TABSTOP: u32 = 0x0001_0000;
const WM_SETTEXT: u32 = 0x0c;
const WM_CLOSE: u32 = 0x10;
const WM_ACTIVATEAPP: u32 = 0x1c;
const WM_GETFONT: u32 = 0x31;
const WM_COMMAND: u32 = 0x111;
const WM_SYSCOMMAND: u32 = 0x112;
const WM_HSCROLL: u32 = 0x114;
const WM_VSCROLL: u32 = 0x115;
const WM_INITMENU: u32 = 0x116;
const WM_INITMENUPOPUP: u32 = 0x117;
const MN_GETHMENU: u32 = 0x1e1;
const SC_RESTORE: u32 = 0xf120;
const BM_GETCHECK: u32 = 0xf0;
const BM_CLICK: u32 = 0xf5;
const LB_SETCURSEL: u32 = 0x186;
const LB_GETCURSEL: u32 = 0x188;
const LB_GETTEXT: u32 = 0x189;
const LB_GETTEXTLEN: u32 = 0x18a;
const LB_GETCOUNT: u32 = 0x18b;
const CB_GETCOUNT: u32 = 0x146;
const CB_GETCURSEL: u32 = 0x147;
const CB_GETLBTEXT: u32 = 0x148;
const CB_GETLBTEXTLEN: u32 = 0x149;
const CB_SETCURSEL: u32 = 0x14e;
const TBM_GETPOS: u32 = 0x400;
const TBM_GETRANGEMIN: u32 = 0x401;
const TBM_GETRANGEMAX: u32 = 0x402;
const TBM_SETPOS: u32 = 0x405;
/// GA_PARENT and GW_OWNER.
const GA_PARENT: u32 = 1;
const GW_OWNER: u32 = 4;
/// `sizeof(MENUITEMINFOW)` (i386), and the fields it asks for: state, ID,
/// submenu, string, bitmap and type.
const MENU_ITEM_INFO: u32 = 48;
const MENU_ITEM_MASK: u32 = 0x1c7;
const LOGFONT: u32 = 92;
/// Text read back is cut at this many UTF-16 units.
const TEXT: u32 = 256;
/// Bounds that keep one snapshot small.
const ITEMS: u32 = 256;
const MENU_DEPTH: u32 = 8;
/// How often a thread's windows are read again.
const REFRESH: Duration = Duration::from_millis(100);

/// Where one window's answers land in the snapshot's guest buffer.
const CLASS_AT: u32 = 0;
const TEXT_AT: u32 = 128;
const RECT_AT: u32 = TEXT_AT + 2 * TEXT;
const CLIENT_AT: u32 = RECT_AT + 16;
const ORIGIN_AT: u32 = CLIENT_AT + 16;
const FONT_AT: u32 = ORIGIN_AT + 8;
const STRIDE: u32 = FONT_AT + LOGFONT + 4;

/// One guest window as its thread last reported it.
#[derive(Clone, Debug, Default)]
struct Info {
    hwnd: u32,
    parent: u32,
    owner: u32,
    style: u32,
    ex_style: u32,
    /// The control ID, or a top-level window's menu.
    id: u32,
    class: String,
    text: String,
    /// The window in screen coordinates.
    rect: Rect,
    client: (i32, i32),
    /// The client area's origin in screen coordinates.
    origin: (i32, i32),
    font: u32,
    logfont: Option<DialogFont>,
    check: u32,
    count: u32,
    items: Vec<String>,
    selection: i32,
    range: Option<(i32, i32)>,
    value: i32,
    menu: Vec<MenuItem>,
}

impl Info {
    fn visible(&self) -> bool {
        self.style & WS_VISIBLE != 0
    }
    fn enabled(&self) -> bool {
        self.style & WS_DISABLED == 0
    }
    fn top_level(&self) -> bool {
        self.parent == 0
    }
    fn is(&self, class: &str) -> bool {
        self.class.eq_ignore_ascii_case(class)
    }
    fn list(&self) -> Option<(u32, u32, u32)> {
        if self.is("ListBox") {
            Some((LB_GETCOUNT, LB_GETTEXTLEN, LB_GETTEXT))
        } else if self.is("ComboBox") {
            Some((CB_GETCOUNT, CB_GETLBTEXTLEN, CB_GETLBTEXT))
        } else {
            None
        }
    }
}

/// A thread's windows, and its focus, active and capture windows.
#[derive(Clone, Default)]
struct Part {
    windows: Vec<Info>,
    focus: u32,
    active: u32,
    capture: u32,
}

#[derive(Default)]
struct Projection {
    parts: HashMap<CtxId, Part>,
    popup: Option<(u32, Arc<AtomicBool>)>,
    choice: Option<VecDeque<MenuCommand>>,
    /// Threads with a read under way, and when each last finished one.
    reading: BTreeSet<CtxId>,
    read: HashMap<CtxId, Instant>,
}

impl Drop for Projection {
    fn drop(&mut self) {
        if let Some((_, active)) = &self.popup {
            active.store(false, Ordering::Release);
        }
    }
}

/// Retire native menu tracking when the process stops.
pub fn stopped(ext: &mut Ext) {
    if let Some(hwnd) = ext
        .try_get::<Projection>()
        .and_then(|state| state.popup.as_ref().map(|(hwnd, _)| *hwnd))
    {
        forget(ext, hwnd);
    }
}

fn projection(ext: &mut Ext) -> &mut Projection {
    ext.get_or_default::<Projection>()
}

fn windows(ext: &Ext) -> impl Iterator<Item = &Info> {
    ext.try_get::<Projection>()
        .into_iter()
        .flat_map(|projection| projection.parts.values())
        .flat_map(|part| part.windows.iter())
}

fn info(ext: &Ext, hwnd: u32) -> Option<&Info> {
    windows(ext).find(|info| info.hwnd == hwnd)
}

/// Read every guest thread's windows again, at most every [`REFRESH`].
/// Returns whether new guest work was queued.
pub fn refresh(ext: &mut Ext) -> bool {
    let now = Instant::now();
    let mut queued = false;
    let mut owners: HashMap<CtxId, u32> = HashMap::new();
    for (hwnd, context) in super::windows(ext) {
        owners.entry(context).or_insert(hwnd);
    }
    for (context, hwnd) in owners {
        let state = projection(ext);
        if state.reading.contains(&context)
            || state
                .read
                .get(&context)
                .is_some_and(|at| now.duration_since(*at) < REFRESH)
        {
            continue;
        }
        if input::schedule(
            ext,
            hwnd,
            Box::new(move |env, done| read(env, context, done)),
        ) {
            projection(ext).reading.insert(context);
            queued = true;
        }
    }
    queued
}

/// The dialogs on screen, as the host shows them natively.
pub fn dialogs(ext: &Ext) -> DialogSet {
    let all: Vec<&Info> = windows(ext).collect();
    let by_parent = |parent: u32| {
        let mut children: Vec<&Info> = all
            .iter()
            .copied()
            .filter(|info| info.parent == parent)
            .collect();
        children.sort_by_key(|info| info.hwnd);
        children
    };
    let font = |info: &Info| info.logfont.clone();
    let mut set = DialogSet::default();
    for dialog in all
        .iter()
        .copied()
        .filter(|info| is_dialog(info, &by_parent))
    {
        let mut pending: VecDeque<&Info> = by_parent(dialog.hwnd).into();
        let mut controls = Vec::new();
        while let Some(control) = pending.pop_front() {
            pending.extend(by_parent(control.hwnd));
            let parent = all.iter().find(|info| info.hwnd == control.parent);
            let (x, y) = parent.map_or((0, 0), |parent| parent.origin);
            controls.push(DialogControl {
                target: target(dialog.hwnd, control),
                class: control.class.clone(),
                style: control.style,
                ex_style: control.ex_style,
                text: control.text.clone(),
                rect: Rect {
                    left: control.rect.left - x,
                    top: control.rect.top - y,
                    right: control.rect.right - x,
                    bottom: control.rect.bottom - y,
                },
                visible: control.visible(),
                enabled: control.enabled(),
                check: control.check & 3,
                items: control.items.clone(),
                selection: control.selection,
                range: control.range,
                value: control.value,
                font: font(control),
                supported: projectable(control),
            });
        }
        let owner = all.iter().find(|info| info.hwnd == dialog.owner);
        let focus = ext
            .try_get::<Projection>()
            .and_then(|projection| {
                projection
                    .parts
                    .values()
                    .find(|part| part.windows.iter().any(|info| info.hwnd == dialog.hwnd))
            })
            .map_or(0, |part| part.focus);
        set.dialogs.push(DialogWindow {
            target: target(dialog.hwnd, dialog),
            owner: dialog.owner,
            owner_enabled: owner.is_none_or(|owner| owner.enabled()),
            class: dialog.class.clone(),
            style: dialog.style,
            ex_style: dialog.ex_style,
            text: dialog.text.clone(),
            rect: dialog.rect,
            client_size: dialog.client,
            visible: true,
            enabled: dialog.enabled(),
            modal: owner.is_some_and(|owner| !owner.enabled()),
            focus,
            font: font(dialog),
            controls,
        });
    }
    set.dialogs.sort_by_key(|dialog| dialog.target.hwnd);
    set
}

/// Whether the host shows `hwnd` as a native dialog or popup instead of frames.
pub fn projected(ext: &Ext, hwnd: u32) -> bool {
    info(ext, hwnd).is_some_and(|info| info.is("#32768"))
        || dialogs(ext)
            .dialogs
            .iter()
            .any(|dialog| dialog.target.hwnd == hwnd)
}

fn target(dialog: u32, info: &Info) -> DialogTarget {
    DialogTarget {
        dialog,
        hwnd: info.hwnd,
        parent: info.parent,
        id: if info.top_level() { 0 } else { info.id },
        created_us: 0,
    }
}

fn projectable(info: &Info) -> bool {
    // Multiple selection and owner-drawn lists without strings stay guest-only.
    let list =
        info.style & (0x8 | 0x800) == 0 && (info.style & 0x30 == 0 || info.style & 0x40 != 0);
    supported(&info.class, info.style, list)
}

fn is_dialog<'a>(info: &Info, children: &impl Fn(u32) -> Vec<&'a Info>) -> bool {
    if !info.top_level() || !info.visible() {
        return false;
    }
    if info.is("#32770") {
        return true;
    }
    // A custom top-level window built only from standard controls, with a
    // push button to answer it, is a dialog in all but class: engines create
    // startup prompts this way. A game canvas has no such children.
    let children = children(info.hwnd);
    !children.is_empty()
        && children.iter().all(|child| projectable(child))
        && children
            .iter()
            .any(|child| child.is("Button") && child.style & 0xf <= 1)
}

/// The active popup, or the menu bar of the presented window or first visible
/// window with one.
pub fn menu_bar(ext: &Ext) -> MenuBar {
    let presented = input::presented_window(ext);
    if let Some((hwnd, active)) = ext
        .try_get::<Projection>()
        .and_then(|state| state.popup.as_ref())
        && let Some(popup) = info(ext, *hwnd)
    {
        let (x, y) = input::presented_origin(ext).unwrap_or((0, 0));
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        Arc::as_ptr(active).hash(&mut hasher);
        return MenuBar {
            key: MenuKey {
                window: *hwnd,
                menu: popup.id,
                revision: hasher.finish(),
                enabled: true,
            },
            items: popup.menu.clone(),
            popup: Some(PopupPresentation {
                x: popup.rect.left - x,
                y: popup.rect.top - y,
                flags: 0,
                exclude: None,
                active: active.clone(),
            }),
        };
    }
    let owner = windows(ext)
        .filter(|info| {
            info.top_level() && info.visible() && !info.is("#32768") && !info.menu.is_empty()
        })
        .min_by_key(|info| (Some(info.hwnd) != presented, info.hwnd));
    let Some(owner) = owner else {
        return MenuBar::default();
    };
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    format!("{:?}", owner.menu).hash(&mut hasher);
    MenuBar {
        key: MenuKey {
            window: owner.hwnd,
            menu: owner.id,
            revision: hasher.finish(),
            enabled: owner.enabled(),
        },
        items: owner.menu.clone(),
        popup: None,
    }
}

/// What host input reaches now.
pub fn view(ext: &Ext) -> InputView {
    let top = input::presented_window(ext).unwrap_or(0);
    let part = ext.try_get::<Projection>().and_then(|projection| {
        projection
            .parts
            .values()
            .find(|part| part.windows.iter().any(|info| info.hwnd == top))
    });
    let mut windows: Vec<WindowView> = windows(ext)
        .filter(|info| info.top_level())
        .map(window_view)
        .collect();
    windows.sort_by_key(|view| view.hwnd);
    InputView {
        top,
        focus: part.map_or(0, |part| part.focus),
        active: part.map_or(0, |part| part.active),
        capture: part.map_or(0, |part| part.capture),
        windows,
    }
}

fn window_view(info: &Info) -> WindowView {
    WindowView {
        hwnd: info.hwnd,
        parent: info.parent,
        id: info.id,
        class: info.class.clone(),
        text: info.text.clone(),
        visible: info.visible(),
        enabled: info.enabled(),
    }
}

/// The deepest visible window under a point of the presented frame.
pub fn window_at(ext: &Ext, x: i32, y: i32) -> Option<WindowView> {
    let mut hwnd = input::presented_window(ext)?;
    let (left, top) = info(ext, hwnd)?.origin;
    let (x, y) = (x + left, y + top);
    loop {
        let child = windows(ext)
            .filter(|info| info.parent == hwnd && info.visible())
            .filter(|info| {
                let r = info.rect;
                (r.left..r.right).contains(&x) && (r.top..r.bottom).contains(&y)
            })
            .max_by_key(|info| info.hwnd);
        match child {
            Some(child) => hwnd = child.hwnd,
            None => return info(ext, hwnd).map(window_view),
        }
    }
}

/// Whether the presented window is minimized.
pub fn minimized(ext: &Ext) -> bool {
    input::presented_window(ext)
        .and_then(|hwnd| info(ext, hwnd))
        .is_some_and(|info| info.style & WS_MINIMIZE != 0)
}

/// A message for a guest window, on its own thread.
enum Message {
    Post(u32, u32, u32, u32),
    Send(u32, u32, u32, u32),
    SetText(u32, String),
    Focus(u32),
}

/// Deliver `messages` on the thread of `hwnd`. False when it is gone.
fn deliver(ext: &mut Ext, hwnd: u32, messages: Vec<Message>) -> bool {
    if messages.is_empty() {
        return false;
    }
    projection(ext).read.clear();
    input::schedule(
        ext,
        hwnd,
        Box::new(move |env, done| send(env, messages.into(), done)),
    )
}

fn send(env: &mut Env<'_>, mut messages: VecDeque<Message>, done: Done) -> Outcome {
    let Some(message) = messages.pop_front() else {
        return done(env);
    };
    let (name, args, text) = match message {
        Message::Post(hwnd, message, wparam, lparam) => {
            ("PostMessageW", vec![hwnd, message, wparam, lparam], None)
        }
        Message::Send(hwnd, message, wparam, lparam) => {
            ("SendMessageW", vec![hwnd, message, wparam, lparam], None)
        }
        Message::SetText(hwnd, text) => {
            let mut bytes: Vec<u8> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
            bytes.extend([0, 0]);
            let buffer = match env.sys.vm.alloc(env.cpu, bytes.len() as u32, Prot::RW) {
                Ok(buffer) => buffer,
                Err(error) => return Outcome::Fail(format!("{MODULE}: {error}")),
            };
            if let Err(error) = env.cpu.write(buffer, &bytes) {
                return Outcome::Fail(format!("{MODULE}: {error}"));
            }
            (
                "SendMessageW",
                vec![hwnd, WM_SETTEXT, 0, buffer],
                Some(buffer),
            )
        }
        Message::Focus(hwnd) => ("SetFocus", vec![hwnd], None),
    };
    let target = match user32(env, name) {
        Ok(target) => target,
        Err(error) => return Outcome::Fail(error),
    };
    Outcome::call(target, args, move |env, _| {
        if let Some(buffer) = text {
            let _ = env.sys.vm.free(env.cpu, buffer);
        }
        send(env, messages, done)
    })
}

/// Answer a native dialog: the guest's controls change through their own
/// messages and notify their parents as a click or edit would.
pub fn dialog_event(ext: &mut Ext, event: DialogEvent) -> bool {
    let target = event.target();
    let set = dialogs(ext);
    let Some(dialog) = set
        .dialogs
        .iter()
        .find(|dialog| dialog.target.hwnd == target.dialog && dialog.enabled)
    else {
        return false;
    };
    let control = dialog
        .controls
        .iter()
        .find(|control| control.target == target && control.visible && control.enabled);
    let command = |code: u32| (target.id & 0xffff) | code << 16;
    let messages = match (&event, control) {
        (DialogEvent::Close(_), _) if target.hwnd == target.dialog => {
            vec![Message::Post(target.dialog, WM_CLOSE, 0, 0)]
        }
        (DialogEvent::Key { key, shift, .. }, _) => key_messages(dialog, *key, *shift),
        (_, None) => Vec::new(),
        (DialogEvent::Click(_), Some(control))
            if control.class.eq_ignore_ascii_case("Button") && control.style & 0xf != 7 =>
        {
            vec![Message::Post(target.hwnd, BM_CLICK, 0, 0)]
        }
        (DialogEvent::Select { index, .. }, Some(control))
            if (0..control.items.len() as i32).contains(index) =>
        {
            if control.class.eq_ignore_ascii_case("ListBox") {
                let mut messages = vec![Message::Send(target.hwnd, LB_SETCURSEL, *index as u32, 0)];
                // LBN_SELCHANGE reaches the parent only with LBS_NOTIFY.
                if control.style & 1 != 0 {
                    messages.push(Message::Post(
                        target.parent,
                        WM_COMMAND,
                        command(1),
                        target.hwnd,
                    ));
                }
                messages
            } else if control.class.eq_ignore_ascii_case("ComboBox") {
                vec![
                    Message::Send(target.hwnd, CB_SETCURSEL, *index as u32, 0),
                    Message::Post(target.parent, WM_COMMAND, command(1), target.hwnd),
                ]
            } else {
                Vec::new()
            }
        }
        (DialogEvent::Track { value, code, .. }, Some(control))
            if control.class.eq_ignore_ascii_case("msctls_trackbar32") && *code <= 8 =>
        {
            let (min, max) = control.range.unwrap_or((0, 100));
            let value = (*value).clamp(min.min(max), min.max(max));
            let scroll = if control.style & 2 != 0 {
                WM_VSCROLL
            } else {
                WM_HSCROLL
            };
            vec![
                Message::Send(target.hwnd, TBM_SETPOS, 1, value as u32),
                Message::Post(
                    target.parent,
                    scroll,
                    u32::from(*code) | (value as u32) << 16,
                    target.hwnd,
                ),
            ]
        }
        (DialogEvent::Text { text, .. }, Some(control))
            if control.class.eq_ignore_ascii_case("Edit")
                && control.style & 0x800 == 0
                && text.len() <= 65536 =>
        {
            vec![
                Message::SetText(target.hwnd, text.clone()),
                Message::Post(target.parent, WM_COMMAND, command(0x300), target.hwnd),
            ]
        }
        (DialogEvent::Focus(_), Some(_)) => vec![Message::Focus(target.hwnd)],
        _ => Vec::new(),
    };
    deliver(ext, target.dialog, messages)
}

/// Tab moves the focus; Enter and Esc answer the dialog as its default and
/// cancel buttons would.
fn key_messages(dialog: &DialogWindow, key: u32, shift: bool) -> Vec<Message> {
    let usable = |control: &&DialogControl| control.visible && control.enabled;
    if key == 9 {
        let stops: Vec<_> = dialog
            .controls
            .iter()
            .filter(usable)
            .filter(|control| control.style & WS_TABSTOP != 0)
            .collect();
        if stops.is_empty() {
            return Vec::new();
        }
        let next = stops
            .iter()
            .position(|control| control.target.hwnd == dialog.focus)
            .map_or(if shift { stops.len() - 1 } else { 0 }, |index| {
                if shift {
                    (index + stops.len() - 1) % stops.len()
                } else {
                    (index + 1) % stops.len()
                }
            });
        return vec![Message::Focus(stops[next].target.hwnd)];
    }
    let id = match key {
        13 => 1,
        27 => 2,
        _ => return Vec::new(),
    };
    let buttons: Vec<_> = dialog
        .controls
        .iter()
        .filter(usable)
        .filter(|control| {
            control.target.parent == dialog.target.hwnd
                && control.class.eq_ignore_ascii_case("Button")
                && control.style & 0xf <= 1
        })
        .collect();
    let button = if key == 13 {
        buttons
            .iter()
            .find(|control| control.target.hwnd == dialog.focus)
            .or_else(|| buttons.iter().find(|control| control.style & 0xf == 1))
            .copied()
            .or_else(|| {
                buttons
                    .iter()
                    .find(|control| control.target.id == 1)
                    .copied()
            })
    } else {
        buttons
            .iter()
            .find(|control| control.target.id == 2)
            .copied()
    };
    match button {
        Some(button) => vec![Message::Post(button.target.hwnd, BM_CLICK, 0, 0)],
        None => vec![Message::Post(dialog.target.hwnd, WM_COMMAND, id, 0)],
    }
}

/// Answer the native menu bar, as the guest's own menu loop would.
pub fn menu_event(ext: &mut Ext, event: MenuEvent) -> bool {
    let bar = menu_bar(ext);
    let (window, root) = match event {
        MenuEvent::Cancel { window, root } => (window, root),
        MenuEvent::Command(command) => (command.window, command.root),
        MenuEvent::Initialize { window, root, .. } => (window, root),
    };
    if root == 0 || bar.key.window != window || bar.key.menu != root || !bar.key.enabled {
        return false;
    }
    fn find<'a>(items: &'a [MenuItem], test: &impl Fn(&MenuItem) -> bool) -> Option<&'a MenuItem> {
        items.iter().find_map(|item| {
            (test(item))
                .then_some(item)
                .or_else(|| find(&item.children, test))
        })
    }
    if bar.popup.is_some() {
        return match event {
            // Native hosts follow a choice with cancellation for rejected choices.
            // Accepted choices still need Wine's loop to consume their clicks.
            MenuEvent::Cancel { .. } if projection(ext).choice.is_some() => true,
            MenuEvent::Cancel { .. } => input::schedule(
                ext,
                window,
                Box::new(|env, done| calls(env, vec![("EndMenu", vec![])], |env, _| done(env))),
            ),
            MenuEvent::Initialize { .. } => true,
            MenuEvent::Command(command) => {
                fn path(items: &[MenuItem], command: MenuCommand) -> Option<VecDeque<MenuCommand>> {
                    for item in items {
                        if item.flags & (3 | 0x800 | 0x4 | 0x100) != 0 {
                            continue;
                        }
                        if item.command == command && item.submenu == 0 {
                            return Some(VecDeque::from([command]));
                        }
                        if let Some(mut path) = path(&item.children, command) {
                            path.push_front(item.command);
                            return Some(path);
                        }
                    }
                    None
                }
                let Some(path) = path(&bar.items, command) else {
                    return false;
                };
                projection(ext).choice = Some(path);
                input::schedule(
                    ext,
                    window,
                    Box::new(move |env, done| click_next(env, root, done)),
                )
            }
        };
    }
    let message = match event {
        MenuEvent::Cancel { .. } => return false,
        MenuEvent::Command(command) => {
            let selectable = find(&bar.items, &|item: &MenuItem| {
                item.command == command
                    && item.submenu == 0
                    && item.flags & (3 | 0x800 | 0x4 | 0x100) == 0
            });
            if selectable.is_none() {
                return false;
            }
            Message::Post(window, WM_COMMAND, command.id & 0xffff, 0)
        }
        MenuEvent::Initialize { menu, position, .. } => match position {
            None if menu == root => Message::Post(window, WM_INITMENU, menu, 0),
            Some(index) if find(&bar.items, &|item: &MenuItem| item.submenu == menu).is_some() => {
                Message::Post(window, WM_INITMENUPOPUP, menu, u32::from(index))
            }
            _ => return false,
        },
    };
    deliver(ext, window, vec![message])
}

/// Ask the presented window to close, as its close box would.
pub fn close(ext: &mut Ext) -> bool {
    input::presented_window(ext)
        .is_some_and(|hwnd| deliver(ext, hwnd, vec![Message::Post(hwnd, WM_CLOSE, 0, 0)]))
}

/// The host application gained or lost activation.
pub fn activate(ext: &mut Ext, active: bool) -> bool {
    input::presented_window(ext).is_some_and(|hwnd| {
        let message = Message::Post(hwnd, WM_ACTIVATEAPP, u32::from(active), 0);
        deliver(ext, hwnd, vec![message])
    })
}

/// Restore the presented window the guest minimized, as its taskbar
/// button would.
pub fn restore(ext: &mut Ext) -> bool {
    minimized(ext)
        && input::presented_window(ext).is_some_and(|hwnd| {
            deliver(
                ext,
                hwnd,
                vec![Message::Post(hwnd, WM_SYSCOMMAND, SC_RESTORE, 0)],
            )
        })
}

fn user32(env: &Env<'_>, name: &str) -> Result<u32, String> {
    let dll = if name == "GetObjectW" {
        "gdi32.dll"
    } else {
        "user32.dll"
    };
    module_base(env.cpu, env.sys.peb, dll)
        .and_then(|base| module_export(env.cpu, base, name))
        .ok_or_else(|| format!("{MODULE}: {dll} exports no {name}"))
}

/// Guest calls in turn; `then` gets their results in order.
fn calls(
    env: &mut Env<'_>,
    list: Vec<(&'static str, Vec<u32>)>,
    then: impl FnOnce(&mut Env<'_>, Vec<u32>) -> Outcome + 'static,
) -> Outcome {
    let mut resolved = VecDeque::with_capacity(list.len());
    for (name, args) in list {
        match user32(env, name) {
            Ok(target) => resolved.push_back((target, args)),
            Err(error) => return Outcome::Fail(error),
        }
    }
    step(env, resolved, Vec::new(), Box::new(then))
}

type Then = Box<dyn FnOnce(&mut Env<'_>, Vec<u32>) -> Outcome>;

fn step(
    env: &mut Env<'_>,
    mut list: VecDeque<(u32, Vec<u32>)>,
    mut results: Vec<u32>,
    then: Then,
) -> Outcome {
    let Some((target, args)) = list.pop_front() else {
        return then(env, results);
    };
    Outcome::call(target, args, move |env, result| {
        results.push(result.value());
        step(env, list, results, then)
    })
}

fn wide(env: &Env<'_>, at: u32, units: u32) -> String {
    let mut bytes = vec![0; 2 * units as usize];
    if env.cpu.read(at, &mut bytes).is_err() {
        return String::new();
    }
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&pair| u16::from_le_bytes(pair))
        .take_while(|&unit| unit != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

fn words<const N: usize>(env: &Env<'_>, at: u32) -> [i32; N] {
    std::array::from_fn(|index| env.cpu.read_u32(at + 4 * index as u32).unwrap_or(0) as i32)
}

/// A snapshot of this thread's windows, in phases: their properties, then
/// control state and fonts, then list items, then menus level by level.
fn read(env: &mut Env<'_>, context: CtxId, done: Done) -> Outcome {
    let hwnds: Vec<u32> = super::windows(env.ext)
        .into_iter()
        .filter(|&(hwnd, owner)| owner == context && !is_desktop(env, hwnd))
        .map(|(hwnd, _)| hwnd)
        .collect();
    let buffer = match env
        .sys
        .vm
        .alloc(env.cpu, STRIDE * hwnds.len().max(1) as u32, Prot::RW)
    {
        Ok(buffer) => buffer,
        Err(error) => return Outcome::Fail(format!("{MODULE}: {error}")),
    };
    let mut list = vec![
        ("GetFocus", vec![]),
        ("GetActiveWindow", vec![]),
        ("GetCapture", vec![]),
    ];
    for (index, &hwnd) in hwnds.iter().enumerate() {
        let at = buffer + STRIDE * index as u32;
        list.extend([
            ("GetAncestor", vec![hwnd, GA_PARENT]),
            ("GetWindow", vec![hwnd, GW_OWNER]),
            ("GetWindowLongW", vec![hwnd, -16i32 as u32]),
            ("GetWindowLongW", vec![hwnd, -20i32 as u32]),
            ("GetWindowLongW", vec![hwnd, -12i32 as u32]),
            ("GetClassNameW", vec![hwnd, at + CLASS_AT, 64]),
            ("InternalGetWindowText", vec![hwnd, at + TEXT_AT, TEXT]),
            ("GetWindowRect", vec![hwnd, at + RECT_AT]),
            ("GetClientRect", vec![hwnd, at + CLIENT_AT]),
            ("ClientToScreen", vec![hwnd, at + ORIGIN_AT]),
            ("SendMessageW", vec![hwnd, WM_GETFONT, 0, 0]),
        ]);
    }
    calls(env, list, move |env, results| {
        let (thread, results) = results.split_at(3);
        let mut part = Part {
            windows: Vec::with_capacity(hwnds.len()),
            focus: thread[0],
            active: thread[1],
            capture: thread[2],
        };
        for (index, (&hwnd, result)) in hwnds.iter().zip(results.as_chunks::<11>().0).enumerate() {
            let at = buffer + STRIDE * index as u32;
            let [left, top, right, bottom] = words::<4>(env, at + RECT_AT);
            let [_, _, width, height] = words::<4>(env, at + CLIENT_AT);
            let [x, y] = words::<2>(env, at + ORIGIN_AT);
            let parent = if is_desktop(env, result[0]) {
                0
            } else {
                result[0]
            };
            part.windows.push(Info {
                hwnd,
                parent,
                owner: result[1],
                style: result[2],
                ex_style: result[3],
                id: result[4],
                class: wide(env, at + CLASS_AT, 64),
                text: wide(env, at + TEXT_AT, TEXT),
                rect: Rect {
                    left,
                    top,
                    right,
                    bottom,
                },
                client: (width, height),
                origin: (x, y),
                font: result[10],
                selection: -1,
                ..Info::default()
            });
        }
        let popups: Vec<usize> = part
            .windows
            .iter()
            .enumerate()
            .filter(|(_, info)| info.is("#32768") && info.visible())
            .map(|(index, _)| index)
            .collect();
        let list = popups
            .iter()
            .map(|&index| {
                (
                    "SendMessageW",
                    vec![part.windows[index].hwnd, MN_GETHMENU, 0, 0],
                )
            })
            .collect();
        calls(env, list, move |env, menus| {
            for (index, menu) in popups.into_iter().zip(menus) {
                part.windows[index].id = menu;
            }
            state(env, context, part, buffer, done)
        })
    })
}

/// Control state and fonts, then list counts and menu sizes.
fn state(env: &mut Env<'_>, context: CtxId, part: Part, buffer: u32, done: Done) -> Outcome {
    // (window index, what the result means)
    enum Ask {
        Check,
        Count,
        Selection,
        Position,
        Min,
        Max,
        Font,
        Menu,
    }
    let mut asks = Vec::new();
    let mut list = Vec::new();
    for (index, info) in part.windows.iter().enumerate() {
        let hwnd = info.hwnd;
        if info.is("Button") {
            asks.push((index, Ask::Check));
            list.push(("SendMessageW", vec![hwnd, BM_GETCHECK, 0, 0]));
        }
        if let Some((count, _, _)) = info.list() {
            let selection = if info.is("ListBox") {
                LB_GETCURSEL
            } else {
                CB_GETCURSEL
            };
            asks.extend([(index, Ask::Count), (index, Ask::Selection)]);
            list.push(("SendMessageW", vec![hwnd, count, 0, 0]));
            list.push(("SendMessageW", vec![hwnd, selection, 0, 0]));
        }
        if info.is("msctls_trackbar32") {
            asks.extend([(index, Ask::Position), (index, Ask::Min), (index, Ask::Max)]);
            for message in [TBM_GETPOS, TBM_GETRANGEMIN, TBM_GETRANGEMAX] {
                list.push(("SendMessageW", vec![hwnd, message, 0, 0]));
            }
        }
        if info.font != 0 {
            let at = buffer + STRIDE * index as u32 + FONT_AT;
            asks.push((index, Ask::Font));
            list.push(("GetObjectW", vec![info.font, LOGFONT, at]));
        }
        if info.top_level() && info.style & WS_CHILD == 0 && info.id != 0 && info.visible() {
            asks.push((index, Ask::Menu));
            list.push(("GetMenuItemCount", vec![info.id]));
        }
    }
    calls(env, list, move |env, results| {
        let mut part = part;
        let mut menus = Vec::new();
        for ((index, ask), result) in asks.into_iter().zip(results) {
            let info = &mut part.windows[index];
            match ask {
                Ask::Check => info.check = result,
                Ask::Count => info.count = result.min(ITEMS),
                Ask::Selection => info.selection = result as i32,
                Ask::Position => info.value = result as i32,
                Ask::Min => info.range = Some((result as i32, 0)),
                Ask::Max => info.range = info.range.map(|(min, _)| (min, result as i32)),
                Ask::Font if result != 0 => {
                    let at = buffer + STRIDE * index as u32 + FONT_AT;
                    let [height, _, _, _, weight] = words::<5>(env, at);
                    let mut flags = [0; 4];
                    let _ = env.cpu.read(at + 20, &mut flags);
                    info.logfont = Some(DialogFont {
                        face: wide(env, at + 28, 32),
                        height,
                        weight,
                        italic: flags[0] != 0,
                        charset: flags[3],
                    });
                }
                Ask::Font => {}
                Ask::Menu if result as i32 > 0 => menus.push(Level {
                    window: index,
                    path: Vec::new(),
                    menu: info.id,
                    count: result.min(ITEMS),
                }),
                Ask::Menu => {}
            }
        }
        let _ = env.sys.vm.free(env.cpu, buffer);
        items(env, context, part, menus, done)
    })
}

/// A menu to read: whose bar it belongs to, where it hangs in the bar, and
/// how many items it has.
struct Level {
    window: usize,
    path: Vec<usize>,
    menu: u32,
    count: u32,
}

/// List items: their lengths, then the ones that fit.
fn items(env: &mut Env<'_>, context: CtxId, part: Part, menus: Vec<Level>, done: Done) -> Outcome {
    let lists: Vec<(usize, u32, u32)> = part
        .windows
        .iter()
        .enumerate()
        .flat_map(|(index, info)| {
            let (count, length) = info
                .list()
                .map_or((0, 0), |(_, length, _)| (info.count, length));
            (0..count).map(move |item| (index, item, length))
        })
        .collect();
    let list = lists
        .iter()
        .map(|&(index, item, length)| {
            (
                "SendMessageW",
                vec![part.windows[index].hwnd, length, item, 0],
            )
        })
        .collect();
    calls(env, list, move |env, lengths| {
        let fits: Vec<(usize, u32)> = lists
            .iter()
            .zip(lengths)
            .filter(|&(_, length)| (length as i32) >= 0 && length < TEXT)
            .map(|(&(index, item, _), _)| (index, item))
            .collect();
        let buffer = match env
            .sys
            .vm
            .alloc(env.cpu, 2 * TEXT * fits.len().max(1) as u32, Prot::RW)
        {
            Ok(buffer) => buffer,
            Err(error) => return Outcome::Fail(format!("{MODULE}: {error}")),
        };
        let list = fits
            .iter()
            .enumerate()
            .map(|(slot, &(index, item))| {
                let info = &part.windows[index];
                let (_, _, text) = info.list().unwrap();
                let at = buffer + 2 * TEXT * slot as u32;
                ("SendMessageW", vec![info.hwnd, text, item, at])
            })
            .collect();
        calls(env, list, move |env, _| {
            let mut part = part;
            for (slot, &(index, _)) in fits.iter().enumerate() {
                let text = wide(env, buffer + 2 * TEXT * slot as u32, TEXT);
                part.windows[index].items.push(text);
            }
            let _ = env.sys.vm.free(env.cpu, buffer);
            menu(env, context, part, menus, 0, done)
        })
    })
}

/// One level of menus: every item's information, then the sizes of the
/// submenus, which make the next level.
fn menu(
    env: &mut Env<'_>,
    context: CtxId,
    part: Part,
    levels: Vec<Level>,
    depth: u32,
    done: Done,
) -> Outcome {
    if levels.is_empty() || depth == MENU_DEPTH {
        return finish(env, context, part, done);
    }
    let slots: Vec<(usize, u32)> = levels
        .iter()
        .enumerate()
        .flat_map(|(level, entry)| (0..entry.count).map(move |item| (level, item)))
        .collect();
    let size = MENU_ITEM_INFO + 2 * TEXT;
    let buffer = match env
        .sys
        .vm
        .alloc(env.cpu, size * slots.len().max(1) as u32, Prot::RW)
    {
        Ok(buffer) => buffer,
        Err(error) => return Outcome::Fail(format!("{MODULE}: {error}")),
    };
    let mut list = Vec::with_capacity(slots.len());
    for (slot, &(level, item)) in slots.iter().enumerate() {
        let at = buffer + size * slot as u32;
        let header = [MENU_ITEM_INFO, MENU_ITEM_MASK, 0, 0, 0, 0, 0, 0, 0];
        let tail = [at + MENU_ITEM_INFO, TEXT - 1, 0];
        if let Err(error) = crate::write_words(env, at, &header)
            .and_then(|()| crate::write_words(env, at + 36, &tail))
        {
            return Outcome::Fail(error);
        }
        list.push(("GetMenuItemInfoW", vec![levels[level].menu, item, 1, at]));
    }
    calls(env, list, move |env, results| {
        let mut part = part;
        let mut next = Vec::new();
        for (slot, (&(level, item), ok)) in slots.iter().zip(results).enumerate() {
            let entry = &levels[level];
            let at = buffer + size * slot as u32;
            let [_, _, kind, state, id, submenu, _, _, _, _, _, bitmap] = words::<12>(env, at);
            let (kind, state, submenu, bitmap) =
                (kind as u32, state as u32, submenu as u32, bitmap as u32);
            let flags = if ok == 0 {
                0x800
            } else {
                kind | state | if submenu != 0 { 0x10 } else { 0 }
            };
            let window = &part.windows[entry.window];
            let item = MenuItem {
                text: wide(env, at + MENU_ITEM_INFO, TEXT)
                    .encode_utf16()
                    .collect(),
                flags,
                supported: flags & (0x4 | 0x100) == 0 && bitmap == 0,
                command: MenuCommand {
                    window: window.hwnd,
                    root: window.id,
                    menu: entry.menu,
                    position: item,
                    id: id as u32,
                },
                submenu,
                children: Vec::new(),
            };
            let menu = &mut part.windows[entry.window].menu;
            let siblings = entry
                .path
                .iter()
                .fold(menu, |items, &index| &mut items[index].children);
            siblings.push(item);
            if submenu != 0 {
                let mut path = entry.path.clone();
                path.push(siblings.len() - 1);
                next.push((entry.window, path, submenu));
            }
        }
        let _ = env.sys.vm.free(env.cpu, buffer);
        let list = next
            .iter()
            .map(|&(_, _, submenu)| ("GetMenuItemCount", vec![submenu]))
            .collect();
        calls(env, list, move |env, counts| {
            let levels = next
                .into_iter()
                .zip(counts)
                .filter(|(_, count)| (*count as i32) > 0)
                .map(|((window, path, menu), count)| Level {
                    window,
                    path,
                    menu,
                    count: count.min(ITEMS),
                })
                .collect();
            menu(env, context, part, levels, depth + 1, done)
        })
    })
}

fn finish(env: &mut Env<'_>, context: CtxId, part: Part, done: Done) -> Outcome {
    let projection = projection(env.ext);
    projection.reading.remove(&context);
    projection.read.insert(context, Instant::now());
    let shown: Vec<_> = part
        .windows
        .iter()
        .filter(|info| info.is("#32768") && info.visible() && info.id != 0)
        .map(|info| (info.hwnd, info.id))
        .collect();
    if projection.popup.is_none()
        && let Some(&(hwnd, _)) = shown.first()
    {
        projection.popup = Some((hwnd, Arc::new(AtomicBool::new(true))));
    }
    let next = projection
        .choice
        .as_ref()
        .and_then(|path| path.front())
        .map(|command| command.menu);
    projection.parts.insert(context, part);
    if let Some(menu) = next
        && shown.iter().any(|&(_, id)| id == menu)
    {
        return click_next(env, menu, done);
    }
    done(env)
}

/// Forget a destroyed window.
pub(super) fn forget(ext: &mut Ext, hwnd: u32) {
    if let Some(projection) = ext.try_get_mut::<Projection>() {
        if projection
            .popup
            .as_ref()
            .is_some_and(|(window, _)| *window == hwnd)
        {
            let (_, active) = projection.popup.take().unwrap();
            active.store(false, Ordering::Release);
            projection.choice = None;
        }
        for part in projection.parts.values_mut() {
            part.windows.retain(|info| info.hwnd != hwnd);
        }
    }
}

/// Read a shown top-level window before its first surface flush, so a menu
/// popup or dialog the host projects never presents a frame.
pub(super) fn shown(env: &mut Env<'_>, hwnd: u32) -> Outcome {
    const GWL_STYLE: u32 = -16i32 as u32;
    const WS_CHILD: u32 = 0x4000_0000;
    calls(
        env,
        vec![("GetWindowLongW", vec![hwnd, GWL_STYLE])],
        move |env, results| {
            if results[0] & WS_CHILD == 0 {
                read(env, env.context, Box::new(|_| Outcome::Return(0)))
            } else {
                Outcome::Return(0)
            }
        },
    )
}

/// Each submenu's show callback advances the click path only after Wine
/// has laid it out. Hardware input goes through the owning thread's queue.
fn click_next(env: &mut Env<'_>, menu: u32, done: Done) -> Outcome {
    let state = projection(env.ext);
    let Some(command) = state
        .choice
        .as_ref()
        .and_then(|path| path.front())
        .copied()
        .filter(|command| command.menu == menu)
    else {
        return done(env);
    };
    state.choice.as_mut().unwrap().pop_front();
    let buffer = match env.sys.vm.alloc(env.cpu, 16, Prot::RW) {
        Ok(buffer) => buffer,
        Err(error) => return Outcome::Fail(format!("{MODULE}: {error}")),
    };
    calls(
        env,
        vec![("GetMenuItemRect", vec![0, menu, command.position, buffer])],
        move |env, results| {
            let [left, top, right, bottom] = words::<4>(env, buffer);
            let _ = env.sys.vm.free(env.cpu, buffer);
            if results[0] == 0 {
                return done(env);
            }
            match input::click(env, command.window, (left + right) / 2, (top + bottom) / 2) {
                Ok(()) => done(env),
                Err(error) => Outcome::Fail(error),
            }
        },
    )
}
