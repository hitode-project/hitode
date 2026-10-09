//! Synchronous row batches over a persistent pool of host workers.
//!
//! The compiler describes a counted inner loop and the affine transition to
//! the next row. Admission proves the complete batch before any store occurs.
//! Workers claim consecutive rows, finish every claim, and merge the last
//! architectural write in guest row order. The owner remains inside its native
//! invocation throughout, keeping code, memory and the permission table pinned.
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};

use crate::linking::NativeContext;
use crate::memory::{PAGE_SIZE, READ, WRITE};
use crate::{CpuState, Exit};

pub(crate) type Kernel = unsafe extern "C" fn(*mut NativeContext, *mut u8, usize) -> u32;

/// Bound the interval spent completing a claimed row without an interrupt
/// check. Larger jobs retain the checked serial path; another batch can resume
/// after the region executes the last row's glue.
const MAX_ROWS: u32 = 256;
const MAX_ROW_COST: u64 = 1 << 20;

#[derive(Default, Clone, Copy, Debug)]
pub struct RowStats {
    pub workers: usize,
    pub batches: u64,
    pub rows: u64,
    pub rejected: u64,
}

/// `[base + index * scale + low, base + index * scale + high)` in iteration
/// zero, translated by `step` on each further inner iteration. `high` is
/// exclusive. A glue read has step zero and an address expressed at the inner
/// header (the compiler accounts for any preceding induction updates).
#[derive(Clone, Debug)]
pub(crate) struct RowGroup {
    pub base: Option<usize>,
    pub index: Option<(usize, u32)>,
    pub step: i64,
    pub low: i64,
    pub high: i64,
    pub read: bool,
    pub write: bool,
}

/// Owned by the compiled region; both native addresses are patched before
/// publication and remain valid until the synchronous batch has drained.
pub(crate) struct Descriptor {
    pub groups: Vec<RowGroup>,
    pub glue_reads: Vec<RowGroup>,
    pub memory_base: u32,
    pub memory_len: usize,
    pub outer_header: u32,
    pub outer_latch: u32,
    pub inner_header: u32,
    pub inner_exit: u32,
    pub inner_counter: usize,
    /// Width in bytes, either two or four.
    pub counter_size: u32,
    pub outer_counter: usize,
    pub inductions: u8,
    /// Header invariants can be reset by glue: use their final-row output.
    pub invariant: u8,
    pub inner_steps: [i64; 8],
    /// GPR bits 0..7, MMX bits 8..15, XMM bits 16..23.
    pub tracked: u32,
    pub inner_cost: u64,
    pub glue_cost: u64,
    pub thunk: usize,
    pub target: usize,
}

struct Worker {
    mailbox: Arc<Mailbox>,
    handle: Option<JoinHandle<()>>,
}

#[derive(Default)]
struct MailboxState {
    pending: Option<Arc<Batch>>,
    shutdown: bool,
}

#[derive(Default)]
struct Mailbox {
    state: Mutex<MailboxState>,
    ready: Condvar,
}

impl Mailbox {
    fn submit(&self, batch: Arc<Batch>) {
        {
            let mut state = lock(&self.state);
            // Submission is serialized. Any older pending batch is closed;
            // replacing it avoids blocking on a worker that has not woken.
            state.pending = Some(batch);
        }
        self.ready.notify_one();
    }

    fn receive(&self) -> Option<Arc<Batch>> {
        let mut state = lock(&self.state);
        loop {
            if state.shutdown {
                return None;
            }
            if let Some(batch) = state.pending.take() {
                return Some(batch);
            }
            state = self
                .ready
                .wait(state)
                .unwrap_or_else(|error| error.into_inner());
        }
    }

    fn shutdown(&self) {
        {
            let mut state = lock(&self.state);
            state.shutdown = true;
            state.pending = None;
        }
        self.ready.notify_one();
    }
}

/// An owner may use one pool repeatedly; a batch always drains before returning.
/// Separate guest execution workers do not receive this pool in their contexts.
pub(crate) struct RowPool {
    workers: Vec<Worker>,
    submission: Mutex<()>,
    threshold: u64,
    rows_completed: AtomicU64,
    batches: AtomicU64,
    rejected: AtomicU64,
}

impl RowPool {
    pub(crate) fn fork_empty(&self) -> Result<Self, String> {
        let mut child = Self::new(self.workers.len())?;
        child.threshold = self.threshold;
        Ok(child)
    }

    pub fn default_workers() -> usize {
        thread::available_parallelism()
            .map_or(1, usize::from)
            .saturating_sub(2)
            .min(3)
    }

    /// `workers` counts additional host threads; the owner also runs rows.
    /// Zero executes admitted batches inline without spawning a thread.
    pub fn new(workers: usize) -> Result<Self, String> {
        if workers > 64 {
            return Err("row worker count outside 0..=64".into());
        }
        let mut pool = Self {
            workers: Vec::with_capacity(workers),
            submission: Mutex::new(()),
            threshold: 65_536,
            rows_completed: AtomicU64::new(0),
            batches: AtomicU64::new(0),
            rejected: AtomicU64::new(0),
        };
        for index in 0..workers {
            let mailbox = Arc::new(Mailbox::default());
            let receive = mailbox.clone();
            let handle = thread::Builder::new()
                .name(format!("adv-row-{}", index + 1))
                .spawn(move || {
                    while let Some(batch) = receive.receive() {
                        // No worker may silently abandon a claimed, possibly
                        // written row. A Rust panic is an invariant violation.
                        let result = std::panic::catch_unwind(|| execute_worker(&batch));
                        if result.is_err() {
                            invariant_failure();
                        }
                    }
                })
                .map_err(|error| format!("create row worker: {error}"))?;
            pool.workers.push(Worker {
                mailbox,
                handle: Some(handle),
            });
        }
        Ok(pool)
    }

