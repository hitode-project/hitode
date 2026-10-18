//! A game session driven by the host main loop.
use crate::{
    Finished, Options, frontend,
    window::{Event, GameView},
};
use adv_runtime::{DialogEvent, DialogSet, MenuBar, MenuEvent, MenuItem};
use adv_sync::{WakeEvent, input::KeyState, spsc};
#[cfg(target_os = "macos")]
use objc2::{MainThreadMarker, rc::Retained, runtime::ProtocolObject};
#[cfg(target_os = "macos")]
use objc2_app_kit::NSApplication;
#[cfg(target_os = "macos")]
use objc2_core_foundation::{
    CFAbsoluteTimeGetCurrent, CFFileDescriptor, CFFileDescriptorContext, CFOptionFlags, CFRetained,
    CFRunLoop, CFRunLoopSource, CFRunLoopTimer, CFRunLoopTimerContext,
    kCFFileDescriptorReadCallBack, kCFRunLoopCommonModes,
};
#[cfg(target_os = "macos")]
use objc2_foundation::{NSActivityOptions, NSObjectProtocol, NSProcessInfo, NSString};
use std::{cell::RefCell, rc::Rc, sync::Arc, time::Instant};
#[cfg(target_os = "macos")]
use std::{ffi::c_void, time::Duration};

pub struct Session {
    driver: Rc<Driver>,
    #[cfg(target_os = "macos")]
    descriptor: CFRetained<CFFileDescriptor>,
    #[cfg(target_os = "macos")]
    source: CFRetained<CFRunLoopSource>,
    #[cfg(target_os = "macos")]
    timer: CFRetained<CFRunLoopTimer>,
}
type OnFinished = Box<dyn FnOnce(Result<Finished, String>)>;

struct Driver {
    #[cfg(mobile)]
    main_loop: RefCell<Option<platform::MainLoop>>,
    #[cfg(target_os = "macos")]
    activity: Retained<ProtocolObject<dyn NSObjectProtocol>>,
    view: View,
    ui: RefCell<Option<frontend::Ui>>,
    done: RefCell<spsc::Consumer<Result<Finished, String>>>,
    pending: RefCell<Option<Result<Finished, String>>>,
    execution: RefCell<Option<std::thread::JoinHandle<()>>>,
    service: RefCell<adv_sync::background::Service>,
    on_finished: RefCell<Option<OnFinished>>,
    #[cfg(target_os = "macos")]
    timer: RefCell<Option<CFRetained<CFRunLoopTimer>>>,
    wake: WakeEvent,
    /// Guest menus and dialogs; borrowed across a nested popup loop.
    #[cfg(target_os = "macos")]
    projection: RefCell<Option<Rc<RefCell<Projection>>>>,
    #[cfg(target_os = "macos")]
    cli: RefCell<Option<Rc<RefCell<Window>>>>,
}
#[cfg(target_os = "macos")]
use crate::window::{Projection, Window};
#[cfg(target_os = "macos")]
type View = Retained<GameView>;
#[cfg(mobile)]
type View = Rc<GameView>;
#[cfg(target_os = "android")]
#[path = "session/android.rs"]
mod platform;
#[cfg(target_os = "ios")]
#[path = "session/ios.rs"]
mod platform;

