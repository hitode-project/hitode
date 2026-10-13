//! The macOS host window: one AppKit window on the main thread, driven by the
//! same pump/wait loop as the Win32 one. Input becomes the Win32 messages the
//! guest expects; menus and dialogs project the guest's presentation data.
//!
//! The canvas is sized in points, so a guest pixel is one point and Retina
//! displays scale it by whole device pixels. A resized or full-screen window
//! letterboxes the canvas with nearest sampling, like the Win32 presenter.

use super::*;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSControl,
    NSControlTextEditingDelegate, NSCursor, NSEvent, NSEventMask, NSEventModifierFlags,
    NSEventType, NSMenu, NSMenuDelegate, NSMenuItem, NSTextFieldDelegate, NSWindow,
    NSWindowCollectionBehavior, NSWindowDelegate, NSWindowStyleMask,
};
use objc2_core_foundation::{
    CFFileDescriptor, CFFileDescriptorContext, CFOptionFlags, CFRetained, CFRunLoop,
    CFRunLoopSource, kCFRunLoopCommonModes,
};
use objc2_foundation::{
    NSDate, NSDefaultRunLoopMode, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect,
    NSSize, NSString, NSTimer,
};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, VecDeque};
use std::rc::Rc;

mod dialog;
mod keys;
mod menu;

/// Subtype of the application-defined event that ends a wait on a wake.
const WAKE: i16 = 0x4144;

pub fn initialize_dpi() -> Result<(), String> {
    Ok(())
}

/// The aspect-preserving placement of the canvas in a client area, and the
/// inverse mapping of client points to canvas pixels (the Win32 `Viewport`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Viewport {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}
impl Viewport {
    fn fit(client: (f64, f64), width: u32, height: u32) -> Self {
        let (cw, ch) = (client.0.max(0.0), client.1.max(0.0));
        if cw == 0.0 || ch == 0.0 || width == 0 || height == 0 {
            return Self {
                x: 0.0,
                y: 0.0,
                width: 0.0,
                height: 0.0,
            };
        }
        let (w, h) = (f64::from(width), f64::from(height));
        let (fw, fh) = if cw * h <= ch * w {
            (cw, (cw * h / w).floor().max(1.0))
        } else {
            ((ch * w / h).floor().max(1.0), ch)
        };
        Self {
            x: ((cw - fw) / 2.0).floor(),
            y: ((ch - fh) / 2.0).floor(),
            width: fw,
            height: fh,
        }
    }
    /// Signed canvas pixels for a top-left client point; positions outside
    /// the canvas stay outside, as during a captured drag.
    fn position(self, x: f64, y: f64, width: u32, height: u32) -> (i32, i32) {
        (
            ((x - self.x) * f64::from(width) / self.width.max(1.0)).floor() as i32,
            ((y - self.y) * f64::from(height) / self.height.max(1.0)).floor() as i32,
        )
    }
}

pub(crate) fn native_viewport(
    width: u32,
    height: u32,
    canvas_width: u32,
    canvas_height: u32,
) -> [f32; 4] {
    let viewport = Viewport::fit(
        (f64::from(width), f64::from(height)),
        canvas_width,
        canvas_height,
    );
    [
        viewport.x as f32,
        viewport.y as f32,
        viewport.width as f32,
        viewport.height as f32,
    ]
}