    pub fn workers(&self) -> usize {
        self.workers.len()
    }

    pub fn set_threshold(&mut self, threshold: u64) {
        self.threshold = threshold;
    }

    pub fn rows_completed(&self) -> u64 {
        self.rows_completed.load(Ordering::Relaxed)
    }

    pub fn batches(&self) -> u64 {
        self.batches.load(Ordering::Relaxed)
    }

    pub fn rejected(&self) -> u64 {
        self.rejected.load(Ordering::Relaxed)
    }

    pub fn stats(&self) -> RowStats {
        RowStats {
            workers: self.workers(),
            batches: self.batches(),
            rows: self.rows_completed(),
            rejected: self.rejected(),
        }
    }

    fn reject(&self) -> u32 {
        self.rejected.fetch_add(1, Ordering::Relaxed);
        0
    }

    fn dispatch(&self, batch: &Arc<Batch>) {
        for worker in &self.workers {
            worker.mailbox.submit(batch.clone());
        }
        execute(batch);
        batch.participants.close_and_wait();
    }
}

impl Drop for RowPool {
    fn drop(&mut self) {
        // Exclusive ownership guarantees the last synchronous batch has drained.
        for worker in &self.workers {
            worker.mailbox.shutdown();
        }
        for worker in &mut self.workers {
            if let Some(handle) = worker.handle.take() {
                let _ = handle.join();
            }
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

#[derive(Clone, Copy, Debug)]
struct Range {
    low: i128,
    high: i128,
    pitch: i128,
    read: bool,
    write: bool,
}

impl Range {
    fn bounds(self, rows: u32) -> (i128, i128) {
        let span = self.pitch * i128::from(rows - 1);
        (self.low + span.min(0), self.high + span.max(0))
    }
}

fn range(group: &RowGroup, regs: &[u32; 8], deltas: &[i32; 8], width: u32) -> Option<Range> {
    if group.low >= group.high || !group.read && !group.write || width == 0 {
        return None;
    }
    let mut address = 0_i128;
    let mut pitch = 0_i128;
    if let Some(base) = group.base {
        address += i128::from(*regs.get(base)?);
        pitch += i128::from(*deltas.get(base)?);
    }
    if let Some((index, scale)) = group.index {
        if !matches!(scale, 1 | 2 | 4 | 8) {
            return None;
        }
        address += i128::from(*regs.get(index)?) * i128::from(scale);
        pitch += i128::from(*deltas.get(index)?) * i128::from(scale);
    }
    let span = i128::from(group.step) * i128::from(width - 1);
    Some(Range {
        low: address + i128::from(group.low) + span.min(0),
        high: address + i128::from(group.high) + span.max(0),
        pitch,
        read: group.read,
        write: group.write,
    })
}

fn disjoint(a: (i128, i128), b: (i128, i128)) -> bool {
    a.1 <= b.0 || b.1 <= a.0
}

/// Whether every pair of *different* rows is disjoint. Equal pitches permit
/// same-row aliasing without rejecting interleaved read/write groups. Different
/// pitches conservatively require disjoint full bounding boxes.
fn different_rows_disjoint(a: Range, b: Range, rows: u32) -> bool {
    if rows <= 1 {
        return true;
    }
    if a.pitch != b.pitch {
        return disjoint(a.bounds(rows), b.bounds(rows));
    }
    if a.pitch == 0 {
        return disjoint((a.low, a.high), (b.low, b.high));
    }
    // Intersection requires k*pitch in this inclusive integer interval, with
    // 1 <= abs(k) <= rows-1. Strict half-open overlap becomes +1/-1 here.
    let mut low = a.low - b.high + 1;
    let mut high = a.high - b.low - 1;
    if a.pitch < 0 {
        (low, high) = (-high, -low);
    }
    let pitch = a.pitch.abs();
    let first = -(-low).div_euclid(pitch);
    let last = high.div_euclid(pitch);
    let span = i128::from(rows - 1);
    !(first.max(-span) <= last.min(-1) || first.max(1) <= last.min(span))
}

/// Permission bytes already distinguish ordinary pages from shared audio,
/// lazy file and COW aliases. Forbid internal barriers and executable pages,
/// ignoring only the known derived grants in native metadata.
fn ordinary(perms: &[u8], descriptor: &Descriptor, range: Range, rows: u32) -> bool {
    let (low, high) = range.bounds(rows);
    let base = i128::from(descriptor.memory_base);
    let end = base + descriptor.memory_len as i128;
    if low < base || high > end || high > 1_i128 << 32 || low >= high {
        return false;
    }
    let required = (if range.read { READ } else { 0 }) | if range.write { WRITE } else { 0 };
    let first = (low - base) as usize / PAGE_SIZE;
    let last = (high - base - 1) as usize / PAGE_SIZE;
    let Some(pages) = perms.get(first..=last) else {
        return false;
    };
    let ordinary = READ | WRITE | crate::memory::FAST_READ | crate::memory::FAST_WRITE;
    pages
        .iter()
        .all(|&permission| permission & required == required && permission & !ordinary == 0)
}

fn admission(
    descriptor: &Descriptor,
    state: &CpuState,
    deltas: &[i32; 8],
    perms: &[u8],
    rows: u32,
    width: u32,
) -> bool {
    if descriptor.memory_len == 0
        || !descriptor.memory_len.is_multiple_of(PAGE_SIZE)
        || i128::from(descriptor.memory_base) + descriptor.memory_len as i128 > 1_i128 << 32
    {
        return false;
    }
    for index in 0..8 {
        if descriptor.inductions & (1 << index) == 0 || index == descriptor.inner_counter {
            continue;
        }
        let first = i128::from(state.regs[index]);
        let last = first + i128::from(deltas[index]) * i128::from(rows - 1);
        let inner = i128::from(descriptor.inner_steps[index]) * i128::from(width);
        if [first, last, first + inner, last + inner]
            .iter()
            .any(|&value| !(0..=i128::from(u32::MAX)).contains(&value))
        {
            return false;
        }
    }
    let Some(groups) = descriptor
        .groups
        .iter()
        .map(|group| range(group, &state.regs, deltas, width))
        .collect::<Option<Vec<_>>>()
    else {
        return false;
    };
    if groups.is_empty()
        || groups
            .iter()
            .any(|&group| !ordinary(perms, descriptor, group, rows))
    {
        return false;
    }
    for (index, &a) in groups.iter().enumerate() {
        for &b in &groups[index..] {
            if (a.write || b.write) && !different_rows_disjoint(a, b, rows) {
                return false;
            }
        }
    }
    // Glue runs between rows. Its reads must also remain stable while workers
    // run future rows, so require disjointness even from same-row writes.
    for glue in &descriptor.glue_reads {
        if !glue.read || glue.write || glue.step != 0 {
            return false;
        }
        let Some(glue) = range(glue, &state.regs, deltas, 1) else {
            return false;
        };
        if !ordinary(perms, descriptor, glue, rows - 1)
            || groups
                .iter()
                .filter(|group| group.write)
                .any(|group| !disjoint(glue.bounds(rows - 1), group.bounds(rows)))
        {
            return false;
        }
    }
    true
}

#[derive(Default)]
struct Claims {
    next: AtomicU32,
    stopped: AtomicBool,
}

const CLOSED: u32 = 1 << 31;

/// A queued Arc may outlive the native call, but it may not borrow the native
/// pointers after closing. The gate counts only workers that entered before
/// close; their release follows all kernel accesses and the result merge.
#[derive(Default)]
struct Participants {
    state: AtomicU32,
    wait: Mutex<()>,
    drained: Condvar,
}

struct Participation<'a>(&'a Participants);

impl Participants {
    fn enter(&self) -> Option<Participation<'_>> {
        let mut state = self.state.load(Ordering::Acquire);
        loop {
            if state & CLOSED != 0 {
                return None;
            }
            match self.state.compare_exchange_weak(
                state,
                state + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Some(Participation(self)),
                Err(current) => state = current,
            }
        }
    }

    fn close_and_wait(&self) {
        if self.state.fetch_or(CLOSED, Ordering::AcqRel) & !CLOSED == 0 {
            return;
        }
        let mut wait = lock(&self.wait);
        while self.state.load(Ordering::Acquire) & !CLOSED != 0 {
            wait = self
                .drained
                .wait(wait)
                .unwrap_or_else(|error| error.into_inner());
        }
    }
}

impl Drop for Participation<'_> {
    fn drop(&mut self) {
        if self.0.state.fetch_sub(1, Ordering::AcqRel) == (CLOSED | 1) {
            // Hold the wait mutex while notifying so a last release cannot
            // race the controller's condition check and lose its wakeup.
            let _wait = lock(&self.0.wait);
            self.0.drained.notify_one();
        }
    }
}

