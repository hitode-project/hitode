//! Guest dialogs as AppKit windows of native controls. Like the Win32
//! adapter, controls never compute guest state: a click restores the guest's
//! check, and only a later snapshot changes what is shown.
use super::*;
use adv_runtime::{DialogControl, DialogEvent, DialogFont, DialogSet, DialogTarget, DialogWindow};
use objc2::sel;
use objc2_app_kit::{
    NSBezelStyle, NSBox, NSBoxType, NSButton, NSFont, NSPopUpButton, NSResponder,
    NSSecureTextField, NSSlider, NSTextAlignment, NSTextField, NSView, NSWindowOrderingMode,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Push,
    Check,
    Radio,
    Group,
    Label,
    Frame,
    Edit,
    Combo,
    Track,
    Unavailable,
}

fn kind(control: &DialogControl) -> Kind {
    if !control.supported {
        return Kind::Unavailable;
    }
    let class = control.class.to_ascii_lowercase();
    match class.as_str() {
        "button" => match control.style & 15 {
            2 | 3 | 5 | 6 => Kind::Check,
            4 | 9 => Kind::Radio,
            7 => Kind::Group,
            _ => Kind::Push,
        },
        "static" => match control.style & 0x1f {
            7..=9 => Kind::Frame,
            _ => Kind::Label,
        },
        "edit" => Kind::Edit,
        "combobox" => Kind::Combo,
        "msctls_trackbar32" => Kind::Track,
        "msctls_statusbar32" => Kind::Label,
        _ => Kind::Unavailable,
    }
}

#[derive(Clone)]
struct Binding {
    target: DialogTarget,
    kind: Kind,
    check: u32,
}

/// Events and bindings the callbacks reach.
#[derive(Default)]
pub(in crate::window) struct Output {
    sink: RefCell<Option<DialogSink>>,
    events: RefCell<VecDeque<DialogEvent>>,
    error: RefCell<Option<String>>,
    syncing: Cell<bool>,
    bindings: RefCell<BTreeMap<usize, Binding>>,
    windows: RefCell<BTreeMap<usize, DialogTarget>>,
}
impl Output {
    fn emit(&self, event: DialogEvent) {
        if self.syncing.get() {
            return;
        }
        let sink = self.sink.borrow_mut().take();
        match sink {
            Some(mut sink) => {
                if let Err(error) = sink(event) {
                    self.fail(error);
                }
                self.sink.borrow_mut().get_or_insert(sink);
            }
            None => self.events.borrow_mut().push_back(event),
        }
    }
    pub(super) fn fail(&self, error: String) {
        self.error.borrow_mut().get_or_insert(error);
    }
    pub(super) fn take_error(&self) -> Option<String> {
        self.error.borrow_mut().take()
    }
    pub(super) fn take_events(&self) -> std::vec::IntoIter<DialogEvent> {
        self.events
            .borrow_mut()
            .drain(..)
            .collect::<Vec<_>>()
            .into_iter()
    }
    pub(super) fn close(&self, window: &NSWindow) {
        let target = self
            .windows
            .borrow()
            .get(&(window as *const NSWindow as usize))
            .copied();
        if let Some(target) = target {
            self.emit(DialogEvent::Close(target));
        }
    }
    pub(super) fn action(&self, sender: &NSControl) {
        let at = sender as *const NSControl as usize;
        let Some(binding) = self.bindings.borrow().get(&at).cloned() else {
            return;
        };
        if self.syncing.get() {
            return;
        }
        let target = binding.target;
        match binding.kind {
            Kind::Push | Kind::Check | Kind::Radio => {
                // AppKit toggled the native state; the guest owns it.
                if let Some(button) = sender.downcast_ref::<NSButton>() {
                    button.setState(binding.check as isize);
                }
                self.emit(DialogEvent::Click(target));
            }
            Kind::Combo => {
                if let Some(popup) = sender.downcast_ref::<NSPopUpButton>() {
                    self.emit(DialogEvent::Select {
                        target,
                        index: popup.indexOfSelectedItem() as i32,
                    });
                }
            }
            Kind::Track => {
                let value = sender.integerValue() as i32;
                let released = NSApplication::sharedApplication(sender.mtm())
                    .currentEvent()
                    .is_none_or(|event| event.r#type() != NSEventType::LeftMouseDragged);
                if released {
                    // TB_THUMBPOSITION then TB_ENDTRACK.
                    self.emit(DialogEvent::Track {
                        target,
                        value,
                        code: 4,
                    });
                    self.emit(DialogEvent::Track {
                        target,
                        value,
                        code: 8,
                    });
                } else {
                    // TB_THUMBTRACK.
                    self.emit(DialogEvent::Track {
                        target,
                        value,
                        code: 5,
                    });
                }
            }
            _ => {}
        }
    }
    pub(super) fn text(&self, object: &AnyObject) {
        let at = object as *const AnyObject as usize;
        let Some(binding) = self.bindings.borrow().get(&at).cloned() else {
            return;
        };
        if binding.kind != Kind::Edit {
            return;
        }
        if let Some(field) = object.downcast_ref::<NSTextField>() {
            self.emit(DialogEvent::Text {
                target: binding.target,
                text: field.stringValue().to_string(),
            });
        }
    }
}

define_class!(
    // SAFETY: NSView has no subclassing requirements and the class has no Drop.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    struct FlippedView;

    impl FlippedView {
        /// Guest client coordinates are top-left.
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }
    }
);