/// UI state the Objective-C callbacks reach. Callbacks run inside AppKit
/// dispatch on the main thread, so no borrow is held across `sendEvent`.
#[derive(Default)]
pub(super) struct Hub {
    main: Cell<usize>,
    events: RefCell<VecDeque<Event>>,
    closed: Cell<bool>,
    focus_check: Cell<bool>,
    menus: RefCell<menu::Runtime>,
    menu_sink: RefCell<Option<MenuSink>>,
    dialogs: dialog::Output,
}
impl Hub {
    fn push(&self, event: Event) {
        self.events.borrow_mut().push_back(event);
    }
    fn menu_event(&self, event: adv_runtime::MenuEvent) {
        let sink = self.menu_sink.borrow_mut().take();
        match sink {
            Some(mut sink) => {
                if let Err(error) = sink(event) {
                    self.dialogs.fail(error);
                }
                self.menu_sink.borrow_mut().get_or_insert(sink);
            }
            None => self.push(Event::Menu(event)),
        }
    }
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements and `Target` has no Drop.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = Rc<Hub>]
    pub(super) struct Target;

    unsafe impl NSObjectProtocol for Target {}

    unsafe impl NSWindowDelegate for Target {
        #[unsafe(method(windowShouldClose:))]
        fn window_should_close(&self, sender: &NSWindow) -> bool {
            let hub = self.ivars();
            if sender as *const NSWindow as usize == hub.main.get() {
                if !hub.closed.replace(true) {
                    hub.push(Event::Close);
                }
                sender.orderOut(None);
            } else {
                // Closing is a guest request; only a later snapshot retires it.
                hub.dialogs.close(sender);
            }
            false
        }
        #[unsafe(method(windowDidResignKey:))]
        fn window_did_resign_key(&self, notification: &NSNotification) {
            let hub = self.ivars();
            if notification_window(notification) == hub.main.get() {
                hub.focus_check.set(true);
            }
        }
        #[unsafe(method(windowDidResize:))]
        fn window_did_resize(&self, notification: &NSNotification) {
            let hub = self.ivars();
            if notification_window(notification) == hub.main.get() {
                hub.push(Event::Repaint);
            }
        }
        #[unsafe(method(windowDidEnterFullScreen:))]
        fn window_did_enter_full_screen(&self, notification: &NSNotification) {
            let hub = self.ivars();
            if notification_window(notification) == hub.main.get() {
                hub.push(Event::Repaint);
            }
        }
        #[unsafe(method(windowDidExitFullScreen:))]
        fn window_did_exit_full_screen(&self, notification: &NSNotification) {
            let hub = self.ivars();
            if notification_window(notification) == hub.main.get() {
                hub.push(Event::Repaint);
            }
        }
        #[unsafe(method(windowDidChangeBackingProperties:))]
        fn window_did_change_backing(&self, notification: &NSNotification) {
            let hub = self.ivars();
            if notification_window(notification) == hub.main.get() {
                hub.push(Event::Repaint);
            }
        }
    }

    unsafe impl NSMenuDelegate for Target {
        #[unsafe(method(menuWillOpen:))]
        fn menu_will_open(&self, menu: &NSMenu) {
            let event = self.ivars().menus.borrow_mut().opened(menu);
            for event in event.into_iter().flatten() {
                self.ivars().menu_event(event);
            }
        }
        #[unsafe(method(menuDidClose:))]
        fn menu_did_close(&self, menu: &NSMenu) {
            self.ivars().menus.borrow_mut().closed(menu);
        }
    }

    unsafe impl NSControlTextEditingDelegate for Target {}

    unsafe impl NSTextFieldDelegate for Target {}

    impl Target {
        #[unsafe(method(guestMenuCommand:))]
        fn guest_menu_command(&self, item: &NSMenuItem) {
            let event = self.ivars().menus.borrow_mut().command(item.tag());
            if let Some(event) = event {
                self.ivars().menu_event(event);
            }
        }
        #[unsafe(method(hostQuit:))]
        fn host_quit(&self, _item: &NSMenuItem) {
            let hub = self.ivars();
            if !hub.closed.replace(true) {
                hub.push(Event::Close);
            }
        }
        #[unsafe(method(popupPoll:))]
        fn popup_poll(&self, _timer: &NSTimer) {
            self.ivars().menus.borrow().poll();
        }
        #[unsafe(method(dialogAction:))]
        fn dialog_action(&self, sender: &NSControl) {
            self.ivars().dialogs.action(sender);
        }
        #[unsafe(method(controlTextDidChange:))]
        fn control_text_did_change(&self, notification: &NSNotification) {
            if let Some(object) = notification.object() {
                self.ivars().dialogs.text(&object);
            }
        }
    }
);

impl Target {
    fn new(hub: Rc<Hub>, mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(hub);
        // SAFETY: NSObject's init has this signature.
        unsafe { msg_send![super(this), init] }
    }
}

