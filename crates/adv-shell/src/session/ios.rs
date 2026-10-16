//! The iOS main loop: the wake descriptor and a deadline timer on the main
//! CFRunLoop, and a display link that runs due presentations on a vsync and
//! publishes the vsync grid for guest pacing (PROC-CLOCK-4, DEV-FRAME-9).
use super::Driver;
use objc2::rc::Retained;
use objc2::runtime::NSObject;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_core_foundation::{
    CFAbsoluteTimeGetCurrent, CFFileDescriptor, CFFileDescriptorContext, CFOptionFlags, CFRetained,
    CFRunLoop, CFRunLoopSource, CFRunLoopTimer, CFRunLoopTimerContext,
    kCFFileDescriptorReadCallBack, kCFRunLoopCommonModes,
};
use objc2_foundation::{NSObjectProtocol, NSRunLoop, NSRunLoopCommonModes};
use objc2_quartz_core::{CADisplayLink, CAFrameRateRange};
use std::cell::Cell;
use std::ffi::c_void;
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

pub struct Target {
    driver: Weak<Driver>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = Target]
    pub struct FrameTarget;
    unsafe impl NSObjectProtocol for FrameTarget {}
    impl FrameTarget {
        #[unsafe(method(frame:))]
        fn frame(&self, link: &CADisplayLink) {
            link.setPaused(true);
            let Some(driver) = self.ivars().driver.upgrade() else {
                return;
            };
            if let Some(main_loop) = driver.main_loop.borrow().as_ref() {
                main_loop.pending.set(false);
            }
            let period = link.targetTimestamp() - link.timestamp();
            if period > 0.0 {
                let now = Instant::now();
                let ago = (objc2_quartz_core::CACurrentMediaTime() - link.timestamp()).max(0.0);
                let at = now.checked_sub(Duration::from_secs_f64(ago)).unwrap_or(now);
                driver.view.vsync.publish(at, Duration::from_secs_f64(period));
                driver.view.period.set((period * 1e9) as i64);
            }
            driver.view.ready.set(true);
            driver.drive();
            driver.view.ready.set(false);
        }
    }
);

pub(super) struct MainLoop {
    descriptor: CFRetained<CFFileDescriptor>,
    source: CFRetained<CFRunLoopSource>,
    timer: CFRetained<CFRunLoopTimer>,
    link: Retained<CADisplayLink>,
    pending: Cell<bool>,
}

impl MainLoop {
    pub(super) fn new(driver: &Rc<Driver>) -> Result<Self, String> {
        let mtm = MainThreadMarker::new().ok_or("the session must run on the main thread")?;
        // The Session retains Driver and invalidates every source before dropping it.
        let info = Rc::as_ptr(driver) as *mut c_void;
        let context = CFFileDescriptorContext {
            version: 0,
            info,
            retain: None,
            release: None,
            copyDescription: None,
        };
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
        let run_loop = CFRunLoop::main().ok_or("no main run loop")?;
        run_loop.add_source(Some(&source), unsafe { kCFRunLoopCommonModes });
        run_loop.add_timer(Some(&timer), unsafe { kCFRunLoopCommonModes });
        descriptor.enable_call_backs(kCFFileDescriptorReadCallBack);
        let target = FrameTarget::alloc(mtm).set_ivars(Target {
            driver: Rc::downgrade(driver),
        });
        let target: Retained<FrameTarget> = unsafe { msg_send![super(target), init] };
        // The link retains its target until `invalidate`.
        let link = unsafe { CADisplayLink::displayLinkWithTarget_selector(&target, sel!(frame:)) };
        link.setPaused(true);
        // The guest is told the display runs at 60 Hz (`refresh_interval`).
        link.setPreferredFrameRateRange(CAFrameRateRange {
            minimum: 30.0,
            maximum: 60.0,
            preferred: 60.0,
        });
        unsafe { link.addToRunLoop_forMode(&NSRunLoop::mainRunLoop(), NSRunLoopCommonModes) };
        driver.wake.signal();
        Ok(Self {
            descriptor,
            source,
            timer,
            link,
            pending: Cell::new(false),
        })
    }

    /// A due presentation waits for the next vsync; anything later sleeps on
    /// the timer.
    pub(super) fn arm(&self, next: Option<Instant>) -> Result<(), String> {
        let mut date = f64::MAX;
        if crate::frontend::request_vsync(next, self.pending.get()) {
            self.pending.set(true);
            self.link.setPaused(false);
        } else if let Some(next) = next
            && !self.pending.get()
        {
            date = CFAbsoluteTimeGetCurrent()
                + next.saturating_duration_since(Instant::now()).as_secs_f64();
        }
        if self.timer.is_valid() {
            self.timer.set_next_fire_date(date);
        }
        Ok(())
    }
}

impl Drop for MainLoop {
    fn drop(&mut self) {
        self.link.invalidate();
        self.timer.invalidate();
        self.descriptor.invalidate();
        self.source.invalidate();
    }
}

// A completion callback may drop its Session. Keep Driver alive until this
// invocation returns.
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
