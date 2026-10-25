//! The mobile game view, on the host's main thread: an Android SurfaceView
//! supplied by the embedding host, or an iOS `CAMetalLayer` view this crate
//! creates for the host to insert.
use super::{Event, WakeEvent, refresh_interval};
#[cfg(target_os = "android")]
use std::ptr::NonNull;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::Arc,
    time::Instant,
};
#[cfg(target_os = "android")]
#[path = "android/input.rs"]
mod input;
#[cfg(target_os = "ios")]
#[path = "ios.rs"]
mod input;
#[cfg(target_os = "ios")]
pub use input::GameContainer;
pub use input::TouchAction;
#[cfg(target_os = "android")]
type SurfaceSink = Box<dyn Fn(Option<NonNull<ndk_sys::ANativeWindow>>) -> Result<(), String>>;
use crate::mobile::{Gesture, Gestures, TouchMode};

/// What the game view tells its host.
#[derive(Clone, Debug, PartialEq)]
pub enum ViewEvent {
    /// The screen layout changed (AND-APP-16).
    Layout(crate::mobile::Layout),
    /// A three-finger tap asked for the keyboard.
    Keyboard,
    /// Where the host draws the trackpad cursor, in view pixels; None hides
    /// it. iOS draws it in the game view itself.
    Cursor(Option<(f32, f32)>),
    /// The guest's dialogs changed; the overlay draws them and answers
    /// through `Session::dialog_event`, as the macOS projection does.
    Dialogs(adv_runtime::DialogSet),
    /// The guest's menu bar or tracked popup changed; the overlay lists it
    /// and answers through `Session::menu_event`.
    Menu(adv_runtime::MenuBar),
}

