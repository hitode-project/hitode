//! Game view events for the mobile overlay (AND-APP-16): the screen layout
//! as `game_layout`, three-finger taps as `game_keyboard`, and the guest's
//! dialogs as `game_dialogs` and menus as `game_menu`, which the overlay
//! answers with the `game_dialog` and `game_menu` commands.
use std::collections::HashMap;

use adv_shell::mobile::{Insets, Layout, Rect};
use adv_shell::window::{
    DialogControl, DialogEvent, DialogSet, DialogTarget, MenuBar, MenuCommand, MenuEvent, MenuItem,
    ViewEvent,
};
use serde::{Deserialize, Serialize};
use tauri::{Emitter, WebviewWindow};

#[derive(Clone, Serialize)]
struct Area {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

impl From<Rect> for Area {
    fn from(rect: Rect) -> Self {
        Self {
            x: rect.x,
            y: rect.y,
            width: rect.width,
            height: rect.height,
        }
    }
}

#[derive(Clone, Serialize)]
struct Edges {
    top: f64,
    right: f64,
    bottom: f64,
    left: f64,
}

impl From<Insets> for Edges {
    fn from(insets: Insets) -> Self {
        Self {
            top: insets.top,
            right: insets.right,
            bottom: insets.bottom,
            left: insets.left,
        }
    }
}

/// The `game_layout` payload, in CSS pixels of the webview.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct GameLayout {
    width: f64,
    height: f64,
    game: Area,
    cutouts: Vec<Area>,
    corner_radius: f64,
    gestures: Edges,
}

impl From<Layout> for GameLayout {
    fn from(layout: Layout) -> Self {
        Self {
            width: layout.width,
            height: layout.height,
            game: layout.game.into(),
            cutouts: layout
                .occlusion
                .cutouts
                .into_iter()
                .map(Area::from)
                .collect(),
            corner_radius: layout.occlusion.corner_radius,
            gestures: layout.occlusion.gestures.into(),
        }
    }
}

/// Forwards a game view's events to the overlay in `window`; `cursor`
/// draws the trackpad cursor where the host must.
pub fn listener(
    window: WebviewWindow,
    cursor: impl Fn(Option<(f32, f32)>) + 'static,
) -> Box<dyn Fn(ViewEvent)> {
    Box::new(move |event| match event {
        ViewEvent::Layout(layout) => {
            let _ = window.emit("game_layout", GameLayout::from(layout));
        }
        ViewEvent::Keyboard => {
            let _ = window.emit("game_keyboard", ());
        }
        ViewEvent::Cursor(point) => cursor(point),
        ViewEvent::Dialogs(set) => {
            let _ = window.emit("game_dialogs", &dialogs(&set));
        }
        ViewEvent::Menu(bar) => {
            let _ = window.emit("game_menu", &menu(&bar));
        }
    })
}

/// A guest window a dialog event names, as the overlay echoes it back.
#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Target {
    dialog: u32,
    hwnd: u32,
    parent: u32,
    id: u32,
    created_us: u64,
}

impl From<DialogTarget> for Target {
    fn from(t: DialogTarget) -> Self {
        Self {
            dialog: t.dialog,
            hwnd: t.hwnd,
            parent: t.parent,
            id: t.id,
            created_us: t.created_us,
        }
    }
}

impl From<Target> for DialogTarget {
    fn from(t: Target) -> Self {
        Self {
            dialog: t.dialog,
            hwnd: t.hwnd,
            parent: t.parent,
            id: t.id,
            created_us: t.created_us,
        }
    }
}

/// One control of a `game_dialogs` dialog, in the dialog's client pixels.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Control {
    target: Target,
    kind: &'static str,
    text: String,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    enabled: bool,
    check: u32,
    items: Vec<String>,
    selection: i32,
    range: Option<(i32, i32)>,
    value: i32,
}

/// A visible guest dialog for the overlay.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Dialog {
    target: Target,
    text: String,
    width: i32,
    height: i32,
    controls: Vec<Control>,
}

/// The control kinds the overlay draws, as the macOS projection sorts them.
fn kind(control: &DialogControl) -> &'static str {
    if !control.supported {
        return "unavailable";
    }
    match control.class.to_ascii_lowercase().as_str() {
        "button" => match control.style & 15 {
            2 | 3 | 5 | 6 => "check",
            4 | 9 => "radio",
            7 => "group",
            _ => "push",
        },
        "static" => match control.style & 0x1f {
            7..=9 => "frame",
            _ => "label",
        },
        "edit" => "edit",
        "combobox" => "combo",
        "listbox" => "list",
        "msctls_trackbar32" => "track",
        "msctls_statusbar32" => "label",
        _ => "unavailable",
    }
}

fn dialogs(set: &DialogSet) -> Vec<Dialog> {
    set.dialogs
        .iter()
        .filter(|d| d.visible)
        .map(|d| {
            // Child rectangles are in their parent's client coordinates, and
            // parents precede their descendants.
            let mut origin = HashMap::from([(d.target.hwnd, (0, 0))]);
            let controls = d
                .controls
                .iter()
                .filter_map(|c| {
                    let (px, py) = *origin.get(&c.target.parent)?;
                    let (x, y) = (px + c.rect.left, py + c.rect.top);
                    origin.insert(c.target.hwnd, (x, y));
                    c.visible.then(|| Control {
                        target: c.target.into(),
                        kind: kind(c),
                        text: caption(&c.text),
                        x,
                        y,
                        width: c.rect.right - c.rect.left,
                        height: c.rect.bottom - c.rect.top,
                        enabled: c.enabled,
                        check: c.check,
                        items: c.items.clone(),
                        selection: c.selection,
                        range: c.range,
                        value: c.value,
                    })
                })
                .collect();
            Dialog {
                target: d.target.into(),
                text: d.text.clone(),
                width: d.client_size.0,
                height: d.client_size.1,
                controls,
            }
        })
        .collect()
}

