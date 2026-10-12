//! The window model: classes, windows, geometry, and the window-management API.
//!
//! Windows live in the common object table. A window procedure is a guest address, which may
//! be a host thunk (built-in classes, DefWindowProc, PAL's HLE windows), so messages are
//! delivered the same way to both: an [`Outcome::Call`] on the current context (see
//! [`super::message::send`]). Coordinates follow Win32: a top-level window's rect is in
//! screen coordinates, a child's in its parent's client coordinates.

use std::collections::{BTreeMap, HashMap, VecDeque};

use adv_abi::{AnsiStr, GuestPtr, Handle as AbiHandle, Registry, WideStr, win32};
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
    pub small_icon: u32,
    pub cursor: u32,
    pub background: u32,
    pub menu_name: u32,
    /// Registration-time menu resource text, independent of the caller's buffer.
    pub menu_text: Option<String>,
    /// USER-owned A/W query strings, retained until this class is unregistered.
    pub menu_query_strings: [u32; 2],
    /// Built-in classes support both encodings; guest classes use their registered ABI.
    pub unicode: Option<bool>,
}

/// Per-window state of built-in controls and dialogs.
#[derive(Default)]
pub struct ControlState {
    pub(crate) properties: Vec<(u16, u32, bool)>,
    pub(crate) update: super::update::UpdateRegion,
    pub(crate) scrolls: Option<[super::scroll::ScrollState; 2]>,
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
    pub(crate) tab: Option<super::tab::TabState>,
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
    /// Creation-time class DC policy; later class mutations affect new windows.
    pub(crate) dc_style: u32,
    pub proc: u32,
    /// Encoding of string messages and CREATESTRUCT pointers seen by the procedure.
    pub unicode: bool,
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
    /// Restore geometry and show history belong to this same USER object.
    pub placement: super::placement::PlacementState,
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
    /// ShowOwnedPopups(FALSE) hid this popup; ShowOwnedPopups(TRUE) brings it back.
    pub hidden_popup: bool,
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
    pub(crate) fn invalidate_client(&mut self, erase: bool) {
        super::update::invalidate_client(self, erase);
    }
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
    /// Next generated thread timer ID; zero until the first one.
    pub next_timer_id: u32,
    pub focus: u32,
    pub active: u32,
    pub capture: u32,
    pub last_time: u32,
    pub last_pos: (i32, i32),
    /// Application value, replaced on retrieval by the untagged message's zero.
    pub extra_info: i32,
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
    pub tab: u32,
    pub rich_edit: u32,
}

pub struct User {
    pub classes: Vec<Class>,
    /// Process-lifetime encoding evidence for raw procedures returned by Get/Set.
    /// It survives window destruction and does not pin guest code mappings;
    /// address reuse can retain old evidence, so an opposite ABI is rejected.
    /// Bits 1/2 mean A/W; an address legitimately used by both stays ambiguous.
    saved_procedure_encodings: BTreeMap<u32, u8>,
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
    pub(super) double_click: super::input::double_click::DoubleClick,
    pub icon_title_font: super::system_parameters::IconFont,
    pub clipboard: adv_os::clipboard::Clipboard,
    pub cursor_pos: (i32, i32),
    /// Shared guest desktop constraint; None follows the current display bounds.
    pub(super) cursor_clip: Option<Rect>,
    pub cursor_count: i32,
    pub cursor_handle: u32,
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
            saved_procedure_encodings: BTreeMap::new(),
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
            double_click: super::input::double_click::DoubleClick::default(),
            icon_title_font: super::system_parameters::IconFont::default(),
            clipboard: adv_os::clipboard::Clipboard::default(),
            cursor_pos: (screen.0 / 2, screen.1 / 2),
            cursor_clip: None,
            cursor_count: 0,
            cursor_handle: 0,
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