/// A guest rectangle (left, top, right, bottom) in flipped client points.
fn frame((left, top, right, bottom): (i32, i32, i32, i32)) -> NSRect {
    NSRect::new(
        NSPoint::new(f64::from(left), f64::from(top)),
        NSSize::new(
            f64::from((right - left).max(1)),
            f64::from((bottom - top).max(1)),
        ),
    )
}

fn font(source: Option<&DialogFont>) -> Retained<NSFont> {
    let Some(source) = source else {
        return NSFont::systemFontOfSize(NSFont::smallSystemFontSize());
    };
    // A negative LOGFONT height is the em size, a positive one the cell.
    let size = if source.height < 0 {
        f64::from(-source.height)
    } else if source.height > 0 {
        f64::from(source.height) * 0.8
    } else {
        NSFont::smallSystemFontSize()
    };
    NSFont::fontWithName_size(&NSString::from_str(&source.face), size).unwrap_or_else(|| {
        if source.weight >= 600 {
            NSFont::boldSystemFontOfSize(size)
        } else {
            NSFont::systemFontOfSize(size)
        }
    })
}

struct Control {
    view: Retained<NSView>,
    kind: Kind,
    source: DialogControl,
}

fn create(mtm: MainThreadMarker, target: &Target, control: &DialogControl) -> Retained<NSView> {
    let text = NSString::from_str(&control.text);
    let action = Some(sel!(dialogAction:));
    let object: Option<&AnyObject> = Some(target.as_ref());
    // SAFETY: the target implements the action and outlives the controls.
    let view: Retained<NSView> = unsafe {
        match kind(control) {
            Kind::Push => {
                let button = NSButton::buttonWithTitle_target_action(&text, object, action, mtm);
                button.setBezelStyle(NSBezelStyle::FlexiblePush);
                Retained::into_super(Retained::into_super(button))
            }
            Kind::Check => {
                let button = NSButton::checkboxWithTitle_target_action(&text, object, action, mtm);
                button.setAllowsMixedState(matches!(control.style & 15, 5 | 6));
                Retained::into_super(Retained::into_super(button))
            }
            Kind::Radio => {
                let button =
                    NSButton::radioButtonWithTitle_target_action(&text, object, action, mtm);
                Retained::into_super(Retained::into_super(button))
            }
            Kind::Group => {
                let group = NSBox::new(mtm);
                group.setTitle(&text);
                Retained::into_super(group)
            }
            Kind::Frame => {
                let group = NSBox::new(mtm);
                group.setBoxType(NSBoxType::Custom);
                group.setBorderWidth(1.0);
                group.setTitlePosition(objc2_app_kit::NSTitlePosition::NoTitle);
                Retained::into_super(group)
            }
            Kind::Label | Kind::Unavailable => {
                let label = if control.style & 0x1f <= 2 {
                    NSTextField::wrappingLabelWithString(&text, mtm)
                } else {
                    NSTextField::labelWithString(&text, mtm)
                };
                label.setAlignment(match control.style & 3 {
                    1 => NSTextAlignment::Center,
                    2 => NSTextAlignment::Right,
                    _ => NSTextAlignment::Left,
                });
                Retained::into_super(Retained::into_super(label))
            }
            Kind::Edit => {
                let field: Retained<NSTextField> = if control.style & 0x20 != 0 {
                    Retained::into_super(NSSecureTextField::new(mtm))
                } else {
                    NSTextField::textFieldWithString(&text, mtm)
                };
                field.setEditable(control.style & 0x800 == 0);
                field.setDelegate(Some(ProtocolObject::from_ref(target)));
                Retained::into_super(Retained::into_super(field))
            }
            Kind::Combo => {
                let popup = NSPopUpButton::initWithFrame_pullsDown(
                    NSPopUpButton::alloc(mtm),
                    frame((0, 0, 1, 1)),
                    false,
                );
                popup.setTarget(object);
                popup.setAction(action);
                Retained::into_super(Retained::into_super(Retained::into_super(popup)))
            }
            Kind::Track => {
                let slider = NSSlider::sliderWithTarget_action(object, action, mtm);
                slider.setContinuous(true);
                Retained::into_super(Retained::into_super(slider))
            }
        }
    };
    view
}

