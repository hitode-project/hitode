use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::cliffs::InterpreterDetail;
use crate::compile::{self, Compiled};
use crate::trap::{Bridge, ExecutionTrap};
use crate::{CompileStrategy, CpuState, Exit, Memory, Region};

mod code_store;
mod coverage;
mod instruction_trace;
mod interpreter_profile;
mod parallel;
mod schedule;
pub use schedule::CompilationSchedule;
mod aot;
mod persistent;
mod tier1;
mod verify;
mod warm;
pub use aot::{AotImage, AotStats};
use code_store::CodeStore;
pub use verify::VerifyStats;

#[derive(Default, Clone, Debug)]
pub struct Stats {
    pub aot: AotStats,
    pub compilations: u64,
    pub compile_requests: u64,
    /// Regions queued from a warm-start profile.
    pub warm_requests: u64,
    pub tier_up_requests: u64,
    pub tier_up_publications: u64,
    pub compile_failures: u64,
    pub stale_compilations: u64,
    pub pending_compilations: usize,
    pub peak_pending_compilations: usize,
    pub deferred_compilations: usize,
    pub peak_deferred_compilations: usize,
    pub compile_slices: u64,
    pub compile_work_rejections: u64,
    /// Largest responsive work estimate; zero means unrestricted/unmeasured.
    pub max_compile_work: usize,
    pub compile_time: Duration,
    pub sync_compile_time: Duration,
    pub compile_wait_execution_ns: u64,
    /// Opt-in fallback lane timings, split by the existing entry reason.
    pub frame_reason_ns: [u64; 6],
    pub compile_wait_units: u64,
    pub compilation_schedule: CompilationSchedule,
    pub native_code_bytes: usize,
    pub native_regions: usize,
    pub cache_evictions: u64,
    pub cache_rejections: u64,
    pub native_entries: u64,
    /// Entries reached by a generated tail call without returning to run_inner.
    pub linked_entries: u64,
    /// RET hints validated against the actual guest stack, including local hits.
    pub shadow_matches: u64,
    /// Validated hints that actually bypassed the hash search and tail-linked.
    pub shadow_hits: u64,
    pub shadow_misses: u64,
    pub shadow_overflows: u64,
    pub interpreter_instructions: u64,
    pub interpreter_entries: u64,
    pub native_blocks: u64,
    /// Opt-in exact Tier 0 retirement, excluding REP elements and oracle replay.
    pub frame_retired_tier0: u64,
    pub invalidations: u64,
    /// Region candidates checked after changes to their code pages only.
    pub invalidation_candidates: u64,
    pub code_revalidations: u64,
    /// Raw traps delivered to the caller-owned handler.
    pub trap_calls: u64,
    pub bulk_calls: u64,
    pub bulk_bytes: u64,
    /// Slow-path counters by instruction and reason.
    pub cliffs: crate::Cliffs,
    /// Interpreter checks of newly published native code.
    pub verification: VerifyStats,
    pub direct_memory: DirectMemoryStats,
}

/// Direct scalar site publications and dynamic access outcomes.
#[derive(Default, Clone, Debug)]
pub struct DirectMemoryStats {
    pub sites: u64,
    /// Regions recompiled with legacy scalar helpers after epilogue rejection.
    pub inline_fallbacks: u64,
    pub accesses: u64,
    pub recoveries: u64,
    pub violations: u64,
    pub checked_fallbacks: u64,
    pub code_writes: u64,
}

#[derive(Default, Clone, Debug)]
pub struct ProfileSample {
    pub native_entries: u64,
    pub native_ns: u64,
    pub interpreter_entries: u64,
    pub interpreter_instructions: u64,
    pub interpreter_ns: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum RunOutcome {
    /// The controller found a compiled entry; a native worker must execute it.
    NativeReady,
    ReachedStop,
    Budget,
    Interrupted,
    ReadFault {
        pc: u32,
        address: u32,
    },
    WriteFault {
        pc: u32,
        address: u32,
    },
    FetchFault {
        pc: u32,
        address: u32,
    },
    InvalidInstruction {
        pc: u32,
    },
    UnsupportedInstruction {
        pc: u32,
        code: iced_x86::Code,
    },
    Trap {
        pc: u32,
        vector: u8,
    },
    Parked,
}

/// Executed blocks after which an automatic Fast unit is recompiled for Speed.
const HOT_BLOCKS: u64 = 16_384;
/// Native blocks between scans of the profiled Fast units.
const SCAN_BLOCKS: u64 = 1 << 18;

/// Owns the guest CPU, decoded instructions and native cache. Both executors
/// access the same state and address space; only this owner publishes links.
pub struct Executor {
    pub(crate) frame_timing: bool,
    rows: Option<Box<crate::rows::RowPool>>,
    parallel: Option<crate::parallel::Pool>,
    parallel_image: Option<(u64, Arc<crate::parallel::Image>)>,
    parallel_stop: Option<crate::parallel::Stop>,
    admission: crate::parallel::Admission,
    pub(crate) parallel_demand: bool,
    code: CodeStore,
    pub(crate) memory: Memory,
    region_linking: bool,
    shadow_returns: Option<Box<crate::linking::shadow::Stack>>,
    pub(crate) interpreter: crate::interpreter::Interpreter,
    tier1: Option<crate::tier1::Engine>,
    tier1_mode: crate::Tier1Mode,
    auto_jit: bool,
    auto_compile_strategy: CompileStrategy,
    tiered_compilation: bool,
    native_heat: HashMap<u32, u64>,
    tier_attempted: BTreeSet<u32>,
    /// Indirect call targets already offered to each unit's recompilation.
    callee_attempts: HashMap<u32, BTreeSet<u32>>,
    /// `stats.native_blocks` at the last `scan_profiles`.
    scanned_blocks: u64,
    /// Published units with a profile. A profile nothing else holds belongs to
    /// retired code.
    profiled: Vec<(u32, Arc<compile::Profile>)>,
    async_compilation: bool,
    compiler: Option<crate::compiler::Worker>,
    schedule: schedule::Tracker,
    background_sender: Option<adv_sync::background::LocalSender>,
    background_service: Option<std::rc::Rc<adv_sync::background::Service>>,
    background_wake: Option<adv_sync::WakeEvent>,
    completion_wake: Option<crate::compiler::CompletionWake>,
    interrupt: Arc<AtomicBool>,
    inactive_interrupt: AtomicBool,
    host_interrupts: bool,
    pending_regions: BTreeMap<u32, u64>,
    deferred_compilations: BTreeMap<u32, compile::Snapshot>,
    /// Queue order of deferred work; later discoveries are what runs now.
    deferred_order: HashMap<u32, u64>,
    deferred_sequence: u64,
    /// Interpreter starts at unowned pcs while compilation work is deferred.
    demand: HashMap<u32, u64>,
    warm: Option<warm::Pending>,
    aot: aot::State,
    persistent_cache: Option<Arc<crate::code_cache::Cache>>,
    cached_pending: Option<persistent::Pending>,
    cache_images: Vec<(u32, u32, [u8; 32])>,
    cache_guest: Option<[u8; 32]>,
    /// Dedicated compile threads; zero shares the background service.
    compile_threads: usize,
    automatic_regions: BTreeSet<u32>,
    hits: HashMap<u32, u32>,
    attempted: BTreeMap<u32, InterpreterDetail>,
    cache_region_limit: usize,
    cache_byte_limit: usize,
    cache_clock: u64,
    rejected_entries: BTreeMap<u32, Vec<(usize, u64)>>,
    observed_epoch: u64,
    profiling: bool,
    pub(crate) deadline: Option<Instant>,
    /// Measured native blocks per microsecond, which sizes the safepoint
    /// allowance under a wall deadline; zero until measured.
    native_rate: u64,
    pub profile: BTreeMap<u32, ProfileSample>,
    /// Bounded diagnostics for automatic compilation failures. Guest execution
    /// continues in the interpreter; explicit compilation still returns errors.
    pub compile_errors: BTreeMap<u32, String>,
    deferred_fallback: Option<u32>,
    interpreter_profile: Option<Box<interpreter_profile::Profile>>,
    instruction_trace: Option<Box<instruction_trace::Trace>>,
    verifier: Option<Box<verify::Verifier>>,
    timestamp: crate::timestamp::Counter,
    pub state: CpuState,
    pub stats: Stats,
}

/// Full per-thread CPU state and resumable executor progress.
#[derive(Clone)]
pub struct ExecutionContext {
    cpu: CpuState,
    deferred_fallback: Option<u32>,
    parallel_stop: Option<crate::parallel::Stop>,
    admission: crate::parallel::Admission,
}

impl ExecutionContext {
    pub fn cpu(&self) -> &CpuState {
        &self.cpu
    }
    /// Changes apply on restore, including the full raw floating-point state.
    pub fn cpu_mut(&mut self) -> &mut CpuState {
        &mut self.cpu
    }
}

impl Executor {
    pub fn save_context(&self) -> Result<ExecutionContext, String> {
        let mut cpu = self.state.clone();
        cpu.refresh_mmx();
        Ok(ExecutionContext {
            cpu,
            deferred_fallback: self.deferred_fallback,
            parallel_stop: self.parallel_stop.clone(),
            admission: self.admission,
        })
    }
    pub fn restore_context(&mut self, context: &ExecutionContext) -> Result<(), String> {
        self.state = context.cpu.clone();
        self.state.refresh_mmx();
        self.deferred_fallback = context.deferred_fallback;
        self.parallel_stop = context.parallel_stop.clone();
        self.admission = context.admission;
        Ok(())
    }

    /// Execute with a caller-owned raw trap handler. The execution layer has no
    /// API signatures, callbacks, waits or host continuations.
    pub fn run_with_traps(
        &mut self,
        stop: u32,
        budget: u64,
        handler: &mut dyn ExecutionTrap,
        interpreter_only: bool,
    ) -> Result<RunOutcome, String> {
        self.run_execution(stop, budget, Some(handler), interpreter_only)
    }