    /// Whether `window` is owed a WM_PAINT. The system paints the desktop; its
    /// thread field names no guest thread, so invalidating every window (as
    /// InvalidateRect(NULL) does) must not queue it to context 0.
    pub fn paints(&self, window: &Window) -> bool {
        window.needs_paint && window.visible() && !window.destroying && window.hwnd != self.desktop
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
            small_icon: 0,
            cursor: 0,
            background: 0,
            menu_name: 0,
            menu_text: None,
            menu_query_strings: [0; 2],
            unicode: None,
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
        c.unicode = Some(false);
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
        self.add_class_raw("SysTabControl32", 0x400b, procs.tab, 4, 0);
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
        self.double_click.forget_window(hwnd);
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
    /// Construct metadata with the default DC policy, before USER notifications.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
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
            dc_style: 0,
            proc,
            unicode: false,
            style,
            ex_style,
            parent,
            owner: 0,
            id: 0,
            instance: 0,
            rect,
            placement: super::placement::PlacementState::new(rect),
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
            hidden_popup: false,
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
    let small_icon = if extended {
        env.cpu.read_u32(pointer + 44).unwrap_or(0)
    } else {
        0
    };
    register_class(env, &fields, small_icon, true)
}

fn register_class(env: &mut Env<'_>, f: &[u32], small_icon: u32, wide: bool) -> Outcome {
    let menu_text = if f[8] >= 0x1_0000 {
        let Some(text) = super::message::read_text(env, f[8], wide) else {
            set_last_error(env, 998);
            return Outcome::Return(0);
        };
        Some(text)
    } else {
        None
    };
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
    c.small_icon = small_icon;
    c.menu_text = menu_text;
    c.unicode = Some(wide);
    let atom = c.atom;
    super::menu::class_menu(env, i, f[8], wide);
    log::debug!("RegisterClass({name}) proc {:#010x} extra {}", f[1], f[3]);
    Outcome::Return(u32::from(atom))
}

fn unregister_class_a(env: &mut Env<'_>) -> Outcome {
    unregister_class(env, false)
}

#[win32(user32, name = "UnregisterClassW", level = state)]
fn unregister_class_w(env: &mut Env<'_>, _name: WideStr, _instance: AbiHandle) -> Outcome {
    unregister_class(env, true)
}

fn unregister_class(env: &mut Env<'_>, wide: bool) -> Outcome {
    let (name, inst) = (env.arg(0), env.arg(1));
    let key = if wide && name >= 0x1_0000 {
        ClassKey::Name(String::from_utf16_lossy(&crate::util::wstr(env.cpu, name)))
    } else {
        ClassKey::read(env, name)
    };
    let u = &mut env.ext.get_mut::<crate::Win32>().user;
    let Some(i) = u.find_class(&key, inst) else {
        crate::util::set_last_error(env, 1411); // ERROR_CLASS_DOES_NOT_EXIST
        return Outcome::Return(0);
    };
    if u.classes[i].instance == 0 {
        crate::util::set_last_error(env, 1411);
        return Outcome::Return(0);
    }
    if u.windows.values(&env.sys.objects).any(|w| w.class == i) {
        crate::util::set_last_error(env, 1412); // ERROR_CLASS_HAS_WINDOWS
        return Outcome::Return(0);
    }
    let strings = std::mem::take(&mut u.classes[i].menu_query_strings);
    // Keep indices stable: blank the slot and retire its owned query strings.
    u.classes[i].name.clear();
    u.classes[i].menu_text = None;
    for pointer in strings {
        if pointer != 0 {
            temp_free(env, pointer);
        }
    }
    Outcome::Return(1)
}

fn get_class_info_a(env: &mut Env<'_>) -> Outcome {
    let (inst, name, out) = (env.arg(0), env.arg(1), env.arg(2));
    get_class_info(env, inst, name, out, false, false)
}

#[win32(user32, name = "GetClassInfoW", level = state)]
fn get_class_info_w(
    env: &mut Env<'_>,
    instance: AbiHandle,
    name: WideStr,
    out: GuestPtr<u8>,
) -> Outcome {
    get_class_info(env, instance.0, name.0.addr(), out.addr(), true, false)
}

#[win32(user32, name = "GetClassInfoExA", level = state)]
fn get_class_info_ex_a(
    env: &mut Env<'_>,
    instance: AbiHandle,
    name: AnsiStr,
    out: GuestPtr<u8>,
) -> Outcome {
    get_class_info(env, instance.0, name.0.addr(), out.addr(), false, true)
}

#[win32(user32, name = "GetClassInfoExW", level = state)]
fn get_class_info_ex_w(
    env: &mut Env<'_>,
    instance: AbiHandle,
    name: WideStr,
    out: GuestPtr<u8>,
) -> Outcome {
    get_class_info(env, instance.0, name.0.addr(), out.addr(), true, true)
}

fn get_class_info(
    env: &mut Env<'_>,
    inst: u32,
    name: u32,
    out: u32,
    wide: bool,
    extended: bool,
) -> Outcome {
    let api = match (wide, extended) {
        (false, false) => "user32.dll!GetClassInfoA",
        (true, false) => "user32.dll!GetClassInfoW",
        (false, true) => "user32.dll!GetClassInfoExA",
        (true, true) => "user32.dll!GetClassInfoExW",
    };
    if !wide && !matches!(env.ext.get::<Win32>().codec.acp, 932 | 936 | 1252 | 65001) {
        adv_abi::warn_unavailable(
            env,
            api,
            "class query ANSI code page has no supported encoder",
        );
        set_last_error(env, 50);
        return Outcome::Return(0);
    }
    let key = if name < 0x1_0000 {
        ClassKey::Atom(name as u16)
    } else {
        let Some(text) = super::message::read_text(env, name, wide) else {
            set_last_error(env, 998);
            return Outcome::Return(0);
        };
        ClassKey::Name(text)
    };
    let Some(i) = user(env).find_class(&key, inst) else {
        set_last_error(env, 1411); // ERROR_CLASS_DOES_NOT_EXIST
        return Outcome::Return(0);
    };
    let u = user(env);
    let c = &u.classes[i];
    if c.unicode
        .is_some_and(|registered_wide| registered_wide != wide)
        && !stock_procedure(u, c.proc)
    {
        // Native returns an A/W procedure thunk for a custom class queried via
        // the opposite encoding. A raw guest procedure is not that thunk.
        adv_abi::warn_unavailable(
            env,
            api,
            "cross-encoding custom class procedure thunk is unavailable",
        );
        set_last_error(env, 50);
        return Outcome::Return(0);
    }
    let size = if extended { 48 } else { 40 };
    if adv_abi::guest::check_range(env.cpu, out, size, adv_cpu::backend::Prot::WRITE).is_err() {
        // The checked-pointer profile reports 998 rather than reproducing a native AV.
        set_last_error(env, 998);
        return Outcome::Return(0);
    }
    let mut f = [
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
    let small_icon = c.small_icon;
    let atom = c.atom;
    let encoding = usize::from(wide);
    let mut allocated = 0;
    if let Some(text) = c.menu_text.clone() {
        f[8] = c.menu_query_strings[encoding];
        if f[8] == 0 {
            allocated = super::message::guest_text(env, &text, wide);
            if allocated == 0 {
                set_last_error(env, 8);
                return Outcome::Return(0);
            }
            f[8] = allocated;
        }
    }
    let mut bytes: Vec<_> = f.into_iter().flat_map(u32::to_le_bytes).collect();
    let target = if extended {
        bytes.extend(small_icon.to_le_bytes());
        // Native fills WNDCLASSEX's fields while preserving the caller's cbSize.
        out + 4
    } else {
        out
    };
    if env.cpu.write(target, &bytes).is_err() {
        temp_free(env, allocated);
        set_last_error(env, 998);
        return Outcome::Return(0);
    }
    if allocated != 0 {
        user_mut(env).classes[i].menu_query_strings[encoding] = allocated;
    }
    // Native preserves LastError, returns the class atom as its nonzero BOOL,
    // and copies the original name/atom argument into lpszClassName.
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

#[win32(user32, name = "GetClassNameW", level = state)]
fn get_class_name_w(env: &mut Env<'_>, hwnd: AbiHandle, output: WideStr, capacity: i32) -> i32 {
    let u = user(env);
    let Some(window) = u.windows.get(&env.sys.objects, &hwnd.0) else {
        set_last_error(env, 1400);
        return 0;
    };
    // Native rejects a buffer with no room for a character and the terminator,
    // without changing it. The signed API capacity is consumed as an unsigned size.
    if (capacity as u32) < 2 {
        set_last_error(env, 122);
        return 0;
    }
    let units: Vec<_> = u.classes[window.class]
        .name
        .encode_utf16()
        .take(capacity as u32 as usize - 1)
        .collect();
    let length = units.len();
    let bytes: Vec<_> = units
        .into_iter()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect();
    if adv_abi::guest::check_range(
        env.cpu,
        output.0.addr(),
        bytes.len() as u32,
        adv_cpu::backend::Prot::WRITE,
    )
    .is_err()
        || env.cpu.write(output.0.addr(), &bytes).is_err()
    {
        set_last_error(env, 998);
        return 0;
    }
    length as i32
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
    let args = std::array::from_fn(|i| env.arg(i as u32));
    create_window_ex(env, args, false)
}

#[win32(user32, name = "CreateWindowExW", level = implement)]
#[allow(clippy::too_many_arguments)]
fn create_window_ex_w(
    env: &mut Env<'_>,
    ex_style: u32,
    class: WideStr,
    name: WideStr,
    style: u32,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    parent: AbiHandle,
    menu: AbiHandle,
    instance: AbiHandle,
    param: GuestPtr<u8>,
) -> Outcome {
    create_window_ex(
        env,
        [
            ex_style,
            class.0.addr(),
            name.0.addr(),
            style,
            x as u32,
            y as u32,
            width as u32,
            height as u32,
            parent.0,
            menu.0,
            instance.0,
            param.addr(),
        ],
        true,
    )
}

fn create_window_ex(env: &mut Env<'_>, a: [u32; 12], wide: bool) -> Outcome {
    let class = if a[1] < 0x1_0000 {
        ClassKey::Atom(a[1] as u16)
    } else {
        let Some(name) = super::message::read_text(env, a[1], wide) else {
            // Guarded guest-pointer failure, rather than claiming native bad-pointer AVs.
            set_last_error(env, 998);
            return Outcome::Return(0);
        };
        ClassKey::Name(name)
    };
    let Some(text) = super::message::read_text(env, a[2], wide) else {
        set_last_error(env, 998);
        return Outcome::Return(0);
    };
    let p = CreateParams {
        ex_style: a[0],
        class,
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
    create_window_encoded(env, p, wide, |_, hwnd| Outcome::Return(hwnd))
}

/// CreateWindowEx: make the window, then send WM_NCCREATE, WM_CREATE, WM_SIZE, WM_MOVE
/// (and WM_PARENTNOTIFY, and the show messages for WS_VISIBLE). `then` gets the HWND, or 0.
pub fn create_window(
    env: &mut Env<'_>,
    p: CreateParams,
    then: impl FnOnce(&mut Env<'_>, u32) -> Outcome + 'static,
) -> Outcome {
    create_window_encoded(env, p, false, then)
}

/// A/W creation shares one lifecycle. Registered guest classes retain their ABI;
/// dual-encoding built-in classes inherit the caller's encoding.
pub(super) fn create_window_encoded(
    env: &mut Env<'_>,
    mut p: CreateParams,
    wide: bool,
    then: impl FnOnce(&mut Env<'_>, u32) -> Outcome + 'static,
) -> Outcome {
    let unicode = user(env)
        .find_class(&p.class, p.instance)
        .and_then(|index| user(env).classes[index].unicode)
        .unwrap_or(wide);
    let mut scratch = Vec::new();
    if unicode != wide {
        if !matches!(env.ext.get::<Win32>().codec.acp, 932 | 936 | 1252 | 65001) {
            adv_abi::warn_unavailable(
                env,
                if wide {
                    "user32.dll!CreateWindowExW"
                } else {
                    "user32.dll!CreateWindowExA"
                },
                "CREATESTRUCT ANSI conversion has no supported encoder",
            );
            set_last_error(env, 50);
            return then(env, 0);
        }
        if p.class_ptr >= 0x1_0000
            && let ClassKey::Name(name) = &p.class
        {
            p.class_ptr = super::message::guest_text(env, name, unicode);
            scratch.push(p.class_ptr);
        }
        if p.name_ptr >= 0x1_0000 {
            p.name_ptr = super::message::guest_text(env, &p.text, unicode);
            scratch.push(p.name_ptr);
        }
        if scratch.contains(&0) {
            for pointer in scratch {
                temp_free(env, pointer);
            }
            set_last_error(env, 8);
            return then(env, 0);
        }
    }
    create_window_inner(env, p, unicode, move |env, hwnd| {
        for pointer in scratch {
            temp_free(env, pointer);
        }
        then(env, hwnd)
    })
}

fn create_window_inner(
    env: &mut Env<'_>,
    p: CreateParams,
    unicode: bool,
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
    win.dc_style = u.classes[ci].style & 0x60; // CS_OWNDC | CS_CLASSDC.
    win.owner = if child {
        0
    } else {
        u.root(&env.sys.objects, p.parent)
    };
    win.id = if child { p.menu } else { 0 };
    win.instance = p.instance;
    win.unicode = unicode;
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

    // CREATESTRUCTA/W share the x86 layout. String pointers use the registered
    // procedure's encoding; class atoms and opaque lpCreateParams remain unchanged.
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
    let bytes: Vec<_> = fields.into_iter().flat_map(u32::to_le_bytes).collect();
    if cs == 0 || env.cpu.write(cs, &bytes).is_err() {
        temp_free(env, cs);
        env.ext
            .get_mut::<crate::Win32>()
            .user
            .remove_window(hwnd, &mut env.sys.objects);
        set_last_error(env, 8);
        return then(env, 0);
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
                    crate::gdi32::raster::front::retire_window(env, *h);
                    super::property::retire_window(env, *h);
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

fn animate_window(env: &mut Env<'_>) -> Outcome {
    let (hwnd, _time, flags) = (env.arg(0), env.arg(1), env.arg(2));
    if !user(env)
        .windows
        .get(&env.sys.objects, &hwnd)
        .is_some_and(|w| !w.destroying)
    {
        set_last_error(env, 1400);
        return Outcome::Return(0);
    }
    const AW_HIDE: u32 = 0x0001_0000;
    let cmd = if flags & AW_HIDE != 0 {
        SW_HIDE
    } else {
        SW_SHOW
    };
    show_window(env, hwnd, cmd, |_, _| Outcome::Return(1))
}

const SW_PARENTCLOSING: u32 = 1;
const SW_PARENTOPENING: u32 = 3;

/// The owner tells its popups through WM_SHOWWINDOW; DefWindowProc does the hiding and
/// showing, so a popup procedure that swallows the message keeps its state.
fn show_owned_popups(env: &mut Env<'_>) -> Outcome {
    let (owner, show) = (env.arg(0), env.arg(1) != 0);
    let u = &mut env.ext.get_mut::<crate::Win32>().user;
    if !u
        .windows
        .get(&env.sys.objects, &owner)
        .is_some_and(|w| !w.destroying)
    {
        set_last_error(env, 1400);
        return Outcome::Return(0);
    }
    let mut msgs = VecDeque::new();
    for &top in &u.top {
        let Some(w) = u.windows.get_mut(&mut env.sys.objects, &top) else {
            continue;
        };
        if w.owner != owner || w.destroying {
            continue;
        }
        if show && w.hidden_popup {
            msgs.push_back([top, wm::SHOWWINDOW, 1, SW_PARENTOPENING]);
        } else if !show && w.visible() {
            w.hidden_popup = true;
            msgs.push_back([top, wm::SHOWWINDOW, 0, SW_PARENTCLOSING]);
        }
    }
    send_all(env, msgs, |_| Outcome::Return(1))
}

/// DefWindowProc's WM_SHOWWINDOW: an owner-driven show or hide applies to popups only.
pub(super) fn parent_show(
    env: &mut Env<'_>,
    hwnd: u32,
    show: bool,
    reason: u32,
    then: impl FnOnce(&mut Env<'_>, u32) -> Outcome + 'static,
) -> Outcome {
    let Some(w) = user(env).windows.get(&env.sys.objects, &hwnd) else {
        return then(env, 0);
    };
    if reason == 0 || w.style & WS_POPUP == 0 || w.visible() == show {
        return then(env, 0);
    }
    let cmd = if show { SW_SHOWNOACTIVATE } else { SW_HIDE };
    show_window(env, hwnd, cmd, |env, _| then(env, 0))
}

fn set_layered_window_attributes(env: &mut Env<'_>) -> Outcome {
    let hwnd = env.arg(0);
    if !user(env)
        .windows
        .get(&env.sys.objects, &hwnd)
        .is_some_and(|w| !w.destroying)
    {
        set_last_error(env, 1400);
        return Outcome::Return(0);
    }
    Outcome::Return(1)
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
    let minimize_area = u
        .windows
        .get(&env.sys.objects, &hwnd)
        .and_then(|window| u.windows.get(&env.sys.objects, &window.parent))
        .map_or((sw, sh - 40), Window::client_size);
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
        w.hidden_popup = false;
        if !was {
            w.style |= WS_VISIBLE;
            w.invalidate_client(true);
            msgs.push([hwnd, wm::SHOWWINDOW, 1, 0]);
        }
        if let Some(kind) = super::placement::show_state(w, cmd, (sw, sh), minimize_area) {
            let size = if kind == 1 {
                0
            } else {
                let (cw, ch) = w.client_size();
                make_long(cw, ch)
            };
            msgs.push([hwnd, wm::SIZE, kind, size]);
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
            super::placement::position_changed(w);
            if flags & SWP_NOSIZE == 0 {
                w.invalidate_client(true);
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
    let u = user(env);
    let paint = u
        .windows
        .get(&env.sys.objects, &hwnd)
        .is_some_and(|w| u.paints(w));
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
const MAX_SAVED_PROCEDURE_ENCODINGS: usize = 4096;

fn can_capture_procedures(u: &User, procedures: &[u32]) -> bool {
    let mut missing = 0;
    for (index, proc) in procedures.iter().enumerate() {
        if !u.saved_procedure_encodings.contains_key(proc) && !procedures[..index].contains(proc) {
            missing += 1;
        }
    }
    u.saved_procedure_encodings.len() + missing <= MAX_SAVED_PROCEDURE_ENCODINGS
}

fn get_window_long_a(env: &mut Env<'_>) -> Outcome {
    let (hwnd, idx) = (env.arg(0), env.arg(1) as i32);
    get_window_long(env, hwnd, idx, false)
}

#[win32(user32, name = "GetWindowLongW", level = state)]
fn get_window_long_w(env: &mut Env<'_>, hwnd: AbiHandle, index: i32) -> Outcome {
    get_window_long(env, hwnd.0, index, true)
}

fn procedure_encoding(env: &Env<'_>, proc: u32) -> Option<bool> {
    let u = user(env);
    // Shared host controls decode using the target USER window, not a global
    // encoding attached to their common address. Captured A and W windows may
    // therefore legitimately return this same procedure address.
    if stock_procedure(u, proc) {
        return None;
    }
    let mut mask = u.saved_procedure_encodings.get(&proc).copied().unwrap_or(0);
    for class in &u.classes {
        if class.proc == proc && !class.name.is_empty() {
            if let Some(wide) = class.unicode {
                mask |= if wide { 2 } else { 1 };
            }
        }
    }
    match mask {
        1 => Some(false),
        2 => Some(true),
        _ => None,
    }
}

fn stock_procedure(u: &User, proc: u32) -> bool {
    proc != 0
        && [
            u.procs.def_window,
            u.procs.def_dlg,
            u.procs.button,
            u.procs.static_,
            u.procs.edit,
            u.procs.combo,
            u.procs.trackbar,
            u.procs.tab,
            u.procs.rich_edit,
        ]
        .contains(&proc)
}

fn procedure_boundary(env: &mut Env<'_>, api: &str, reason: &str) -> Outcome {
    adv_abi::warn_unavailable(env, api, reason);
    set_last_error(env, 50);
    Outcome::Return(0)
}

fn get_window_long(env: &mut Env<'_>, hwnd: u32, idx: i32, wide: bool) -> Outcome {
    let Some(w) = user(env).windows.get(&env.sys.objects, &hwnd) else {
        set_last_error(env, 1400);
        return Outcome::Return(0);
    };
    if idx == GWL_WNDPROC && w.unicode != wide {
        return procedure_boundary(
            env,
            if wide {
                "user32.dll!GetWindowLongW"
            } else {
                "user32.dll!GetWindowLongA"
            },
            "cross-encoding GWL_WNDPROC query requires an unavailable procedure thunk",
        );
    }
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
        Some(v) => {
            if idx == GWL_WNDPROC {
                if !can_capture_procedures(user(env), &[v]) {
                    return procedure_boundary(
                        env,
                        if wide {
                            "user32.dll!GetWindowLongW"
                        } else {
                            "user32.dll!GetWindowLongA"
                        },
                        "saved procedure encoding metadata reached its bounded capacity",
                    );
                }
                *user_mut(env)
                    .saved_procedure_encodings
                    .entry(v)
                    .or_default() |= if wide { 2 } else { 1 };
            }
            Outcome::Return(v)
        }
        None => {
            set_last_error(env, 1413); // ERROR_INVALID_INDEX
            Outcome::Return(0)
        }
    }
}

fn set_window_long_a(env: &mut Env<'_>) -> Outcome {
    let (hwnd, idx, v) = (env.arg(0), env.arg(1) as i32, env.arg(2));
    set_window_long(env, hwnd, idx, v, false)
}

#[win32(user32, name = "SetWindowLongW", level = state)]
fn set_window_long_w(env: &mut Env<'_>, hwnd: AbiHandle, index: i32, value: i32) -> Outcome {
    set_window_long(env, hwnd.0, index, value as u32, true)
}

fn set_window_long(env: &mut Env<'_>, hwnd: u32, idx: i32, v: u32, wide: bool) -> Outcome {
    let Some(current) = user(env).windows.get(&env.sys.objects, &hwnd) else {
        set_last_error(env, 1400);
        return Outcome::Return(0);
    };
    if idx == GWL_HWNDPARENT {
        return procedure_boundary(
            env,
            if wide {
                "user32.dll!SetWindowLongW"
            } else {
                "user32.dll!SetWindowLongA"
            },
            "GWL_HWNDPARENT reparent/owner lifecycle is unsupported",
        );
    }
    if idx == GWL_WNDPROC {
        if current.unicode != wide
            || procedure_encoding(env, v).is_some_and(|target| target != wide)
        {
            return procedure_boundary(
                env,
                if wide {
                    "user32.dll!SetWindowLongW"
                } else {
                    "user32.dll!SetWindowLongA"
                },
                "cross-encoding GWL_WNDPROC replacement requires an unavailable procedure thunk",
            );
        }
        if v == 0 {
            set_last_error(env, 87);
            return Outcome::Return(0);
        }
        if !can_capture_procedures(user(env), &[current.proc, v]) {
            return procedure_boundary(
                env,
                if wide {
                    "user32.dll!SetWindowLongW"
                } else {
                    "user32.dll!SetWindowLongA"
                },
                "saved procedure encoding metadata reached its bounded capacity",
            );
        }
    }
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
        GWL_WNDPROC => {
            w.unicode = wide;
            Some(std::mem::replace(&mut w.proc, v))
        }
        GWL_HINSTANCE => Some(std::mem::replace(&mut w.instance, v)),
        GWL_ID => Some(std::mem::replace(&mut w.id, v)),
        GWL_STYLE => Some(std::mem::replace(&mut w.style, v)),
        GWL_EXSTYLE => Some(std::mem::replace(&mut w.ex_style, v)),
        GWL_USERDATA => Some(std::mem::replace(&mut w.user_data, v)),
        i if i >= 0 => w.set_extra_u32(i as u32, v),
        _ => None,
    };
    match old {
        Some(o) => {
            if idx == GWL_WNDPROC {
                let bit = if wide { 2 } else { 1 };
                let saved = &mut user_mut(env).saved_procedure_encodings;
                *saved.entry(o).or_default() |= bit;
                *saved.entry(v).or_default() |= bit;
            }
            set_last_error(env, err::SUCCESS);
            Outcome::Return(o)
        }
        None => {
            set_last_error(env, 1413);
            Outcome::Return(0)
        }
    }
}

/// Raw guest callbacks use the caller's declared ABI without HWND validation.
/// Shared USER procedures use the captured window encoding for supported text
/// conversion; known opposite-encoding guest procedures still require a thunk.
#[allow(clippy::too_many_arguments)]
pub(super) fn call_window_proc(
    env: &mut Env<'_>,
    proc: u32,
    hwnd: u32,
    msg: u32,
    wp: u32,
    lp: u32,
    wide: bool,
) -> Outcome {
    let api = if wide {
        "user32.dll!CallWindowProcW"
    } else {
        "user32.dll!CallWindowProcA"
    };
    if procedure_encoding(env, proc).is_some_and(|target| target != wide) {
        return procedure_boundary(
            env,
            api,
            "saved/custom procedure A/W conversion thunk is unavailable",
        );
    }
    let u = user(env);
    let stock = stock_procedure(u, proc);
    if stock {
        let Some(target) = u.windows.get(&env.sys.objects, &hwnd) else {
            return procedure_boundary(
                env,
                api,
                "shared built-in procedure requires a captured USER window",
            );
        };
        if target.unicode != wide {
            if matches!(msg, wm::SETTEXT | wm::GETTEXT | wm::GETTEXTLENGTH) {
                return super::message::call_proc_encoded(env, proc, hwnd, msg, wp, lp, wide);
            }
            // Ordinary controls delegate these messages to our default handler:
            // CREATE/NCCREATE do not read CREATESTRUCT strings, and destruction
            // has no encoding-sensitive payload. DialogWndProc instead invokes
            // an application DLGPROC first, so it cannot use this exception.
            let creation_metadata =
                matches!(msg, wm::CREATE | wm::NCCREATE | wm::DESTROY | wm::NCDESTROY)
                    && [
                        u.procs.def_window,
                        u.procs.button,
                        u.procs.static_,
                        u.procs.edit,
                        u.procs.combo,
                        u.procs.trackbar,
                        u.procs.tab,
                        u.procs.rich_edit,
                    ]
                    .contains(&proc);
            let numeric = matches!(
                msg,
                0 | wm::MOVE | wm::SIZE | wm::SETFONT | wm::GETFONT
                | wm::ENABLE | 0x000b /* WM_SETREDRAW */ | wm::PAINT | wm::ERASEBKGND
                | wm::SETFOCUS | wm::KILLFOCUS | wm::GETDLGCODE
                | wm::WINDOWPOSCHANGING | wm::WINDOWPOSCHANGED
                | wm::KEYDOWN | wm::KEYUP | wm::SYSKEYDOWN | 0x0105 /* WM_SYSKEYUP */
            );
            if !creation_metadata && !numeric {
                return procedure_boundary(
                    env,
                    api,
                    "saved built-in message needs an unavailable A/W structure or character conversion",
                );
            }
        }
    }
    // Call the saved procedure itself, never SendMessage/current w.proc.
    super::message::call_proc(proc, hwnd, msg, wp, lp, |_, r| Outcome::Return(r))
}

#[win32(user32, name = "CallWindowProcW", level = implement)]
fn call_window_proc_w(
    env: &mut Env<'_>,
    proc: GuestPtr<()>,
    hwnd: AbiHandle,
    msg: u32,
    wp: u32,
    lp: i32,
) -> Outcome {
    call_window_proc(env, proc.addr(), hwnd.0, msg, wp, lp as u32, true)
}

fn get_class_long_a(env: &mut Env<'_>) -> Outcome {
    get_class_long(env, false)
}
#[win32(user32, name = "GetClassLongW", level = state)]
fn get_class_long_w(env: &mut Env<'_>, _hwnd: AbiHandle, _index: i32) -> Outcome {
    get_class_long(env, true)
}
fn get_class_long(env: &mut Env<'_>, wide: bool) -> Outcome {
    let (hwnd, idx) = (env.arg(0), env.arg(1) as i32);
    let u = user(env);
    let Some(w) = u.windows.get(&env.sys.objects, &hwnd) else {
        set_last_error(env, 1400);
        return Outcome::Return(0);
    };
    let index = w.class;
    let c = &u.classes[index];
    let api = if wide {
        "user32.dll!GetClassLongW"
    } else {
        "user32.dll!GetClassLongA"
    };
    if idx == -24 && c.unicode.is_some_and(|encoding| encoding != wide) {
        return procedure_boundary(
            env,
            api,
            "cross-encoding class procedure query requires an unavailable thunk",
        );
    }
    if idx == -8
        && let Some(text) = c.menu_text.clone()
    {
        let encoding = usize::from(wide);
        let cached = c.menu_query_strings[encoding];
        if cached != 0 {
            return Outcome::Return(cached);
        }
        let pointer = super::message::guest_text(env, &text, wide);
        if pointer == 0 {
            set_last_error(env, 8);
            return Outcome::Return(0);
        }
        user_mut(env).classes[index].menu_query_strings[encoding] = pointer;
        return Outcome::Return(pointer);
    }
    Outcome::Return(match idx {
        -24 => c.proc,
        -26 => c.style,
        -14 => c.icon,
        -34 => c.small_icon,
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
    set_class_long(env, false)
}
#[win32(user32, name = "SetClassLongW", level = state)]
fn set_class_long_w(env: &mut Env<'_>, _hwnd: AbiHandle, _index: i32, _value: i32) -> Outcome {
    set_class_long(env, true)
}
fn set_class_long(env: &mut Env<'_>, wide: bool) -> Outcome {
    let (hwnd, idx, v) = (env.arg(0), env.arg(1) as i32, env.arg(2));
    let u = &mut env.ext.get_mut::<crate::Win32>().user;
    let Some(ci) = u.windows.get(&env.sys.objects, &hwnd).map(|w| w.class) else {
        set_last_error(env, 1400);
        return Outcome::Return(0);
    };
    let api = if wide {
        "user32.dll!SetClassLongW"
    } else {
        "user32.dll!SetClassLongA"
    };
    if idx == -24
        && (u.classes[ci]
            .unicode
            .is_some_and(|encoding| encoding != wide)
            || procedure_encoding(env, v).is_some_and(|encoding| encoding != wide))
    {
        return procedure_boundary(
            env,
            api,
            "cross-encoding class procedure replacement requires an unavailable thunk",
        );
    }
    if idx == -24 {
        let previous = user(env).classes[ci].proc;
        if !can_capture_procedures(user(env), &[previous, v]) {
            return procedure_boundary(
                env,
                api,
                "saved class procedure encoding metadata reached its bounded capacity",
            );
        }
        let old_encoding = user(env).classes[ci].unicode;
        let previous_is_stock = stock_procedure(user(env), previous);
        let next_is_stock = stock_procedure(user(env), v);
        let state = user_mut(env);
        if !previous_is_stock && let Some(encoding) = old_encoding {
            *state.saved_procedure_encodings.entry(previous).or_default() |=
                if encoding { 2 } else { 1 };
        }
        if !next_is_stock {
            *state.saved_procedure_encodings.entry(v).or_default() |= if wide { 2 } else { 1 };
        }
        state.classes[ci].unicode = if next_is_stock { None } else { Some(wide) };
        return Outcome::Return(std::mem::replace(&mut state.classes[ci].proc, v));
    }
    let c = &mut user_mut(env).classes[ci];
    let slot = match idx {
        -26 => &mut c.style,
        -14 => &mut c.icon,
        -34 => &mut c.small_icon,
        -12 => &mut c.cursor,
        -10 => &mut c.background,
        _ => return Outcome::Return(0),
    };
    Outcome::Return(std::mem::replace(slot, v))
}

#[win32(user32, name = "SetParent", level = state)]
fn set_parent(env: &mut Env<'_>, hwnd: AbiHandle, parent: AbiHandle) -> Outcome {
    let desktop = user(env).desktop;
    let target = if parent.0 == 0 { desktop } else { parent.0 };
    let state = user(env);
    let Some(w) = state.windows.get(&env.sys.objects, &hwnd.0) else {
        set_last_error(env, 1400);
        return Outcome::Return(0);
    };
    if hwnd.0 == desktop || !state.is_window(&env.sys.objects, target) {
        set_last_error(env, 1400);
        return Outcome::Return(0);
    }
    if state.subtree(&env.sys.objects, hwnd.0).contains(&target) {
        set_last_error(env, 87);
        return Outcome::Return(0);
    }
    let old = w.parent;
    let previous = if old == 0 { desktop } else { old };
    if target == previous {
        return Outcome::Return(previous);
    }
    let state = &mut env.ext.get_mut::<crate::Win32>().user;
    if old == 0 {
        state.top.retain(|&h| h != hwnd.0);
    } else if let Some(w) = state.windows.get_mut(&mut env.sys.objects, &old) {
        w.children.retain(|&h| h != hwnd.0);
        w.needs_paint = true;
    }
    if target == desktop {
        state.top.insert(0, hwnd.0);
    } else if let Some(w) = state.windows.get_mut(&mut env.sys.objects, &target) {
        w.children.insert(0, hwnd.0);
        w.needs_paint = true;
    }
    let w = state
        .windows
        .get_mut(&mut env.sys.objects, &hwnd.0)
        .unwrap();
    // Native SetParent preserves local coordinates and CHILD/POPUP styles.
    // Desktop parentage remains the existing zero-parent traversal convention.
    w.parent = if target == desktop { 0 } else { target };
    if target == desktop {
        w.style |= 0x04000000;
    } // WS_CLIPSIBLINGS
    w.needs_paint = true;
    set_last_error(env, 0);
    Outcome::Return(previous)
}

fn get_parent(env: &mut Env<'_>) -> Outcome {
    let hwnd = env.arg(0);
    let Some(w) = user(env).windows.get(&env.sys.objects, &hwnd) else {
        return Outcome::Return(0);
    };
    Outcome::Return(if w.style & WS_CHILD != 0 {
        if w.parent == 0 {
            user(env).desktop
        } else {
            w.parent
        }
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

#[win32(user32,name="GetTopWindow",level=state)]
fn get_top_window(env: &mut Env<'_>, hwnd: AbiHandle) -> Outcome {
    let u = user(env);
    if hwnd.0 == 0 || hwnd.0 == u.desktop {
        return Outcome::Return(
            u.top
                .iter()
                .copied()
                .find(|&h| h != u.desktop && u.is_window(&env.sys.objects, h))
                .unwrap_or(0),
        );
    }
    let Some(w) = u.windows.get(&env.sys.objects, &hwnd.0) else {
        set_last_error(env, 1400);
        return Outcome::Return(0);
    };
    Outcome::Return(w.children.first().copied().unwrap_or(0))
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
        5 if hwnd == u.desktop => u.top.iter().copied().find(|&h| h != u.desktop).unwrap_or(0),
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

fn find_window_w(env: &mut Env<'_>) -> Outcome {
    let (class, title) = (env.arg(0), env.arg(1));
    let class = if class == 0 {
        None
    } else if class > 0xffff {
        let mut u16_chars = Vec::new();
        let mut p = class;
        while let Ok(u) = env.cpu.read_u16(p) {
            if u == 0 {
                break;
            }
            u16_chars.push(u);
            p += 2;
        }
        Some(ClassKey::Name(String::from_utf16_lossy(&u16_chars)))
    } else {
        Some(ClassKey::Atom(class as u16))
    };
    let title = if title == 0 {
        None
    } else {
        let mut u16_chars = Vec::new();
        let mut p = title;
        while let Ok(u) = env.cpu.read_u16(p) {
            if u == 0 {
                break;
            }
            u16_chars.push(u);
            p += 2;
        }
        Some(String::from_utf16_lossy(&u16_chars))
    };
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
    window_text(
        env,
        hwnd,
        wm::SETTEXT,
        0,
        text,
        false,
        "user32.dll!SetWindowTextA",
    )
}

fn get_window_text_a(env: &mut Env<'_>) -> Outcome {
    let (hwnd, buf, cap) = (env.arg(0), env.arg(1), env.arg(2));
    window_text(
        env,
        hwnd,
        wm::GETTEXT,
        cap as i32,
        buf,
        false,
        "user32.dll!GetWindowTextA",
    )
}

fn get_window_text_length_a(env: &mut Env<'_>) -> Outcome {
    let hwnd = env.arg(0);
    window_text(
        env,
        hwnd,
        wm::GETTEXTLENGTH,
        0,
        0,
        false,
        "user32.dll!GetWindowTextLengthA",
    )
}

#[win32(user32, name = "SetWindowTextW", level = implement)]
fn set_window_text_w(env: &mut Env<'_>, hwnd: AbiHandle, text: WideStr) -> Outcome {
    window_text(
        env,
        hwnd.0,
        wm::SETTEXT,
        0,
        text.0.addr(),
        true,
        "user32.dll!SetWindowTextW",
    )
}

#[win32(user32, name = "GetWindowTextW", level = implement)]
fn get_window_text_w(env: &mut Env<'_>, hwnd: AbiHandle, text: WideStr, capacity: i32) -> Outcome {
    window_text(
        env,
        hwnd.0,
        wm::GETTEXT,
        capacity,
        text.0.addr(),
        true,
        "user32.dll!GetWindowTextW",
    )
}

#[win32(user32, name = "GetWindowTextLengthW", level = implement)]
fn get_window_text_length_w(env: &mut Env<'_>, hwnd: AbiHandle) -> Outcome {
    window_text(
        env,
        hwnd.0,
        wm::GETTEXTLENGTH,
        0,
        0,
        true,
        "user32.dll!GetWindowTextLengthW",
    )
}

#[allow(clippy::too_many_arguments)]
fn window_text(
    env: &mut Env<'_>,
    hwnd: u32,
    msg: u32,
    capacity: i32,
    pointer: u32,
    wide: bool,
    api: &str,
) -> Outcome {
    use adv_abi::guest::check_range;
    use adv_cpu::backend::Prot;
    // Native performs these no-work checks before HWND validation/dispatch.
    if msg == wm::GETTEXT && (capacity == 0 || pointer == 0) {
        return Outcome::Return(0);
    }
    let Some(target_wide) = user(env)
        .windows
        .get(&env.sys.objects, &hwnd)
        .map(|w| w.unicode)
    else {
        if msg == wm::GETTEXT && capacity > 0 {
            // A valid output receives an empty string for an invalid HWND.
            let _ = super::message::write_text(env, pointer, 1, "", wide);
        }
        set_last_error(env, 1400);
        return Outcome::Return(0);
    };
    if (!wide || !target_wide)
        && !matches!(env.ext.get::<Win32>().codec.acp, 932 | 936 | 1252 | 65001)
    {
        adv_abi::warn_unavailable(
            env,
            api,
            "window text ANSI code page has no supported encoder",
        );
        set_last_error(env, 50);
        return Outcome::Return(0);
    }
    if msg == wm::GETTEXT {
        // Negative native counts behave as unsigned capacities. The guarded
        // profile refuses unbounded buffers, rather than handing them to a
        // guest procedure or allowing native A/W thunk truncation overruns.
        if capacity < 0 || capacity as usize > crate::util::MAX_STR {
            adv_abi::warn_unavailable(
                env,
                api,
                "window text capacity exceeds the supported bounded buffer profile",
            );
            set_last_error(env, 87);
            return Outcome::Return(0);
        }
        let bytes = capacity as u32 * if wide { 2 } else { 1 };
        if check_range(env.cpu, pointer, bytes, Prot::WRITE).is_err() {
            set_last_error(env, 998);
            return Outcome::Return(0);
        }
    } else if msg == wm::SETTEXT && pointer != 0 {
        let valid = if wide {
            WideStr(adv_abi::GuestPtr::new(pointer))
                .read(env.cpu, crate::util::MAX_STR)
                .is_ok()
        } else {
            AnsiStr(adv_abi::GuestPtr::new(pointer))
                .read(env.cpu, crate::util::MAX_STR)
                .is_ok()
        };
        if !valid {
            // Checked-pointer failure deliberately does not reproduce native
            // bad-pointer WM_SETTEXT success/access-violation side effects.
            set_last_error(env, 998);
            return Outcome::Return(0);
        }
    }
    super::message::send_encoded(
        env,
        hwnd,
        msg,
        capacity as u32,
        pointer,
        wide,
        |_, result| Outcome::Return(result),
    )
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
    reg.register(unregister_class_w_ABI);
    s(reg, "GetClassInfoA", 3, get_class_info_a);
    reg.register(get_class_info_w_ABI);
    reg.register(get_class_info_ex_a_ABI);
    reg.register(get_class_info_ex_w_ABI);
    s(reg, "GetClassNameA", 3, get_class_name_a);
    reg.register(get_class_name_w_ABI);
    s(reg, "CreateWindowExA", 12, create_window_ex_a);
    reg.register(create_window_ex_w_ABI);
    s(reg, "RegisterClassW", 1, |e| register_class_w(e, false));
    s(reg, "RegisterClassExW", 1, |e| register_class_w(e, true));
    s(reg, "DestroyWindow", 1, destroy_window_api);
    s(reg, "ShowWindow", 2, show_window_api);
    s(reg, "AnimateWindow", 3, animate_window);
    s(reg, "ShowOwnedPopups", 2, show_owned_popups);
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
    reg.register(get_window_long_w_ABI);
    reg.register(set_window_long_w_ABI);
    reg.register(call_window_proc_w_ABI);
    s(
        reg,
        "SetLayeredWindowAttributes",
        4,
        set_layered_window_attributes,
    );
    s(reg, "GetClassLongA", 2, get_class_long_a);
    reg.register(get_class_long_w_ABI);
    s(reg, "SetClassLongA", 3, set_class_long_a);
    reg.register(set_class_long_w_ABI);
    s(reg, "GetParent", 1, get_parent);
    reg.register(get_top_window_ABI);
    reg.register(set_parent_ABI);
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
    s(reg, "FindWindowW", 2, find_window_w);
    s(reg, "SetWindowTextA", 2, set_window_text_a);
    s(reg, "GetWindowTextA", 3, get_window_text_a);
    s(reg, "GetWindowTextLengthA", 1, get_window_text_length_a);
    reg.register(set_window_text_w_ABI);
    reg.register(get_window_text_w_ABI);
    reg.register(get_window_text_length_w_ABI);
    s(reg, "SetCapture", 1, set_capture);
    s(reg, "ReleaseCapture", 0, release_capture);
    s(reg, "GetCapture", 0, get_capture);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_class_name_queries_share_window_identity_and_preserve_failed_outputs() {
        use adv_cpu::{JitCpu, backend::Prot};
        use adv_machine::{Clock, Machine, layout::PARK_ADDR};
        let mut registry = Registry::default();
        crate::register(&mut registry);
        assert!(registry.dynamic_available("user32.dll", "GetClassNameW"));
        assert_eq!(
            registry.get("user32.dll", "GetClassNameW").unwrap().conv,
            adv_abi::Conv::Stdcall { argc: 3 }
        );
        let mut machine = Machine::new(
            JitCpu::new(PARK_ADDR, false).unwrap(),
            registry,
            Clock::virtual_time(),
        )
        .unwrap();
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/scratch/class-name-tests");
        Win32::install(
            &mut machine,
            crate::Win32Config::new(&root, "fixture.exe", &root),
        )
        .unwrap();
        machine.spawn("class query", PARK_ADDR, &[], 65536).unwrap();
        let data = machine
            .sys
            .vm
            .alloc(&mut machine.cpu, 4096, Prot::RW)
            .unwrap();
        let state = &mut machine.ext.get_mut::<Win32>().user;
        let hwnd = state.desktop;
        let class = state
            .windows
            .get(&machine.sys.objects, &hwnd)
            .unwrap()
            .class;
        state.classes[class].name = "Window窓🎮".into();
        let units: Vec<_> = state.classes[class].name.encode_utf16().collect();
        let mut e = Env {
            context: machine.sys.current,
            cpu: &mut machine.cpu,
            sys: &mut machine.sys,
            ext: &mut machine.ext,
            thunk: 0,
        };
        for cap in [-1, 2, 4, 1024] {
            e.cpu.write(data, &[0xa5; 64]).unwrap();
            set_last_error(&mut e, 0x4242);
            let count = units.len().min(cap as u32 as usize - 1);
            assert_eq!(
                get_class_name_w(&mut e, AbiHandle(hwnd), WideStr(GuestPtr::new(data)), cap),
                count as i32
            );
            let expected: Vec<_> = units[..count]
                .iter()
                .copied()
                .chain([0])
                .flat_map(u16::to_le_bytes)
                .collect();
            let mut actual = vec![0; expected.len() + 2];
            e.cpu.read(data, &mut actual).unwrap();
            assert_eq!(&actual[..expected.len()], expected);
            assert_eq!(&actual[expected.len()..], &[0xa5; 2]);
            assert_eq!(crate::util::last_error(&e), 0x4242);
        }
        for (handle, cap, error) in [
            (hwnd, 0, 122),
            (hwnd, 1, 122),
            (0, 1024, 1400),
            (0, 0, 1400),
        ] {
            e.cpu.write(data, &[0xa5; 16]).unwrap();
            assert_eq!(
                get_class_name_w(&mut e, AbiHandle(handle), WideStr(GuestPtr::new(data)), cap),
                0
            );
            assert_eq!(crate::util::last_error(&e), error);
            assert_eq!(e.cpu.read_u32(data).unwrap(), 0xa5a5a5a5);
        }
        e.cpu.protect(data, 4096, Prot::READ).unwrap();
        assert_eq!(
            get_class_name_w(&mut e, AbiHandle(hwnd), WideStr(GuestPtr::new(data)), 1024),
            0
        );
        assert_eq!(crate::util::last_error(&e), 998);
        assert_eq!(e.cpu.read_u32(data).unwrap(), 0xa5a5a5a5);
    }

    #[test]
    fn saved_procedure_capacity_preflights_all_unique_entries() {
        let mut objects = Objects::default();
        let mut u = User::new(&mut objects, (800, 600), None).unwrap();
        for proc in 0..MAX_SAVED_PROCEDURE_ENCODINGS as u32 - 1 {
            u.saved_procedure_encodings.insert(proc, 1);
        }
        let before = u.saved_procedure_encodings.clone();
        assert!(!can_capture_procedures(&u, &[0x9000, 0x9001]));
        assert!(can_capture_procedures(&u, &[0x9000, 0x9000]));
        assert!(can_capture_procedures(&u, &[1, 0x9000]));
        assert_eq!(u.saved_procedure_encodings, before);
        u.saved_procedure_encodings.insert(0x9000, 2);
        assert!(!can_capture_procedures(&u, &[0x9001]));
        assert!(can_capture_procedures(&u, &[1, 0x9000]));
        assert_eq!(u.windows.get(&objects, &u.desktop).unwrap().proc, 0);
    }

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