struct Results {
    completed: u32,
    ordinals: [Option<u32>; 24],
    regs: [u32; 8],
    mmx: [u64; 8],
    xmm: [[u8; 16]; 8],
    mmx_mode: u32,
    last: Option<(u32, CpuState)>,
}

struct Batch {
    initial: CpuState,
    /// Borrowed addresses stay pinned until the closed participant gate drains.
    descriptor: usize,
    memory: usize,
    permissions: usize,
    interrupt: usize,
    deltas: [i32; 8],
    rows: u32,
    row_cost: u64,
    claims: Claims,
    participants: Participants,
    results: Mutex<Results>,
}

impl Batch {
    fn claim(&self) -> Option<u32> {
        if self.claims.stopped.load(Ordering::Acquire) {
            return None;
        }
        // SAFETY: the interrupt is pinned by the owner's native invocation.
        let interrupt = unsafe { &*(self.interrupt as *const AtomicBool) };
        if interrupt.load(Ordering::Acquire) {
            self.claims.stopped.store(true, Ordering::Release);
            return None;
        }
        // Racing cancellation may admit one more row per participant, but
        // fetch_add still assigns a contiguous prefix and every valid claim
        // finishes. Out-of-range claims contain no work and never enter a
        // kernel. Their only effect is bounded counter overshoot after rows.
        let row = self.claims.next.fetch_add(1, Ordering::Relaxed);
        (row < self.rows).then_some(row)
    }

    fn merge(&self, mut local: Results) {
        if local.completed == 0 {
            return;
        }
        let mut results = lock(&self.results);
        for index in 0..24 {
            let Some(row) = local.ordinals[index] else {
                continue;
            };
            if results.ordinals[index].is_some_and(|previous| previous > row) {
                continue;
            }
            results.ordinals[index] = Some(row);
            match index {
                0..=7 => results.regs[index] = local.regs[index],
                8..=15 => results.mmx[index - 8] = local.mmx[index - 8],
                16..=23 => results.xmm[index - 16] = local.xmm[index - 16],
                _ => unreachable!(),
            }
        }
        results.mmx_mode = combine_mmx_mode(results.mmx_mode, local.mmx_mode);
        results.completed += local.completed;
        if let Some((row, state)) = local.last.take()
            && results.last.as_ref().is_none_or(|(last, _)| *last < row)
        {
            results.last = Some((row, state));
        }
    }
}

fn combine_mmx_mode(old: u32, new: u32) -> u32 {
    ((old | new) & !2) | ((old & new) & 2)
}

