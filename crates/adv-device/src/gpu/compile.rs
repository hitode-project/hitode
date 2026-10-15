//! One low-priority compiler, bounded independently of guest execution. Demand
//! steals queued work; OnceLock serializes a running job with its first user.
use std::sync::{
    Arc, Mutex, OnceLock,
    atomic::{AtomicU64, AtomicUsize, Ordering},
    mpsc,
};
use std::time::Instant;

type Task = Box<dyn FnOnce() + Send>;
const BYTES: usize = 32 * 1024 * 1024;
static RESERVED: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Default)]
pub struct CompileCounters(Arc<(AtomicU64, AtomicU64)>);
impl CompileCounters {
    pub fn snapshot(&self) -> (u64, u64) {
        (
            self.0.0.load(Ordering::Relaxed),
            self.0.1.load(Ordering::Relaxed),
        )
    }
}
struct Job<T> {
    result: OnceLock<Result<T, String>>,
    run: Mutex<Option<Box<dyn FnOnce() -> Result<T, String> + Send>>>,
    counters: CompileCounters,
}
pub struct Compile<T>(Arc<Job<T>>);
impl<T> Clone for Compile<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}
impl<T: Send + Sync + 'static> Compile<T> {
    pub fn spawn(
        bytes: usize,
        counters: CompileCounters,
        run: impl FnOnce() -> Result<T, String> + Send + 'static,
    ) -> Self {
        let work = Self(Arc::new(Job {
            result: OnceLock::new(),
            run: Mutex::new(Some(Box::new(run))),
            counters,
        }));
        if RESERVED
            .fetch_update(Ordering::AcqRel, Ordering::Relaxed, |used| {
                used.checked_add(bytes).filter(|v| *v <= BYTES)
            })
            .is_ok()
        {
            struct Reservation(usize);
            impl Drop for Reservation {
                fn drop(&mut self) {
                    RESERVED.fetch_sub(self.0, Ordering::AcqRel);
                }
            }
            let reservation = Reservation(bytes);
            let job = Arc::downgrade(&work.0);
            if let Some(sender) = worker() {
                let _ = sender.try_send(Box::new(move || {
                    let _reservation = reservation;
                    if let Some(job) = job.upgrade() {
                        Compile(job).resolve();
                    }
                }));
            }
        }
        work
    }
    pub fn ready(&self) -> Option<&Result<T, String>> {
        self.0.result.get()
    }
    /// Used by background dependencies; these waits are not first-draw stalls.
    pub fn resolve(&self) -> &Result<T, String> {
        self.0.result.get_or_init(|| {
            let run = self.0.run.lock().unwrap().take().unwrap();
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(run))
                .unwrap_or_else(|_| Err("background shader compiler panicked".into()))
        })
    }
    pub fn wait(&self) -> &Result<T, String> {
        if let Some(result) = self.ready() {
            return result;
        }
        let _demand = Demand::enter();
        let start = Instant::now();
        let result = self.resolve();
        self.0.counters.0.0.fetch_add(1, Ordering::Relaxed);
        self.0.counters.0.1.fetch_add(
            start.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64,
            Ordering::Relaxed,
        );
        result
    }
}
fn worker() -> Option<&'static mpsc::SyncSender<Task>> {
    static WORKER: OnceLock<Option<mpsc::SyncSender<Task>>> = OnceLock::new();
    WORKER
        .get_or_init(|| {
            let (sender, receiver) = mpsc::sync_channel::<Task>(64);
            std::thread::Builder::new()
                .name("adv-gpu-compile".into())
                .spawn(move || {
                    let allowed = lower_priority();
                    while let Ok(task) = receiver.recv() {
                        if allowed {
                            task();
                        }
                    }
                })
                .ok()
                .map(|_| sender)
        })
        .as_ref()
}
fn lower_priority() -> bool {
    // Failure leaves jobs lazy: compilation can never silently compete at the
    // execution/audio priority. No affinity or real-time scheduling is used.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    unsafe {
        return libc::setpriority(libc::PRIO_PROCESS, 0, 10) == 0;
    }
    #[cfg(target_vendor = "apple")]
    unsafe {
        unsafe extern "C" {
            fn pthread_set_qos_class_self_np(class: u32, relative: i32) -> i32;
        }
        return pthread_set_qos_class_self_np(0x09, 0) == 0;
    }
    #[cfg(windows)]
    unsafe {
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetCurrentThread() -> *mut core::ffi::c_void;
            fn SetThreadPriority(thread: *mut core::ffi::c_void, priority: i32) -> i32;
        }
        return SetThreadPriority(GetCurrentThread(), -2) != 0;
    }
    #[allow(unreachable_code)]
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    // spec: GFX-CACHE-7
    #[test]
    fn demand_steals_lazy_work_and_compiles_once() {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let stats = CompileCounters::default();
        let job = Compile::spawn(BYTES + 1, stats.clone(), move || {
            counter.fetch_add(1, Ordering::Relaxed);
            Ok(42)
        });
        assert!(job.ready().is_none());
        assert_eq!(*job.wait(), Ok(42));
        assert_eq!(*job.clone().wait(), Ok(42));
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert_eq!(stats.snapshot().0, 1);
    }
    // spec: GFX-CACHE-7
    #[test]
    fn running_background_work_is_joined_without_recompiling() {
        let (entered, started) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let job = Compile::spawn(BYTES + 1, CompileCounters::default(), move || {
            entered.send(()).unwrap();
            gate.recv().unwrap();
            Ok(7)
        });
        let background = job.clone();
        let thread = std::thread::spawn(move || {
            background.resolve();
        });
        started.recv().unwrap();
        release.send(()).unwrap();
        assert_eq!(*job.wait(), Ok(7));
        thread.join().unwrap();
    }
}

static DEMAND: Mutex<usize> = Mutex::new(0);
static DEMAND_DONE: std::sync::Condvar = std::sync::Condvar::new();
struct Demand;
impl Demand {
    fn enter() -> Self {
        *DEMAND.lock().unwrap() += 1;
        Self
    }
}
impl Drop for Demand {
    fn drop(&mut self) {
        *DEMAND.lock().unwrap() -= 1;
        DEMAND_DONE.notify_all();
    }
}
/// A bounded archive uses one queue slot. Demand can steal any individual job;
/// speculative work yields to foreground compilation at each pipeline boundary.
pub fn prebuild<T: Send + Sync + 'static>(bytes: usize, jobs: Vec<Compile<T>>) -> Compile<()> {
    Compile::spawn(bytes, CompileCounters::default(), move || {
        for job in jobs {
            if Arc::strong_count(&job.0) == 1 {
                continue;
            }
            let mut active = DEMAND.lock().unwrap();
            while *active != 0 {
                active = DEMAND_DONE.wait(active).unwrap();
            }
            drop(active);
            job.resolve();
        }
        Ok(())
    })
}

#[cfg(test)]
mod cancellation_tests {
    use super::*;
    // spec: GFX-CACHE-7, GFX-CACHE-10
    #[test]
    fn abandoned_prebuilds_do_not_compile_or_retain_devices() {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let job = Compile::spawn(BYTES + 1, CompileCounters::default(), move || {
            counter.fetch_add(1, Ordering::Relaxed);
            Ok(())
        });
        let batch = prebuild(0, vec![job]);
        batch.resolve().as_ref().unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 0);
    }
}
