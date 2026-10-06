//! Bounded native execution workers. The controller alone owns code publication,
//! mappings and API state. A permit pins every native pointer until completion.
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::parallel_memory::{Fence, Permit};
use crate::{
    CpuState, Exit,
    linking::{NativeContext, Table},
    memory::WorkerLayout,
};

pub(crate) type Entry = unsafe extern "C" fn(*mut NativeContext, *mut u8) -> u32;

/// Contains addresses, not ownership of code. Only Pool accepts this private
/// descriptor. Publication/retirement must quiesce its fence before freeing code.
pub(crate) struct Image {
    pub entries: HashMap<u32, Entry>,
    pub links: Arc<Table>,
}

#[derive(Clone, Debug)]
pub(crate) enum Stop {
    Trap(u8),
    Fallback,
    CacheMiss,
    Quantum,
    Interrupted,
    Parked,
    Failed(String),
}

/// Per-guest-context admission history; never a guest priority or fixed role.
/// Two useful bursts earn tolerance for occasional short API boundaries. Cold
/// execution returns to the inline lane until it demonstrates native work again.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Admission {
    credit: u8,
    /// Inline native time since the last boundary. Safepoints sized from the
    /// block rate split one burst into many short runs.
    inline: Duration,
    pub preferred: Option<usize>,
}

/// Native execution long enough to be worth a worker lease.
const BURST: Duration = Duration::from_micros(100);

impl Admission {
    pub fn ready(self) -> bool {
        self.credit != 0
    }
    pub fn native_burst(&mut self) {
        self.credit = self.credit.saturating_add(2).min(4);
    }
    pub fn cold(&mut self) {
        self.credit = 0;
        self.inline = Duration::ZERO;
    }
    /// An inline run that executed only native code and ended at its budget.
    pub fn inline_native(&mut self, elapsed: Duration) {
        self.inline = self.inline.saturating_add(elapsed);
        if self.inline >= BURST {
            self.inline = Duration::ZERO;
            self.native_burst();
        }
    }
    /// An inline run that ended at a host call or other boundary.
    pub fn inline_boundary(&mut self) {
        self.inline = Duration::ZERO;
    }
    fn completed(&mut self, stop: &Stop, elapsed: Duration, blocks: u64) {
        if matches!(stop, Stop::Fallback | Stop::Failed(_)) {
            self.cold();
        } else if blocks != 0 && !matches!(stop, Stop::Interrupted) {
            if elapsed >= BURST {
                self.native_burst();
            } else if matches!(stop, Stop::Trap(_) | Stop::Parked) {
                self.credit = self.credit.saturating_sub(1);
            }
            // A short quantum may be a host/fence interruption, not a short
            // guest burst. Do not penalize it or the idle/expired job.
        }
    }
}

pub(crate) struct Job {
    pub token: u64,
    pub cpu: CpuState,
    pub memory: WorkerLayout,
    pub image: Arc<Image>,
    pub stop: u32,
    pub submitted_at: Instant,
    pub deadline: Instant,
    pub admission: Admission,
    pub guest_clock: (u64, Instant),
    pub host_interrupt: Arc<AtomicBool>,
    pub permit: Permit,
}

pub(crate) struct Completion {
    pub token: u64,
    pub cpu: CpuState,
    pub stop: Stop,
    pub worker: usize,
    pub entries: u64,
    pub blocks: u64,
    pub bulk_calls: u64,
    pub bulk_bytes: u64,
    pub elapsed: Duration,
    pub queue_time: Duration,
    pub admission: Admission,
}

#[derive(Clone, Debug, Default)]
pub struct ParallelStats {
    pub workers: usize,
    pub submitted: u64,
    pub completed: u64,
    /// Sum of worker run wall time, not process CPU utilization.
    pub worker_run_ns: u64,
    pub queue_wait_ns: u64,
    pub queue_wait_max_ns: u64,
    pub image_builds: u64,
    pub image_build_ns: u64,
    pub empty_jobs: u64,
    pub short_traps: u64,
    pub overlap_ns: u64,
    pub max_running: usize,
    pub traps: u64,
    pub cold_retries: u64,
    pub cache_misses: u64,
    pub quantum_returns: u64,
    pub quiescence_waits: u64,
    pub quiescence_wait_ns: u64,
    pub quiescence_wait_max_ns: u64,
}

struct Overlap {
    running: usize,
    maximum: usize,
    last: Instant,
    nanos: u64,
}