impl Results {
    fn new(initial: &CpuState) -> Self {
        Self {
            completed: 0,
            ordinals: [None; 24],
            regs: initial.regs,
            mmx: initial.mmx,
            xmm: initial.xmm,
            mmx_mode: initial.mmx_mode,
            last: None,
        }
    }

    fn complete(&mut self, row: u32, state: CpuState, written: u32, tracked: u32) {
        let mask = written & tracked;
        for index in 0..24 {
            if mask & (1 << index) == 0
                || self.ordinals[index].is_some_and(|previous| previous > row)
            {
                continue;
            }
            self.ordinals[index] = Some(row);
            match index {
                0..=7 => self.regs[index] = state.regs[index],
                8..=15 => self.mmx[index - 8] = state.mmx[index - 8],
                16..=23 => self.xmm[index - 16] = state.xmm[index - 16],
                _ => unreachable!(),
            }
        }
        // EMMS and x87 instructions are excluded by the row compiler. MMX
        // entry and per-slot pending writes are cumulative across rows. An
        // incoming deferred EMMS (bit 1) is cleared by any MMX instruction,
        // even when later rows skip all MMX instructions.
        self.mmx_mode = combine_mmx_mode(self.mmx_mode, state.mmx_mode);
        self.completed += 1;
        if self.last.as_ref().is_none_or(|(last, _)| *last < row) {
            self.last = Some((row, state));
        }
    }
}

static NO_INTERRUPT: AtomicBool = AtomicBool::new(false);

fn execute_worker(batch: &Batch) {
    // These fields are owned by the Arc. If all work is already claimed,
    // avoid entering the native lease merely to discover an empty queue.
    if batch.claims.next.load(Ordering::Relaxed) >= batch.rows
        || batch.claims.stopped.load(Ordering::Acquire)
    {
        return;
    }
    // This must precede every descriptor, memory, permission or interrupt
    // dereference. A late queued worker only drops an owned Arc of a closed
    // batch, whose remaining fields have no borrowed-pointer destructors.
    let Some(_participation) = batch.participants.enter() else {
        return;
    };
    execute(batch);
}

fn execute(batch: &Batch) {
    // SAFETY: the controller borrows all native addresses through dispatch;
    // workers additionally hold a participation guard until this call ends.
    // Admission established disjoint cross-row accesses and stable permissions.
    let descriptor = unsafe { &*(batch.descriptor as *const Descriptor) };
    let kernel: Kernel = unsafe { std::mem::transmute(descriptor.thunk) };
    let mut local = Results::new(&batch.initial);
    while let Some(row) = batch.claim() {
        let mut state = batch.initial.clone();
        for index in 0..8 {
            if descriptor.inductions & (1 << index) != 0 {
                let value = i128::from(state.regs[index])
                    + i128::from(batch.deltas[index]) * i128::from(row);
                let Ok(value) = u32::try_from(value) else {
                    invariant_failure();
                };
                state.regs[index] = value;
            }
        }
        state.regs[descriptor.outer_counter] = batch.initial.regs[descriptor.outer_counter] - row;
        state.eip = descriptor.inner_header;
        state.fuel = batch.row_cost;
        let mut context = NativeContext {
            state: &mut state,
            perms: batch.permissions as *const u8,
            bridge: std::ptr::null_mut(),
            interrupt: &NO_INTERRUPT,
            slots: std::ptr::null(),
            mask: 0,
            remaining: 0,
            stop: 0,
            #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
            fault_reads: 0,
            #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
            recovery_slot: std::ptr::null(),
            linked_entries: 0,
            rows: std::ptr::null(),
            written: 0,
        };
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        crate::jit_memory_macos::synchronize_execution();
        let reason = unsafe { kernel(&mut context, batch.memory as *mut u8, descriptor.target) };
        let counter = state.regs[descriptor.inner_counter];
        let exhausted = if descriptor.counter_size == 2 {
            counter & 0xffff == 0
                && counter & 0xffff_0000
                    == batch.initial.regs[descriptor.inner_counter] & 0xffff_0000
        } else {
            counter == 0
        };
        if reason != Exit::Dispatch as u32
            || state.eip != descriptor.inner_exit
            || !exhausted
            || state.fuel != 0
        {
            // An admitted kernel may have committed stores. Returning a
            // rejection would replay them; terminate on this compiler defect.
            invariant_failure();
        }
        local.complete(
            row,
            state,
            context.written,
            descriptor.tracked & !u32::from(descriptor.invariant),
        );
    }
    batch.merge(local);
}

#[cold]
fn invariant_failure() -> ! {
    use std::io::Write;
    let _ = std::io::stderr().write_all(b"admitted row kernel violated its completion contract\n");
    std::process::abort()
}

/// Returns zero when admission rejects without executing a row, or the exact
/// contiguous row prefix completed at the last row's inner exit. The region
/// executes that row's glue normally; advancing by `rows` here would run it twice.
///
/// SAFETY: context, state, descriptor, eight deltas, memory and permissions must
/// be valid for this complete synchronous invocation. The owner pins mapping
/// and code publication, and the descriptor describes a proved row kernel.
pub(crate) unsafe extern "C" fn run(
    context: *mut NativeContext,
    memory: *mut u8,
    descriptor: *const Descriptor,
    deltas: *const i32,
) -> u32 {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: upheld by the native caller; all participants drain before return.
        unsafe { try_run(context, memory, descriptor, deltas) }
    }));
    result.unwrap_or_else(|_| invariant_failure())
}