struct HostDialog {
    window: Retained<NSWindow>,
    content: Retained<FlippedView>,
    source: DialogWindow,
    controls: BTreeMap<u32, Control>,
    owner: usize,
}

impl HostDialog {
    fn new(mtm: MainThreadMarker, target: &Target, source: &DialogWindow) -> Self {
        let mut style = NSWindowStyleMask::Titled;
        if source.style & 0x8_0000 != 0 {
            style |= NSWindowStyleMask::Closable;
        }
        let size = NSSize::new(
            f64::from(source.client_size.0.max(1)),
            f64::from(source.client_size.1.max(1)),
        );
        // SAFETY: retained by `HostDialog`, not released on close.
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                NSRect::new(NSPoint::new(0.0, 0.0), size),
                style,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        unsafe { window.setReleasedWhenClosed(false) };
        window.setDelegate(Some(ProtocolObject::from_ref(target)));
        let content: Retained<FlippedView> = unsafe {
            msg_send![FlippedView::alloc(mtm), initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), size)]
        };
        window.setContentView(Some(&content));
        Self {
            window,
            content,
            source: source.clone(),
            controls: BTreeMap::new(),
            owner: 0,
        }
    }

    /// Centers the dialog over its owner, which carries it as a child.
    fn attach(&mut self, owner: &NSWindow) {
        let at = owner as *const NSWindow as usize;
        if self.owner == at {
            return;
        }
        if self.owner != 0
            && let Some(previous) = self.window.parentWindow()
        {
            previous.removeChildWindow(&self.window);
        }
        let outer = owner.frame();
        let own = self.window.frame();
        self.window.setFrameOrigin(NSPoint::new(
            (outer.origin.x + (outer.size.width - own.size.width) / 2.0).floor(),
            (outer.origin.y + (outer.size.height - own.size.height) / 2.0).floor(),
        ));
        // SAFETY: both windows are retained and the child is removed on retire.
        unsafe { owner.addChildWindow_ordered(&self.window, NSWindowOrderingMode::Above) };
        self.owner = at;
    }

    fn update(
        &mut self,
        mtm: MainThreadMarker,
        target: &Target,
        output: &Output,
        source: &DialogWindow,
    ) {
        if self.source.client_size != source.client_size || self.controls.is_empty() {
            self.window.setContentSize(NSSize::new(
                f64::from(source.client_size.0.max(1)),
                f64::from(source.client_size.1.max(1)),
            ));
        }
        self.window.setTitle(&NSString::from_str(&source.text));
        // Controls whose identity or kind changed are recreated.
        let stale: Vec<u32> = self
            .controls
            .iter()
            .filter(|(_, native)| {
                !source.controls.iter().any(|c| {
                    c.target == native.source.target
                        && kind(c) == native.kind
                        && c.style == native.source.style
                })
            })
            .map(|(&hwnd, _)| hwnd)
            .collect();
        for hwnd in stale {
            if let Some(control) = self.controls.remove(&hwnd) {
                output
                    .bindings
                    .borrow_mut()
                    .remove(&(Retained::as_ptr(&control.view) as usize));
                control.view.removeFromSuperview();
            }
        }
        // Parents precede descendants: offsets accumulate into the dialog's
        // flipped client area, where every native control lives.
        let mut origins: BTreeMap<u32, (i32, i32)> = BTreeMap::new();
        origins.insert(source.target.hwnd, (0, 0));
        for control in &source.controls {
            let offset = origins
                .get(&control.target.parent)
                .copied()
                .unwrap_or((0, 0));
            origins.insert(
                control.target.hwnd,
                (offset.0 + control.rect.left, offset.1 + control.rect.top),
            );
            let rect = (
                offset.0 + control.rect.left,
                offset.1 + control.rect.top,
                offset.0 + control.rect.right,
                offset.1 + control.rect.bottom,
            );
            let native = self.controls.entry(control.target.hwnd).or_insert_with(|| {
                let view = create(mtm, target, control);
                self.content.addSubview(&view);
                if !control.supported {
                    log::warn!(
                        "native dialog control unavailable: class={} guest={:#x}",
                        control.class,
                        control.target.hwnd
                    );
                }
                Control {
                    view,
                    kind: kind(control),
                    source: DialogControl {
                        // Forces the first projection of every property.
                        text: "\u{0}".into(),
                        items: vec!["\u{0}".into()],
                        ..control.clone()
                    },
                }
            });
            output.bindings.borrow_mut().insert(
                Retained::as_ptr(&native.view) as usize,
                Binding {
                    target: control.target,
                    kind: native.kind,
                    check: control.check,
                },
            );
            project(native, control, rect, source.font.as_ref());
        }
    }
}

