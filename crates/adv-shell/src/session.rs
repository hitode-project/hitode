//! A game session driven by the host main loop.
use crate::{
    Finished, Options, RunError, frontend,
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
type OnFinished = Box<dyn FnOnce(Result<Finished, RunError>)>;

#[cfg(target_os = "android")]
struct OwnedNativeWindow(std::ptr::NonNull<ndk_sys::ANativeWindow>);
#[cfg(target_os = "android")]
// SAFETY: ANativeWindow refcounting and SurfaceControl creation have no thread affinity.
unsafe impl Send for OwnedNativeWindow {}
#[cfg(target_os = "android")]
impl OwnedNativeWindow {
    /// # Safety
    /// `window` must be valid on entry; this wrapper acquires its own reference.
    unsafe fn acquire(window: std::ptr::NonNull<ndk_sys::ANativeWindow>) -> Self {
        unsafe {
            ndk_sys::ANativeWindow_acquire(window.as_ptr());
        }
        Self(window)
    }
    fn as_non_null(&self) -> std::ptr::NonNull<ndk_sys::ANativeWindow> {
        self.0
    }
    fn addr(&self) -> usize {
        self.0.as_ptr() as usize
    }
}
#[cfg(target_os = "android")]
impl Drop for OwnedNativeWindow {
    fn drop(&mut self) {
        unsafe {
            ndk_sys::ANativeWindow_release(self.0.as_ptr());
        }
    }
}

/// Stand-in compositor used only if the Android surface is transiently absent
/// while Vulkan initializes off the UI thread; suspended before any present.
#[cfg(target_os = "android")]
struct PendingCompositor(adv_display::plan::CompositorCaps);
#[cfg(target_os = "android")]
impl adv_display::compositor::Compositor for PendingCompositor {
    fn detach(&mut self) -> Option<adv_gpu::queue::Fence> {
        None
    }
    fn caps(&self) -> adv_display::plan::CompositorCaps {
        self.0.clone()
    }
    fn present(
        &mut self,
        _: &adv_display::plan::Layers,
        _: &adv_display::plan::Tiles,
        _: &mut [adv_display::compositor::AcquiredImage],
        _: u64,
        _: Instant,
    ) -> Result<(), adv_display::compositor::Error> {
        Err(adv_display::compositor::Error::SurfaceLost)
    }
    fn drain(
        &mut self,
        _: &mut Vec<adv_display::compositor::Presented>,
        _: &mut Vec<adv_display::compositor::Released>,
    ) {
    }
}

#[cfg(target_os = "android")]
struct AndroidDisplayInit {
    latest_window: Arc<std::sync::Mutex<Option<OwnedNativeWindow>>>,
    ready: std::sync::mpsc::Receiver<(crate::display::Ui, Option<usize>)>,
    upscaler: Option<crate::Upscaler>,
    scaling: Option<crate::Scaling>,
    trim: bool,
}

#[cfg(target_os = "android")]
fn prepare_android_display(
    options: &Options,
    latest_window: &std::sync::Mutex<Option<OwnedNativeWindow>>,
    view_size: (u32, u32),
    wake: WakeEvent,
) -> Result<(crate::display::Startup, crate::display::Ui, Option<usize>), String> {
    crate::display::open_with(
        options.gpu_software,
        Some(options.gpu_health_dir()),
        |device, queue| {
            let caps = crate::display::platform_caps(Some(&device));
            let current = latest_window
                .lock()
                .unwrap()
                .as_ref()
                .map(|w| unsafe { OwnedNativeWindow::acquire(w.as_non_null()) });
            let (compositor, built_window): (
                Box<dyn adv_display::compositor::Compositor>,
                Option<usize>,
            ) = match current {
                Some(window) => match unsafe {
                    adv_display::compositor::android::SurfaceCompositor::new(
                        window.as_non_null(),
                        caps.clone(),
                    )
                } {
                    Ok(compositor) => (Box::new(compositor), Some(window.addr())),
                    Err(adv_display::compositor::Error::SurfaceLost) => {
                        (Box::new(PendingCompositor(caps)), None)
                    }
                    Err(error) => return Ok(Err(format!("SurfaceControl: {error:?}"))),
                },
                None => (Box::new(PendingCompositor(caps)), None),
            };
            let (startup, ui) =
                crate::display::channel(device, queue, compositor, view_size, wake.clone())?;
            Ok(Ok((startup, ui, built_window)))
        },
    )
}

struct Driver {
    options: Arc<Options>,
    #[cfg(mobile)]
    main_loop: RefCell<Option<platform::MainLoop>>,
    #[cfg(target_os = "macos")]
    activity: Retained<ProtocolObject<dyn NSObjectProtocol>>,
    view: View,
    ui: RefCell<Option<frontend::Ui>>,
    #[cfg(target_os = "android")]
    display_init: RefCell<Option<AndroidDisplayInit>>,
    #[cfg(target_os = "macos")]
    damage_hud: adv_display::display::DamageHud,
    done: RefCell<spsc::Consumer<Result<Finished, RunError>>>,
    pending: RefCell<Option<Result<Finished, RunError>>>,
    execution: RefCell<Option<crate::execution::Task>>,
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
        on_finished: Box<dyn FnOnce(Result<Finished, RunError>)>,
    ) -> Result<Self, String> {
        Self::new_inner(view, options, on_finished, None)
    }

    /// Starts one Wine game on the player's persistent execution owner.
    pub fn new_with_execution(
        view: View,
        mut options: Options,
        on_finished: Box<dyn FnOnce(Result<Finished, RunError>)>,
        execution: &crate::Execution,
    ) -> Result<Self, String> {
        options.wine = Some(execution.distribution.clone());
        Self::new_inner(view, options, on_finished, Some(execution))
    }

    /// Starts one Wine game on Android without blocking the activity's UI
    /// thread on Vulkan device open, health probing, or initial display
    /// pipeline compilation (AND-APP-10, AND-APP-13).
    ///
    /// # Safety
    /// `window` must be valid on entry; the session acquires its own reference.
    #[cfg(target_os = "android")]
    pub unsafe fn new_with_execution_surface(
        view: View,
        window: std::ptr::NonNull<ndk_sys::ANativeWindow>,
        view_size: (u32, u32),
        mut options: Options,
        on_finished: Box<dyn FnOnce(Result<Finished, String>)>,
        execution: &crate::Execution,
    ) -> Result<Self, String> {
        options.wine = Some(execution.distribution.clone());
        let initial = unsafe { OwnedNativeWindow::acquire(window) };
        Self::start(
            view,
            options,
            on_finished,
            Some(execution),
            Some((initial, view_size)),
        )
    }

    fn new_inner(
        view: View,
        options: Options,
        on_finished: OnFinished,
        execution: Option<&crate::Execution>,
    ) -> Result<Self, String> {
        #[cfg(target_os = "macos")]
        let projection = Rc::new(RefCell::new(Projection::embedded(view.clone())?));
        Self::start(
            view,
            options,
            on_finished,
            execution,
            #[cfg(target_os = "macos")]
            projection,
            #[cfg(target_os = "android")]
            None,
        )
    }
    fn start(
        view: View,
        options: Options,
        on_finished: Box<dyn FnOnce(Result<Finished, RunError>)>,
        execution: Option<&crate::Execution>,
        #[cfg(target_os = "macos")] projection: Rc<RefCell<Projection>>,
        #[cfg(target_os = "android")] android_surface: Option<(OwnedNativeWindow, (u32, u32))>,
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

        #[cfg(target_os = "android")]
        let (display_init, android_display) = match android_surface {
            Some((initial_window, view_size)) => {
                let latest_window = Arc::new(std::sync::Mutex::new(Some(initial_window)));
                let (display_tx, display_rx) = std::sync::mpsc::sync_channel(1);
                let display_wake = ui.wake();
                (
                    Some(AndroidDisplayInit {
                        latest_window: latest_window.clone(),
                        ready: display_rx,
                        upscaler: None,
                        scaling: None,
                        trim: false,
                    }),
                    Some((latest_window, view_size, display_tx, display_wake)),
                )
            }
            None => {
                let (startup, display) = view.create_display(
                    options.gpu_software,
                    ui.wake(),
                    Some(options.gpu_health_dir()),
                )?;
                worker.display = Some(startup);
                ui.display(display);
                (None, None)
            }
        };
        #[cfg(target_os = "macos")]
        let damage_hud;
        #[cfg(not(target_os = "android"))]
        {
            let (startup, display) = view.create_display(
                options.gpu_software,
                ui.wake(),
                Some(options.gpu_health_dir()),
            )?;
            #[cfg(target_os = "macos")]
            {
                damage_hud = startup.debug_settings().damage_hud.clone();
            }
            worker.display = Some(startup);
            ui.display(display);
            #[cfg(target_os = "macos")]
            ui.install_sinks(&mut projection.borrow_mut());
        }
        #[cfg(target_os = "macos")]
        {
            worker.vsync = Some(view.vsync());
        }
        #[cfg(mobile)]
        {
            worker.vsync = Some(view.vsync.clone());
        }
        let service = background.start().map_err(|e| e.to_string())?;
        let completion_wake = ui.wake();
        view.watch(completion_wake.clone());
        let (mut tx, rx) = spsc::channel(1);
        #[cfg(mobile)]
        let view_keys = keys.clone();
        let options = Arc::new(options);
        let execution_options = options.clone();
        let execute = move |environment: Option<(
            &mut adv_runtime::wine::session::Host,
            &std::path::Path,
        )>| {
            let started = Instant::now();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                #[cfg(target_os = "android")]
                if let Some((latest_window, view_size, display_tx, display_wake)) = android_display
                {
                    if worker.stopped() {
                        return Ok(Finished {
                            exit_code: 0,
                            summary: Default::default(),
                            registry: None,
                        });
                    }
                    let (startup, mut display_ui, built_window) = prepare_android_display(
                        &execution_options,
                        &latest_window,
                        view_size,
                        display_wake.clone(),
                    )
                    .map_err(|e| RunError::from_message(e, started.elapsed()))?;
                    if built_window.is_none() {
                        // Start suspended until the UI thread delivers a live surface.
                        unsafe {
                            display_ui
                                .replace_window(None)
                                .map_err(|e| RunError::from_message(e, started.elapsed()))?;
                        }
                    }
                    worker.display = Some(startup);
                    let _ = display_tx.send((display_ui, built_window));
                    display_wake.signal();
                    if worker.stopped() {
                        return Ok(Finished {
                            exit_code: 0,
                            summary: Default::default(),
                            registry: None,
                        });
                    }
                }
                match environment {
                    Some((environment, dist)) => crate::wine::run_execution(
                        environment,
                        &execution_options,
                        dist,
                        worker,
                        wake,
                        keys,
                        execution_sender,
                    ),
                    None => crate::run_execution(
                        &execution_options,
                        worker,
                        wake,
                        keys,
                        execution_sender,
                    ),
                }
            }))
            .unwrap_or_else(|_| {
                Err(RunError::from_message(
                    "execution worker panicked",
                    started.elapsed(),
                ))
            });
            let _ = tx.push(result);
            completion_wake.signal();
        };
        let execution = match execution {
            Some(execution) => {
                execution.submit(move |environment, dist| execute(Some((environment, dist))))?
            }
            None => crate::execution::Task::Thread(
                std::thread::Builder::new()
                    .name("adv-execution".into())
                    .spawn(move || execute(None))
                    .map_err(|e| e.to_string())?,
            ),
        };
        let driver = Rc::new(Driver {
            options,
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
            #[cfg(target_os = "android")]
            display_init: RefCell::new(display_init),
            #[cfg(target_os = "macos")]
            damage_hud,
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
                        if let Some(init) = driver.display_init.borrow().as_ref() {
                            *init.latest_window.lock().unwrap() =
                                window.map(|w| unsafe { OwnedNativeWindow::acquire(w) });
                        }
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
                    driver.refreshed();
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
        #[cfg(target_os = "android")]
        if let Some(init) = self.driver.display_init.borrow_mut().as_mut() {
            init.upscaler = Some(value);
        }
        if let Some(ui) = self.driver.ui.borrow_mut().as_mut() {
            ui.set_upscaler(value);
        }
    }
    /// Set the presentation and input coordinate scaling together.
    pub fn set_scaling(&self, value: crate::Scaling) {
        self.driver.view.hub().scaling.set(value);
        #[cfg(target_os = "android")]
        if let Some(init) = self.driver.display_init.borrow_mut().as_mut() {
            init.scaling = Some(value);
        }
        if let Some(ui) = self.driver.ui.borrow_mut().as_mut() {
            ui.set_scaling(value);
        }
    }
    pub fn set_fast_forward_toggle(&self, toggle: bool) {
        self.driver.view.set_fast_forward_toggle(toggle);
    }
    /// Presses or releases a Win32 virtual key from a synthetic source (e.g. gamepad).
    pub fn synthetic_key(&self, code: u32, down: bool) {
        self.driver.view.overlay_key(code, down);
    }
    /// Presses or releases left (`0`), right (`1`), or middle (`2`) mouse button synthetically.
    pub fn synthetic_mouse_button(&self, button: u8, down: bool) {
        self.driver.view.mouse_button_code(button, down);
    }
    /// Turns the scroll wheel synthetically at the cursor position.
    pub fn synthetic_wheel(&self, notches: i32) {
        self.driver.view.wheel(notches);
    }
    /// Moves the cursor by a delta in canvas pixels.
    pub fn synthetic_cursor_delta(&self, dx: f32, dy: f32) {
        self.driver.view.move_cursor_delta(dx, dy);
    }
    pub fn fps(&self) -> f64 {
        self.driver.ui.borrow().as_ref().map_or(0.0, |ui| ui.fps())
    }
    #[cfg(target_os = "macos")]
    pub fn observe_damage(
        &self,
        observer: impl Fn(Option<[Option<f64>; 2]>) + Send + Sync + 'static,
    ) {
        self.driver.damage_hud.observe(observer);
    }
    /// Calls `observer` once the guest first shows a frame or a dialog: a
    /// script that never does needs no game view.
    pub fn observe_shown(&self, observer: impl FnOnce() + 'static) {
        if let Some(ui) = self.driver.ui.borrow_mut().as_mut() {
            ui.observe_shown(Box::new(observer));
        }
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
    /// Puts the disc in `image` in drive `letter`; the result arrives once the
    /// game's thread has changed it.
    pub fn change_disc(
        &self,
        letter: char,
        image: std::path::PathBuf,
    ) -> std::sync::mpsc::Receiver<Result<(), String>> {
        let (reply, result) = std::sync::mpsc::sync_channel(1);
        self.driver
            .view
            .hub()
            .push(Event::Disc(letter, image, reply));
        result
    }

    /// Requests a live read-only report snapshot from the execution thread
    /// along with the current host display and input settings.
    pub fn report_snapshot(
        &self,
    ) -> (
        std::sync::mpsc::Receiver<adv_report::RunSummary>,
        adv_report::Settings,
    ) {
        let display = self
            .driver
            .ui
            .borrow()
            .as_ref()
            .and_then(frontend::Ui::report_settings);
        let fast_forward = if self.driver.view.fast_forward_toggle() {
            adv_report::FastForward::Toggle
        } else {
            adv_report::FastForward::Hold
        };
        let settings = crate::report_settings(&self.driver.options, display, Some(fast_forward));
        let (reply, result) = std::sync::mpsc::sync_channel(1);
        self.driver.view.hub().push(Event::ReportSnapshot(reply));
        (result, settings)
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
    /// The system asked for memory back, or the app went to the background.
    #[cfg(mobile)]
    pub fn trim_memory(&self) {
        #[cfg(target_os = "android")]
        if let Some(init) = self.driver.display_init.borrow_mut().as_mut() {
            init.trim = true;
        }
        if let Some(ui) = self.driver.ui.borrow_mut().as_mut() {
            ui.trim_memory();
        }
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
    #[cfg(target_os = "android")]
    fn install_pending_display(&self) -> Result<(), String> {
        let ready = {
            let mut init = self.display_init.borrow_mut();
            let Some(state) = init.as_mut() else {
                return Ok(());
            };
            match state.ready.try_recv() {
                Ok(pair) => {
                    let state = init.take().unwrap();
                    Some((pair, state))
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => None,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    init.take();
                    None
                }
            }
        };
        let Some(((mut display_ui, built_window), state)) = ready else {
            return Ok(());
        };
        let current = state
            .latest_window
            .lock()
            .unwrap()
            .as_ref()
            .map(|w| unsafe { OwnedNativeWindow::acquire(w.as_non_null()) });
        let current_addr = current.as_ref().map(OwnedNativeWindow::addr);
        if current_addr != built_window {
            unsafe {
                display_ui.replace_window(current.as_ref().map(OwnedNativeWindow::as_non_null))?;
            }
        }
        drop(current);
        if let Some(upscaler) = state.upscaler {
            display_ui.set_upscaler(upscaler);
        }
        if let Some(scaling) = state.scaling {
            display_ui.set_scaling(scaling);
        }
        if state.trim {
            display_ui.trim();
        }
        if let Some(ui) = self.ui.borrow_mut().as_mut() {
            ui.display(display_ui);
        }
        Ok(())
    }
    fn tick(&self) -> Result<Option<Instant>, String> {
        #[cfg(target_os = "android")]
        self.install_pending_display()?;
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
    fn finish(&self, mut result: Result<Finished, RunError>) {
        #[cfg(target_os = "android")]
        let _ = self.install_pending_display();
        let display = self
            .ui
            .borrow()
            .as_ref()
            .and_then(frontend::Ui::report_settings);
        let fast_forward = if self.view.fast_forward_toggle() {
            adv_report::FastForward::Toggle
        } else {
            adv_report::FastForward::Hold
        };
        let settings = crate::report_settings(&self.options, display, Some(fast_forward));
        match &mut result {
            Ok(finished) => finished.summary.settings = Some(settings),
            Err(error) => error.summary.settings = Some(settings),
        }
        drop(self.ui.borrow_mut().take());
        // Retires guest menus and dialogs unless a nested loop still holds them.
        #[cfg(target_os = "macos")]
        drop(self.projection.borrow_mut().take());
        if let Some(execution) = self.execution.borrow_mut().take()
            && execution.join().is_err()
        {
            result = Err(RunError::from("execution worker panicked".to_owned()));
        }
        self.service.borrow_mut().shutdown();
        if let Some(callback) = self.on_finished.borrow_mut().take() {
            callback(result);
        }
    }
    fn refreshed(&self) {
        if let Some(ui) = self.ui.borrow().as_ref() {
            ui.refreshed();
        }
    }
    fn drive(&self) {
        let next = match self.tick() {
            Ok(next) => next,
            Err(error) => {
                self.pending
                    .borrow_mut()
                    .get_or_insert_with(|| Err(RunError::from(error)));
                self.finish_pending();
                None
            }
        };
        #[cfg(mobile)]
        {
            let result = self.main_loop.borrow().as_ref().map(|main_loop| {
                #[cfg(target_os = "ios")]
                {
                    main_loop.arm(next, self.view.frame_clock.replace(false))
                }
                #[cfg(target_os = "android")]
                {
                    main_loop.arm(next, self.view.frame_clock.replace(false))
                }
            });
            if let Some(Err(error)) = result {
                self.finish(Err(RunError::from(error)));
            }
        }
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
        Err(error) => driver.finish(Err(RunError::from(error.to_string()))),
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
                .started()
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or_default()
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
        None,
        projection,
    )?;
    *session.driver.cli.borrow_mut() = Some(window);
    NSApplication::sharedApplication(MainThreadMarker::new().unwrap()).run();
    drop(session);
    result
        .borrow_mut()
        .take()
        .unwrap_or_else(|| {
            Err(RunError::from(
                "application run loop stopped before the game finished".to_owned(),
            ))
        })
        .map(|finished| finished.exit_code)
        .map_err(|error| error.message)
}