/// The controller co-locates the inline worker, retaining cheap synchronous API
/// calls. This interval includes its API work, not just native instructions.
pub(crate) struct InlineActivity(Arc<Mutex<Overlap>>);
impl Drop for InlineActivity {
    fn drop(&mut self) {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .change(false);
    }
}
impl Overlap {
    fn change(&mut self, entering: bool) {
        let now = Instant::now();
        if self.running > 1 {
            self.nanos += now.duration_since(self.last).as_nanos() as u64;
        }
        self.last = now;
        if entering {
            self.running += 1;
        } else {
            self.running -= 1;
        }
        self.maximum = self.maximum.max(self.running);
    }
}

struct Worker {
    sender: mpsc::SyncSender<Option<Job>>,
    thread: Option<JoinHandle<()>>,
    token: Option<u64>,
}

pub(crate) struct Pool {
    pub fence: Arc<Fence>,
    workers: Vec<Worker>,
    results: mpsc::Receiver<Completion>,
    event: adv_sync::WakeEvent,
    overlap: Arc<Mutex<Overlap>>,
    stats: ParallelStats,
    next_token: u64,
    tsc: Arc<AtomicU64>,
}

impl Pool {
    pub fn new(count: usize, tsc: Arc<AtomicU64>) -> Result<Self, String> {
        if count == 0 || count > 64 {
            return Err("guest worker count outside 1..=64".into());
        }
        let fence = Arc::new(Fence::default());
        let event = adv_sync::WakeEvent::new().map_err(|e| e.to_string())?;
        let (send, results) = mpsc::sync_channel(count);
        let overlap = Arc::new(Mutex::new(Overlap {
            running: 0,
            maximum: 0,
            last: Instant::now(),
            nanos: 0,
        }));
        let mut pool = Self {
            fence,
            workers: Vec::new(),
            results,
            event,
            overlap,
            stats: ParallelStats {
                workers: count + 1,
                ..Default::default()
            },
            next_token: 0,
            tsc,
        };
        for index in 0..count {
            let (sender, jobs) = mpsc::sync_channel::<Option<Job>>(1);
            let send = send.clone();
            let event = pool.event.clone();
            let fence = pool.fence.clone();
            let overlap = pool.overlap.clone();
            let tsc = pool.tsc.clone();
            let handle = thread::Builder::new()
                .name(format!("adv-guest-{}", index + 1))
                .spawn(move || {
                    while let Ok(Some(job)) = jobs.recv() {
                        // The permit covers facade destruction. CPU completion is
                        // published separately and must also be drained by the machine.
                        overlap
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .change(true);
                        let completion = execute(job, index, &fence, &tsc);
                        overlap
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .change(false);
                        if send.send(completion).is_err() {
                            break;
                        }
                        event.notify();
                    }
                })
                .map_err(|e| format!("create guest worker: {e}"))?;
            pool.workers.push(Worker {
                sender,
                thread: Some(handle),
                token: None,
            });
        }
        Ok(pool)
    }

    pub fn record_image_build(&mut self, elapsed: Duration) {
        self.stats.image_builds += 1;
        self.stats.image_build_ns += elapsed.as_nanos() as u64;
    }

