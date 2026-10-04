//! Native controls are a projection of immutable guest state. All HWNDs, fonts,
//! callbacks and reconciliation stay on the existing UI thread.
use super::*;
use adv_runtime::{DialogControl, DialogEvent, DialogFont, DialogSet, DialogTarget, DialogWindow};
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;
use windows_sys::Win32::UI::Controls::{
    ICC_BAR_CLASSES, INITCOMMONCONTROLSEX, InitCommonControlsEx, TBM_SETPOS, TBM_SETRANGEMAX,
    TBM_SETRANGEMIN,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    EnableWindow, GetFocus, GetKeyState, IsWindowEnabled, SetFocus,
};
use windows_sys::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
// TBM_GETPOS is the WM_USER alias omitted from windows-sys metadata.
const TBM_GETPOS: u32 = WM_USER;

fn failure(operation: &str) -> String {
    format!("{operation} failed: {}", unsafe { GetLastError() })
}
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}
fn text(hwnd: HWND) -> String {
    let length = unsafe { GetWindowTextLengthW(hwnd) }.max(0) as usize;
    let mut buffer = vec![0; length + 1];
    let length = unsafe { GetWindowTextW(hwnd, buffer.as_mut_ptr(), buffer.len() as i32) };
    String::from_utf16_lossy(&buffer[..length.max(0) as usize])
}

#[derive(Default)]
struct Output {
    sink: Option<DialogSink>,
    events: VecDeque<DialogEvent>,
    error: Option<String>,
}
#[derive(Clone)]
struct Binding {
    target: DialogTarget,
    class: String,
    supported: bool,
}
struct DialogData {
    target: DialogTarget,
    syncing: Cell<bool>,
    bindings: RefCell<BTreeMap<usize, Binding>>,
    output: Rc<RefCell<Output>>,
}
impl DialogData {
    fn emit(&self, event: DialogEvent) {
        if self.syncing.get() {
            return;
        }
        let mut output = self.output.borrow_mut();
        if let Some(sink) = output.sink.as_mut() {
            if let Err(error) = sink(event) {
                output.error = Some(error);
            }
        } else {
            output.events.push_back(event);
        }
    }
    fn key(&self, target: DialogTarget, message: u32, wparam: WPARAM) -> bool {
        if matches!(wparam, 9 | 13 | 27) && matches!(message, WM_KEYDOWN | WM_KEYUP | WM_CHAR) {
            if message == WM_KEYDOWN {
                self.emit(DialogEvent::Key {
                    target,
                    key: wparam as u32,
                    shift: unsafe { GetKeyState(0x10) } < 0,
                });
            }
            return true;
        }
        false
    }
    fn notification(&self, message: u32, wparam: WPARAM, lparam: LPARAM) -> bool {
        if !matches!(message, WM_COMMAND | WM_HSCROLL | WM_VSCROLL) || lparam == 0 {
            return false;
        }
        // Clone before delivering: no RefCell borrow survives a callback.
        let binding = self.bindings.borrow().get(&(lparam as usize)).cloned();
        let Some(binding) = binding.filter(|b| b.supported) else {
            return false;
        };
        if self.syncing.get() {
            return true;
        }
        let hwnd = lparam as HWND;
        let code = (wparam >> 16) as u32;
        let event = if matches!(message, WM_HSCROLL | WM_VSCROLL)
            && binding.class.eq_ignore_ascii_case("msctls_trackbar32")
        {
            Some(DialogEvent::Track {
                target: binding.target,
                value: unsafe { SendMessageW(hwnd, TBM_GETPOS, 0, 0) } as i32,
                code: wparam as u16,
            })
        } else if (binding.class.eq_ignore_ascii_case("Button") && code == BN_CLICKED)
            || (binding.class.eq_ignore_ascii_case("Static") && code == STN_CLICKED)
        {
            Some(DialogEvent::Click(binding.target))
        } else if binding.class.eq_ignore_ascii_case("Edit") && code == EN_CHANGE {
            Some(DialogEvent::Text {
                target: binding.target,
                text: text(hwnd),
            })
        } else if binding.class.eq_ignore_ascii_case("ComboBox") && code == CBN_SELCHANGE {
            Some(DialogEvent::Select {
                target: binding.target,
                index: unsafe { SendMessageW(hwnd, CB_GETCURSEL, 0, 0) } as i32,
            })
        } else {
            None
        };
        if let Some(event) = event {
            self.emit(event);
        }
        true
    }
}

