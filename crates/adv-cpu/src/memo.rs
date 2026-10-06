//! Guest function result reuse. A hooked guest PC is the entry of a code
//! sequence that ends when it returns past its entry stack pointer. The first
//! executions record, through a hook on every guest access, which bytes the
//! sequence read before writing them (with their values) and which bytes it
//! wrote (with their final values). A later entry with an identical canonical
//! CPU state, identical recorded input bytes, the same page permissions and
//! unchanged code applies the recorded writes and final state instead of
//! executing: deterministic code given the same inputs produces the same
//! effects. Anything that is not a plain guest access (traps, REP helpers,
//! x87 helpers, the timestamp counter, shared or file pages, interpreter
//! steps, faults) aborts the recording, so such code never gets an entry.
//!
//! Hooks come from sampled guest stacks: functions whose frames are present
//! in a large share of samples are hooked and their owner regions recompiled.
use std::collections::{BTreeSet, HashMap};
use std::time::{Duration, Instant};

use crate::memory::{EXEC, READ, SHARED, WRITE};
use crate::{CpuState, Memory};

const PAGE: usize = 4096;
/// Work budget while recording; blocks executed are measured from it.
pub(crate) const RECORD_FUEL: u64 = 1 << 40;
/// A recording that runs longer than this many blocks is abandoned.
pub(crate) const RECORD_LIMIT: u64 = 1 << 28;
/// Distinct guest pages one recording may touch.
const RECORD_PAGES: usize = 1 << 16;
const ENTRIES: usize = 256;
/// Entries per entry state, which differ only in memory inputs.
const VARIANTS: usize = 8;
const BUDGET: usize = 256 << 20;
const SEEN: usize = 512;
/// Calls per profitability window of an active function.
const WINDOW: u32 = 4096;
/// Failures (impure code, no benefit) before a function is never retried.
const STRIKES: u32 = 3;
const SAMPLE_PERIOD: Duration = Duration::from_millis(1);
const DECIDE_PERIOD: Duration = Duration::from_secs(1);
/// Minimum share of stack samples, in percent, that hooks a function.
const SHARE: u32 = 5;
const STACK_WORDS: u32 = 256;
const FRAMES: usize = 8;

/// Tunables; the defaults suit interactive guests.
#[derive(Clone, Debug)]
pub struct MemoOptions {
    /// Sightings of one entry state before it is recorded.
    pub sightings: u32,
    /// Minimum time between two recordings, which stall the guest.
    pub record_gap: Duration,
    /// Hook functions found by stack sampling.
    pub automatic: bool,
}

