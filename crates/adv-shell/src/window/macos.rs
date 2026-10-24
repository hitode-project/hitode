//! The macOS CLI window, driven by the AppKit run loop. Input becomes the Win32 messages the
//! guest expects; menus and dialogs project the guest's presentation data.
//!
//! The canvas is sized in points, so a guest pixel is one point and Retina
//! displays scale it by whole device pixels. A resized or full-screen window
//! letterboxes the canvas with nearest sampling, like the Win32 presenter.

use super::*;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate,
    NSApplicationTerminateReply, NSBackingStoreType, NSControl, NSControlTextEditingDelegate,
    NSCursor, NSEvent, NSEventMask, NSEventModifierFlags, NSEventType, NSMenu, NSMenuDelegate,
    NSMenuItem, NSTableColumn, NSTableView, NSTableViewDataSource, NSTableViewDelegate,
    NSTextFieldDelegate, NSWindow, NSWindowCollectionBehavior, NSWindowDelegate,
    NSWindowOcclusionState, NSWindowStyleMask, NSWorkspace, NSWorkspaceDidWakeNotification,
    NSWorkspaceWillPowerOffNotification, NSWorkspaceWillSleepNotification,
};
use objc2_foundation::{
    NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString, NSTimer,
};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, VecDeque};
use std::rc::Rc;

