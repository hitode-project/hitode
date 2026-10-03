use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::compile::{self, Compiled};
use crate::host::{Bridge, RejectServices};
use crate::{ApiMemory, ApiOutcome, ApiSpec, Convention, HostServices, Resume, Wait};
use crate::{CompileStrategy, CpuState, Exit, Memory, Region};

#[derive(Default, Clone, Debug)]
pub struct Stats {
    pub compilations: u64,
    pub compile_requests: u64,
    pub tier_up_requests: u64,
    pub tier_up_publications: u64,
    pub compile_failures: u64,
    pub stale_compilations: u64,
    pub pending_compilations: usize,
    pub peak_pending_compilations: usize,
    pub compile_time: Duration,
    pub native_code_bytes: usize,
    pub native_entries: u64,
    /// Entries reached by a generated tail call without returning to run_inner.
    pub linked_entries: u64,
    pub interpreter_instructions: u64,
    pub interpreter_entries: u64,
    pub native_blocks: u64,
    pub invalidations: u64,
    pub code_revalidations: u64,
    pub host_calls: u64,
    pub callbacks: u64,
    pub waits: u64,
    pub bulk_calls: u64,
    pub bulk_bytes: u64,
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
    ReachedStop,
    Budget,
    Interrupted,
    ReadFault { pc: u32, address: u32 },
    WriteFault { pc: u32, address: u32 },
    FetchFault { pc: u32, address: u32 },
    InvalidInstruction { pc: u32 },
    UnsupportedInstruction { pc: u32, code: iced_x86::Code },
    Trap { pc: u32, vector: u8 },
    Waiting(Wait),
    Parked,
}

#[derive(Clone, Copy)]
struct Origin {
    esp: u32,
    eip: u32,
}
#[derive(Clone)]
struct CallbackFrame {
    origin: Origin,
    continuation: u64,
    return_esp: u32,
}
#[derive(Clone)]
struct Suspended {
    origin: Origin,
    wait: Wait,
    continuation: u64,
}

/// Owns the guest CPU, decoded instructions and native cache. Both executors
/// access the same state and address space; only this owner publishes links.
pub struct Machine {
    pub(crate) memory: Memory,
    compiled: BTreeMap<u32, Compiled>,
    regions: BTreeMap<u32, Region>,
    entry_owner: BTreeMap<u32, u32>,
    links: crate::linking::Table,
    links_revision: u64,
    region_linking: bool,
    interpreter: crate::interpreter::Interpreter,
    auto_jit: bool,
    auto_compile_strategy: CompileStrategy,
    tiered_compilation: bool,
    native_heat: HashMap<u32, u64>,
    tier_attempted: BTreeSet<u32>,
    async_compilation: bool,
    compiler: Option<crate::compiler::Worker>,
    completion_wake: Option<crate::compiler::CompletionWake>,
    interrupt: Arc<AtomicBool>,
    inactive_interrupt: AtomicBool,
    host_interrupts: bool,
    pending_regions: BTreeMap<u32, u64>,
    automatic_regions: BTreeSet<u32>,
    hits: HashMap<u32, u32>,
    attempted: BTreeSet<u32>,
    cache_revision: u64,
    observed_epoch: u64,
    profiling: bool,
    pub(crate) deadline: Option<Instant>,
    pub profile: BTreeMap<u32, ProfileSample>,
    /// Bounded diagnostics for automatic compilation failures. Guest execution
    /// continues in the interpreter; explicit compilation still returns errors.
    pub compile_errors: BTreeMap<u32, String>,
    deferred_fallback: Option<u32>,
    apis: Arc<BTreeMap<u32, ApiSpec>>,
    return_pad: Option<u32>,
    callbacks: Vec<CallbackFrame>,
    suspended: Option<Suspended>,
    wake: Option<Resume>,
    timestamp: crate::timestamp::Counter,
    pub state: CpuState,
    pub stats: Stats,
}