impl Default for MemoOptions {
    fn default() -> Self {
        Self {
            sightings: 3,
            record_gap: Duration::from_millis(20),
            automatic: true,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct MemoStats {
    pub hits: u64,
    /// Entries whose state matched but whose input bytes did not.
    pub misses: u64,
    pub recordings: u64,
    pub entries: u64,
    pub aborts: u64,
    /// Completed recordings that cost more to check than to execute.
    pub unprofitable: u64,
    pub hooked: usize,
    pub bytes: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Abort {
    /// Code that is not a plain sequence of guest accesses.
    Impure,
    /// Too long to record.
    Limit,
    /// The execution slice ended; not a property of the function.
    Context,
}

struct Page {
    read: [u64; PAGE / 64],
    written: [u64; PAGE / 64],
    values: [u8; PAGE],
}

impl Page {
    fn new() -> Box<Self> {
        Box::new(Self {
            read: [0; PAGE / 64],
            written: [0; PAGE / 64],
            values: [0; PAGE],
        })
    }
}

fn bit(bits: &[u64; PAGE / 64], index: usize) -> bool {
    bits[index / 64] & (1 << (index % 64)) != 0
}

fn set(bits: &mut [u64; PAGE / 64], index: usize) {
    bits[index / 64] |= 1 << (index % 64);
}

/// One recording in progress. Generated record-mode code reports every guest
/// access here before performing it.
pub(crate) struct Recorder {
    function: u32,
    key: Box<CpuState>,
    hash: u64,
    pub(crate) ret: u32,
    pub(crate) entry_esp: u32,
    pages: HashMap<usize, Box<Page>>,
    /// Pages in first-access order; early inputs (arguments) tell entries
    /// with the same state apart soonest.
    order: Vec<usize>,
    poisoned: bool,
    /// Diagnostic: the access that poisoned this recording.
    why: String,
    pub(crate) armed: bool,
    pub(crate) saved_fuel: u64,
    /// x87 last-instruction fields (see `x87_pointers`) some interpreted
    /// instruction changed.
    pub(crate) x87_written: u8,
    /// Physical x87 registers some interpreted instruction loaded or changed.
    pub(crate) x87_touched: u8,
    /// Set when an instruction could observe empty x87 registers: MMX
    /// aliases, environment stores and restores.
    pub(crate) x87_opaque: bool,
    /// Set when an instruction could read, clear or unmask the x87
    /// exception flags.
    pub(crate) x87_status: bool,
    /// x87 condition code bits some interpreted instruction changed.
    pub(crate) x87_conditions: u16,
}

impl Recorder {
    fn poison(&mut self, why: String) {
        self.poisoned = true;
        self.why = why;
    }

    pub(crate) fn access(&mut self, memory: &Memory, addr: u32, len: u32, write: bool) {
        if self.poisoned {
            return;
        }
        let Ok(range) = memory.range(addr, len as usize) else {
            self.poison(format!("outside {addr:#x}"));
            return;
        };
        let need = if write { WRITE } else { READ };
        let mut offset = range.start;
        while offset < range.end {
            let page = offset / PAGE;
            let end = range.end.min((page + 1) * PAGE);
            let permission = memory.perms[page];
            if permission & need == 0 || permission & SHARED != 0 || !memory.mapped[page] {
                // The access itself faults or needs a helper; either aborts.
                self.poison(format!("{addr:#x} perms {permission:#x} write={write}"));
                return;
            }
            if self.pages.len() >= RECORD_PAGES && !self.pages.contains_key(&page) {
                self.poison("too many pages".into());
                return;
            }
            let record = self.pages.entry(page).or_insert_with(|| {
                self.order.push(page);
                Page::new()
            });
            for at in offset..end {
                let index = at % PAGE;
                if write {
                    set(&mut record.written, index);
                } else if !bit(&record.written, index) && !bit(&record.read, index) {
                    set(&mut record.read, index);
                    // SAFETY: in-window offset of a mapped page; generated
                    // code is paused inside this call.
                    record.values[index] = unsafe { *memory.ptr.as_ptr().add(at) };
                }
            }
            offset = end;
        }
    }
}

/// Coalesced byte runs with their data, in window offsets.
#[derive(Default)]
struct Runs {
    runs: Vec<(usize, usize)>,
    data: Vec<u8>,
    pages: Vec<usize>,
}

impl Runs {
    fn push(&mut self, offset: usize, byte: u8) {
        match self.runs.last_mut() {
            Some((start, len)) if *start + *len == offset => *len += 1,
            _ => self.runs.push((offset, 1)),
        }
        self.data.push(byte);
        let page = offset / PAGE;
        if self.pages.last() != Some(&page) {
            self.pages.push(page);
        }
    }
}

struct Entry {
    key: Box<CpuState>,
    hash: u64,
    reads: Runs,
    writes: Runs,
    code: Vec<(usize, u64)>,
    /// x87 pointer fields the result passes through; `key` holds only these.
    x87_inputs: u8,
    /// Physical registers empty on entry whose stale contents nothing
    /// observed; `key` omits them.
    x87_free: u8,
    /// The free registers the function left alone; a hit keeps the caller's.
    x87_kept: u8,
    /// When nothing observed the x87 exception flags: those set on entry.
    /// The function only adds flags, so any caller with at least these ends
    /// with its own flags plus the recorded result's.
    sticky: Option<u16>,
    /// x87 status bits outside the key: unobserved sticky flags and
    /// condition codes the function overwrites.
    status_free: u16,
    out: Box<CpuState>,
    /// Memory epoch at which permissions and code versions last matched.
    epoch: u64,
    hits: u64,
    used: u64,
}

impl Entry {
    fn bytes(&self) -> usize {
        self.reads.data.len() + self.writes.data.len() + 2 * std::mem::size_of::<CpuState>()
    }

    /// Diagnostic: input runs that differ now, as (guest address, length).
    fn differences(&self, memory: &Memory) -> (usize, Vec<(u32, usize)>) {
        let mut at = 0;
        let mut total = 0;
        let mut runs = Vec::new();
        for &(offset, len) in &self.reads.runs {
            let live = unsafe { std::slice::from_raw_parts(memory.ptr.as_ptr().add(offset), len) };
            let old = &self.reads.data[at..at + len];
            let mut index = 0;
            while index < len {
                if live[index] == old[index] {
                    index += 1;
                    continue;
                }
                let start = index;
                while index < len && live[index] != old[index] {
                    index += 1;
                }
                total += index - start;
                if runs.len() < 6 {
                    runs.push((memory.base + (offset + start) as u32, index - start));
                }
            }
            at += len;
        }
        (total, runs)
    }

    /// Whether `probe`, the canonical entry state, equals the key once
    /// pointer fields this entry overwrites are cleared.
    fn same_key(&self, hash: u64, key: &CpuState, probe: &mut CpuState) -> bool {
        if self.hash != hash {
            return false;
        }
        if let Some(sticky) = self.sticky
            && key.x87.status & sticky != sticky
        {
            return false;
        }
        set_x87_pointers(probe, x87_pointers(key));
        probe.x87.regs = key.x87.regs;
        probe.mmx = key.mmx;
        probe.x87.status = key.x87.status;
        mask_key(probe, self.x87_inputs, self.x87_free, self.status_free);
        *self.key == *probe
    }

    fn matches(&mut self, memory: &Memory) -> bool {
        if memory.epoch != self.epoch {
            let readable =
                self.reads.pages.iter().all(|&page| {
                    memory.mapped[page] && memory.perms[page] & (READ | SHARED) == READ
                });
            let writable = self.writes.pages.iter().all(|&page| {
                memory.mapped[page] && memory.perms[page] & (WRITE | SHARED | EXEC) == WRITE
            });
            let code = self
                .code
                .iter()
                .all(|&(page, version)| memory.versions[page] == version);
            if !(readable && writable && code) {
                return false;
            }
            self.epoch = memory.epoch;
        }
        let mut at = 0;
        for &(offset, len) in &self.reads.runs {
            // SAFETY: pages were validated as mapped and readable above, and
            // every permission or mapping change advances the epoch.
            let live = unsafe { std::slice::from_raw_parts(memory.ptr.as_ptr().add(offset), len) };
            if live != &self.reads.data[at..at + len] {
                return false;
            }
            at += len;
        }
        true
    }

    fn apply(&self, state: &mut CpuState, memory: &Memory) {
        let mut at = 0;
        for &(offset, len) in &self.writes.runs {
            // SAFETY: validated private writable pages; no generated code
            // holds a reference across this helper call.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    self.writes.data.as_ptr().add(at),
                    memory.ptr.as_ptr().add(offset),
                    len,
                );
            }
            at += len;
        }
        let fuel = state.fuel;
        let fault_addr = state.fault_addr;
        state.refresh_mmx();
        let regs = state.x87.regs;
        let status = state.x87.status;
        state.clone_from(&self.out);
        if self.sticky.is_some() {
            state.x87.status |= status & STICKY;
        }
        for slot in 0..8 {
            if self.x87_kept & 1 << slot != 0 {
                state.x87.regs[slot] = regs[slot];
                state.mmx[slot] = regs[slot].significand();
            }
        }
        state.fuel = fuel;
        state.fault_addr = fault_addr;
        state.rep = None;
    }
}

enum Off {
    Until(Instant),
    Forever,
}

struct Function {
    entries: Vec<Entry>,
    seen: Vec<(u64, u32)>,
    off: Option<Off>,
    hooked: bool,
    failures: u32,
    /// Delay before the next recording; grows while recordings go unused.
    gap: Duration,
    next_record: Option<Instant>,
    window_calls: u32,
    window_hits: u32,
}

impl Function {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
            seen: Vec::new(),
            off: None,
            hooked: true,
            failures: 0,
            gap: Duration::ZERO,
            next_record: None,
            window_calls: 0,
            window_hits: 0,
        }
    }