unsafe fn try_run(
    context: *mut NativeContext,
    memory: *mut u8,
    descriptor: *const Descriptor,
    deltas: *const i32,
) -> u32 {
    if context.is_null() || descriptor.is_null() || deltas.is_null() || memory.is_null() {
        return 0;
    }
    // SAFETY: all borrowed pointers are valid for the native invocation.
    let context = unsafe { &mut *context };
    if context.rows.is_null() {
        return 0;
    }
    let pool = unsafe { &*context.rows };
    let descriptor = unsafe { &*descriptor };
    if context.state.is_null()
        || context.perms.is_null()
        || context.interrupt.is_null()
        || descriptor.inner_counter >= 8
        || descriptor.outer_counter >= 8
        || descriptor.inner_counter == descriptor.outer_counter
        || !matches!(descriptor.counter_size, 2 | 4)
        || descriptor.thunk == 0
        || descriptor.target == 0
        || descriptor.invariant & descriptor.inductions != 0
        || descriptor.tracked & !0x00ff_ffff != 0
        || (descriptor.outer_header..=descriptor.outer_latch).contains(&context.stop)
    {
        return pool.reject();
    }
    let initial = unsafe { &*context.state };
    let interrupt = unsafe { &*context.interrupt };
    if initial.eip != descriptor.inner_header
        || initial.eflags & (1 << 8) != 0
        || initial.x87.status & 0x80 != 0
        || interrupt.load(Ordering::Acquire)
    {
        return pool.reject();
    }
    let width = if descriptor.counter_size == 2 {
        initial.regs[descriptor.inner_counter] & 0xffff
    } else {
        initial.regs[descriptor.inner_counter]
    };
    let Some(row_cost) = descriptor.inner_cost.checked_mul(u64::from(width)) else {
        return pool.reject();
    };
    let Some(full_cost) = row_cost
        .checked_add(descriptor.glue_cost)
        .filter(|&cost| cost != 0)
    else {
        return pool.reject();
    };
    let outer = initial.regs[descriptor.outer_counter];
    let rows = u64::from(outer.saturating_sub(1))
        .min(initial.fuel / full_cost)
        .min(u64::from(MAX_ROWS)) as u32;
    if width == 0
        || row_cost == 0
        || row_cost > MAX_ROW_COST
        || rows < 2
        || u64::from(rows) * full_cost < pool.threshold
    {
        return pool.reject();
    }
    let deltas: [i32; 8] = unsafe { std::ptr::read(deltas.cast()) };
    if deltas[descriptor.inner_counter] != 0
        || deltas[descriptor.outer_counter] != -1
        || (0..8).any(|index| descriptor.inductions & (1 << index) == 0 && deltas[index] != 0)
    {
        return pool.reject();
    }
    // The memory window is page-aligned and permission bytes have one entry
    // per page. No byte is read until range() proves its page index in bounds.
    // Serial native snapshots may omit the EXEC store barrier on unobserved
    // RWX data. Row admission retains its existing conservative guest EXEC
    // exclusion; row workers are outside the observed-code optimization.
    let perms = if context.bridge.is_null() {
        // Authored standalone kernel tests provide their own permission table.
        unsafe { std::slice::from_raw_parts(context.perms, descriptor.memory_len / PAGE_SIZE) }
    } else {
        let bridge = unsafe { &*context.bridge.cast::<crate::trap::Bridge<'_>>() };
        if bridge.memory.is_null() {
            return pool.reject();
        }
        let owner = unsafe { &*bridge.memory };
        if owner.base != descriptor.memory_base || owner.len != descriptor.memory_len {
            return pool.reject();
        }
        owner.perms.as_slice()
    };
    if !admission(descriptor, initial, &deltas, perms, rows, width) {
        return pool.reject();
    }
    let _submission = lock(&pool.submission);
    let batch = Arc::new(Batch {
        initial: initial.clone(),
        descriptor: descriptor as *const Descriptor as usize,
        memory: memory as usize,
        permissions: context.perms as usize,
        interrupt: context.interrupt as usize,
        deltas,
        rows,
        row_cost,
        claims: Claims::default(),
        participants: Participants::default(),
        results: Mutex::new(Results::new(initial)),
    });
    pool.dispatch(&batch);
    let claimed = batch.claims.next.load(Ordering::Acquire).min(batch.rows);
    let mut results = lock(&batch.results);
    if results.completed != claimed {
        invariant_failure();
    }
    let Some((last_row, mut merged)) = results.last.take() else {
        return pool.reject();
    };
    if last_row + 1 != claimed {
        invariant_failure();
    }
    for index in 0..24 {
        if results.ordinals[index].is_some() {
            match index {
                0..=7 => merged.regs[index] = results.regs[index],
                8..=15 => merged.mmx[index - 8] = results.mmx[index - 8],
                16..=23 => merged.xmm[index - 16] = results.xmm[index - 16],
                _ => unreachable!(),
            }
        }
    }
    merged.mmx_mode = results.mmx_mode;
    merged.fuel = batch.initial.fuel
        - (u64::from(claimed) * row_cost + u64::from(claimed - 1) * descriptor.glue_cost);
    // The last kernel contains cur + (claimed-1)*delta and the outer counter
    // before its glue. Both are already the precise serial inner-exit state.
    unsafe { *context.state = merged };
    pool.rows_completed
        .fetch_add(u64::from(claimed), Ordering::Relaxed);
    pool.batches.fetch_add(1, Ordering::Relaxed);
    claimed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn access(low: i128, high: i128, pitch: i128) -> Range {
        Range {
            low,
            high,
            pitch,
            read: true,
            write: true,
        }
    }

    #[test]
    fn signed_pitch_proof_matches_enumerated_cross_row_pairs() {
        for a_pitch in -4..=4 {
            for b_pitch in -4..=4 {
                for offset in -8..=8 {
                    let a = access(0, 3, a_pitch);
                    let b = access(offset, offset + 2, b_pitch);
                    for rows in 2..=6 {
                        let enumerated = (0..rows).all(|i| {
                            (0..rows).all(|j| {
                                i == j
                                    || disjoint(
                                        (
                                            a.low + i128::from(i) * a.pitch,
                                            a.high + i128::from(i) * a.pitch,
                                        ),
                                        (
                                            b.low + i128::from(j) * b.pitch,
                                            b.high + i128::from(j) * b.pitch,
                                        ),
                                    )
                            })
                        });
                        let proved = different_rows_disjoint(a, b, rows);
                        assert!(!proved || enumerated, "{a:?}, {b:?}, {rows}");
                        if a_pitch == b_pitch {
                            assert_eq!(proved, enumerated, "{a:?}, {b:?}, {rows}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn zero_pitch_writes_and_touching_rows() {
        assert!(!different_rows_disjoint(
            access(4, 8, 0),
            access(4, 8, 0),
            2
        ));
        assert!(different_rows_disjoint(
            access(4, 8, 4),
            access(4, 8, 4),
            10
        ));
        assert!(different_rows_disjoint(
            access(100, 104, -4),
            access(100, 104, -4),
            10
        ));
        assert!(!different_rows_disjoint(
            access(4, 9, 4),
            access(4, 9, 4),
            10
        ));
    }

    fn descriptor() -> Descriptor {
        Descriptor {
            groups: vec![RowGroup {
                base: Some(6),
                index: None,
                step: 4,
                low: 0,
                high: 4,
                read: false,
                write: true,
            }],
            glue_reads: vec![],
            memory_base: 0x10000,
            memory_len: PAGE_SIZE,
            outer_header: 0x10000,
            outer_latch: 0x10040,
            inner_header: 0x10010,
            inner_exit: 0x10020,
            inner_counter: 1,
            counter_size: 4,
            outer_counter: 2,
            inductions: (1 << 6) | (1 << 2),
            invariant: 0,
            inner_steps: [0, -1, 0, 0, 0, 0, 4, 0],
            tracked: 1 | (1 << 8) | (1 << 16),
            inner_cost: 1,
            glue_cost: 1,
            thunk: fake_kernel as *const () as usize,
            target: 1,
        }
    }

    #[test]
    fn ordinary_bounds_reject_aliases_executable_pages_and_wrapping() {
        let descriptor = descriptor();
        let ordinary_range = access(0x10000, 0x10004, 4);
        assert!(ordinary(&[READ | WRITE], &descriptor, ordinary_range, 16));
        let derived = crate::memory::FAST_READ | crate::memory::FAST_WRITE;
        assert!(ordinary(
            &[READ | WRITE | derived],
            &descriptor,
            ordinary_range,
            16
        ));
        for bit in [
            crate::memory::EXEC,
            crate::memory::SHARED,
            crate::memory::AUDIO,
            0x80, // Unknown metadata bits must not silently bypass admission.
        ] {
            assert!(!ordinary(
                &[READ | WRITE | bit],
                &descriptor,
                ordinary_range,
                16
            ));
        }
        assert!(!ordinary(&[READ], &descriptor, ordinary_range, 16));
        assert!(!ordinary(
            &[READ | WRITE],
            &descriptor,
            access(0xffff, 0x10003, 4),
            2
        ));
        assert!(!ordinary(
            &[READ | WRITE],
            &descriptor,
            access(0x10ffc, 0x11000, 4),
            2
        ));
        assert!(!ordinary(
            &[READ | WRITE],
            &descriptor,
            access(i128::from(u32::MAX), 1_i128 << 32, 4),
            2
        ));
    }

    #[test]
    fn glue_reads_must_miss_all_batch_writes() {
        let mut descriptor = descriptor();
        let mut state = CpuState::default();
        state.regs[6] = 0x10000;
        state.regs[2] = 10;
        let deltas = [0, 0, -1, 0, 0, 0, 8, 0];
        assert!(admission(
            &descriptor,
            &state,
            &deltas,
            &[READ | WRITE],
            4,
            2
        ));
        descriptor.glue_reads.push(RowGroup {
            base: Some(6),
            index: None,
            step: 0,
            low: 0,
            high: 4,
            read: true,
            write: false,
        });
        assert!(!admission(
            &descriptor,
            &state,
            &deltas,
            &[READ | WRITE],
            4,
            2
        ));
        descriptor.glue_reads[0].base = None;
        descriptor.glue_reads[0].low = 0x10800;
        descriptor.glue_reads[0].high = 0x10804;
        assert!(admission(
            &descriptor,
            &state,
            &deltas,
            &[READ | WRITE],
            4,
            2
        ));
    }

    unsafe extern "C" fn fake_kernel(
        context: *mut NativeContext,
        memory: *mut u8,
        _: usize,
    ) -> u32 {
        let context = unsafe { &mut *context };
        let state = unsafe { &mut *context.state };
        let row = (state.regs[6] - 0x10000) / 8;
        let value = row as u8;
        // Deliberately return out of order; a conditional register write must
        // retain the greatest guest ordinal, not the final host completion.
        if row.is_multiple_of(3) {
            thread::yield_now();
        }
        unsafe { *memory.add((state.regs[6] - 0x10000) as usize) = value };
        if row % 3 == 1 {
            state.regs[0] = row;
            state.mmx[0] = u64::from(row);
            state.xmm[0].fill(row as u8);
            state.mmx_mode = (state.mmx_mode & !2) | 5 | (1 << 8);
            context.written |= 1 | (1 << 8) | (1 << 16);
        }
        state.regs[6] += state.regs[1] * 4;
        state.regs[1] = 0;
        state.fuel = 0;
        state.eip = 0x10020;
        state.eflags = row | 2;
        Exit::Dispatch as u32
    }

    #[test]
    fn persistent_pool_merges_guest_order_and_charges_exact_prefix() {
        let mut pool = RowPool::new(3).unwrap();
        pool.set_threshold(0);
        let mut descriptor = descriptor();
        let permissions = [READ | WRITE];
        let interrupt = AtomicBool::new(false);
        let deltas = [0, 0, -1, 0, 0, 0, 8, 0];
        let mut memory = vec![0_u8; PAGE_SIZE];
        for invariant in [0, 1, 0] {
            descriptor.invariant = invariant;
            let mut state = CpuState::default();
            state.regs[6] = 0x10000;
            state.regs[1] = 2;
            state.regs[2] = 20;
            state.regs[0] = 99;
            state.mmx_mode = 2; // A prior deferred EMMS.
            state.eip = descriptor.inner_header;
            state.fuel = 45; // 15 rows, with the final glue still pending.
            let mut context = NativeContext {
                state: &mut state,
                perms: permissions.as_ptr(),
                bridge: std::ptr::null_mut(),
                interrupt: &interrupt,
                slots: std::ptr::null(),
                mask: 0,
                remaining: 0,
                stop: 0,
                #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
                fault_reads: 0,
                #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
                recovery_slot: std::ptr::null(),
                linked_entries: 0,
                rows: &pool,
                written: 0,
            };
            let rows = unsafe {
                run(
                    &mut context,
                    memory.as_mut_ptr(),
                    &descriptor,
                    deltas.as_ptr(),
                )
            };
            assert_eq!(rows, 15);
            // Row 14 does not write EAX. A prefix reset must supersede the
            // earlier row-13 writer; a carried temporary keeps that writer.
            assert_eq!(state.regs[0], if invariant == 1 { 99 } else { 13 });
            assert_eq!(state.mmx[0], 13);
            assert_eq!(state.xmm[0], [13; 16]);
            assert_eq!(state.mmx_mode, 5 | (1 << 8));
            assert_eq!(state.regs[6], 0x10000 + 15 * 8);
            assert_eq!(state.regs[2], 20 - 14); // Last glue executes in region.
            assert_eq!(state.regs[1], 0);
            assert_eq!(state.eflags, 14 | 2);
            assert_eq!(state.fuel, 1); // 15*2 + 14*1.
            for row in 0..15 {
                assert_eq!(memory[row * 8], row as u8);
            }
        }
        assert_eq!(pool.batches(), 3);
        assert_eq!(pool.rows_completed(), 45);
        assert_eq!(pool.rejected(), 0);
    }

    unsafe extern "C" fn cancelling_kernel(
        context: *mut NativeContext,
        memory: *mut u8,
        target: usize,
    ) -> u32 {
        // The child's interrupt remains NO_INTERRUPT. Only subsequent claims
        // consult the owner's flag; every already-claimed kernel finishes.
        let row = unsafe { ((*(*context).state).regs[6] - 0x10000) / 8 };
        let result = unsafe { fake_kernel(context, memory, 0) };
        if row >= 3 {
            let interrupt = unsafe { &*(target as *const AtomicBool) };
            interrupt.store(true, Ordering::Release);
        }
        result
    }

    #[test]
    fn cancellation_finishes_and_merges_the_claimed_row_prefix() {
        const OUTER: u32 = 25;
        const FUEL: u64 = 100;
        for workers in [0, 3] {
            let mut pool = RowPool::new(workers).unwrap();
            pool.set_threshold(0);
            let interrupt = AtomicBool::new(false);
            let mut descriptor = descriptor();
            descriptor.thunk = cancelling_kernel as *const () as usize;
            descriptor.target = &interrupt as *const AtomicBool as usize;
            let permissions = [READ | WRITE];
            let deltas = [0, 0, -1, 0, 0, 0, 8, 0];
            let mut memory = vec![0xa5_u8; PAGE_SIZE];
            let mut state = CpuState {
                eip: descriptor.inner_header,
                fuel: FUEL,
                mmx_mode: 2,
                ..CpuState::default()
            };
            state.regs[0] = 99;
            state.regs[1] = 2;
            state.regs[2] = OUTER;
            state.regs[6] = 0x10000;
            let mut context = NativeContext {
                state: &mut state,
                perms: permissions.as_ptr(),
                bridge: std::ptr::null_mut(),
                interrupt: &interrupt,
                slots: std::ptr::null(),
                mask: 0,
                remaining: 0,
                stop: 0,
                #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
                fault_reads: 0,
                #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
                recovery_slot: std::ptr::null(),
                linked_entries: 0,
                rows: &pool,
                written: 0,
            };
            let rows = unsafe {
                run(
                    &mut context,
                    memory.as_mut_ptr(),
                    &descriptor,
                    deltas.as_ptr(),
                )
            };
            // Scheduling may admit extra rows before a participant publishes
            // cancellation. Every ordinal below the returned count must finish.
            assert!((4..OUTER).contains(&rows), "workers={workers}, rows={rows}");
            if workers == 0 {
                assert_eq!(rows, 4);
            }
            assert!(interrupt.load(Ordering::Acquire));
            let mut expected = vec![0xa5_u8; PAGE_SIZE];
            for row in 0..rows {
                expected[row as usize * 8] = row as u8;
            }
            assert_eq!(memory, expected, "workers={workers}: memory prefix");
            let last_writer = (0..rows).rev().find(|row| row % 3 == 1).unwrap();
            assert_eq!(state.regs[0], last_writer);
            assert_eq!(state.mmx[0], u64::from(last_writer));
            assert_eq!(state.xmm[0], [last_writer as u8; 16]);
            assert_eq!(state.mmx_mode, 5 | (1 << 8));
            assert_eq!(state.regs[6], 0x10000 + rows * 8);
            assert_eq!(state.regs[2], OUTER - (rows - 1));
            assert_eq!(state.regs[1], 0);
            assert_eq!(state.eip, descriptor.inner_exit);
            assert_eq!(state.eflags, (rows - 1) | 2);
            assert_eq!(
                state.fuel,
                FUEL - u64::from(rows) * 2 - u64::from(rows - 1),
                "workers={workers}: exact prefix charge"
            );
            assert_eq!(pool.rows_completed(), u64::from(rows));
            assert_eq!(pool.batches(), 1);
            assert_eq!(pool.rejected(), 0);
            // Dropping joins late queued workers. The returned memory prefix
            // must already be complete and receive no later writes.
            drop(pool);
            assert_eq!(memory, expected, "workers={workers}: late memory writer");
        }
    }

    #[test]
    fn interruption_claims_only_a_contiguous_prefix() {
        let interrupt = AtomicBool::new(false);
        let batch = Batch {
            initial: CpuState::default(),
            descriptor: 0,
            memory: 0,
            permissions: 0,
            interrupt: &interrupt as *const AtomicBool as usize,
            deltas: [0; 8],
            rows: 100,
            row_cost: 1,
            claims: Claims::default(),
            participants: Participants::default(),
            results: Mutex::new(Results {
                completed: 0,
                ordinals: [None; 24],
                regs: [0; 8],
                mmx: [0; 8],
                xmm: [[0; 16]; 8],
                mmx_mode: 0,
                last: None,
            }),
        };
        let claims = thread::scope(|scope| {
            let batch = &batch;
            let interrupt = &interrupt;
            let handles: Vec<_> = (0..4)
                .map(|_| {
                    scope.spawn(move || {
                        let mut rows = Vec::new();
                        while let Some(row) = batch.claim() {
                            rows.push(row);
                            if row >= 9 {
                                interrupt.store(true, Ordering::Release);
                            }
                            thread::yield_now();
                        }
                        rows
                    })
                })
                .collect();
            handles
                .into_iter()
                .flat_map(|handle| handle.join().unwrap())
                .collect::<Vec<_>>()
        });
        let mut claims = claims;
        claims.sort_unstable();
        assert_eq!(claims, (0..claims.len() as u32).collect::<Vec<_>>());
        assert!(claims.len() >= 10 && claims.len() < 100);
        interrupt.store(false, Ordering::Release);
        assert_eq!(batch.claim(), None); // Cancellation is sticky for this batch.
        batch.claims.stopped.store(false, Ordering::Release);
        assert_eq!(batch.claim(), Some(claims.len() as u32));
    }

    #[test]
    fn deferred_emms_is_cleared_by_an_early_mmx_row_in_both_completion_orders() {
        let mut initial = CpuState::default();
        initial.mmx_mode = 2;
        let mut early = initial.clone();
        early.mmx[0] = 7;
        early.mmx_mode = 5 | (1 << 8);
        for reverse in [false, true] {
            let mut results = Results::new(&initial);
            if reverse {
                results.complete(1, initial.clone(), 0, 1 << 8);
                results.complete(0, early.clone(), 1 << 8, 1 << 8);
            } else {
                results.complete(0, early.clone(), 1 << 8, 1 << 8);
                results.complete(1, initial.clone(), 0, 1 << 8);
            }
            assert_eq!(results.mmx_mode, 5 | (1 << 8));
            assert_eq!(results.mmx[0], 7);
            assert_eq!(results.ordinals[8], Some(0));
            assert_eq!(results.last.as_ref().unwrap().0, 1);
        }
    }

    fn closed_batch() -> Arc<Batch> {
        let initial = CpuState::default();
        let results = Mutex::new(Results::new(&initial));
        let batch = Arc::new(Batch {
            initial,
            // A late queued batch need not retain valid native pointers.
            descriptor: usize::MAX,
            memory: usize::MAX,
            permissions: usize::MAX,
            interrupt: usize::MAX,
            deltas: [0; 8],
            rows: 1,
            row_cost: 1,
            claims: Claims::default(),
            participants: Participants::default(),
            results,
        });
        batch.participants.close_and_wait();
        batch
    }

    #[test]
    fn closed_queued_batch_skips_native_pointer_accesses() {
        let batch = closed_batch();
        execute_worker(&batch);
        assert_eq!(batch.claims.next.load(Ordering::Relaxed), 0);
        assert_eq!(lock(&batch.results).completed, 0);
    }

    #[test]
    fn mailbox_replaces_closed_stale_jobs() {
        let mailbox = Mailbox::default();
        let stale = closed_batch();
        let latest = closed_batch();
        mailbox.submit(stale);
        mailbox.submit(latest.clone());
        let queued = mailbox.receive().unwrap();
        assert!(Arc::ptr_eq(&queued, &latest));
        execute_worker(&queued);
        mailbox.shutdown();
        assert!(mailbox.receive().is_none());
    }

    #[test]
    fn closing_waits_for_entered_workers_and_rejects_late_workers() {
        let participants = Participants::default();
        let entered = participants.enter().unwrap();
        thread::scope(|scope| {
            let closing = scope.spawn(|| participants.close_and_wait());
            while participants.state.load(Ordering::Acquire) & CLOSED == 0 {
                thread::yield_now();
            }
            assert_eq!(participants.state.load(Ordering::Acquire), CLOSED | 1);
            assert!(participants.enter().is_none());
            drop(entered);
            closing.join().unwrap();
        });
        assert_eq!(participants.state.load(Ordering::Acquire), CLOSED);
    }
}