/// Full guest-owned per-thread CPU state and host continuations.
#[derive(Clone)]
pub struct ExecutionContext {
    cpu: CpuState,
    callbacks: Vec<CallbackFrame>,
    suspended: Option<Suspended>,
    wake: Option<Resume>,
    deferred_fallback: Option<u32>,
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

impl Machine {
    pub fn save_context(&mut self) -> Result<ExecutionContext, String> {
        self.state.refresh_mmx();
        Ok(ExecutionContext {
            cpu: self.state.clone(),
            callbacks: self.callbacks.clone(),
            suspended: self.suspended.clone(),
            wake: self.wake,
            deferred_fallback: self.deferred_fallback,
        })
    }
    pub fn restore_context(&mut self, context: &ExecutionContext) -> Result<(), String> {
        self.state = context.cpu.clone();
        self.state.refresh_mmx();
        self.callbacks = context.callbacks.clone();
        self.suspended = context.suspended.clone();
        self.wake = context.wake;
        self.deferred_fallback = context.deferred_fallback;
        Ok(())
    }

    #[cfg(feature = "pal")]
    pub(crate) fn run_traps(
        &mut self,
        stop: u32,
        budget: u64,
        raw: &mut dyn crate::host::RawTrap,
        interpreter_only: bool,
    ) -> Result<RunOutcome, String> {
        let apis = BTreeMap::new();
        let mut bridge = Bridge {
            timestamp: std::mem::take(&mut self.timestamp),
            apis: &apis,
            services: &mut RejectServices,
            memory: &mut self.memory,
            pending: None,
            calls: 0,
            raw: Some(raw),
            parked: false,
            deadline: self.deadline,
            bulk_calls: 0,
            bulk_bytes: 0,
        };
        let result = self.run_inner(stop, budget, &mut bridge, interpreter_only);
        self.stats.host_calls += bridge.calls;
        self.stats.bulk_calls += bridge.bulk_calls;
        self.stats.bulk_bytes += bridge.bulk_bytes;
        self.timestamp = bridge.timestamp;
        result
    }
    pub fn new(base: u32, len: usize) -> Result<Self, String> {
        Self::create(Memory::new(base, len)?)
    }

    /// Reserve all 4 GiB; only mapped guest pages consume physical memory.
    pub fn address_space() -> Result<Self, String> {
        Self::create(Memory::reserve(0, 1usize << 32)?)
    }

    fn create(memory: Memory) -> Result<Self, String> {
        Ok(Self {
            memory,
            compiled: BTreeMap::new(),
            regions: BTreeMap::new(),
            entry_owner: BTreeMap::new(),
            links: crate::linking::Table::default(),
            links_revision: 0,
            region_linking: true,
            interpreter: crate::interpreter::Interpreter::new(),
            auto_jit: false,
            auto_compile_strategy: CompileStrategy::Fast,
            tiered_compilation: true,
            native_heat: HashMap::new(),
            tier_attempted: BTreeSet::new(),
            async_compilation: false,
            compiler: None,
            completion_wake: None,
            interrupt: Arc::new(AtomicBool::new(false)),
            inactive_interrupt: AtomicBool::new(false),
            host_interrupts: true,
            pending_regions: BTreeMap::new(),
            automatic_regions: BTreeSet::new(),
            hits: HashMap::new(),
            attempted: BTreeSet::new(),
            cache_revision: 1,
            observed_epoch: 0,
            profiling: false,
            deadline: None,
            profile: BTreeMap::new(),
            compile_errors: BTreeMap::new(),
            deferred_fallback: None,
            apis: Arc::new(BTreeMap::new()),
            return_pad: None,
            callbacks: Vec::new(),
            suspended: None,
            wake: None,
            timestamp: crate::timestamp::Counter::default(),
            state: CpuState::default(),
            stats: Stats::default(),
        })
    }

    #[cfg(feature = "pal")]
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
        self.state.segments = crate::SegmentState {
            selectors: [0x2b, 0x23, 0x30, 0x2b, 0x53, 0],
            gdt_base: addr,
            gdt_limit: 127,
        };
        Ok(())
    }

    #[cfg(feature = "pal")]
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

    pub fn memory(&self) -> &Memory {
        &self.memory
    }

    /// Registered ABI signatures, also usable by independent validation backends.
    pub fn api_specs(&self) -> impl Iterator<Item = &ApiSpec> {
        self.apis.values()
    }

    pub fn register_api(&mut self, spec: ApiSpec) -> Result<(), String> {
        if spec.convention.argc() > 16
            || self.apis.contains_key(&spec.id)
            || self.apis.values().any(|a| {
                u64::from(a.entry) < u64::from(spec.entry) + 16
                    && u64::from(spec.entry) < u64::from(a.entry) + 16
            })
        {
            return Err("invalid or overlapping API registration".into());
        }
        self.write(spec.entry, &spec.stub())?;
        Arc::make_mut(&mut self.apis).insert(spec.id, spec);
        Ok(())
    }