    fn sighting(&mut self, hash: u64) -> u32 {
        if let Some((_, count)) = self.seen.iter_mut().find(|(seen, _)| *seen == hash) {
            *count += 1;
            return *count;
        }
        if self.seen.len() >= SEEN {
            self.seen.remove(0);
        }
        self.seen.push((hash, 1));
        1
    }

    fn slower(&mut self) {
        self.gap = (self.gap * 2).clamp(Duration::from_millis(50), Duration::from_secs(10));
    }
}

/// Canonical entry state: everything a guest instruction can observe, with
/// pending MMX state committed. Bookkeeping fields do not take part.
fn canonical(state: &CpuState) -> Box<CpuState> {
    let mut key = Box::new(state.clone());
    key.refresh_mmx();
    key.eip = 0;
    key.fault_addr = 0;
    key.fuel = 0;
    key.rep = None;
    key.last_indirect_branch = Default::default();
    key
}

/// The x87 last-instruction opcode and pointers. Only environment stores
/// observe them; a recording either overwrites a field or passes it through,
/// and only a passed-through field is an input.
pub(crate) fn x87_pointers(state: &CpuState) -> [u32; 5] {
    let x87 = &state.x87;
    [
        u32::from(x87.opcode),
        x87.ip,
        x87.dp,
        u32::from(x87.cs),
        u32::from(x87.ds),
    ]
}

fn set_x87_pointers(state: &mut CpuState, pointers: [u32; 5]) {
    let x87 = &mut state.x87;
    x87.opcode = pointers[0] as u16;
    x87.ip = pointers[1];
    x87.dp = pointers[2];
    x87.cs = pointers[3] as u16;
    x87.ds = pointers[4] as u16;
}