fn notification_window(notification: &NSNotification) -> usize {
    notification
        .object()
        .map_or(0, |object| Retained::as_ptr(&object) as usize)
}

/// Wakes `nextEventMatchingMask` when the wake pipe becomes readable.
struct WakeSource {
    fd: i32,
    descriptor: CFRetained<CFFileDescriptor>,
    _source: CFRetained<CFRunLoopSource>,
}

unsafe extern "C-unwind" fn wake_callback(
    _: *mut CFFileDescriptor,
    _: CFOptionFlags,
    _: *mut std::ffi::c_void,
) {
    // SAFETY: the source is only in the main run loop.
    let mtm = unsafe { MainThreadMarker::new_unchecked() };
    if let Some(event) = wake_event() {
        NSApplication::sharedApplication(mtm).postEvent_atStart(&event, false);
    }
}

fn wake_event() -> Option<Retained<NSEvent>> {
    NSEvent::otherEventWithType_location_modifierFlags_timestamp_windowNumber_context_subtype_data1_data2(
        NSEventType::ApplicationDefined,
        NSPoint::new(0.0, 0.0),
        NSEventModifierFlags::empty(),
        0.0,
        0,
        None,
        WAKE,
        0,
        0,
    )
}

fn is_wake(event: &NSEvent) -> bool {
    event.r#type() == NSEventType::ApplicationDefined
        && event.subtype() == objc2_app_kit::NSEventSubtype(WAKE)
}

pub struct Window {
    mtm: MainThreadMarker,
    app: Retained<NSApplication>,
    window: Retained<NSWindow>,
    target: Retained<Target>,
    hub: Rc<Hub>,
    width: u32,
    height: u32,
    frame_size: Option<(u32, u32)>,
    hidden: bool,
    cursor_visible: bool,
    cursor_hidden: bool,
    regions: Arc<[adv_runtime::VideoCursorRegion]>,
    wheel: f64,
    modifiers: usize,
    wake: Option<WakeSource>,
    menus: menu::Menus,
    dialogs: dialog::Dialogs,
}

/// Device-dependent modifier bits (`NX_DEVICE*KEYMASK`) and Caps Lock.
const LEFT_CONTROL: usize = 0x1;
const LEFT_SHIFT: usize = 0x2;
const RIGHT_SHIFT: usize = 0x4;
const LEFT_ALT: usize = 0x20;
const RIGHT_ALT: usize = 0x40;
const RIGHT_CONTROL: usize = 0x2000;
const CAPS_LOCK: usize = 0x1_0000;

impl Window {
    pub fn new(title: &str, width: u32, height: u32, hidden: bool) -> Result<Self, String> {
        let mtm = MainThreadMarker::new().ok_or("the macOS window must run on the main thread")?;
        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
        // Harmless if repeated for a later window.
        app.finishLaunching();
        let hub = Rc::new(Hub::default());
        let target = Target::new(hub.clone(), mtm);
        // SAFETY: the window is retained by `Window` and not released on close.
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                NSRect::new(
                    NSPoint::new(0.0, 0.0),
                    NSSize::new(f64::from(width.max(1)), f64::from(height.max(1))),
                ),
                NSWindowStyleMask::Titled
                    | NSWindowStyleMask::Closable
                    | NSWindowStyleMask::Miniaturizable
                    | NSWindowStyleMask::Resizable,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        unsafe { window.setReleasedWhenClosed(false) };
        window.setTitle(&NSString::from_str(title));
        window.setAcceptsMouseMovedEvents(true);
        window.setCollectionBehavior(NSWindowCollectionBehavior::FullScreenPrimary);
        window.setDelegate(Some(ProtocolObject::from_ref(&*target)));
        hub.main.set(Retained::as_ptr(&window) as usize);
        window.center();
        let menus = menu::Menus::new(mtm, &app, &target)?;
        let this = Self {
            mtm,
            app,
            window,
            target,
            hub,
            width,
            height,
            frame_size: None,
            hidden,
            cursor_visible: true,
            cursor_hidden: false,
            regions: Arc::from([]),
            wheel: 0.0,
            modifiers: 0,
            wake: None,
            menus,
            dialogs: dialog::Dialogs::default(),
        };
        if !hidden {
            this.window.makeKeyAndOrderFront(None);
            #[allow(deprecated)]
            this.app.activateIgnoringOtherApps(true);
        }
        Ok(this)
    }

