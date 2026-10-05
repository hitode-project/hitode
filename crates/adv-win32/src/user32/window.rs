//! The window model: classes, windows, geometry, and the window-management API.
//!
//! Windows live in the common object table. A window procedure is a guest address, which may
//! be a host thunk (built-in classes, DefWindowProc, PAL's HLE windows), so messages are
//! delivered the same way to both: an [`Outcome::Call`] on the current context (see
//! [`super::message::send`]). Coordinates follow Win32: a top-level window's rect is in
//! screen coordinates, a child's in its parent's client coordinates.

use std::collections::{BTreeMap, HashMap, VecDeque};

use adv_abi::Registry;
use adv_cpu::backend::{Guest, GuestExt};
use adv_machine::{CtxId, Env, Handle, Identity, Namespace, Objects, Outcome};
use adv_os::Heaps;

use super::message::{send, send_all};
use super::{USER32, user, user_mut};
use crate::util::{cstr, err, put_cstr, set_last_error};
use crate::{Win32, stdcall};

pub const WS_OVERLAPPED: u32 = 0;
pub const WS_MAXIMIZE: u32 = 0x0100_0000;
pub const WS_MINIMIZE: u32 = 0x2000_0000;
pub const WS_CHILD: u32 = 0x4000_0000;
pub const WS_POPUP: u32 = 0x8000_0000;
pub const WS_VISIBLE: u32 = 0x1000_0000;
pub const WS_DISABLED: u32 = 0x0800_0000;
pub const WS_CAPTION: u32 = 0x00c0_0000;
pub const WS_BORDER: u32 = 0x0080_0000;
pub const WS_DLGFRAME: u32 = 0x0040_0000;
pub const WS_THICKFRAME: u32 = 0x0004_0000;
pub const WS_TABSTOP: u32 = 0x0001_0000;
pub const WS_EX_DLGMODALFRAME: u32 = 0x1;
pub const WS_EX_NOPARENTNOTIFY: u32 = 0x4;
pub const WS_EX_CLIENTEDGE: u32 = 0x200;

pub const CW_USEDEFAULT: u32 = 0x8000_0000;

/// Extra window bytes of dialog windows (DWL_MSGRESULT, DWL_DLGPROC, DWL_USER, ...).
pub const DLGWINDOWEXTRA: u32 = 30;
pub const DWL_MSGRESULT: u32 = 0;
pub const DWL_DLGPROC: u32 = 4;

pub mod wm {
    pub const CREATE: u32 = 0x01;
    pub const DESTROY: u32 = 0x02;
    pub const MOVE: u32 = 0x03;
    pub const SIZE: u32 = 0x05;
    pub const ACTIVATE: u32 = 0x06;
    pub const SETFOCUS: u32 = 0x07;
    pub const KILLFOCUS: u32 = 0x08;
    pub const ENABLE: u32 = 0x0a;
    pub const SETTEXT: u32 = 0x0c;
    pub const GETTEXT: u32 = 0x0d;
    pub const GETTEXTLENGTH: u32 = 0x0e;
    pub const PAINT: u32 = 0x0f;
    pub const CLOSE: u32 = 0x10;
    pub const QUIT: u32 = 0x12;
    pub const ERASEBKGND: u32 = 0x14;
    pub const SHOWWINDOW: u32 = 0x18;
    pub const ACTIVATEAPP: u32 = 0x1c;
    pub const SETCURSOR: u32 = 0x20;
    pub const MOUSEACTIVATE: u32 = 0x21;
    pub const SETFONT: u32 = 0x30;
    pub const GETFONT: u32 = 0x31;
    pub const WINDOWPOSCHANGING: u32 = 0x46;
    pub const WINDOWPOSCHANGED: u32 = 0x47;
    pub const NCCREATE: u32 = 0x81;
    pub const NCDESTROY: u32 = 0x82;
    pub const NCHITTEST: u32 = 0x84;
    pub const GETDLGCODE: u32 = 0x87;
    pub const KEYDOWN: u32 = 0x100;
    pub const KEYUP: u32 = 0x101;
    pub const CHAR: u32 = 0x102;
    pub const SYSKEYDOWN: u32 = 0x104;
    pub const SYSCHAR: u32 = 0x106;
    pub const INITDIALOG: u32 = 0x110;
    pub const COMMAND: u32 = 0x111;
    pub const SYSCOMMAND: u32 = 0x112;
    pub const TIMER: u32 = 0x113;
    pub const CTLCOLORMSGBOX: u32 = 0x132;
    pub const CTLCOLORSTATIC: u32 = 0x138;
    pub const MOUSEMOVE: u32 = 0x200;
    pub const LBUTTONDOWN: u32 = 0x201;
    pub const LBUTTONUP: u32 = 0x202;
    pub const RBUTTONDOWN: u32 = 0x204;
    pub const RBUTTONUP: u32 = 0x205;
    pub const MOUSEWHEEL: u32 = 0x20a;
    pub const PARENTNOTIFY: u32 = 0x210;
    pub const DWMNCRENDERINGCHANGED: u32 = 0x31f;
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    pub fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self {
            left: x,
            top: y,
            right: x + w,
            bottom: y + h,
        }
    }
    pub fn width(&self) -> i32 {
        self.right - self.left
    }
    pub fn height(&self) -> i32 {
        self.bottom - self.top
    }
    pub fn offset(&self, dx: i32, dy: i32) -> Self {
        Self {
            left: self.left + dx,
            top: self.top + dy,
            right: self.right + dx,
            bottom: self.bottom + dy,
        }
    }
    pub fn read(cpu: &dyn Guest, p: u32) -> Option<Self> {
        let v = |i: u32| cpu.read_u32(p + 4 * i).ok().map(|v| v as i32);
        Some(Self {
            left: v(0)?,
            top: v(1)?,
            right: v(2)?,
            bottom: v(3)?,
        })
    }
    pub fn write(&self, cpu: &mut dyn Guest, p: u32) -> bool {
        [self.left, self.top, self.right, self.bottom]
            .iter()
            .enumerate()
            .all(|(i, &v)| cpu.write_u32(p + 4 * i as u32, v as u32).is_ok())
    }
}

/// Non-client insets (left, top, right, bottom) of a window with these styles, using the
/// same metrics as `GetSystemMetrics`.
pub fn nc_insets(style: u32, ex_style: u32) -> (i32, i32, i32, i32) {
    let frame = if style & WS_THICKFRAME != 0 {
        8
    } else if style & WS_DLGFRAME != 0 || ex_style & WS_EX_DLGMODALFRAME != 0 {
        3
    } else if style & WS_BORDER != 0 {
        1
    } else {
        0
    };
    let edge = if ex_style & WS_EX_CLIENTEDGE != 0 {
        2
    } else {
        0
    };
    let caption = if style & WS_CAPTION == WS_CAPTION {
        23
    } else {
        0
    };
    let s = frame + edge;
    (s, s + caption, s, s)
}

pub struct Class {
    pub name: String,
    pub atom: u16,
    pub style: u32,
    pub proc: u32,
    pub cls_extra: u32,
    pub wnd_extra: u32,
    /// 0 for system (global) classes.
    pub instance: u32,
    pub icon: u32,
    pub cursor: u32,
    pub background: u32,
    pub menu_name: u32,
}

/// Per-window state of built-in controls and dialogs.
#[derive(Default)]
pub struct ControlState {
    /// Button check state (BST_*), and BST_PUSHED while the mouse is down.
    pub button: u32,
    /// Dialog: EndDialog result once called.
    pub dialog_end: Option<u32>,
    /// Dialog: created by DialogBoxParam (a modal loop is running).
    pub modal: bool,
    /// Dialog: base units (DLU -> pixels).
    pub base_units: (i32, i32),
    pub trackbar: Option<TrackbarState>,
    pub choices: Option<ChoiceState>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackbarState {
    pub min: i32,
    pub max: i32,
    pub value: i32,
}
impl Default for TrackbarState {
    fn default() -> Self {
        Self {
            min: 0,
            max: 100,
            value: 0,
        }
    }
}

#[derive(Default)]
pub struct ChoiceState {
    pub items: Vec<String>,
    pub data: Vec<u32>,
    pub selected: Option<usize>,
}

pub struct Window {
    pub hwnd: u32,
    pub class: usize,
    pub proc: u32,
    pub style: u32,
    pub ex_style: u32,
    /// 0 for top-level windows.
    pub parent: u32,
    pub owner: u32,
    /// Control id (the `hMenu` argument of child windows).
    pub id: u32,
    pub instance: u32,
    /// Screen coordinates for top-level windows, parent-client coordinates for children.
    pub rect: Rect,
    pub text: String,
    pub extra: Vec<u8>,
    pub user_data: u32,
    pub thread: CtxId,
    /// Child windows, in z-order (top first).
    pub children: Vec<u32>,
    pub font: u32,
    pub needs_paint: bool,
    pub created_us: u64,
    /// Native overlapped windows defer their first SIZE/MOVE until first show.
    pub initial_geometry_reported: bool,
    pub destroying: bool,
    pub control: ControlState,
    /// Headless dialog policy: auto clicks done so far.
    pub auto_clicks: u32,
}

/// Typed aliases and a non-owning traversal index over the common object table.
/// Iterating windows does not scan unrelated file, audio or GDI objects.
pub struct Windows {
    namespace: Namespace,
    index: BTreeMap<u32, Identity<Window>>,
}
impl Windows {
    fn new(objects: &mut Objects) -> Option<Self> {
        Some(Self {
            namespace: objects.new_namespace()?,
            index: BTreeMap::new(),
        })
    }
    pub fn get<'a>(&self, objects: &'a Objects, hwnd: &u32) -> Option<&'a Window> {
        objects.owned(*self.index.get(hwnd)?)
    }
    pub fn get_mut<'a>(&self, objects: &'a mut Objects, hwnd: &u32) -> Option<&'a mut Window> {
        objects.owned_mut(*self.index.get(hwnd)?)
    }
    pub fn contains_key(&self, objects: &Objects, hwnd: &u32) -> bool {
        self.get(objects, hwnd).is_some()
    }
    pub fn values<'a>(&'a self, objects: &'a Objects) -> impl Iterator<Item = &'a Window> {
        self.index
            .values()
            .filter_map(|&identity| objects.owned(identity))
    }
    pub fn len(&self) -> usize {
        self.index.len()
    }
    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }
    pub fn for_each_mut(&self, objects: &mut Objects, mut apply: impl FnMut(&mut Window)) {
        for &identity in self.index.values() {
            if let Some(window) = objects.owned_mut(identity) {
                apply(window);
            }
        }
    }
    pub fn insert(&mut self, objects: &mut Objects, hwnd: u32, window: Window) -> bool {
        if !objects.insert_host_at(self.namespace, hwnd, window) {
            return false;
        }
        self.index
            .insert(hwnd, objects.lookup_alias(self.namespace, hwnd).unwrap());
        true
    }
    fn remove(&mut self, objects: &mut Objects, hwnd: &u32) -> Option<Window> {
        let Some(adv_machine::Object::Host(value)) = objects.close_alias(self.namespace, *hwnd)?
        else {
            return None;
        };
        self.index.remove(hwnd);
        value.downcast::<Window>().ok().map(|window| *window)
    }
}