/// `state` with the x87 pointer fields outside `inputs` cleared.
fn mask_x87_pointers(state: &mut CpuState, inputs: u8) {
    let mut pointers = x87_pointers(state);
    for (index, pointer) in pointers.iter_mut().enumerate() {
        if inputs & 1 << index == 0 {
            *pointer = 0;
        }
    }
    set_x87_pointers(state, pointers);
}

/// Physical x87 registers tagged empty.
pub(crate) fn x87_empty(state: &CpuState) -> u8 {
    (0..8).fold(0, |empty, slot| {
        if state.x87.tag >> (2 * slot) & 3 == 3 {
            empty | 1 << slot
        } else {
            empty
        }
    })
}

/// Sticky x87 exception and stack fault flags.
const STICKY: u16 = 0x7f;
/// x87 condition codes C0–C3.
pub(crate) const CONDITIONS: u16 = 0x4700;

/// Clears what the key of an entry does not depend on: x87 pointer fields
/// outside `pointers`, the contents of the physical registers in `free` and
/// the x87 status bits in `status`.
fn mask_key(state: &mut CpuState, pointers: u8, free: u8, status: u16) {
    mask_x87_pointers(state, pointers);
    state.x87.status &= !status;
    for slot in 0..8 {
        if free & 1 << slot != 0 {
            state.x87.regs[slot] = Default::default();
            state.mmx[slot] = 0;
        }
    }
}

fn hash(state: &CpuState) -> u64 {
    let mut hash = 0u64;
    let mut mix = |value: u64| {
        hash = (hash.rotate_left(5) ^ value).wrapping_mul(0x517c_c1b7_2722_0a95);
    };
    for value in state.regs {
        mix(u64::from(value));
    }
    mix(u64::from(state.eflags));
    for xmm in &state.xmm {
        mix(u64::from_le_bytes(xmm[..8].try_into().unwrap()));
        mix(u64::from_le_bytes(xmm[8..].try_into().unwrap()));
    }
    // Stale contents of empty registers are inputs only to some entries.
    let empty = x87_empty(state);
    for (slot, value) in state.mmx.into_iter().enumerate() {
        if empty & 1 << slot == 0 {
            mix(value);
        }
    }
    let status = state.x87.status & !(STICKY | CONDITIONS);
    mix(u64::from(status) | u64::from(state.x87.control) << 16);
    hash
}

/// Bytes of ordinary guest memory with permission `need` on every page.
fn guest(memory: &Memory, addr: u32, len: usize, need: u8) -> Option<&[u8]> {
    let range = memory.range(addr, len).ok()?;
    for page in range.start / PAGE..=(range.end - 1) / PAGE {
        if !memory.mapped[page] || memory.perms[page] & (need | SHARED) != need {
            return None;
        }
    }
    // SAFETY: in-window mapped private pages; the owner thread is paused.
    Some(unsafe { std::slice::from_raw_parts(memory.ptr.as_ptr().add(range.start), len) })
}

fn word(memory: &Memory, addr: u32) -> Option<u32> {
    guest(memory, addr, 4, READ).map(|bytes| u32::from_le_bytes(bytes.try_into().unwrap()))
}

/// The target of the direct near call that returns to `ret`.
fn call_target(memory: &Memory, ret: u32) -> Option<u32> {
    let site = guest(memory, ret.checked_sub(5)?, 5, READ | EXEC)?;
    if site[0] != 0xe8 {
        return None;
    }
    let target = ret.wrapping_add(u32::from_le_bytes(site[1..].try_into().unwrap()));
    guest(memory, target, 1, READ | EXEC).map(|_| target)
}

#[derive(Default)]
struct Sampler {
    counts: HashMap<u32, u32>,
    samples: u32,
    next_sample: Option<Instant>,
    next_decide: Option<Instant>,
}

pub(crate) struct Memo {
    options: MemoOptions,
    hooks: BTreeSet<u32>,
    functions: HashMap<u32, Function>,
    pub(crate) recording: Option<Box<Recorder>>,
    /// Record-mode variants by owner region, linked only to one another.
    pub(crate) code: HashMap<u32, crate::compile::Compiled>,
    pub(crate) links: crate::linking::Table,
    /// Functions whose hook state changed since the owner last recompiled.
    pub(crate) changed: BTreeSet<u32>,
    clock: u64,
    last_record: Option<Instant>,
    sampler: Sampler,
    pub(crate) stats: MemoStats,
    log: Option<Instant>,
    diag: HashMap<u32, Diag>,
}