unsafe extern "system" fn control_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _: usize,
    reference: usize,
) -> LRESULT {
    // SAFETY: the parent owns this Rc allocation until DestroyWindow has finished
    // synchronously destroying every subclassed child.
    unsafe {
        let data = &*(reference as *const DialogData);
        let binding = data.bindings.borrow().get(&(hwnd as usize)).cloned();
        if let Some(binding) = binding.filter(|b| b.supported) {
            if data.key(binding.target, message, wparam) {
                return 0;
            }
            if message == WM_SETFOCUS {
                data.emit(DialogEvent::Focus(binding.target));
            }
        }
        if data.notification(message, wparam, lparam) {
            return 0;
        }
        DefSubclassProc(hwnd, message, wparam, lparam)
    }
}
unsafe extern "system" fn dialog_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe {
        if message == WM_NCCREATE {
            let create = &*(lparam as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
        }
        let pointer = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const DialogData;
        if !pointer.is_null() {
            let data = &*pointer;
            if message == WM_CLOSE {
                // Closing is a guest request. Only a later snapshot retires HWND.
                data.emit(DialogEvent::Close(data.target));
                return 0;
            }
            if data.key(data.target, message, wparam) || data.notification(message, wparam, lparam)
            {
                return 0;
            }
            if message == WM_NCDESTROY {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            }
        }
        DefWindowProcW(hwnd, message, wparam, lparam)
    }
}