    pub fn set_callback_return_pad(&mut self, address: u32) -> Result<(), String> {
        if !self.memory.executable(address, 1)
            || self.regions.values().any(|r| {
                u64::from(address) >= u64::from(r.start)
                    && u64::from(address) < u64::from(r.start) + r.len as u64
            })
        {
            return Err("callback return pad must be executable and outside the hot region".into());
        }
        self.return_pad = Some(address);
        Ok(())
    }

    /// Queue a matching wake-up. The continuation runs on the next run_with_services.
    pub fn wake(&mut self, reason: Resume) -> bool {
        let matches = self
            .suspended
            .as_ref()
            .is_some_and(|s| match (s.wait, reason) {
                (Wait::Event(a), Resume::Signaled(b)) => a == b,
                (Wait::Until(deadline), Resume::Deadline(now)) => now >= deadline,
                _ => false,
            });
        if matches && self.wake.is_none() {
            self.wake = Some(reason);
            true
        } else {
            false
        }
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
        self.compiled.clear();
        self.regions.clear();
        self.entry_owner.clear();
        self.pending_regions.clear();
        self.automatic_regions.clear();
        self.native_heat.clear();
        self.tier_attempted.clear();
        self.hits.clear();
        self.attempted.clear();
        self.stats.pending_compilations = 0;
        self.cache_revision += 1;
        self.add_region(region)
    }

    pub fn add_region(&mut self, region: Region) -> Result<(), String> {
        if self.return_pad.is_some_and(|p| {
            u64::from(p) >= u64::from(region.start)
                && u64::from(p) < u64::from(region.start) + region.len as u64
        }) {
            return Err("region overlaps callback return pad".into());
        }
        let start = Instant::now();
        let compiled = compile::compile(&region, &self.memory)?;
        self.stats.compilations += 1;
        self.stats.compile_time += start.elapsed();
        self.pending_regions.remove(&region.start);
        self.stats.pending_compilations = self.pending_regions.len();
        self.automatic_regions.remove(&region.start);
        self.publish(region, compiled);
        Ok(())
    }

    fn publish(&mut self, region: Region, compiled: Compiled) {
        self.native_heat.remove(&region.start);
        self.tier_attempted.remove(&region.start);
        self.compile_errors.remove(&region.start);
        self.entry_owner.retain(|_, owner| *owner != region.start);
        for &pc in &compiled.entries {
            self.entry_owner.insert(pc, region.start);
        }
        self.compiled.insert(region.start, compiled);
        self.regions.insert(region.start, region);
        self.stats.native_code_bytes = self.compiled.values().map(|c| c.code_size).sum();
        self.cache_revision += 1;
    }