/// Per-function diagnostics printed with ADV_MEMO_LOG.
#[derive(Default)]
struct Diag {
    calls: u64,
    hits: u64,
    key_misses: u64,
    data_misses: u64,
    recordings: u64,
    aborts: u64,
    unprofitable: u64,
    last_key: String,
    last_data: String,
    last_cost: String,
    last_abort: String,
}

impl Memo {
    pub(crate) fn new(options: MemoOptions) -> Self {
        Self {
            options,
            hooks: BTreeSet::new(),
            functions: HashMap::new(),
            recording: None,
            code: HashMap::new(),
            links: Default::default(),
            changed: BTreeSet::new(),
            clock: 0,
            last_record: None,
            sampler: Sampler::default(),
            stats: MemoStats::default(),
            log: std::env::var_os("ADV_MEMO_LOG").map(|_| Instant::now()),
            diag: HashMap::new(),
        }
    }

    /// `ADV_MEMO=1` enables reuse; `ADV_MEMO_FUNCS=pc,...` (hexadecimal)
    /// adds fixed hooks.
    pub(crate) fn from_env() -> Option<Box<Self>> {
        std::env::var_os("ADV_MEMO").filter(|value| value != "0")?;
        let mut memo = Box::new(Self::new(MemoOptions::default()));
        for pc in std::env::var("ADV_MEMO_FUNCS")
            .unwrap_or_default()
            .split(',')
        {
            if let Ok(pc) = u32::from_str_radix(pc.trim().trim_start_matches("0x"), 16) {
                memo.hook(pc);
            }
        }
        Some(memo)
    }

    pub(crate) fn hook(&mut self, pc: u32) {
        let function = self.functions.entry(pc).or_insert_with(Function::new);
        function.off = None;
        function.hooked = true;
        if self.hooks.insert(pc) {
            self.changed.insert(pc);
        }
    }

    /// Hooked PCs inside `[start, start + len)`.
    pub(crate) fn hooks_in(&self, start: u32, len: usize) -> Vec<u32> {
        let end = u64::from(start) + len as u64;
        self.hooks
            .range(start..)
            .take_while(|&&pc| u64::from(pc) < end)
            .copied()
            .collect()
    }

    fn switch_off(&mut self, pc: u32, off: Off) {
        let Some(function) = self.functions.get_mut(&pc) else {
            return;
        };
        for entry in function.entries.drain(..) {
            self.stats.bytes -= entry.bytes();
        }
        function.seen.clear();
        function.off = Some(off);
        if function.hooked {
            function.hooked = false;
            self.hooks.remove(&pc);
            self.changed.insert(pc);
        }
    }

    fn fail(&mut self, pc: u32, now: Instant) {
        let Some(function) = self.functions.get_mut(&pc) else {
            return;
        };
        function.failures += 1;
        let off = if function.failures >= STRIKES {
            Off::Forever
        } else {
            Off::Until(now + Duration::from_secs(1 << (2 * function.failures)))
        };
        self.switch_off(pc, off);
    }

