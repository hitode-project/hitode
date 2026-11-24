//! What the host shows natively for guest windows: dialogs, menus and the
//! windows input reaches, and the events the host sends back. Snapshots are
//! owned data; no guest payload or native platform handle is shared.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

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
    /// What the host shows it as.
    pub kind: ControlKind,
    /// A picture static's bitmap.
    pub image: Option<std::sync::Arc<DialogImage>>,
}

/// How the host shows a standard control natively.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlKind {
    Push,
    Check,
    Radio,
    Group,
    Label,
    /// Etched or drawn rectangles and lines.
    Frame,
    Edit,
    /// Read-only text of many lines, as a license in a rich edit.
    Text,
    Combo,
    /// A list box, or a list view's items.
    List,
    Track,
    Progress,
    /// A picture static with its bitmap.
    Image,
    /// A child dialog, as a property sheet page: its controls show, it does not.
    Container,
    Unavailable,
}

/// A bitmap's pixels, top row first, as BGRA with opaque alpha.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DialogImage {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

/// The rich edit classes Windows and Wine register.
fn rich_edit(class: &str) -> bool {
    ["RichEdit", "RichEdit20A", "RichEdit20W", "RICHEDIT50W"]
        .iter()
        .any(|name| class.eq_ignore_ascii_case(name))
}

/// Whether `class` holds text the guest reads back through `WM_GETTEXT`.
pub fn text_control(class: &str) -> bool {
    class.eq_ignore_ascii_case("Edit") || rich_edit(class)
}

