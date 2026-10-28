//! In-process sampler of the execution thread. Each sample reads the
//! thread's PC, LR and frame-pointer chain within its own stack, and records
//! it outside the thread, so the sampler never waits on a lock the sampled
//! thread may hold. Apple hosts suspend the thread; Linux and Android hosts
//! interrupt it with a signal while it runs and count it idle while it
//! sleeps. Supported on arm64.
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

pub(crate) const FRAMES: usize = 16;
/// Samples kept: a minute at the default rate.
const CAPACITY: usize = 30_000;
pub(crate) const DEFAULT_HZ: u32 = 500;

#[derive(Clone, Copy)]
pub(crate) struct Sample {
    pub unix_us: u64,
    /// PC, LR, then return addresses up the frame-pointer chain.
    pub frames: [usize; FRAMES],
    /// No frames: the thread was blocked in the kernel.
    pub depth: u8,
}

pub(crate) struct Profiler {
    samples: Mutex<VecDeque<Sample>>,
    /// Samples per second; 0 stops sampling.
    hz: AtomicU32,
    pub(crate) supported: bool,
}

/// Whether a frame record (previous FP, return address) at `fp` lies inside
/// the stack. Generated code keeps guest values in x29, so `fp` may be any
/// value, and the end is computed without overflow.
#[cfg_attr(not(target_arch = "aarch64"), allow(dead_code))]
fn within(fp: usize, bottom: usize, top: usize) -> bool {
    fp.is_multiple_of(8) && fp >= bottom && fp.checked_add(16).is_some_and(|end| end <= top)
}

impl Profiler {
    /// Starts sampling the calling thread at the default rate where supported.
    pub(crate) fn start_on_current_thread() -> Arc<Self> {
        let profiler = Arc::new(Self {
            samples: Mutex::new(VecDeque::with_capacity(CAPACITY)),
            hz: AtomicU32::new(DEFAULT_HZ),
            supported: cfg!(all(
                any(
                    target_os = "macos",
                    target_os = "ios",
                    target_os = "linux",
                    target_os = "android"
                ),
                target_arch = "aarch64"
            )),
        });
        #[cfg(all(any(target_os = "macos", target_os = "ios"), target_arch = "aarch64"))]
        apple::spawn(profiler.clone());
        #[cfg(all(
            any(target_os = "linux", target_os = "android"),
            target_arch = "aarch64"
        ))]
        signal::spawn(profiler.clone());
        profiler
    }

    #[cfg(unix)]
    pub(crate) fn set_hz(&self, hz: u32) {
        self.hz.store(hz.min(2000), Ordering::Release);
    }

    pub(crate) fn hz(&self) -> u32 {
        if self.supported {
            self.hz.load(Ordering::Acquire)
        } else {
            0
        }
    }

    /// Samples taken at or after `since_us`, oldest first.
    pub(crate) fn since(&self, since_us: u64) -> Vec<Sample> {
        let samples = self.samples.lock().unwrap_or_else(|p| p.into_inner());
        let start = samples.partition_point(|s| s.unix_us < since_us);
        samples.range(start..).copied().collect()
    }

    #[cfg_attr(not(target_arch = "aarch64"), allow(dead_code))]
    fn push(&self, sample: Sample) {
        let mut samples = self.samples.lock().unwrap_or_else(|p| p.into_inner());
        if samples.len() == CAPACITY {
            samples.pop_front();
        }
        samples.push_back(sample);
    }
}

#[cfg(all(any(target_os = "macos", target_os = "ios"), target_arch = "aarch64"))]
mod apple {
    use super::{FRAMES, Profiler, Sample, within};
    use std::sync::Arc;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    const ARM_THREAD_STATE64: i32 = 6;
    const ARM_THREAD_STATE64_COUNT: u32 = 68;
    const ADDRESS: usize = 0x0000_ffff_ffff_ffff;

    unsafe extern "C" {
        fn mach_thread_self() -> u32;
        fn thread_suspend(thread: u32) -> i32;
        fn thread_resume(thread: u32) -> i32;
        fn thread_get_state(thread: u32, flavor: i32, state: *mut u32, count: *mut u32) -> i32;
    }