    /// Called by generated code at a hooked PC with the state saved. Returns 0
    /// to execute normally, 1 after applying a recorded result, or 2 to
    /// leave native code so the executor records this execution.
    fn enter(&mut self, state: &mut CpuState, memory: &Memory, pc: u32) -> u32 {
        if self.recording.is_some() || memory.foreign_access_required() {
            return 0;
        }
        let Some(function) = self.functions.get_mut(&pc) else {
            return 0;
        };
        if function.off.is_some() {
            return 0;
        }
        self.clock += 1;
        function.window_calls += 1;
        let key = canonical(state);
        let hash = hash(&key);
        let mut probe = key.clone();
        let mut variants = 0;
        for entry in &mut function.entries {
            if !entry.same_key(hash, &key, &mut probe) {
                continue;
            }
            if entry.matches(memory) {
                entry.apply(state, memory);
                entry.hits += 1;
                entry.used = self.clock;
                function.window_hits += 1;
                function.gap = Duration::ZERO;
                self.stats.hits += 1;
                if self.log.is_some() {
                    let diag = self.diag.entry(pc).or_default();
                    diag.calls += 1;
                    diag.hits += 1;
                }
                return 1;
            }
            variants += 1;
        }
        if variants != 0 {
            self.stats.misses += 1;
        }
        if self.log.is_some() {
            let diag = self.diag.entry(pc).or_default();
            diag.calls += 1;
            let latest = function
                .entries
                .iter()
                .filter(|entry| entry.same_key(hash, &key, &mut probe))
                .max_by_key(|entry| entry.used);
            if let Some(entry) = latest {
                diag.data_misses += 1;
                let (bytes, runs) = entry.differences(memory);
                diag.last_data = format!(
                    "{bytes}/{} bytes differ: {}",
                    entry.reads.data.len(),
                    runs.iter()
                        .map(|(addr, len)| format!("{addr:#x}+{len}"))
                        .collect::<Vec<_>>()
                        .join(" ")
                );
            } else if let Some(entry) = function.entries.iter().max_by_key(|entry| entry.used) {
                diag.key_misses += 1;
                let (a, b) = (&*entry.key, &*key);
                let mut fields = Vec::new();
                for index in 0..8 {
                    if a.regs[index] != b.regs[index] {
                        fields.push(format!("r{index}:{:x}->{:x}", a.regs[index], b.regs[index]));
                    }
                }
                if a.eflags != b.eflags {
                    fields.push(format!("fl:{:x}->{:x}", a.eflags, b.eflags));
                }
                if a.xmm != b.xmm {
                    fields.push("xmm".into());
                }
                if a.mmx != b.mmx {
                    fields.push("mmx".into());
                }
                if a.x87 != b.x87 {
                    fields.push("x87".into());
                }
                if a.mxcsr != b.mxcsr || a.segments != b.segments || a.fs_base != b.fs_base {
                    fields.push("other".into());
                }
                diag.last_key = fields.join(" ");
            }
        }
        let now = Instant::now();
        if function.window_calls >= WINDOW {
            let useless = function.window_hits * 8 < function.window_calls;
            function.window_calls = 0;
            function.window_hits = 0;
            if useless {
                self.fail(pc, now);
                return 0;
            }
        }
        if function.sighting(hash) < self.options.sightings
            || function.next_record.is_some_and(|next| now < next)
            || self
                .last_record
                .is_some_and(|last| now < last + self.options.record_gap)
        {
            return 0;
        }
        let esp = state.regs[4];
        let Some(ret) = word(memory, esp) else {
            return 0;
        };
        function.next_record = Some(now + function.gap);
        self.last_record = Some(now);
        if self.log.is_some() {
            self.diag.entry(pc).or_default().recordings += 1;
        }
        self.recording = Some(Box::new(Recorder {
            function: pc,
            key,
            hash,
            ret,
            entry_esp: esp,
            pages: HashMap::new(),
            order: Vec::new(),
            poisoned: false,
            why: String::new(),
            armed: false,
            saved_fuel: 0,
            x87_written: 0,
            x87_touched: 0,
            x87_opaque: false,
            x87_status: false,
            x87_conditions: 0,
        }));
        2
    }