    pub fn capacity(&self) -> usize {
        self.workers.len()
    }
    pub fn inline_activity(&self) -> InlineActivity {
        self.overlap
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .change(true);
        InlineActivity(self.overlap.clone())
    }
    pub fn has_capacity(&self) -> bool {
        self.workers.iter().any(|w| w.token.is_none())
    }
    pub fn next_token(&mut self) -> Result<u64, String> {
        self.next_token = self
            .next_token
            .checked_add(1)
            .ok_or("native lease identity exhausted")?;
        Ok(self.next_token)
    }
    pub fn submit(&mut self, job: Job) -> Result<u64, String> {
        let index = job
            .admission
            .preferred
            .filter(|&i| self.workers.get(i).is_some_and(|w| w.token.is_none()))
            .or_else(|| self.workers.iter().position(|w| w.token.is_none()))
            .ok_or("guest execution pool is full")?;
        let worker = &mut self.workers[index];
        let token = job.token;
        worker
            .sender
            .send(Some(job))
            .map_err(|_| "guest execution worker closed")?;
        worker.token = Some(token);
        self.stats.submitted += 1;
        Ok(token)
    }
    pub fn poll(&mut self) -> Option<Completion> {
        let completion = self.results.try_recv().ok()?;
        let worker = &mut self.workers[completion.worker];
        assert_eq!(
            worker.token.take(),
            Some(completion.token),
            "stale native execution completion"
        );
        self.stats.completed += 1;
        self.stats.worker_run_ns += completion.elapsed.as_nanos() as u64;
        self.stats.queue_wait_ns += completion.queue_time.as_nanos() as u64;
        self.stats.queue_wait_max_ns = self
            .stats
            .queue_wait_max_ns
            .max(completion.queue_time.as_nanos() as u64);
        self.stats.empty_jobs += u64::from(completion.blocks == 0);
        self.stats.short_traps += u64::from(
            matches!(completion.stop, Stop::Trap(_))
                && completion.elapsed < Duration::from_micros(100),
        );
        match completion.stop {
            Stop::Trap(_) => self.stats.traps += 1,
            Stop::Fallback => self.stats.cold_retries += 1,
            Stop::CacheMiss => self.stats.cache_misses += 1,
            Stop::Quantum => self.stats.quantum_returns += 1,
            _ => {}
        }
        Some(completion)
    }
    pub fn wait(&self, duration: Duration) -> Result<(), String> {
        self.event
            .wait(Some(duration))
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    pub fn stats(&self) -> ParallelStats {
        let mut stats = self.stats.clone();
        let overlap = self.overlap.lock().unwrap_or_else(|e| e.into_inner());
        stats.overlap_ns = overlap.nanos;
        stats.max_running = overlap.maximum;
        stats.quiescence_waits = self.fence.waits.load(Ordering::Relaxed);
        stats.quiescence_wait_ns = self.fence.wait_ns.load(Ordering::Relaxed);
        stats.quiescence_wait_max_ns = self.fence.wait_max_ns.load(Ordering::Relaxed);
        stats
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        self.fence.quiesce();
        // One completion per worker fits the result queue without controller
        // consumption, including process failure and unwinding.
        for worker in &self.workers {
            let _ = worker.sender.send(None);
        }
        for worker in &mut self.workers {
            if let Some(handle) = worker.thread.take() {
                let _ = handle.join();
            }
        }
    }
}

fn execute(job: Job, worker: usize, fence: &Fence, tsc: &Arc<AtomicU64>) -> Completion {
    let Job {
        token,
        mut cpu,
        memory,
        image,
        stop,
        submitted_at,
        deadline,
        mut admission,
        guest_clock,
        host_interrupt,
        permit,
    } = job;
    let started = Instant::now();
    let queue_time = started.saturating_duration_since(submitted_at);
    // SAFETY: permit pins the owner allocation/mappings/code; no API state or
    // Rc file object enters this worker. Helpers use the restricted facade.
    let mut memory = unsafe { memory.into_memory() };
    let mut bridge = crate::trap::Bridge {
        timestamp: crate::timestamp::Counter::anchored(guest_clock, tsc.clone()),
        memory: &mut memory,
        error: None,
        trap: None,
        calls: 0,
        handler: None,
        parked: false,
        deadline: Some(deadline),
        bulk_calls: 0,
        bulk_bytes: 0,
    };
    let mut entries = 0;
    let mut blocks = 0;
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        loop {
            if host_interrupt.load(Ordering::Acquire) {
                return Stop::Interrupted;
            }
            if fence.stop.load(Ordering::Acquire) {
                return Stop::Quantum;
            }
            if Instant::now() >= deadline {
                return Stop::Quantum;
            }
            if cpu.eip == stop {
                return Stop::Parked;
            }
            if cpu.eflags & (1 << 8) != 0 || cpu.x87.status & 0x80 != 0 {
                return Stop::Fallback;
            }
            let Some(entry) = image.entries.get(&cpu.eip) else {
                return Stop::CacheMiss;
            };
            cpu.fuel = 256;
            let (slots, mask) = image.links.parts();
            let mut context = NativeContext {
                state: &mut cpu,
                rows: std::ptr::null(),
                written: 0,
                perms: memory.perms.as_ptr(),
                bridge: (&mut bridge as *mut crate::trap::Bridge<'_>).cast(),
                interrupt: &fence.stop,
                slots,
                mask,
                remaining: crate::linking::CHAIN_LIMIT,
                stop,
                #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
                fault_reads: 0,
                linked_entries: 0,
                shadow: std::ptr::null_mut(),
            };
            let result = unsafe { entry(&mut context, memory.ptr.as_ptr()) };
            entries += 1 + context.linked_entries;
            blocks += 256 - cpu.fuel;
            match result {
                n if n == Exit::Trap as u32 => {
                    if let Some(error) = bridge.error.take() {
                        return Stop::Failed(error);
                    }
                    return bridge.trap.take().map_or_else(
                        || Stop::Failed("native trap without vector".into()),
                        |(_, vector)| Stop::Trap(vector),
                    );
                }
                n if n == Exit::Dispatch as u32 || n == Exit::Budget as u32 => {}
                n if n == Exit::Interrupted as u32 => {
                    return if host_interrupt.load(Ordering::Acquire) {
                        Stop::Interrupted
                    } else {
                        Stop::Quantum
                    };
                }
                // The owner retries this precise instruction. Lazy file/COW
                // operands and executable writes need the owner's cold lane;
                // true faults are classified by the same interpreter there.
                n if n == Exit::Fallback as u32
                    || n == Exit::ReadFault as u32
                    || n == Exit::WriteFault as u32 =>
                {
                    return Stop::Fallback;
                }
                n => return Stop::Failed(format!("unknown native exit {n}")),
            }
        }
    }))
    .unwrap_or_else(|_| Stop::Failed("guest native worker panicked".into()));
    cpu.refresh_mmx();
    let elapsed = started.elapsed();
    admission.preferred = Some(worker);
    admission.completed(&outcome, elapsed, blocks);
    let completion = Completion {
        token,
        cpu,
        stop: outcome,
        worker,
        entries,
        blocks,
        bulk_calls: bridge.bulk_calls,
        bulk_bytes: bridge.bulk_bytes,
        elapsed,
        queue_time,
        admission,
    };
    drop(bridge);
    drop(memory);
    drop(image);
    drop(permit);
    completion
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn useful_history_survives_an_api_but_repeated_short_traps_and_cold_code_demote() {
        let mut admission = Admission::default();
        admission.native_burst();
        admission.native_burst();
        admission.completed(&Stop::Trap(0x80), Duration::from_micros(20), 1);
        assert!(admission.ready());
        for _ in 0..4 {
            admission.completed(&Stop::Trap(0x80), Duration::from_micros(20), 1);
        }
        assert!(!admission.ready());
        admission.native_burst();
        admission.completed(&Stop::Quantum, Duration::ZERO, 0);
        admission.completed(&Stop::CacheMiss, Duration::from_micros(10), 0);
        assert!(
            admission.ready(),
            "host interruption or missing publication is not guest cold code"
        );
        admission.completed(&Stop::Fallback, Duration::from_millis(1), 50);
        assert!(!admission.ready());
    }

    #[test]
    fn consecutive_short_native_runs_form_one_burst_until_a_boundary() {
        let mut admission = Admission::default();
        for _ in 0..3 {
            admission.inline_native(Duration::from_micros(30));
        }
        admission.inline_boundary();
        admission.inline_native(Duration::from_micros(30));
        assert!(!admission.ready());
        for _ in 0..3 {
            admission.inline_native(Duration::from_micros(30));
        }
        assert!(admission.ready());
    }

    #[test]
    fn expired_lease_and_missing_image_return_the_exact_cpu_state() {
        let mut memory = crate::Memory::new(0x1000, 4096).unwrap();
        let fence = Arc::new(Fence::default());
        memory.parallel = Some(fence.clone());
        let tsc = Arc::new(AtomicU64::new(0));
        for expired in [true, false] {
            let now = Instant::now();
            let mut cpu = CpuState::default();
            cpu.eip = 0x1000;
            let completion = execute(
                Job {
                    token: 7,
                    cpu,
                    memory: memory.worker_layout(),
                    image: Arc::new(Image {
                        entries: HashMap::new(),
                        links: Arc::default(),
                    }),
                    stop: 0x2000,
                    submitted_at: now,
                    deadline: if expired {
                        now
                    } else {
                        now + Duration::from_secs(10)
                    },
                    admission: Admission::default(),
                    guest_clock: (0, now),
                    host_interrupt: Arc::new(AtomicBool::new(false)),
                    permit: fence.enter(),
                },
                0,
                &fence,
                &tsc,
            );
            assert_eq!(completion.cpu.eip, 0x1000);
            assert_eq!(completion.blocks, 0);
            assert_eq!(completion.token, 7);
            assert!(fence.is_idle());
            if expired {
                assert!(matches!(completion.stop, Stop::Quantum));
            } else {
                assert!(matches!(completion.stop, Stop::CacheMiss));
            }
        }
    }
}