    pub(super) fn spawn(profiler: Arc<Profiler>) {
        // SAFETY: plain queries of the calling (execution) thread.
        let (thread, top, size) = unsafe {
            let this = libc::pthread_self();
            (
                mach_thread_self(),
                libc::pthread_get_stackaddr_np(this) as usize,
                libc::pthread_get_stacksize_np(this),
            )
        };
        let bottom = top - size;
        let _ = std::thread::Builder::new()
            .name("adv-debug-sampler".into())
            .spawn(move || {
                loop {
                    let hz = profiler.hz.load(Ordering::Acquire);
                    if Arc::strong_count(&profiler) == 1 {
                        return;
                    }
                    if hz == 0 {
                        std::thread::sleep(Duration::from_millis(50));
                        continue;
                    }
                    std::thread::sleep(Duration::from_micros(1_000_000 / u64::from(hz)));
                    if let Some(sample) = sample(thread, bottom, top) {
                        profiler.push(sample);
                    }
                }
            });
    }

    /// One suspended read. Nothing here allocates or locks while the
    /// thread is stopped; frame pointers are followed only inside its stack.
    fn sample(thread: u32, bottom: usize, top: usize) -> Option<Sample> {
        let mut state = [0u64; 34];
        let mut count = ARM_THREAD_STATE64_COUNT;
        let mut frames = [0usize; FRAMES];
        let mut depth = 0;
        // SAFETY: `thread` is this process's execution thread, which outlives
        // the session; reads follow the checked chain within its stack.
        unsafe {
            if thread_suspend(thread) != 0 {
                return None;
            }
            let read = thread_get_state(
                thread,
                ARM_THREAD_STATE64,
                state.as_mut_ptr().cast(),
                &mut count,
            );
            if read == 0 {
                // x0..x28, fp, lr, sp, pc. Frame records lie above SP.
                let (mut fp, lr, pc) = (state[29] as usize, state[30] as usize, state[32] as usize);
                let bottom = bottom.max(state[31] as usize);
                frames[0] = pc & ADDRESS;
                frames[1] = lr & ADDRESS;
                depth = 2;
                while depth < FRAMES && within(fp, bottom, top) {
                    let next = *(fp as *const usize);
                    let ret = *((fp + 8) as *const usize) & ADDRESS;
                    if ret == 0 {
                        break;
                    }
                    frames[depth] = ret;
                    depth += 1;
                    if next <= fp {
                        break;
                    }
                    fp = next;
                }
            }
            thread_resume(thread);
            if read != 0 {
                return None;
            }
        }
        Some(Sample {
            unix_us: super::super::unix_us(),
            frames,
            depth: depth as u8,
        })
    }
}

#[cfg(all(
    any(target_os = "linux", target_os = "android"),
    target_arch = "aarch64"
))]
mod signal {
    use super::{FRAMES, Profiler, Sample, within};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    const ADDRESS: usize = 0x0000_ffff_ffff_ffff;
    const IDLE: u32 = 0;
    const ASKED: u32 = 1;
    const FILLED: u32 = 2;

    /// One request at a time: the sampler asks, the handler fills.
    static STATE: AtomicU32 = AtomicU32::new(IDLE);
    static FRAMES_SEEN: [AtomicUsize; FRAMES] = [const { AtomicUsize::new(0) }; FRAMES];
    static DEPTH: AtomicUsize = AtomicUsize::new(0);
    static BOTTOM: AtomicUsize = AtomicUsize::new(0);
    static TOP: AtomicUsize = AtomicUsize::new(0);

    /// The kernel's arm64 `struct sigcontext`, which starts 176 bytes into
    /// its `struct ucontext` (flags, link, stack, an 8-byte signal mask,
    /// 120 reserved bytes, 16-byte alignment). The libc crate's Android
    /// `ucontext_t` omits the reserved bytes, so it is not used.
    #[repr(C)]
    struct SigContext {
        fault_address: u64,
        regs: [u64; 31],
        sp: u64,
        pc: u64,
    }
    const SIGCONTEXT: usize = 176;

    pub(super) fn spawn(profiler: Arc<Profiler>) {
        // SAFETY: queries of the calling (execution) thread and installation
        // of a handler that only reads its own context and stack.
        let tid = unsafe {
            let mut attr: libc::pthread_attr_t = std::mem::zeroed();
            if libc::pthread_getattr_np(libc::pthread_self(), &mut attr) != 0 {
                return;
            }
            let (mut base, mut size) = (std::ptr::null_mut(), 0);
            let read = libc::pthread_attr_getstack(&attr, &mut base, &mut size);
            libc::pthread_attr_destroy(&mut attr);
            if read != 0 {
                return;
            }
            BOTTOM.store(base as usize, Ordering::Relaxed);
            TOP.store(base as usize + size, Ordering::Relaxed);
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = handler as *const () as usize;
            // SA_ONSTACK keeps an installed alternate stack in use, as the
            // fault handlers on this thread do.
            action.sa_flags = libc::SA_SIGINFO | libc::SA_RESTART | libc::SA_ONSTACK;
            libc::sigemptyset(&mut action.sa_mask);
            if libc::sigaction(libc::SIGPROF, &action, std::ptr::null_mut()) != 0 {
                return;
            }
            libc::gettid()
        };
        let stat = format!("/proc/self/task/{tid}/stat");
        let _ = std::thread::Builder::new()
            .name("adv-debug-sampler".into())
            .spawn(move || {
                loop {
                    let hz = profiler.hz.load(Ordering::Acquire);
                    if Arc::strong_count(&profiler) == 1 {
                        return;
                    }
                    if hz == 0 {
                        std::thread::sleep(Duration::from_millis(50));
                        continue;
                    }
                    std::thread::sleep(Duration::from_micros(1_000_000 / u64::from(hz)));
                    if let Some(sample) = sample(tid, &stat) {
                        profiler.push(sample);
                    }
                }
            });
    }