    fn client(&self) -> (f64, f64) {
        self.window.contentView().map_or((0.0, 0.0), |view| {
            let size = view.bounds().size;
            (size.width, size.height)
        })
    }

    fn set_canvas_size(&mut self, width: u32, height: u32) {
        let full = self
            .window
            .styleMask()
            .contains(NSWindowStyleMask::FullScreen);
        if !full {
            self.window
                .setContentSize(NSSize::new(f64::from(width), f64::from(height)));
        }
    }

    pub fn next_refresh(&self) -> std::time::Instant {
        let fps = self
            .window
            .screen()
            .map_or(60, |screen| screen.maximumFramesPerSecond())
            .clamp(24, 240);
        std::time::Instant::now() + Duration::from_secs_f64(1.0 / fps as f64)
    }

    pub fn pump(&mut self) -> Vec<Event> {
        loop {
            let event = self.app.nextEventMatchingMask_untilDate_inMode_dequeue(
                NSEventMask::Any,
                Some(&NSDate::distantPast()),
                unsafe { NSDefaultRunLoopMode },
                true,
            );
            let Some(event) = event else {
                break;
            };
            if is_wake(&event) {
                continue;
            }
            if self.dialogs.intercept(self.mtm, &self.hub, &event) {
                continue;
            }
            if self.handle(&event) {
                self.app.sendEvent(&event);
            }
        }
        self.app.updateWindows();
        if self.hub.focus_check.replace(false) {
            let key = self.app.keyWindow();
            let ours = key.as_ref().is_some_and(|key| {
                Retained::as_ptr(key) as usize == self.hub.main.get() || self.dialogs.owns(key)
            });
            if !ours {
                self.hub.push(Event::FocusLost);
            }
        }
        self.dialogs.poll_focus(&self.hub);
        let mut events: Vec<_> = self.hub.events.borrow_mut().drain(..).collect();
        events.extend(self.hub.dialogs.take_events().map(Event::Dialog));
        events
    }

    /// Translates input for the canvas; returns whether AppKit still gets it.
    fn handle(&mut self, event: &NSEvent) -> bool {
        let main = self.hub.main.get();
        let in_main = event
            .window(self.mtm)
            .is_some_and(|window| Retained::as_ptr(&window) as usize == main);
        let kind = event.r#type();
        match kind {
            NSEventType::KeyDown | NSEventType::KeyUp => {
                let flags = event.modifierFlags();
                if !in_main || flags.contains(NSEventModifierFlags::Command) {
                    return true;
                }
                if self.dialogs.main_enabled()
                    && let Some(key) = keys::key(event.keyCode())
                {
                    let alt = flags.contains(NSEventModifierFlags::Option)
                        && !flags.contains(NSEventModifierFlags::Control);
                    let down = kind == NSEventType::KeyDown;
                    let (message, wparam, lparam) =
                        keys::message(key, down, down && event.isARepeat(), alt);
                    self.hub.push(Event::Input {
                        message,
                        wparam,
                        lparam,
                    });
                }
                // Unhandled keys would only beep.
                false
            }
            NSEventType::FlagsChanged => {
                let flags = event.modifierFlags().0;
                let changed = flags ^ self.modifiers;
                self.modifiers = flags;
                if in_main && self.dialogs.main_enabled() {
                    self.modifier(event.keyCode(), flags, changed);
                }
                true
            }
            NSEventType::LeftMouseDown
            | NSEventType::LeftMouseUp
            | NSEventType::LeftMouseDragged
            | NSEventType::RightMouseDown
            | NSEventType::RightMouseUp
            | NSEventType::RightMouseDragged
            | NSEventType::OtherMouseDown
            | NSEventType::OtherMouseUp
            | NSEventType::OtherMouseDragged
            | NSEventType::MouseMoved
            | NSEventType::ScrollWheel => {
                if in_main {
                    self.mouse(event, kind);
                }
                true
            }
            _ => true,
        }
    }