/// Trackpad cursor travel per finger travel.
const TRACKPAD_GAIN: f32 = 1.25;
pub struct GameView {
    #[cfg(target_os = "android")]
    window: Cell<Option<NonNull<ndk_sys::ANativeWindow>>>,
    #[cfg(target_os = "ios")]
    pub(super) host: RefCell<Option<objc2::rc::Retained<input::HostView>>>,
    /// The display-link vsync grid for guest pacing (PROC-CLOCK-4).
    #[cfg(target_os = "ios")]
    pub(crate) vsync: Arc<adv_sync::vsync::Vsync>,
    client: Cell<(u32, u32)>,
    canvas: Cell<(u32, u32)>,
    pub(crate) scaling: Cell<crate::Scaling>,
    events: RefCell<Vec<Event>>,
    wake: RefCell<Option<WakeEvent>>,
    #[cfg(target_os = "android")]
    surface_sink: RefCell<Option<SurfaceSink>>,
    error: RefCell<Option<String>>,
    gestures: RefCell<Gestures>,
    mode: Cell<TouchMode>,
    /// The mouse position in canvas pixels: the last touch in direct mode,
    /// the cursor in trackpad mode.
    cursor: Cell<(f32, f32)>,
    /// MK_ flags of buttons held through `mouse_button`.
    buttons: Cell<u32>,
    listener: RefCell<Option<Box<dyn Fn(ViewEvent)>>>,
    /// The dialogs last sent to the listener.
    dialogs: RefCell<adv_runtime::DialogSet>,
    menu: RefCell<adv_runtime::MenuBar>,
    /// Where the host placed the surface, as a layout whose game is that
    /// area, with the points-to-pixels scale (AND-APP-16).
    #[cfg(target_os = "android")]
    screen: RefCell<Option<(crate::mobile::Layout, f64)>>,
    #[cfg(target_os = "android")]
    reported: RefCell<Option<crate::mobile::Layout>>,
    created: Instant,
    paused: Cell<bool>,
    toggle_control: Cell<bool>,
    control: Cell<bool>,
    physical_keys: RefCell<std::collections::BTreeSet<i32>>,
    keys: RefCell<Arc<adv_sync::input::KeyState>>,
    pub(crate) requested_rate: Cell<u32>,
    pub(crate) frame_clock: Cell<bool>,
    pub(crate) period: Cell<i64>,
}
impl GameView {
    pub fn new(width: u32, height: u32) -> Result<Rc<Self>, String> {
        #[cfg(target_os = "android")]
        let main = unsafe { libc::gettid() } == unsafe { libc::getpid() };
        #[cfg(target_os = "ios")]
        let main = objc2::MainThreadMarker::new().is_some();
        if !main {
            return Err("the game view must run on the main thread".into());
        }
        let view = Rc::new(Self {
            #[cfg(target_os = "android")]
            window: Cell::new(None),
            #[cfg(target_os = "ios")]
            host: RefCell::new(None),
            #[cfg(target_os = "ios")]
            vsync: Arc::default(),
            client: Cell::new((width, height)),
            canvas: Cell::new((width.max(1), height.max(1))),
            scaling: Cell::new(crate::Scaling::Fit),
            events: RefCell::new(Vec::new()),
            wake: RefCell::new(None),
            #[cfg(target_os = "android")]
            surface_sink: RefCell::new(None),
            error: RefCell::new(None),
            gestures: RefCell::new(Gestures::default()),
            mode: Cell::new(TouchMode::Direct),
            cursor: Cell::new((0.0, 0.0)),
            buttons: Cell::new(0),
            listener: RefCell::new(None),
            dialogs: RefCell::default(),
            menu: RefCell::default(),
            #[cfg(target_os = "android")]
            screen: RefCell::new(None),
            #[cfg(target_os = "android")]
            reported: RefCell::new(None),
            created: Instant::now(),
            paused: Cell::new(false),
            toggle_control: Cell::new(false),
            control: Cell::new(false),
            physical_keys: RefCell::new(Default::default()),
            keys: RefCell::new(Arc::default()),
            requested_rate: Cell::new(60),
            frame_clock: Cell::new(false),
            period: Cell::new(16_666_667),
        });
        #[cfg(target_os = "ios")]
        {
            let host = input::HostView::new(Rc::downgrade(&view))?;
            *view.host.borrow_mut() = Some(host);
        }
        Ok(view)
    }
    /// The `UIView` for the host to insert below its webview; it stays valid
    /// while this game view lives.
    #[cfg(target_os = "ios")]
    pub fn ui_view(&self) -> *mut std::ffi::c_void {
        self.host
            .borrow()
            .as_ref()
            .map_or(std::ptr::null_mut(), |host| {
                objc2::rc::Retained::as_ptr(host).cast_mut().cast()
            })
    }
    #[cfg(target_os = "ios")]
    pub(crate) fn place(&self, placement: Option<crate::native_gpu::Placement>) {
        if let Some(host) = self.host.borrow().as_ref() {
            host.place(placement);
        }
    }
    /// The presented frame's rectangle in view pixels.
    pub(crate) fn frame_rectangle(&self) -> [f64; 4] {
        let (width, height) = self.client.get();
        self.scaling
            .get()
            .rectangle((f64::from(width), f64::from(height)), self.canvas.get())
    }
    /// The host view's layout: its drawable size in pixels.
    #[cfg(target_os = "ios")]
    pub(crate) fn resized(&self, width: u32, height: u32) {
        if self.client.replace((width, height)) != (width, height) {
            self.push(Event::Repaint);
        }
    }
    /// # Safety
    /// The host must keep the native window valid until it calls this with None.
    #[cfg(target_os = "android")]
    pub unsafe fn set_window(
        &self,
        window: Option<NonNull<ndk_sys::ANativeWindow>>,
        width: u32,
        height: u32,
    ) {
        if let Some(window) = window {
            unsafe {
                ndk_sys::ANativeWindow_acquire(window.as_ptr());
            }
        }
        if let Some(sink) = self.surface_sink.borrow().as_ref() {
            if let Err(error) = sink(window) {
                *self.error.borrow_mut() = Some(error);
            }
        }
        if let Some(old) = self.window.replace(window) {
            unsafe {
                ndk_sys::ANativeWindow_release(old.as_ptr());
            }
        }
        self.client.set((width, height));
        self.apply_frame_rate();
        self.push(Event::Repaint);
        self.report_layout();
    }
    /// Where the host placed the surface: `screen.game` is the surface's
    /// area in the host view's points, `scale` its pixels per point.
    #[cfg(target_os = "android")]
    pub fn set_screen(&self, screen: crate::mobile::Layout, scale: f64) {
        *self.screen.borrow_mut() = Some((screen, scale));
        self.set_touch_scale(scale as f32);
        self.report_layout();
    }
    /// Tells the host where the frame and the screen's obstructions are,
    /// when that changed (AND-APP-16).
    #[cfg(target_os = "android")]
    fn report_layout(&self) {
        let Some((screen, scale)) = self.screen.borrow().clone() else {
            return;
        };
        let [x, y, width, height] = self.frame_rectangle();
        let layout = crate::mobile::Layout {
            game: crate::mobile::Rect {
                x: screen.game.x + x / scale,
                y: screen.game.y + y / scale,
                width: width / scale,
                height: height / scale,
            },
            ..screen
        };
        if self.reported.borrow().as_ref() != Some(&layout) {
            *self.reported.borrow_mut() = Some(layout.clone());
            self.notify(ViewEvent::Layout(layout));
        }
    }
    /// Receives the host's view events: layout, keyboard requests and, on
    /// Android, the trackpad cursor.
    pub fn set_listener(&self, listener: Box<dyn Fn(ViewEvent)>) {
        *self.listener.borrow_mut() = Some(listener);
    }
    pub(crate) fn notify(&self, event: ViewEvent) {
        if let Some(listener) = self.listener.borrow().as_ref() {
            listener(event);
        }
    }
    pub fn touch(&self, pointer: i32, action: TouchAction, x: f32, y: f32) {
        if self.paused.get() {
            return;
        }
        let now = self.now();
        let gestures = self
            .gestures
            .borrow_mut()
            .update(pointer, action, x, y, now);
        for gesture in gestures {
            self.gesture(gesture);
        }
    }
    /// View pixels per point, for the gesture distances.
    pub(crate) fn set_touch_scale(&self, scale: f32) {
        self.gestures.borrow_mut().set_scale(scale);
    }
    fn now(&self) -> u64 {
        self.created.elapsed().as_millis() as u64
    }
    /// Fires the gesture timers that are due: long presses and the end of
    /// a click's hold.
    fn poll_gestures(&self) {
        if self.paused.get() {
            return;
        }
        let gestures = self.gestures.borrow_mut().tick(self.now());
        for gesture in gestures {
            self.gesture(gesture);
        }
    }
    /// When a gesture timer next needs `take_events`.
    pub(crate) fn input_deadline(&self) -> Option<Instant> {
        let at = self.gestures.borrow().deadline()?;
        Some(self.created + std::time::Duration::from_millis(at))
    }
    /// Switches between direct touch and the trackpad cursor (AND-APP-14).
    pub fn set_touch_mode(&self, mode: TouchMode) {
        let ended = self.gestures.borrow_mut().set_mode(mode);
        for gesture in ended {
            self.gesture(gesture);
        }
        self.release_buttons();
        if self.mode.replace(mode) != mode && mode == TouchMode::Trackpad {
            let (width, height) = self.canvas.get();
            self.cursor.set((width as f32 / 2.0, height as f32 / 2.0));
        }
        self.show_cursor();
    }
    /// Presses or releases a mouse button at the mouse position, for the
    /// overlay's buttons; dragging moves with it held.
    pub fn mouse_button(&self, right: bool, down: bool) {
        if self.paused.get() {
            return;
        }
        let flag = if right { 2 } else { 1 };
        let held = self.buttons.get();
        if down == (held & flag != 0) {
            return;
        }
        let buttons = if down { held | flag } else { held & !flag };
        self.buttons.set(buttons);
        let at = self.cursor_position();
        let message = match (right, down) {
            (false, true) => 0x201,
            (false, false) => 0x202,
            (true, true) => 0x204,
            (true, false) => 0x205,
        };
        for (message, wparam, lparam) in [(0x200, held, at), (message, buttons, at)] {
            self.push(Event::Input {
                message,
                wparam,
                lparam,
            });
        }
    }
    /// Presses or releases a virtual key for the overlay's keys: Enter, Esc,
    /// Space, Tab and Ctrl.
    pub fn overlay_key(&self, key: u32, down: bool) {
        if self.paused.get() {
            return;
        }
        let Some(scan) = crate::mobile::overlay_scan_code(key) else {
            return;
        };
        if key == 0x11 {
            self.control.set(down);
        }
        self.keys.borrow().set(key, down);
        let (message, wparam, lparam) = input::message(key, scan, down, false, false);
        self.push(Event::Input {
            message,
            wparam,
            lparam,
        });
    }
    /// Turns the wheel at the mouse position, for the overlay's wheel keys.
    pub fn wheel(&self, notches: i32) {
        if !self.paused.get() {
            self.gesture(Gesture::Scroll(notches));
        }
    }
    fn release_buttons(&self) {
        for (flag, right) in [(1, false), (2, true)] {
            if self.buttons.get() & flag != 0 {
                self.mouse_button(right, false);
            }
        }
    }
    fn gesture(&self, gesture: Gesture) {
        let (client, canvas, scaling) = (self.client.get(), self.canvas.get(), self.scaling.get());
        match gesture {
            Gesture::Keyboard => return self.notify(ViewEvent::Keyboard),
            Gesture::Move(dx, dy) if self.mode.get() == TouchMode::Trackpad => {
                let (sx, sy) = crate::mobile::magnification(client, canvas, scaling);
                let (x, y) = self.cursor.get();
                let limit = |v: f32, size: u32| v.clamp(0.0, size.saturating_sub(1) as f32);
                self.cursor.set((
                    limit(x + dx * TRACKPAD_GAIN / sx.max(f32::EPSILON), canvas.0),
                    limit(y + dy * TRACKPAD_GAIN / sy.max(f32::EPSILON), canvas.1),
                ));
                self.show_cursor();
            }
            Gesture::Point(x, y) => {
                let at = crate::mobile::position(client, canvas, scaling, x, y);
                self.cursor.set(((at & 0xffff) as f32, (at >> 16) as f32));
            }
            _ => {}
        }
        let (buttons, message) =
            crate::mobile::mouse(gesture, self.cursor_position(), self.buttons.get());
        self.buttons.set(buttons);
        if let Some((message, wparam, lparam)) = message {
            self.push(Event::Input {
                message,
                wparam,
                lparam,
            });
        }
    }
    /// The mouse position as a packed client lParam.
    fn cursor_position(&self) -> u32 {
        let (x, y) = self.cursor.get();
        (x as u32 & 0xffff) | (y as u32) << 16
    }
    /// Shows the trackpad cursor at the mouse position, or hides it.
    fn show_cursor(&self) {
        let point = (self.mode.get() == TouchMode::Trackpad).then(|| {
            let (x, y) = self.cursor.get();
            crate::mobile::client_point(
                self.client.get(),
                self.canvas.get(),
                self.scaling.get(),
                x,
                y,
            )
        });
        #[cfg(target_os = "ios")]
        if let Some(host) = self.host.borrow().as_ref() {
            host.set_cursor(point);
        }
        #[cfg(target_os = "android")]
        self.notify(ViewEvent::Cursor(point));
    }
    pub fn key(&self, key_code: i32, mut down: bool) {
        if self.paused.get() {
            return;
        }
        let Some((key, scan)) = input::key(key_code) else {
            return;
        };
        let repeat = !self.physical_keys.borrow_mut().insert(key_code);
        if !down {
            self.physical_keys.borrow_mut().remove(&key_code);
        }
        if key == 0x11 {
            if self.toggle_control.get() {
                if !down || repeat {
                    return;
                }
                down = !self.control.get();
            }
            self.control.set(down);
        }
        self.keys.borrow().set(key, down);
        let alt =
            self.keys.borrow().get(0x12) & 0x80 != 0 && self.keys.borrow().get(0x11) & 0x80 == 0;
        let (message, wparam, lparam) = input::message(key, scan, down, repeat, alt);
        self.push(Event::Input {
            message,
            wparam,
            lparam,
        });
    }
    pub(crate) fn bind_keys(&self, keys: Arc<adv_sync::input::KeyState>) {
        *self.keys.borrow_mut() = keys;
    }
    #[cfg(target_os = "android")]
    pub(crate) fn bind(&self, keys: Arc<adv_sync::input::KeyState>, sink: SurfaceSink) {
        self.bind_keys(keys);
        *self.surface_sink.borrow_mut() = Some(sink);
    }
    #[cfg(target_os = "android")]
    pub(crate) fn unbind(&self) {
        self.surface_sink.borrow_mut().take();
    }
    pub(crate) fn hub(&self) -> &Self {
        self
    }
    pub(crate) fn push(&self, event: Event) {
        if self.events.borrow().len() == 4096 {
            *self.error.borrow_mut() = Some("mobile input queue capacity exceeded".into());
        } else {
            self.events.borrow_mut().push(event);
        }
        if let Some(wake) = self.wake.borrow().as_ref() {
            wake.signal();
        }
    }
    pub(crate) fn activate(&self, active: bool) {
        if !active {
            self.push(Event::FocusLost);
        }
        self.push(Event::Input {
            message: 0x1c,
            wparam: u32::from(active),
            lparam: 0,
        });
    }
    pub(crate) fn pause(&self, paused: bool) {
        self.paused.set(paused);
        self.gestures.borrow_mut().set_mode(self.mode.get());
        self.buttons.set(0);
        self.events.borrow_mut().retain(|event| {
            !matches!(
                event,
                Event::Input { .. } | Event::Menu(_) | Event::Dialog(_) | Event::Text(..)
            )
        });
        self.clear_control_latch();
        self.push(Event::FocusLost);
        self.push(Event::Paused(paused));
    }
    pub(crate) fn request_close(&self) {
        self.push(Event::Close);
    }
    pub(crate) fn force_close(&self) {
        self.push(Event::ForceClose);
    }
    pub(crate) fn clear_control_latch(&self) {
        self.control.set(false);
        self.physical_keys.borrow_mut().clear();
        self.keys.borrow().release_all();
    }
    pub(crate) fn set_fast_forward_toggle(&self, toggle: bool) {
        if self.toggle_control.replace(toggle) != toggle {
            self.clear_control_latch();
            self.push(Event::FocusLost);
        }
    }
    pub(crate) fn post_input(&self, message: u32, wparam: u32, lparam: u32) -> Result<(), String> {
        let lparam = if (0x200..=0x20a).contains(&message) {
            input::position(
                self.client.get(),
                self.canvas.get(),
                self.scaling.get(),
                lparam as u16 as i16 as f32,
                (lparam >> 16) as u16 as i16 as f32,
            )
        } else {
            lparam
        };
        if wparam == 0x11 && matches!(message, 0x100 | 0x101 | 0x104 | 0x105) {
            self.keys
                .borrow()
                .set(wparam, matches!(message, 0x100 | 0x104));
        }
        self.push(Event::Input {
            message,
            wparam,
            lparam,
        });
        Ok(())
    }
    pub(crate) fn create_native(
        &self,
        fallback: bool,
    ) -> Result<(adv_device::gpu::Gpu, crate::native_gpu::WindowPresenter), String> {
        #[cfg(target_os = "android")]
        let surface = self
            .window
            .get()
            .ok_or("the Android view has no surface")?
            .as_ptr();
        #[cfg(target_os = "ios")]
        let surface = self
            .host
            .borrow()
            .as_ref()
            .ok_or("the iOS view is gone")?
            .metal_layer();
        let size = self.client.get();
        let (gpu, mut presenter) = pollster::block_on(unsafe {
            crate::native_gpu::WindowPresenter::request(
                surface.cast(),
                size.0.max(1),
                size.1.max(1),
                fallback,
            )
        })?;
        presenter.configure_exclusive(size.0.max(1), size.1.max(1))?;
        Ok((gpu, presenter))
    }
    pub(crate) fn native_client_size(&self) -> Result<(u32, u32), String> {
        Ok(if self.has_surface() && !self.paused.get() {
            self.client.get()
        } else {
            (0, 0)
        })
    }
    pub(crate) fn native_canvas(&self, canvas: (u32, u32)) -> Result<bool, String> {
        let changed = canvas.0 != 0 && canvas.1 != 0 && self.canvas.replace(canvas) != canvas;
        if changed {
            let (x, y) = self.cursor.get();
            let limit = |v: f32, size: u32| v.min(size.saturating_sub(1) as f32);
            self.cursor.set((limit(x, canvas.0), limit(y, canvas.1)));
            self.show_cursor();
            #[cfg(target_os = "android")]
            self.report_layout();
        }
        Ok(changed)
    }
    pub(crate) fn presentation(
        &self,
        menu: &adv_runtime::MenuBar,
        dialogs: &adv_runtime::DialogSet,
    ) -> Result<(), String> {
        // A dialog can come before the first frame (a startup prompt), so
        // the overlay hears about it whether or not the game has drawn.
        if *self.dialogs.borrow() != *dialogs {
            *self.dialogs.borrow_mut() = dialogs.clone();
            self.notify(ViewEvent::Dialogs(dialogs.clone()));
        }
        if *self.menu.borrow() != *menu {
            *self.menu.borrow_mut() = menu.clone();
            self.notify(ViewEvent::Menu(menu.clone()));
        }
        self.error.borrow_mut().take().map_or(Ok(()), Err)
    }
    pub(crate) fn take_events(&self) -> Vec<Event> {
        self.poll_gestures();
        self.events.borrow_mut().drain(..).collect()
    }
    pub(crate) fn watch(&self, wake: WakeEvent) {
        *self.wake.borrow_mut() = Some(wake);
    }
    pub(crate) fn hide(&self) {
        self.pause(true);
    }
    /// A mobile view cannot be minimized; the next touch restores the guest.
    pub(crate) fn minimize(&self) {}
    pub(crate) fn set_video_cursor_regions(&self, _: Arc<[adv_runtime::VideoCursorRegion]>) {}
    pub(crate) fn frame_rate(&self, rate: u32) {
        self.frame_clock.set(true);
        if self.requested_rate.replace(rate) == rate {
            return;
        }
        #[cfg(target_os = "android")]
        self.apply_frame_rate();
    }
    #[cfg(target_os = "android")]
    fn apply_frame_rate(&self) {
        if let Some(window) = self.window.get() {
            // API 30 symbol; the minimum supported Android API is 29.
            let symbol =
                unsafe { libc::dlsym(libc::RTLD_DEFAULT, c"ANativeWindow_setFrameRate".as_ptr()) };
            if !symbol.is_null() {
                let set: unsafe extern "C" fn(*mut ndk_sys::ANativeWindow, f32, i8) -> i32 =
                    unsafe { std::mem::transmute(symbol) };
                unsafe {
                    set(window.as_ptr(), self.requested_rate.get() as f32, 0);
                }
            }
        }
    }
    pub(crate) fn next_refresh(&self) -> Instant {
        let fps = (1_000_000_000.0 / self.period.get() as f64).round() as isize;
        Instant::now() + refresh_interval(fps)
    }
}
impl GameView {
    #[cfg(target_os = "android")]
    fn has_surface(&self) -> bool {
        self.window.get().is_some()
    }
    /// The layer persists; a view outside any window has nothing to show.
    #[cfg(target_os = "ios")]
    fn has_surface(&self) -> bool {
        self.host
            .borrow()
            .as_ref()
            .is_some_and(|host| host.window().is_some())
    }
}
#[cfg(target_os = "android")]
impl Drop for GameView {
    fn drop(&mut self) {
        if let Some(window) = self.window.take() {
            unsafe {
                ndk_sys::ANativeWindow_release(window.as_ptr());
            }
        }
    }
}
pub(crate) fn native_viewport(
    width: u32,
    height: u32,
    canvas_width: u32,
    canvas_height: u32,
) -> [f32; 4] {
    crate::Scaling::Fit
        .rectangle((width as f64, height as f64), (canvas_width, canvas_height))
        .map(|v| v as f32)
}
