use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

#[cfg(windows)]
mod os {
    use super::*;
    use std::ffi::c_void;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateEventW(
            attributes: *const c_void,
            manual: i32,
            initial: i32,
            name: *const u16,
        ) -> *mut c_void;
        fn SetEvent(event: *mut c_void) -> i32;
        fn WaitForSingleObject(event: *mut c_void, milliseconds: u32) -> u32;
        fn WaitForMultipleObjects(
            count: u32,
            handles: *const *mut c_void,
            all: i32,
            milliseconds: u32,
        ) -> u32;
        fn CreateWaitableTimerExW(
            attributes: *const c_void,
            name: *const u16,
            flags: u32,
            access: u32,
        ) -> *mut c_void;
        fn SetWaitableTimerEx(
            timer: *mut c_void,
            due: *const i64,
            period: i32,
            callback: *const c_void,
            argument: *const c_void,
            context: *const c_void,
            tolerance: u32,
        ) -> i32;
        fn CancelWaitableTimer(timer: *mut c_void) -> i32;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }
    // One timer per waiting thread, not per cloned event. Concurrent waiters
    // cannot rearm each other's deadlines. Non-alertable waits never call user
    // code while the timer is armed. No global timer-resolution request is made.
    struct DeadlineTimer(*mut c_void);
    impl DeadlineTimer {
        fn new() -> Option<Self> {
            const HIGH_RESOLUTION: u32 = 2;
            const SYNCHRONIZE_AND_MODIFY: u32 = 0x0010_0002;
            let timer = unsafe {
                CreateWaitableTimerExW(
                    std::ptr::null(),
                    std::ptr::null(),
                    HIGH_RESOLUTION,
                    SYNCHRONIZE_AND_MODIFY,
                )
            };
            if timer.is_null() {
                None
            } else {
                Some(Self(timer))
            }
        }
        fn wait(&self, event: *mut c_void, timeout: Duration) -> std::io::Result<bool> {
            let ticks = timeout.as_nanos().div_ceil(100).clamp(1, i64::MAX as u128);
            let due = -(ticks as i64);
            // Negative 100 ns units specify a relative one-shot deadline. Windows
            // copies due synchronously; no callback or pointer is retained.
            if unsafe {
                SetWaitableTimerEx(
                    self.0,
                    &due,
                    0,
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                    0,
                )
            } == 0
            {
                return Err(std::io::Error::last_os_error());
            }
            let handles = [event, self.0];
            // An already-pending channel event wins even when both are signaled.
            let result = match unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, u32::MAX) } {
                0 => Ok(true),
                1 => Ok(false),
                _ => Err(std::io::Error::last_os_error()),
            };
            // Cancel an unexpired deadline after an early channel wake. The next
            // SetWaitableTimerEx also resets any already-signaled timer state.
            unsafe {
                CancelWaitableTimer(self.0);
            }
            result
        }
    }
    impl Drop for DeadlineTimer {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
    thread_local! { static DEADLINE_TIMER: Option<DeadlineTimer> = DeadlineTimer::new(); }
    pub struct Handle(*mut c_void);
    // Windows events allow concurrent SetEvent and waiting. Arc pins the handle.
    unsafe impl Send for Handle {}
    unsafe impl Sync for Handle {}
    impl Handle {
        pub fn new() -> std::io::Result<Self> {
            let event = unsafe { CreateEventW(std::ptr::null(), 0, 0, std::ptr::null()) };
            if event.is_null() {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(Self(event))
            }
        }
        pub fn signal(&self) {
            unsafe {
                SetEvent(self.0);
            }
        }
        pub fn wait(&self, timeout: Option<Duration>) -> std::io::Result<bool> {
            if let Some(duration) = timeout.filter(|duration| !duration.is_zero())
                && let Ok(Some(result)) = DEADLINE_TIMER
                    .try_with(|timer| timer.as_ref().map(|timer| timer.wait(self.0, duration)))
            {
                return result;
            }
            // Unbounded waits and zero-duration polls need no timer. Older
            // Windows versions without high-resolution timers retain the event
            // timeout fallback and the same signal-before-wait behavior.
            let ms = timeout.map_or(u32::MAX, |d| {
                d.as_nanos()
                    .div_ceil(1_000_000)
                    .min(u128::from(u32::MAX - 1)) as u32
            });
            match unsafe { WaitForSingleObject(self.0, ms) } {
                0 => Ok(true),
                258 => Ok(false),
                _ => Err(std::io::Error::last_os_error()),
            }
        }
        pub fn raw(&self) -> *mut c_void {
            self.0
        }
    }
    impl Drop for Handle {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}
#[cfg(unix)]
mod os {
    use super::*;
    pub struct Handle {
        read: i32,
        write: i32,
    }
    impl Handle {
        pub fn new() -> std::io::Result<Self> {
            let mut fds = [-1; 2];
            if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
                return Err(std::io::Error::last_os_error());
            }
            let handle = Self {
                read: fds[0],
                write: fds[1],
            };
            for fd in fds {
                if unsafe { libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK) } < 0
                    || unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } < 0
                {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(handle)
        }
        pub fn signal(&self) {
            unsafe {
                libc::write(self.write, [1u8].as_ptr().cast(), 1);
            }
        }
        pub fn wait(&self, timeout: Option<Duration>) -> std::io::Result<bool> {
            let started = std::time::Instant::now();
            loop {
                let remaining = timeout.map(|d| d.saturating_sub(started.elapsed()));
                let mut fd = libc::pollfd {
                    fd: self.read,
                    events: libc::POLLIN,
                    revents: 0,
                };
                // Linux/Android take a nanosecond deadline: poll's millisecond
                // timeout rounds every short frame wait up by up to 1 ms. Keep
                // the pipe in the wait set: input/audio/compiler wakes must
                // interrupt even an indefinite deadline. EINTR retries use the
                // original monotonic start, never a fresh timeout. A null mask
                // leaves CPU fault/safepoint signal masks intact.
                #[cfg(any(target_os = "linux", target_os = "android"))]
                let result = {
                    let timespec = remaining.map(|d| libc::timespec {
                        tv_sec: d.as_secs().min(libc::time_t::MAX as u64) as libc::time_t,
                        tv_nsec: d.subsec_nanos().into(),
                    });
                    unsafe {
                        libc::ppoll(
                            &mut fd,
                            1,
                            timespec.as_ref().map_or(std::ptr::null(), |t| t),
                            std::ptr::null(),
                        )
                    }
                };
                #[cfg(not(any(target_os = "linux", target_os = "android")))]
                let result = {
                    let ms = remaining.map_or(-1, |d| {
                        d.as_nanos().div_ceil(1_000_000).min(i32::MAX as u128) as i32
                    });
                    unsafe { libc::poll(&mut fd, 1, ms) }
                };
                if result == 0 {
                    return Ok(false);
                }
                if result < 0 {
                    let error = std::io::Error::last_os_error();
                    if error.kind() == std::io::ErrorKind::Interrupted {
                        continue;
                    }
                    return Err(error);
                }
                let mut bytes = [0u8; 256];
                unsafe {
                    libc::read(self.read, bytes.as_mut_ptr().cast(), bytes.len());
                }
                return Ok(true);
            }
        }
    }
    impl Drop for Handle {
        fn drop(&mut self) {
            unsafe {
                libc::close(self.read);
                libc::close(self.write);
            }
        }
    }
}

