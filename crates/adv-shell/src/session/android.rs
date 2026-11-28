use super::Driver;
use ndk_sys::*;
use std::{
    cell::{Cell, RefCell},
    ffi::c_void,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    ptr::NonNull,
    rc::{Rc, Weak},
    time::{Duration, Instant},
};
type RateHook =
    unsafe extern "C" fn(*mut AChoreographer, AChoreographer_refreshRateCallback, *mut c_void);

fn monotonic_ns() -> i64 {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut time) };
    time.tv_sec * 1_000_000_000 + time.tv_nsec
}

fn instant_from_monotonic(frame_ns: i64) -> Instant {
    let now = Instant::now();
    let now_ns = monotonic_ns();
    if now_ns >= frame_ns {
        now.checked_sub(Duration::from_nanos((now_ns - frame_ns) as u64))
            .unwrap_or(now)
    } else {
        now + Duration::from_nanos((frame_ns - now_ns) as u64)
    }
}

pub(super) struct MainLoop {
    looper: NonNull<ALooper>,
    timer: OwnedFd,
    wake_fd: i32,
    choreographer: *mut AChoreographer,
    pending: Cell<bool>,
    frame_waiting: Cell<bool>,
    probed: Cell<bool>,
    pacer: RefCell<crate::frontend::RefreshPacer>,
    estimator: RefCell<crate::frontend::PeriodEstimator>,
    unregister: Option<RateHook>,
    driver: Weak<Driver>,
}
impl MainLoop {
    pub(super) fn new(driver: &Rc<Driver>) -> Result<Self, String> {
        let looper = NonNull::new(unsafe { ALooper_forThread() })
            .ok_or("the host main thread has no looper")?;
        let fd = unsafe {
            libc::timerfd_create(
                libc::CLOCK_MONOTONIC,
                libc::TFD_CLOEXEC | libc::TFD_NONBLOCK,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let timer = unsafe { OwnedFd::from_raw_fd(fd) };
        let choreographer = unsafe { AChoreographer_getInstance() };
        if choreographer.is_null() {
            return Err("no main-thread Choreographer".into());
        }
        let register = unsafe {
            libc::dlsym(
                libc::RTLD_DEFAULT,
                c"AChoreographer_registerRefreshRateCallback".as_ptr(),
            )
        };
        let unregister = unsafe {
            libc::dlsym(
                libc::RTLD_DEFAULT,
                c"AChoreographer_unregisterRefreshRateCallback".as_ptr(),
            )
        };
        let main_loop = Self {
            looper,
            timer,
            wake_fd: driver.wake.as_raw_fd(),
            choreographer,
            pending: Cell::new(false),
            frame_waiting: Cell::new(false),
            probed: Cell::new(false),
            pacer: RefCell::default(),
            estimator: RefCell::default(),
            unregister: (!register.is_null() && !unregister.is_null())
                .then(|| unsafe { std::mem::transmute::<*mut c_void, RateHook>(unregister) }),
            driver: Rc::downgrade(driver),
        };
        unsafe {
            ALooper_acquire(looper.as_ptr());
        }
        let data = Rc::as_ptr(driver) as *mut c_void;
        for fd in [main_loop.wake_fd, main_loop.timer.as_raw_fd()] {
            if unsafe {
                ALooper_addFd(
                    looper.as_ptr(),
                    fd,
                    ALOOPER_POLL_CALLBACK,
                    ALOOPER_EVENT_INPUT as i32,
                    Some(fd_callback),
                    data,
                )
            } != 1
            {
                return Err("ALooper_addFd failed".into());
            }
        }
        if main_loop.unregister.is_some() {
            let register: RateHook = unsafe { std::mem::transmute(register) };
            unsafe {
                register(choreographer, Some(rate_callback), data);
            }
        }
        driver.wake.signal();
        Ok(main_loop)
    }
    fn post_frame(&self) {
        self.pending.set(true);
        self.estimator.borrow_mut().on_post(monotonic_ns());
        // One Weak token survives teardown until the uncancellable callback.
        let data = Box::into_raw(Box::new(self.driver.clone())).cast();
        unsafe {
            AChoreographer_postFrameCallback64(self.choreographer, Some(frame_callback), data);
        }
    }
    pub(super) fn arm(&self, next: Option<Instant>, frame: bool) -> Result<(), String> {
        let mut spec: libc::itimerspec = unsafe { std::mem::zeroed() };
        if frame {
            self.frame_waiting.set(true);
        }
        if crate::frontend::request_vsync(frame.then(Instant::now), self.pending.get()) {
            self.post_frame();
        }
        if let Some(next) = next {
            let delay = next.saturating_duration_since(Instant::now());
            spec.it_value.tv_sec = delay.as_secs() as libc::time_t;
            spec.it_value.tv_nsec =
                delay.subsec_nanos().max(u32::from(delay.as_secs() == 0)) as libc::c_long;
        }
        if unsafe { libc::timerfd_settime(self.timer.as_raw_fd(), 0, &spec, std::ptr::null_mut()) }
            != 0
        {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(())
    }
}
unsafe extern "C" fn fd_callback(fd: i32, _: i32, data: *mut c_void) -> i32 {
    let pointer = data as *const Driver;
    let driver = unsafe {
        Rc::increment_strong_count(pointer);
        Rc::from_raw(pointer)
    };
    if fd == driver.wake.as_raw_fd() {
        if let Err(error) = driver.wake.wait(Some(std::time::Duration::ZERO)) {
            driver.finish(Err(error.to_string()));
            return 1;
        }
    } else {
        let mut count = 0u64;
        unsafe {
            libc::read(fd, (&mut count as *mut u64).cast(), 8);
        }
    }
    driver.drive();
    1
}
unsafe extern "C" fn frame_callback(frame_ns: i64, data: *mut c_void) {
    let weak = unsafe { Box::from_raw(data as *mut Weak<Driver>) };
    if let Some(driver) = weak.upgrade() {
        let counted = driver.main_loop.borrow().as_ref().is_some_and(|main_loop| {
            main_loop.pending.set(false);
            let period = {
                let mut estimator = main_loop.estimator.borrow_mut();
                let reported = driver.view.period.get();
                if main_loop.unregister.is_some() && reported > 0 {
                    estimator.set_reported(reported);
                }
                let period = estimator.on_frame(frame_ns).unwrap_or(0);
                if main_loop.unregister.is_none() && period > 0 {
                    driver.view.period.set(period);
                }
                period
            };
            if period > 0 {
                driver.view.vsync.publish(
                    instant_from_monotonic(frame_ns),
                    Duration::from_nanos(period as u64),
                );
            }
            if !main_loop.frame_waiting.replace(false) {
                return false;
            }
            let counted = main_loop.pacer.borrow_mut().counts(frame_ns, period);
            if !counted {
                // A waiting frame attaches on the next counted refresh.
                main_loop.frame_waiting.set(true);
                main_loop.post_frame();
            } else if main_loop.unregister.is_none()
                && period == 0
                && !main_loop.probed.replace(true)
            {
                // API 29 lacks refresh-rate callbacks: take one adjacent
                // sample after the first active frame to measure the panel.
                main_loop.post_frame();
            }
            counted
        });
        if counted {
            driver.refreshed();
        }
        driver.drive();
    }
}
unsafe extern "C" fn rate_callback(period: i64, data: *mut c_void) {
    if period > 0 {
        let driver = unsafe { &*(data as *const Driver) };
        driver.view.period.set(period);
    }
}
impl Drop for MainLoop {
    fn drop(&mut self) {
        if let Some(driver) = self.driver.upgrade() {
            driver.view.vsync.clear();
        }
        unsafe {
            if let Some(unregister) = self.unregister {
                unregister(
                    self.choreographer,
                    Some(rate_callback),
                    self.driver.as_ptr() as *mut c_void,
                );
            }
            ALooper_removeFd(self.looper.as_ptr(), self.wake_fd);
            ALooper_removeFd(self.looper.as_ptr(), self.timer.as_raw_fd());
            ALooper_release(self.looper.as_ptr());
        }
    }
}