struct Font(HFONT);
impl Font {
    fn new(font: &DialogFont) -> Result<Self, String> {
        let face = wide(&font.face);
        let handle = unsafe {
            CreateFontW(
                font.height,
                0,
                0,
                0,
                font.weight,
                u32::from(font.italic),
                0,
                0,
                font.charset as u32,
                0,
                0,
                0,
                0,
                face.as_ptr(),
            )
        };
        if handle.is_null() {
            return Err(failure("CreateFontW"));
        }
        Ok(Self(handle))
    }
}
impl Drop for Font {
    fn drop(&mut self) {
        unsafe {
            DeleteObject(self.0);
        }
    }
}
fn install_font(hwnd: HWND, source: Option<&DialogFont>) -> Result<Option<Font>, String> {
    let font = source.map(Font::new).transpose()?;
    let handle = font.as_ref().map_or_else(
        || unsafe { GetStockObject(DEFAULT_GUI_FONT) },
        |font| font.0,
    );
    unsafe {
        SendMessageW(hwnd, WM_SETFONT, handle as usize, 1);
    }
    Ok(font)
}
fn control_style(control: &DialogControl) -> u32 {
    if !control.supported {
        return WS_CHILD;
    }
    let mut style = (control.style | WS_CHILD) & !(WS_VISIBLE | WS_DISABLED);
    if control.class.eq_ignore_ascii_case("Button") {
        // Native automatic buttons must never calculate guest check/radio state.
        let kind = match style & 15 {
            3 => 2,
            6 => 5,
            9 => 4,
            kind => kind,
        };
        style = (style & !15) | kind;
    } else if control.class.eq_ignore_ascii_case("ComboBox") {
        // The guest's item order and indices are authoritative across locales.
        style &= !(CBS_SORT as u32);
    }
    style
}
struct Control {
    hwnd: HWND,
    source: DialogControl,
    font: Option<Font>,
}
struct HostDialog {
    hwnd: HWND,
    data: Rc<DialogData>,
    source: DialogWindow,
    controls: BTreeMap<u32, Control>,
    font: Option<Font>,
}
impl HostDialog {
    fn new(
        source: &DialogWindow,
        owner: HWND,
        output: Rc<RefCell<Output>>,
    ) -> Result<Self, String> {
        let class = wide("AdvGuestDialog");
        let title = wide(&source.text);
        let data = Rc::new(DialogData {
            target: source.target,
            syncing: Cell::new(true),
            bindings: RefCell::new(BTreeMap::new()),
            output,
        });
        let hwnd = unsafe {
            let instance = GetModuleHandleW(null());
            let definition = WNDCLASSW {
                lpfnWndProc: Some(dialog_proc),
                hInstance: instance,
                lpszClassName: class.as_ptr(),
                hCursor: LoadCursorW(null_mut(), IDC_ARROW),
                hbrBackground: (COLOR_BTNFACE + 1) as usize as HBRUSH,
                ..std::mem::zeroed()
            };
            if RegisterClassW(&definition) == 0 && GetLastError() != 1410 {
                return Err(failure("RegisterClassW(dialog)"));
            }
            CreateWindowExW(
                source.ex_style
                    | if source.style & 1 != 0 {
                        WS_EX_DLGMODALFRAME
                    } else {
                        0
                    },
                class.as_ptr(),
                title.as_ptr(),
                source.style & !(WS_CHILD | WS_VISIBLE | WS_DISABLED),
                source.rect.left,
                source.rect.top,
                (source.rect.right - source.rect.left).max(1),
                (source.rect.bottom - source.rect.top).max(1),
                owner,
                null_mut(),
                instance,
                Rc::as_ptr(&data).cast_mut().cast(),
            )
        };
        if hwnd.is_null() {
            return Err(failure("CreateWindowExW(dialog)"));
        }
        let mut dialog = Self {
            hwnd,
            data,
            source: source.clone(),
            controls: BTreeMap::new(),
            font: None,
        };
        dialog.font = install_font(hwnd, source.font.as_ref())?;
        dialog.geometry(source)?;
        dialog.update_controls(source)?;
        Ok(dialog)
    }
    fn geometry(&self, source: &DialogWindow) -> Result<(), String> {
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: source.client_size.0.max(1),
            bottom: source.client_size.1.max(1),
        };
        unsafe {
            if AdjustWindowRectExForDpi(
                &mut rect,
                GetWindowLongW(self.hwnd, GWL_STYLE) as u32,
                0,
                GetWindowLongW(self.hwnd, GWL_EXSTYLE) as u32,
                GetDpiForWindow(self.hwnd),
            ) == 0
            {
                return Err(failure("AdjustWindowRectExForDpi(dialog)"));
            }
            if SetWindowPos(
                self.hwnd,
                null_mut(),
                source.rect.left,
                source.rect.top,
                rect.right - rect.left,
                rect.bottom - rect.top,
                SWP_NOZORDER | SWP_NOACTIVATE,
            ) == 0
            {
                return Err(failure("SetWindowPos(dialog)"));
            }
        }
        Ok(())
    }
    fn update_controls(&mut self, source: &DialogWindow) -> Result<(), String> {
        let mut stale: Vec<_> = self
            .controls
            .iter()
            .filter_map(|(&guest, native)| {
                let live = source.controls.iter().any(|c| {
                    c.target == native.source.target
                        && c.class == native.source.class
                        && c.ex_style == native.source.ex_style
                        && control_style(c) == control_style(&native.source)
                });
                (!live).then_some(guest)
            })
            .collect();
        // Recreating a native parent destroys all native descendants, even if
        // their guest identities and styles are unchanged.
        loop {
            let descendants: Vec<_> = self
                .controls
                .iter()
                .filter_map(|(&guest, native)| {
                    (stale.contains(&native.source.target.parent) && !stale.contains(&guest))
                        .then_some(guest)
                })
                .collect();
            if descendants.is_empty() {
                break;
            }
            stale.extend(descendants);
        }
        for guest in stale {
            if let Some(control) = self.controls.remove(&guest) {
                self.data
                    .bindings
                    .borrow_mut()
                    .remove(&(control.hwnd as usize));
                unsafe {
                    DestroyWindow(control.hwnd);
                }
            }
        }
        let mut previous_sibling = BTreeMap::new();
        for control in &source.controls {
            if !self.controls.contains_key(&control.target.hwnd) {
                let parent = if control.target.parent == source.target.hwnd {
                    self.hwnd
                } else {
                    self.controls
                        .get(&control.target.parent)
                        .map_or(self.hwnd, |c| c.hwnd)
                };
                let class = wide(if control.supported {
                    &control.class
                } else {
                    "Static"
                });
                let title = wide(&control.text);
                let hwnd = unsafe {
                    CreateWindowExW(
                        control.ex_style,
                        class.as_ptr(),
                        title.as_ptr(),
                        control_style(control),
                        control.rect.left,
                        control.rect.top,
                        (control.rect.right - control.rect.left).max(1),
                        (control.rect.bottom - control.rect.top).max(1),
                        parent,
                        control.target.id as usize as HMENU,
                        GetModuleHandleW(null()),
                        null(),
                    )
                };
                if hwnd.is_null() {
                    return Err(failure("CreateWindowExW(control)"));
                }
                self.data.bindings.borrow_mut().insert(
                    hwnd as usize,
                    Binding {
                        target: control.target,
                        class: control.class.clone(),
                        supported: control.supported,
                    },
                );
                let font = install_font(hwnd, control.font.as_ref().or(source.font.as_ref()))?;
                self.controls.insert(
                    control.target.hwnd,
                    Control {
                        hwnd,
                        source: control.clone(),
                        font,
                    },
                );
                if unsafe {
                    SetWindowSubclass(hwnd, Some(control_proc), 1, Rc::as_ptr(&self.data) as usize)
                } == 0
                {
                    return Err(failure("SetWindowSubclass"));
                }
                if !control.supported {
                    log::warn!(
                        "native dialog control unavailable: class={} guest={:#x}",
                        control.class,
                        control.target.hwnd
                    );
                }
            }
            let native = self.controls.get_mut(&control.target.hwnd).unwrap();
            let previous = previous_sibling
                .insert(control.target.parent, native.hwnd)
                .unwrap_or(HWND_TOP);
            unsafe {
                SetWindowPos(
                    native.hwnd,
                    previous,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                );
            }
            if native.source.rect != control.rect {
                unsafe {
                    SetWindowPos(
                        native.hwnd,
                        null_mut(),
                        control.rect.left,
                        control.rect.top,
                        (control.rect.right - control.rect.left).max(1),
                        (control.rect.bottom - control.rect.top).max(1),
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                }
            }
            if native.source.font != control.font
                || (control.font.is_none() && self.source.font != source.font)
            {
                native.font =
                    install_font(native.hwnd, control.font.as_ref().or(source.font.as_ref()))?;
            }
            let displayed = if control.supported {
                control.text.clone()
            } else {
                format!("Unavailable: {} · {}", control.class, control.text)
            };
            if text(native.hwnd) != displayed {
                let title = wide(&displayed);
                unsafe {
                    SetWindowTextW(native.hwnd, title.as_ptr());
                }
            }
            unsafe {
                EnableWindow(native.hwnd, i32::from(control.enabled && control.supported));
                if control.class.eq_ignore_ascii_case("Button") && control.supported {
                    SendMessageW(native.hwnd, BM_SETCHECK, control.check as usize, 0);
                }
                if control.class.eq_ignore_ascii_case("msctls_trackbar32") && control.supported {
                    if let Some((min, max)) = control.range {
                        SendMessageW(native.hwnd, TBM_SETRANGEMIN, 0, min as isize);
                        SendMessageW(native.hwnd, TBM_SETRANGEMAX, 0, max as isize);
                    }
                    SendMessageW(native.hwnd, TBM_SETPOS, 1, control.value as isize);
                }
                if control.class.eq_ignore_ascii_case("ComboBox") && control.supported {
                    // The first projection also populates items despite source
                    // being cloned before native control creation.
                    let count = SendMessageW(native.hwnd, CB_GETCOUNT, 0, 0);
                    if native.source.items != control.items || count != control.items.len() as isize
                    {
                        SendMessageW(native.hwnd, CB_RESETCONTENT, 0, 0);
                        for item in &control.items {
                            let item = wide(item);
                            SendMessageW(native.hwnd, CB_ADDSTRING, 0, item.as_ptr() as isize);
                        }
                    }
                    SendMessageW(native.hwnd, CB_SETCURSEL, control.selection as usize, 0);
                }
                ShowWindow(
                    native.hwnd,
                    if control.visible { SW_SHOWNA } else { SW_HIDE },
                );
            }
            native.source = control.clone();
        }
        Ok(())
    }
    fn update(&mut self, source: &DialogWindow, hidden: bool) -> Result<(), String> {
        self.data.syncing.set(true);
        if self.source.rect != source.rect || self.source.client_size != source.client_size {
            self.geometry(source)?;
        }
        if self.source.text != source.text {
            let title = wide(&source.text);
            unsafe {
                SetWindowTextW(self.hwnd, title.as_ptr());
            }
        }
        if self.source.font != source.font {
            self.font = install_font(self.hwnd, source.font.as_ref())?;
        }
        self.update_controls(source)?;
        unsafe {
            EnableWindow(self.hwnd, i32::from(source.enabled));
            let newly_visible = IsWindowVisible(self.hwnd) == 0 && source.visible && !hidden;
            ShowWindow(
                self.hwnd,
                if source.visible && !hidden {
                    SW_SHOWNA
                } else {
                    SW_HIDE
                },
            );
            if source.visible && !hidden && (newly_visible || self.source.focus != source.focus) {
                let root = GetAncestor(GetForegroundWindow(), GA_ROOTOWNER);
                if root == GetAncestor(self.hwnd, GA_ROOTOWNER) {
                    if newly_visible {
                        SetForegroundWindow(self.hwnd);
                    }
                    let focus = self
                        .controls
                        .get(&source.focus)
                        .map_or(self.hwnd, |c| c.hwnd);
                    if IsWindowEnabled(focus) != 0 && IsWindowVisible(focus) != 0 {
                        SetFocus(focus);
                    }
                }
            }
        }
        self.source = source.clone();
        self.data.syncing.set(false);
        Ok(())
    }
}
impl Drop for HostDialog {
    fn drop(&mut self) {
        self.data.syncing.set(true);
        unsafe {
            DestroyWindow(self.hwnd);
        }
        // Font handles remain alive until the native controls have been destroyed.
    }
}

