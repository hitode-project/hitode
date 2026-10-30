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

/// Whether a standard control can be shown natively.
pub fn supported(class: &str, style: u32, list_box: bool) -> bool {
    // Owner-drawn buttons retain guest input/state; the platform adapter can
    // project their captions without inventing or running the guest painter.
    (class.eq_ignore_ascii_case("Button") && matches!(style & 0xf, 0..=9 | 11))
        || (class.eq_ignore_ascii_case("Static") && matches!(style & 0x1f, 0..=3 | 7..=9 | 11..=12))
        || class.eq_ignore_ascii_case("Edit")
        || (class.eq_ignore_ascii_case("ComboBox") && style & 3 == 3)
        || (class.eq_ignore_ascii_case("ListBox") && list_box)
        || class.eq_ignore_ascii_case("msctls_trackbar32")
        || class.eq_ignore_ascii_case("msctls_statusbar32")
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

/// Screen placement is projected to the guest client's coordinate space. UI
/// reads the completion token only; no guest objects or native HWNDs are shared.
#[derive(Clone, Debug)]
pub struct PopupPresentation {
    pub x: i32,
    pub y: i32,
    pub flags: u32,
    pub exclude: Option<[i32; 4]>,
    pub active: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl PartialEq for PopupPresentation {
    fn eq(&self, other: &Self) -> bool {
        self.x == other.x
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
            supported: true,
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
}
