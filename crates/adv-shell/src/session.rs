//! A game session driven by the host's main AppKit run loop.
use crate::{
    Finished, Options, frontend,
    window::{Event, GameView, Projection, Window},
};
use adv_runtime::{DialogEvent, DialogSet, MenuBar, MenuEvent, MenuItem};
use adv_sync::{WakeEvent, input::KeyState, spsc};
use objc2::{MainThreadMarker, rc::Retained, runtime::ProtocolObject};
use objc2_app_kit::NSApplication;
use objc2_core_foundation::{
    CFAbsoluteTimeGetCurrent, CFFileDescriptor, CFFileDescriptorContext, CFOptionFlags, CFRetained,
    CFRunLoop, CFRunLoopSource, CFRunLoopTimer, CFRunLoopTimerContext,
    kCFFileDescriptorReadCallBack, kCFRunLoopCommonModes,
};
use objc2_foundation::{NSActivityOptions, NSObjectProtocol, NSProcessInfo, NSString};
use std::{
    cell::RefCell,
    ffi::c_void,
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

pub struct Session {
    driver: Rc<Driver>,
    descriptor: CFRetained<CFFileDescriptor>,
    source: CFRetained<CFRunLoopSource>,
    timer: CFRetained<CFRunLoopTimer>,
}
struct Driver {
    activity: Retained<ProtocolObject<dyn NSObjectProtocol>>,
    view: Retained<GameView>,
    ui: RefCell<Option<frontend::Ui>>,
    done: RefCell<spsc::Consumer<Result<Finished, String>>>,
    pending: RefCell<Option<Result<Finished, String>>>,
    execution: RefCell<Option<std::thread::JoinHandle<()>>>,
    service: RefCell<adv_sync::background::Service>,
    on_finished: RefCell<Option<Box<dyn FnOnce(Result<Finished, String>)>>>,
    timer: RefCell<Option<CFRetained<CFRunLoopTimer>>>,
    wake: WakeEvent,
    /// Guest menus and dialogs; borrowed across a nested popup loop.
    projection: RefCell<Option<Rc<RefCell<Projection>>>>,
    cli: RefCell<Option<Rc<RefCell<Window>>>>,
}
impl Session {
    /// Starts a game in `view`. Guest menus join the application's main menu
    /// after its first item and guest dialogs open over the view's window.
    pub fn new(
        view: Retained<GameView>,
        options: Options,
        on_finished: Box<dyn FnOnce(Result<Finished, String>)>,
    ) -> Result<Self, String> {
        let projection = Rc::new(RefCell::new(Projection::embedded(view.clone())?));
        Self::start(view, options, on_finished, projection)
    }
    fn start(
        view: Retained<GameView>,
        options: Options,
        on_finished: Box<dyn FnOnce(Result<Finished, String>)>,
        projection: Rc<RefCell<Projection>>,
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
        ui.install_sinks(&mut projection.borrow_mut());
        worker.gpu = Some(gpu);
        let service = background.start().map_err(|e| e.to_string())?;
        let completion_wake = ui.wake();
        view.watch(completion_wake.clone());
        let (mut tx, rx) = spsc::channel(1);
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
            timer: RefCell::new(None),
            projection: RefCell::new(Some(projection)),
            cli: RefCell::new(None),
        });
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
        Ok(Self {
            driver,
            descriptor,
            source,
            timer,
        })
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
        self.driver.view.hub().activate(active);
    }
    /// The window showing the game lost key status. A guest dialog taking it
    /// keeps the guest's focus; another window removes it.
    pub fn resign_key(&self) {
        self.driver.view.hub().resign_key();
    }
    pub fn set_minimized(&self, minimized: bool) {
        self.driver.view.hub().minimized(minimized);
    }
    pub fn set_occluded(&self, occluded: bool) {
        self.driver.view.hub().set_occluded(occluded);
    }
    pub fn sleep(&self, sleeping: bool) {
        self.driver.view.hub().sleep(sleeping);
    }
    /// Pause or resume the game, as the launcher's pause menu does. A close
    /// request resumes it so the guest can answer.
    pub fn set_paused(&self, paused: bool) {
        self.driver.view.hub().pause(paused);
    }
    pub fn request_close(&self) {
        self.driver.view.hub().request_close();
    }
    pub fn force_close(&self) {
        self.driver.view.hub().force_close();
    }
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
    pub fn dialog_event(&self, event: DialogEvent) {
        self.driver.view.hub().push(Event::Dialog(event));
    }
}
impl Driver {
    fn tick(&self) -> Result<Option<Instant>, String> {
        let projection = self.projection.borrow().clone();
        if self.finish_pending() {
            return Ok(None);
        }
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
        drop(self.projection.borrow_mut().take());
        if let Some(execution) = self.execution.borrow_mut().take() {
            if execution.join().is_err() {
                result = Err("execution worker panicked".into());
            }
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
        if let Some(timer) = self.timer.borrow().as_ref() {
            if timer.is_valid() {
                let date = next.map_or(f64::MAX, |next| {
                    CFAbsoluteTimeGetCurrent()
                        + next.saturating_duration_since(Instant::now()).as_secs_f64()
                });
                timer.set_next_fire_date(date);
            }
        }
    }
}
// A completion callback may drop its Session. Keep Driver alive until this
// invocation returns, including when AppKit is dispatching a nested menu loop.
unsafe fn callback_driver(info: *mut c_void) -> Rc<Driver> {
    let pointer = info as *const Driver;
    unsafe {
        Rc::increment_strong_count(pointer);
        Rc::from_raw(pointer)
    }
}
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
unsafe extern "C-unwind" fn timer_callback(_: *mut CFRunLoopTimer, info: *mut c_void) {
    unsafe { callback_driver(info) }.drive();
}
impl Drop for Session {
    fn drop(&mut self) {
        self.timer.invalidate();
        self.descriptor.invalidate();
        self.source.invalidate();
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
        unsafe {
            NSProcessInfo::processInfo().endActivity(&self.activity);
        }
    }
}

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