    /// Ends a recording at its return. `code` lists the code pages its
    /// record-mode variants were compiled from.
    pub(crate) fn finish(
        &mut self,
        recorder: Box<Recorder>,
        state: &CpuState,
        memory: &Memory,
        blocks: u64,
    ) {
        let now = Instant::now();
        if recorder.poisoned {
            self.stats.aborts += 1;
            if self.log.is_some() {
                let diag = self.diag.entry(recorder.function).or_default();
                diag.aborts += 1;
                diag.last_abort = format!("poisoned {}", recorder.why);
            }
            self.fail(recorder.function, now);
            return;
        }
        let mut reads = Runs::default();
        let mut writes = Runs::default();
        for &page in &recorder.order {
            let record = &recorder.pages[&page];
            for word in 0..PAGE / 64 {
                let (read, written) = (record.read[word], record.written[word]);
                if read | written == 0 {
                    continue;
                }
                for index in word * 64..word * 64 + 64 {
                    let offset = page * PAGE + index;
                    if bit(&record.read, index) {
                        reads.push(offset, record.values[index]);
                    }
                    if bit(&record.written, index) {
                        // SAFETY: recorded writes completed on mapped pages.
                        writes.push(offset, unsafe { *memory.ptr.as_ptr().add(offset) });
                    }
                }
            }
        }
        let moved = (reads.data.len() + writes.data.len()) as u64;
        if blocks < 32 || blocks * 32 < moved {
            self.stats.unprofitable += 1;
            if self.log.is_some() {
                let diag = self.diag.entry(recorder.function).or_default();
                diag.unprofitable += 1;
                diag.last_cost = format!("blocks={blocks} bytes={moved}");
            }
            self.fail(recorder.function, now);
            return;
        }
        let mut code: Vec<_> = self
            .code
            .values()
            .flat_map(|compiled| compiled.pages.iter().copied())
            .collect();
        code.sort_unstable();
        code.dedup();
        let mut out = Box::new(state.clone());
        out.refresh_mmx();
        out.fuel = 0;
        out.rep = None;
        let (before, after) = (x87_pointers(&recorder.key), x87_pointers(&out));
        let mut x87_inputs = 0;
        for index in 0..5 {
            if recorder.x87_written & 1 << index == 0 && before[index] == after[index] {
                x87_inputs |= 1 << index;
            }
        }
        let empty = x87_empty(&recorder.key);
        let free = if recorder.x87_opaque { 0 } else { empty };
        let mut touched = recorder.x87_touched;
        for slot in 0..8 {
            if out.x87.regs[slot] != recorder.key.x87.regs[slot] {
                touched |= 1 << slot;
            }
        }
        let sticky = (!recorder.x87_status && recorder.key.x87.control & 0x3f == 0x3f)
            .then_some(recorder.key.x87.status & STICKY);
        // Unchanged condition codes pass through, as for pointer fields.
        let mut status_free = if sticky.is_some() { STICKY } else { 0 };
        if !recorder.x87_status {
            status_free |=
                (recorder.x87_conditions | (out.x87.status ^ recorder.key.x87.status)) & CONDITIONS;
        }
        let mut key = recorder.key;
        mask_key(&mut key, x87_inputs, free, status_free);
        self.clock += 1;
        let entry = Entry {
            key,
            x87_inputs,
            x87_free: free,
            x87_kept: free & !touched,
            sticky,
            status_free,
            hash: recorder.hash,
            reads,
            writes,
            code,
            out,
            epoch: 0,
            hits: 0,
            used: self.clock,
        };
        let bytes = entry.bytes();
        while self.stats.bytes + bytes > BUDGET && self.evict_oldest() {}
        let Some(function) = self.functions.get_mut(&recorder.function) else {
            return;
        };
        let same = |old: &Entry| {
            old.hash == entry.hash
                && old.x87_inputs == entry.x87_inputs
                && old.x87_free == entry.x87_free
                && old.sticky == entry.sticky
                && old.status_free == entry.status_free
                && old.key == entry.key
        };
        let victim = if function.entries.iter().filter(|old| same(old)).count() >= VARIANTS {
            (0..function.entries.len())
                .filter(|&index| same(&function.entries[index]))
                .min_by_key(|&index| function.entries[index].used)
        } else if function.entries.len() >= ENTRIES {
            (0..function.entries.len()).min_by_key(|&index| function.entries[index].used)
        } else {
            None
        };
        if let Some(index) = victim {
            let old = function.entries.swap_remove(index);
            if old.hits == 0 {
                function.slower();
            }
            self.stats.bytes -= old.bytes();
        }
        function.entries.push(entry);
        self.stats.bytes += bytes;
        self.stats.entries += 1;
    }

    fn evict_oldest(&mut self) -> bool {
        let Some((pc, index)) = self
            .functions
            .iter()
            .flat_map(|(&pc, function)| {
                function
                    .entries
                    .iter()
                    .enumerate()
                    .map(move |(index, entry)| (entry.used, pc, index))
            })
            .min()
            .map(|(_, pc, index)| (pc, index))
        else {
            return false;
        };
        let entry = self
            .functions
            .get_mut(&pc)
            .unwrap()
            .entries
            .swap_remove(index);
        self.stats.bytes -= entry.bytes();
        true
    }

    pub(crate) fn logging(&self) -> bool {
        self.log.is_some()
    }

    pub(crate) fn abort(&mut self, recorder: Box<Recorder>, reason: Abort, detail: String) {
        self.stats.aborts += 1;
        if self.log.is_some() {
            let diag = self.diag.entry(recorder.function).or_default();
            diag.aborts += 1;
            diag.last_abort = format!("{reason:?} {detail}");
        }
        let now = Instant::now();
        match reason {
            Abort::Impure => self.fail(recorder.function, now),
            Abort::Limit => self.switch_off(recorder.function, Off::Forever),
            Abort::Context => {
                if let Some(function) = self.functions.get_mut(&recorder.function) {
                    function.slower();
                    function.next_record = Some(now + function.gap);
                }
            }
        }
    }

    /// Adds a record-mode variant and relinks the variants.
    pub(crate) fn add_code(&mut self, owner: u32, compiled: crate::compile::Compiled) {
        if self.code.len() >= 512 {
            self.code.clear();
        }
        self.code.insert(owner, compiled);
        self.relink();
    }

    /// Drops variants whose code pages changed.
    pub(crate) fn prune_code(&mut self, memory: &Memory) {
        let before = self.code.len();
        self.code.retain(|_, compiled| compiled.valid(memory));
        if self.code.len() != before {
            self.relink();
        }
    }

    fn relink(&mut self) {
        let mut entries = Vec::new();
        for compiled in self.code.values() {
            for &pc in &compiled.entries {
                entries.push((pc, compiled.tail_entry));
            }
        }
        entries.sort_unstable_by_key(|&(pc, _)| pc);
        entries.dedup_by_key(|&mut (pc, _)| pc);
        self.links.rebuild(entries.into_iter());
    }

