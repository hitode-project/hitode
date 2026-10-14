use super::Driver;
use ndk_sys::*;
use std::{
    cell::Cell,
    ffi::c_void,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    ptr::NonNull,
    rc::{Rc, Weak},
    time::Instant,
};
type RateHook =
    unsafe extern "C" fn(*mut AChoreographer, AChoreographer_refreshRateCallback, *mut c_void);
pub(super) struct MainLoop {
    looper: NonNull<ALooper>,
    timer: OwnedFd,
    wake_fd: i32,
    choreographer: *mut AChoreographer,
    pending: Cell<bool>,
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
                    ALOOPER_POLL_CALLBACK as i32,
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
    pub(super) fn arm(&self, next: Option<Instant>) -> Result<(), String> {
        let mut spec: libc::itimerspec = unsafe { std::mem::zeroed() };
        if crate::frontend::request_vsync(next, self.pending.get()) {
            self.pending.set(true);
            // One Weak token survives teardown until the uncancellable callback.
            let data = Box::into_raw(Box::new(self.driver.clone())).cast();
            unsafe {
                AChoreographer_postFrameCallback64(self.choreographer, Some(frame_callback), data);
            }
        } else if let Some(next) = next
            && !self.pending.get()
        {
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
unsafe extern "C" fn frame_callback(_: i64, data: *mut c_void) {
    let weak = unsafe { Box::from_raw(data as *mut Weak<Driver>) };
    if let Some(driver) = weak.upgrade() {
        if let Some(main_loop) = driver.main_loop.borrow().as_ref() {
            main_loop.pending.set(false);
        }
        driver.view.ready.set(true);
        driver.drive();
        driver.view.ready.set(false);
    }
}
unsafe extern "C" fn rate_callback(period: i64, data: *mut c_void) {
    let driver = unsafe { &*(data as *const Driver) };
    driver.view.period.set(period);
}
impl Drop for MainLoop {
    fn drop(&mut self) {
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