impl Window {
    pub fn visible(&self) -> bool {
        self.style & WS_VISIBLE != 0
    }
    pub fn enabled(&self) -> bool {
        self.style & WS_DISABLED == 0
    }
    pub fn client_size(&self) -> (i32, i32) {
        let (l, t, r, b) = nc_insets(self.style, self.ex_style);
        (
            (self.rect.width() - l - r).max(0),
            (self.rect.height() - t - b).max(0),
        )
    }
    fn extra_u32(&self, off: u32) -> Option<u32> {
        let b = self.extra.get(off as usize..off as usize + 4)?;
        Some(u32::from_le_bytes(b.try_into().unwrap()))
    }
    fn set_extra_u32(&mut self, off: u32, v: u32) -> Option<u32> {
        let old = self.extra_u32(off)?;
        self.extra[off as usize..off as usize + 4].copy_from_slice(&v.to_le_bytes());
        Some(old)
    }
    pub fn dlgproc(&self) -> u32 {
        if self.extra.len() >= DLGWINDOWEXTRA as usize {
            self.extra_u32(DWL_DLGPROC).unwrap_or(0)
        } else {
            0
        }
    }
}

/// Thread message queue and per-thread input state.
pub struct QueueKeys(pub [u8; 256]);
impl Default for QueueKeys {
    fn default() -> Self {
        Self([0; 256])
    }
}

#[derive(Default)]
pub struct Queue {
    pub(super) sent: super::message::sent::Queue,
    pub posted: std::collections::VecDeque<super::message::Msg>,
    /// Logical key state of input already removed from this thread's queue.
    pub keyboard: QueueKeys,
    pub quit: Option<u32>,
    /// Input not yet observed by Peek/GetMessage or GetQueueStatus.
    pub changed_status: u32,
    /// Last observed availability of generated paint/timer messages.
    pub generated_status: u32,
    /// Manual-reset event signaled while messages are pending (0 until someone waits).
    pub event: Handle,
    /// New input matching the most recent MsgWait wake mask. Kept separate
    /// from GetMessage's queue-change event so old/unmatched input cannot
    /// satisfy or partially consume a multi-object wait.
    pub message_wait_event: Handle,
    pub message_wait_notification: Handle,
    pub message_wait_mask: u32,
    pub timers: Vec<super::message::Timer>,
    pub focus: u32,
    pub active: u32,
    pub capture: u32,
    pub last_time: u32,
    pub last_pos: (i32, i32),
}

impl Queue {
    /// Append a message and preserve its new-input category until observation.
    pub fn post(&mut self, message: super::message::Msg) {
        self.changed_status |= super::message::posted_status(&message);
        self.posted.push_back(message);
    }
}

/// Thunk addresses of the built-in window procedures.
#[derive(Default, Clone, Copy)]
pub struct SysProcs {
    pub def_window: u32,
    pub def_dlg: u32,
    pub button: u32,
    pub static_: u32,
    pub edit: u32,
    pub combo: u32,
    pub trackbar: u32,
}

pub struct User {
    pub classes: Vec<Class>,
    next_atom: u16,
    pub windows: Windows,
    /// Top-level windows in z-order (top first).
    pub top: Vec<u32>,
    next_hwnd: u32,
    pub desktop: u32,
    pub queues: HashMap<CtxId, Queue>,
    pub procs: SysProcs,
    pub screen: (i32, i32),
    /// Headless: click a dialog's default button after this many ms (None: never).
    pub auto_dialog_ms: Option<u32>,
    /// Cursors, icons and other USER objects: handle -> description.
    pub objects: super::resources::UserObjects,
    pub menus: super::menu::Menus,
    /// Key state by virtual-key code (bit 7: down, bit 0: toggled).
    pub keys: [u8; 256],
    /// Optional native frontend state, independently published from its input queue.
    pub host_keys: Option<std::sync::Arc<adv_sync::input::KeyState>>,
    /// Opt-in host input receipts. Guest message layouts and scheduling stay
    /// unchanged; ordinary interactive execution leaves this disabled.
    pub input_tracking: Option<Box<super::message::InputTracking>>,
    /// SM_SWAPBUTTON, shared by GetSystemMetrics and PAL input sampling.
    pub swap_buttons: bool,
    /// The guest's installed input profile, shared by all its current threads.
    pub keyboard_layout: u32,
    pub icon_title_font: super::system_parameters::IconFont,
    pub clipboard: adv_os::clipboard::Clipboard,
    pub cursor_pos: (i32, i32),
    pub cursor_count: i32,
    pub locked_window: Option<u32>,
}

/// Handles of USER objects loaded from the system (IDC_*, IDI_*), by resource id.
pub const SYSTEM_OBJECT_BASE: u32 = 0x0006_0000;
const HWND_BASE: u32 = 0x0001_0010;
const HWND_STEP: u32 = 0x10;
pub const HMONITOR_PRIMARY: u32 = 0x0001_0001;

impl User {
    pub fn new(
        objects: &mut Objects,
        screen: (i32, i32),
        auto_dialog_ms: Option<u32>,
    ) -> Option<Self> {
        let mut u = Self {
            classes: Vec::new(),
            next_atom: 0xc000,
            windows: Windows::new(objects)?,
            top: Vec::new(),
            next_hwnd: HWND_BASE,
            desktop: 0,
            queues: HashMap::new(),
            procs: SysProcs::default(),
            screen,
            auto_dialog_ms,
            objects: super::resources::UserObjects::default(),
            menus: super::menu::Menus::default(),
            keys: [0; 256],
            host_keys: None,
            input_tracking: None,
            swap_buttons: false,
            keyboard_layout: 0x0411_0411,
            icon_title_font: super::system_parameters::IconFont::default(),
            clipboard: adv_os::clipboard::Clipboard::default(),
            cursor_pos: (screen.0 / 2, screen.1 / 2),
            cursor_count: 0,
            locked_window: None,
        };
        let desktop = u.add_class_raw("#32769", 0, 0, 0, 0);
        u.desktop = u.alloc_hwnd()?;
        let rect = Rect::new(0, 0, screen.0, screen.1);
        if !u.windows.insert(
            objects,
            u.desktop,
            Window::new(u.desktop, desktop, 0, WS_VISIBLE | WS_POPUP, 0, 0, rect, 0),
        ) {
            return None;
        }
        Some(u)
    }

    fn alloc_hwnd(&mut self) -> Option<u32> {
        let h = self.next_hwnd;
        self.next_hwnd = self.next_hwnd.checked_add(HWND_STEP)?;
        Some(h)
    }

    fn add_class_raw(
        &mut self,
        name: &str,
        style: u32,
        proc: u32,
        wnd_extra: u32,
        instance: u32,
    ) -> usize {
        let atom = self.next_atom;
        self.next_atom += 1;
        self.classes.push(Class {
            name: name.into(),
            atom,
            style,
            proc,
            cls_extra: 0,
            wnd_extra,
            instance,
            icon: 0,
            cursor: 0,
            background: 0,
            menu_name: 0,
        });
        self.classes.len() - 1
    }

    /// RegisterClassEx for host code (PAL's window class). Returns the atom.
    #[allow(clippy::too_many_arguments)]
    pub fn register_class(
        &mut self,
        name: &str,
        style: u32,
        proc: u32,
        wnd_extra: u32,
        instance: u32,
        icon: u32,
        cursor: u32,
        background: u32,
    ) -> u16 {
        let i = self.add_class_raw(name, style, proc, wnd_extra, instance);
        let c = &mut self.classes[i];
        (c.icon, c.cursor, c.background) = (icon, cursor, background);
        c.atom
    }

    /// Register the built-in classes once their procedures' thunks are known.
    pub fn add_system_classes(&mut self, procs: SysProcs) {
        self.procs = procs;
        self.classes[0].proc = procs.def_window;
        self.add_class_raw("Button", 0, procs.button, 0, 0);
        self.add_class_raw("Static", 0, procs.static_, 0, 0);
        self.add_class_raw("Edit", 0, procs.edit, 0, 0);
        self.add_class_raw("ComboBox", 0, procs.combo, 0, 0);
        self.add_class_raw("msctls_trackbar32", 0, procs.trackbar, 0, 0);
        for c in ["ListBox", "ScrollBar", "msctls_progress32", "SysListView32"] {
            self.add_class_raw(c, 0, procs.def_window, 0, 0);
        }
        self.add_class_raw("#32770", 0, procs.def_dlg, DLGWINDOWEXTRA, 0);
    }