/// A caption without mnemonics or a menu item's accelerator column.
fn caption(text: &str) -> String {
    let text = text.split('\t').next().unwrap_or_default();
    let mut caption = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '&' || chars.next_if_eq(&'&').is_some() {
            caption.push(c);
        }
    }
    caption
}

/// A guest menu command, as the overlay echoes it back.
#[derive(Clone, Copy, Serialize, Deserialize)]
pub struct Command {
    window: u32,
    root: u32,
    menu: u32,
    position: u32,
    id: u32,
}

impl From<MenuCommand> for Command {
    fn from(c: MenuCommand) -> Self {
        Self {
            window: c.window,
            root: c.root,
            menu: c.menu,
            position: c.position,
            id: c.id,
        }
    }
}

impl From<Command> for MenuCommand {
    fn from(c: Command) -> Self {
        Self {
            window: c.window,
            root: c.root,
            menu: c.menu,
            position: c.position,
            id: c.id,
        }
    }
}

/// One menu entry; a submenu carries its children instead of a command.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
    text: String,
    separator: bool,
    enabled: bool,
    checked: bool,
    command: Command,
    submenu: u32,
    position: u16,
    children: Vec<Entry>,
}

/// The guest's menu bar, or the popup it is tracking, for the overlay.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Menu {
    window: u32,
    root: u32,
    /// A tracked popup at this client point waits for a choice or a cancel.
    popup: Option<(i32, i32)>,
    items: Vec<Entry>,
}

fn entries(items: &[MenuItem], enabled: bool) -> Vec<Entry> {
    items
        .iter()
        .enumerate()
        .map(|(position, item)| {
            let enabled = enabled && item.supported && item.flags & 3 == 0;
            Entry {
                text: if item.supported {
                    caption(&String::from_utf16_lossy(&item.text))
                } else {
                    "Unavailable menu item".into()
                },
                separator: item.flags & 0x800 != 0,
                enabled,
                checked: item.flags & 8 != 0,
                command: item.command.into(),
                submenu: item.submenu,
                position: position as u16,
                children: entries(&item.children, enabled),
            }
        })
        .collect()
}

fn menu(bar: &MenuBar) -> Option<Menu> {
    let popup = bar.popup.as_ref().map(|p| (p.x, p.y));
    let present = bar.key.menu != 0 && !bar.items.is_empty();
    // A finished popup leaves nothing to answer.
    let live = bar
        .popup
        .as_ref()
        .is_none_or(|p| p.active.load(std::sync::atomic::Ordering::Acquire));
    (present && live).then(|| Menu {
        window: bar.key.window,
        root: bar.key.menu,
        popup,
        items: entries(&bar.items, bar.key.enabled),
    })
}

/// What the overlay did with a guest menu.
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum MenuInput {
    /// The bar or popup opened (`menu` 0), or one of its submenus did.
    Open {
        window: u32,
        root: u32,
        menu: u32,
        position: Option<u16>,
    },
    /// A popup choice also retires the guest's tracking, as on macOS.
    Choose {
        command: Command,
        popup: bool,
    },
    Cancel {
        window: u32,
        root: u32,
    },
}

pub fn menu_events(input: MenuInput) -> Vec<MenuEvent> {
    match input {
        MenuInput::Open {
            window,
            root,
            menu,
            position,
        } => vec![MenuEvent::Initialize {
            window,
            root,
            menu: if menu == 0 { root } else { menu },
            position,
        }],
        MenuInput::Choose { command, popup } => {
            let mut events = vec![MenuEvent::Command(command.into())];
            if popup {
                events.push(MenuEvent::Cancel {
                    window: command.window,
                    root: command.root,
                });
            }
            events
        }
        MenuInput::Cancel { window, root } => vec![MenuEvent::Cancel { window, root }],
    }
}

/// What the overlay did to a dialog control.
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum DialogInput {
    Click { target: Target },
    Select { target: Target, index: i32 },
    Track { target: Target, value: i32 },
    Text { target: Target, text: String },
    Close { target: Target },
}

/// The guest events for an overlay action; a released trackbar reports
/// TB_THUMBPOSITION then TB_ENDTRACK, as on macOS.
pub fn dialog_events(input: DialogInput) -> Vec<DialogEvent> {
    match input {
        DialogInput::Click { target } => vec![DialogEvent::Click(target.into())],
        DialogInput::Select { target, index } => vec![DialogEvent::Select {
            target: target.into(),
            index,
        }],
        DialogInput::Track { target, value } => [4, 8]
            .map(|code| DialogEvent::Track {
                target: target.into(),
                value,
                code,
            })
            .into(),
        DialogInput::Text { target, text } => vec![DialogEvent::Text {
            target: target.into(),
            text,
        }],
        DialogInput::Close { target } => vec![DialogEvent::Close(target.into())],
    }
}

/// The overlay's touch mode names.
pub fn touch_mode(mode: &str) -> Result<adv_shell::mobile::TouchMode, String> {
    match mode {
        "direct" => Ok(adv_shell::mobile::TouchMode::Direct),
        "trackpad" => Ok(adv_shell::mobile::TouchMode::Trackpad),
        _ => Err(format!("unknown touch mode {mode}")),
    }
}