/// How a standard control shows natively: `list_box` says a list box or list
/// view keeps its own item strings, and `image` that a picture has its bitmap.
pub fn kind(class: &str, style: u32, list_box: bool, image: bool) -> ControlKind {
    let is = |name: &str| class.eq_ignore_ascii_case(name);
    if is("Button") {
        // Owner-drawn buttons retain guest input/state; the platform adapter
        // can project their captions without running the guest painter.
        match style & 0xf {
            2 | 3 | 5 | 6 => ControlKind::Check,
            4 | 9 => ControlKind::Radio,
            7 => ControlKind::Group,
            0 | 1 | 8 | 11 => ControlKind::Push,
            _ => ControlKind::Unavailable,
        }
    } else if is("Static") {
        match style & 0x1f {
            0..=3 | 11 | 12 => ControlKind::Label,
            7..=9 | 16..=18 => ControlKind::Frame,
            14 if image => ControlKind::Image,
            _ => ControlKind::Unavailable,
        }
    } else if text_control(class) {
        // ES_MULTILINE with ES_READONLY.
        if style & 0x804 == 0x804 {
            ControlKind::Text
        } else if is("Edit") {
            ControlKind::Edit
        } else {
            ControlKind::Unavailable
        }
    } else if is("ComboBox") && style & 3 == 3 {
        ControlKind::Combo
    } else if (is("ListBox") || is("SysListView32")) && list_box {
        ControlKind::List
    } else if is("msctls_trackbar32") {
        ControlKind::Track
    } else if is("msctls_statusbar32") {
        ControlKind::Label
    } else if is("msctls_progress32") {
        ControlKind::Progress
    } else if is("#32770") {
        ControlKind::Container
    } else {
        ControlKind::Unavailable
    }
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

impl DialogEvent {
    pub fn target(&self) -> DialogTarget {
        match self {
            Self::Click(target) | Self::Focus(target) | Self::Close(target) => *target,
            Self::Select { target, .. }
            | Self::Track { target, .. }
            | Self::Text { target, .. }
            | Self::Key { target, .. } => *target,
        }
    }
}

impl DialogFont {
    /// The em size in guest pixels: a negative LOGFONT height is the em, a
    /// positive one the cell, which an em fills to about 80%.
    pub fn em(&self) -> Option<f64> {
        match self.height {
            0 => None,
            h if h < 0 => Some(f64::from(-h)),
            h => Some(f64::from(h) * 0.8),
        }
    }
}

impl DialogControl {
    pub fn supported(&self) -> bool {
        self.kind != ControlKind::Unavailable
    }

    /// A static's horizontal text alignment: 0 left, 1 center, 2 right.
    pub fn alignment(&self) -> u8 {
        if self.class.eq_ignore_ascii_case("Static") && matches!(self.style & 0x1f, 1 | 2) {
            (self.style & 0x1f) as u8
        } else {
            0
        }
    }

    /// The text as the control paints it: buttons and statics without
    /// SS_NOPREFIX underline the character after `&`, and `&&` shows one `&`.
    pub fn caption(&self) -> String {
        let prefixed = self.class.eq_ignore_ascii_case("Button")
            || (self.class.eq_ignore_ascii_case("Static") && self.style & 0x80 == 0);
        if prefixed {
            without_mnemonics(&self.text)
        } else {
            self.text.clone()
        }
    }
}

/// `text` without mnemonic markers.
pub fn without_mnemonics(text: &str) -> String {
    let mut caption = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '&' || chars.next_if_eq(&'&').is_some() {
            caption.push(c);
        }
    }
    caption
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MenuKey {
    pub window: u32,
    pub menu: u32,
    pub revision: u64,
    pub enabled: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MenuBar {
    pub key: MenuKey,
    pub items: Vec<MenuItem>,
    pub popup: Option<PopupPresentation>,
}

/// Screen placement is projected to the coordinate space of what shows it:
/// the guest window `window` when that shows as its own layer, else the
/// presented client. UI reads the completion token only; no guest objects or
/// native HWNDs are shared.
#[derive(Clone, Debug)]
pub struct PopupPresentation {
    pub window: u32,
    pub x: i32,
    pub y: i32,
    pub flags: u32,
    pub exclude: Option<[i32; 4]>,
    pub active: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl PartialEq for PopupPresentation {
    fn eq(&self, other: &Self) -> bool {
        self.window == other.window
            && self.x == other.x
            && self.y == other.y
            && self.flags == other.flags
            && self.exclude == other.exclude
            && std::sync::Arc::ptr_eq(&self.active, &other.active)
    }
}

impl Eq for PopupPresentation {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MenuItem {
    pub text: Vec<u16>,
    pub flags: u32,
    pub supported: bool,
    pub command: MenuCommand,
    pub submenu: u32,
    pub children: Vec<MenuItem>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MenuCommand {
    pub window: u32,
    pub root: u32,
    pub menu: u32,
    pub position: u32,
    pub id: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuEvent {
    Cancel {
        window: u32,
        root: u32,
    },
    Command(MenuCommand),
    Initialize {
        window: u32,
        root: u32,
        menu: u32,
        position: Option<u16>,
    },
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

#[cfg(test)]
mod tests {
    use super::*;

    fn control(class: &str, style: u32, text: &str) -> DialogControl {
        DialogControl {
            target: DialogTarget::default(),
            class: class.into(),
            style,
            ex_style: 0,
            text: text.into(),
            rect: Rect::default(),
            visible: true,
            enabled: true,
            check: 0,
            items: Vec::new(),
            selection: -1,
            range: None,
            value: 0,
            font: None,
            kind: kind(class, style, true, false),
            image: None,
        }
    }

    #[test]
    fn captions_drop_mnemonics_where_the_control_paints_them() {
        assert_eq!(control("Button", 0, "&Yes").caption(), "Yes");
        assert_eq!(
            control("button", 0, "Save && &Quit").caption(),
            "Save & Quit"
        );
        assert_eq!(control("Static", 0, "&Name:").caption(), "Name:");
        // SS_NOPREFIX statics and edit text show every ampersand.
        assert_eq!(control("Static", 0x80, "A & B").caption(), "A & B");
        assert_eq!(control("Edit", 0, "a&b").caption(), "a&b");
    }

    #[test]
    fn font_em_and_static_alignment_follow_win32() {
        let font = |height| DialogFont {
            face: "Tahoma".into(),
            height,
            weight: 400,
            italic: false,
            charset: 1,
        };
        assert_eq!(font(-11).em(), Some(11.0));
        assert_eq!(font(15).em(), Some(12.0));
        assert_eq!(font(0).em(), None);
        assert_eq!(control("Static", 0x5002_0001, "").alignment(), 1);
        assert_eq!(control("Static", 0x5002_0002, "").alignment(), 2);
        assert_eq!(control("Static", 0x5002_0080, "").alignment(), 0);
        assert_eq!(control("Button", 1, "").alignment(), 0);
    }

    #[test]
    fn standard_controls_show_as_their_kind() {
        let kind = |class, style| kind(class, style, true, false);
        for style in [0x5000_0010, 0x5000_0011, 0x5000_0012] {
            assert_eq!(kind("Static", style), ControlKind::Frame, "{style:#x}");
        }
        assert_eq!(kind("Static", 0x5000_000d), ControlKind::Unavailable);
        assert_eq!(kind("Static", 0x5000_000e), ControlKind::Unavailable);
        assert_eq!(
            super::kind("Static", 0x5000_000e, false, true),
            ControlKind::Image
        );
        // A license: multiline and read-only, in either text class.
        assert_eq!(kind("RichEdit20W", 0x5020_0804), ControlKind::Text);
        assert_eq!(kind("Edit", 0x5020_0804), ControlKind::Text);
        assert_eq!(kind("Edit", 0x5001_0080), ControlKind::Edit);
        assert_eq!(kind("RichEdit20W", 0x5001_0080), ControlKind::Unavailable);
        assert_eq!(
            kind("msctls_progress32", 0x5000_0000),
            ControlKind::Progress
        );
        assert_eq!(kind("SysListView32", 0x5000_0003), ControlKind::List);
        assert_eq!(
            super::kind("SysListView32", 0x5000_1003, false, false),
            ControlKind::Unavailable
        );
        assert_eq!(kind("#32770", 0x5000_0000), ControlKind::Container);
        assert_eq!(kind("SysTreeView32", 0x5000_0000), ControlKind::Unavailable);
    }
}