/// Shows the guest state of one control.
fn project(
    native: &mut Control,
    control: &DialogControl,
    rect: (i32, i32, i32, i32),
    inherited: Option<&DialogFont>,
) {
    let view = &native.view;
    view.setFrame(frame(rect));
    view.setHidden(!control.visible);
    let text = if control.supported {
        control.text.clone()
    } else {
        format!("Unavailable: {} · {}", control.class, control.text)
    };
    let font = font(control.font.as_ref().or(inherited));
    // A popup is also a button, so it is matched first.
    if let Some(popup) = view.downcast_ref::<NSPopUpButton>() {
        if native.source.items != control.items {
            popup.removeAllItems();
            for item in &control.items {
                popup.addItemWithTitle(&NSString::from_str(item));
            }
        }
        popup.selectItemAtIndex(control.selection as isize);
        popup.setEnabled(control.enabled);
        popup.setFont(Some(&font));
    } else if let Some(button) = view.downcast_ref::<NSButton>() {
        if native.source.text != text {
            button.setTitle(&NSString::from_str(&text));
        }
        button.setState(control.check as isize);
        button.setEnabled(control.enabled);
        button.setFont(Some(&font));
    } else if let Some(group) = view.downcast_ref::<NSBox>() {
        if native.kind == Kind::Group && native.source.text != text {
            group.setTitle(&NSString::from_str(&text));
        }
        group.setTitleFont(&font);
    } else if let Some(slider) = view.downcast_ref::<NSSlider>() {
        if let Some((min, max)) = control.range {
            slider.setMinValue(f64::from(min));
            slider.setMaxValue(f64::from(max));
        }
        slider.setIntegerValue(control.value as isize);
        slider.setEnabled(control.enabled);
    } else if let Some(field) = view.downcast_ref::<NSTextField>() {
        // Keep the user's caret: only a guest change replaces the text.
        if native.source.text != text && field.stringValue().to_string() != text {
            field.setStringValue(&NSString::from_str(&text));
        }
        field.setEnabled(control.enabled && control.supported);
        field.setFont(Some(&font));
    }
    native.source = DialogControl {
        text,
        ..control.clone()
    };
}