    /// Find a class by name or atom: the instance's own classes first, then global ones.
    pub fn find_class(&self, key: &ClassKey, instance: u32) -> Option<usize> {
        let matches = |c: &Class| match key {
            ClassKey::Atom(a) => c.atom == *a,
            ClassKey::Name(n) => c.name.eq_ignore_ascii_case(n),
        };
        let pos = |local: bool| {
            self.classes
                .iter()
                .position(|c| matches(c) && (c.instance == instance) == local && !c.name.is_empty())
        };
        pos(true).or_else(|| {
            self.classes
                .iter()
                .position(|c| matches(c) && c.instance == 0 && !c.name.is_empty())
        })
    }

    pub fn queue(&mut self, ctx: CtxId) -> &mut Queue {
        self.queues.entry(ctx).or_default()
    }

    /// Screen position of `hwnd`'s client-area origin.
    pub fn client_origin(&self, objects: &Objects, hwnd: u32) -> (i32, i32) {
        let Some(w) = self.windows.get(objects, &hwnd) else {
            return (0, 0);
        };
        let (l, t, _, _) = nc_insets(w.style, w.ex_style);
        let (px, py) = if w.parent != 0 {
            self.client_origin(objects, w.parent)
        } else {
            (0, 0)
        };
        (px + w.rect.left + l, py + w.rect.top + t)
    }

    /// Window rect in screen coordinates.
    pub fn screen_rect(&self, objects: &Objects, hwnd: u32) -> Option<Rect> {
        let w = self.windows.get(objects, &hwnd)?;
        let (px, py) = if w.parent != 0 {
            self.client_origin(objects, w.parent)
        } else {
            (0, 0)
        };
        Some(w.rect.offset(px, py))
    }

    /// `hwnd` and all its descendants, parents before children.
    pub fn subtree(&self, objects: &Objects, hwnd: u32) -> Vec<u32> {
        let mut out = vec![hwnd];
        let mut i = 0;
        while i < out.len() {
            if let Some(w) = self.windows.get(objects, &out[i]) {
                out.extend(w.children.iter().copied());
            }
            i += 1;
        }
        out
    }

    pub fn is_window(&self, objects: &Objects, hwnd: u32) -> bool {
        hwnd != 0 && self.windows.contains_key(objects, &hwnd)
    }

    /// The top-level ancestor of `hwnd`.
    pub fn root(&self, objects: &Objects, mut hwnd: u32) -> u32 {
        while let Some(w) = self.windows.get(objects, &hwnd) {
            if w.parent == 0 {
                break;
            }
            hwnd = w.parent;
        }
        hwnd
    }

    fn remove_window(&mut self, hwnd: u32, objects: &mut adv_machine::Objects) {
        if self.locked_window == Some(hwnd) {
            self.locked_window = None;
        }
        self.clipboard.remove_window(hwnd);
        self.menus.remove_window(hwnd, objects);
        let Some(w) = self.windows.remove(objects, &hwnd) else {
            return;
        };
        if w.parent != 0 {
            if let Some(p) = self.windows.get_mut(objects, &w.parent) {
                p.children.retain(|&c| c != hwnd);
            }
        } else {
            self.top.retain(|&t| t != hwnd);
        }
        for q in self.queues.values_mut() {
            for f in [&mut q.focus, &mut q.active, &mut q.capture] {
                if *f == hwnd {
                    *f = 0;
                }
            }
            q.timers.retain(|t| t.hwnd != hwnd);
        }
    }
}

impl Window {
    #[allow(clippy::too_many_arguments)]
    fn new(
        hwnd: u32,
        class: usize,
        proc: u32,
        style: u32,
        ex_style: u32,
        parent: u32,
        rect: Rect,
        thread: CtxId,
    ) -> Self {
        Self {
            hwnd,
            class,
            proc,
            style,
            ex_style,
            parent,
            owner: 0,
            id: 0,
            instance: 0,
            rect,
            text: String::new(),
            extra: Vec::new(),
            user_data: 0,
            thread,
            children: Vec::new(),
            font: 0,
            needs_paint: false,
            created_us: 0,
            initial_geometry_reported: false,
            destroying: false,
            control: ControlState::default(),
            auto_clicks: 0,
        }
    }
}

pub enum ClassKey {
    Atom(u16),
    Name(String),
}

impl ClassKey {
    pub fn read(env: &Env<'_>, p: u32) -> Self {
        if p < 0x1_0000 {
            ClassKey::Atom(p as u16)
        } else {
            ClassKey::Name(env.ext.get::<Win32>().codec.decode(&cstr(env.cpu, p)))
        }
    }
}

// ---- classes ----

fn register_class_a(env: &mut Env<'_>) -> Outcome {
    // WNDCLASSA: style, lpfnWndProc, cbClsExtra, cbWndExtra, hInstance, hIcon, hCursor,
    // hbrBackground, lpszMenuName, lpszClassName.
    let p = env.arg(0);
    let f: Vec<u32> = (0..10)
        .map(|i| env.cpu.read_u32(p + 4 * i).unwrap_or(0))
        .collect();
    register_class(env, &f, 0, false)
}

fn register_class_ex_a(env: &mut Env<'_>) -> Outcome {
    // WNDCLASSEXA: cbSize, then WNDCLASSA's fields, then hIconSm.
    let p = env.arg(0);
    let f: Vec<u32> = (1..11)
        .map(|i| env.cpu.read_u32(p + 4 * i).unwrap_or(0))
        .collect();
    let small = env.cpu.read_u32(p + 44).unwrap_or(0);
    register_class(env, &f, small, false)
}

fn register_class_w(env: &mut Env<'_>, extended: bool) -> Outcome {
    let pointer = env.arg(0);
    let start = u32::from(extended);
    let fields: Vec<_> = (start..start + 10)
        .map(|at| env.cpu.read_u32(pointer + at * 4).unwrap_or(0))
        .collect();
    register_class(env, &fields, 0, true)
}

fn register_class(env: &mut Env<'_>, f: &[u32], _small_icon: u32, wide: bool) -> Outcome {
    let key = if wide && f[9] >= 0x1_0000 {
        ClassKey::Name(String::from_utf16_lossy(&crate::util::wstr(env.cpu, f[9])))
    } else {
        ClassKey::read(env, f[9])
    };
    let name = match &key {
        ClassKey::Name(n) => n.clone(),
        ClassKey::Atom(a) => format!("#{a}"),
    };
    let u = &mut env.ext.get_mut::<crate::Win32>().user;
    if u.find_class(&key, f[4])
        .is_some_and(|i| u.classes[i].instance == f[4])
    {
        set_last_error(env, 1410); // ERROR_CLASS_ALREADY_EXISTS
        return Outcome::Return(0);
    }
    let u = &mut env.ext.get_mut::<crate::Win32>().user;
    let i = u.add_class_raw(&name, f[0], f[1], f[3], f[4]);
    let c = &mut u.classes[i];
    (c.cls_extra, c.icon, c.cursor, c.background, c.menu_name) = (f[2], f[5], f[6], f[7], f[8]);
    let atom = c.atom;
    super::menu::class_menu(env, i, f[8], wide);
    log::debug!("RegisterClass({name}) proc {:#010x} extra {}", f[1], f[3]);
    Outcome::Return(u32::from(atom))
}

fn unregister_class_a(env: &mut Env<'_>) -> Outcome {
    let (name, inst) = (env.arg(0), env.arg(1));
    let key = ClassKey::read(env, name);
    let u = &mut env.ext.get_mut::<crate::Win32>().user;
    let Some(i) = u.find_class(&key, inst) else {
        return Outcome::Return(0);
    };
    if u.classes[i].instance == 0 || u.windows.values(&env.sys.objects).any(|w| w.class == i) {
        return Outcome::Return(0);
    }
    // Keep indices stable: blank the slot.
    u.classes[i].name.clear();
    Outcome::Return(1)
}

fn get_class_info_a(env: &mut Env<'_>) -> Outcome {
    let (inst, name, out) = (env.arg(0), env.arg(1), env.arg(2));
    let key = ClassKey::read(env, name);
    let u = user(env);
    let Some(i) = u.find_class(&key, inst) else {
        set_last_error(env, 1411); // ERROR_CLASS_DOES_NOT_EXIST
        return Outcome::Return(0);
    };
    let c = &u.classes[i];
    let f = [
        c.style,
        c.proc,
        c.cls_extra,
        c.wnd_extra,
        c.instance,
        c.icon,
        c.cursor,
        c.background,
        c.menu_name,
        name,
    ];
    let atom = c.atom;
    for (i, v) in f.into_iter().enumerate() {
        let _ = env.cpu.write_u32(out + 4 * i as u32, v);
    }
    Outcome::Return(u32::from(atom))
}

fn get_class_name_a(env: &mut Env<'_>) -> Outcome {
    let (hwnd, buf, cap) = (env.arg(0), env.arg(1), env.arg(2));
    let u = user(env);
    let Some(w) = u.windows.get(&env.sys.objects, &hwnd) else {
        return Outcome::Return(0);
    };
    let name = env
        .ext
        .get::<Win32>()
        .codec
        .encode(&u.classes[w.class].name);
    Outcome::Return(put_cstr(env.cpu, buf, cap, &name).unwrap_or(cap.saturating_sub(1)))
}

// ---- creation and destruction ----