    /// Stack sampling for automatic hooks: every frame whose return address
    /// follows a direct call counts once for the called function.
    pub(crate) fn sample(&mut self, state: &CpuState, memory: &Memory) {
        if !self.options.automatic {
            return;
        }
        let now = Instant::now();
        if self.sampler.next_sample.is_some_and(|next| now < next) {
            return;
        }
        self.sampler.next_sample = Some(now + SAMPLE_PERIOD);
        let mut frames = [0u32; FRAMES];
        let mut found = 0;
        let esp = state.regs[4];
        for index in 0..STACK_WORDS {
            let Some(value) = word(memory, esp.wrapping_add(index * 4)) else {
                break;
            };
            if let Some(target) = call_target(memory, value)
                && !frames[..found].contains(&target)
            {
                frames[found] = target;
                found += 1;
                *self.sampler.counts.entry(target).or_default() += 1;
                if found == FRAMES {
                    break;
                }
            }
        }
        self.sampler.samples += 1;
        let decide = *self.sampler.next_decide.get_or_insert(now + DECIDE_PERIOD);
        if now >= decide {
            self.decide(now);
        }
        self.report(now);
    }

    fn decide(&mut self, now: Instant) {
        let samples = std::mem::take(&mut self.sampler.samples);
        let counts = std::mem::take(&mut self.sampler.counts);
        self.sampler.next_decide = Some(now + DECIDE_PERIOD);
        if samples < 50 {
            return;
        }
        for (pc, count) in counts {
            if count * 100 < samples * SHARE {
                continue;
            }
            match self.functions.get(&pc).map(|function| &function.off) {
                None => self.hook(pc),
                Some(Some(Off::Until(until))) if now >= *until => self.hook(pc),
                Some(_) => {}
            }
        }
    }

    fn report(&mut self, now: Instant) {
        let Some(last) = self.log else {
            return;
        };
        if now < last + Duration::from_secs(5) {
            return;
        }
        self.log = Some(now);
        let stats = self.stats;
        eprintln!(
            "memo: hooked={} hits={} misses={} recordings={} entries={} aborts={} unprofitable={} bytes={}",
            self.hooks.len(),
            stats.hits,
            stats.misses,
            stats.recordings,
            stats.entries,
            stats.aborts,
            stats.unprofitable,
            stats.bytes,
        );
        let mut rows: Vec<_> = self.diag.iter().collect();
        rows.sort_unstable_by_key(|(_, diag)| std::cmp::Reverse(diag.calls));
        for (pc, diag) in rows.into_iter().take(12) {
            let state = match self.functions.get(pc).and_then(|f| f.off.as_ref()) {
                None => "on",
                Some(Off::Until(_)) => "wait",
                Some(Off::Forever) => "off",
            };
            eprintln!(
                "  {pc:08x} {state} calls={} hits={} keymiss={} datamiss={} rec={} abort={} unprof={} | key[{}] data[{}] cost[{}] abort[{}]",
                diag.calls,
                diag.hits,
                diag.key_misses,
                diag.data_misses,
                diag.recordings,
                diag.aborts,
                diag.unprofitable,
                diag.last_key,
                diag.last_data,
                diag.last_cost,
                diag.last_abort,
            );
        }
    }

    pub(crate) fn stats(&self) -> MemoStats {
        MemoStats {
            hooked: self.hooks.len(),
            ..self.stats
        }
    }
}

/// C ABI called by generated code at a hooked PC with the state saved.
///
/// # Safety
/// `bridge` and `state` are the live objects of the paused native call.
pub(crate) unsafe extern "C" fn enter(
    bridge: *mut std::ffi::c_void,
    state: *mut CpuState,
    pc: u32,
) -> u32 {
    let bridge = unsafe { &mut *bridge.cast::<crate::trap::Bridge<'_>>() };
    if bridge.memo.is_null() {
        return 0;
    }
    let (memo, state, memory) = unsafe { (&mut *bridge.memo, &mut *state, &*bridge.memory) };
    let result = memo.enter(state, memory, pc);
    if result == 2 {
        memo.stats.recordings += 1;
    }
    result
}

/// C ABI called by record-mode code before each guest access.
///
/// # Safety
/// `bridge` is the live Bridge of the paused native call.
pub(crate) unsafe extern "C" fn access(
    bridge: *mut std::ffi::c_void,
    addr: u32,
    len: u32,
    write: u32,
) {
    let bridge = unsafe { &mut *bridge.cast::<crate::trap::Bridge<'_>>() };
    if bridge.recorder.is_null() {
        return;
    }
    let (recorder, memory) = unsafe { (&mut *bridge.recorder, &*bridge.memory) };
    recorder.access(memory, addr, len, write != 0);
}