    fn modifier(&mut self, code: u16, flags: usize, changed: usize) {
        let bit = match code {
            0x38 => LEFT_SHIFT,
            0x3c => RIGHT_SHIFT,
            0x3b => LEFT_CONTROL,
            0x3e => RIGHT_CONTROL,
            0x3a => LEFT_ALT,
            0x3d => RIGHT_ALT,
            0x39 => CAPS_LOCK,
            _ => return,
        };
        let Some(key) = keys::key(code) else {
            return;
        };
        if changed & bit == 0 {
            return;
        }
        let alt = |flags: usize| {
            flags & (LEFT_ALT | RIGHT_ALT) != 0 && flags & (LEFT_CONTROL | RIGHT_CONTROL) == 0
        };
        // Each Caps Lock press toggles the light; it is still one key press.
        let presses: &[bool] = if bit == CAPS_LOCK {
            &[true, false]
        } else if flags & bit != 0 {
            &[true]
        } else {
            &[false]
        };
        for &down in presses {
            let (message, wparam, lparam) = keys::message(key, down, false, alt(flags));
            self.hub.push(Event::Input {
                message,
                wparam,
                lparam,
            });
        }
    }

    /// Top-left client points of an event in the main window.
    fn point(&self, event: &NSEvent) -> (f64, f64) {
        let location = event.locationInWindow();
        let (_, ch) = self.client();
        (location.x, ch - location.y)
    }

    fn mouse(&mut self, event: &NSEvent, kind: NSEventType) {
        let (x, y) = self.point(event);
        let (cw, ch) = self.client();
        self.update_cursor(Some((x, y)));
        if !self.dialogs.main_enabled() {
            return;
        }
        let inside = x >= 0.0 && y >= 0.0 && x < cw && y < ch;
        let viewport = Viewport::fit((cw, ch), self.width, self.height);
        let (gx, gy) = viewport.position(x, y, self.width, self.height);
        let on_canvas = gx >= 0 && gy >= 0 && gx < self.width as i32 && gy < self.height as i32;
        let pressed = NSEvent::pressedMouseButtons();
        let flags = event.modifierFlags();
        let mut buttons = u32::from(pressed & 1 != 0)
            | u32::from(pressed & 2 != 0) << 1
            | u32::from(pressed & 4 != 0) << 4;
        if flags.contains(NSEventModifierFlags::Shift) {
            buttons |= 4;
        }
        if flags.contains(NSEventModifierFlags::Control) {
            buttons |= 8;
        }
        let other = event.buttonNumber() == 2;
        let message = match kind {
            NSEventType::LeftMouseDown => 0x201,
            NSEventType::LeftMouseUp => 0x202,
            NSEventType::RightMouseDown => 0x204,
            NSEventType::RightMouseUp => 0x205,
            NSEventType::OtherMouseDown if other => 0x207,
            NSEventType::OtherMouseUp if other => 0x208,
            NSEventType::ScrollWheel => 0x20a,
            NSEventType::OtherMouseDown | NSEventType::OtherMouseUp => return,
            _ => 0x200,
        };
        // Presses and the wheel need the pointer over the canvas; moves and
        // releases keep going while a drag holds the capture.
        if matches!(message, 0x201 | 0x204 | 0x207 | 0x20a) && !(inside && on_canvas) {
            return;
        }
        if message == 0x200 && !inside && buttons & 0x13 == 0 {
            return;
        }
        let wparam = if message == 0x20a {
            let delta = if event.hasPreciseScrollingDeltas() {
                event.scrollingDeltaY() * 4.0
            } else {
                event.scrollingDeltaY() * 120.0
            };
            self.wheel += delta;
            let notches = (self.wheel / 120.0).trunc();
            if notches == 0.0 {
                return;
            }
            self.wheel -= notches * 120.0;
            let delta = (notches * 120.0).clamp(-32768.0, 32767.0) as i16;
            u32::from(delta as u16) << 16 | (buttons & 0x1f)
        } else {
            buttons
        };
        let position = u32::from(gx as u16) | (u32::from(gy as u16) << 16);
        let (message, wparam) = adv_runtime::mouse_mapping(message, wparam, false);
        self.hub.push(Event::Input {
            message,
            wparam,
            lparam: position,
        });
    }