    fn queue_region(&mut self, region: Region) -> Result<bool, String> {
        if self.return_pad.is_some_and(|pad| {
            u64::from(pad) >= u64::from(region.start)
                && u64::from(pad) < u64::from(region.start) + region.len as u64
        }) {
            return Err("region overlaps callback return pad".into());
        }
        if self.pending_regions.contains_key(&region.start)
            || self.pending_regions.len() >= crate::compiler::CAPACITY
        {
            return Ok(false);
        }
        let snapshot = compile::Snapshot::capture(&region, &self.memory)?;
        if self.compiler.is_none() {
            let mut worker = crate::compiler::Worker::new()
                .map_err(|e| format!("start compiler worker: {e}"))?;
            if let Some(wake) = &self.completion_wake {
                worker.set_waker(wake.clone());
            }
            self.compiler = Some(worker);
        }
        match self.compiler.as_ref().unwrap().try_submit(snapshot) {
            Ok(ticket) => {
                self.stats.compile_requests += 1;
                self.pending_regions.insert(region.start, ticket);
                self.stats.pending_compilations = self.pending_regions.len();
                self.stats.peak_pending_compilations = self
                    .stats
                    .peak_pending_compilations
                    .max(self.pending_regions.len());
                self.automatic_regions.insert(region.start);
                self.regions.insert(region.start, region);
                Ok(true)
            }
            Err(std::sync::mpsc::TrySendError::Full(_)) => Ok(false),
            Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                Err("compiler worker disconnected".into())
            }
        }
    }

    /// Publish only on the execution owner, outside generated code, after both
    /// request identity and every captured code-page version have been checked.
    fn collect_compilations(&mut self) -> Result<(), String> {
        loop {
            let Some(worker) = self.compiler.as_ref() else {
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
            self.stats.compile_time += completion.duration;
            let start = completion.region.start;
            if self.pending_regions.get(&start) != Some(&completion.ticket) {
                self.stats.stale_compilations += 1;
                continue;
            }
            self.pending_regions.remove(&start);
            self.stats.pending_compilations = self.pending_regions.len();
            if !completion.input.valid(&self.memory) {
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
                    if completion.region.compile_strategy == CompileStrategy::Speed
                        && self.compiled.contains_key(&start)
                        && self.tier_attempted.contains(&start)
                    {
                        self.stats.tier_up_publications += 1;
                    }
                    self.publish(completion.region, compiled);
                }
                Err(error) => {
                    // A compilation failure must leave the original guest code
                    // executable by the interpreter; explicit compilation returns errors.
                    self.stats.compile_failures += 1;
                    self.compile_errors.insert(start, error);
                    while self.compile_errors.len() > 64 {
                        self.compile_errors.pop_first();
                    }
                }
            }
        }
    }

    fn refresh(&mut self) -> Result<(), String> {
        if self.observed_epoch == self.memory.epoch {
            return Ok(());
        }
        self.observed_epoch = self.memory.epoch;
        let mut stale = Vec::new();
        for (&pc, compiled) in &mut self.compiled {
            if compiled.valid(&self.memory) {
                continue;
            }
            if compiled.revalidate(&self.memory) {
                self.stats.code_revalidations += 1;
            } else {
                stale.push(pc);
            }
        }
        for pc in stale {
            self.compiled.remove(&pc);
            self.native_heat.remove(&pc);
            self.tier_attempted.remove(&pc);
            self.entry_owner.retain(|_, owner| *owner != pc);
            self.stats.invalidations += 1;
            self.cache_revision += 1;
            if let Some(region) = self.regions.get(&pc) {
                for entry in &region.entries {
                    self.attempted.remove(entry);
                }
            }
        }
        self.stats.native_code_bytes = self.compiled.values().map(|c| c.code_size).sum();
        let missing: Vec<_> = self
            .regions
            .values()
            .filter(|r| {
                !self.compiled.contains_key(&r.start) && self.memory.executable(r.start, r.len)
            })
            .cloned()
            .collect();
        for region in missing {
            if self.async_compilation && self.automatic_regions.contains(&region.start) {
                self.queue_region(region)?;
            } else {
                self.add_region(region)?;
            }
        }
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
        self.tiered_compilation = enabled;
    }
    pub fn set_region_linking(&mut self, enabled: bool) {
        self.region_linking = enabled;
    }
    pub fn set_profiling(&mut self, enabled: bool) {
        self.profiling = enabled;
    }
    /// Automatic hot-code promotion uses a bounded worker when enabled.
    /// Explicit add_region remains synchronous for authored code and tests.
    pub fn set_async_compilation(&mut self, enabled: bool) {
        self.async_compilation = enabled;
        if !enabled {
            self.tier_attempted.clear();
            self.pending_regions.clear();
            self.stats.pending_compilations = 0;
            self.compiler = None;
        }
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

    pub fn set_host_interrupts(&mut self, enabled: bool) {
        self.host_interrupts = enabled;
    }

    fn record_native_work(&mut self, entry: u32, blocks: u64) {
        const HOT_BLOCKS: u64 = 16_384;
        if !self.auto_jit || !self.async_compilation || !self.tiered_compilation || blocks == 0 {
            return;
        }
        let Some(&owner) = self.entry_owner.get(&entry) else {
            return;
        };
        if !self.automatic_regions.contains(&owner)
            // Reserve at least half the bounded queue for cold discoveries.
            || self.pending_regions.len() >= crate::compiler::CAPACITY / 2
            || self.pending_regions.contains_key(&owner)
            || self.tier_attempted.contains(&owner)
        {
            return;
        }
        let Some(region) = self.regions.get(&owner) else {
            return;
        };
        if region.compile_strategy != CompileStrategy::Fast
            || !self
                .compiled
                .get(&owner)
                .is_some_and(|c| c.valid(&self.memory))
        {
            return;
        }
        // Native chains are sampled at their Rust entry, not timed per guest
        // instruction. This intentionally approximates heat without perturbing
        // generated code or attributing wall time to a background compiler.
        let heat = self.native_heat.entry(owner).or_default();
        *heat = heat.saturating_add(blocks);
        if *heat < HOT_BLOCKS {
            return;
        }
        let old_region = region.clone();
        let mut optimized = old_region.clone();
        optimized.compile_strategy = CompileStrategy::Speed;
        match self.queue_region(optimized) {
            Ok(true) => {
                // Keep Fast code AND its published metadata live while Speed is
                // compiling. Failure must not demote hot code to interpretation.
                self.regions.insert(owner, old_region);
                self.tier_attempted.insert(owner);
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
        if self.links_revision == self.cache_revision {
            return;
        }
        self.links
            .rebuild(self.entry_owner.iter().filter_map(|(&pc, owner)| {
                self.compiled
                    .get(owner)
                    .map(|compiled| (pc, compiled.tail_entry))
            }));
        self.links_revision = self.cache_revision;
    }
    fn discover(&mut self, stop: u32) -> Result<(), String> {
        let pc = self.state.eip;
        if !self.auto_jit || self.entry_owner.contains_key(&pc) || !self.memory.executable(pc, 1) {
            return Ok(());
        }
        if self.attempted.contains(&pc) {
            return Ok(());
        }
        let hits = self.hits.entry(pc).or_default();
        *hits = hits.saturating_add(1);
        if *hits < 32 {
            return Ok(());
        }
        // Small regions keep cold startup compilation bounded; loops remain linked inside them.
        let start = pc & !511;
        let mut region = self
            .regions
            .values()
            .find(|r| pc >= r.start && u64::from(pc) < u64::from(r.start) + r.len as u64)
            .cloned()
            .unwrap_or_else(|| {
                let mut r = Region::new(start, 512);
                r.reachable = true;
                r.complete_tail = true;
                r.compile_strategy = self.auto_compile_strategy;
                r
            });
        if !region.entries.contains(&pc) {
            region.entries.push(pc);
        }
        let contains_boundary = |boundary: u32| {
            u64::from(boundary) >= u64::from(region.start)
                && u64::from(boundary) < u64::from(region.start) + region.len as u64
        };
        if contains_boundary(stop) || self.return_pad.is_some_and(contains_boundary) {
            // Keep this small automatic window interpreted rather than absorb a
            // continuation boundary into a native region's internal dispatch.
            self.attempted.insert(pc);
            return Ok(());
        }
        if self.async_compilation {
            if self.queue_region(region)? {
                self.attempted.insert(pc);
            }
        } else {
            self.add_region(region)?;
            self.attempted.insert(pc);
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
        self.run_with_services(stop, budget, &mut RejectServices)
    }

    pub fn run_with_services(
        &mut self,
        stop: u32,
        budget: u64,
        services: &mut dyn HostServices,
    ) -> Result<RunOutcome, String> {
        self.run_services(stop, budget, services, false)
    }

    fn run_services(
        &mut self,
        stop: u32,
        budget: u64,
        services: &mut dyn HostServices,
        interpreter_only: bool,
    ) -> Result<RunOutcome, String> {
        // Share immutable signatures: short scheduler slices must not deep-copy
        // the complete API table and all module/name strings on every entry.
        let apis = Arc::clone(&self.apis);
        let mut bridge = Bridge {
            timestamp: std::mem::take(&mut self.timestamp),
            apis: &apis,
            services,
            memory: &mut self.memory,
            pending: None,
            calls: 0,
            raw: None,
            parked: false,
            deadline: self.deadline,
            bulk_calls: 0,
            bulk_bytes: 0,
        };
        let result = self.run_inner(stop, budget, &mut bridge, interpreter_only);
        self.stats.host_calls += bridge.calls;
        self.stats.bulk_calls += bridge.bulk_calls;
        self.stats.bulk_bytes += bridge.bulk_bytes;
        self.timestamp = bridge.timestamp;
        result
    }

    fn apply_host(&mut self, outcome: ApiOutcome, origin: Origin) -> Result<(), String> {
        match outcome {
            ApiOutcome::Return(value) => {
                self.state.regs[0] = value;
                self.state.regs[4] = origin.esp;
                self.state.eip = origin.eip;
            }
            ApiOutcome::Callback {
                target,
                args,
                convention,
                continuation,
            } => {
                if args.len() != convention.argc() {
                    return Err("callback ABI argument count mismatch".into());
                }
                let pad = self
                    .return_pad
                    .ok_or("callback return pad is not configured")?;
                let bytes = (args.len() as u32 + 1)
                    .checked_mul(4)
                    .ok_or("callback stack overflow")?;
                let esp = origin
                    .esp
                    .checked_sub(bytes)
                    .ok_or("callback stack underflow")?;
                let mut memory = ApiMemory {
                    inner: &mut self.memory,
                };
                memory.writable(esp, bytes as usize)?;
                memory.put_u32(esp, pad)?;
                for (n, value) in args.iter().enumerate() {
                    memory.put_u32(esp + 4 + n as u32 * 4, *value)?;
                }
                let return_esp = match convention {
                    Convention::Stdcall(_) => origin.esp,
                    Convention::Cdecl(_) => esp + 4,
                };
                self.callbacks.push(CallbackFrame {
                    origin,
                    continuation,
                    return_esp,
                });
                self.state.regs[4] = esp;
                self.state.eip = target;
                self.stats.callbacks += 1;
            }
            ApiOutcome::Wait { wait, continuation } => {
                self.suspended = Some(Suspended {
                    origin,
                    wait,
                    continuation,
                });
                self.stats.waits += 1;
            }
        }
        Ok(())
    }

    pub(crate) fn run_inner(
        &mut self,
        stop: u32,
        budget: u64,
        bridge: &mut Bridge<'_>,
        interpreter_only: bool,
    ) -> Result<RunOutcome, String> {
        if self.regions.values().any(|r| {
            u64::from(stop) >= u64::from(r.start)
                && u64::from(stop) < u64::from(r.start) + r.len as u64
        }) {
            return Err("stop address must be outside the compiled region".into());
        }
        self.state.refresh_mmx();
        self.state.fuel = budget;
        loop {
            if bridge.parked {
                return Ok(RunOutcome::Parked);
            }
            if self.state.eip == stop {
                return Ok(RunOutcome::ReachedStop);
            }
            if self.deadline.is_some_and(|t| Instant::now() >= t) {
                return Ok(RunOutcome::Budget);
            }
            if let Some(waiting) = &self.suspended {
                if let Some(reason) = self.wake.take() {
                    let pending = self.suspended.take().unwrap();
                    let out = bridge.services.resume(
                        pending.continuation,
                        reason,
                        &mut ApiMemory {
                            inner: &mut self.memory,
                        },
                    )?;
                    self.apply_host(out, pending.origin)?;
                    continue;
                }
                return Ok(RunOutcome::Waiting(waiting.wait));
            }
            if self.return_pad == Some(self.state.eip) {
                let frame = self.callbacks.pop().ok_or("unexpected callback return")?;
                if self.state.regs[4] != frame.return_esp {
                    return Err("callback returned with wrong stack convention".into());
                }
                let value = self.state.regs[0];
                self.state.regs[4] = frame.origin.esp;
                self.state.eip = frame.origin.eip;
                let out = bridge.services.resume(
                    frame.continuation,
                    Resume::Returned(value),
                    &mut ApiMemory {
                        inner: &mut self.memory,
                    },
                )?;
                self.apply_host(out, frame.origin)?;
                continue;
            }
            if self.state.eip == stop {
                return Ok(RunOutcome::ReachedStop);
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
            let single_step = self.state.eflags & (1 << 8) != 0;
            // Pending x87 exceptions must reach a waiting x87/MMX instruction
            // before it changes state. Native MMX assumes no pending #MF.
            let pending_fpu = self.state.x87.status & 0x80 != 0;
            if !interpreter_only && !single_step && !pending_fpu {
                self.discover(stop)?;
            }
            self.refresh_links();
            let native = self
                .entry_owner
                .get(&self.state.eip)
                .and_then(|owner| self.compiled.get(owner))
                .filter(|c| {
                    !interpreter_only
                        && !single_step
                        && !pending_fpu
                        && c.entries.contains(&self.state.eip)
                        && self.deferred_fallback != Some(self.state.eip)
                });
            if let Some(native) = native {
                // Internal region loops do not use the inter-region chain limit.
                // Reserve unused caller fuel and periodically return through Rust
                // while a wall deadline is active, without charging extra work.
                let reserved = if self.deadline.is_some() {
                    let reserved = self.state.fuel.saturating_sub(256);
                    self.state.fuel -= reserved;
                    reserved
                } else {
                    0
                };
                let before = self.state.fuel;
                let pc = self.state.eip;
                let timer = self.profiling.then(Instant::now);
                let (result, linked) = native.run(
                    &mut self.state,
                    &mut self.memory,
                    bridge,
                    compile::NativeRun {
                        links: (self.region_linking && !self.profiling).then_some(&self.links),
                        stop,
                        return_pad: self.return_pad,
                        interrupt: if self.host_interrupts {
                            self.interrupt.as_ref()
                        } else {
                            &self.inactive_interrupt
                        },
                    },
                );
                if let Some(timer) = timer {
                    let p = self.profile.entry(pc).or_default();
                    p.native_entries += 1;
                    p.native_ns += timer.elapsed().as_nanos() as u64;
                }
                self.stats.native_entries += 1 + linked;
                self.stats.linked_entries += linked;
                let completed = before - self.state.fuel;
                self.state.fuel += reserved;
                self.stats.native_blocks += completed;
                self.record_native_work(pc, completed);
                match result {
                    n if n == Exit::Host as u32 => {
                        if let Some(outcome) = bridge.pending.take() {
                            let origin = Origin {
                                esp: self.state.regs[4],
                                eip: self.state.eip,
                            };
                            self.apply_host(outcome?, origin)?;
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
            let pc = self.state.eip;
            let timer = self.profiling.then(Instant::now);
            // Contiguous integer and floating fallback instructions share a bounded
            // decoded block. Transfers, exceptions, SMC and REP return here; native
            // publications change the revision used to build fallthrough boundaries.
            let units = self.state.fuel.min(256) as u32;
            let (execution, last_units) = if self.profiling || single_step {
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
                    self.return_pad,
                    (!interpreter_only
                        && !single_step
                        && !pending_fpu
                        && !self.entry_owner.is_empty())
                    .then_some((&self.entry_owner, self.cache_revision)),
                )
            };
            self.stats.interpreter_entries += 1;
            self.stats.interpreter_instructions += u64::from(execution.units);
            self.state.fuel -= u64::from(execution.units);
            if let Some(timer) = timer {
                let p = self.profile.entry(pc).or_default();
                p.interpreter_entries += 1;
                p.interpreter_instructions += u64::from(execution.units);
                p.interpreter_ns += timer.elapsed().as_nanos() as u64;
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
                Step::InvalidInstruction { pc } if bridge.raw.is_some() => {
                    Step::Trap { pc, vector: 6 }
                }
                other => other,
            };
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
                    if vector != 0x2e && bridge.raw.is_none() {
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
                    if bridge.raw.is_some() {
                        crate::host::call_raw(bridge, &mut self.state, u32::from(vector));
                    } else {
                        // SAFETY: bridge and CPU are live and exclusively borrowed.
                        unsafe {
                            crate::host::host_call(
                                (bridge as *mut Bridge<'_>).cast(),
                                &mut self.state,
                            );
                        }
                    }
                    if let Some(outcome) = bridge.pending.take() {
                        let origin = Origin {
                            esp: self.state.regs[4],
                            eip: self.state.eip,
                        };
                        self.apply_host(outcome?, origin)?;
                    }
                }
            }
        }
    }
}

#[cfg(feature = "pal")]
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

    fn fixture() -> Machine {
        let mut m = Machine::new(0x1000, 0x4000).unwrap();
        m.protect(0x1000, 0x4000, READ | WRITE).unwrap();
        m.write(0x1000, &[0xb8, 1, 0, 0, 0, 0xc3]).unwrap();
        m.protect(0x1000, 4096, READ | EXEC).unwrap();
        m.write(0x4f00, &0x3000u32.to_le_bytes()).unwrap();
        m.state.eip = 0x1000;
        m.state.regs[4] = 0x4f00;
        m.set_async_compilation(true);
        m
    }
    #[cfg(feature = "pal")]
    #[test]
    fn lazy_worker_inherits_wake_and_pending_jobs_reject_replacement() {
        let mut m = fixture();
        let wake = pal_host::WakeEvent::new().unwrap();
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
    fn collect_one(m: &mut Machine) {
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
        assert!(m.compiled.is_empty());
        assert!(m.entry_owner.is_empty());
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
    fn automatic_regions_cannot_absorb_the_callback_return_pad() {
        let mut m = fixture();
        m.set_callback_return_pad(0x1080).unwrap();
        m.write(0x1000, &[0x40, 0xeb, 0xfd]).unwrap();
        m.enable_auto_jit(true);
        assert_eq!(m.run(0x3000, 1000).unwrap(), RunOutcome::Budget);
        assert_eq!(m.stats.compile_requests, 0);
        assert_eq!(m.state.regs[0], 500);
        assert!(m.queue_region(Region::new(0x1000, 512)).is_err());
    }
    #[test]
    fn automatic_discovery_preserves_a_stop_inside_its_nominal_window() {
        let mut m = fixture();
        m.write(0x1000, &[0x40, 0x83, 0xf8, 100, 0x75, 0xfa])
            .unwrap();
        m.enable_auto_jit(true);
        assert_eq!(m.run(0x1006, 1000).unwrap(), RunOutcome::ReachedStop);
        assert_eq!(m.state.regs[0], 100);
        assert_eq!(m.stats.compile_requests, 0);
    }
}

#[cfg(all(test, feature = "pal"))]
mod floating_boundary_tests {
    use super::*;
    use crate::{EXEC, READ, WRITE};

    #[test]
    fn raw_host_exception_exits_native_before_waiting_mmx() {
        struct Handler(Vec<(u32, u32)>);
        impl crate::host::RawTrap for Handler {
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
        let mut m = Machine::new(0x1000, 0x4000).unwrap();
        m.protect(0x1000, 0x4000, READ | WRITE).unwrap();
        m.write(0x1000, &[0xcd, 0x2e, 0x40, 0x0f, 0xef, 0xc0, 0xc3])
            .unwrap();
        m.protect(0x1000, 4096, READ | EXEC).unwrap();
        m.state.eip = 0x1000;
        m.add_region(Region::new(0x1000, 7)).unwrap();
        let registers = m.state.x87.regs;
        let mut handler = Handler(Vec::new());
        assert_eq!(
            m.run_traps(0x3000, 100, &mut handler, false).unwrap(),
            RunOutcome::Parked
        );
        assert_eq!(handler.0, [(0x2e, 0x1002), (16, 0x1003)]);
        assert_eq!(m.state.x87.regs, registers);
        assert_eq!(m.stats.native_entries, 1);
    }

    #[test]
    fn batched_fault_prefix_charges_the_same_fuel_as_single_instruction_execution() {
        struct Repair(usize);
        impl crate::host::RawTrap for Repair {
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
            let mut m = Machine::new(0x1000, 0x4000).unwrap();
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
                m.run_traps(0x3000, 2, handler, true).unwrap(),
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
                m.run_traps(0x3000, 2, handler, true).unwrap(),
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
    fn hot_machine() -> Machine {
        let mut m = Machine::new(0x1000, 0x4000).unwrap();
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
    fn collect(m: &mut Machine) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !m.pending_regions.is_empty() {
            m.collect_compilations().unwrap();
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
    }
    #[test]
    fn speed_compilation_keeps_fast_code_live_and_publishes_once() {
        let mut m = hot_machine();
        let entry = m.compiled[&0x1000].tail_entry;
        m.record_native_work(0x1000, 16_383);
        assert_eq!(m.stats.tier_up_requests, 0);
        m.record_native_work(0x1000, 1);
        assert_eq!(m.stats.tier_up_requests, 1);
        assert_eq!(m.compiled[&0x1000].tail_entry, entry);
        assert_eq!(m.regions[&0x1000].compile_strategy, CompileStrategy::Fast);
        m.record_native_work(0x1000, 100_000);
        assert_eq!(m.stats.tier_up_requests, 1);
        collect(&mut m);
        assert_eq!(m.regions[&0x1000].compile_strategy, CompileStrategy::Speed);
        assert_eq!(m.stats.tier_up_publications, 1);
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
}

#[cfg(test)]
mod native_deadline_tests {
    use super::*;
    use crate::{EXEC, READ, WRITE};

    fn machine(code: &[u8]) -> Machine {
        let mut m = Machine::new(0x1000, 0x5000).unwrap();
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