/// A retained wake: signaling immediately before wait cannot be lost. All
/// execution-side producers clone this same event. There is one execution waiter.
#[derive(Clone)]
pub struct WakeEvent {
    handle: Arc<os::Handle>,
    interrupt: Arc<AtomicBool>,
}
impl WakeEvent {
    pub fn new() -> std::io::Result<Self> {
        Ok(Self {
            handle: Arc::new(os::Handle::new()?),
            interrupt: Arc::new(AtomicBool::new(false)),
        })
    }
    pub fn signal(&self) {
        self.interrupt.store(true, Ordering::Release);
        self.notify();
    }
    /// Retain an OS wake without requesting a CPU safepoint exit. Read-only
    /// diagnostics use this so they cannot overtake an input's guest time slice.
    /// A previously requested interrupt remains pending.
    pub fn notify(&self) {
        self.handle.signal();
    }
    /// Submillisecond deadlines on Windows (high-resolution timer) and
    /// Linux/Android (ppoll); other Unix hosts round up to poll's milliseconds.
    /// No spinning, timer-resolution changes or guest-clock advancement.
    pub fn wait(&self, timeout: Option<Duration>) -> std::io::Result<bool> {
        self.handle.wait(timeout)
    }
    /// Native/interpreted backedges may read this flag without reading a clock.
    pub fn interrupt_flag(&self) -> Arc<AtomicBool> {
        self.interrupt.clone()
    }
    /// Call before draining channels, never after: a concurrent new delivery
    /// must keep its interrupt until the next drain, including during guest work.
    pub fn take_interrupt(&self) -> bool {
        self.interrupt.swap(false, Ordering::AcqRel)
    }
    #[cfg(windows)]
    pub fn as_raw_handle(&self) -> *mut std::ffi::c_void {
        self.handle.raw()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn precise_deadline_preserves_notifications_and_never_expires_early() {
        let wake = WakeEvent::new().unwrap();
        wake.notify();
        assert!(wake.wait(Some(Duration::ZERO)).unwrap());
        assert!(!wake.wait(Some(Duration::ZERO)).unwrap());
        for duration in [Duration::from_micros(250), Duration::from_micros(1667)] {
            let start = std::time::Instant::now();
            assert!(!wake.wait(Some(duration)).unwrap());
            assert!(start.elapsed() >= duration);
        }
        let signal = wake.clone();
        let producer = std::thread::spawn(move || signal.signal());
        assert!(wake.wait(None).unwrap());
        producer.join().unwrap();
        assert!(wake.take_interrupt());
        assert!(!wake.wait(Some(Duration::ZERO)).unwrap());
    }
    #[test]
    fn wake_before_wait_is_retained_and_cross_thread_signal_interrupts_wait() {
        let wake = WakeEvent::new().unwrap();
        wake.signal();
        assert!(wake.interrupt_flag().load(Ordering::Acquire));
        assert!(wake.take_interrupt());
        assert!(!wake.take_interrupt());
        assert!(wake.wait(Some(Duration::ZERO)).unwrap());
        assert!(!wake.wait(Some(Duration::ZERO)).unwrap());
        let signal = wake.clone();
        let producer = std::thread::spawn(move || signal.signal());
        assert!(wake.wait(Some(Duration::from_secs(2))).unwrap());
        producer.join().unwrap();
    }
    #[test]
    fn notification_wakes_a_waiter_without_creating_or_clearing_interrupts() {
        let wake = WakeEvent::new().unwrap();
        wake.notify();
        assert!(wake.wait(Some(Duration::ZERO)).unwrap());
        assert!(!wake.interrupt_flag().load(Ordering::Acquire));
        let notify = wake.clone();
        let producer = std::thread::spawn(move || notify.notify());
        assert!(wake.wait(Some(Duration::from_secs(2))).unwrap());
        producer.join().unwrap();
        assert!(!wake.take_interrupt());
        wake.signal();
        wake.notify();
        assert!(wake.wait(Some(Duration::ZERO)).unwrap());
        assert!(wake.take_interrupt());
        assert!(!wake.wait(Some(Duration::ZERO)).unwrap());
    }

    #[test]
    fn deadline_expiry_and_early_wake_do_not_leave_a_stale_timer() {
        let wake = WakeEvent::new().unwrap();
        assert!(!wake.wait(Some(Duration::from_millis(1))).unwrap());
        assert!(!wake.take_interrupt());
        wake.notify();
        assert!(wake.wait(Some(Duration::from_secs(2))).unwrap());
        assert!(!wake.wait(Some(Duration::ZERO)).unwrap());
        assert!(!wake.wait(Some(Duration::from_millis(1))).unwrap());
        wake.signal();
        assert!(wake.wait(None).unwrap());
        assert!(wake.take_interrupt());
    }

    #[test]
    fn independent_waiting_threads_have_independent_deadlines() {
        let workers: Vec<_> = (0..4)
            .map(|_| {
                std::thread::spawn(|| {
                    let wake = WakeEvent::new().unwrap();
                    for _ in 0..8 {
                        assert!(!wake.wait(Some(Duration::from_millis(1))).unwrap());
                        wake.notify();
                        assert!(wake.wait(Some(Duration::from_millis(20))).unwrap());
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
    }
}