    fn run_execution(
        &mut self,
        stop: u32,
        budget: u64,
        handler: Option<&mut dyn ExecutionTrap>,
        interpreter_only: bool,
    ) -> Result<RunOutcome, String> {
        let started = Instant::now();
        let interpreted_before = self.stats.interpreter_entries;
        let native_before = self.stats.native_blocks;
        let mut bridge = Bridge {
            timestamp: std::mem::take(&mut self.timestamp),
            memory: &mut self.memory,
            error: None,
            trap: None,
            calls: 0,
            handler,
            parked: false,
            deadline: self.deadline,
            clock_due: true,
            unclocked_bytes: 0,
            bulk_calls: 0,
            bulk_bytes: 0,
            cliffs: std::mem::take(&mut self.stats.cliffs),
        };
        // Diagnostics use only the explicitly selected interpreter backend.
        // Disabled runs retain block execution and its instruction hot path.
        let mut instruction_trace = interpreter_only
            .then(|| self.instruction_trace.take())
            .flatten();
        let result = self.run_inner(
            stop,
            budget,
            &mut bridge,
            interpreter_only,
            instruction_trace.as_deref_mut(),
        );
        if let Some(trace) = &mut instruction_trace {
            trace.boundary(&result, &self.state, &self.memory);
        }
        if interpreter_only {
            self.instruction_trace = instruction_trace;
        }
        if self.parallel.is_some() {
            if self.stats.interpreter_entries != interpreted_before {
                self.admission.cold();
            } else if bridge.calls == 0
                && self.stats.native_blocks != native_before
                && matches!(result, Ok(RunOutcome::Budget))
            {
                self.admission.inline_native(started.elapsed());
            } else {
                self.admission.inline_boundary();
            }
        }
        // Export canonical floating state at every public CPU boundary. Context
        // saves can then borrow immutably without hiding MMX updates in RefCell.
        self.state.refresh_mmx();
        self.stats.trap_calls += bridge.calls;
        self.stats.bulk_calls += bridge.bulk_calls;
        self.stats.bulk_bytes += bridge.bulk_bytes;
        self.stats.cliffs = std::mem::take(&mut bridge.cliffs);
        #[cfg(all(
            any(
                target_os = "linux",
                target_os = "android",
                all(target_os = "macos", target_arch = "aarch64")
            ),
            any(target_arch = "x86_64", target_arch = "aarch64")
        ))]
        if let Some(engine) = &self.memory.direct_engine {
            let stats = &mut self.stats.direct_memory;
            stats.accesses = engine.accesses.get();
            stats.recoveries = engine.recoveries.get();
            stats.violations = engine.violations.get();
            stats.checked_fallbacks = engine.checked.get();
            stats.code_writes = engine.code_writes.get();
        }
        self.timestamp = bridge.timestamp;
        result
    }
    /// Check the first native entries of current and later publications
    /// against the interpreter; `strict` turns a mismatch into a run error.
    pub fn set_tier_verification(&mut self, mode: Option<bool>) {
        self.verifier = mode.map(|strict| {
            let mut verifier = verify::Verifier::new(strict);
            for &start in self.code.compiled.keys() {
                verifier.pending.insert(start, verify::ENTRIES);
            }
            Box::new(verifier)
        });
    }
    /// Account one verified native exit; strict mode stops at a mismatch.
    fn record_verification(
        &mut self,
        check: Option<Result<verify::Check, String>>,
    ) -> Result<(), String> {
        let stats = &mut self.stats.verification;
        match check {
            None => stats.inconclusive += 1,
            Some(Ok(verify::Check::Equal)) => stats.verified += 1,
            Some(Ok(verify::Check::MxcsrFlags)) => stats.mxcsr_flags += 1,
            Some(Err(report)) => {
                stats.mismatches += 1;
                if self.verifier.as_ref().is_some_and(|v| v.strict) {
                    return Err(format!("tier verification: {report}"));
                }
                if stats.reports.len() < verify::REPORTS {
                    stats.reports.push(report);
                }
            }
        }
        Ok(())
    }
    /// Select helper threads before execution. Zero runs admitted batches inline.
    pub fn set_row_workers(&mut self, count: usize) -> Result<(), String> {
        if self.stats.native_entries != 0 || self.stats.interpreter_entries != 0 {
            return Err("row workers must be selected before execution".into());
        }
        if self.parallel.is_some() {
            return Err("row batching is disabled with multiple guest workers".into());
        }
        self.rows = Some(Box::new(crate::rows::RowPool::new(count)?));
        Ok(())
    }
    /// Disable row batching without changing guest thread scheduling.
    pub fn disable_row_batching(&mut self) {
        self.rows = None;
    }
    pub fn set_row_threshold(&mut self, work: u64) {
        if let Some(pool) = &mut self.rows {
            pool.set_threshold(work);
        }
    }
    pub fn row_parallel_stats(&self) -> crate::RowStats {
        self.rows
            .as_ref()
            .map_or_else(Default::default, |pool| pool.stats())
    }
    /// Choose before compiling or executing. The default remains checked.
    /// Disabling after reservation requires a fresh owner, so outstanding host
    /// aliases can never silently move to a different allocation.
    pub fn set_direct_memory(&mut self, enabled: bool) -> Result<(), String> {
        if enabled == self.memory.direct_memory_enabled() {
            return Ok(());
        }
        if !enabled {
            return Err("create a fresh executor to disable direct memory".into());
        }
        if self.stats.native_entries != 0
            || self.stats.interpreter_entries != 0
            || !self.code.compiled.is_empty()
            || !self.pending_regions.is_empty()
            || !self.deferred_compilations.is_empty()
        {
            return Err("direct memory must be selected before compilation and execution".into());
        }
        self.memory.enable_direct_memory()
    }

    pub fn new(base: u32, len: usize) -> Result<Self, String> {
        Self::create(Memory::new(base, len)?)
    }

    /// Reserve all 4 GiB; only mapped guest pages consume physical memory.
    pub fn address_space() -> Result<Self, String> {
        Self::create(Memory::reserve(0, 1usize << 32)?)
    }

    fn create(memory: Memory) -> Result<Self, String> {
        // An optional acceleration must not prevent a guest from starting.
        let rows = match crate::rows::RowPool::new(crate::rows::RowPool::default_workers()) {
            Ok(pool) => Some(Box::new(pool)),
            Err(error) => {
                eprintln!("warning: row helpers unavailable ({error}); using serial execution");
                None
            }
        };
        let mut executor = Self {
            frame_timing: false,
            rows,
            parallel: None,
            parallel_image: None,
            parallel_stop: None,
            admission: crate::parallel::Admission::default(),
            parallel_demand: true,
            code: CodeStore::default(),
            memory,
            region_linking: true,
            shadow_returns: None,
            interpreter: crate::interpreter::Interpreter::new(),
            tier1: None,
            tier1_mode: crate::Tier1Mode::Off,
            auto_jit: false,
            auto_compile_strategy: CompileStrategy::Fast,
            tiered_compilation: true,
            native_heat: HashMap::new(),
            tier_attempted: BTreeSet::new(),
            callee_attempts: HashMap::new(),
            scanned_blocks: 0,
            profiled: Vec::new(),
            async_compilation: false,
            compiler: None,
            schedule: Default::default(),
            background_sender: None,
            background_service: None,
            background_wake: None,
            completion_wake: None,
            interrupt: Arc::new(AtomicBool::new(false)),
            inactive_interrupt: AtomicBool::new(false),
            host_interrupts: true,
            pending_regions: BTreeMap::new(),
            deferred_compilations: BTreeMap::new(),
            deferred_order: HashMap::new(),
            deferred_sequence: 0,
            demand: HashMap::new(),
            warm: None,
            aot: aot::State::default(),
            persistent_cache: None,
            cached_pending: None,
            cache_images: Vec::new(),
            cache_guest: None,
            compile_threads: 0,
            automatic_regions: BTreeSet::new(),
            hits: HashMap::new(),
            attempted: BTreeMap::new(),
            cache_region_limit: 16384,
            cache_byte_limit: 128 * 1024 * 1024,
            cache_clock: 0,
            rejected_entries: BTreeMap::new(),
            observed_epoch: 0,
            profiling: false,
            deadline: None,
            native_rate: 0,
            profile: BTreeMap::new(),
            compile_errors: BTreeMap::new(),
            deferred_fallback: None,
            interpreter_profile: interpreter_profile::Profile::from_env(),
            instruction_trace: instruction_trace::Trace::from_env(),
            verifier: verify::Verifier::from_env(),
            timestamp: crate::timestamp::Counter::default(),
            state: CpuState::default(),
            stats: Stats::default(),
        };
        if std::env::var("ADV_DIRECT_MEMORY").is_ok_and(|value| value == "1") {
            executor.set_direct_memory(true)?;
        }
        match std::env::var("ADV_GIR").as_deref() {
            Ok("off" | "" | "0") | Err(_) => {}
            Ok(_) => return Err("ADV_GIR execution has been removed; use the default GIR frontend or ADV_TIER2_FRONTEND=legacy for comparison".into()),
        }
        Ok(executor)
    }

    pub(crate) fn setup_wow64_segments(&mut self, addr: u32) -> Result<(), String> {
        self.memory.pages(addr, 4096)?;
        self.memory.committed(addr, 4096)?;
        let mut gdt = [0u8; 128];
        for (selector, code, dpl) in [
            (0x23u16, true, 3),
            (0x2b, false, 3),
            (0x30, false, 0),
            (0x53, false, 3),
        ] {
            let offset = usize::from(selector >> 3) * 8;
            gdt[offset..offset + 8].copy_from_slice(&descriptor_dpl(0, code, dpl).to_le_bytes());
        }
        self.memory.write(addr, &gdt)?;
        let idt_base = addr + crate::backend::SegmentSetup::IDT_OFFSET;
        self.memory.write(
            idt_base,
            &[0; crate::backend::SegmentSetup::IDT_LIMIT as usize + 1],
        )?;
        self.state.segments = crate::SegmentState {
            selectors: [0x2b, 0x23, 0x30, 0x2b, 0x53, 0],
            gdt_base: addr,
            gdt_limit: 127,
            idt_base,
            idt_limit: crate::backend::SegmentSetup::IDT_LIMIT,
            ldt_selector: 0,
        };
        Ok(())
    }

    pub(crate) fn set_fs_base(&mut self, base: u32) -> Result<(), String> {
        let segments = self.state.segments;
        if segments.gdt_limit != 0 {
            let offset = u32::from(segments.selectors[4] >> 3) * 8;
            let addr = segments
                .gdt_base
                .checked_add(offset)
                .ok_or("GDT address overflow")?;
            self.memory
                .write(addr, &descriptor_dpl(base, false, 3).to_le_bytes())?;
        }
        self.state.fs_base = base;
        Ok(())
    }

    /// A new guest process shares policy and the bounded disk archive, never
    /// execution contexts or published native code. Its image identity is new.
    pub(crate) fn inherit_process_policy(&mut self, parent: &Self) -> Result<(), String> {
        self.set_guest_workers(parent.guest_workers())?;
        self.background_service = parent.background_service.clone();
        self.auto_jit = parent.auto_jit;
        self.auto_compile_strategy = parent.auto_compile_strategy;
        self.tiered_compilation = parent.tiered_compilation;
        self.async_compilation = parent.async_compilation;
        self.persistent_cache = parent.persistent_cache.clone();
        self.set_aot_enabled(parent.aot.enabled);
        self.set_tier1_mode(parent.tier1_mode)?;
        self.region_linking = parent.region_linking;
        self.profiling = parent.profiling;
        self.cache_region_limit = parent.cache_region_limit;
        self.cache_byte_limit = parent.cache_byte_limit;
        self.set_shadow_returns(parent.shadow_returns.is_some())?;
        self.set_compile_threads(parent.compile_threads)?;
        self.host_interrupts = parent.host_interrupts;
        self.rows = parent
            .rows
            .as_ref()
            .map(|rows| rows.fork_empty().map(Box::new))
            .transpose()?;
        Ok(())
    }

    pub fn memory(&self) -> &Memory {
        &self.memory
    }

    /// Opt into synchronized host protections before compiling native regions.
    #[cfg(windows)]
    pub fn enable_native_protection(&mut self) -> Result<(), String> {
        self.memory.enable_native_protection()
    }

    /// OS committed/reserved bytes for finalized code and its xdata pages.
    /// Module/Rust heap data and unrelated guest storage are not included.
    #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
    pub fn native_code_storage_bytes(&self) -> Result<(usize, usize), String> {
        self.code
            .compiled
            .values()
            .try_fold((0, 0), |(committed, reserved), code| {
                let (code_committed, code_reserved) = code.storage_bytes()?;
                Ok((committed + code_committed, reserved + code_reserved))
            })
    }

    /// Loader/debugger writes bypass guest permissions and invalidate code.
    pub fn write(&mut self, addr: u32, bytes: &[u8]) -> Result<(), String> {
        self.memory.write(addr, bytes)
    }
    pub fn protect(&mut self, addr: u32, len: usize, perms: u8) -> Result<(), String> {
        self.memory.protect(addr, len, perms)
    }

    /// Compile explicitly selected hot code. Cold code is executed by the shared-state interpreter.
    pub fn set_region(&mut self, region: Region) -> Result<(), String> {
        self.memory.quiesce_workers();
        self.code.clear_links();
        self.code.cache_revision += 1;
        self.code.compiled.clear();
        self.stats.native_code_bytes = 0;
        self.stats.native_regions = 0;
        self.rejected_entries.clear();
        self.code.clear_regions();
        self.code.page_regions.clear();
        self.code.region_pages.clear();
        self.code.entry_owner.clear();
        self.code.fast_regions.clear();
        self.code.parallel_entries.clear();
        self.pending_regions.clear();
        self.deferred_compilations.clear();
        self.stats.deferred_compilations = 0;
        self.automatic_regions.clear();
        self.native_heat.clear();
        self.callee_attempts.clear();
        self.profiled.clear();
        self.tier_attempted.clear();
        self.hits.clear();
        self.demand.clear();
        self.deferred_order.clear();
        self.attempted.clear();
        self.stats.pending_compilations = 0;
        self.code.cache_revision += 1;
        self.add_region(region)
    }

    pub fn add_region(&mut self, region: Region) -> Result<(), String> {
        let start = Instant::now();
        let compiled = compile::compile(&region, &mut self.memory, self.shadow_returns.is_some())?;
        self.stats.compilations += 1;
        let elapsed = start.elapsed();
        self.stats.compile_time += elapsed;
        self.stats.sync_compile_time += elapsed;
        self.pending_regions.remove(&region.start);
        self.deferred_compilations.remove(&region.start);
        self.stats.deferred_compilations = self.deferred_compilations.len();
        self.stats.pending_compilations = self.pending_regions.len();
        self.automatic_regions.remove(&region.start);
        self.publish(region, compiled);
        Ok(())
    }

    fn publish(&mut self, region: Region, compiled: Compiled) {
        if self.cache_region_limit == 0 || compiled.code_size > self.cache_byte_limit {
            self.stats.cache_rejections += 1;
            for &pc in region.entries.iter().chain(std::iter::once(&region.start)) {
                self.rejected_entries.insert(pc, compiled.pages.clone());
            }
            while self.rejected_entries.len() > 4096 {
                let (pc, _) = self.rejected_entries.pop_first().unwrap();
                self.attempted.remove(&pc);
            }
            if !self.code.compiled.contains_key(&region.start) {
                self.forget_region(region.start);
            }
            return;
        }
        self.native_heat.remove(&region.start);
        self.tier_attempted.remove(&region.start);
        self.compile_errors.remove(&region.start);
        self.retire_compiled(region.start);
        while self.code.compiled.len() >= self.cache_region_limit
            || self.stats.native_code_bytes > self.cache_byte_limit - compiled.code_size
        {
            self.evict_oldest();
        }
        self.cache_clock = self.cache_clock.saturating_add(1);
        compiled.cache_stamp.set(self.cache_clock);
        for &pc in &compiled.entries {
            self.code.entry_owner.insert(pc, region.start);
            self.code.link(pc, compiled.tail_entry);
            if self.parallel.is_some() {
                self.code
                    .parallel_entries
                    .insert(pc, compiled.parallel_entry());
            }
            self.demand.remove(&pc);
        }
        self.stats.direct_memory.sites += compiled.direct_sites as u64;
        self.stats.direct_memory.inline_fallbacks += compiled.direct_inline_fallbacks;
        self.stats.native_code_bytes += compiled.code_size;
        if let Some(profile) = compiled.shared_profile() {
            self.profiled.push((region.start, profile));
        }
        self.code.compiled.insert(region.start, compiled);
        if let Some(verifier) = &mut self.verifier {
            verifier.pending.insert(region.start, verify::ENTRIES);
        }
        if region.compile_strategy == CompileStrategy::Fast {
            self.code.fast_regions.insert(region.start);
        }
        self.stats.native_regions = self.code.compiled.len();
        let pages = self.code.compiled[&region.start]
            .pages
            .iter()
            .map(|&(page, _)| self.memory.base + (page * 4096) as u32)
            .collect();
        self.index_region_with_pages(region, Some(pages));
        self.code.cache_revision += 1;
    }

    fn retire_compiled(&mut self, start: u32) {
        if self.code.compiled.contains_key(&start) {
            self.memory.quiesce_workers();
        }
        if let Some(old) = self.code.compiled.remove(&start) {
            // Only the execution owner reaches this path, after every generated
            // call has returned. Remove all published raw targets before Drop
            // unregisters unwind tables and releases executable memory. A stale
            // worker image would keep the table shared and force a rebuild.
            self.parallel_image = None;
            self.code.retire_links();
            self.code.cache_revision += 1;
            self.stats.native_code_bytes -= old.code_size;
            self.code.fast_regions.remove(&start);
            for entry in old.entries.iter() {
                if self.code.entry_owner.get(entry) == Some(&start) {
                    self.code.entry_owner.remove(entry);
                    self.code.unlink(*entry);
                    self.code.parallel_entries.remove(entry);
                }
            }
            self.stats.native_regions = self.code.compiled.len();
        }
    }

    fn forget_region(&mut self, start: u32) {
        if let Some(region) = self.code.remove_region(start) {
            for pc in region.entries {
                self.attempted.remove(&pc);
                self.hits.remove(&pc);
            }
        }
        if let Some(pages) = self.code.region_pages.remove(&start) {
            for page in pages {
                if let Some(owners) = self.code.page_regions.get_mut(&page) {
                    owners.remove(&start);
                    if owners.is_empty() {
                        self.code.page_regions.remove(&page);
                    }
                }
            }
        }
        self.attempted.remove(&start);
        self.hits.remove(&start);
        self.pending_regions.remove(&start);
        self.deferred_compilations.remove(&start);
        self.stats.deferred_compilations = self.deferred_compilations.len();
        self.stats.pending_compilations = self.pending_regions.len();
        self.automatic_regions.remove(&start);
        self.native_heat.remove(&start);
        self.tier_attempted.remove(&start);
    }

    fn evict_oldest(&mut self) {
        let owner = *self
            .code
            .compiled
            .iter()
            .min_by_key(|(_, compiled)| compiled.cache_stamp.get())
            .expect("every published native region has cache age")
            .0;
        self.retire_compiled(owner);
        self.forget_region(owner);
        self.stats.cache_evictions += 1;
    }

    /// Bound resident generated code by both region count and executable bytes.
    /// Defaults are 16384 regions and 128 MiB; a real sample filled 32 MiB within
    /// a minute and then recompiled evicted code. Zero disables native residency.
    /// Eviction is owner-only, with recency sampled at Rust native entries;
    /// individual Tail-linked callees do not add hot-path instrumentation.
    pub fn set_code_cache_limits(&mut self, regions: usize, bytes: usize) {
        self.cache_region_limit = regions;
        self.cache_byte_limit = bytes;
        self.rejected_entries.clear();
        self.attempted.clear();
        while self.code.compiled.len() > regions || self.stats.native_code_bytes > bytes {
            self.evict_oldest();
        }
    }

    fn index_region(&mut self, region: Region) {
        self.index_region_with_pages(region, None);
    }

    fn index_region_with_pages(&mut self, region: Region, dependencies: Option<BTreeSet<u32>>) {
        if let Some(old) = self.code.region_pages.remove(&region.start) {
            for page in old {
                if let Some(owners) = self.code.page_regions.get_mut(&page) {
                    owners.remove(&region.start);
                    if owners.is_empty() {
                        self.code.page_regions.remove(&page);
                    }
                }
            }
        }
        // Include an incomplete instruction's following page: adding EXEC there
        // can make a previously truncated instruction compilable. Keep the index
        // for temporarily unmapped/unexecutable regions so restoration is seen.
        let end = (u64::from(region.start)
            + region.len as u64
            + if region.complete_tail { 15 } else { 0 })
        .min(u64::from(self.memory.base) + self.memory.len as u64);
        let pages = dependencies.unwrap_or_else(|| {
            (u64::from(region.start & !4095)..end)
                .step_by(4096)
                .map(|page| page as u32)
                .collect()
        });
        for &page in &pages {
            self.code
                .page_regions
                .entry(page)
                .or_default()
                .insert(region.start);
        }
        self.code.region_pages.insert(region.start, pages);
        self.code.insert_region(region);
    }

    fn queue_region(&mut self, region: Region) -> Result<bool, String> {
        if self.pending_regions.contains_key(&region.start)
            || self.deferred_compilations.contains_key(&region.start)
        {
            self.stats.compilation_schedule.duplicate_requests += 1;
            return Ok(false);
        }
        let policy = self.background_sender()?.borrow().work_policy();
        // Cold Fast work keeps the responsive slices. A dedicated compiler may
        // optimize an already admitted whole loop under a separate finite cap;
        // shared service jobs retain their original latency budget.
        let limit = policy.background_limit();
        if limit.is_none() && self.pending_regions.len() >= crate::compiler::CAPACITY {
            self.stats.compilation_schedule.capacity_rejections += 1;
            return Ok(false);
        }
        let mut snapshot = compile::Snapshot::capture(&region, &mut self.memory)?;
        snapshot.shadow_returns = self.shadow_returns.is_some();
        self.locate_cache_input(&mut snapshot);
        let mut fragments = if let Some(limit) = limit {
            let budget = crate::compile_budget::background_budget(
                snapshot.region.compile_strategy,
                limit,
                self.compile_threads != 0,
            );
            match crate::compile_budget::plan_with_budget(snapshot, budget) {
                Ok(fragments) => fragments,
                Err(error) => {
                    self.stats.compile_work_rejections += 1;
                    self.compile_errors.insert(region.start, error);
                    while self.compile_errors.len() > 64 {
                        self.compile_errors.pop_first();
                    }
                    self.attempted.extend(
                        region
                            .entries
                            .iter()
                            .map(|&pc| (pc, InterpreterDetail::WorkBudget)),
                    );
                    self.attempted
                        .insert(region.start, InterpreterDetail::WorkBudget);
                    self.tier_attempted.insert(region.start);
                    return Ok(false);
                }
            }
        } else {
            vec![snapshot]
        };
        if self.aot.is_admitting() {
            fragments.truncate(
                self.compile_threads
                    .max(1)
                    .saturating_sub(self.pending_regions.len()),
            );
        }
        // The owner may hold bounded immutable fragments waiting for a permit;
        // no submission waits and no single job contains the original giant unit.
        if self.deferred_compilations.len() + fragments.len() > 4096 {
            self.stats.compilation_schedule.capacity_rejections += 1;
            return Ok(false);
        }
        self.stats.compile_slices += fragments.len().saturating_sub(1) as u64;
        if self.tiered_compilation && self.parallel.is_none() {
            for fragment in &mut fragments {
                if fragment.region.compile_strategy == CompileStrategy::Fast {
                    fragment.profile = Some(Arc::default());
                }
            }
        }
        for fragment in fragments {
            self.mark_aot_fragment(&fragment.region);
            if let Some(compiled) = self
                .persistent_cache
                .as_ref()
                .and_then(|cache| cache.restore(&fragment))
            {
                if !self.aot.is_speculative(fragment.region.start) {
                    self.witness_cache_input(&fragment);
                }
                if let Some((region, compiled)) =
                    self.accept_aot_completion((*fragment.region).clone(), fragment, compiled)
                {
                    self.automatic_regions.insert(region.start);
                    self.publish(region, compiled);
                }
                continue;
            }
            if !self.pending_regions.contains_key(&fragment.region.start) {
                self.schedule_deferred(fragment.region.start);
                self.deferred_sequence += 1;
                self.deferred_order
                    .insert(fragment.region.start, self.deferred_sequence);
                self.deferred_compilations
                    .insert(fragment.region.start, fragment);
            }
        }
        self.stats.deferred_compilations = self.deferred_compilations.len();
        self.stats.peak_deferred_compilations = self
            .stats
            .peak_deferred_compilations
            .max(self.deferred_compilations.len());
        self.submit_deferred_compilations()?;
        Ok(true)
    }

    /// Submit the work the interpreter is spending the most time in first;
    /// otherwise the newest discovery, which is what the guest runs now.
    fn next_deferred(&mut self) -> Option<u32> {
        self.deferred_order
            .retain(|start, _| self.deferred_compilations.contains_key(start));
        self.deferred_compilations
            .iter()
            .max_by_key(|(start, snapshot)| {
                let demand: u64 = snapshot
                    .region
                    .entries
                    .iter()
                    .chain(std::iter::once(*start))
                    .map(|pc| self.demand.get(pc).copied().unwrap_or(0))
                    .sum();
                (
                    !self.aot.is_speculative(**start),
                    demand,
                    self.deferred_order.get(start).copied().unwrap_or(0),
                )
            })
            .map(|(&start, _)| start)
    }

    fn submit_deferred_compilations(&mut self) -> Result<(), String> {
        if self.deferred_compilations.is_empty() {
            return Ok(());
        }
        if self.compiler.is_none() {
            let sender = self.background_sender()?;
            let mut worker = if self.compile_threads == 0 {
                crate::compiler::Worker::with_sender(sender)
            } else {
                crate::compiler::Worker::with_threads(sender, self.compile_threads)
                    .map_err(|error| error.to_string())?
            };
            if let Some(wake) = &self.completion_wake {
                worker.set_waker(wake.clone());
            }
            worker.set_code_cache(self.persistent_cache.clone());
            self.compiler = Some(worker);
            self.stats.compilation_schedule.compile_threads = self.compile_threads.max(1);
            self.stats.compilation_schedule.available_cores =
                std::thread::available_parallelism().map_or(1, usize::from);
        }
        while self.pending_regions.len() < crate::compiler::CAPACITY {
            let Some(start) = self.next_deferred() else {
                break;
            };
            let snapshot = &self.deferred_compilations[&start];
            if !snapshot.valid(&self.memory) {
                let snapshot = self.deferred_compilations.remove(&start).unwrap();
                self.forget_aot_fragment(start);
                self.stats.stale_compilations += 1;
                self.attempted.remove(&start);
                for pc in &snapshot.region.entries {
                    self.attempted.remove(pc);
                }
                continue;
            }
            let region = (*snapshot.region).clone();
            match self.compiler.as_mut().unwrap().try_submit(snapshot.clone()) {
                Ok(ticket) => {
                    self.schedule_submitted(&region, ticket);
                    self.deferred_compilations.remove(&start);
                    self.stats.compile_requests += 1;
                    self.stats.max_compile_work = self
                        .stats
                        .max_compile_work
                        .max(self.compiler.as_ref().unwrap().last_work());
                    self.pending_regions.insert(region.start, ticket);
                    self.stats.pending_compilations = self.pending_regions.len();
                    self.stats.peak_pending_compilations = self
                        .stats
                        .peak_pending_compilations
                        .max(self.pending_regions.len());
                    if !self.aot.is_speculative(start) {
                        self.automatic_regions.insert(region.start);
                        self.index_region(region);
                    }
                }
                Err(crate::compiler::SubmitError::Full) => {
                    self.stats.compilation_schedule.worker_full += 1;
                    break;
                }
                Err(crate::compiler::SubmitError::Closed) => {
                    return Err("compiler worker disconnected".into());
                }
                Err(crate::compiler::SubmitError::WorkLimit { work, limit }) => {
                    self.deferred_compilations.remove(&start);
                    let speculative = self.aot.is_speculative(start);
                    self.forget_aot_fragment(start);
                    if !speculative {
                        self.record_rejection(&region, InterpreterDetail::WorkBudget);
                    }
                    self.stats.compile_work_rejections += 1;
                    self.compile_errors.insert(
                        start,
                        format!("compile work {work} exceeds background budget {limit}"),
                    );
                }
            }
        }
        self.stats.deferred_compilations = self.deferred_compilations.len();
        Ok(())
    }

    /// Publish only on the execution owner, outside generated code, after both
    /// request identity and every captured code-page version have been checked.
    fn collect_compilations(&mut self) -> Result<(), String> {
        self.submit_deferred_compilations()?;
        loop {
            let Some(worker) = self.compiler.as_mut() else {
                return Ok(());
            };
            let completion = match worker.try_recv() {
                Ok(completion) => completion,
                Err(std::sync::mpsc::TryRecvError::Empty) => return Ok(()),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    return Err("compiler worker disconnected".into());
                }
            };
            self.stats.compilations += 1;
            self.schedule_completed(&completion);
            self.stats.compile_time += completion.duration;
            let start = completion.region.start;
            if self.pending_regions.get(&start) != Some(&completion.ticket) {
                self.stats.stale_compilations += 1;
                continue;
            }
            self.pending_regions.remove(&start);
            self.stats.pending_compilations = self.pending_regions.len();
            if !completion.input.valid(&self.memory) {
                self.forget_aot_fragment(start);
                self.stats.stale_compilations += 1;
                self.tier_attempted.remove(&start);
                self.native_heat.remove(&start);
                for pc in &completion.region.entries {
                    self.attempted.remove(pc);
                }
                continue;
            }
            match completion.result {
                Ok(compiled) => {
                    if !self.aot.is_speculative(start) {
                        self.witness_cache_input(&completion.input);
                    }
                    if completion.region.compile_strategy == CompileStrategy::Speed
                        && self.code.compiled.contains_key(&start)
                        && self.tier_attempted.contains(&start)
                    {
                        self.stats.tier_up_publications += 1;
                    }
                    if let Some((region, compiled)) =
                        self.accept_aot_completion(completion.region, completion.input, compiled)
                    {
                        self.publish(region, compiled);
                    }
                }
                Err(error) => {
                    let speculative = self.aot.is_speculative(start);
                    self.forget_aot_fragment(start);
                    // A compilation failure must leave the original guest code
                    // executable by the interpreter; explicit compilation returns errors.
                    self.stats.compile_failures += 1;
                    if !speculative {
                        self.record_rejection(
                            &completion.region,
                            InterpreterDetail::CompileFailure,
                        );
                    }
                    self.compile_errors.insert(start, error);
                    while self.compile_errors.len() > 64 {
                        self.compile_errors.pop_first();
                    }
                }
            }
            self.submit_deferred_compilations()?;
        }
    }

    fn refresh(&mut self) -> Result<(), String> {
        if self.observed_epoch == self.memory.epoch {
            return Ok(());
        }
        self.observed_epoch = self.memory.epoch;
        let mut affected = BTreeSet::new();
        for page in std::mem::take(&mut self.memory.dirty_pages) {
            if let Some(owners) = self.code.page_regions.get(&page) {
                affected.extend(owners.iter().copied());
            }
        }
        let mut missing = Vec::new();
        for pc in affected {
            if let Some(compiled) = self.code.compiled.get_mut(&pc) {
                self.stats.invalidation_candidates += 1;
                if compiled.valid(&self.memory) {
                    continue;
                }
                if compiled.revalidate(&self.memory) {
                    self.stats.code_revalidations += 1;
                    continue;
                }
                self.retire_compiled(pc);
                self.native_heat.remove(&pc);
                self.tier_attempted.remove(&pc);
                self.stats.invalidations += 1;
                self.code.cache_revision += 1;
                if let Some(region) = self.code.regions.get(&pc) {
                    for entry in &region.entries {
                        self.attempted.remove(entry);
                    }
                }
            }
            if let Some(region) = self.code.regions.get(&pc)
                && self.memory.executable(region.start, region.len)
            {
                missing.push(region.clone());
            }
        }
        for region in missing {
            if self.async_compilation && self.automatic_regions.contains(&region.start) {
                self.queue_region(region)?;
            } else {
                self.add_region(region)?;
            }
        }
        Ok(())
    }

    /// Compile automatic regions on `threads` dedicated threads instead of the
    /// shared background service. Configure before the first compilation; zero
    /// (the default) shares the service.
    pub fn set_compile_threads(&mut self, threads: usize) -> Result<(), String> {
        if self.compiler.is_some() {
            return Err("compile threads must be configured before compilation".into());
        }
        self.compile_threads = threads;
        Ok(())
    }

    pub fn enable_auto_jit(&mut self, enabled: bool) {
        self.auto_jit = enabled;
    }
    pub fn set_auto_compile_strategy(&mut self, strategy: CompileStrategy) {
        self.auto_compile_strategy = strategy;
    }
    /// Allow automatic Fast regions to be recompiled for sustained speed. This
    /// only submits bounded asynchronous work; explicit regions are never tiered.
    /// Disabling prevents new promotions; in-flight compilation may still publish.
    pub fn set_tiered_compilation(&mut self, enabled: bool) {
        self.tiered_compilation = enabled && self.parallel.is_none();
    }
    pub fn set_region_linking(&mut self, enabled: bool) {
        if self.region_linking != enabled {
            self.parallel_image = None;
        }
        self.region_linking = enabled;
    }

    /// Experimental return prediction; choose codegen before compiling/submitting any region.
    /// Hints are private to one bounded native chain and never enter a saved CPU
    /// context. Default false preserves the existing checked CALL/RET path.
    pub fn set_shadow_returns(&mut self, enabled: bool) -> Result<(), String> {
        if enabled && self.parallel.is_some() {
            return Err("parallel return-stack hints are not enabled in this release".into());
        }
        if self.shadow_returns.is_some() == enabled {
            return Ok(());
        }
        if self.stats.compilations != 0
            || self.stats.compile_requests != 0
            || !self.code.compiled.is_empty()
            || self.aot.has_ready()
            || !self.pending_regions.is_empty()
            || !self.deferred_compilations.is_empty()
        {
            return Err("shadow-return codegen must be configured before compilation".into());
        }
        self.shadow_returns = enabled.then(|| Box::new(crate::linking::shadow::Stack::new()));
        Ok(())
    }
    pub fn set_profiling(&mut self, enabled: bool) {
        self.profiling = enabled;
    }
    /// Automatic hot-code promotion uses a bounded worker when enabled.
    /// Explicit add_region remains synchronous for authored code and tests.
    pub fn set_async_compilation(&mut self, enabled: bool) {
        self.async_compilation = enabled;
        if !enabled {
            self.schedule = Default::default();
            self.reset_aot_compilations();
            self.tier_attempted.clear();
            self.pending_regions.clear();
            self.deferred_compilations.clear();
            self.stats.deferred_compilations = 0;
            self.stats.pending_compilations = 0;
            self.compiler = None;
        }
    }

    /// Inject a startup-registered owner endpoint before the lazy background
    /// service is first used. UI/GPU and CPU jobs then share exactly one worker.
    pub fn set_background_sender(
        &mut self,
        sender: adv_sync::background::LocalSender,
    ) -> Result<(), String> {
        if self.background_sender.is_some()
            || self
                .compiler
                .as_ref()
                .is_some_and(|worker| worker.has_outstanding())
        {
            return Err("background sender must be installed before first use".into());
        }
        let wake = sender.borrow().execution_wake();
        if self
            .background_wake
            .as_ref()
            .is_some_and(|old| !Arc::ptr_eq(&old.interrupt_flag(), &wake.interrupt_flag()))
        {
            return Err("background service must use the execution owner's wake event".into());
        }
        self.set_wake_event(wake)?;
        self.background_sender = Some(sender);
        Ok(())
    }

    /// Return the same local producer for other execution-owned components.
    /// A standalone executor lazily creates one service; shells requiring a UI
    /// endpoint must register/inject both ports before the first guest run.
    pub fn background_sender(&mut self) -> Result<adv_sync::background::LocalSender, String> {
        if let Some(sender) = &self.background_sender {
            return Ok(sender.clone());
        }
        let wake = self
            .background_wake
            .clone()
            .map_or_else(adv_sync::WakeEvent::new, Ok)
            .map_err(|error| error.to_string())?;
        let mut builder =
            adv_sync::background::Builder::new(wake).map_err(|error| error.to_string())?;
        let sender = builder
            .sender(crate::compiler::CAPACITY, crate::compiler::CAPACITY)
            .map_err(|error| error.to_string())?
            .into_local();
        self.background_service = Some(std::rc::Rc::new(
            builder.start().map_err(|error| error.to_string())?,
        ));
        self.background_sender = Some(sender.clone());
        Ok(sender)
    }

    pub(crate) fn set_wake_event(&mut self, wake: adv_sync::WakeEvent) -> Result<(), String> {
        if self.background_sender.as_ref().is_some_and(|sender| {
            !Arc::ptr_eq(
                &sender.borrow().execution_wake().interrupt_flag(),
                &wake.interrupt_flag(),
            )
        }) {
            return Err("wake event cannot change after background service registration".into());
        }
        let signal = wake.clone();
        self.set_completion_waker(Arc::new(move || signal.signal()))?;
        self.interrupt = wake.interrupt_flag();
        self.background_wake = Some(wake);
        Ok(())
    }

    /// Register before running guest code. Replacing a notifier while owned
    /// compile jobs are pending would leave their completions on the old wake.
    pub fn set_completion_waker(
        &mut self,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<(), String> {
        if !self.pending_regions.is_empty() {
            return Err("cannot replace compiler wake while compile jobs are pending".into());
        }
        if let Some(worker) = &mut self.compiler {
            worker.set_waker(wake.clone());
        }
        self.completion_wake = Some(wake);
        Ok(())
    }

    /// The owner clears this flag before draining host channels, never here.
    pub fn set_host_interrupt_flag(&mut self, interrupt: Arc<AtomicBool>) {
        self.interrupt = interrupt;
    }

    pub fn host_interrupt_pending(&self) -> bool {
        self.host_interrupts && self.interrupt.load(Ordering::Acquire)
    }

    pub fn set_host_interrupts(&mut self, enabled: bool) {
        self.host_interrupts = enabled;
    }

    fn may_tier_up(&self, owner: u32) -> bool {
        self.auto_jit
            && self.async_compilation
            && self.tiered_compilation
            && self.code.fast_regions.contains(&owner)
            && !self.tier_attempted.contains(&owner)
            && self.automatic_regions.contains(&owner)
            // Reserve at least half the bounded queue for cold discoveries.
            && self.pending_regions.len() < crate::compiler::CAPACITY / 2
            && !self.pending_regions.contains_key(&owner)
    }

    fn record_native_work(&mut self, owner: u32, blocks: u64) {
        if blocks == 0 || !self.may_tier_up(owner) {
            return;
        }
        // Native chains are sampled at their Rust entry, not timed per guest
        // instruction. Units entered through region links count their own
        // blocks in their profile instead (`scan_profiles`).
        let heat = self.native_heat.entry(owner).or_default();
        *heat = heat.saturating_add(blocks);
        if *heat >= HOT_BLOCKS {
            self.request_tier_up(owner);
        }
    }

    /// Tier up the hottest profiled Fast unit, including one that only runs
    /// behind region links and never returns to Rust itself. Cold code waiting
    /// for its first compilation keeps the background compiler first.
    fn scan_profiles(&mut self) {
        if !self.deferred_compilations.is_empty() {
            return;
        }
        self.profiled
            .retain(|(_, profile)| Arc::strong_count(profile) > 1);
        // Reading a counter is cheap; only hot units pay the eligibility lookups.
        let hottest = self
            .profiled
            .iter()
            .map(|(owner, profile)| (profile.heat(), *owner))
            .filter(|&(heat, owner)| heat >= HOT_BLOCKS && self.may_tier_up(owner))
            .max();
        if let Some((_, owner)) = hottest {
            self.request_tier_up(owner);
        }
    }

    fn request_tier_up(&mut self, owner: u32) {
        const CALLEES: usize = 8;
        let Some(region) = self.code.regions.get(&owner) else {
            return;
        };
        let Some(compiled) = self
            .code
            .compiled
            .get(&owner)
            .filter(|c| c.valid(&self.memory))
        else {
            return;
        };
        if region.compile_strategy != CompileStrategy::Fast {
            return;
        }
        let old_region = region.clone();
        let mut optimized = old_region.clone();
        optimized.compile_strategy = CompileStrategy::Speed;
        // Direct leaves already inside this unit's immutable byte window do not
        // need a racy call-target hint. Admission still decodes the current bytes
        // and permits only a bounded, register-only leaf through its return.
        let direct: BTreeSet<u32> = compile::Snapshot::capture(&old_region, &mut self.memory)
            .map(|input| input.direct_leaf_callees().into_iter().collect())
            .unwrap_or_default();
        // Observed indirect call targets join the unit, so the calls and their
        // returns dispatch inside it. Each target is offered once per unit.
        let tried = self.callee_attempts.entry(owner).or_default();
        let observed = compiled
            .profile()
            .map(|p| p.call_targets())
            .unwrap_or_default();
        let fresh: Vec<u32> = observed
            .into_iter()
            .chain(direct.iter().copied())
            .filter(|target| !tried.contains(target))
            .collect();
        if !old_region.callees.is_empty() && fresh.is_empty() {
            // Already whole with its callees, since Speed did not fit. Later
            // scans look for targets the guest selects afterwards.
            return;
        }
        for target in fresh {
            if optimized.callees.len() < CALLEES && !optimized.callees.contains(&target) {
                optimized.callees.push(target);
            }
            if direct.contains(&target) && optimized.callees.contains(&target) {
                // Only this verified leaf's entry changes from external to
                // internal. Other published, pending and CFG frontiers survive.
                optimized.frontier.retain(|pc| *pc != target);
            }
            tried.insert(target);
        }
        match self.queue_region(optimized) {
            Ok(true) => {
                // Keep Fast code AND its published metadata live while Speed is
                // compiling. Failure must not demote hot code to interpretation.
                if self.pending_regions.contains_key(&owner)
                    || self.deferred_compilations.contains_key(&owner)
                {
                    self.code.insert_region(old_region);
                    self.tier_attempted.insert(owner);
                }
                self.stats.tier_up_requests += 1;
            }
            Ok(false) => {} // Cold compilation owns the bounded queue first.
            Err(error) => {
                self.tier_attempted.insert(owner);
                self.compile_errors.insert(owner, error);
                while self.compile_errors.len() > 64 {
                    self.compile_errors.pop_first();
                }
            }
        }
    }

    fn refresh_links(&mut self) {
        if self.code.links_revision != self.code.cache_revision {
            // Rebuilt on the next parallel dispatch; dropping it lets the table update in place.
            self.parallel_image = None;
        }
        self.code.refresh_links();
    }

    fn validate_stop(&mut self, stop: u32) -> Result<(), String> {
        if self.code.stop_is_outside(stop) {
            Ok(())
        } else {
            Err("stop address must be outside the compiled region".into())
        }
    }
    fn discover(&mut self, stop: u32) -> Result<(), String> {
        let pc = self.state.eip;
        if !self.auto_jit
            || self.code.entry_owner.contains_key(&pc)
            || !self.memory.executable(pc, 1)
        {
            return Ok(());
        }
        if let Some(pages) = self.rejected_entries.get(&pc) {
            if pages
                .iter()
                .all(|&(page, version)| self.memory.versions.get(page) == Some(&version))
            {
                return Ok(());
            }
            self.rejected_entries.remove(&pc);
            self.attempted.remove(&pc);
        }
        if self.attempted.contains_key(&pc) {
            return Ok(());
        }
        let hits = self.hits.entry(pc).or_default();
        *hits = hits.saturating_add(1);
        if *hits < 32 {
            return Ok(());
        }
        // The interpreter keeps running a loop while its region compiles. Its
        // interior PCs must not become regions of their own: each would bound
        // later discoveries and turn every iteration into region transfers.
        let covers = |region: &Region| pc.wrapping_sub(region.start) < region.len as u32;
        if self
            .pending_regions
            .keys()
            .filter_map(|owner| self.code.regions.get(owner))
            .any(covers)
            || self
                .deferred_compilations
                .values()
                .filter(|input| !self.aot.is_speculative(input.region.start))
                .any(|input| covers(&input.region))
        {
            self.stats.compilation_schedule.window_deferrals += 1;
            return Ok(());
        }
        let boundaries = self
            .code
            .entry_owner
            .keys()
            .copied()
            .chain(
                self.pending_regions
                    .keys()
                    .filter_map(|owner| self.code.regions.get(owner))
                    .flat_map(|region| {
                        region
                            .entries
                            .iter()
                            .copied()
                            .chain(std::iter::once(region.start))
                    }),
            )
            .chain(
                self.deferred_compilations
                    .values()
                    .filter(|input| !self.aot.is_speculative(input.region.start))
                    .flat_map(|input| {
                        input
                            .region
                            .entries
                            .iter()
                            .copied()
                            .chain(std::iter::once(input.region.start))
                    }),
            )
            .collect();
        let region = match crate::discovery::discover(
            &mut self.memory,
            pc,
            stop,
            self.auto_compile_strategy,
            &boundaries,
        ) {
            Ok(region) => region,
            Err(reason) => {
                self.attempted.insert(pc, reason);
                return Ok(());
            }
        };
        if self.async_compilation {
            if self.queue_region(region)? {
                self.attempted
                    .insert(pc, InterpreterDetail::NoPublishedEntry);
            }
        } else {
            self.add_region(region)?;
            self.attempted
                .insert(pc, InterpreterDetail::NoPublishedEntry);
        }
        Ok(())
    }

    pub fn map(&mut self, addr: u32, len: usize, perms: u8) -> Result<(), String> {
        self.memory.map(addr, len, perms)
    }
    pub fn unmap(&mut self, addr: u32, len: usize) -> Result<(), String> {
        self.memory.unmap(addr, len)
    }

    pub fn run(&mut self, stop: u32, budget: u64) -> Result<RunOutcome, String> {
        self.run_execution(stop, budget, None, false)
    }

    pub(crate) fn run_inner(
        &mut self,
        stop: u32,
        budget: u64,
        bridge: &mut Bridge<'_>,
        interpreter_only: bool,
        mut instruction_trace: Option<&mut instruction_trace::Trace>,
    ) -> Result<RunOutcome, String> {
        self.validate_stop(stop)?;
        self.state.refresh_mmx();
        self.state.fuel = budget;
        if !interpreter_only {
            self.apply_aot()?;
            self.apply_code_cache(stop);
            self.apply_warm()?;
            self.validate_stop(stop)?;
        }
        loop {
            if bridge.parked {
                return Ok(RunOutcome::Parked);
            }
            if self.state.eip == stop {
                return Ok(RunOutcome::ReachedStop);
            }
            let now = self.deadline.map(|_| Instant::now());
            if self.deadline.zip(now).is_some_and(|(t, now)| now >= t) {
                return Ok(RunOutcome::Budget);
            }
            if self.state.fuel == 0 {
                return Ok(RunOutcome::Budget);
            }
            if self.host_interrupts && self.interrupt.load(Ordering::Acquire) {
                return Ok(RunOutcome::Interrupted);
            }
            if self
                .state
                .rep
                .as_ref()
                .is_some_and(|rep| rep.pc != self.state.eip)
            {
                self.state.rep = None;
            }
            self.refresh()?;
            self.collect_compilations()?;
            if !interpreter_only {
                self.apply_aot()?;
                self.apply_code_cache(stop);
                self.admit_aot(self.state.eip, stop);
            }
            let single_step = self.state.eflags & (1 << 8) != 0;
            // Pending x87 exceptions must reach a waiting x87/MMX instruction
            // before it changes state. Native MMX assumes no pending #MF.
            let pending_fpu = self.state.x87.status & 0x80 != 0;
            if !interpreter_only
                && !single_step
                && !pending_fpu
                && self.tier1_mode != crate::Tier1Mode::Exclusive
            {
                self.discover(stop)?;
            }
            // Publications and queued regions can change the ranges during this
            // run. The same stop fact remains reusable until their generation changes.
            self.validate_stop(stop)?;
            let native = self
                .code
                .entry_owner
                .get(&self.state.eip)
                .copied()
                .and_then(|owner| self.code.compiled.get(&owner).map(|native| (owner, native)));
            if let Some((_, native)) = native {
                self.cache_clock = self.cache_clock.saturating_add(1);
                native.cache_stamp.set(self.cache_clock);
            }
            let native = native.filter(|(_, code)| {
                !interpreter_only
                    && self.tier1_mode != crate::Tier1Mode::Exclusive
                    && !single_step
                    && !pending_fpu
                    && code.entries.contains(&self.state.eip)
                    && self.deferred_fallback != Some(self.state.eip)
            });
            let native = native.map(|(owner, _)| owner);
            if native.is_some()
                && self.parallel.is_some()
                && self.parallel_demand
                && self.admission.ready()
                && self.has_parallel_capacity()
                && self.deadline.is_none_or(|deadline| {
                    deadline.saturating_duration_since(Instant::now()) >= Duration::from_micros(100)
                })
            {
                return Ok(RunOutcome::NativeReady);
            }
            if let Some(owner) = native {
                if self.region_linking && !self.profiling {
                    self.refresh_links();
                }
                // Parallel workers share memory, so a private undo is not possible.
                let serial = self.parallel.is_none();
                let trace = match self.verifier.as_deref_mut() {
                    Some(verifier) if serial && verifier.pending.contains_key(&owner) => {
                        verifier.take(owner);
                        Some(verifier.record(
                            owner,
                            stop,
                            &mut self.interpreter,
                            &mut self.state,
                            &mut self.memory,
                        )?)
                    }
                    _ => None,
                };
                let native = &self.code.compiled[&owner];
                // Internal region loops do not use the inter-region chain limit.
                // Reserve unused caller fuel and periodically return through Rust
                // while a wall deadline is active, without charging extra work.
                // At the measured rate, the allowance covers about half the time
                // left, so hot loops rarely leave native code early.
                const SAFEPOINT: u64 = 256;
                let timed = self.deadline.zip(now);
                let allowance = timed.map_or(u64::MAX, |(deadline, now)| {
                    let left = deadline.saturating_duration_since(now).as_micros() as u64;
                    (left / 2).saturating_mul(self.native_rate).max(SAFEPOINT)
                });
                // A verified entry runs one block: without back edges, its exit
                // PC names a single interpreted state.
                let allowance = if trace.is_some() { 1 } else { allowance };
                let reserved = self.state.fuel.saturating_sub(allowance);
                self.state.fuel -= reserved;
                let before = self.state.fuel;
                let pc = self.state.eip;
                let timer = self.profiling.then(Instant::now);
                let (result, linked) = native.run(
                    &mut self.state,
                    &mut self.memory,
                    bridge,
                    compile::NativeRun {
                        rows: if self.parallel.is_none() {
                            self.rows.as_deref()
                        } else {
                            None
                        },
                        // A verified entry stops at the region boundary.
                        links: (self.region_linking && !self.profiling && trace.is_none())
                            .then_some(&self.code.links),
                        stop,
                        interrupt: if self.host_interrupts {
                            self.interrupt.as_ref()
                        } else {
                            &self.inactive_interrupt
                        },
                        shadow: self
                            .shadow_returns
                            .as_deref_mut()
                            .filter(|_| trace.is_none()),
                    },
                );
                if let Some(trace) = &trace {
                    let check = trace.check(&self.state, &self.memory);
                    self.record_verification(check)?;
                }
                if let Some(timer) = timer {
                    let p = self.profile.entry(pc).or_default();
                    p.native_entries += 1;
                    p.native_ns += timer.elapsed().as_nanos() as u64;
                }
                self.stats.native_entries += 1 + linked;
                self.stats.linked_entries += linked;
                if let Some(shadow) = &self.shadow_returns {
                    self.stats.shadow_matches = shadow.matches;
                    self.stats.shadow_hits = shadow.hits;
                    self.stats.shadow_misses = shadow.misses;
                    self.stats.shadow_overflows = shadow.overflows;
                }
                let completed = before - self.state.fuel;
                if let Some((_, now)) = timed
                    && reserved != 0
                    && trace.is_none()
                    && result == Exit::Budget as u32
                {
                    let elapsed = (now.elapsed().as_micros() as u64).max(1);
                    let rate = completed / elapsed;
                    self.native_rate = if self.native_rate == 0 {
                        rate
                    } else {
                        (3 * self.native_rate + rate) / 4
                    };
                }
                self.state.fuel += reserved;
                self.stats.native_blocks += completed;
                self.record_native_work(owner, completed);
                // A scan visits every Fast unit; pace it by native work so
                // short native runs between interpreted code stay cheap.
                if self.stats.native_blocks - self.scanned_blocks >= SCAN_BLOCKS {
                    self.scanned_blocks = self.stats.native_blocks;
                    self.scan_profiles();
                }
                match result {
                    n if n == Exit::Trap as u32 => {
                        if let Some(error) = bridge.error.take() {
                            return Err(error);
                        }
                        if let Some((pc, vector)) = bridge.trap.take() {
                            return Ok(RunOutcome::Trap { pc, vector });
                        }
                        continue;
                    }
                    n if n == Exit::Dispatch as u32 => continue,
                    n if n == Exit::Interrupted as u32 => {
                        if self.deadline.is_some_and(|t| Instant::now() >= t) {
                            return Ok(RunOutcome::Budget);
                        }
                        return Ok(RunOutcome::Interrupted);
                    }
                    n if n == Exit::Budget as u32 => {
                        if reserved != 0 {
                            // Only the internal safepoint allowance ran out.
                            // Recheck the actual deadline and retain caller fuel.
                            continue;
                        }
                        return Ok(RunOutcome::Budget);
                    }
                    n if n == Exit::ReadFault as u32 => {
                        return Ok(RunOutcome::ReadFault {
                            pc: self.state.eip,
                            address: self.state.fault_addr,
                        });
                    }
                    n if n == Exit::WriteFault as u32 => {
                        return Ok(RunOutcome::WriteFault {
                            pc: self.state.eip,
                            address: self.state.fault_addr,
                        });
                    }
                    n if n == Exit::Fallback as u32 => {
                        self.deferred_fallback = Some(self.state.eip);
                        bridge.cliffs.fallback(&self.memory, self.state.eip);
                        // Preserve the pending instruction across a scheduler
                        // slice, even when its native guard consumed the last unit.
                        if self.state.fuel == 0 {
                            return Ok(RunOutcome::Budget);
                        }
                    }
                    _ => return Err(format!("invalid native exit {result}")),
                }
            }
            use crate::interpreter::Step;
            // The interpreter owns its decode cache and this CPU state. Ordinary
            // scalar memory operations use the same foreign-memory accessors as
            // API handlers; only atomic/extended operations request a barrier.
            // Executable writes and mapping changes also retain Memory's barrier.
            let pc = self.state.eip;
            let timer = self.profiling.then(Instant::now);
            // Contiguous integer and floating fallback instructions share a bounded
            // decoded block. Transfers, exceptions, SMC and REP return here; native
            // publications change the revision used to build fallthrough boundaries.
            let units = self.state.fuel.min(256) as u32;
            let (units, observed) = instruction_trace
                .as_deref_mut()
                .map_or((units, None), |trace| {
                    trace.prepare(units, &self.state, &self.memory)
                });
            let reason = {
                use crate::InterpreterReason as Reason;
                if interpreter_only || single_step || pending_fpu {
                    Reason::Forced
                } else if self.deferred_fallback == Some(pc) {
                    Reason::Fallback
                } else {
                    match self.code.entry_owner.get(&pc) {
                        Some(owner) if self.code.compiled.contains_key(owner) => Reason::Interior,
                        Some(_) => Reason::Pending,
                        None if self.attempted.contains_key(&pc) => Reason::Rejected,
                        None => Reason::Uncovered,
                    }
                }
            };
            let detail = matches!(
                reason,
                crate::InterpreterReason::Uncovered | crate::InterpreterReason::Rejected
            )
            .then(|| self.interpreter_detail(pc));
            if self.interpreter.census_enabled && detail.is_some() && self.tier1.is_none() {
                self.interpreter.census = Some(Default::default());
            }
            let waiting = matches!(
                detail,
                Some(
                    InterpreterDetail::PendingCompilation | InterpreterDetail::DeferredCompilation
                )
            );
            let wait_timer = self.frame_timing.then(Instant::now);
            let retired_before = self.interpreter.frame_retired;
            let tier1 = self.run_tier1(
                stop,
                units,
                interpreter_only
                    || single_step
                    || pending_fpu
                    || (self.tier1_mode == crate::Tier1Mode::Bridge
                        && (self.parallel.is_some() || self.deferred_fallback == Some(pc))),
            )?;
            let (execution, last_units) = if let Some(result) = &tier1 {
                (result.execution, result.last_units)
            } else if self.profiling || single_step || observed.is_some() {
                let execution =
                    self.interpreter
                        .run_chunk(&mut self.state, &mut self.memory, units);
                (execution, execution.units)
            } else {
                self.interpreter.run_block(
                    &mut self.state,
                    &mut self.memory,
                    units,
                    stop,
                    (!interpreter_only
                        && !single_step
                        && !pending_fpu
                        && !self.code.entry_owner.is_empty())
                    .then_some((&self.code.entry_owner, self.code.cache_revision)),
                )
            };
            if let Some(entry) = self.interpreter.census.take() {
                bridge
                    .cliffs
                    .tier1_census
                    .get_or_insert_with(Default::default)
                    .record(reason, detail.unwrap(), entry);
            }
            let interpreted_units = tier1
                .as_ref()
                .map_or(execution.units, |r| r.interpreted_units);
            if tier1.is_none()
                && let Some(before) = retired_before
            {
                self.stats.frame_retired_tier0 += self
                    .interpreter
                    .frame_retired
                    .unwrap()
                    .saturating_sub(before);
            }
            if let Some(timer) = wait_timer {
                let elapsed = timer.elapsed().as_nanos() as u64;
                // Tier 1 bridge time remains inclusive; frame reports identify
                // that configuration rather than claiming pure Tier 0 time.
                let index = crate::frame_counters::reason_index(if waiting {
                    crate::InterpreterReason::Pending
                } else {
                    reason
                });
                self.stats.frame_reason_ns[index] += elapsed;
                if waiting || reason == crate::InterpreterReason::Pending {
                    self.stats.compile_wait_execution_ns += elapsed;
                    self.stats.compile_wait_units += u64::from(execution.units);
                }
            }
            let interpreted = tier1.as_ref().is_none_or(|r| r.interpreted);
            if self.tier1_mode == crate::Tier1Mode::Bridge && tier1.is_some() {
                let native_units = execution.units - interpreted_units;
                if native_units != 0 {
                    bridge.cliffs.bridged(reason, detail, native_units);
                }
            }
            self.stats.interpreter_entries += u64::from(interpreted);
            self.stats.interpreter_instructions += u64::from(interpreted_units);
            if interpreted {
                if matches!(
                    detail,
                    Some(
                        InterpreterDetail::PendingCompilation
                            | InterpreterDetail::DeferredCompilation
                    )
                ) {
                    self.schedule_interpreted(pc, interpreted_units);
                }
                bridge.cliffs.interpreted(reason, interpreted_units);
                if let Some(detail) = detail {
                    bridge.cliffs.detail(reason, detail, interpreted_units);
                }
                if let Some(profile) = &mut self.interpreter_profile {
                    profile.record(pc, interpreted_units, reason, &self.memory);
                }
            }
            if !self.deferred_compilations.is_empty() && !self.code.entry_owner.contains_key(&pc) {
                if self.demand.len() >= 65_536 {
                    self.demand.clear();
                }
                *self.demand.entry(pc).or_default() += 1;
            }
            self.state.fuel -= u64::from(execution.units);
            if let Some(timer) = timer {
                let p = self.profile.entry(pc).or_default();
                p.interpreter_entries += u64::from(interpreted);
                p.interpreter_instructions += u64::from(interpreted_units);
                if tier1.is_some() {
                    p.native_entries += 1;
                    p.native_ns += timer.elapsed().as_nanos() as u64;
                } else {
                    p.interpreter_ns += timer.elapsed().as_nanos() as u64;
                }
            }
            self.deferred_fallback = None;
            let step = match execution.step {
                Step::ReadTimestamp { pc, next } => {
                    let ticks = bridge.read_timestamp();
                    self.state.regs[0] = ticks as u32;
                    self.state.regs[2] = (ticks >> 32) as u32;
                    self.state.eip = next;
                    if single_step {
                        Step::Trap { pc, vector: 1 }
                    } else {
                        Step::Executed
                    }
                }
                Step::InvalidInstruction { pc } if bridge.handler.is_some() => {
                    Step::Trap { pc, vector: 6 }
                }
                other => other,
            };
            if let Some(observed) = observed {
                instruction_trace.as_deref_mut().unwrap().complete(
                    observed,
                    step,
                    execution.units,
                    &self.state,
                    &self.memory,
                );
            }
            match step {
                Step::Executed => {}
                Step::ReadTimestamp { .. } => unreachable!("completed by the execution owner"),
                Step::ReadFault { pc, address } => {
                    self.state.fault_addr = address;
                    return Ok(RunOutcome::ReadFault { pc, address });
                }
                Step::WriteFault { pc, address } => {
                    self.state.fault_addr = address;
                    return Ok(RunOutcome::WriteFault { pc, address });
                }
                Step::FetchFault { pc, address } => {
                    self.state.fault_addr = address;
                    return Ok(RunOutcome::FetchFault { pc, address });
                }
                Step::InvalidInstruction { pc } => {
                    return Ok(RunOutcome::InvalidInstruction { pc });
                }
                Step::Unsupported { pc, code } => {
                    return Ok(RunOutcome::UnsupportedInstruction { pc, code });
                }
                Step::Trap { pc, vector } => {
                    if bridge.handler.is_none() {
                        return Ok(RunOutcome::Trap { pc, vector });
                    }
                    if last_units == 0 {
                        // Prefix instructions in a batch must not hide the final
                        // faulting instruction's zero-progress fuel charge.
                        // A handler repeatedly returning to an unchanged fault
                        // must still yield to the machine scheduler.
                        self.state.fuel = self.state.fuel.saturating_sub(1);
                    }
                    bridge.memory = &mut self.memory;
                    crate::trap::call_trap(bridge, &mut self.state, u32::from(vector));
                    if let Some(error) = bridge.error.take() {
                        return Err(error);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod responsive_compile_tests {
    use super::*;
    use crate::{EXEC, READ, WRITE};
    use adv_sync::{
        WakeEvent,
        background::{Builder, Service, WorkPolicy},
    };
    use iced_x86::code_asm::*;

    const CODE: u32 = 0x10000;
    const DATA: u32 = 0x20000;
    const STACK: u32 = 0x2f000;
    const STOP: u32 = 0x2e000;

    fn fixture(limit: usize) -> (Executor, Service) {
        let mut m = Executor::new(CODE, 0x20000).unwrap();
        m.protect(CODE, 0x20000, READ | WRITE).unwrap();
        let wake = WakeEvent::new().unwrap();
        let mut builder = Builder::new(wake).unwrap().with_work_policy(if limit == 0 {
            WorkPolicy::default()
        } else {
            WorkPolicy::responsive(limit)
        });
        let sender = builder.sender(4, 8).unwrap().into_local();
        let service = builder.start().unwrap();
        m.set_background_sender(sender).unwrap();
        m.set_async_compilation(true);
        m.set_host_interrupts(false);
        (m, service)
    }
    fn assemble(f: impl FnOnce(&mut CodeAssembler)) -> Vec<u8> {
        let mut a = CodeAssembler::new(32).unwrap();
        f(&mut a);
        a.assemble(u64::from(CODE)).unwrap()
    }
    fn region(m: &mut Executor, bytes: &[u8]) -> Region {
        m.write(CODE, bytes).unwrap();
        m.protect(CODE, 4096, READ | EXEC).unwrap();
        let mut region = Region::new(CODE, bytes.len());
        region.compile_strategy = CompileStrategy::Fast;
        region
    }
    fn drain(m: &mut Executor) {
        let until = Instant::now() + Duration::from_secs(10);
        while !m.pending_regions.is_empty() || !m.deferred_compilations.is_empty() {
            assert!(Instant::now() < until, "fragment publication stalled");
            m.collect_compilations().unwrap();
            std::thread::yield_now();
        }
    }
    fn seed(m: &mut Executor, iterations: u32) {
        m.write(STACK, &STOP.to_le_bytes()).unwrap();
        m.state = CpuState::default();
        m.state.eip = CODE;
        m.state.regs[1] = iterations;
        m.state.regs[4] = STACK;
    }

    #[test]
    fn dedicated_pool_tiers_whole_loops_beyond_the_shared_speed_limit() {
        let bytes = assemble(|a| {
            let mut top = a.create_label();
            a.set_label(&mut top).unwrap();
            for _ in 0..64 {
                a.mov(eax, dword_ptr(DATA)).unwrap();
            }
            a.dec(ecx).unwrap();
            a.jnz(top).unwrap();
            a.ret().unwrap();
        });
        let mut states = Vec::new();
        for dedicated in [false, true] {
            let (mut m, _service) = fixture(256);
            m.set_compile_threads(usize::from(dedicated)).unwrap();
            let unit = region(&mut m, &bytes);
            assert!(m.queue_region(unit).unwrap());
            drain(&mut m);
            assert_eq!(m.stats.compile_slices, 0);
            m.request_tier_up(CODE);
            // Publication never removes the current Fast body while waiting.
            assert_eq!(
                m.code.regions[&CODE].compile_strategy,
                CompileStrategy::Fast
            );
            drain(&mut m);
            assert_eq!(
                m.code.regions[&CODE].compile_strategy,
                if dedicated {
                    CompileStrategy::Speed
                } else {
                    CompileStrategy::Fast
                }
            );
            assert_eq!(m.stats.compile_work_rejections, u64::from(!dedicated));
            assert_eq!(m.stats.tier_up_publications, u64::from(dedicated));
            m.write(DATA, &37u32.to_le_bytes()).unwrap();
            seed(&mut m, 3);
            assert_eq!(m.run(STOP, 20).unwrap(), RunOutcome::ReachedStop);
            assert_eq!(m.state.regs[0], 37);
            states.push(m.state.clone());
        }
        assert_eq!(states[0], states[1]);
    }

    #[test]
    fn large_authored_units_are_bounded_and_linked_without_losing_budget_or_smc() {
        let (mut m, _service) = fixture(crate::RESPONSIVE_COMPILE_WORK);
        let bytes = assemble(|a| {
            let mut begin = a.create_label();
            a.set_label(&mut begin).unwrap();
            // Too large even for the loop allowance, so it must still be sliced.
            for _ in 0..200 {
                a.add(dword_ptr(DATA), 1).unwrap();
            }
            a.dec(ecx).unwrap();
            a.jnz(begin).unwrap();
            a.ret().unwrap();
        });
        let region = region(&mut m, &bytes);
        assert!(m.queue_region(region.clone()).unwrap());
        drain(&mut m);
        assert!(m.code.compiled.len() > 1);
        assert!(m.stats.max_compile_work <= crate::RESPONSIVE_COMPILE_WORK);
        seed(&mut m, 2);
        loop {
            match m.run(STOP, 2).unwrap() {
                RunOutcome::Budget => {}
                RunOutcome::ReachedStop => break,
                other => panic!("unexpected {other:?}"),
            }
        }
        assert_eq!(
            m.memory.read_data(DATA, 4).unwrap().as_ref(),
            &400u32.to_le_bytes()
        );
        assert_eq!(m.state.regs[1], 0);
        assert_eq!(m.state.regs[4], STACK + 4);
        assert!(m.stats.linked_entries > 0);
        // Patch an instruction in the last fragment; no old piece may execute.
        let mut patched = bytes.clone();
        let position = patched
            .windows(7)
            .rposition(|w| w == [0x83, 0x05, 0, 0, 2, 0, 1])
            .unwrap();
        patched[position + 6] = 2;
        m.write(CODE, &patched).unwrap();
        seed(&mut m, 1);
        assert_eq!(m.run(STOP, 1000).unwrap(), RunOutcome::ReachedStop);
        assert_eq!(
            m.memory.read_data(DATA, 4).unwrap().as_ref(),
            &601u32.to_le_bytes()
        );
        drain(&mut m);
        assert!(m.stats.invalidations > 0);
    }

    #[test]
    fn deferred_inputs_keep_versions_and_speed_rejection_preserves_live_fast_code() {
        let (mut m, _service) = fixture(16);
        let bytes = assemble(|a| {
            for _ in 0..32 {
                a.mov(eax, dword_ptr(DATA)).unwrap();
            }
            a.ret().unwrap();
        });
        let mut original = region(&mut m, &bytes);
        assert!(m.queue_region(original.clone()).unwrap());
        assert!(!m.deferred_compilations.is_empty());
        m.write(CODE, &[0xb8, 42, 0, 0, 0, 0x90]).unwrap();
        drain(&mut m);
        assert!(
            m.code.compiled.is_empty(),
            "old deferred/active inputs must never publish"
        );
        assert!(m.stats.stale_compilations > 0);
        assert!(m.queue_region(original.clone()).unwrap());
        drain(&mut m);
        let old = m.code.compiled[&CODE].tail_entry;
        original.compile_strategy = CompileStrategy::Speed;
        assert!(!m.queue_region(original).unwrap());
        assert_eq!(m.code.compiled[&CODE].tail_entry, old);
        assert!(m.tier_attempted.contains(&CODE));
        assert_eq!(m.stats.compile_work_rejections, 1);
    }

    #[test]
    fn faults_in_later_fragments_preserve_completed_prefix_and_retry_precisely() {
        let (mut m, _service) = fixture(16);
        let bytes = assemble(|a| {
            a.mov(dword_ptr(DATA), 123).unwrap();
            a.add(eax, 7).unwrap();
            a.mov(ebx, dword_ptr(DATA + 4096)).unwrap();
            a.mov(dword_ptr(DATA + 4), ebx).unwrap();
            a.ret().unwrap();
        });
        let original = region(&mut m, &bytes);
        m.queue_region(original).unwrap();
        drain(&mut m);
        m.protect(DATA + 4096, 4096, 0).unwrap();
        seed(&mut m, 0);
        let RunOutcome::ReadFault { pc, address } = m.run(STOP, 100).unwrap() else {
            panic!("missing precise fault");
        };
        assert_eq!(address, DATA + 4096);
        assert_eq!(m.state.regs[0], 7);
        assert_eq!(
            m.memory.read_data(DATA, 4).unwrap().as_ref(),
            &123u32.to_le_bytes()
        );
        assert_eq!(m.state.eip, pc);
        m.protect(DATA + 4096, 4096, READ | WRITE).unwrap();
        m.write(DATA + 4096, &77u32.to_le_bytes()).unwrap();
        assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
        assert_eq!(m.state.regs[0], 7, "completed prefix was replayed");
        assert_eq!(
            m.memory.read_data(DATA + 4, 4).unwrap().as_ref(),
            &77u32.to_le_bytes()
        );
    }

    #[test]
    #[ignore = "diagnostic responsive compilation cost/throughput, not a timing gate"]
    fn report_responsive_compile_tradeoffs() {
        #[cfg(windows)]
        unsafe {
            unsafe extern "system" {
                fn GetCurrentProcess() -> *mut std::ffi::c_void;
                fn SetProcessAffinityMask(process: *mut std::ffi::c_void, mask: usize) -> i32;
            }
            assert_ne!(SetProcessAffinityMask(GetCurrentProcess(), 1), 0);
        }
        println!(
            "case,work_limit,cold_p50_ms,total_compile_p50_ms,hot_p50_ns_per_loop,regions,code_bytes,linked_per_loop"
        );
        for case in ["scalar", "simd"] {
            let bytes = assemble(|a| {
                let mut begin = a.create_label();
                a.set_label(&mut begin).unwrap();
                if case == "scalar" {
                    for _ in 0..63 {
                        a.mov(eax, dword_ptr(DATA)).unwrap();
                        a.add(ebx, dword_ptr(DATA + 4)).unwrap();
                        a.mov(dword_ptr(DATA + 8), ebx).unwrap();
                        a.xor(edx, eax).unwrap();
                    }
                } else {
                    for _ in 0..16 {
                        a.movdqu(xmm0, xmmword_ptr(DATA)).unwrap();
                        a.movdqu(xmmword_ptr(DATA + 16), xmm0).unwrap();
                    }
                }
                a.dec(ecx).unwrap();
                a.jnz(begin).unwrap();
                a.ret().unwrap();
            });
            // A/B/A uses one executable and identical compiler/memory features.
            for limit in [0, 128, 0, 64, 256, 512, 0] {
                let mut cold = Vec::new();
                let mut compilation = Vec::new();
                let mut hot = Vec::new();
                let mut sizes = (0, 0);
                let mut linked = 0.0;
                for _ in 0..5 {
                    let (mut m, _service) = fixture(limit);
                    m.write(DATA, &[7, 0, 0, 0, 3, 0, 0, 0, 0, 0, 0, 0, 1, 2, 3, 4])
                        .unwrap();
                    let original = region(&mut m, &bytes);
                    let begun = Instant::now();
                    m.queue_region(original).unwrap();
                    drain(&mut m);
                    cold.push(begun.elapsed().as_secs_f64() * 1000.0);
                    compilation.push(m.stats.compile_time.as_secs_f64() * 1000.0);
                    sizes = (m.code.compiled.len(), m.stats.native_code_bytes);
                    let before_linked = m.stats.linked_entries;
                    for _ in 0..7 {
                        seed(&mut m, 20_000);
                        let begun = Instant::now();
                        assert_eq!(m.run(STOP, 20_000_000).unwrap(), RunOutcome::ReachedStop);
                        hot.push(begun.elapsed().as_secs_f64() * 1e9 / 20_000.0);
                        assert_eq!(m.state.regs[1], 0);
                        assert_eq!(m.state.regs[4], STACK + 4);
                        if case == "scalar" {
                            assert_eq!(m.state.regs[3], 20_000 * 63 * 3);
                        } else {
                            assert_eq!(
                                m.memory.read_data(DATA + 16, 16).unwrap(),
                                m.memory.read_data(DATA, 16).unwrap()
                            );
                        }
                    }
                    linked = (m.stats.linked_entries - before_linked) as f64 / 140_000.0;
                    assert!(m.stats.interpreter_instructions == 0);
                    if limit != 0 {
                        assert!(m.stats.max_compile_work <= limit);
                    }
                }
                cold.sort_by(f64::total_cmp);
                compilation.sort_by(f64::total_cmp);
                hot.sort_by(f64::total_cmp);
                println!(
                    "{case},{limit},{:.3},{:.3},{:.3},{},{},{linked:.3}",
                    cold[2], compilation[2], hot[17], sizes.0, sizes.1
                );
            }
        }
    }
}

fn descriptor_dpl(base: u32, code: bool, dpl: u64) -> u64 {
    let base = u64::from(base);
    0xffff
        | ((base & 0xffff) << 16)
        | (((base >> 16) & 0xff) << 32)
        | (((if code { 0x9b } else { 0x93 }) | (dpl << 5)) << 40)
        | (0xcf << 48)
        | ((base >> 24) << 56)
}

#[cfg(test)]
mod asynchronous_tests {
    use super::*;
    use crate::{EXEC, READ, WRITE};

    fn fixture() -> Executor {
        let mut m = Executor::new(0x1000, 0x4000).unwrap();
        m.protect(0x1000, 0x4000, READ | WRITE).unwrap();
        m.write(0x1000, &[0xb8, 1, 0, 0, 0, 0xc3]).unwrap();
        m.protect(0x1000, 4096, READ | EXEC).unwrap();
        m.write(0x4f00, &0x3000u32.to_le_bytes()).unwrap();
        m.state.eip = 0x1000;
        m.state.regs[4] = 0x4f00;
        m.set_async_compilation(true);
        m
    }
    #[test]
    fn lazy_worker_inherits_wake_and_pending_jobs_reject_replacement() {
        let mut m = fixture();
        let wake = adv_sync::WakeEvent::new().unwrap();
        let signal = wake.clone();
        m.set_completion_waker(Arc::new(move || signal.signal()))
            .unwrap();
        assert!(m.compiler.is_none());
        assert!(m.queue_region(Region::new(0x1000, 6)).unwrap());
        assert!(
            m.set_completion_waker(Arc::new(|| {}))
                .unwrap_err()
                .contains("pending")
        );
        assert!(wake.wait(Some(Duration::from_secs(5))).unwrap());
        m.collect_compilations().unwrap();
        assert_eq!(m.stats.compilations, 1);
        assert_eq!(m.stats.pending_compilations, 0);
        m.set_completion_waker(Arc::new(|| {})).unwrap();
    }
    fn collect_one(m: &mut Executor) {
        let before = m.stats.compilations;
        let deadline = Instant::now() + Duration::from_secs(10);
        while m.stats.compilations == before {
            m.collect_compilations().unwrap();
            assert!(
                Instant::now() < deadline,
                "background compilation did not complete"
            );
            std::thread::yield_now();
        }
    }
    #[test]
    fn modification_during_background_compilation_discards_the_result() {
        let mut m = fixture();
        assert!(m.queue_region(Region::new(0x1000, 6)).unwrap());
        m.write(0x1001, &42u32.to_le_bytes()).unwrap();
        collect_one(&mut m);
        assert_eq!(m.stats.stale_compilations, 1);
        assert!(m.code.compiled.is_empty());
        assert!(m.code.entry_owner.is_empty());
        assert_eq!(m.stats.pending_compilations, 0);
        assert!(m.queue_region(Region::new(0x1000, 6)).unwrap());
        collect_one(&mut m);
        assert_eq!(m.run(0x3000, 100).unwrap(), RunOutcome::ReachedStop);
        assert_eq!(m.state.regs[0], 42);
        assert_eq!(m.stats.native_entries, 1);
    }
    #[test]
    fn explicit_replacement_cannot_be_overwritten_by_an_old_ticket() {
        let mut m = fixture();
        assert!(m.queue_region(Region::new(0x1000, 6)).unwrap());
        m.write(0x1001, &77u32.to_le_bytes()).unwrap();
        m.add_region(Region::new(0x1000, 6)).unwrap();
        collect_one(&mut m);
        assert_eq!(m.stats.stale_compilations, 1);
        assert_eq!(m.run(0x3000, 100).unwrap(), RunOutcome::ReachedStop);
        assert_eq!(m.state.regs[0], 77);
        assert_eq!(m.stats.native_entries, 1);
    }
    #[test]
    fn hot_code_keeps_executing_while_the_worker_compiles_it() {
        let mut m = fixture();
        m.write(0x1000, &[0x40, 0xeb, 0xfd]).unwrap(); // inc eax; jmp back
        m.enable_auto_jit(true);
        let deadline = Instant::now() + Duration::from_secs(10);
        while m.stats.native_entries == 0 {
            assert_eq!(m.run(0x3000, 1000).unwrap(), RunOutcome::Budget);
            assert!(Instant::now() < deadline);
        }
        assert!(m.state.regs[0] > 32);
        assert!(m.stats.interpreter_instructions > 0);
        assert!(m.stats.compile_requests > 0);
        assert!(m.stats.peak_pending_compilations <= crate::compiler::CAPACITY);
    }
    #[test]
    fn dedicated_compile_threads_publish_automatic_regions() {
        let mut m = fixture();
        m.write(0x1000, &[0x40, 0xeb, 0xfd]).unwrap(); // inc eax; jmp back
        m.enable_auto_jit(true);
        m.set_compile_threads(2).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while m.stats.native_entries == 0 {
            m.run(0x3000, 1000).unwrap();
            assert!(Instant::now() < deadline);
        }
        assert!(m.compiler.is_some());
        assert!(m.set_compile_threads(1).is_err());
    }
    #[test]
    fn warm_profile_queues_unchanged_code_before_it_runs() {
        let mut first = fixture();
        first.write(0x1000, &[0x40, 0xeb, 0xfd]).unwrap(); // inc eax; jmp back
        first.enable_auto_jit(true);
        let deadline = Instant::now() + Duration::from_secs(10);
        while first.stats.native_entries == 0 {
            first.run(0x3000, 1000).unwrap();
            assert!(Instant::now() < deadline);
        }
        let profile = first.warm_profile();

        let mut second = fixture();
        second.write(0x1000, &[0x40, 0xeb, 0xfd]).unwrap();
        second.enable_auto_jit(true);
        assert_eq!(second.load_warm_profile(&profile), 1);
        assert_eq!(second.load_warm_profile(&profile[..profile.len() - 1]), 0);
        assert_eq!(second.load_warm_profile(&profile), 1);
        second.run(0x3000, 1).unwrap();
        assert_eq!(second.stats.warm_requests, 1);
        while second.stats.native_entries == 0 {
            second.run(0x3000, 1000).unwrap();
            assert!(Instant::now() < deadline);
        }

        // Different code at the same address is left to normal discovery.
        let mut third = fixture();
        third.write(0x1000, &[0x48, 0xeb, 0xfd]).unwrap();
        third.enable_auto_jit(true);
        third.load_warm_profile(&profile);
        third.run(0x3000, 1).unwrap();
        assert_eq!(third.stats.warm_requests, 0);
        assert!(third.warm.is_none());
    }
    #[test]
    fn automatic_discovery_compiles_up_to_a_stop_boundary() {
        let mut m = fixture();
        m.write(0x1000, &[0x40, 0x83, 0xf8, 100, 0x75, 0xfa])
            .unwrap();
        m.enable_auto_jit(true);
        m.set_async_compilation(false);
        assert_eq!(m.run(0x1006, 1000).unwrap(), RunOutcome::ReachedStop);
        assert_eq!(m.state.regs[0], 100);
        assert_eq!(m.stats.compilations, 1);
        assert!(m.stats.native_entries > 0);
    }

    #[test]
    fn lazy_background_handle_retains_the_installed_execution_wake_without_jit() {
        let mut m = fixture();
        m.enable_auto_jit(false);
        let wake = adv_sync::WakeEvent::new().unwrap();
        m.set_wake_event(wake.clone()).unwrap();
        let sender = m.background_sender().unwrap();
        assert!(Arc::ptr_eq(
            &wake.interrupt_flag(),
            &sender.borrow().execution_wake().interrupt_flag()
        ));
        assert!(m.compiler.is_none());
        sender
            .borrow_mut()
            .try_submit(
                adv_sync::background::Priority::Urgent,
                adv_sync::background::Task::new(|| {}),
            )
            .unwrap();
        assert!(wake.wait(Some(Duration::from_secs(5))).unwrap());
        assert!(wake.take_interrupt());
        assert!(
            m.set_wake_event(adv_sync::WakeEvent::new().unwrap())
                .unwrap_err()
                .contains("cannot change")
        );
    }

    #[test]
    fn startup_background_injection_rejects_a_different_execution_event() {
        let mut m = fixture();
        let wake = adv_sync::WakeEvent::new().unwrap();
        m.set_wake_event(wake.clone()).unwrap();
        let mut wrong =
            adv_sync::background::Builder::new(adv_sync::WakeEvent::new().unwrap()).unwrap();
        let sender = wrong.sender(2, 8).unwrap().into_local();
        let _wrong_service = wrong.start().unwrap();
        assert!(
            m.set_background_sender(sender)
                .unwrap_err()
                .contains("owner's wake")
        );
        let mut right = adv_sync::background::Builder::new(wake).unwrap();
        let sender = right.sender(2, 8).unwrap().into_local();
        let _service = right.start().unwrap();
        m.set_background_sender(sender.clone()).unwrap();
        assert!(
            m.set_background_sender(sender)
                .unwrap_err()
                .contains("before first use")
        );
        assert!(
            m.background_service.is_none(),
            "injection must not spawn a second worker"
        );
    }
}

#[cfg(test)]
mod topology_tests {
    use super::*;
    use iced_x86::code_asm::*;

    fn machine() -> Executor {
        let mut m = Executor::create(Memory::reserve(0x1000, 0x9000).unwrap()).unwrap();
        m.map(0x1000, 0x9000, crate::READ | crate::WRITE | crate::EXEC)
            .unwrap();
        m.protect(0x9000, 4096, crate::READ | crate::WRITE).unwrap();
        m.enable_auto_jit(true);
        m.set_async_compilation(false);
        m.state.regs[4] = 0x9f00;
        m.write(0x9f00, &0x8000u32.to_le_bytes()).unwrap();
        m
    }

    fn write(m: &mut Executor, pc: u32, emit: impl FnOnce(&mut CodeAssembler)) {
        let mut a = CodeAssembler::new(32).unwrap();
        emit(&mut a);
        m.write(pc, &a.assemble(u64::from(pc)).unwrap()).unwrap();
    }

    fn discover(m: &mut Executor, pc: u32) {
        m.state.eip = pc;
        m.hits.insert(pc, 31);
        m.discover(0x8000).unwrap();
    }

    #[test]
    fn sparse_loop_merges_control_flow_and_ignores_padding_pages() {
        let mut m = machine();
        write(&mut m, 0x1000, |a| {
            a.inc(eax).unwrap();
            a.jmp(0x4000u64).unwrap();
        });
        write(&mut m, 0x4000, |a| {
            a.dec(ecx).unwrap();
            a.jnz(0x1000u64).unwrap();
            a.ret().unwrap();
        });
        discover(&mut m, 0x1000);
        assert_eq!(m.code.compiled.len(), 1);
        assert!(m.code.entry_owner.contains_key(&0x4000));
        assert_eq!(
            m.code.region_pages[&0x1000],
            BTreeSet::from([0x1000, 0x4000])
        );
        m.refresh().unwrap();
        let checks = m.stats.invalidation_candidates;
        m.write(0x2000, &[42]).unwrap();
        m.unmap(0x3000, 4096).unwrap();
        m.refresh().unwrap();
        assert_eq!(m.stats.invalidation_candidates, checks);
        m.state.regs[1] = 100;
        assert_eq!(m.run(0x8000, 1000).unwrap(), RunOutcome::ReachedStop);
        assert_eq!(m.state.regs[0], 100);
        assert_eq!(m.stats.compilations, 1);
        assert_eq!(m.stats.interpreter_instructions, 0);
    }

    #[test]
    fn overlapping_new_entry_preserves_old_native_code_and_invalidates_both() {
        let mut m = machine();
        // The immediate is also valid overlapping x86: INC EAX; RET.
        m.write(0x1000, &[0xb8, 0x40, 0xc3, 0, 0, 0xc3]).unwrap();
        discover(&mut m, 0x1000);
        let entry = m.code.compiled[&0x1000].tail_entry;
        let size = m.code.compiled[&0x1000].code_size;
        discover(&mut m, 0x1001);
        assert_eq!(m.code.compiled.len(), 2);
        assert_eq!(m.code.compiled[&0x1000].tail_entry, entry);
        assert_eq!(m.code.compiled[&0x1000].code_size, size);
        assert_eq!(m.code.entry_owner[&0x1000], 0x1000);
        assert_eq!(m.code.entry_owner[&0x1001], 0x1001);
        m.state.regs[0] = 7;
        assert_eq!(m.run(0x8000, 10).unwrap(), RunOutcome::ReachedStop);
        assert_eq!(m.state.regs[0], 8);
        m.write(0x1001, &[0x48]).unwrap();
        m.refresh().unwrap();
        assert_eq!(m.stats.invalidations, 2);
        assert_eq!(m.stats.compilations, 4);
        assert_eq!(
            m.stats.native_code_bytes,
            m.code.compiled.values().map(|c| c.code_size).sum::<usize>()
        );
    }

    #[test]
    fn new_fragment_links_to_existing_entry_without_recompiling_it() {
        let mut m = machine();
        write(&mut m, 0x1300, |a| {
            a.add(eax, 3).unwrap();
            a.ret().unwrap();
        });
        discover(&mut m, 0x1300);
        let old_entry = m.code.compiled[&0x1300].tail_entry;
        write(&mut m, 0x1100, |a| {
            a.inc(eax).unwrap();
            a.jmp(0x1300u64).unwrap();
        });
        discover(&mut m, 0x1100);
        assert_eq!(m.code.regions[&0x1100].frontier, [0x1300]);
        assert_eq!(m.code.compiled[&0x1300].tail_entry, old_entry);
        assert_eq!(m.run(0x8000, 10).unwrap(), RunOutcome::ReachedStop);
        assert_eq!(m.state.regs[0], 4);
        assert_eq!(m.stats.linked_entries, 1);
        assert_eq!(m.stats.interpreter_instructions, 0);
    }

    #[test]
    fn direct_callees_inside_the_span_remain_separate_units() {
        let mut m = machine();
        write(&mut m, 0x1000, |a| {
            a.call(0x1100u64).unwrap();
            a.jmp(0x1300u64).unwrap();
        });
        write(&mut m, 0x1100, |a| {
            a.add(eax, 2).unwrap();
            a.ret().unwrap();
        });
        write(&mut m, 0x1300, |a| {
            a.add(eax, 3).unwrap();
            a.ret().unwrap();
        });
        discover(&mut m, 0x1000);
        assert!(!m.code.entry_owner.contains_key(&0x1100));
        assert!(m.code.regions[&0x1000].frontier.contains(&0x1100));
        discover(&mut m, 0x1100);
        m.state.eip = 0x1000;
        assert_eq!(m.run(0x8000, 10).unwrap(), RunOutcome::ReachedStop);
        assert_eq!(m.state.regs[0], 5);
        assert_eq!(m.code.compiled.len(), 2);
        assert_eq!(m.stats.interpreter_instructions, 0);
    }

    #[test]
    fn span_and_instruction_limits_leave_executable_frontiers() {
        let mut m = machine();
        write(&mut m, 0x1000, |a| {
            a.inc(eax).unwrap();
            a.jmp(0x6000u64).unwrap();
        });
        write(&mut m, 0x6000, |a| {
            a.inc(eax).unwrap();
            a.ret().unwrap();
        });
        discover(&mut m, 0x1000);
        assert!(m.code.regions[&0x1000].len < 512);
        assert!(m.code.regions[&0x1000].frontier.contains(&0x6000));
        discover(&mut m, 0x6000);
        m.state.eip = 0x1000;
        assert_eq!(m.run(0x8000, 100).unwrap(), RunOutcome::ReachedStop);
        assert_eq!(m.state.regs[0], 2);
        m.write(0x2000, &[0x40; 300]).unwrap();
        m.write(0x212c, &[0xc3]).unwrap();
        discover(&mut m, 0x2000);
        assert_eq!(m.code.regions[&0x2000].len, 256);
        assert!(m.code.regions[&0x2000].frontier.contains(&0x2100));
        discover(&mut m, 0x2100);
        m.state.eip = 0x2000;
        m.state.regs[4] = 0x9f00;
        assert_eq!(m.run(0x8000, 100).unwrap(), RunOutcome::ReachedStop);
        assert_eq!(m.state.regs[0], 302);
    }

    #[test]
    fn incomplete_fetch_keeps_a_negative_dependency_on_the_next_page() {
        let mut m = machine();
        m.write(0x1ffe, &[0xb8, 7, 0, 0, 0, 0xc3]).unwrap();
        m.protect(0x2000, 4096, crate::READ).unwrap();
        discover(&mut m, 0x1ffe);
        assert!(m.code.region_pages[&0x1ffe].contains(&0x2000));
        assert_eq!(
            m.run(0x8000, 10).unwrap(),
            RunOutcome::FetchFault {
                pc: 0x1ffe,
                address: 0x2000
            }
        );
        m.protect(0x2000, 4096, crate::READ | crate::EXEC).unwrap();
        m.state.eip = 0x1ffe;
        assert_eq!(m.run(0x8000, 100).unwrap(), RunOutcome::ReachedStop);
        assert_eq!(m.state.regs[0], 7);
        assert!(m.stats.invalidations > 0);
    }

    #[test]
    fn cache_count_limit_preserves_recent_code_and_clears_published_targets() {
        let mut m = machine();
        m.set_code_cache_limits(2, 1 << 20);
        for pc in [0x1000, 0x1200, 0x1400] {
            m.write(pc, &[0x40, 0xc3]).unwrap();
        }
        discover(&mut m, 0x1000);
        discover(&mut m, 0x1200);
        m.state.eip = 0x1000;
        assert_eq!(m.run(0x8000, 10).unwrap(), RunOutcome::ReachedStop);
        let recent = m.code.compiled[&0x1000].tail_entry;
        let revision = m.code.cache_revision;
        discover(&mut m, 0x1400);
        assert_eq!(m.code.compiled.len(), 2);
        assert_eq!(m.stats.native_regions, 2);
        assert_eq!(m.stats.cache_evictions, 1);
        assert_eq!(m.code.compiled[&0x1000].tail_entry, recent);
        assert!(!m.code.entry_owner.contains_key(&0x1200));
        assert!(!m.code.region_pages.contains_key(&0x1200));
        assert!(!m.code.regions.contains_key(&0x1200));
        assert!(m.code.cache_revision > revision);
        assert_ne!(m.code.links_revision, m.code.cache_revision);
        m.set_code_cache_limits(0, 0);
        assert!(m.code.compiled.is_empty());
        assert!(m.code.entry_owner.is_empty());
        assert!(m.code.page_regions.is_empty());
        assert_eq!(m.stats.native_code_bytes, 0);
        m.state.eip = 0x1200;
        m.state.regs[4] = 0x9f00;
        assert_eq!(m.run(0x8000, 10).unwrap(), RunOutcome::ReachedStop);
    }

    #[test]
    fn byte_budget_and_versioned_rejections_keep_residency_bounded() {
        let mut m = machine();
        for pc in [0x1000, 0x1200] {
            m.write(pc, &[0x40, 0xc3]).unwrap();
        }
        // Identical guest bytes need not compile to identical sizes at every
        // address on every host, or even twice at one address (macOS ARM64
        // varied by a few bytes), so the budget fits either region alone with
        // slack but never both.
        let mut probe = machine();
        for pc in [0x1000, 0x1200] {
            probe.write(pc, &[0x40, 0xc3]).unwrap();
            discover(&mut probe, pc);
        }
        let sizes = [0x1000, 0x1200].map(|pc| probe.code.compiled[&pc].code_size);
        eprintln!("region code sizes: {sizes:?}");
        let size = sizes[0].max(sizes[1]) + sizes[0].min(sizes[1]) / 2;
        discover(&mut m, 0x1000);
        m.set_code_cache_limits(8, size);
        discover(&mut m, 0x1200);
        assert!(m.stats.native_code_bytes <= size);
        assert_eq!(m.stats.native_regions, 1);
        assert_eq!(m.stats.cache_evictions, 1);
        m.set_code_cache_limits(8, 1);
        discover(&mut m, 0x1000);
        assert_eq!(m.stats.cache_rejections, 1);
        assert_eq!(m.stats.native_regions, 0);
        let compilations = m.stats.compilations;
        for _ in 0..64 {
            discover(&mut m, 0x1000);
        }
        assert_eq!(m.stats.compilations, compilations);
        assert!(m.rejected_entries.contains_key(&0x1000));
        m.write(0x1000, &[0x48]).unwrap();
        discover(&mut m, 0x1000);
        assert_eq!(m.stats.compilations, compilations + 1);
        m.set_code_cache_limits(8, 1 << 20);
        discover(&mut m, 0x1000);
        assert!(m.code.entry_owner.contains_key(&0x1000));
        assert_eq!(m.stats.cache_rejections, 2);
    }

    #[test]
    fn late_worker_publication_obeys_the_latest_cache_budget() {
        let mut m = machine();
        m.write(0x1000, &[0x40, 0xc3]).unwrap();
        m.set_async_compilation(true);
        m.hits.insert(0x1000, 31);
        m.state.eip = 0x1000;
        m.discover(0x8000).unwrap();
        assert_eq!(m.stats.pending_compilations, 1);
        m.set_code_cache_limits(0, 0);
        let until = Instant::now() + Duration::from_secs(10);
        while m.stats.pending_compilations != 0 {
            m.collect_compilations().unwrap();
            assert!(Instant::now() < until);
            std::thread::yield_now();
        }
        assert_eq!(m.stats.cache_rejections, 1);
        assert_eq!(m.stats.native_regions, 0);
        assert!(m.code.regions.is_empty());
        assert!(m.code.page_regions.is_empty());
        let requests = m.stats.compile_requests;
        for _ in 0..64 {
            m.discover(0x8000).unwrap();
        }
        assert_eq!(m.stats.compile_requests, requests);
    }

    #[test]
    fn padding_on_a_used_page_revalidates_only_decoded_instruction_bytes() {
        let mut m = machine();
        write(&mut m, 0x1000, |a| {
            a.inc(eax).unwrap();
            a.jmp(0x1300u64).unwrap();
        });
        m.write(0x1300, &[0xc3]).unwrap();
        discover(&mut m, 0x1000);
        m.refresh().unwrap();
        let entry = m.code.compiled[&0x1000].tail_entry;
        m.write(0x1200, &[0xcc]).unwrap();
        m.refresh().unwrap();
        assert_eq!(m.stats.code_revalidations, 1);
        assert_eq!(m.stats.invalidations, 0);
        assert_eq!(m.code.compiled[&0x1000].tail_entry, entry);
    }
}

#[cfg(test)]
mod permission_tests {
    use super::*;
    use crate::{EXEC, READ, WRITE};

    fn machine(code: &[u8]) -> Executor {
        let mut m = Executor::new(0x1000, 0x4000).unwrap();
        m.protect(0x1000, 0x4000, READ | WRITE).unwrap();
        m.write(0x1000, code).unwrap();
        m.protect(0x1000, 4096, READ | EXEC).unwrap();
        m.add_region(Region::new(0x1000, code.len())).unwrap();
        m.state.eip = 0x1000;
        m
    }

    #[test]
    fn protection_by_a_trap_reaches_hoisted_constant_address_checks() {
        struct Revoke(u32);
        impl crate::ExecutionTrap for Revoke {
            fn call(
                &mut self,
                _: &mut CpuState,
                memory: &mut Memory,
                _: u32,
            ) -> Result<bool, String> {
                self.0 += 1;
                if self.0 == 2 {
                    memory.protect(0x2000, 4096, 0)?;
                }
                Ok(false)
            }
        }
        // top: mov ebx,[0x2000]; int 0x2e; dec ecx; jnz top; ret
        let mut m = machine(&[
            0x8b, 0x1d, 0, 0x20, 0, 0, 0xcd, 0x2e, 0x49, 0x75, 0xf5, 0xc3,
        ]);
        m.state.regs[1] = 5;
        let mut handler = Revoke(0);
        assert_eq!(
            m.run_with_traps(0x3000, 1000, &mut handler, false).unwrap(),
            RunOutcome::ReadFault {
                pc: 0x1000,
                address: 0x2000
            }
        );
        assert_eq!(handler.0, 2);
        assert_eq!(m.state.regs[1], 3);
        assert!(m.stats.native_entries > 0);
    }

    #[test]
    fn constant_addresses_check_both_pages_only_when_crossing() {
        // mov eax,[0x2ffe]; mov [0x2ff8],eax; ret
        let code = [0xa1, 0xfe, 0x2f, 0, 0, 0xa3, 0xf8, 0x2f, 0, 0, 0xc3];
        let mut m = machine(&code);
        m.protect(0x3000, 4096, 0).unwrap();
        assert_eq!(
            m.run(0x4000, 100).unwrap(),
            RunOutcome::ReadFault {
                pc: 0x1000,
                address: 0x2ffe
            }
        );
        let mut m = machine(&code);
        m.protect(0x2000, 4096, READ).unwrap();
        m.write(0x2ffe, &[1, 2, 3, 4]).unwrap();
        assert_eq!(
            m.run(0x4000, 100).unwrap(),
            RunOutcome::WriteFault {
                pc: 0x1005,
                address: 0x2ff8
            }
        );
        assert_eq!(m.state.regs[0], 0x0403_0201);
        assert!(m.stats.native_entries > 0);
    }
}

#[cfg(test)]
mod floating_boundary_tests {
    use super::*;
    use crate::{EXEC, READ, WRITE};

    #[test]
    fn raw_host_exception_exits_native_before_waiting_mmx() {
        struct Handler(Vec<(u32, u32)>);
        impl crate::ExecutionTrap for Handler {
            fn call(
                &mut self,
                state: &mut CpuState,
                _: &mut Memory,
                vector: u32,
            ) -> Result<bool, String> {
                self.0.push((vector, state.eip));
                if vector == 0x2e {
                    state.x87.control &= !1;
                    state.x87.status = 0x81;
                    Ok(false)
                } else {
                    assert_eq!(vector, 16);
                    assert_eq!(state.regs[0], 1);
                    Ok(true)
                }
            }
        }
        let mut m = Executor::new(0x1000, 0x4000).unwrap();
        m.protect(0x1000, 0x4000, READ | WRITE).unwrap();
        m.write(0x1000, &[0xcd, 0x2e, 0x40, 0x0f, 0xef, 0xc0, 0xc3])
            .unwrap();
        m.protect(0x1000, 4096, READ | EXEC).unwrap();
        m.state.eip = 0x1000;
        m.add_region(Region::new(0x1000, 7)).unwrap();
        let registers = m.state.x87.regs;
        let mut handler = Handler(Vec::new());
        assert_eq!(
            m.run_with_traps(0x3000, 100, &mut handler, false).unwrap(),
            RunOutcome::Parked
        );
        assert_eq!(handler.0, [(0x2e, 0x1002), (16, 0x1003)]);
        assert_eq!(m.state.x87.regs, registers);
        assert_eq!(m.stats.native_entries, 1);
    }

    #[test]
    fn batched_fault_prefix_charges_the_same_fuel_as_single_instruction_execution() {
        struct Repair(usize);
        impl crate::ExecutionTrap for Repair {
            fn call(
                &mut self,
                state: &mut CpuState,
                _: &mut Memory,
                vector: u32,
            ) -> Result<bool, String> {
                assert_eq!(vector, 0);
                assert_eq!(state.eip, 0x1001);
                assert_eq!(state.regs[3], 1);
                state.regs[1] = 3;
                self.0 += 1;
                Ok(false)
            }
        }
        let make = |profile| {
            let mut m = Executor::new(0x1000, 0x4000).unwrap();
            m.protect(0x1000, 0x4000, READ | WRITE).unwrap();
            // INC EBX; IDIV ECX; RET. The handler repairs ECX without moving EIP.
            m.write(0x1000, &[0x43, 0xf7, 0xf9, 0xc3]).unwrap();
            m.protect(0x1000, 4096, READ | EXEC).unwrap();
            m.write(0x4f00, &0x3000u32.to_le_bytes()).unwrap();
            m.state.eip = 0x1000;
            m.state.regs[0] = 9;
            m.state.regs[4] = 0x4f00;
            m.set_profiling(profile);
            m
        };
        let mut batched = make(false);
        let mut single = make(true);
        let mut a = Repair(0);
        let mut b = Repair(0);
        for m in [&mut batched, &mut single] {
            let handler = if m.profiling { &mut b } else { &mut a };
            assert_eq!(
                m.run_with_traps(0x3000, 2, handler, true).unwrap(),
                RunOutcome::Budget
            );
            assert_eq!(m.state.eip, 0x1001);
            assert_eq!(m.state.fuel, 0);
        }
        assert_eq!(a.0, 1);
        assert_eq!(b.0, 1);
        assert_eq!(batched.state, single.state);
        for m in [&mut batched, &mut single] {
            let handler = if m.profiling { &mut b } else { &mut a };
            assert_eq!(
                m.run_with_traps(0x3000, 2, handler, true).unwrap(),
                RunOutcome::ReachedStop
            );
            assert_eq!(m.state.regs[0], 3);
            assert_eq!(m.state.regs[3], 1);
        }
        assert_eq!(batched.state, single.state);
        assert_eq!(a.0, 1);
        assert_eq!(b.0, 1);
    }
}

#[cfg(test)]
mod tiering_tests {
    use super::*;
    fn hot_machine() -> Executor {
        let mut m = Executor::new(0x1000, 0x4000).unwrap();
        m.protect(0x1000, 0x4000, crate::READ | crate::WRITE)
            .unwrap();
        m.write(0x1000, &[0x40, 0xc3]).unwrap();
        m.protect(0x1000, 4096, crate::READ | crate::EXEC).unwrap();
        m.write(0x4f00, &0x3000u32.to_le_bytes()).unwrap();
        let mut region = Region::new(0x1000, 2);
        region.compile_strategy = CompileStrategy::Fast;
        m.add_region(region).unwrap();
        m.automatic_regions.insert(0x1000);
        m.enable_auto_jit(true);
        m.set_async_compilation(true);
        m.state.eip = 0x1000;
        m.state.regs[4] = 0x4f00;
        m
    }
    fn collect(m: &mut Executor) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !m.pending_regions.is_empty() {
            m.collect_compilations().unwrap();
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
    }
    #[test]
    fn deferred_work_is_ordered_by_interpreter_demand_then_recency() {
        let mut m = Executor::new(0x1000, 0x4000).unwrap();
        m.protect(0x1000, 0x4000, crate::READ | crate::WRITE)
            .unwrap();
        for start in [0x1000, 0x1100, 0x1200] {
            m.write(start, &[0x40, 0xc3]).unwrap();
        }
        m.protect(0x1000, 4096, crate::READ | crate::EXEC).unwrap();
        for (sequence, start) in [0x1000u32, 0x1100, 0x1200].into_iter().enumerate() {
            let region = Region::new(start, 2);
            let snapshot = compile::Snapshot::capture(&region, &mut m.memory).unwrap();
            m.deferred_compilations.insert(start, snapshot);
            m.deferred_order.insert(start, sequence as u64);
        }
        assert_eq!(m.next_deferred(), Some(0x1200));
        m.demand.insert(0x1000, 3);
        assert_eq!(m.next_deferred(), Some(0x1000));
        m.deferred_compilations.remove(&0x1000);
        assert_eq!(m.next_deferred(), Some(0x1200));
        assert!(!m.deferred_order.contains_key(&0x1000));
    }
    #[test]
    fn speed_compilation_keeps_fast_code_live_and_publishes_once() {
        let mut m = hot_machine();
        let entry = m.code.compiled[&0x1000].tail_entry;
        m.record_native_work(0x1000, 16_383);
        assert_eq!(m.stats.tier_up_requests, 0);
        m.record_native_work(0x1000, 1);
        assert_eq!(m.stats.tier_up_requests, 1);
        assert_eq!(m.code.compiled[&0x1000].tail_entry, entry);
        assert_eq!(
            m.code.regions[&0x1000].compile_strategy,
            CompileStrategy::Fast
        );
        m.record_native_work(0x1000, 100_000);
        assert_eq!(m.stats.tier_up_requests, 1);
        collect(&mut m);
        assert_eq!(
            m.code.regions[&0x1000].compile_strategy,
            CompileStrategy::Speed
        );
        assert_eq!(m.stats.tier_up_publications, 1);
        assert!(!m.code.fast_regions.contains(&0x1000));
        m.record_native_work(0x1000, 100_000);
        assert!(!m.native_heat.contains_key(&0x1000));
        assert_eq!(m.stats.tier_up_requests, 1);
        assert_eq!(m.run(0x3000, 100).unwrap(), RunOutcome::ReachedStop);
        assert_eq!(m.state.regs[0], 1);
    }
    #[test]
    fn stale_optimized_code_is_never_published() {
        let mut m = hot_machine();
        m.record_native_work(0x1000, 16_384);
        m.write(0x1000, &[0x43, 0xc3]).unwrap();
        collect(&mut m);
        assert_eq!(m.stats.tier_up_publications, 0);
        assert_eq!(m.stats.stale_compilations, 1);
        assert_eq!(m.run(0x3000, 100).unwrap(), RunOutcome::ReachedStop);
        assert_eq!(m.state.regs[0], 0);
        assert_eq!(m.state.regs[3], 1);
    }
    #[test]
    fn tiering_can_be_disabled_and_never_rewrites_explicit_regions() {
        let mut m = hot_machine();
        m.set_tiered_compilation(false);
        m.record_native_work(0x1000, 100_000);
        assert_eq!(m.stats.tier_up_requests, 0);
        m.set_tiered_compilation(true);
        m.automatic_regions.clear();
        m.record_native_work(0x1000, 100_000);
        assert_eq!(m.stats.tier_up_requests, 0);
    }

    #[test]
    fn profiled_units_tier_up_with_their_observed_indirect_callees() {
        let mut m = Executor::new(0x1000, 0x4000).unwrap();
        m.protect(0x1000, 0x4000, crate::READ | crate::WRITE)
            .unwrap();
        // inc ebx; ret
        m.write(0x1000, &[0x43, 0xc3]).unwrap();
        // top: call esi; dec ecx; jnz top; ret
        m.write(0x1100, &[0xff, 0xd6, 0x49, 0x75, 0xfb, 0xc3])
            .unwrap();
        m.write(0x4f00, &0x3000u32.to_le_bytes()).unwrap();
        m.protect(0x1000, 4096, crate::READ | crate::EXEC).unwrap();
        m.enable_auto_jit(true);
        m.set_auto_compile_strategy(CompileStrategy::Fast);
        m.set_async_compilation(true);
        m.state.eip = 0x1100;
        m.state.regs[1] = 100_000;
        m.state.regs[4] = 0x4f00;
        m.state.regs[6] = 0x1000;
        // Observe native heat only after Fast is published. A busy compiler
        // must not let the finite fixture finish entirely in the interpreter.
        m.hits.insert(0x1100, 31);
        m.discover(0x3000).unwrap();
        m.set_tiered_compilation(false);
        collect(&mut m);
        let deadline = Instant::now() + Duration::from_secs(10);
        while !m
            .code
            .compiled
            .get(&0x1100)
            .and_then(Compiled::profile)
            .is_some_and(|profile| profile.heat() >= HOT_BLOCKS)
        {
            assert_eq!(m.run(0x3000, 1000).unwrap(), RunOutcome::Budget);
            assert!(Instant::now() < deadline);
        }
        // Keep the observed Fast profile alive until its heat is checked.
        m.set_tiered_compilation(true);
        m.scan_profiles();
        collect(&mut m);
        let region = &m.code.regions[&0x1100];
        assert_eq!(region.callees, [0x1000]);
        assert_eq!(region.compile_strategy, CompileStrategy::Speed);
        // Finish post-publication oracle samples before measuring steady-state
        // entries: verification intentionally returns after each native block.
        assert_eq!(m.run(0x3000, 1000).unwrap(), RunOutcome::Budget);
        // The call and its return no longer leave the unit.
        let entries = m.stats.native_entries;
        assert_eq!(m.run(0x3000, 3000).unwrap(), RunOutcome::Budget);
        assert!(m.stats.native_entries - entries <= 2);
        assert_eq!(m.run(0x3000, u64::MAX).unwrap(), RunOutcome::ReachedStop);
        assert_eq!(m.state.regs[3], 100_000);
    }

    fn direct_leaf_machine(leaf: &[u8], tiering: bool) -> Executor {
        let mut m = Executor::new(0x1000, 0x4000).unwrap();
        m.protect(0x1000, 0x4000, crate::READ | crate::WRITE)
            .unwrap();
        // CALL 1180; ADD [4000],EAX; DEC ECX; JNZ 1100; JMP 1200.
        // The final direct jump makes 1180 lie inside the discovered byte span,
        // while CALL discovery still leaves it on the initial frontier.
        m.write(
            0x1100,
            &[
                0xe8, 0x7b, 0, 0, 0, 0x01, 0x05, 0, 0x40, 0, 0, 0x49, 0x75, 0xf2, 0xe9, 0xed, 0, 0,
                0,
            ],
        )
        .unwrap();
        m.write(0x1180, leaf).unwrap();
        m.write(0x1200, &[0xc3]).unwrap();
        m.protect(0x1000, 4096, crate::READ | crate::EXEC).unwrap();
        m.enable_auto_jit(true);
        m.set_auto_compile_strategy(CompileStrategy::Fast);
        m.set_async_compilation(true);
        m.set_tiered_compilation(tiering);
        seed_direct_leaf(&mut m, 100_000);
        m
    }

    fn seed_direct_leaf(m: &mut Executor, iterations: u32) {
        m.state = CpuState::default();
        m.state.regs = [
            5, iterations, 0xabcdef01, 3, 0x4f00, 0x4500, 0x12345678, 0x98765432,
        ];
        m.state.eip = 0x1100;
        m.state.eflags = 0x202;
        for (index, register) in m.state.xmm.iter_mut().enumerate() {
            *register = [index as u8 * 7 + 3; 16];
        }
        m.write(0x4000, &[0; 64]).unwrap();
        m.write(0x4ee0, &[0; 64]).unwrap();
        m.write(0x4f00, &0x3000u32.to_le_bytes()).unwrap();
    }

    fn warm_direct_leaf(m: &mut Executor, merged: bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let ready = m.code.compiled.contains_key(&0x1100)
                && if merged {
                    m.code.regions[&0x1100].callees.contains(&0x1180)
                        && m.stats.tier_up_publications > 0
                } else {
                    m.code.compiled.contains_key(&0x1180)
                };
            if ready {
                return;
            }
            let outcome = m.run(0x3000, 1000).unwrap();
            assert!(matches!(
                outcome,
                RunOutcome::Budget | RunOutcome::ReachedStop
            ));
            if outcome == RunOutcome::ReachedStop {
                seed_direct_leaf(m, 100_000);
            }
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
    }

    #[test]
    fn automatic_tier_up_merges_direct_register_leaves_with_exact_state_and_fewer_entries() {
        // ADD EAX,3; XOR EBX,EAX; PADDB XMM0,XMM1; RET.
        let leaf = [0x83, 0xc0, 3, 0x31, 0xc3, 0x66, 0x0f, 0xfc, 0xc1, 0xc3];
        let mut merged = direct_leaf_machine(&leaf, true);
        let mut separate = direct_leaf_machine(&leaf, false);
        warm_direct_leaf(&mut merged, true);
        warm_direct_leaf(&mut separate, false);
        assert!(!merged.code.regions[&0x1100].frontier.contains(&0x1180));
        assert!(separate.code.regions[&0x1100].frontier.contains(&0x1180));
        assert_eq!(merged.code.entry_owner.get(&0x1180), Some(&0x1180));
        for m in [&mut merged, &mut separate] {
            seed_direct_leaf(m, 1000);
        }
        let before_merged = merged.stats.native_entries;
        let before_separate = separate.stats.native_entries;
        assert_eq!(merged.run(0x3000, 10_000).unwrap(), RunOutcome::ReachedStop);
        assert_eq!(
            separate.run(0x3000, 10_000).unwrap(),
            RunOutcome::ReachedStop
        );
        assert_eq!(merged.state, separate.state);
        for (address, length) in [(0x4000, 64), (0x4ee0, 64)] {
            assert_eq!(
                merged.memory.read_data(address, length).unwrap(),
                separate.memory.read_data(address, length).unwrap()
            );
        }
        assert!(merged.stats.native_entries - before_merged <= 2);
        assert!(separate.stats.native_entries - before_separate >= 2000);

        // Scheduler slices stop at the same guest boundary after fusion.
        for m in [&mut merged, &mut separate] {
            seed_direct_leaf(m, 11);
        }
        for budget in [1, 2, 1, 5, 3, 8, 1, 2, 10, 100] {
            let a = merged.run(0x3000, budget).unwrap();
            let b = separate.run(0x3000, budget).unwrap();
            assert_eq!(a, b);
            assert_eq!(merged.state, separate.state, "fuel {budget}");
            if a == RunOutcome::ReachedStop {
                break;
            }
        }

        // Pending owner interrupts retain the same architectural boundary.
        let interrupt = Arc::new(AtomicBool::new(true));
        for m in [&mut merged, &mut separate] {
            seed_direct_leaf(m, 3);
            m.set_host_interrupt_flag(interrupt.clone());
        }
        let a = merged.run(0x3000, 100).unwrap();
        let b = separate.run(0x3000, 100).unwrap();
        assert_eq!(a, RunOutcome::Interrupted);
        assert_eq!(a, b);
        assert_eq!(merged.state, separate.state);
        interrupt.store(false, Ordering::Release);

        // SMC in the captured leaf invalidates the fused caller as well.
        let invalidations = merged.stats.invalidations;
        merged.write(0x1182, &[7]).unwrap();
        separate.write(0x1182, &[7]).unwrap();
        for m in [&mut merged, &mut separate] {
            seed_direct_leaf(m, 3);
        }
        assert_eq!(merged.run(0x3000, 100).unwrap(), RunOutcome::ReachedStop);
        assert_eq!(separate.run(0x3000, 100).unwrap(), RunOutcome::ReachedStop);
        assert!(merged.stats.invalidations > invalidations);
        // SMC queues asynchronous recompilation. Each side can publish at a
        // different instruction: interpreted work charges instructions, native
        // work charges blocks. The 3-iteration program has 26 instructions, so
        // either mix must finish with 1..=26 charged units. Only this cold SMC
        // comparison normalizes fuel; the warm and sliced checks above do not.
        for m in [&merged, &separate] {
            assert!((74..100).contains(&m.state.fuel));
        }
        let mut a = merged.state.clone();
        let mut b = separate.state.clone();
        a.fuel = 0;
        b.fuel = 0;
        assert_eq!(a, b);
        for (address, length) in [(0x4000, 64), (0x4ee0, 64)] {
            assert_eq!(
                merged.memory.read_data(address, length).unwrap(),
                separate.memory.read_data(address, length).unwrap()
            );
        }
        assert_eq!(merged.state.regs[0], 26);
    }

    #[test]
    fn direct_leaf_admission_rejects_helpers_memory_stack_changes_and_other_control_flow() {
        let cases: &[&[u8]] = &[
            &[0x0f, 0xa2, 0xc3],          // CPUID helper.
            &[0x8b, 0x00, 0xc3],          // MOV EAX,[EAX].
            &[0x89, 0x00, 0xc3],          // MOV [EAX],EAX, possibly executable.
            &[0x89, 0xc4, 0xc3],          // MOV ESP,EAX.
            &[0xe8, 0x7b, 0, 0, 0, 0xc3], // Nested CALL.
            &[0xeb, 0, 0xc3],             // JMP.
            &[0xff, 0xe0, 0xc3],          // Indirect JMP EAX.
            &[0x40, 0x40, 0x40, 0x40, 0x40, 0x40, 0x40, 0x40, 0xc3],
        ];
        for leaf in cases {
            let mut m = direct_leaf_machine(leaf, true);
            let region = crate::discovery::discover(
                &mut m.memory,
                0x1100,
                0x3000,
                CompileStrategy::Fast,
                &BTreeSet::new(),
            )
            .unwrap();
            let input = compile::Snapshot::capture(&region, &mut m.memory).unwrap();
            assert!(input.direct_leaf_callees().is_empty(), "leaf {leaf:02x?}");
        }

        let mut m = direct_leaf_machine(&[0x40, 0xc3], true);
        // Move the caller's continuation below the leaf: the CALL target is now
        // outside the captured span and cannot borrow even its executable page.
        m.write(0x110f, &0x0du32.to_le_bytes()).unwrap();
        m.write(0x1120, &[0xc3]).unwrap();
        let region = crate::discovery::discover(
            &mut m.memory,
            0x1100,
            0x3000,
            CompileStrategy::Fast,
            &BTreeSet::new(),
        )
        .unwrap();
        assert!(region.start + region.len as u32 <= 0x1180);
        assert!(
            compile::Snapshot::capture(&region, &mut m.memory)
                .unwrap()
                .direct_leaf_callees()
                .is_empty()
        );
    }

    #[test]
    fn a_secondary_native_entry_records_heat_for_its_resolved_owner() {
        let mut m = hot_machine();
        let mut region = m.code.regions[&0x1000].clone();
        region.entries.push(0x1001);
        m.add_region(region).unwrap();
        m.automatic_regions.insert(0x1000);
        m.state.eip = 0x1001;
        assert_eq!(m.run(0x3000, 100).unwrap(), RunOutcome::ReachedStop);
        assert_eq!(m.native_heat.get(&0x1000), Some(&1));
        assert!(!m.native_heat.contains_key(&0x1001));
        assert_eq!(m.state.regs[0], 0);
    }
}

#[cfg(test)]
mod removed_selector_tests {
    use super::Executor;

    #[test]
    fn legacy_gir_execution_selector_fails_explicitly() {
        if std::env::var("ADV_GIR").as_deref() != Ok("state-ssa") {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "executor::removed_selector_tests::legacy_gir_execution_selector_fails_explicitly",
                ])
                .env("ADV_GIR", "state-ssa")
                .status()
                .unwrap();
            assert!(status.success());
            return;
        }
        let error = Executor::new(0x10000, 4096)
            .err()
            .expect("removed execution selector must reject construction");
        assert!(error.contains("ADV_GIR execution has been removed"));
        assert!(error.contains("ADV_TIER2_FRONTEND=legacy"));
    }
}

#[cfg(test)]
mod publication_tests {
    use super::*;

    fn machine() -> Executor {
        let mut m = Executor::new(0x1000, 0x4000).unwrap();
        m.protect(0x1000, 0x4000, crate::READ | crate::WRITE)
            .unwrap();
        m.write(0x1000, &[0x40, 0xc3, 0x43, 0xc3]).unwrap();
        m.protect(0x1000, 4096, crate::READ | crate::EXEC).unwrap();
        m
    }

    fn seed(m: &mut Executor) {
        m.state.eip = 0x1000;
        m.state.regs[4] = 0x4f00;
        m.write(0x4f00, &0x3000u32.to_le_bytes()).unwrap();
    }

    fn targets(table: &crate::linking::Table) -> BTreeMap<u32, usize> {
        let (pointer, mask) = table.parts();
        if pointer.is_null() {
            return BTreeMap::new();
        }
        // SAFETY: parts belongs to this borrowed immutable table generation.
        unsafe { std::slice::from_raw_parts(pointer, mask as usize + 1) }
            .iter()
            .filter_map(|slot| {
                let entry = slot.entry.load(Ordering::Acquire);
                (entry != 0).then_some((slot.pc, entry))
            })
            .collect()
    }

    #[test]
    fn retiring_an_overlapped_owner_preserves_the_new_entry_target() {
        let mut m = machine();
        let mut region = Region::new(0x1000, 2);
        region.entries.push(0x1001);
        m.add_region(region).unwrap();
        m.add_region(Region::new(0x1001, 1)).unwrap();
        let target = m.code.compiled[&0x1001].tail_entry;
        assert_eq!(m.code.entry_owner[&0x1001], 0x1001);
        assert_eq!(m.code.link_targets[&0x1001], target);
        m.retire_compiled(0x1000);
        assert!(!m.code.link_targets.contains_key(&0x1000));
        assert_eq!(m.code.entry_owner[&0x1001], 0x1001);
        assert_eq!(m.code.link_targets[&0x1001], target);
        m.refresh_links();
        assert_eq!(targets(&m.code.links), BTreeMap::from([(0x1001, target)]));
    }

    #[test]
    fn unshared_links_update_in_place_and_never_name_retired_code() {
        let mut m = machine();
        let mut region = Region::new(0x1000, 2);
        region.entries.push(0x1001);
        m.add_region(region).unwrap();
        m.refresh_links();
        let table = Arc::as_ptr(&m.code.links);
        let retired = m.code.compiled[&0x1000].tail_entry;
        // Both publications stay pending: 0x1001 still names the old owner.
        m.add_region(Region::new(0x1001, 1)).unwrap();
        m.add_region(Region::new(0x1002, 2)).unwrap();
        m.retire_compiled(0x1000);
        assert_eq!(Arc::as_ptr(&m.code.links), table);
        assert_eq!(targets(&m.code.links), m.code.link_targets);
        assert!(!targets(&m.code.links).values().any(|&t| t == retired));
        let borrowed = m.code.links.clone();
        let before = targets(&borrowed);
        m.retire_compiled(0x1002);
        assert!(!Arc::ptr_eq(&borrowed, &m.code.links));
        assert_eq!(targets(&borrowed), before);
        m.refresh_links();
        assert_eq!(targets(&m.code.links), m.code.link_targets);
        assert_eq!(
            m.code.link_targets.keys().copied().collect::<Vec<_>>(),
            [0x1001]
        );
    }

    #[test]
    fn deferred_link_builds_preserve_borrowed_generations_and_follow_native_demand() {
        let mut m = machine();
        m.add_region(Region::new(0x1000, 2)).unwrap();
        seed(&mut m);
        assert_eq!(
            m.run_execution(0x3000, 100, None, true).unwrap(),
            RunOutcome::ReachedStop
        );
        assert_ne!(m.code.links_revision, m.code.cache_revision);
        assert!(targets(&m.code.links).is_empty());
        m.set_profiling(true);
        seed(&mut m);
        assert_eq!(m.run(0x3000, 100).unwrap(), RunOutcome::ReachedStop);
        assert_ne!(m.code.links_revision, m.code.cache_revision);
        m.set_profiling(false);
        m.set_region_linking(false);
        seed(&mut m);
        assert_eq!(m.run(0x3000, 100).unwrap(), RunOutcome::ReachedStop);
        assert_ne!(m.code.links_revision, m.code.cache_revision);
        m.set_region_linking(true);
        seed(&mut m);
        assert_eq!(m.run(0x3000, 100).unwrap(), RunOutcome::ReachedStop);
        assert_eq!(m.code.links_revision, m.code.cache_revision);
        let borrowed = m.code.links.clone();
        let before = targets(&borrowed);
        m.add_region(Region::new(0x1002, 2)).unwrap();
        assert!(Arc::ptr_eq(&borrowed, &m.code.links));
        seed(&mut m);
        assert_eq!(m.run(0x3000, 100).unwrap(), RunOutcome::ReachedStop);
        assert!(!Arc::ptr_eq(&borrowed, &m.code.links));
        assert_eq!(targets(&borrowed), before);
        assert_eq!(targets(&m.code.links), m.code.link_targets);
        assert!(targets(&m.code.links).contains_key(&0x1002));
        m.set_region(Region::new(0x1002, 2)).unwrap();
        assert_eq!(
            m.code.link_targets.keys().copied().collect::<Vec<_>>(),
            [0x1002]
        );
        assert!(m.validate_stop(0x1000).is_ok());
    }
}

#[cfg(test)]
mod native_deadline_tests {
    use super::*;
    use crate::{EXEC, READ, WRITE};

    fn machine(code: &[u8]) -> Executor {
        let mut m = Executor::new(0x1000, 0x5000).unwrap();
        m.protect(0x1000, 0x5000, READ | WRITE).unwrap();
        m.write(0x1000, code).unwrap();
        m.protect(0x1000, 0x1000, READ | EXEC).unwrap();
        m.add_region(Region::new(0x1000, code.len())).unwrap();
        m.state.eip = 0x1000;
        m.state.regs[0] = 7;
        m.state.regs[4] = 0x5f00;
        m.write(0x5f00, &0x4000u32.to_le_bytes()).unwrap();
        m
    }

    #[test]
    #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
    fn native_time_slice_exhaustion_qualifies_for_parallel_execution() {
        let mut m = Executor::new(0x1000, 0x5000).unwrap();
        m.set_guest_workers(2).unwrap();
        m.protect(0x1000, 0x5000, READ | WRITE).unwrap();
        let code = [0x40, 0x83, 0xc3, 3, 0xeb, 0xfa];
        m.write(0x1000, &code).unwrap();
        m.protect(0x1000, 0x1000, READ | EXEC).unwrap();
        m.add_region(Region::new(0x1000, code.len())).unwrap();
        m.state.eip = 0x1000;
        m.parallel_demand = false;
        m.deadline = Some(Instant::now() + Duration::from_millis(5));
        assert_eq!(m.run(0x4000, u64::MAX).unwrap(), RunOutcome::Budget);
        assert!(m.stats.native_blocks > 0);
        assert_eq!(m.stats.interpreter_entries, 0);
        assert!(
            m.admission.ready(),
            "wall-time exhaustion must qualify just like instruction fuel"
        );
    }

    #[test]
    fn safepoints_preserve_exact_caller_fuel_and_cpu_state() {
        // inc eax; add ebx,3; jmp back -- one native basic block.
        let code = [0x40, 0x83, 0xc3, 3, 0xeb, 0xfa];
        for budget in [1, 255, 256, 257, 511, 512, 10001] {
            let mut plain = machine(&code);
            let mut timed = machine(&code);
            timed.deadline = Some(Instant::now() + Duration::from_secs(60));
            assert_eq!(plain.run(0x4000, budget).unwrap(), RunOutcome::Budget);
            assert_eq!(timed.run(0x4000, budget).unwrap(), RunOutcome::Budget);
            assert_eq!(timed.state, plain.state, "fuel {budget}");
            assert_eq!(timed.stats.native_blocks, plain.stats.native_blocks);
            assert_eq!(timed.stats.native_blocks, budget);
            if budget > 256 {
                assert!(timed.stats.native_entries > 1);
            }
        }
    }

    #[test]
    fn safepoint_prefix_before_a_precise_memory_fault_is_not_replayed() {
        // dec ecx; jnz back; mov eax,[unmapped/outside window].
        let code = [0x49, 0x75, 0xfd, 0xa1, 0, 0, 9, 0];
        let mut plain = machine(&code);
        let mut timed = machine(&code);
        plain.state.regs[1] = 600;
        timed.state.regs[1] = 600;
        timed.deadline = Some(Instant::now() + Duration::from_secs(60));
        let a = plain.run(0x4000, 1000).unwrap();
        let b = timed.run(0x4000, 1000).unwrap();
        assert_eq!(a, b);
        assert!(matches!(
            b,
            RunOutcome::ReadFault {
                pc: 0x1003,
                address: 0x90000
            }
        ));
        assert_eq!(timed.state, plain.state);
    }

    #[test]
    fn completed_native_region_keeps_unspent_outer_fuel() {
        let mut m = machine(&[0x40, 0xc3]);
        m.deadline = Some(Instant::now() + Duration::from_secs(60));
        assert_eq!(m.run(0x4000, 10000).unwrap(), RunOutcome::ReachedStop);
        assert_eq!(m.state.regs[0], 8);
        assert_eq!(m.state.fuel, 9999);
    }

    #[test]
    fn invalidation_visits_only_regions_on_changed_pages() {
        let mut m = Executor::create(Memory::reserve(0, 0x40000).unwrap()).unwrap();
        for page in 1..=16 {
            let address = page * 4096;
            m.map(address, 4096, crate::READ | crate::WRITE | crate::EXEC)
                .unwrap();
            m.write(address, &[0x40, 0xc3]).unwrap();
            m.add_region(Region::new(address, 2)).unwrap();
        }
        m.refresh().unwrap();
        let checks = m.stats.invalidation_candidates;
        let compilations = m.stats.compilations;
        let entries: Vec<_> = m
            .code
            .compiled
            .iter()
            .map(|(&pc, c)| (pc, c.tail_entry))
            .collect();
        m.map(0x30000, 4096, crate::READ | crate::WRITE).unwrap();
        m.protect(0x30000, 4096, crate::READ).unwrap();
        m.unmap(0x30000, 4096).unwrap();
        m.refresh().unwrap();
        assert_eq!(m.stats.invalidation_candidates, checks);
        assert_eq!(m.stats.compilations, compilations);
        // Mutable data on a code page checks one region without recompiling it.
        m.write(0x1080, &[42]).unwrap();
        m.refresh().unwrap();
        assert_eq!(m.stats.invalidation_candidates, checks + 1);
        assert_eq!(m.stats.code_revalidations, 1);
        assert_eq!(m.stats.compilations, compilations);
        m.write(0x1000, &[0x48]).unwrap();
        m.refresh().unwrap();
        assert_eq!(m.stats.invalidations, 1);
        assert_eq!(m.stats.compilations, compilations + 1);
        for (pc, entry) in entries.into_iter().filter(|(pc, _)| *pc != 0x1000) {
            assert_eq!(m.code.compiled[&pc].tail_entry, entry);
        }
        assert!(m.memory.dirty_pages.is_empty());
        assert_eq!(
            m.stats.native_code_bytes,
            m.code.compiled.values().map(|c| c.code_size).sum::<usize>()
        );
    }

    #[test]
    fn invalidation_index_survives_protection_and_partial_unmap() {
        let mut m = Executor::create(Memory::reserve(0, 0x10000).unwrap()).unwrap();
        m.map(0x1000, 8192, crate::READ | crate::WRITE | crate::EXEC)
            .unwrap();
        // MOV EAX, imm32 crosses a page; RET is on the second page.
        m.write(0x1ffe, &[0xb8, 7, 0, 0, 0, 0xc3]).unwrap();
        let mut region = Region::new(0x1ffe, 2);
        region.complete_tail = true;
        region.reachable = true;
        m.add_region(region).unwrap();
        m.refresh().unwrap();
        m.protect(0x2000, 4096, crate::READ).unwrap();
        m.refresh().unwrap();
        let after_protect = m.stats.compilations;
        m.protect(0x2000, 4096, crate::READ | crate::WRITE | crate::EXEC)
            .unwrap();
        m.refresh().unwrap();
        assert!(m.stats.compilations > after_protect);
        m.unmap(0x1000, 4096).unwrap();
        m.refresh().unwrap();
        assert!(!m.code.compiled.contains_key(&0x1ffe));
        m.map(0x1000, 4096, crate::READ | crate::WRITE | crate::EXEC)
            .unwrap();
        m.write(0x1ffe, &[0xb8, 9]).unwrap();
        m.refresh().unwrap();
        assert!(m.code.compiled.contains_key(&0x1ffe));
        assert!(m.code.page_regions[&0x2000].contains(&0x1ffe));
    }

    // This source is copied unchanged into baseline and candidate for the probe.
    #[test]
    #[ignore = "diagnostic deadline latency, not a shared-runner timing gate"]
    fn report_long_region_deadline_latency() {
        let mut code = Vec::new();
        for _ in 0..200 {
            // imul eax,eax,1664525 -- a dependent chain that must execute.
            code.extend_from_slice(&[0x69, 0xc0, 0x0d, 0x66, 0x19, 0]);
            code.extend_from_slice(&[0x35, 0x79, 0x37, 0x9e, 0x55]); // xor prevents algebraic folding
        }
        code.push(0xe9);
        let displacement = -((code.len() + 4) as i32);
        code.extend_from_slice(&displacement.to_le_bytes());
        let mut m = machine(&code);
        m.run(0x4000, 100).unwrap(); // warm executable pages, not timed
        for sample in 0..9 {
            let before = m.stats.native_blocks;
            let started = Instant::now();
            m.deadline = Some(started + Duration::from_micros(100));
            assert_eq!(m.run(0x4000, 32768).unwrap(), RunOutcome::Budget);
            println!(
                "NATIVE_DEADLINE sample={sample} requested_us=100 elapsed_us={} blocks={}",
                started.elapsed().as_micros(),
                m.stats.native_blocks - before
            );
        }
    }
}
