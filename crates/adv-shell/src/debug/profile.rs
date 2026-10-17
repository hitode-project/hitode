//! In-process sampler of the execution thread. Each sample suspends the
//! thread, reads its PC, LR and frame-pointer chain within its own stack,
//! and resumes it before recording anything, so the sampler never waits on
//! a lock the suspended thread may hold. Supported on Apple arm64 hosts.
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
    pub depth: u8,
}

pub(crate) struct Profiler {
    samples: Mutex<VecDeque<Sample>>,
    /// Samples per second; 0 stops sampling.
    hz: AtomicU32,
    pub(crate) supported: bool,
}

impl Profiler {
    /// Starts sampling the calling thread at the default rate where supported.
    pub(crate) fn start_on_current_thread() -> Arc<Self> {
        let profiler = Arc::new(Self {
            samples: Mutex::new(VecDeque::with_capacity(CAPACITY)),
            hz: AtomicU32::new(DEFAULT_HZ),
            supported: cfg!(all(
                any(target_os = "macos", target_os = "ios"),
                target_arch = "aarch64"
            )),
        });
        #[cfg(all(any(target_os = "macos", target_os = "ios"), target_arch = "aarch64"))]
        apple::spawn(profiler.clone());
        profiler
    }

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

    #[cfg_attr(
        not(all(any(target_os = "macos", target_os = "ios"), target_arch = "aarch64")),
        allow(dead_code)
    )]
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
    use super::{FRAMES, Profiler, Sample};
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
                // x0..x28, fp, lr, sp, pc
                let (mut fp, lr, pc) = (state[29] as usize, state[30] as usize, state[32] as usize);
                frames[0] = pc & ADDRESS;
                frames[1] = lr & ADDRESS;
                depth = 2;
                while depth < FRAMES && fp % 8 == 0 && fp >= bottom && fp + 16 <= top {
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