    /// A running thread is interrupted; a sleeping one is counted idle
    /// without a signal, so its blocking calls are not interrupted.
    fn sample(tid: libc::pid_t, stat: &str) -> Option<Sample> {
        let text = std::fs::read_to_string(stat).ok()?;
        let state = text.rsplit_once(") ")?.1.chars().next()?;
        let mut frames = [0usize; FRAMES];
        let mut depth = 0;
        if state == 'R' {
            STATE.store(ASKED, Ordering::Release);
            // SAFETY: a signal to a thread of this process.
            if unsafe { libc::syscall(libc::SYS_tgkill, libc::getpid(), tid, libc::SIGPROF) } != 0 {
                STATE.store(IDLE, Ordering::Release);
                return None;
            }
            let deadline = Instant::now() + Duration::from_millis(5);
            while STATE.load(Ordering::Acquire) != FILLED {
                if Instant::now() > deadline {
                    // A late handler finds no request and leaves.
                    STATE.store(IDLE, Ordering::Release);
                    return None;
                }
                std::thread::yield_now();
            }
            depth = DEPTH.load(Ordering::Relaxed);
            for (frame, seen) in frames.iter_mut().zip(&FRAMES_SEEN).take(depth) {
                *frame = seen.load(Ordering::Relaxed);
            }
            STATE.store(IDLE, Ordering::Release);
        }
        Some(Sample {
            unix_us: super::super::unix_us(),
            frames,
            depth: depth as u8,
        })
    }

    /// Async-signal-safe: atomics and reads inside the thread's own stack.
    extern "C" fn handler(_: libc::c_int, _: *mut libc::siginfo_t, context: *mut libc::c_void) {
        if STATE.load(Ordering::Acquire) != ASKED || context.is_null() {
            return;
        }
        let (bottom, top) = (BOTTOM.load(Ordering::Relaxed), TOP.load(Ordering::Relaxed));
        // SAFETY: the kernel passes this thread's saved context; frame
        // pointers are followed only inside its stack.
        unsafe {
            let context = &*context.cast::<u8>().add(SIGCONTEXT).cast::<SigContext>();
            let mut fp = context.regs[29] as usize;
            // Frame records lie above SP; below it are guard pages that
            // the runtime may have protected.
            let bottom = bottom.max(context.sp as usize);
            FRAMES_SEEN[0].store(context.pc as usize & ADDRESS, Ordering::Relaxed);
            FRAMES_SEEN[1].store(context.regs[30] as usize & ADDRESS, Ordering::Relaxed);
            let mut depth = 2;
            while depth < FRAMES && within(fp, bottom, top) {
                let next = *(fp as *const usize);
                let ret = *((fp + 8) as *const usize) & ADDRESS;
                if ret == 0 {
                    break;
                }
                FRAMES_SEEN[depth].store(ret, Ordering::Relaxed);
                depth += 1;
                if next <= fp {
                    break;
                }
                fp = next;
            }
            DEPTH.store(depth, Ordering::Relaxed);
        }
        STATE.store(FILLED, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    // spec: WINE-UI-6
    #[test]
    fn frame_records_stay_inside_the_stack_for_any_frame_pointer() {
        let (bottom, top) = (0x1000, 0x2000);
        assert!(super::within(0x1ff0, bottom, top));
        assert!(!super::within(0x1ff8, bottom, top));
        assert!(!super::within(0x0ff8, bottom, top));
        assert!(!super::within(0x1004, bottom, top));
        assert!(!super::within(usize::MAX - 7, bottom, usize::MAX));
        assert!(!super::within(usize::MAX - 15, 0, usize::MAX - 1));
    }
}
