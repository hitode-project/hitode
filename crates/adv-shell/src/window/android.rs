//! An Android SurfaceView supplied by the embedding host. All access is on its main looper.
use super::{Event, WakeEvent, refresh_interval};
use std::{
    cell::{Cell, RefCell},
    ptr::NonNull,
    rc::Rc,
    sync::Arc,
    time::Instant,
};
#[path = "android/input.rs"]
mod input;
pub use input::TouchAction;
type SurfaceSink = Box<dyn Fn(Option<NonNull<ndk_sys::ANativeWindow>>) -> Result<(), String>>;
pub struct GameView {
    window: Cell<Option<NonNull<ndk_sys::ANativeWindow>>>,
    client: Cell<(u32, u32)>,
    canvas: Cell<(u32, u32)>,
    pub(crate) scaling: Cell<crate::Scaling>,
    events: RefCell<Vec<Event>>,
    wake: RefCell<Option<WakeEvent>>,
    surface_sink: RefCell<Option<SurfaceSink>>,
    error: RefCell<Option<String>>,
    touch: RefCell<input::Touch>,
    paused: Cell<bool>,
    toggle_control: Cell<bool>,
    control: Cell<bool>,
    physical_keys: RefCell<std::collections::BTreeSet<i32>>,
    keys: RefCell<Arc<adv_sync::input::KeyState>>,
    pub(crate) ready: Cell<bool>,
    pub(crate) period: Cell<i64>,
}
impl GameView {
    pub fn new(width: u32, height: u32) -> Result<Rc<Self>, String> {
        if unsafe { libc::gettid() } != unsafe { libc::getpid() } {
            return Err("the game view must run on the main thread".into());
        }
        Ok(Rc::new(Self {
            window: Cell::new(None),
            client: Cell::new((width, height)),
            canvas: Cell::new((width.max(1), height.max(1))),
            scaling: Cell::new(crate::Scaling::Fit),
            events: RefCell::new(Vec::new()),
            wake: RefCell::new(None),
            surface_sink: RefCell::new(None),
            error: RefCell::new(None),
            touch: RefCell::new(input::Touch::default()),
            paused: Cell::new(false),
            toggle_control: Cell::new(false),
            control: Cell::new(false),
            physical_keys: RefCell::new(Default::default()),
            keys: RefCell::new(Arc::default()),
            ready: Cell::new(false),
            period: Cell::new(16_666_667),
        }))
    }
    /// # Safety
    /// The host must keep the native window valid until it calls this with None.
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
        if let Some(window) = window {
            // API 30 symbol; the minimum supported Android API is 29.
            let symbol =
                unsafe { libc::dlsym(libc::RTLD_DEFAULT, c"ANativeWindow_setFrameRate".as_ptr()) };
            if !symbol.is_null() {
                let set: unsafe extern "C" fn(*mut ndk_sys::ANativeWindow, f32, i8) -> i32 =
                    unsafe { std::mem::transmute(symbol) };
                unsafe {
                    set(window.as_ptr(), 60.0, 0);
                }
            }
        }
        self.push(Event::Repaint);
    }
    pub fn touch(&self, pointer: i32, action: TouchAction, x: f32, y: f32) {
        if self.paused.get() {
            return;
        }
        let position = input::position(
            self.client.get(),
            self.canvas.get(),
            self.scaling.get(),
            x,
            y,
        );
        for (message, wparam, lparam) in self.touch.borrow_mut().update(pointer, action, position) {
            self.push(Event::Input {
                message,
                wparam,
                lparam,
            });
        }
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
    pub(crate) fn bind(&self, keys: Arc<adv_sync::input::KeyState>, sink: SurfaceSink) {
        *self.keys.borrow_mut() = keys;
        *self.surface_sink.borrow_mut() = Some(sink);
    }
    pub(crate) fn unbind(&self) {
        self.surface_sink.borrow_mut().take();
    }
    pub(crate) fn hub(&self) -> &Self {
        self
    }
    pub(crate) fn push(&self, event: Event) {
        if self.events.borrow().len() == 4096 {
            *self.error.borrow_mut() = Some("Android input queue capacity exceeded".into());
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
        *self.touch.borrow_mut() = input::Touch::default();
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
        let window = self.window.get().ok_or("the Android view has no surface")?;
        let size = self.client.get();
        let (gpu, mut presenter) = pollster::block_on(unsafe {
            crate::native_gpu::WindowPresenter::request(
                window.as_ptr().cast(),
                size.0.max(1),
                size.1.max(1),
                fallback,
            )
        })?;
        presenter.configure_exclusive(size.0.max(1), size.1.max(1))?;
        Ok((gpu, presenter))
    }
    pub(crate) fn native_client_size(&self) -> Result<(u32, u32), String> {
        Ok(if self.window.get().is_some() && !self.paused.get() {
            self.client.get()
        } else {
            (0, 0)
        })
    }
    pub(crate) fn native_canvas(&self, canvas: (u32, u32)) -> Result<bool, String> {
        Ok(canvas.0 != 0 && canvas.1 != 0 && self.canvas.replace(canvas) != canvas)
    }
    pub(crate) fn presentation(
        &self,
        _: &adv_runtime::MenuBar,
        _: &adv_runtime::DialogSet,
    ) -> Result<(), String> {
        self.error.borrow_mut().take().map_or(Ok(()), Err)
    }
    pub(crate) fn take_events(&self) -> Vec<Event> {
        self.events.borrow_mut().drain(..).collect()
    }
    pub(crate) fn watch(&self, wake: WakeEvent) {
        *self.wake.borrow_mut() = Some(wake);
    }
    pub(crate) fn hide(&self) {
        self.pause(true);
    }
    pub(crate) fn set_video_cursor_regions(&self, _: Arc<[adv_runtime::VideoCursorRegion]>) {}
    pub(crate) fn refresh_ready(&self) -> bool {
        self.ready.get()
    }
    pub(crate) fn next_refresh(&self) -> Instant {
        let fps = (1_000_000_000.0 / self.period.get() as f64).round() as isize;
        Instant::now() + refresh_interval(fps)
    }
}
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
