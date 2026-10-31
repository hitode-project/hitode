use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

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
        pub fn raw(&self) -> i32 {
            self.read
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
    interrupt: Arc<AtomicU32>,
    generation: Arc<AtomicU64>,
}
impl WakeEvent {
    pub fn new() -> std::io::Result<Self> {
        Ok(Self {
            handle: Arc::new(os::Handle::new()?),
            interrupt: Arc::new(AtomicU32::new(0)),
            generation: Arc::new(AtomicU64::new(0)),
        })
    }
    pub fn signal(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.interrupt
            .fetch_or(crate::pending::INTERRUPT, Ordering::Release);
        self.notify();
    }
    /// Execution deliveries since creation, including repeated signals before a drain.
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }
    /// Retain an OS wake without requesting a CPU safepoint exit. Read-only
    /// diagnostics use this so they cannot overtake an input's guest time slice.
    /// A previously requested interrupt remains pending.
    pub fn notify(&self) {
        self.handle.signal();
    }
    /// Submillisecond deadlines on Linux/Android (ppoll); other Unix hosts
    /// round up to poll's milliseconds.
    /// No spinning, timer-resolution changes or guest-clock advancement.
    pub fn wait(&self, timeout: Option<Duration>) -> std::io::Result<bool> {
        self.handle.wait(timeout)
    }
    /// Native/interpreted backedges may read this flag without reading a clock.
    pub fn interrupt_flag(&self) -> Arc<AtomicU32> {
        self.interrupt.clone()
    }
    /// Call before draining channels, never after: a concurrent new delivery
    /// must keep its interrupt until the next drain, including during guest work.
    pub fn take_interrupt(&self) -> bool {
        self.interrupt
            .fetch_and(!crate::pending::INTERRUPT, Ordering::AcqRel)
            & crate::pending::INTERRUPT
            != 0
    }

    /// The pipe's read end, readable while a wake is pending, for a host
    /// event loop to watch. `wait` drains it.
    #[cfg(unix)]
    pub fn as_raw_fd(&self) -> i32 {
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
        assert_ne!(wake.interrupt_flag().load(Ordering::Acquire), 0);
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
        assert_eq!(wake.interrupt_flag().load(Ordering::Acquire), 0);
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