#[derive(Default)]
pub(super) struct Dialogs {
    dialogs: BTreeMap<u32, HostDialog>,
    /// Whether the guest's menu state enables the main window.
    menu_enabled: Option<bool>,
    /// Whether modal dialogs leave the main window input enabled.
    owner_enabled: Option<bool>,
    focus: Option<DialogTarget>,
}

impl Dialogs {
    pub(super) fn main_enabled(&self) -> bool {
        self.owner_enabled.or(self.menu_enabled).unwrap_or(true)
    }
    pub(super) fn set_main_enabled(&mut self, enabled: bool) {
        self.menu_enabled = Some(enabled);
    }
    pub(super) fn owns(&self, window: &NSWindow) -> bool {
        let at = window as *const NSWindow as usize;
        self.dialogs
            .values()
            .any(|dialog| Retained::as_ptr(&dialog.window) as usize == at)
    }

    fn dialog_of(&self, window: usize) -> Option<&HostDialog> {
        self.dialogs
            .values()
            .find(|dialog| Retained::as_ptr(&dialog.window) as usize == window)
    }

    /// The guest control the dialog's first responder belongs to.
    fn focused(&self, dialog: &HostDialog) -> Option<DialogTarget> {
        let responder = dialog.window.firstResponder()?;
        let at = Retained::as_ptr(&responder) as usize;
        dialog.controls.values().find_map(|control| {
            let view = Retained::as_ptr(&control.view) as usize;
            // An editing text field's first responder is its field editor.
            let editing = control
                .view
                .downcast_ref::<NSControl>()
                .and_then(|c| c.currentEditor())
                .is_some_and(|editor| Retained::as_ptr(&editor) as usize == at);
            (view == at || editing).then_some(control.source.target)
        })
    }

    /// Tab, Enter and Escape go to the guest dialog, which owns navigation
    /// and default buttons. Input to a disabled dialog is dropped.
    pub(super) fn intercept(&self, mtm: MainThreadMarker, hub: &Hub, event: &NSEvent) -> bool {
        let kind = event.r#type();
        let Some(window) = event.window(mtm) else {
            return false;
        };
        let Some(dialog) = self.dialog_of(Retained::as_ptr(&window) as usize) else {
            return false;
        };
        if !dialog.source.enabled
            && matches!(
                kind,
                NSEventType::KeyDown
                    | NSEventType::KeyUp
                    | NSEventType::LeftMouseDown
                    | NSEventType::RightMouseDown
            )
        {
            return true;
        }
        if !matches!(kind, NSEventType::KeyDown | NSEventType::KeyUp) {
            return false;
        }
        let key = match event.keyCode() {
            0x30 => 9,
            0x24 | 0x4c => 13,
            0x35 => 27,
            _ => return false,
        };
        if kind == NSEventType::KeyDown {
            let target = self.focused(dialog).unwrap_or(dialog.source.target);
            hub.dialogs.emit(DialogEvent::Key {
                target,
                key,
                shift: event.modifierFlags().contains(NSEventModifierFlags::Shift),
            });
        }
        true
    }

    /// Reports a focus move between controls, as WM_SETFOCUS would.
    pub(super) fn poll_focus(&mut self, hub: &Hub) {
        let focus = self
            .dialogs
            .values()
            .find(|dialog| dialog.window.isKeyWindow())
            .and_then(|dialog| self.focused(dialog));
        if let Some(target) = focus
            && focus != self.focus
        {
            hub.dialogs.emit(DialogEvent::Focus(target));
        }
        self.focus = focus;
    }

