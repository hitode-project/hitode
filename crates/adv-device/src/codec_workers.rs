//! Shared codec workers. Visible-frame work has priority over run-ahead.
use std::{
    cell::Cell,
    collections::VecDeque,
    ffi::c_void,
    sync::{Arc, Condvar, Mutex, OnceLock, mpsc},
    thread,
};

type Run = unsafe extern "C" fn(*mut c_void, i32);
type Job = Box<dyn FnOnce() + Send>;

thread_local! {
    static SPECULATING: Cell<bool> = const { Cell::new(false) };
}

#[derive(Default)]
struct Queue {
    foreground: VecDeque<Job>,
    reconstruction: VecDeque<Job>,
    speculative: VecDeque<Job>,
    active: usize,
    stopped: bool,
}

#[derive(Default)]
struct Work {
    queue: Mutex<Queue>,
    ready: Condvar,
}

struct Pool {
    work: Arc<Work>,
    slots: i32,
    caller: Mutex<()>,
}

#[cfg(target_vendor = "apple")]
fn performance_cores() -> Option<usize> {
    unsafe extern "C" {
        fn sysctlbyname(
            name: *const std::ffi::c_char,
            old: *mut c_void,
            old_len: *mut usize,
            new: *mut c_void,
            new_len: usize,
        ) -> i32;
    }
    let mut count = 0i32;
    let mut size = std::mem::size_of_val(&count);
    // SAFETY: the output points to a writable int and the name is terminated.
    let result = unsafe {
        sysctlbyname(
            c"hw.perflevel0.physicalcpu".as_ptr(),
            (&mut count as *mut i32).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    (result == 0 && count > 0).then_some(count as usize)
}

#[cfg(target_os = "android")]
fn performance_cores() -> Option<usize> {
    let frequencies: Vec<u64> = std::fs::read_dir("/sys/devices/system/cpu")
        .ok()?
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let name = entry.file_name();
            name.to_str()?.strip_prefix("cpu")?.parse::<u32>().ok()?;
            let path = entry.path().join("cpufreq/cpuinfo_max_freq");
            std::fs::read_to_string(path).ok()?.trim().parse().ok()
        })
        .collect();
    let fastest = frequencies.iter().max()?;
    Some(frequencies.iter().filter(|&freq| freq == fastest).count())
}

#[cfg(not(any(target_vendor = "apple", target_os = "android")))]
fn performance_cores() -> Option<usize> {
    None
}

impl Pool {
    fn new() -> Option<Self> {
        let cores = performance_cores()
            .or_else(|| thread::available_parallelism().ok().map(usize::from))
            .unwrap_or(1);
        let workers = cores.saturating_sub(1).max(2);
        let work = Arc::new(Work::default());
        for worker in 0..workers {
            let worker_work = Arc::clone(&work);
            if thread::Builder::new()
                .name(format!("adv-codec-{worker}"))
                .spawn(move || worker_work.run(workers - 1))
                .is_err()
            {
                work.queue.lock().unwrap().stopped = true;
                work.ready.notify_all();
                return None;
            }
        }
        Some(Self {
            work,
            slots: workers as i32 + 1,
            caller: Mutex::new(()),
        })
    }
}

impl Work {
    fn run(&self, speculative_slots: usize) {
        #[cfg(target_vendor = "apple")]
        // SAFETY: sets the QoS of the calling codec worker only.
        unsafe {
            unsafe extern "C" {
                fn pthread_set_qos_class_self_np(class: u32, relative: i32) -> i32;
            }
            pthread_set_qos_class_self_np(0x21, 0);
        }
        loop {
            let (job, speculative) = {
                let mut queue = self.queue.lock().unwrap();
                loop {
                    if queue.stopped {
                        return;
                    }
                    if let Some(job) = queue
                        .foreground
                        .pop_front()
                        .or_else(|| queue.reconstruction.pop_front())
                    {
                        break (job, false);
                    }
                    // Keep a worker available for nested reconstruction dispatch.
                    if queue.active < speculative_slots
                        && let Some(job) = queue.speculative.pop_front()
                    {
                        queue.active += 1;
                        break (job, true);
                    }
                    queue = self.ready.wait(queue).unwrap();
                }
            };
            SPECULATING.set(speculative);
            job();
            if speculative {
                self.queue.lock().unwrap().active -= 1;
                self.ready.notify_one();
            }
        }
    }
}

fn pool() -> Option<&'static Pool> {
    static POOL: OnceLock<Option<Pool>> = OnceLock::new();
    POOL.get_or_init(Pool::new).as_ref()
}

pub fn available() -> bool {
    pool().is_some()
}

/// Queue owned work without borrowing guest state or reserving a reconstruction worker.
pub fn submit_speculation(job: impl FnOnce() + Send + 'static) {
    let pool = pool().expect("codec workers are available");
    pool.work
        .queue
        .lock()
        .unwrap()
        .speculative
        .push_back(Box::new(job));
    pool.work.ready.notify_one();
}

#[unsafe(no_mangle)]
extern "C" fn adv_theora_slots() -> i32 {
    pool().map_or(1, |pool| pool.slots)
}

#[unsafe(no_mangle)]
unsafe extern "C" fn adv_theora_dispatch(context: *mut c_void, count: i32, run: Run) {
    let pool = pool().expect("codec pool was initialized before banding");
    let speculative = SPECULATING.get();
    let caller = (!speculative)
        .then(|| pool.caller.try_lock().ok())
        .flatten();
    let first = i32::from(speculative || caller.is_some());
    let (done, completion) = mpsc::channel();
    {
        let mut queue = pool.work.queue.lock().unwrap();
        let jobs = if speculative {
            &mut queue.reconstruction
        } else {
            &mut queue.foreground
        };
        for index in first..count {
            let context = context as usize;
            let done = done.clone();
            jobs.push_back(Box::new(move || {
                // SAFETY: dispatch joins each task before its context expires.
                // Each task owns disjoint storage for its phase.
                unsafe { run(context as *mut c_void, index) };
                done.send(()).expect("waiting codec caller");
            }));
        }
    }
    pool.work.ready.notify_all();
    if first != 0 {
        // SAFETY: the first band or plane was reserved for this caller.
        unsafe { run(context, 0) };
    }
    drop(caller);
    for _ in first..count {
        completion.recv().expect("codec task completed");
    }
}