    fn cursor_visible_at(&self, point: (f64, f64)) -> bool {
        let viewport = Viewport::fit(self.client(), self.width, self.height);
        let (x, y) = viewport.position(point.0, point.1, self.width, self.height);
        self.cursor_visible && !adv_runtime::VideoCursorRegion::hidden_at(&self.regions, x, y)
    }

    /// Hides the cursor only over the client area; AppKit counts hides, so
    /// this keeps exactly one outstanding.
    fn update_cursor(&mut self, point: Option<(f64, f64)>) {
        let point = point.unwrap_or_else(|| {
            let location = self.window.mouseLocationOutsideOfEventStream();
            (location.x, self.client().1 - location.y)
        });
        let (cw, ch) = self.client();
        let inside = point.0 >= 0.0 && point.1 >= 0.0 && point.0 < cw && point.1 < ch;
        let hide = inside && self.window.isKeyWindow() && !self.cursor_visible_at(point);
        if hide != self.cursor_hidden {
            self.cursor_hidden = hide;
            if hide {
                NSCursor::hide();
            } else {
                NSCursor::unhide();
            }
        }
    }

    pub fn set_cursor_visible(&mut self, visible: bool) {
        if self.cursor_visible != visible {
            self.cursor_visible = visible;
            self.update_cursor(None);
        }
    }

    pub fn set_video_cursor_regions(&mut self, regions: Arc<[adv_runtime::VideoCursorRegion]>) {
        self.regions = regions;
        self.update_cursor(None);
    }

    /// Mouse lParams are client points, mapped like physical input.
    pub fn post_input(&mut self, message: u32, wparam: u32, lparam: u32) -> Result<(), String> {
        let lparam = if (0x200..=0x20a).contains(&message) {
            let (x, y) = (
                f64::from(lparam as u16 as i16),
                f64::from((lparam >> 16) as u16 as i16),
            );
            let viewport = Viewport::fit(self.client(), self.width, self.height);
            let (x, y) = viewport.position(x, y, self.width, self.height);
            u32::from(x as u16) | (u32::from(y as u16) << 16)
        } else {
            lparam
        };
        self.hub.push(Event::Input {
            message,
            wparam,
            lparam,
        });
        Ok(())
    }

    /// Create the Metal surface on the UI thread before guest submissions begin.
    pub fn create_native(
        &mut self,
        fallback: bool,
    ) -> Result<(adv_device::gpu::Gpu, crate::native_gpu::WindowPresenter), String> {
        let view = self
            .window
            .contentView()
            .ok_or("window without a content view")?;
        let (width, height) = self.native_client_size()?;
        // SAFETY: the window retains NSView until the background service joins
        // and the presenter drops. Surface creation stays on the main thread.
        let (gpu, mut presenter) = pollster::block_on(unsafe {
            crate::native_gpu::WindowPresenter::request(
                Retained::as_ptr(&view) as *mut std::ffi::c_void,
                width.max(1),
                height.max(1),
                fallback,
            )
        })?;
        presenter.configure_exclusive(width.max(1), height.max(1))?;
        Ok((gpu, presenter))
    }

    pub fn native_client_size(&self) -> Result<(u32, u32), String> {
        let (width, height) = self.client();
        let scale = self.window.backingScaleFactor();
        Ok((
            (width * scale).round() as u32,
            (height * scale).round() as u32,
        ))
    }

    pub fn native_canvas(&mut self, canvas: (u32, u32)) -> Result<bool, String> {
        if canvas.0 == 0 || canvas.1 == 0 {
            return Ok(false);
        }
        self.width = canvas.0;
        self.height = canvas.1;
        let changed = self.frame_size != Some(canvas);
        if changed {
            self.set_canvas_size(canvas.0, canvas.1);
            self.frame_size = Some(canvas);
        }
        Ok(changed)
    }

    #[allow(dead_code)]
    pub fn wait(&mut self, duration: Option<Duration>) -> Result<WaitOutcome, String> {
        self.wait_with_wake(duration, None)
    }