#[derive(Default)]
pub(super) struct HostDialogs {
    dialogs: BTreeMap<u32, HostDialog>,
    owners: BTreeMap<usize, bool>,
    main_enabled: Option<(usize, bool)>,
    output: Rc<RefCell<Output>>,
}
impl HostDialogs {
    fn update(&mut self, main: HWND, set: &DialogSet, hidden: bool) -> Result<(), String> {
        let common = INITCOMMONCONTROLSEX {
            dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
            // Standard USER32 controls need no COMCTL registration. Request only
            // the trackbar class family, also supported without a v6 manifest.
            dwICC: ICC_BAR_CLASSES,
        };
        if unsafe { InitCommonControlsEx(&common) } == 0 {
            return Err(failure("InitCommonControlsEx"));
        }
        let focus = unsafe { GetFocus() };
        let restore_focus = !hidden
            && self.dialogs.values().any(|dialog| {
                !set.dialogs.iter().any(|d| d.target == dialog.source.target)
                    && unsafe { GetAncestor(focus, GA_ROOT) } == dialog.hwnd
            });
        for dialog in self.dialogs.values() {
            dialog.data.syncing.set(true);
        }
        self.dialogs
            .retain(|_, dialog| set.dialogs.iter().any(|d| d.target == dialog.source.target));
        for source in &set.dialogs {
            if !self.dialogs.contains_key(&source.target.hwnd) {
                let owner = self.dialogs.get(&source.owner).map_or(main, |d| d.hwnd);
                let dialog = HostDialog::new(source, owner, self.output.clone())?;
                self.dialogs.insert(source.target.hwnd, dialog);
            }
        }
        let mut desired = BTreeMap::new();
        for source in &set.dialogs {
            let owner = self.dialogs.get(&source.owner).map_or(main, |d| d.hwnd);
            if source.owner != 0 {
                desired
                    .entry(owner as usize)
                    .and_modify(|enabled| *enabled &= source.owner_enabled)
                    .or_insert(source.owner_enabled);
            }
            unsafe {
                SetWindowLongPtrW(
                    self.dialogs[&source.target.hwnd].hwnd,
                    GWLP_HWNDPARENT,
                    if source.owner == 0 { 0 } else { owner as isize },
                );
            }
        }
        let main_enabled = self.main_enabled;
        self.owners.retain(|&owner, previous| {
            if desired.contains_key(&owner) {
                return true;
            }
            unsafe {
                if IsWindow(owner as HWND) != 0 {
                    let enabled = main_enabled
                        .filter(|(main, _)| *main == owner)
                        .map_or(*previous, |(_, enabled)| enabled);
                    EnableWindow(owner as HWND, i32::from(enabled));
                }
            }
            false
        });
        for (&owner, &enabled) in &desired {
            self.owners
                .entry(owner)
                .or_insert_with(|| unsafe { IsWindowEnabled(owner as HWND) != 0 });
            unsafe {
                EnableWindow(owner as HWND, i32::from(enabled));
            }
        }
        for source in &set.dialogs {
            self.dialogs
                .get_mut(&source.target.hwnd)
                .unwrap()
                .update(source, hidden)?;
        }
        if restore_focus && set.dialogs.is_empty() && unsafe { IsWindowEnabled(main) } != 0 {
            unsafe {
                SetForegroundWindow(main);
                SetFocus(main);
            }
        }
        Ok(())
    }
    pub(super) fn hide(&mut self) {
        for dialog in self.dialogs.values() {
            dialog.data.syncing.set(true);
            unsafe {
                ShowWindow(dialog.hwnd, SW_HIDE);
            }
            dialog.data.syncing.set(false);
        }
    }
    pub(super) fn clear(&mut self) {
        self.dialogs.clear();
        for (&owner, &previous) in &self.owners {
            unsafe {
                if IsWindow(owner as HWND) != 0 {
                    let enabled = self
                        .main_enabled
                        .filter(|(main, _)| *main == owner)
                        .map_or(previous, |(_, enabled)| enabled);
                    EnableWindow(owner as HWND, i32::from(enabled));
                }
            }
        }
        self.owners.clear();
    }
    pub(super) fn set_main_enabled(&mut self, hwnd: HWND, enabled: bool) {
        self.main_enabled = Some((hwnd as usize, enabled));
        unsafe {
            EnableWindow(hwnd, i32::from(enabled));
        }
    }
    pub(super) fn take_events(&self) -> std::vec::IntoIter<DialogEvent> {
        self.output
            .borrow_mut()
            .events
            .drain(..)
            .collect::<Vec<_>>()
            .into_iter()
    }
    pub(super) fn take_error(&self) -> Option<String> {
        self.output.borrow_mut().error.take()
    }
}
impl Window {
    pub fn set_dialog_event_sink(&mut self, sink: DialogSink) {
        self.dialogs.output.borrow_mut().sink = Some(sink);
    }
    pub fn set_dialogs(&mut self, set: &DialogSet) -> Result<(), String> {
        self.dialogs.update(self.hwnd, set, self.hidden)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_dialog_projects_guest_state_without_feedback_and_keeps_guest_identity() {
        let mut window = Window::new("dialog projection", 320, 180, true).unwrap();
        let root = DialogTarget {
            dialog: 0x100,
            hwnd: 0x100,
            created_us: 7,
            ..Default::default()
        };
        let target = DialogTarget {
            dialog: root.hwnd,
            hwnd: 0x104,
            parent: root.hwnd,
            id: 1004,
            created_us: 8,
        };
        let mut rect = Default::default();
        // Infer the guest rectangle type through the DTO, keeping shell coupled
        // only to the runtime presentation facade.
        let mut set = DialogSet {
            dialogs: vec![DialogWindow {
                target: root,
                owner: 0x90,
                owner_enabled: false,
                class: "#32770".into(),
                style: WS_POPUP | WS_CAPTION | WS_SYSMENU,
                ex_style: 0,
                text: "設定".into(),
                rect,
                client_size: (220, 160),
                visible: true,
                enabled: true,
                modal: true,
                focus: target.hwnd,
                font: None,
                controls: vec![DialogControl {
                    target,
                    class: "Button".into(),
                    style: WS_CHILD | WS_VISIBLE | WS_TABSTOP | 3,
                    ex_style: 0,
                    text: "&Check".into(),
                    rect: Default::default(),
                    visible: true,
                    enabled: true,
                    check: 1,
                    items: Vec::new(),
                    selection: -1,
                    range: None,
                    value: 0,
                    font: None,
                    supported: true,
                }],
            }],
        };
        rect = set.dialogs[0].controls[0].rect;
        rect.right = 120;
        rect.bottom = 24;
        set.dialogs[0].controls[0].rect = rect;
        let mut track = set.dialogs[0].controls[0].clone();
        track.target.hwnd = 0x108;
        track.target.id = 1001;
        track.class = "msctls_trackbar32".into();
        track.style = WS_CHILD | WS_VISIBLE | WS_TABSTOP;
        track.range = Some((-50, 5000));
        track.value = 4300;
        let track_target = track.target;
        let mut combo = track.clone();
        combo.target.hwnd = 0x10c;
        combo.target.id = 1009;
        combo.class = "ComboBox".into();
        combo.style = WS_CHILD | WS_VISIBLE | WS_TABSTOP | 3 | CBS_SORT as u32;
        combo.items = vec!["Zeta".into(), "Alpha".into()];
        combo.selection = 1;
        let combo_target = combo.target;
        set.dialogs[0].controls.extend([track, combo]);
        window.set_dialogs(&set).unwrap();
        let native = window.dialogs.dialogs[&root.hwnd].hwnd;
        let checkbox = window.dialogs.dialogs[&root.hwnd].controls[&target.hwnd].hwnd;
        let track = window.dialogs.dialogs[&root.hwnd].controls[&track_target.hwnd].hwnd;
        let combo = window.dialogs.dialogs[&root.hwnd].controls[&combo_target.hwnd].hwnd;
        assert_eq!(
            unsafe { IsWindowVisible(native) },
            0,
            "hidden runs keep dialogs hidden"
        );
        assert_eq!(
            unsafe { IsWindowEnabled(window.hwnd) },
            0,
            "guest owner disable is projected"
        );
        assert_eq!(unsafe { SendMessageW(checkbox, BM_GETCHECK, 0, 0) }, 1);
        assert_eq!(unsafe { SendMessageW(track, TBM_GETPOS, 0, 0) }, 4300);
        assert_eq!(unsafe { SendMessageW(combo, CB_GETCURSEL, 0, 0) }, 1);
        let mut first = [0u16; 32];
        let length = unsafe { SendMessageW(combo, CB_GETLBTEXT, 0, first.as_mut_ptr() as isize) };
        assert_eq!(
            String::from_utf16(&first[..length as usize]).unwrap(),
            "Zeta",
            "native locale must not reorder canonical guest indices"
        );
        assert!(
            window.dialogs.take_events().next().is_none(),
            "snapshot sync generates no guest input"
        );
        unsafe {
            SendMessageW(checkbox, BM_CLICK, 0, 0);
        }
        assert_eq!(
            unsafe { SendMessageW(checkbox, BM_GETCHECK, 0, 0) },
            1,
            "native automatic style must not change authoritative guest check state"
        );
        assert_eq!(
            window.dialogs.take_events().collect::<Vec<_>>(),
            vec![DialogEvent::Focus(target), DialogEvent::Click(target)]
        );
        set.dialogs[0].controls[0].check = 0;
        window.set_dialogs(&set).unwrap();
        assert_eq!(unsafe { SendMessageW(checkbox, BM_GETCHECK, 0, 0) }, 0);
        assert!(window.dialogs.take_events().next().is_none());
        unsafe {
            SendMessageW(track, TBM_SETPOS, 1, 1250);
            SendMessageW(native, WM_HSCROLL, 4 | (1250 << 16), track as isize);
            SendMessageW(combo, CB_SETCURSEL, 0, 0);
            SendMessageW(
                native,
                WM_COMMAND,
                combo_target.id as usize | ((CBN_SELCHANGE as usize) << 16),
                combo as isize,
            );
        }
        assert_eq!(
            window.dialogs.take_events().collect::<Vec<_>>(),
            vec![
                DialogEvent::Track {
                    target: track_target,
                    value: 1250,
                    code: 4
                },
                DialogEvent::Select {
                    target: combo_target,
                    index: 0
                },
            ]
        );
        unsafe {
            SendMessageW(checkbox, WM_KEYDOWN, 9, 0);
            SendMessageW(native, WM_CLOSE, 0, 0);
        }
        let input = window.dialogs.take_events().collect::<Vec<_>>();
        assert!(
            matches!(input.as_slice(), [DialogEvent::Key { target: actual, key: 9, .. }, DialogEvent::Close(close)]
            if *actual == target && *close == root)
        );
        assert_ne!(
            unsafe { IsWindow(native) },
            0,
            "native close waits for guest snapshot retirement"
        );
        window.set_dialogs(&DialogSet::default()).unwrap();
        assert_eq!(unsafe { IsWindow(native) }, 0);
        assert_ne!(unsafe { IsWindowEnabled(window.hwnd) }, 0);
        // A later empty menu still carries the guest owner's current state.
        let bar = adv_runtime::MenuBar {
            key: adv_runtime::MenuKey {
                window: 0x90,
                enabled: false,
                ..Default::default()
            },
            items: Vec::new(),
        };
        window.set_menu_bar(&bar).unwrap();
        window.set_dialogs(&set).unwrap();
        window.set_dialogs(&DialogSet::default()).unwrap();
        assert_eq!(
            unsafe { IsWindowEnabled(window.hwnd) },
            0,
            "retiring a dialog must preserve the latest guest owner disable snapshot"
        );
    }
}