    fn update(
        &mut self,
        mtm: MainThreadMarker,
        target: &Target,
        hub: &Hub,
        main: &NSWindow,
        set: &DialogSet,
        hidden: bool,
    ) {
        let output = &hub.dialogs;
        output.syncing.set(true);
        let gone: Vec<u32> = self
            .dialogs
            .iter()
            .filter(|(_, dialog)| !set.dialogs.iter().any(|d| d.target == dialog.source.target))
            .map(|(&hwnd, _)| hwnd)
            .collect();
        let refocus = !gone.is_empty();
        for hwnd in gone {
            if let Some(dialog) = self.dialogs.remove(&hwnd) {
                retire(output, &dialog);
            }
        }
        for source in &set.dialogs {
            self.dialogs
                .entry(source.target.hwnd)
                .or_insert_with(|| HostDialog::new(mtm, target, source));
        }
        // Owners: another projected dialog or the main window.
        let mut desired: BTreeMap<u32, bool> = BTreeMap::new();
        for source in &set.dialogs {
            if source.owner != 0 {
                let owner = if self.dialogs.contains_key(&source.owner) {
                    source.owner
                } else {
                    0
                };
                *desired.entry(owner).or_insert(true) &= source.owner_enabled;
            }
        }
        self.owner_enabled = desired.get(&0).copied();
        let mut windows = BTreeMap::new();
        for source in &set.dialogs {
            let owner_window = self
                .dialogs
                .get(&source.owner)
                .filter(|_| source.owner != source.target.hwnd)
                .map(|owner| owner.window.clone());
            let dialog = self.dialogs.get_mut(&source.target.hwnd).unwrap();
            match &owner_window {
                Some(owner) => dialog.attach(owner),
                None => dialog.attach(main),
            }
            dialog.update(mtm, target, output, source);
            let mut source = source.clone();
            if let Some(&enabled) = desired.get(&source.target.hwnd) {
                source.enabled &= enabled;
            }
            windows.insert(Retained::as_ptr(&dialog.window) as usize, source.target);
            let visible = source.visible && !hidden;
            let newly = visible && !dialog.window.isVisible();
            if visible {
                dialog.window.orderFront(None);
            } else {
                dialog.window.orderOut(None);
            }
            if visible && (newly || dialog.source.focus != source.focus) {
                dialog.window.makeKeyWindow();
                let focus = dialog
                    .controls
                    .get(&source.focus)
                    .map(|control| control.view.clone());
                if let Some(focus) = focus.filter(|view| !view.isHidden()) {
                    dialog.window.makeFirstResponder(Some(&focus));
                }
            }
            dialog.source = source;
        }
        *output.windows.borrow_mut() = windows;
        if refocus && self.dialogs.is_empty() && self.main_enabled() && !hidden {
            main.makeKeyAndOrderFront(None);
        }
        output.syncing.set(false);
    }

    pub(super) fn hide(&mut self) {
        for dialog in self.dialogs.values() {
            dialog.window.orderOut(None);
        }
    }

    pub(super) fn clear(&mut self, hub: &Hub) {
        for dialog in std::mem::take(&mut self.dialogs).into_values() {
            retire(&hub.dialogs, &dialog);
        }
        self.owner_enabled = None;
    }
}

fn retire(output: &Output, dialog: &HostDialog) {
    let mut bindings = output.bindings.borrow_mut();
    for control in dialog.controls.values() {
        bindings.remove(&(Retained::as_ptr(&control.view) as usize));
    }
    if let Some(parent) = dialog.window.parentWindow() {
        parent.removeChildWindow(&dialog.window);
    }
    dialog.window.setDelegate(None);
    dialog.window.orderOut(None);
}

impl Projection {
    pub(crate) fn set_dialog_event_sink(&mut self, sink: DialogSink) {
        *self.hub.dialogs.sink.borrow_mut() = Some(sink);
    }
    pub(crate) fn set_dialogs(&mut self, set: &DialogSet) -> Result<(), String> {
        // Dialogs are children of the window that shows the game.
        let Some(main) = self.view.window() else {
            return Ok(());
        };
        self.dialogs.borrow_mut().update(
            self.mtm,
            &self.target,
            &self.hub,
            &main,
            set,
            self.hidden,
        );
        self.hub
            .main_enabled
            .set(self.dialogs.borrow().main_enabled());
        Ok(())
    }
}