    pub fn hide(&mut self) {
        self.hidden = true;
        self.dialogs.hide();
        self.window.orderOut(None);
    }

    fn watch(&mut self, wake: &WakeEvent) -> Result<(), String> {
        let fd = wake.as_raw_fd();
        if self.wake.as_ref().is_none_or(|source| source.fd != fd) {
            self.wake = None;
            let context = CFFileDescriptorContext {
                version: 0,
                info: std::ptr::null_mut(),
                retain: None,
                release: None,
                copyDescription: None,
            };
            // SAFETY: the descriptor outlives the source; the callback keeps no state.
            let descriptor =
                unsafe { CFFileDescriptor::new(None, fd, false, Some(wake_callback), &context) }
                    .ok_or("CFFileDescriptorCreate failed")?;
            let source = CFFileDescriptor::new_run_loop_source(None, Some(&descriptor), 0)
                .ok_or("CFFileDescriptorCreateRunLoopSource failed")?;
            CFRunLoop::main()
                .ok_or("no main run loop")?
                .add_source(Some(&source), unsafe { kCFRunLoopCommonModes });
            self.wake = Some(WakeSource {
                fd,
                descriptor,
                _source: source,
            });
        }
        // One-shot: re-arm for this wait.
        self.wake
            .as_ref()
            .unwrap()
            .descriptor
            .enable_call_backs(objc2_core_foundation::kCFFileDescriptorReadCallBack);
        Ok(())
    }

    pub fn wait_with_wake(
        &mut self,
        duration: Option<Duration>,
        wake: Option<&WakeEvent>,
    ) -> Result<WaitOutcome, String> {
        if let Some(error) = self.hub.dialogs.take_error() {
            return Err(error);
        }
        // A wake drained here has been seen; its pipe byte is consumed.
        if let Some(wake) = wake {
            if wake.wait(Some(Duration::ZERO)).map_err(|e| e.to_string())? {
                return Ok(WaitOutcome::Input);
            }
            self.watch(wake)?;
        }
        let until = match duration {
            Some(duration) if duration.is_zero() => NSDate::distantPast(),
            Some(duration) => NSDate::dateWithTimeIntervalSinceNow(duration.as_secs_f64()),
            None => NSDate::distantFuture(),
        };
        let event = self.app.nextEventMatchingMask_untilDate_inMode_dequeue(
            NSEventMask::Any,
            Some(&until),
            unsafe { NSDefaultRunLoopMode },
            false,
        );
        match event {
            None => Ok(WaitOutcome::Deadline),
            Some(event) => {
                if is_wake(&event)
                    && let Some(wake) = wake
                {
                    wake.wait(Some(Duration::ZERO)).map_err(|e| e.to_string())?;
                }
                Ok(WaitOutcome::Input)
            }
        }
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        self.dialogs.clear(&self.hub);
        if self.cursor_hidden {
            NSCursor::unhide();
        }
        self.menus.clear(&self.app);
        self.window.setDelegate(None);
        self.window.orderOut(None);
        if let Some(wake) = &self.wake {
            wake.descriptor.invalidate();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn viewport_letterboxes_and_maps_back_to_canvas_pixels() {
        let viewport = Viewport::fit((800.0, 800.0), 800, 600);
        assert_eq!(
            viewport,
            Viewport {
                x: 0.0,
                y: 100.0,
                width: 800.0,
                height: 600.0
            }
        );
        assert_eq!(viewport.position(400.0, 400.0, 800, 600), (400, 300));
        // Outside the canvas stays signed, as during a captured drag.
        assert_eq!(viewport.position(400.0, 99.0, 800, 600), (400, -1));
        assert_eq!(viewport.position(400.0, 50.0, 800, 600), (400, -50));
        // A doubled window maps two points to one pixel.
        let double = Viewport::fit((1280.0, 960.0), 640, 480);
        assert_eq!(double.position(1279.0, 959.0, 640, 480), (639, 479));
        assert_eq!(
            Viewport::fit((0.0, 10.0), 640, 480),
            Viewport {
                x: 0.0,
                y: 0.0,
                width: 0.0,
                height: 0.0
            }
        );
    }
}