impl Session {
    /// Starts a game in the host view, on the main thread.
    /// Android requires the initial SurfaceHolder window to be attached.
    pub fn new(
        view: View,
        options: Options,
        on_finished: Box<dyn FnOnce(Result<Finished, String>)>,
    ) -> Result<Self, String> {
        #[cfg(target_os = "macos")]
        let projection = Rc::new(RefCell::new(Projection::embedded(view.clone())?));
        Self::start(
            view,
            options,
            on_finished,
            #[cfg(target_os = "macos")]
            projection,
        )
    }
    fn start(
        view: View,
        options: Options,
        on_finished: Box<dyn FnOnce(Result<Finished, String>)>,
        #[cfg(target_os = "macos")] projection: Rc<RefCell<Projection>>,
    ) -> Result<Self, String> {
        let wake = WakeEvent::new().map_err(|e| e.to_string())?;
        let keys = Arc::new(KeyState::default());
        let mut background = adv_sync::background::Builder::new(wake.clone())
            .map_err(|e| e.to_string())?
            .with_work_policy(adv_sync::background::WorkPolicy::responsive(256));
        let execution_sender = background
            .sender_with_capacity_wake(8, 8, wake.clone())
            .map_err(|e| e.to_string())?;
        let (mut worker, mut ui) = frontend::channel(wake.clone(), keys.clone())?;
        let (gpu, presenter) = view.create_native(options.gpu_software)?;
        gpu.context()
            .set_execution_wake(wake.clone())
            .map_err(|_| "GPU execution wake was already registered")?;
        let sender = background
            .sender_with_capacity_wake(1, 1, ui.wake())
            .map_err(|e| e.to_string())?;
        ui.native(presenter, sender);
        #[cfg(target_os = "macos")]
        ui.install_sinks(&mut projection.borrow_mut());
        worker.gpu = Some(gpu);
        #[cfg(target_os = "macos")]
        {
            worker.vsync = Some(view.vsync());
        }
        #[cfg(target_os = "ios")]
        {
            worker.vsync = Some(view.vsync.clone());
        }
        let service = background.start().map_err(|e| e.to_string())?;
        let completion_wake = ui.wake();
        view.watch(completion_wake.clone());
        let (mut tx, rx) = spsc::channel(1);
        #[cfg(mobile)]
        let view_keys = keys.clone();
        let execution = std::thread::Builder::new()
            .name("adv-execution".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    crate::run_execution(options, Some(worker), wake, keys, execution_sender)
                }))
                .unwrap_or_else(|_| Err("execution worker panicked".into()));
                let _ = tx.push(result);
                completion_wake.signal();
            })
            .map_err(|e| e.to_string())?;
        let driver = Rc::new(Driver {
            #[cfg(mobile)]
            main_loop: RefCell::new(None),
            #[cfg(target_os = "macos")]
            activity: NSProcessInfo::processInfo().beginActivityWithOptions_reason(
                NSActivityOptions::UserInitiatedAllowingIdleSystemSleep,
                &NSString::from_str("Guest execution and audio playback"),
            ),
            view,
            wake: ui.wake(),
            ui: RefCell::new(Some(ui)),
            done: RefCell::new(rx),
            pending: RefCell::new(None),
            execution: RefCell::new(Some(execution)),
            service: RefCell::new(service),
            on_finished: RefCell::new(Some(on_finished)),
            #[cfg(target_os = "macos")]
            timer: RefCell::new(None),
            #[cfg(target_os = "macos")]
            projection: RefCell::new(Some(projection)),
            #[cfg(target_os = "macos")]
            cli: RefCell::new(None),
        });
        #[cfg(target_os = "ios")]
        {
            *driver.main_loop.borrow_mut() = Some(platform::MainLoop::new(&driver)?);
            driver.view.bind_keys(view_keys);
            Ok(Self { driver })
        }
        #[cfg(target_os = "android")]
        {
            *driver.main_loop.borrow_mut() = Some(platform::MainLoop::new(&driver)?);
            let weak = Rc::downgrade(&driver);
            driver.view.bind(
                view_keys,
                Box::new(move |window| {
                    if let Some(driver) = weak.upgrade() {
                        if let Some(ui) = driver.ui.borrow_mut().as_mut() {
                            unsafe {
                                ui.replace_window(window)?;
                            }
                        }
                    }
                    Ok(())
                }),
            );
            Ok(Self { driver })
        }
        #[cfg(target_os = "macos")]
        {
            let info = Rc::as_ptr(&driver) as *mut c_void;
            let context = CFFileDescriptorContext {
                version: 0,
                info,
                retain: None,
                release: None,
                copyDescription: None,
            };
            // The Session retains Driver and invalidates both sources before dropping it.
            let descriptor = unsafe {
                CFFileDescriptor::new(
                    None,
                    driver.wake.as_raw_fd(),
                    false,
                    Some(wake_callback),
                    &context,
                )
            }
            .ok_or("CFFileDescriptorCreate failed")?;
            let source = CFFileDescriptor::new_run_loop_source(None, Some(&descriptor), 0)
                .ok_or("CFFileDescriptorCreateRunLoopSource failed")?;
            let mut context = CFRunLoopTimerContext {
                version: 0,
                info,
                retain: None,
                release: None,
                copyDescription: None,
            };
            let timer = unsafe {
                CFRunLoopTimer::new(
                    None,
                    f64::MAX,
                    f64::MAX,
                    0,
                    0,
                    Some(timer_callback),
                    &mut context,
                )
            }
            .ok_or("CFRunLoopTimerCreate failed")?;
            *driver.timer.borrow_mut() = Some(timer.clone());
            let run_loop = CFRunLoop::main().ok_or("no main run loop")?;
            run_loop.add_source(Some(&source), unsafe { kCFRunLoopCommonModes });
            run_loop.add_timer(Some(&timer), unsafe { kCFRunLoopCommonModes });
            descriptor.enable_call_backs(kCFFileDescriptorReadCallBack);
            let weak = Rc::downgrade(&driver);
            driver.view.start_frame_clock(Rc::new(move || {
                if let Some(driver) = weak.upgrade() {
                    driver.drive();
                }
            }));
            Ok(Self {
                driver,
                descriptor,
                source,
                timer,
            })
        }
    }
    /// Post confirmed IME text through the input channel and return the guest's count.
    pub fn post_text(&self, text: &str) -> usize {
        self.driver
            .ui
            .borrow_mut()
            .as_mut()
            .map_or(0, |ui| ui.post_text(text))
    }
    /// Application activation, delivered to the guest as WM_ACTIVATEAPP.
    pub fn set_active(&self, active: bool) {
        if !active {
            self.driver.view.clear_control_latch();
        }
        self.driver.view.hub().activate(active);
    }
    /// The window showing the game lost key status. A guest dialog taking it
    /// keeps the guest's focus; another window removes it.
    #[cfg(target_os = "macos")]
    pub fn resign_key(&self) {
        self.driver.view.clear_control_latch();
        self.driver.view.hub().resign_key();
    }
    #[cfg(target_os = "macos")]
    pub fn set_minimized(&self, minimized: bool) {
        self.driver.view.hub().minimized(minimized);
    }
    #[cfg(target_os = "macos")]
    pub fn set_occluded(&self, occluded: bool) {
        self.driver.view.hub().set_occluded(occluded);
    }
    #[cfg(target_os = "macos")]
    pub fn sleep(&self, sleeping: bool) {
        self.driver.view.hub().sleep(sleeping);
    }
    pub fn set_upscaler(&self, value: crate::Upscaler) {
        if let Some(ui) = self.driver.ui.borrow_mut().as_mut() {
            ui.set_upscaler(value);
        }
    }
    /// Set the presentation and input coordinate scaling together.
    pub fn set_scaling(&self, value: crate::Scaling) {
        self.driver.view.hub().scaling.set(value);
        if let Some(ui) = self.driver.ui.borrow_mut().as_mut() {
            ui.set_scaling(value);
        }
    }
    pub fn set_fast_forward_toggle(&self, toggle: bool) {
        self.driver.view.set_fast_forward_toggle(toggle);
    }
    pub fn fps(&self) -> f64 {
        self.driver.ui.borrow().as_ref().map_or(0.0, |ui| ui.fps())
    }
    /// Whether the game has shown its first frame.
    pub fn presented(&self) -> bool {
        self.driver
            .ui
            .borrow()
            .as_ref()
            .is_some_and(|ui| ui.presented())
    }
    /// Milliseconds between the presents of the last second, oldest first.
    pub fn frame_intervals(&self) -> Vec<f32> {
        self.driver
            .ui
            .borrow()
            .as_ref()
            .map_or_else(Vec::new, |ui| ui.frame_intervals())
    }
    pub fn set_volume(&self, percent: u8) {
        self.driver.view.hub().push(Event::Volume(percent.min(100)));
    }
    pub fn set_paused(&self, paused: bool) {
        #[cfg(mobile)]
        if let Some(ui) = self.driver.ui.borrow_mut().as_mut() {
            ui.pause_input(paused);
        }
        self.driver.view.hub().pause(paused);
    }
    pub fn request_close(&self) {
        self.driver.view.hub().request_close();
    }
    pub fn force_close(&self) {
        self.driver.view.hub().force_close();
    }
    #[cfg(target_os = "macos")]
    pub fn power_off(&self) {
        self.driver.view.hub().power_off();
    }
    pub fn menu_snapshot(&self) -> MenuBar {
        self.driver
            .ui
            .borrow()
            .as_ref()
            .map(|ui| ui.menu_snapshot().clone())
            .unwrap_or_default()
    }
    pub fn dialog_snapshot(&self) -> DialogSet {
        self.driver
            .ui
            .borrow()
            .as_ref()
            .map(|ui| ui.dialog_snapshot().clone())
            .unwrap_or_default()
    }
    pub fn menu_command(&self, id: u32) {
        fn command(items: &[MenuItem], id: u32) -> Option<adv_runtime::MenuCommand> {
            items.iter().find_map(|item| {
                if item.submenu == 0 && item.command.id == id {
                    Some(item.command)
                } else {
                    command(&item.children, id)
                }
            })
        }
        if let Some(command) = command(&self.menu_snapshot().items, id) {
            self.driver
                .view
                .hub()
                .push(Event::Menu(MenuEvent::Command(command)));
        }
    }
    pub fn menu_event(&self, event: MenuEvent) {
        self.driver.view.hub().push(Event::Menu(event));
    }
    pub fn dialog_event(&self, event: DialogEvent) {
        self.driver.view.hub().push(Event::Dialog(event));
    }
}
impl Driver {
    fn tick(&self) -> Result<Option<Instant>, String> {
        #[cfg(target_os = "macos")]
        let projection = self.projection.borrow().clone();
        if self.finish_pending() {
            return Ok(None);
        }
        #[cfg(target_os = "macos")]
        if let Some(projection) = &projection
            && let Ok(mut projection) = projection.try_borrow_mut()
        {
            let events = projection.pump();
            if let Some(ui) = self.ui.borrow_mut().as_mut() {
                for event in events {
                    ui.input(event)?;
                }
            }
        }
        {
            let mut ui = self.ui.borrow_mut();
            let Some(ui) = ui.as_mut() else {
                return Ok(None);
            };
            ui.tick(&self.view)?;
        }
        // A native popup runs a nested AppKit loop. Its input sinks remain
        // usable while projection is borrowed by the outer invocation.
        #[cfg(target_os = "macos")]
        if let Some(projection) = &projection
            && let Ok(mut projection) = projection.try_borrow_mut()
        {
            let (menu, dialogs) = {
                let mut ui = self.ui.borrow_mut();
                let Some(ui) = ui.as_mut() else {
                    return Ok(None);
                };
                (
                    std::mem::take(&mut ui.menu_changed).then(|| ui.menu_snapshot().clone()),
                    std::mem::take(&mut ui.dialog_changed).then(|| ui.dialog_snapshot().clone()),
                )
            };
            if let Some(menu) = menu {
                projection.set_menu_bar(&menu)?;
            }
            if let Some(dialogs) = dialogs {
                projection.set_dialogs(&dialogs)?;
            }
            // The CLI window's own borrow reaches the projection when hiding.
            drop(projection);
            if let Some(cli) = self.cli.borrow().clone() {
                let mut window = cli.borrow_mut();
                window.native_canvas(self.view.canvas_size())?;
                if self.view.isHidden() {
                    window.hide();
                }
            }
        }
        if self.finish_pending() {
            return Ok(None);
        }
        let ui = self.ui.borrow();
        match ui.as_ref() {
            Some(ui) => ui.deadline(&self.view),
            None => Ok(None),
        }
    }
    fn finish_pending(&self) -> bool {
        #[cfg(target_os = "macos")]
        if self
            .projection
            .borrow()
            .as_ref()
            .is_some_and(|projection| projection.try_borrow().is_err())
        {
            return false;
        }
        let result = self
            .pending
            .borrow_mut()
            .take()
            .or_else(|| self.done.borrow_mut().pop());
        if let Some(result) = result {
            self.finish(result);
            return true;
        }
        false
    }
    fn finish(&self, mut result: Result<Finished, String>) {
        drop(self.ui.borrow_mut().take());
        // Retires guest menus and dialogs unless a nested loop still holds them.
        #[cfg(target_os = "macos")]
        drop(self.projection.borrow_mut().take());
        if let Some(execution) = self.execution.borrow_mut().take()
            && execution.join().is_err()
        {
            result = Err("execution worker panicked".into());
        }
        self.service.borrow_mut().shutdown();
        if let Some(callback) = self.on_finished.borrow_mut().take() {
            callback(result);
        }
    }
    fn drive(&self) {
        let next = match self.tick() {
            Ok(next) => next,
            Err(error) => {
                self.pending.borrow_mut().get_or_insert(Err(error));
                self.finish_pending();
                None
            }
        };
        #[cfg(mobile)]
        {
            let result = self
                .main_loop
                .borrow()
                .as_ref()
                .map(|main_loop| main_loop.arm(next));
            if let Some(Err(error)) = result {
                self.finish(Err(error));
            }
        }
        // A due presentation waits for the next vsync instead of the timer.
        #[cfg(target_os = "macos")]
        let next = match next {
            Some(at) if at <= Instant::now() && self.view.request_frame() => None,
            other => other,
        };
        #[cfg(target_os = "macos")]
        if let Some(timer) = self.timer.borrow().as_ref()
            && timer.is_valid()
        {
            let date = next.map_or(f64::MAX, |next| {
                CFAbsoluteTimeGetCurrent()
                    + next.saturating_duration_since(Instant::now()).as_secs_f64()
            });
            timer.set_next_fire_date(date);
        }
    }
}
// A completion callback may drop its Session. Keep Driver alive until this
// invocation returns, including when AppKit is dispatching a nested menu loop.
#[cfg(target_os = "macos")]
unsafe fn callback_driver(info: *mut c_void) -> Rc<Driver> {
    let pointer = info as *const Driver;
    unsafe {
        Rc::increment_strong_count(pointer);
        Rc::from_raw(pointer)
    }
}
#[cfg(target_os = "macos")]
unsafe extern "C-unwind" fn wake_callback(
    fd: *mut CFFileDescriptor,
    _: CFOptionFlags,
    info: *mut c_void,
) {
    let driver = unsafe { callback_driver(info) };
    match driver.wake.wait(Some(Duration::ZERO)) {
        Ok(_) => {
            unsafe { &*fd }.enable_call_backs(kCFFileDescriptorReadCallBack);
            driver.drive();
        }
        Err(error) => driver.finish(Err(error.to_string())),
    }
}
#[cfg(target_os = "macos")]
unsafe extern "C-unwind" fn timer_callback(_: *mut CFRunLoopTimer, info: *mut c_void) {
    unsafe { callback_driver(info) }.drive();
}
impl Drop for Session {
    fn drop(&mut self) {
        #[cfg(target_os = "android")]
        self.driver.view.unbind();
        #[cfg(mobile)]
        self.driver.main_loop.borrow_mut().take();
        #[cfg(target_os = "macos")]
        {
            self.driver.view.stop_frame_clock();
            self.timer.invalidate();
            self.descriptor.invalidate();
            self.source.invalidate();
        }
    }
}
impl Drop for Driver {
    fn drop(&mut self) {
        // Resource owners finish before the view is released, including startup errors.
        drop(self.ui.get_mut().take());
        if let Some(execution) = self.execution.get_mut().take() {
            let _ = execution.join();
        }
        self.service.get_mut().shutdown();
        #[cfg(target_os = "macos")]
        unsafe {
            NSProcessInfo::processInfo().endActivity(&self.activity);
        }
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn run_cli(options: Options) -> Result<u8, String> {
    let window = Rc::new(RefCell::new(Window::new(
        &format!(
            "ADV — {}",
            options
                .exe
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
        ),
        options.width,
        options.height,
        options.hidden,
    )?));
    let result = Rc::new(RefCell::new(None));
    let output = result.clone();
    let closing = window.clone();
    let (view, projection) = {
        let window = window.borrow();
        (window.view.clone(), window.projection.clone())
    };
    let session = Session::start(
        view,
        options,
        Box::new(move |value| {
            *output.borrow_mut() = Some(value);
            closing.borrow().finish_termination();
            let app = NSApplication::sharedApplication(MainThreadMarker::new().unwrap());
            app.stop(None);
            if let Some(event) = crate::window::macos::wake_event() {
                app.postEvent_atStart(&event, false);
            }
        }),
        projection,
    )?;
    *session.driver.cli.borrow_mut() = Some(window);
    NSApplication::sharedApplication(MainThreadMarker::new().unwrap()).run();
    drop(session);
    result
        .borrow_mut()
        .take()
        .unwrap_or_else(|| Err("application run loop stopped before the game finished".into()))
        .map(|finished| finished.exit_code)
}