/// Parameters of a window being created.
pub struct CreateParams {
    pub ex_style: u32,
    pub class: ClassKey,
    /// Guest pointers for the CREATESTRUCT (class may be an atom).
    pub class_ptr: u32,
    pub name_ptr: u32,
    pub text: String,
    pub style: u32,
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    pub parent: u32,
    pub menu: u32,
    pub instance: u32,
    pub param: u32,
}

fn create_window_ex_a(env: &mut Env<'_>) -> Outcome {
    let a: Vec<u32> = (0..12).map(|i| env.arg(i)).collect();
    let text = if a[2] == 0 {
        String::new()
    } else {
        env.ext.get::<Win32>().codec.decode(&cstr(env.cpu, a[2]))
    };
    let p = CreateParams {
        ex_style: a[0],
        class: ClassKey::read(env, a[1]),
        class_ptr: a[1],
        name_ptr: a[2],
        text,
        style: a[3],
        x: a[4],
        y: a[5],
        w: a[6],
        h: a[7],
        parent: a[8],
        menu: a[9],
        instance: a[10],
        param: a[11],
    };
    create_window(env, p, |_, hwnd| Outcome::Return(hwnd))
}

/// CreateWindowEx: make the window, then send WM_NCCREATE, WM_CREATE, WM_SIZE, WM_MOVE
/// (and WM_PARENTNOTIFY, and the show messages for WS_VISIBLE). `then` gets the HWND, or 0.
pub fn create_window(
    env: &mut Env<'_>,
    p: CreateParams,
    then: impl FnOnce(&mut Env<'_>, u32) -> Outcome + 'static,
) -> Outcome {
    let ctx = env.context;
    let now = env.sys.clock.now_us();
    let u = &mut env.ext.get_mut::<crate::Win32>().user;
    let Some(ci) = u.find_class(&p.class, p.instance) else {
        let name = match &p.class {
            ClassKey::Name(n) => n.clone(),
            ClassKey::Atom(a) => format!("#{a}"),
        };
        log::warn!("CreateWindowEx: unknown class {name}");
        set_last_error(env, 1407); // ERROR_CANNOT_FIND_WND_CLASS
        return then(env, 0);
    };
    let child = p.style & WS_CHILD != 0;
    let parent = if p.parent == u.desktop { 0 } else { p.parent };
    if child && !u.windows.contains_key(&env.sys.objects, &parent) {
        set_last_error(env, 1406); // ERROR_TLW_WITH_WSCHILD
        return then(env, 0);
    }
    let (sw, sh) = u.screen;
    let (mut x, mut y, mut w, mut h) = (p.x as i32, p.y as i32, p.w as i32, p.h as i32);
    if p.x == CW_USEDEFAULT {
        (x, y) = if child { (0, 0) } else { (sw / 16, sh / 16) };
    }
    if p.w == CW_USEDEFAULT {
        (w, h) = if child {
            (0, 0)
        } else {
            (sw * 3 / 4, sh * 3 / 4)
        };
    }
    // Overlapped windows always have a caption and border.
    let mut style = p.style;
    if style & (WS_CHILD | WS_POPUP) == 0 {
        style |= WS_CAPTION;
    }
    let (class_proc, wnd_extra, class_name) = (
        u.classes[ci].proc,
        u.classes[ci].wnd_extra,
        u.classes[ci].name.clone(),
    );
    let Some(hwnd) = u.alloc_hwnd() else {
        set_last_error(env, 8);
        return then(env, 0);
    };
    let mut win = Window::new(
        hwnd,
        ci,
        class_proc,
        style & !WS_VISIBLE,
        p.ex_style,
        if child { parent } else { 0 },
        Rect::new(x, y, w, h),
        ctx,
    );
    win.owner = if child {
        0
    } else {
        u.root(&env.sys.objects, p.parent)
    };
    win.id = if child { p.menu } else { 0 };
    win.instance = p.instance;
    win.text = p.text;
    win.extra = vec![0; wnd_extra as usize];
    win.created_us = now;
    if !u.windows.insert(&mut env.sys.objects, hwnd, win) {
        set_last_error(env, 8);
        return then(env, 0);
    }
    if child {
        u.windows
            .get_mut(&mut env.sys.objects, &parent)
            .unwrap()
            .children
            .push(hwnd);
    } else {
        u.top.insert(0, hwnd);
    }
    log::debug!("CreateWindowEx({class_name}) = {hwnd:#x} style {style:#x} at ({x},{y}) {w}x{h}");

    let menu = match super::menu::attach_creation(env, hwnd, p.menu, child) {
        Ok(menu) => menu,
        Err(error) => {
            env.ext
                .get_mut::<crate::Win32>()
                .user
                .remove_window(hwnd, &mut env.sys.objects);
            set_last_error(env, error);
            return then(env, 0);
        }
    };

    // CREATESTRUCTA: lpCreateParams, hInstance, hMenu, hwndParent, cy, cx, y, x, style,
    // lpszName, lpszClass, dwExStyle.
    let cs = temp_alloc(env, 48);
    let fields = [
        p.param,
        p.instance,
        if child { p.menu } else { menu },
        p.parent,
        h as u32,
        w as u32,
        y as u32,
        x as u32,
        p.style,
        p.name_ptr,
        p.class_ptr,
        p.ex_style,
    ];
    for (i, v) in fields.into_iter().enumerate() {
        let _ = env.cpu.write_u32(cs + 4 * i as u32, v);
    }
    let visible = p.style & WS_VISIBLE != 0;
    let notify = child && p.ex_style & WS_EX_NOPARENTNOTIFY == 0;
    send(env, hwnd, wm::NCCREATE, 0, cs, move |env, r| {
        if r == 0 {
            temp_free(env, cs);
            // Native USER32 sends NCDESTROY, but not DESTROY, when NCCREATE fails.
            return destroy_boxed(env, hwnd, false, Box::new(move |env, _| then(env, 0)));
        }
        // The virtual desktop is composited. Native USER queues this initial
        // notification after NCCREATE succeeds, before CREATE, for decorated
        // top-level windows. Bare popups, children and HWND_MESSAGE are excluded.
        if p.parent != (-3i32) as u32
            && let Some(win) = user(env).windows.get(&env.sys.objects, &hwnd)
            && win.style & WS_CHILD == 0
            && (win.style & WS_CAPTION == WS_CAPTION || win.style & WS_THICKFRAME != 0)
        {
            super::message::post_system(env, hwnd, wm::DWMNCRENDERINGCHANGED, 1);
        }
        send(env, hwnd, wm::CREATE, 0, cs, move |env, r| {
            temp_free(env, cs);
            if r as i32 == -1 {
                return destroy_window(env, hwnd, move |env, _| then(env, 0));
            }
            let Some(win) = user(env).windows.get(&env.sys.objects, &hwnd) else {
                return then(env, 0);
            };
            let (cw, ch) = win.client_size();
            let (il, it, _, _) = nc_insets(win.style, win.ex_style);
            let (l, t) = (win.rect.left + il, win.rect.top + it);
            // Windows initializes child/popup geometry during CreateWindowEx,
            // but an invisible overlapped window receives SIZE/MOVE on first
            // ShowWindow. Early delivery can enter application state before its
            // caller has finished initializing it.
            let mut msgs = Vec::new();
            if win.style & (WS_CHILD | WS_POPUP) != 0 {
                msgs.extend([
                    [hwnd, wm::SIZE, 0, make_long(cw, ch)],
                    [hwnd, wm::MOVE, 0, make_long(l, t)],
                ]);
                env.ext
                    .get::<crate::Win32>()
                    .user
                    .windows
                    .get_mut(&mut env.sys.objects, &hwnd)
                    .unwrap()
                    .initial_geometry_reported = true;
            }
            if notify {
                msgs.push([
                    parent,
                    wm::PARENTNOTIFY,
                    wm::CREATE | (p.menu & 0xffff) << 16,
                    hwnd,
                ]);
            }
            send_all(env, msgs.into(), move |env| {
                if !visible {
                    return then(env, hwnd);
                }
                show_window(env, hwnd, SW_SHOW, move |env, _| then(env, hwnd))
            })
        })
    })
}

pub fn make_long(lo: i32, hi: i32) -> u32 {
    (lo as u32 & 0xffff) | (hi as u32) << 16
}

/// Scratch guest memory from the process heap (CREATESTRUCT, WINDOWPOS, ...).
pub fn temp_alloc(env: &mut Env<'_>, size: u32) -> u32 {
    let heaps = env.ext.get_mut::<Heaps>();
    let h = heaps.process_heap;
    heaps
        .alloc(env.cpu, &mut env.sys.vm, h, size, true)
        .unwrap_or(0)
}

pub fn temp_free(env: &mut Env<'_>, p: u32) {
    let heaps = env.ext.get_mut::<Heaps>();
    let h = heaps.process_heap;
    heaps.free(env.cpu, &mut env.sys.vm, h, p);
}

fn destroy_window_api(env: &mut Env<'_>) -> Outcome {
    let hwnd = env.arg(0);
    if !user(env)
        .windows
        .get(&env.sys.objects, &hwnd)
        .is_some_and(|w| !w.destroying)
    {
        set_last_error(env, 1400); // ERROR_INVALID_WINDOW_HANDLE
        return Outcome::Return(0);
    }
    destroy_window(env, hwnd, |_, ok| Outcome::Return(u32::from(ok)))
}

type Then = Box<dyn FnOnce(&mut Env<'_>) -> Outcome>;
type ThenBool = Box<dyn FnOnce(&mut Env<'_>, bool) -> Outcome>;

/// DestroyWindow, in Win32's order: windows it owns first; then hide it; WM_DESTROY to it
/// and, after that returns, to each child still alive (so a WM_DESTROY handler that destroys
/// its own children works); finally WM_NCDESTROY children-first, and the tree is removed.
pub fn destroy_window(
    env: &mut Env<'_>,
    hwnd: u32,
    then: impl FnOnce(&mut Env<'_>, bool) -> Outcome + 'static,
) -> Outcome {
    destroy_boxed(env, hwnd, true, Box::new(then))
}

fn destroy_boxed(env: &mut Env<'_>, hwnd: u32, initialized: bool, then: ThenBool) -> Outcome {
    let u = &mut env.ext.get_mut::<crate::Win32>().user;
    if !u
        .windows
        .get(&env.sys.objects, &hwnd)
        .is_some_and(|w| !w.destroying)
    {
        return then(env, false);
    }
    let owned = u.top.iter().copied().find(|t| {
        u.windows
            .get(&env.sys.objects, t)
            .is_some_and(|w| w.owner == hwnd && !w.destroying)
    });
    if let Some(o) = owned {
        return destroy_boxed(
            env,
            o,
            true,
            Box::new(move |env, _| destroy_boxed(env, hwnd, initialized, then)),
        );
    }
    let w = u.windows.get_mut(&mut env.sys.objects, &hwnd).unwrap();
    w.destroying = true;
    let was_visible = w.visible();
    let notify = (initialized && w.parent != 0 && w.ex_style & WS_EX_NOPARENTNOTIFY == 0)
        .then_some((w.parent, w.id));
    let finish: Then = Box::new(move |env| {
        let all = user(env).subtree(&env.sys.objects, hwnd);
        let msgs: VecDeque<[u32; 4]> = all
            .iter()
            .rev()
            .map(|&h| [h, wm::NCDESTROY, 0, 0])
            .collect();
        send_all(env, msgs, move |env| {
            let removed = all.clone();
            super::clipboard::remove_windows(env, &removed, move |env| {
                for h in all.iter().rev() {
                    super::message::sent::cancel_window(env, *h);
                    let u = &mut env.ext.get_mut::<crate::Win32>().user;
                    u.remove_window(*h, &mut env.sys.objects);
                }
                then(env, true)
            })
        })
    });
    let destroy: Then = Box::new(move |env| {
        if initialized {
            send_destroy_tree(env, hwnd, finish)
        } else {
            destroy_children(env, hwnd, finish)
        }
    });
    let hide: Then = Box::new(move |env| {
        if let Some(w) = env
            .ext
            .get::<crate::Win32>()
            .user
            .windows
            .get_mut(&mut env.sys.objects, &hwnd)
        {
            w.style &= !WS_VISIBLE;
        }
        if was_visible {
            return send(env, hwnd, wm::SHOWWINDOW, 0, 0, move |env, _| destroy(env));
        }
        destroy(env)
    });
    // Native USER32 notifies before hiding/destroying an explicitly destroyed child.
    // The recursive subtree path below does not send this notification.
    if let Some((parent, id)) = notify {
        return send(
            env,
            parent,
            wm::PARENTNOTIFY,
            wm::DESTROY | (id & 0xffff) << 16,
            hwnd,
            move |env, _| hide(env),
        );
    }
    hide(env)
}

/// WM_DESTROY to `hwnd`, then to its remaining children, recursively.
fn send_destroy_tree(env: &mut Env<'_>, hwnd: u32, then: Then) -> Outcome {
    send(env, hwnd, wm::DESTROY, 0, 0, move |env, _| {
        destroy_children(env, hwnd, then)
    })
}

fn destroy_children(env: &mut Env<'_>, hwnd: u32, then: Then) -> Outcome {
    let u = &mut env.ext.get_mut::<crate::Win32>().user;
    let next = u.windows.get(&env.sys.objects, &hwnd).and_then(|w| {
        w.children.iter().copied().find(|c| {
            u.windows
                .get(&env.sys.objects, c)
                .is_some_and(|c| !c.destroying)
        })
    });
    let Some(child) = next else { return then(env) };
    u.windows
        .get_mut(&mut env.sys.objects, &child)
        .unwrap()
        .destroying = true;
    send_destroy_tree(
        env,
        child,
        Box::new(move |env| destroy_children(env, hwnd, then)),
    )
}

// ---- showing, moving, activation ----

pub const SW_HIDE: u32 = 0;
pub const SW_SHOWNORMAL: u32 = 1;
pub const SW_SHOWMINIMIZED: u32 = 2;
pub const SW_MAXIMIZE: u32 = 3;
pub const SW_SHOWNOACTIVATE: u32 = 4;
pub const SW_SHOW: u32 = 5;
pub const SW_MINIMIZE: u32 = 6;
pub const SW_SHOWMINNOACTIVE: u32 = 7;
pub const SW_SHOWNA: u32 = 8;
pub const SW_RESTORE: u32 = 9;

fn show_window_api(env: &mut Env<'_>) -> Outcome {
    let (hwnd, cmd) = (env.arg(0), env.arg(1));
    show_window(env, hwnd, cmd, |_, was| Outcome::Return(u32::from(was)))
}

/// ShowWindow. `then` gets whether the window was visible before.
pub fn show_window(
    env: &mut Env<'_>,
    hwnd: u32,
    cmd: u32,
    then: impl FnOnce(&mut Env<'_>, bool) -> Outcome + 'static,
) -> Outcome {
    let u = &mut env.ext.get_mut::<crate::Win32>().user;
    let (sw, sh) = u.screen;
    let Some(w) = u.windows.get_mut(&mut env.sys.objects, &hwnd) else {
        return then(env, false);
    };
    let was = w.visible();
    let top = w.parent == 0;
    let mut msgs: Vec<[u32; 4]> = Vec::new();
    if cmd == SW_HIDE {
        if !was {
            return then(env, false);
        }
        w.style &= !WS_VISIBLE;
        msgs.push([hwnd, wm::SHOWWINDOW, 0, 0]);
    } else {
        if !was {
            w.style |= WS_VISIBLE;
            w.needs_paint = true;
            msgs.push([hwnd, wm::SHOWWINDOW, 1, 0]);
        }
        match cmd {
            SW_MAXIMIZE if w.style & WS_MAXIMIZE == 0 => {
                w.style = (w.style | WS_MAXIMIZE) & !WS_MINIMIZE;
                w.rect = Rect::new(0, 0, sw, sh - 40);
                let (cw, ch) = w.client_size();
                msgs.push([hwnd, wm::SIZE, 2, make_long(cw, ch)]); // SIZE_MAXIMIZED
            }
            SW_MINIMIZE | SW_SHOWMINIMIZED | SW_SHOWMINNOACTIVE => {
                w.style = (w.style | WS_MINIMIZE) & !WS_MAXIMIZE;
                msgs.push([hwnd, wm::SIZE, 1, 0]); // SIZE_MINIMIZED
            }
            SW_RESTORE | SW_SHOWNORMAL if w.style & (WS_MINIMIZE | WS_MAXIMIZE) != 0 => {
                w.style &= !(WS_MINIMIZE | WS_MAXIMIZE);
                let (cw, ch) = w.client_size();
                msgs.push([hwnd, wm::SIZE, 0, make_long(cw, ch)]);
            }
            SW_SHOWNOACTIVATE if w.style & WS_MINIMIZE != 0 => {
                // Unlike SW_SHOWNA, native SHOWNOACTIVATE restores a minimized
                // window. Keep the existing no-activation path below.
                w.style &= !WS_MINIMIZE;
                let (cw, ch) = w.client_size();
                msgs.push([hwnd, wm::SIZE, 0, make_long(cw, ch)]);
            }
            _ => {}
        }
        if !w.initial_geometry_reported {
            w.initial_geometry_reported = true;
            let (cw, ch) = w.client_size();
            let (l, t, _, _) = nc_insets(w.style, w.ex_style);
            if !msgs.iter().any(|message| message[1] == wm::SIZE) {
                msgs.push([hwnd, wm::SIZE, 0, make_long(cw, ch)]);
            }
            msgs.push([
                hwnd,
                wm::MOVE,
                0,
                make_long(w.rect.left + l, w.rect.top + t),
            ]);
        }
        let activate = top
            && !matches!(
                cmd,
                SW_SHOWNOACTIVATE | SW_SHOWNA | SW_SHOWMINNOACTIVE | SW_MINIMIZE
            );
        if activate {
            return send_all(env, msgs.into(), move |env| {
                activate_window(env, hwnd, move |env| then(env, was))
            });
        }
    }
    send_all(env, msgs.into(), move |env| then(env, was))
}

/// Make `hwnd` the active window of its thread (WM_ACTIVATE, focus).
pub fn activate_window(
    env: &mut Env<'_>,
    hwnd: u32,
    then: impl FnOnce(&mut Env<'_>) -> Outcome + 'static,
) -> Outcome {
    let u = &mut env.ext.get_mut::<crate::Win32>().user;
    let Some(thread) = u.windows.get(&env.sys.objects, &hwnd).map(|w| w.thread) else {
        return then(env);
    };
    u.top.retain(|&t| t != hwnd);
    u.top.insert(0, hwnd);
    let q = u.queue(thread);
    let old = q.active;
    if old == hwnd {
        return then(env);
    }
    q.active = hwnd;
    let mut msgs = Vec::new();
    if old != 0 {
        msgs.push([old, wm::ACTIVATE, 0, hwnd]);
    } else {
        msgs.push([hwnd, wm::ACTIVATEAPP, 1, 0]);
    }
    msgs.push([hwnd, wm::ACTIVATE, 1, old]); // WA_ACTIVE
    send_all(env, msgs.into(), then)
}

/// SetFocus: WM_KILLFOCUS to the old focus, WM_SETFOCUS to the new. `then` gets the old one.
pub fn set_focus(
    env: &mut Env<'_>,
    hwnd: u32,
    then: impl FnOnce(&mut Env<'_>, u32) -> Outcome + 'static,
) -> Outcome {
    let ctx = env.context;
    let u = &mut env.ext.get_mut::<crate::Win32>().user;
    if hwnd != 0 && !u.windows.contains_key(&env.sys.objects, &hwnd) {
        return then(env, 0);
    }
    let q = u.queue(ctx);
    let old = q.focus;
    if old == hwnd {
        return then(env, old);
    }
    q.focus = hwnd;
    let mut msgs = Vec::new();
    if old != 0 {
        msgs.push([old, wm::KILLFOCUS, hwnd, 0]);
    }
    if hwnd != 0 {
        msgs.push([hwnd, wm::SETFOCUS, old, 0]);
    }
    send_all(env, msgs.into(), move |env| then(env, old))
}

pub const SWP_NOSIZE: u32 = 0x1;
pub const SWP_NOMOVE: u32 = 0x2;
pub const SWP_NOZORDER: u32 = 0x4;
pub const SWP_NOACTIVATE: u32 = 0x10;
pub const SWP_FRAMECHANGED: u32 = 0x20;
pub const SWP_SHOWWINDOW: u32 = 0x40;
pub const SWP_HIDEWINDOW: u32 = 0x80;
const SWP_NOCLIENTSIZE: u32 = 0x800;
const SWP_NOCLIENTMOVE: u32 = 0x1000;

/// SetWindowPos: WM_WINDOWPOSCHANGING, apply, WM_WINDOWPOSCHANGED (whose default handling
/// sends WM_MOVE/WM_SIZE), plus the show/hide.
pub fn set_window_pos(
    env: &mut Env<'_>,
    hwnd: u32,
    after: u32,
    rect: Rect,
    mut flags: u32,
    then: impl FnOnce(&mut Env<'_>, bool) -> Outcome + 'static,
) -> Outcome {
    let u = &mut env.ext.get_mut::<crate::Win32>().user;
    let Some(w) = u.windows.get(&env.sys.objects, &hwnd) else {
        return then(env, false);
    };
    let old = w.rect;
    let (x, y) = if flags & SWP_NOMOVE != 0 {
        (old.left, old.top)
    } else {
        (rect.left, rect.top)
    };
    let (cx, cy) = if flags & SWP_NOSIZE != 0 {
        (old.width(), old.height())
    } else {
        (rect.width(), rect.height())
    };
    if (x, y) == (old.left, old.top) && flags & SWP_FRAMECHANGED == 0 {
        flags |= SWP_NOMOVE | SWP_NOCLIENTMOVE;
    }
    if (cx, cy) == (old.width(), old.height()) && flags & SWP_FRAMECHANGED == 0 {
        flags |= SWP_NOSIZE | SWP_NOCLIENTSIZE;
    }
    // WINDOWPOS: hwnd, hwndInsertAfter, x, y, cx, cy, flags.
    let wp = temp_alloc(env, 28);
    for (i, v) in [hwnd, after, x as u32, y as u32, cx as u32, cy as u32, flags]
        .into_iter()
        .enumerate()
    {
        let _ = env.cpu.write_u32(wp + 4 * i as u32, v);
    }
    send(env, hwnd, wm::WINDOWPOSCHANGING, 0, wp, move |env, _| {
        // The window procedure may have adjusted the position.
        let r = |i: u32| env.cpu.read_u32(wp + 4 * i).unwrap_or(0) as i32;
        let (x, y, cx, cy) = (r(2), r(3), r(4), r(5));
        let flags = r(6) as u32;
        let u = &mut env.ext.get_mut::<crate::Win32>().user;
        if let Some(w) = u.windows.get_mut(&mut env.sys.objects, &hwnd) {
            w.rect = Rect::new(x, y, cx, cy);
            if flags & SWP_NOSIZE == 0 {
                w.needs_paint = true;
            }
        }
        if flags & SWP_NOZORDER == 0
            && u.windows
                .get(&env.sys.objects, &hwnd)
                .is_some_and(|w| w.parent == 0)
        {
            u.top.retain(|&t| t != hwnd);
            // HWND_TOP 0, HWND_BOTTOM 1, HWND_TOPMOST -1, HWND_NOTOPMOST -2, else after that window.
            let at = match after {
                0 | 0xffff_ffff | 0xffff_fffe => 0,
                1 => u.top.len(),
                a => u.top.iter().position(|&t| t == a).map_or(0, |i| i + 1),
            };
            u.top.insert(at, hwnd);
        }
        send(env, hwnd, wm::WINDOWPOSCHANGED, 0, wp, move |env, _| {
            temp_free(env, wp);
            let show = if flags & SWP_SHOWWINDOW != 0 {
                Some(if flags & SWP_NOACTIVATE != 0 {
                    SW_SHOWNA
                } else {
                    SW_SHOW
                })
            } else if flags & SWP_HIDEWINDOW != 0 {
                Some(SW_HIDE)
            } else {
                None
            };
            match show {
                Some(cmd) => show_window(env, hwnd, cmd, move |env, _| then(env, true)),
                None => then(env, true),
            }
        })
    })
}

/// Default handling of WM_WINDOWPOSCHANGED: WM_MOVE and WM_SIZE.
pub fn window_pos_changed_msgs(
    u: &User,
    objects: &Objects,
    hwnd: u32,
    flags: u32,
) -> Vec<[u32; 4]> {
    let Some(w) = u.windows.get(objects, &hwnd) else {
        return Vec::new();
    };
    let mut m = Vec::new();
    if flags & SWP_NOCLIENTMOVE == 0 {
        let (l, t, _, _) = nc_insets(w.style, w.ex_style);
        m.push([
            hwnd,
            wm::MOVE,
            0,
            make_long(w.rect.left + l, w.rect.top + t),
        ]);
    }
    if flags & SWP_NOCLIENTSIZE == 0 {
        let (cw, ch) = w.client_size();
        let kind = if w.style & WS_MAXIMIZE != 0 { 2 } else { 0 };
        m.push([hwnd, wm::SIZE, kind, make_long(cw, ch)]);
    }
    m
}

fn set_window_pos_api(env: &mut Env<'_>) -> Outcome {
    let (hwnd, after, x, y, cx, cy, flags) = (
        env.arg(0),
        env.arg(1),
        env.arg(2) as i32,
        env.arg(3) as i32,
        env.arg(4) as i32,
        env.arg(5) as i32,
        env.arg(6),
    );
    set_window_pos(env, hwnd, after, Rect::new(x, y, cx, cy), flags, |_, ok| {
        Outcome::Return(u32::from(ok))
    })
}

fn move_window(env: &mut Env<'_>) -> Outcome {
    let (hwnd, x, y, w, h) = (
        env.arg(0),
        env.arg(1) as i32,
        env.arg(2) as i32,
        env.arg(3) as i32,
        env.arg(4) as i32,
    );
    set_window_pos(
        env,
        hwnd,
        0,
        Rect::new(x, y, w, h),
        SWP_NOZORDER | SWP_NOACTIVATE,
        |_, ok| Outcome::Return(u32::from(ok)),
    )
}

fn update_window(env: &mut Env<'_>) -> Outcome {
    let hwnd = env.arg(0);
    let paint = user(env)
        .windows
        .get(&env.sys.objects, &hwnd)
        .is_some_and(|w| w.needs_paint && w.visible());
    if !paint {
        return Outcome::Return(u32::from(
            user(env).windows.contains_key(&env.sys.objects, &hwnd),
        ));
    }
    send(env, hwnd, wm::PAINT, 0, 0, |_, _| Outcome::Return(1))
}

fn lock_window_update(env: &mut Env<'_>) -> Outcome {
    let hwnd = env.arg(0);
    if hwnd == 0 {
        user_mut(env).locked_window = None;
        return Outcome::Return(1);
    }
    if !user(env).windows.contains_key(&env.sys.objects, &hwnd) {
        return Outcome::Return(0);
    }
    let u = user_mut(env);
    match u.locked_window {
        Some(existing) if existing != hwnd => Outcome::Return(0),
        _ => {
            u.locked_window = Some(hwnd);
            Outcome::Return(1)
        }
    }
}

fn set_focus_api(env: &mut Env<'_>) -> Outcome {
    let hwnd = env.arg(0);
    set_focus(env, hwnd, |_, old| Outcome::Return(old))
}

fn set_active_window(env: &mut Env<'_>) -> Outcome {
    let hwnd = env.arg(0);
    let ctx = env.context;
    let old = user_mut(env).queue(ctx).active;
    activate_window(env, hwnd, move |_| Outcome::Return(old))
}

fn enable_window(env: &mut Env<'_>) -> Outcome {
    let (hwnd, enable) = (env.arg(0), env.arg(1) != 0);
    let Some(w) = env
        .ext
        .get::<crate::Win32>()
        .user
        .windows
        .get_mut(&mut env.sys.objects, &hwnd)
    else {
        return Outcome::Return(0);
    };
    let was_disabled = !w.enabled();
    if was_disabled == !enable {
        return Outcome::Return(u32::from(was_disabled));
    }
    if enable {
        w.style &= !WS_DISABLED;
    } else {
        w.style |= WS_DISABLED;
    }
    send(env, hwnd, wm::ENABLE, u32::from(enable), 0, move |_, _| {
        Outcome::Return(u32::from(was_disabled))
    })
}

// ---- queries ----

fn get_window_rect(env: &mut Env<'_>) -> Outcome {
    let (hwnd, out) = (env.arg(0), env.arg(1));
    let Some(r) = user(env).screen_rect(&env.sys.objects, hwnd) else {
        return Outcome::Return(0);
    };
    Outcome::Return(u32::from(r.write(env.cpu, out)))
}

fn get_client_rect(env: &mut Env<'_>) -> Outcome {
    let (hwnd, out) = (env.arg(0), env.arg(1));
    let Some(w) = user(env).windows.get(&env.sys.objects, &hwnd) else {
        return Outcome::Return(0);
    };
    let (cw, ch) = w.client_size();
    Outcome::Return(u32::from(Rect::new(0, 0, cw, ch).write(env.cpu, out)))
}

fn client_to_screen(env: &mut Env<'_>) -> Outcome {
    map_point(env, 1)
}

fn screen_to_client(env: &mut Env<'_>) -> Outcome {
    map_point(env, -1)
}

fn map_point(env: &mut Env<'_>, sign: i32) -> Outcome {
    let (hwnd, p) = (env.arg(0), env.arg(1));
    if !user(env).windows.contains_key(&env.sys.objects, &hwnd) {
        return Outcome::Return(0);
    }
    let (ox, oy) = user(env).client_origin(&env.sys.objects, hwnd);
    let x = env.cpu.read_u32(p).unwrap_or(0) as i32 + sign * ox;
    let y = env.cpu.read_u32(p + 4).unwrap_or(0) as i32 + sign * oy;
    let _ = env.cpu.write_u32(p, x as u32);
    let _ = env.cpu.write_u32(p + 4, y as u32);
    Outcome::Return(1)
}

fn adjust_window_rect(env: &mut Env<'_>) -> Outcome {
    let (p, style) = (env.arg(0), env.arg(1));
    adjust(env, p, style, 0)
}

fn adjust_window_rect_ex(env: &mut Env<'_>) -> Outcome {
    let (p, style, ex) = (env.arg(0), env.arg(1), env.arg(3));
    adjust(env, p, style, ex)
}

fn adjust(env: &mut Env<'_>, p: u32, style: u32, ex: u32) -> Outcome {
    let Some(r) = Rect::read(env.cpu, p) else {
        return Outcome::Return(0);
    };
    let (l, t, rr, b) = nc_insets(style, ex);
    let r = Rect {
        left: r.left - l,
        top: r.top - t,
        right: r.right + rr,
        bottom: r.bottom + b,
    };
    Outcome::Return(u32::from(r.write(env.cpu, p)))
}

pub const GWL_WNDPROC: i32 = -4;
pub const GWL_HINSTANCE: i32 = -6;
pub const GWL_HWNDPARENT: i32 = -8;
pub const GWL_ID: i32 = -12;
pub const GWL_STYLE: i32 = -16;
pub const GWL_EXSTYLE: i32 = -20;
pub const GWL_USERDATA: i32 = -21;

fn get_window_long_a(env: &mut Env<'_>) -> Outcome {
    let (hwnd, idx) = (env.arg(0), env.arg(1) as i32);
    let Some(w) = user(env).windows.get(&env.sys.objects, &hwnd) else {
        set_last_error(env, 1400);
        return Outcome::Return(0);
    };
    let v = match idx {
        GWL_WNDPROC => Some(w.proc),
        GWL_HINSTANCE => Some(w.instance),
        GWL_HWNDPARENT => Some(if w.parent != 0 { w.parent } else { w.owner }),
        GWL_ID => Some(w.id),
        GWL_STYLE => Some(w.style),
        GWL_EXSTYLE => Some(w.ex_style),
        GWL_USERDATA => Some(w.user_data),
        i if i >= 0 => w.extra_u32(i as u32),
        _ => None,
    };
    match v {
        Some(v) => Outcome::Return(v),
        None => {
            set_last_error(env, 1413); // ERROR_INVALID_INDEX
            Outcome::Return(0)
        }
    }
}

fn set_window_long_a(env: &mut Env<'_>) -> Outcome {
    let (hwnd, idx, v) = (env.arg(0), env.arg(1) as i32, env.arg(2));
    let Some(w) = env
        .ext
        .get::<crate::Win32>()
        .user
        .windows
        .get_mut(&mut env.sys.objects, &hwnd)
    else {
        set_last_error(env, 1400);
        return Outcome::Return(0);
    };
    let old = match idx {
        GWL_WNDPROC => Some(std::mem::replace(&mut w.proc, v)),
        GWL_HINSTANCE => Some(std::mem::replace(&mut w.instance, v)),
        GWL_ID => Some(std::mem::replace(&mut w.id, v)),
        GWL_STYLE => Some(std::mem::replace(&mut w.style, v)),
        GWL_EXSTYLE => Some(std::mem::replace(&mut w.ex_style, v)),
        GWL_USERDATA => Some(std::mem::replace(&mut w.user_data, v)),
        GWL_HWNDPARENT => Some(std::mem::replace(&mut w.owner, v)),
        i if i >= 0 => w.set_extra_u32(i as u32, v),
        _ => None,
    };
    match old {
        Some(o) => {
            set_last_error(env, err::SUCCESS);
            Outcome::Return(o)
        }
        None => {
            set_last_error(env, 1413);
            Outcome::Return(0)
        }
    }
}

fn get_class_long_a(env: &mut Env<'_>) -> Outcome {
    let (hwnd, idx) = (env.arg(0), env.arg(1) as i32);
    let u = user(env);
    let Some(w) = u.windows.get(&env.sys.objects, &hwnd) else {
        return Outcome::Return(0);
    };
    let c = &u.classes[w.class];
    Outcome::Return(match idx {
        -24 => c.proc,
        -26 => c.style,
        -14 => c.icon,
        -34 => c.icon,
        -12 => c.cursor,
        -10 => c.background,
        -18 => c.wnd_extra,
        -20 => c.cls_extra,
        -16 => c.instance,
        -8 => c.menu_name,
        -32 => u32::from(c.atom),
        _ => 0,
    })
}

fn set_class_long_a(env: &mut Env<'_>) -> Outcome {
    let (hwnd, idx, v) = (env.arg(0), env.arg(1) as i32, env.arg(2));
    let u = &mut env.ext.get_mut::<crate::Win32>().user;
    let Some(ci) = u.windows.get(&env.sys.objects, &hwnd).map(|w| w.class) else {
        return Outcome::Return(0);
    };
    let c = &mut u.classes[ci];
    let slot = match idx {
        -24 => &mut c.proc,
        -26 => &mut c.style,
        -14 | -34 => &mut c.icon,
        -12 => &mut c.cursor,
        -10 => &mut c.background,
        _ => return Outcome::Return(0),
    };
    Outcome::Return(std::mem::replace(slot, v))
}

fn get_parent(env: &mut Env<'_>) -> Outcome {
    let hwnd = env.arg(0);
    let Some(w) = user(env).windows.get(&env.sys.objects, &hwnd) else {
        return Outcome::Return(0);
    };
    Outcome::Return(if w.style & WS_CHILD != 0 {
        w.parent
    } else {
        w.owner
    })
}

fn is_child(env: &mut Env<'_>) -> Outcome {
    let (parent, mut h) = (env.arg(0), env.arg(1));
    let u = user(env);
    while let Some(w) = u.windows.get(&env.sys.objects, &h) {
        if w.parent == 0 {
            break;
        }
        if w.parent == parent {
            return Outcome::Return(1);
        }
        h = w.parent;
    }
    Outcome::Return(0)
}

fn get_window(env: &mut Env<'_>) -> Outcome {
    let (hwnd, cmd) = (env.arg(0), env.arg(1));
    let u = user(env);
    let Some(w) = u.windows.get(&env.sys.objects, &hwnd) else {
        return Outcome::Return(0);
    };
    let siblings = if w.parent == 0 {
        &u.top
    } else {
        &u.windows.get(&env.sys.objects, &w.parent).unwrap().children
    };
    let pos = siblings.iter().position(|&s| s == hwnd);
    Outcome::Return(match cmd {
        0 => siblings.first().copied().unwrap_or(0), // GW_HWNDFIRST
        1 => siblings.last().copied().unwrap_or(0),  // GW_HWNDLAST
        2 => pos.and_then(|i| siblings.get(i + 1)).copied().unwrap_or(0), // GW_HWNDNEXT
        3 => pos
            .and_then(|i| i.checked_sub(1))
            .map_or(0, |i| siblings[i]), // GW_HWNDPREV
        4 => w.owner,                                // GW_OWNER
        5 => w.children.first().copied().unwrap_or(0), // GW_CHILD
        _ => 0,
    })
}

fn is_window(env: &mut Env<'_>) -> Outcome {
    let hwnd = env.arg(0);
    Outcome::Return(u32::from(
        user(env)
            .windows
            .get(&env.sys.objects, &hwnd)
            .is_some_and(|w| !w.destroying),
    ))
}

fn is_window_visible(env: &mut Env<'_>) -> Outcome {
    let mut hwnd = env.arg(0);
    let u = user(env);
    // Visible only if all ancestors are.
    while let Some(w) = u.windows.get(&env.sys.objects, &hwnd) {
        if !w.visible() {
            return Outcome::Return(0);
        }
        if w.parent == 0 {
            return Outcome::Return(1);
        }
        hwnd = w.parent;
    }
    Outcome::Return(0)
}

fn is_window_enabled(env: &mut Env<'_>) -> Outcome {
    let hwnd = env.arg(0);
    Outcome::Return(u32::from(
        user(env)
            .windows
            .get(&env.sys.objects, &hwnd)
            .is_some_and(Window::enabled),
    ))
}

fn is_iconic(env: &mut Env<'_>) -> Outcome {
    let hwnd = env.arg(0);
    Outcome::Return(u32::from(
        user(env)
            .windows
            .get(&env.sys.objects, &hwnd)
            .is_some_and(|w| w.style & WS_MINIMIZE != 0),
    ))
}

fn is_zoomed(env: &mut Env<'_>) -> Outcome {
    let hwnd = env.arg(0);
    Outcome::Return(u32::from(
        user(env)
            .windows
            .get(&env.sys.objects, &hwnd)
            .is_some_and(|w| w.style & WS_MAXIMIZE != 0),
    ))
}

fn get_focus(env: &mut Env<'_>) -> Outcome {
    let ctx = env.context;
    Outcome::Return(user_mut(env).queue(ctx).focus)
}

fn get_active_window(env: &mut Env<'_>) -> Outcome {
    let ctx = env.context;
    Outcome::Return(user_mut(env).queue(ctx).active)
}

fn get_foreground_window(env: &mut Env<'_>) -> Outcome {
    let u = user(env);
    Outcome::Return(
        u.top
            .iter()
            .copied()
            .find(|t| {
                u.windows
                    .get(&env.sys.objects, t)
                    .is_some_and(Window::visible)
            })
            .unwrap_or(0),
    )
}

fn set_foreground_window(env: &mut Env<'_>) -> Outcome {
    let hwnd = env.arg(0);
    if !user(env).windows.contains_key(&env.sys.objects, &hwnd) {
        return Outcome::Return(0);
    }
    activate_window(env, hwnd, |_| Outcome::Return(1))
}

fn get_desktop_window(env: &mut Env<'_>) -> Outcome {
    Outcome::Return(user(env).desktop)
}

fn get_window_thread_process_id(env: &mut Env<'_>) -> Outcome {
    let (hwnd, pid_out) = (env.arg(0), env.arg(1));
    let Some(ctx) = user(env)
        .windows
        .get(&env.sys.objects, &hwnd)
        .map(|w| w.thread)
    else {
        return Outcome::Return(0);
    };
    let pid = env.sys.pid;
    if pid_out != 0 {
        let _ = env.cpu.write_u32(pid_out, pid);
    }
    Outcome::Return(env.sys.context(ctx).tid)
}

fn find_window_a(env: &mut Env<'_>) -> Outcome {
    let (class, title) = (env.arg(0), env.arg(1));
    let class = (class != 0).then(|| ClassKey::read(env, class));
    let title = (title != 0).then(|| env.ext.get::<Win32>().codec.decode(&cstr(env.cpu, title)));
    let u = user(env);
    let found = u.top.iter().copied().find(|t| {
        let w = u.windows.get(&env.sys.objects, t).unwrap();
        let c = &u.classes[w.class];
        let class_ok = match &class {
            None => true,
            Some(ClassKey::Atom(a)) => c.atom == *a,
            Some(ClassKey::Name(n)) => c.name.eq_ignore_ascii_case(n),
        };
        class_ok && title.as_ref().is_none_or(|t| *t == w.text)
    });
    Outcome::Return(found.unwrap_or(0))
}

// ---- text ----

fn set_window_text_a(env: &mut Env<'_>) -> Outcome {
    let (hwnd, text) = (env.arg(0), env.arg(1));
    send(env, hwnd, wm::SETTEXT, 0, text, |_, r| Outcome::Return(r))
}

fn get_window_text_a(env: &mut Env<'_>) -> Outcome {
    let (hwnd, buf, cap) = (env.arg(0), env.arg(1), env.arg(2));
    if cap == 0 {
        return Outcome::Return(0);
    }
    send(env, hwnd, wm::GETTEXT, cap, buf, |_, r| Outcome::Return(r))
}

fn get_window_text_length_a(env: &mut Env<'_>) -> Outcome {
    let hwnd = env.arg(0);
    send(env, hwnd, wm::GETTEXTLENGTH, 0, 0, |_, r| {
        Outcome::Return(r)
    })
}

// ---- capture ----

fn set_capture(env: &mut Env<'_>) -> Outcome {
    let hwnd = env.arg(0);
    let ctx = env.context;
    Outcome::Return(std::mem::replace(
        &mut user_mut(env).queue(ctx).capture,
        hwnd,
    ))
}

fn release_capture(env: &mut Env<'_>) -> Outcome {
    let ctx = env.context;
    user_mut(env).queue(ctx).capture = 0;
    Outcome::Return(1)
}

fn get_capture(env: &mut Env<'_>) -> Outcome {
    let ctx = env.context;
    Outcome::Return(user_mut(env).queue(ctx).capture)
}

pub fn register(reg: &mut Registry) {
    let s = |reg: &mut Registry, name: &str, argc: u8, h| stdcall(reg, USER32, name, argc, h);
    s(reg, "RegisterClassA", 1, register_class_a);
    s(reg, "RegisterClassExA", 1, register_class_ex_a);
    s(reg, "UnregisterClassA", 2, unregister_class_a);
    s(reg, "GetClassInfoA", 3, get_class_info_a);
    s(reg, "GetClassNameA", 3, get_class_name_a);
    s(reg, "CreateWindowExA", 12, create_window_ex_a);
    s(reg, "RegisterClassW", 1, |e| register_class_w(e, false));
    s(reg, "RegisterClassExW", 1, |e| register_class_w(e, true));
    s(reg, "DestroyWindow", 1, destroy_window_api);
    s(reg, "ShowWindow", 2, show_window_api);
    s(reg, "UpdateWindow", 1, update_window);
    s(reg, "LockWindowUpdate", 1, lock_window_update);
    s(reg, "SetWindowPos", 7, set_window_pos_api);
    s(reg, "MoveWindow", 6, move_window);
    s(reg, "SetFocus", 1, set_focus_api);
    s(reg, "GetFocus", 0, get_focus);
    s(reg, "SetActiveWindow", 1, set_active_window);
    s(reg, "GetActiveWindow", 0, get_active_window);
    s(reg, "GetForegroundWindow", 0, get_foreground_window);
    s(reg, "SetForegroundWindow", 1, set_foreground_window);
    s(reg, "EnableWindow", 2, enable_window);
    s(reg, "GetWindowRect", 2, get_window_rect);
    s(reg, "GetClientRect", 2, get_client_rect);
    s(reg, "ClientToScreen", 2, client_to_screen);
    s(reg, "ScreenToClient", 2, screen_to_client);
    s(reg, "AdjustWindowRect", 3, adjust_window_rect);
    s(reg, "AdjustWindowRectEx", 4, adjust_window_rect_ex);
    s(reg, "GetWindowLongA", 2, get_window_long_a);
    s(reg, "SetWindowLongA", 3, set_window_long_a);
    s(reg, "GetClassLongA", 2, get_class_long_a);
    s(reg, "SetClassLongA", 3, set_class_long_a);
    s(reg, "GetParent", 1, get_parent);
    s(reg, "IsChild", 2, is_child);
    s(reg, "GetWindow", 2, get_window);
    s(reg, "IsWindow", 1, is_window);
    s(reg, "IsWindowVisible", 1, is_window_visible);
    s(reg, "IsWindowEnabled", 1, is_window_enabled);
    s(reg, "IsIconic", 1, is_iconic);
    s(reg, "IsZoomed", 1, is_zoomed);
    s(reg, "GetDesktopWindow", 0, get_desktop_window);
    s(
        reg,
        "GetWindowThreadProcessId",
        2,
        get_window_thread_process_id,
    );
    s(reg, "FindWindowA", 2, find_window_a);
    s(reg, "SetWindowTextA", 2, set_window_text_a);
    s(reg, "GetWindowTextA", 3, get_window_text_a);
    s(reg, "GetWindowTextLengthA", 1, get_window_text_length_a);
    s(reg, "SetCapture", 1, set_capture);
    s(reg, "ReleaseCapture", 0, release_capture);
    s(reg, "GetCapture", 0, get_capture);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insets_match_system_metrics() {
        // Overlapped window: sizing frame + caption.
        assert_eq!(nc_insets(0x00cf_0000, 0), (8, 31, 8, 8));
        // Dialog frame + caption (WS_POPUP | WS_CAPTION | WS_SYSMENU).
        assert_eq!(nc_insets(0x80c8_0000, 0), (3, 26, 3, 3));
        // Bare child.
        assert_eq!(nc_insets(WS_CHILD | WS_VISIBLE, 0), (0, 0, 0, 0));
        assert_eq!(nc_insets(WS_CHILD, WS_EX_CLIENTEDGE), (2, 2, 2, 2));
    }

    #[test]
    fn class_lookup_prefers_local() {
        let mut objects = Objects::default();
        let mut u = User::new(&mut objects, (1920, 1080), None).unwrap();
        u.add_system_classes(SysProcs {
            def_window: 1,
            def_dlg: 2,
            button: 3,
            static_: 4,
            edit: 5,
            ..SysProcs::default()
        });
        let local = u.add_class_raw("BUTTON", 0, 9, 0, 0x40_0000);
        assert_eq!(
            u.find_class(&ClassKey::Name("button".into()), 0x40_0000),
            Some(local)
        );
        let sys = u.find_class(&ClassKey::Name("Button".into()), 0).unwrap();
        assert_eq!(u.classes[sys].proc, 3);
    }

    #[test]
    fn lock_window_update_lifecycle() {
        let mut objects = Objects::default();
        let mut u = User::new(&mut objects, (1920, 1080), None).unwrap();
        let cls = u.add_class_raw("TEST", 0, 0, 0, 0);
        let h1 = u.alloc_hwnd().unwrap();
        u.windows.insert(
            &mut objects,
            h1,
            Window::new(h1, cls, 0, WS_VISIBLE, 0, 0, Rect::new(0, 0, 100, 100), 0),
        );
        let h2 = u.alloc_hwnd().unwrap();
        u.windows.insert(
            &mut objects,
            h2,
            Window::new(h2, cls, 0, WS_VISIBLE, 0, 0, Rect::new(0, 0, 100, 100), 0),
        );

        assert_eq!(u.locked_window, None);
        u.locked_window = Some(h1);
        assert_eq!(u.locked_window, Some(h1));

        // When locked window is removed, locked_window is reset to None
        u.remove_window(h1, &mut objects);
        assert_eq!(u.locked_window, None);
    }
}