mod container;
pub use container::GameContainer;
mod dialog;
mod game_view;
pub use game_view::GameView;
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
    fn scaled(
        client: (f64, f64),
        canvas: (u32, u32),
        scaling: crate::Scaling,
        backing: f64,
    ) -> Self {
        let pixels = ((client.0 * backing).round(), (client.1 * backing).round());
        let rect = if scaling == crate::Scaling::Fit || canvas.0 == 0 || canvas.1 == 0 {
            Self::fit(pixels, canvas.0, canvas.1)
        } else {
            let [x, y, width, height] = scaling.rectangle(pixels, canvas);
            Self {
                x,
                y,
                width,
                height,
            }
        };
        Self {
            x: rect.x / backing,
            y: rect.y / backing,
            width: rect.width / backing,
            height: rect.height / backing,
        }
    }

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
pub(crate) struct Hub {
    pub(crate) scaling: Cell<crate::Scaling>,
    main: Cell<usize>,
    wake: RefCell<Option<WakeEvent>>,
    main_enabled: Cell<bool>,
    events: RefCell<VecDeque<Event>>,
    close_requested: Cell<bool>,
    terminating: Cell<bool>,
    occluded: Cell<bool>,
    sleeping: Cell<bool>,
    canvas: Cell<(u32, u32)>,
    focus_check: Cell<bool>,
    menus: RefCell<menu::Runtime>,
    menu_sink: RefCell<Option<MenuSink>>,
    dialogs: dialog::Output,
    /// The view's display link (macOS 14 and later), paused between requests.
    link: RefCell<Option<Retained<objc2_quartz_core::CADisplayLink>>>,
    on_frame: RefCell<Option<Rc<dyn Fn()>>>,
    in_frame: Cell<bool>,
    pub(crate) vsync: Arc<adv_sync::vsync::Vsync>,
}
impl Hub {
    pub(crate) fn push(&self, event: Event) {
        self.events.borrow_mut().push_back(event);
        if let Some(wake) = self.wake.borrow().as_ref() {
            wake.signal();
        }
    }
    pub(crate) fn activate(&self, active: bool) {
        self.push(Event::Input {
            message: 0x1c,
            wparam: u32::from(active),
            lparam: 0,
        });
        if !active {
            self.push(Event::FocusLost);
        }
    }
    pub(crate) fn minimized(&self, minimized: bool) {
        let (width, height) = self.canvas.get();
        self.push(Event::Input {
            message: 5,
            wparam: u32::from(minimized),
            lparam: if minimized {
                0
            } else {
                width.min(65535) | height.min(65535) << 16
            },
        });
        self.occluded.set(minimized);
        self.push(Event::Repaint);
    }
    /// The game's window lost key status; focus is lost unless a guest
    /// dialog took it, which the next pump checks.
    pub(crate) fn resign_key(&self) {
        self.focus_check.set(true);
        if let Some(wake) = self.wake.borrow().as_ref() {
            wake.signal();
        }
    }
    pub(crate) fn set_occluded(&self, occluded: bool) {
        self.occluded.set(occluded);
        self.push(Event::Repaint);
    }
    pub(crate) fn sleep(&self, sleeping: bool) {
        self.sleeping.set(sleeping);
        self.push(Event::AudioSuspended(sleeping));
        self.push(Event::Repaint);
    }
    pub(crate) fn pause(&self, paused: bool) {
        self.push(Event::Paused(paused));
    }
    pub(crate) fn request_close(&self) {
        self.close_requested.set(true);
        self.push(Event::Close);
    }
    pub(crate) fn force_close(&self) {
        self.push(Event::ForceClose);
    }
    pub(crate) fn power_off(&self) {
        self.push(Event::Input {
            message: 0x11,
            wparam: 0,
            lparam: 0,
        });
        self.push(Event::Input {
            message: 0x16,
            wparam: 1,
            lparam: 0,
        });
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
                hub.request_close();
            } else {
                // Closing is a guest request; only a later snapshot retires it.
                hub.dialogs.close(sender);
            }
            false
        }
        #[unsafe(method(windowDidMiniaturize:))]
        fn window_did_miniaturize(&self, note: &NSNotification) {
            if notification_window(note) == self.ivars().main.get() { self.ivars().minimized(true); }
        }
        #[unsafe(method(windowDidDeminiaturize:))]
        fn window_did_deminiaturize(&self, note: &NSNotification) {
            if notification_window(note) == self.ivars().main.get() { self.ivars().minimized(false); }
        }
        #[unsafe(method(windowDidChangeOcclusionState:))]
        fn window_did_change_occlusion(&self, note: &NSNotification) {
            if notification_window(note) == self.ivars().main.get() {
                // SAFETY: the notification was checked against our retained window.
                let window = unsafe { &*(self.ivars().main.get() as *const NSWindow) };
                self.ivars().set_occluded(!window.occlusionState().contains(NSWindowOcclusionState::Visible));
            }
        }
        #[unsafe(method(windowDidResignKey:))]
        fn window_did_resign_key(&self, notification: &NSNotification) {
            let hub = self.ivars();
            if notification_window(notification) == hub.main.get() {
                hub.resign_key();
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

    unsafe impl NSApplicationDelegate for Target {
        #[unsafe(method(applicationShouldTerminate:))]
        fn application_should_terminate(&self, _app: &NSApplication) -> NSApplicationTerminateReply {
            self.ivars().terminating.set(true);
            if !self.ivars().close_requested.replace(true) { self.ivars().push(Event::Close); }
            NSApplicationTerminateReply::TerminateLater
        }
        #[unsafe(method(applicationDidBecomeActive:))]
        fn application_did_become_active(&self, _note: &NSNotification) { self.ivars().activate(true); }
        #[unsafe(method(applicationDidResignActive:))]
        fn application_did_resign_active(&self, _note: &NSNotification) { self.ivars().activate(false); }
        #[unsafe(method(applicationDidHide:))]
        fn application_did_hide(&self, _note: &NSNotification) {
            self.ivars().activate(false);
            self.ivars().set_occluded(true);
        }
        #[unsafe(method(applicationDidUnhide:))]
        fn application_did_unhide(&self, _note: &NSNotification) {
            self.ivars().set_occluded(false);
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

    unsafe impl NSTableViewDataSource for Target {
        #[unsafe(method(numberOfRowsInTableView:))]
        fn number_of_rows(&self, table: &NSTableView) -> isize {
            self.ivars().dialogs.list_len(table) as isize
        }
        #[unsafe(method_id(tableView:objectValueForTableColumn:row:))]
        fn object_value(
            &self,
            table: &NSTableView,
            _column: Option<&NSTableColumn>,
            row: isize,
        ) -> Option<Retained<AnyObject>> {
            self.ivars()
                .dialogs
                .list_item(table, row)
                .map(|item| Retained::into_super(Retained::into_super(NSString::from_str(&item))))
        }
    }

    unsafe impl NSTableViewDelegate for Target {
        #[unsafe(method(tableViewSelectionDidChange:))]
        fn table_selection_did_change(&self, notification: &NSNotification) {
            if let Some(object) = notification.object() {
                self.ivars().dialogs.list_selected(&object);
            }
        }
    }

    impl Target {
        #[unsafe(method(hostWillPowerOff:))]
        fn host_will_power_off(&self, _note: &NSNotification) {
            // Workspace does not distinguish logout, restart and power-off.
            self.ivars().power_off();
        }
        #[unsafe(method(hostWillSleep:))]
        fn host_will_sleep(&self, _note: &NSNotification) {
            self.ivars().sleep(true);
        }
        #[unsafe(method(hostDidWake:))]
        fn host_did_wake(&self, _note: &NSNotification) {
            self.ivars().sleep(false);
        }
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
            if hub.close_requested.replace(true) {
                hub.force_close();
            } else {
                hub.push(Event::Close);
                _item.setTitle(&NSString::from_str("Force Quit (guest is closing)"));
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

pub(crate) fn wake_event() -> Option<Retained<NSEvent>> {
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

pub struct Window {
    app: Retained<NSApplication>,
    window: Retained<NSWindow>,
    target: Retained<Target>,
    hub: Rc<Hub>,
    frame_size: Option<(u32, u32)>,
    pub(crate) view: Retained<GameView>,
    pub(crate) projection: Rc<RefCell<Projection>>,
}

/// Guest menus and dialogs projected around a game view: menus into the
/// application's main menu after its first item, dialogs as child windows of
/// the view's window. The CLI window and an embedding host share it.
pub(crate) struct Projection {
    mtm: MainThreadMarker,
    app: Retained<NSApplication>,
    target: Retained<Target>,
    hub: Rc<Hub>,
    view: Retained<GameView>,
    hidden: bool,
    menus: menu::Menus,
    dialogs: Rc<RefCell<dialog::Dialogs>>,
    monitor: Retained<AnyObject>,
}
impl Projection {
    fn new(
        view: Retained<GameView>,
        target: Retained<Target>,
        menus: menu::Menus,
        hidden: bool,
    ) -> Result<Self, String> {
        let mtm = view.mtm();
        let hub = view.hub();
        let dialogs = Rc::new(RefCell::new(dialog::Dialogs::default()));
        let monitored = dialogs.clone();
        let input_hub = hub.clone();
        let block = block2::RcBlock::new(move |event: std::ptr::NonNull<NSEvent>| {
            let event_ref = unsafe { event.as_ref() };
            if let Some(wake) = input_hub.wake.borrow().as_ref() {
                wake.signal();
            }
            if monitored.borrow().intercept(mtm, &input_hub, event_ref) {
                std::ptr::null_mut()
            } else {
                event.as_ptr()
            }
        });
        let monitor = unsafe {
            NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::Any, &block)
        }
        .ok_or("could not install dialog event monitor")?;
        Ok(Self {
            mtm,
            app: NSApplication::sharedApplication(mtm),
            target,
            hub,
            view,
            hidden,
            menus,
            dialogs,
            monitor,
        })
    }

    /// Projects into the host's existing main menu and the view's window.
    pub(crate) fn embedded(view: Retained<GameView>) -> Result<Self, String> {
        let mtm = view.mtm();
        let target = Target::new(view.hub(), mtm);
        let menus = menu::Menus::attach(mtm, &NSApplication::sharedApplication(mtm));
        Self::new(view, target, menus, false)
    }

    /// Drain translated events; AppKit dispatch belongs to the host run loop.
    pub(crate) fn pump(&mut self) -> Vec<Event> {
        if self.hub.focus_check.replace(false) {
            let key = self.app.keyWindow();
            let ours = key.as_ref().is_some_and(|key| {
                Retained::as_ptr(key) as usize == self.hub.main.get()
                    || self.dialogs.borrow().owns(key)
            });
            if !ours {
                self.hub.push(Event::FocusLost);
            }
        }
        self.dialogs.borrow_mut().poll_focus(&self.hub);
        let mut events: Vec<_> = self.hub.events.borrow_mut().drain(..).collect();
        events.extend(self.hub.dialogs.take_events().map(Event::Dialog));
        events
    }

    pub(crate) fn hide(&mut self) {
        self.hidden = true;
        self.dialogs.borrow_mut().hide();
    }
}
impl Drop for Projection {
    fn drop(&mut self) {
        unsafe { NSEvent::removeMonitor(&self.monitor) };
        self.dialogs.borrow_mut().clear(&self.hub);
        self.menus.clear();
    }
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
        let view = GameView::new(width, height)?;
        let hub = view.hub();
        let target = Target::new(hub.clone(), mtm);
        hub.canvas.set((width, height));
        app.setDelegate(Some(ProtocolObject::from_ref(&*target)));
        let center = NSWorkspace::sharedWorkspace().notificationCenter();
        // SAFETY: selectors accept notifications; registration ends before target drops.
        unsafe {
            center.addObserver_selector_name_object(
                &target,
                sel!(hostWillPowerOff:),
                Some(NSWorkspaceWillPowerOffNotification),
                None,
            );
            center.addObserver_selector_name_object(
                &target,
                sel!(hostWillSleep:),
                Some(NSWorkspaceWillSleepNotification),
                None,
            );
            center.addObserver_selector_name_object(
                &target,
                sel!(hostDidWake:),
                Some(NSWorkspaceDidWakeNotification),
                None,
            );
        }
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
        window.setContentView(Some(&view));
        window.makeFirstResponder(Some(&view));
        window.setTitle(&NSString::from_str(title));
        window.setAcceptsMouseMovedEvents(true);
        window.setCollectionBehavior(NSWindowCollectionBehavior::FullScreenPrimary);
        window.setDelegate(Some(ProtocolObject::from_ref(&*target)));
        hub.main.set(Retained::as_ptr(&window) as usize);
        window.center();
        let menus = menu::Menus::new(mtm, &app, &target);
        let projection = Projection::new(view.clone(), target.clone(), menus, hidden)?;
        let this = Self {
            app,
            window,
            target,
            hub,
            frame_size: None,
            view,
            projection: Rc::new(RefCell::new(projection)),
        };
        if !hidden {
            this.window.makeKeyAndOrderFront(None);
            #[allow(deprecated)]
            this.app.activateIgnoringOtherApps(true);
        }
        Ok(this)
    }

    /// Reply only after the execution worker and background service have joined:
    /// guest files are closed and the registry snapshot has been persisted.
    pub fn finish_termination(&self) {
        if self.hub.terminating.replace(false) {
            self.app.replyToApplicationShouldTerminate(true);
        }
    }

    fn set_canvas_size(&mut self, width: u32, height: u32) {
        let full = self
            .window
            .styleMask()
            .contains(NSWindowStyleMask::FullScreen);
        if !full {
            let available = self.window.screen().map_or((f64::MAX, f64::MAX), |screen| {
                let size = self
                    .window
                    .contentRectForFrameRect(screen.visibleFrame())
                    .size;
                (size.width, size.height)
            });
            let (width, height) = fitted(
                (f64::from(width), f64::from(height)),
                available,
                self.window.backingScaleFactor(),
            );
            self.window.setContentSize(NSSize::new(width, height));
        }
    }

    pub fn pump(&mut self) -> Vec<Event> {
        self.projection.borrow_mut().pump()
    }
    pub fn set_menu_event_sink(&mut self, sink: MenuSink) {
        self.projection.borrow_mut().set_menu_event_sink(sink);
    }
    pub fn set_menu_bar(&mut self, bar: &adv_runtime::MenuBar) -> Result<(), String> {
        self.projection.borrow_mut().set_menu_bar(bar)
    }
    pub fn set_dialog_event_sink(&mut self, sink: DialogSink) {
        self.projection.borrow_mut().set_dialog_event_sink(sink);
    }
    pub fn set_dialogs(&mut self, set: &adv_runtime::DialogSet) -> Result<(), String> {
        self.projection.borrow_mut().set_dialogs(set)
    }

    pub fn set_cursor_visible(&mut self, visible: bool) {
        self.view.set_cursor_visible(visible);
    }
    pub fn set_video_cursor_regions(&mut self, regions: Arc<[adv_runtime::VideoCursorRegion]>) {
        self.view.set_video_cursor_regions(regions);
    }

    pub fn post_input(&mut self, message: u32, wparam: u32, lparam: u32) -> Result<(), String> {
        self.view.post_input(message, wparam, lparam)
    }
    pub fn create_native(
        &mut self,
        fallback: bool,
    ) -> Result<(adv_device::gpu::Gpu, crate::native_gpu::WindowPresenter), String> {
        self.view.create_native(fallback)
    }
    pub fn native_client_size(&self) -> Result<(u32, u32), String> {
        self.view.native_client_size()
    }

    pub fn native_canvas(&mut self, canvas: (u32, u32)) -> Result<bool, String> {
        if canvas.0 == 0 || canvas.1 == 0 {
            return Ok(false);
        }
        self.view.canvas(canvas.0, canvas.1);
        let changed = self.frame_size != Some(canvas);
        if changed {
            self.set_canvas_size(canvas.0, canvas.1);
            self.frame_size = Some(canvas);
        }
        Ok(changed)
    }

    pub fn hide(&mut self) {
        self.hub.occluded.set(true);
        self.projection.borrow_mut().hide();
        self.window.orderOut(None);
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        self.app.setDelegate(None);
        // SAFETY: this is the observer registered by this window.
        unsafe {
            NSWorkspace::sharedWorkspace()
                .notificationCenter()
                .removeObserver(&self.target);
        }
        self.window.setDelegate(None);
        self.window.orderOut(None);
    }
}

/// The content size, in points, that shows a `canvas` of game pixels: the
/// most whole device pixels per game pixel that fit in `available` points,
/// down to one, and below that the proportional size that fits.
fn fitted(canvas: (f64, f64), available: (f64, f64), scale: f64) -> (f64, f64) {
    let mut pixels = scale.floor().max(1.0);
    while pixels >= 1.0 {
        let size = (canvas.0 * pixels / scale, canvas.1 * pixels / scale);
        if size.0 <= available.0 && size.1 <= available.1 {
            return size;
        }
        pixels -= 1.0;
    }
    let shrink = (available.0 / canvas.0).min(available.1 / canvas.1);
    (canvas.0 * shrink, canvas.1 * shrink)
}

#[cfg(test)]
mod tests {
    use super::*;

    // spec: HOST-RUN-1
    #[test]
    fn the_cli_window_fits_whole_device_pixels_on_the_screen() {
        let screen = (1920.0, 1020.0);
        assert_eq!(fitted((800.0, 600.0), screen, 2.0), (800.0, 600.0));
        assert_eq!(fitted((1920.0, 1080.0), screen, 2.0), (960.0, 540.0));
        assert_eq!(
            fitted((1920.0, 1080.0), (2560.0, 1400.0), 1.0),
            (1920.0, 1080.0)
        );
        assert_eq!(
            fitted((3840.0, 2160.0), (960.0, 1000.0), 1.0),
            (960.0, 540.0)
        );
    }

    // spec: HOST-RUN-1
    #[test]
    fn retina_scaling_maps_the_presented_rectangle() {
        let integer = Viewport::scaled((600.0, 500.0), (800, 600), crate::Scaling::Integer, 2.0);
        assert_eq!(
            (integer.x, integer.y, integer.width, integer.height),
            (100.0, 100.0, 400.0, 300.0)
        );
        assert_eq!(integer.position(100.0, 100.0, 800, 600), (0, 0));
        assert_eq!(integer.position(499.5, 399.5, 800, 600), (799, 599));
        let stretch = Viewport::scaled((600.0, 500.0), (800, 600), crate::Scaling::Stretch, 2.0);
        assert_eq!(stretch.position(300.0, 250.0, 800, 600), (400, 300));
        assert_eq!(
            (stretch.x, stretch.y, stretch.width, stretch.height),
            (0.0, 0.0, 600.0, 500.0)
        );
        assert_eq!(
            crate::Scaling::Integer.rectangle((400.0, 300.0), (800, 600)),
            [0.0, 0.0, 400.0, 300.0]
        );
    }

    // spec: HOST-VIEW-1
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
