use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::compile::{self, Compiled};
use crate::trap::{Bridge, ExecutionTrap};
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
    pub deferred_compilations: usize,
    pub peak_deferred_compilations: usize,
    pub compile_slices: u64,
    pub compile_work_rejections: u64,
    /// Largest responsive work estimate; zero means unrestricted/unmeasured.
    pub max_compile_work: usize,
    pub compile_time: Duration,
    pub native_code_bytes: usize,
    pub native_regions: usize,
    pub cache_evictions: u64,
    pub cache_rejections: u64,
    pub native_entries: u64,
    /// Entries reached by a generated tail call without returning to run_inner.
    pub linked_entries: u64,
    pub interpreter_instructions: u64,
    pub interpreter_entries: u64,
    pub native_blocks: u64,
    pub invalidations: u64,
    /// Region candidates checked after changes to their code pages only.
    pub invalidation_candidates: u64,
    pub code_revalidations: u64,
    /// Raw traps delivered to the caller-owned handler.
    pub trap_calls: u64,
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
    Parked,
}

/// Owns the guest CPU, decoded instructions and native cache. Both executors
/// access the same state and address space; only this owner publishes links.
pub struct Executor {
    pub(crate) memory: Memory,
    compiled: BTreeMap<u32, Compiled>,
    regions: BTreeMap<u32, Region>,
    page_regions: BTreeMap<u32, BTreeSet<u32>>,
    region_pages: BTreeMap<u32, BTreeSet<u32>>,
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
    background_sender: Option<adv_sync::background::LocalSender>,
    background_service: Option<adv_sync::background::Service>,
    background_wake: Option<adv_sync::WakeEvent>,
    completion_wake: Option<crate::compiler::CompletionWake>,
    interrupt: Arc<AtomicBool>,
    inactive_interrupt: AtomicBool,
    host_interrupts: bool,
    pending_regions: BTreeMap<u32, u64>,
    deferred_compilations: BTreeMap<u32, compile::Snapshot>,
    automatic_regions: BTreeSet<u32>,
    hits: HashMap<u32, u32>,
    attempted: BTreeSet<u32>,
    cache_revision: u64,
    cache_region_limit: usize,
    cache_byte_limit: usize,
    cache_clock: u64,
    rejected_entries: BTreeMap<u32, Vec<(usize, u64)>>,
    observed_epoch: u64,
    profiling: bool,
    pub(crate) deadline: Option<Instant>,
    pub profile: BTreeMap<u32, ProfileSample>,
    /// Bounded diagnostics for automatic compilation failures. Guest execution
    /// continues in the interpreter; explicit compilation still returns errors.
    pub compile_errors: BTreeMap<u32, String>,
    deferred_fallback: Option<u32>,
    timestamp: crate::timestamp::Counter,
    pub state: CpuState,
    pub stats: Stats,
}

/// Full per-thread CPU state and resumable executor progress.
#[derive(Clone)]
pub struct ExecutionContext {
    cpu: CpuState,
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

impl Executor {
    pub fn save_context(&self) -> Result<ExecutionContext, String> {
        let mut cpu = self.state.clone();
        cpu.refresh_mmx();
        Ok(ExecutionContext {
            cpu,
            deferred_fallback: self.deferred_fallback,
        })
    }
    pub fn restore_context(&mut self, context: &ExecutionContext) -> Result<(), String> {
        self.state = context.cpu.clone();
        self.state.refresh_mmx();
        self.deferred_fallback = context.deferred_fallback;
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
        let mut bridge = Bridge {
            timestamp: std::mem::take(&mut self.timestamp),
            memory: &mut self.memory,
            error: None,
            trap: None,
            calls: 0,
            handler,
            parked: false,
            deadline: self.deadline,
            bulk_calls: 0,
            bulk_bytes: 0,
        };
        let result = self.run_inner(stop, budget, &mut bridge, interpreter_only);
        // Export canonical floating state at every public CPU boundary. Context
        // saves can then borrow immutably without hiding MMX updates in RefCell.
        self.state.refresh_mmx();
        self.stats.trap_calls += bridge.calls;
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
            page_regions: BTreeMap::new(),
            region_pages: BTreeMap::new(),
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
            background_sender: None,
            background_service: None,
            background_wake: None,
            completion_wake: None,
            interrupt: Arc::new(AtomicBool::new(false)),
            inactive_interrupt: AtomicBool::new(false),
            host_interrupts: true,
            pending_regions: BTreeMap::new(),
            deferred_compilations: BTreeMap::new(),
            automatic_regions: BTreeSet::new(),
            hits: HashMap::new(),
            attempted: BTreeSet::new(),
            cache_revision: 1,
            cache_region_limit: 4096,
            cache_byte_limit: 32 * 1024 * 1024,
            cache_clock: 0,
            rejected_entries: BTreeMap::new(),
            observed_epoch: 0,
            profiling: false,
            deadline: None,
            profile: BTreeMap::new(),
            compile_errors: BTreeMap::new(),
            deferred_fallback: None,
            timestamp: crate::timestamp::Counter::default(),
            state: CpuState::default(),
            stats: Stats::default(),
        })
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
        self.state.segments = crate::SegmentState {
            selectors: [0x2b, 0x23, 0x30, 0x2b, 0x53, 0],
            gdt_base: addr,
            gdt_limit: 127,
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
        self.compiled
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
        self.links.rebuild(std::iter::empty());
        self.cache_revision += 1;
        self.compiled.clear();
        self.stats.native_code_bytes = 0;
        self.stats.native_regions = 0;
        self.rejected_entries.clear();
        self.regions.clear();
        self.page_regions.clear();
        self.region_pages.clear();
        self.entry_owner.clear();
        self.pending_regions.clear();
        self.deferred_compilations.clear();
        self.stats.deferred_compilations = 0;
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
        let start = Instant::now();
        let compiled = compile::compile(&region, &self.memory)?;
        self.stats.compilations += 1;
        self.stats.compile_time += start.elapsed();
        self.pending_regions.remove(&region.start);
        self.deferred_compilations.remove(&region.start);
        self.stats.deferred_compilations = self.deferred_compilations.len();
        self.stats.pending_compilations = self.pending_regions.len();
        self.automatic_regions.remove(&region.start);
        self.publish(region, compiled);
        Ok(())
    }

    fn publish(&mut self, region: Region, mut compiled: Compiled) {
        if self.cache_region_limit == 0 || compiled.code_size > self.cache_byte_limit {
            self.stats.cache_rejections += 1;
            for &pc in region.entries.iter().chain(std::iter::once(&region.start)) {
                self.rejected_entries.insert(pc, compiled.pages.clone());
            }
            while self.rejected_entries.len() > 4096 {
                let (pc, _) = self.rejected_entries.pop_first().unwrap();
                self.attempted.remove(&pc);
            }
            if !self.compiled.contains_key(&region.start) {
                self.forget_region(region.start);
            }
            return;
        }
        self.native_heat.remove(&region.start);
        self.tier_attempted.remove(&region.start);
        self.compile_errors.remove(&region.start);
        self.retire_compiled(region.start);
        while self.compiled.len() >= self.cache_region_limit
            || self.stats.native_code_bytes > self.cache_byte_limit - compiled.code_size
        {
            self.evict_oldest();
        }
        self.cache_clock = self.cache_clock.saturating_add(1);
        compiled.cache_stamp = self.cache_clock;
        for &pc in &compiled.entries {
            self.entry_owner.insert(pc, region.start);
        }
        self.stats.native_code_bytes += compiled.code_size;
        self.compiled.insert(region.start, compiled);
        self.stats.native_regions = self.compiled.len();
        let pages = self.compiled[&region.start]
            .pages
            .iter()
            .map(|&(page, _)| self.memory.base + (page * 4096) as u32)
            .collect();
        self.index_region_with_pages(region, Some(pages));
        self.cache_revision += 1;
    }

    fn retire_compiled(&mut self, start: u32) {
        if let Some(old) = self.compiled.remove(&start) {
            // Only the execution owner reaches this path, after every generated
            // call has returned. Remove all published raw targets before Drop
            // unregisters unwind tables and releases executable memory.
            self.links.rebuild(std::iter::empty());
            self.cache_revision += 1;
            self.stats.native_code_bytes -= old.code_size;
            for entry in old.entries.iter() {
                if self.entry_owner.get(entry) == Some(&start) {
                    self.entry_owner.remove(entry);
                }
            }
            self.stats.native_regions = self.compiled.len();
        }
    }

    fn forget_region(&mut self, start: u32) {
        if let Some(region) = self.regions.remove(&start) {
            for pc in region.entries {
                self.attempted.remove(&pc);
                self.hits.remove(&pc);
            }
        }
        if let Some(pages) = self.region_pages.remove(&start) {
            for page in pages {
                if let Some(owners) = self.page_regions.get_mut(&page) {
                    owners.remove(&start);
                    if owners.is_empty() {
                        self.page_regions.remove(&page);
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
            .compiled
            .iter()
            .min_by_key(|(_, compiled)| compiled.cache_stamp)
            .expect("every published native region has cache age")
            .0;
        self.retire_compiled(owner);
        self.forget_region(owner);
        self.stats.cache_evictions += 1;
    }

    /// Bound resident generated code by both region count and executable bytes.
    /// Defaults are 4096 regions and 32 MiB. Zero disables native residency.
    /// Eviction is owner-only, with recency sampled at Rust native entries;
    /// individual Tail-linked callees do not add hot-path instrumentation.
    pub fn set_code_cache_limits(&mut self, regions: usize, bytes: usize) {
        self.cache_region_limit = regions;
        self.cache_byte_limit = bytes;
        self.rejected_entries.clear();
        self.attempted.clear();
        while self.compiled.len() > regions || self.stats.native_code_bytes > bytes {
            self.evict_oldest();
        }
    }

    fn index_region(&mut self, region: Region) {
        self.index_region_with_pages(region, None);
    }

    fn index_region_with_pages(&mut self, region: Region, dependencies: Option<BTreeSet<u32>>) {
        if let Some(old) = self.region_pages.remove(&region.start) {
            for page in old {
                if let Some(owners) = self.page_regions.get_mut(&page) {
                    owners.remove(&region.start);
                    if owners.is_empty() {
                        self.page_regions.remove(&page);
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
            self.page_regions
                .entry(page)
                .or_default()
                .insert(region.start);
        }
        self.region_pages.insert(region.start, pages);
        self.regions.insert(region.start, region);
    }

    fn queue_region(&mut self, region: Region) -> Result<bool, String> {
        if self.pending_regions.contains_key(&region.start)
            || self.deferred_compilations.contains_key(&region.start)
        {
            return Ok(false);
        }
        let policy = self.background_sender()?.borrow().work_policy();
        if policy.background_limit().is_none()
            && self.pending_regions.len() >= crate::compiler::CAPACITY
        {
            return Ok(false);
        }
        let snapshot = compile::Snapshot::capture(&region, &self.memory)?;
        let fragments = if let Some(limit) = policy.background_limit() {
            match crate::compile_budget::plan(snapshot, limit) {
                Ok(fragments) => fragments,
                Err(error) => {
                    self.stats.compile_work_rejections += 1;
                    self.compile_errors.insert(region.start, error);
                    while self.compile_errors.len() > 64 {
                        self.compile_errors.pop_first();
                    }
                    self.attempted.extend(region.entries.iter().copied());
                    self.attempted.insert(region.start);
                    self.tier_attempted.insert(region.start);
                    return Ok(false);
                }
            }
        } else {
            vec![snapshot]
        };
        // The owner may hold bounded immutable fragments waiting for a permit;
        // no submission waits and no single job contains the original giant unit.
        if self.deferred_compilations.len() + fragments.len() > 4096 {
            return Ok(false);
        }
        self.stats.compile_slices += fragments.len().saturating_sub(1) as u64;
        for fragment in fragments {
            if !self.pending_regions.contains_key(&fragment.region.start) {
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

    fn submit_deferred_compilations(&mut self) -> Result<(), String> {
        if self.deferred_compilations.is_empty() {
            return Ok(());
        }
        if self.compiler.is_none() {
            let sender = self.background_sender()?;
            let mut worker = crate::compiler::Worker::with_sender(sender);
            if let Some(wake) = &self.completion_wake {
                worker.set_waker(wake.clone());
            }
            self.compiler = Some(worker);
        }
        while self.pending_regions.len() < crate::compiler::CAPACITY {
            let Some((&start, snapshot)) = self.deferred_compilations.first_key_value() else {
                break;
            };
            if !snapshot.valid(&self.memory) {
                let snapshot = self.deferred_compilations.remove(&start).unwrap();
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
                    self.automatic_regions.insert(region.start);
                    self.index_region(region);
                }
                Err(crate::compiler::SubmitError::Full) => break,
                Err(crate::compiler::SubmitError::Closed) => {
                    return Err("compiler worker disconnected".into());
                }
                Err(crate::compiler::SubmitError::WorkLimit { work, limit }) => {
                    self.deferred_compilations.remove(&start);
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
            if let Some(owners) = self.page_regions.get(&page) {
                affected.extend(owners.iter().copied());
            }
        }
        let mut missing = Vec::new();
        for pc in affected {
            if let Some(compiled) = self.compiled.get_mut(&pc) {
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
                self.cache_revision += 1;
                if let Some(region) = self.regions.get(&pc) {
                    for entry in &region.entries {
                        self.attempted.remove(entry);
                    }
                }
            }
            if let Some(region) = self.regions.get(&pc)
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
        self.background_service = Some(builder.start().map_err(|error| error.to_string())?);
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
        if self.attempted.contains(&pc) {
            return Ok(());
        }
        let hits = self.hits.entry(pc).or_default();
        *hits = hits.saturating_add(1);
        if *hits < 32 {
            return Ok(());
        }
        let boundaries = self
            .entry_owner
            .keys()
            .copied()
            .chain(
                self.pending_regions
                    .keys()
                    .filter_map(|owner| self.regions.get(owner))
                    .flat_map(|region| {
                        region
                            .entries
                            .iter()
                            .copied()
                            .chain(std::iter::once(region.start))
                    }),
            )
            .chain(self.deferred_compilations.values().flat_map(|input| {
                input
                    .region
                    .entries
                    .iter()
                    .copied()
                    .chain(std::iter::once(input.region.start))
            }))
            .collect();
        let Some(region) = crate::discovery::discover(
            &self.memory,
            pc,
            stop,
            self.auto_compile_strategy,
            &boundaries,
        ) else {
            self.attempted.insert(pc);
            return Ok(());
        };
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
        self.run_execution(stop, budget, None, false)
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
                .and_then(|owner| self.compiled.get_mut(owner))
                .filter(|c| {
                    !interpreter_only
                        && !single_step
                        && !pending_fpu
                        && c.entries.contains(&self.state.eip)
                        && self.deferred_fallback != Some(self.state.eip)
                });
            if let Some(native) = native {
                self.cache_clock = self.cache_clock.saturating_add(1);
                native.cache_stamp = self.cache_clock;
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
                Step::InvalidInstruction { pc } if bridge.handler.is_some() => {
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
    fn large_authored_units_are_bounded_and_linked_without_losing_budget_or_smc() {
        let (mut m, _service) = fixture(crate::RESPONSIVE_COMPILE_WORK);
        let bytes = assemble(|a| {
            let mut begin = a.create_label();
            a.set_label(&mut begin).unwrap();
            for _ in 0..40 {
                a.add(dword_ptr(DATA), 1).unwrap();
            }
            a.dec(ecx).unwrap();
            a.jnz(begin).unwrap();
            a.ret().unwrap();
        });
        let region = region(&mut m, &bytes);
        assert!(m.queue_region(region.clone()).unwrap());
        drain(&mut m);
        assert!(m.compiled.len() > 1);
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
            &80u32.to_le_bytes()
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
            &121u32.to_le_bytes()
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
            m.compiled.is_empty(),
            "old deferred/active inputs must never publish"
        );
        assert!(m.stats.stale_compilations > 0);
        assert!(m.queue_region(original.clone()).unwrap());
        drain(&mut m);
        let old = m.compiled[&CODE].tail_entry;
        original.compile_strategy = CompileStrategy::Speed;
        assert!(!m.queue_region(original).unwrap());
        assert_eq!(m.compiled[&CODE].tail_entry, old);
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
                    sizes = (m.compiled.len(), m.stats.native_code_bytes);
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
        assert_eq!(m.compiled.len(), 1);
        assert!(m.entry_owner.contains_key(&0x4000));
        assert_eq!(m.region_pages[&0x1000], BTreeSet::from([0x1000, 0x4000]));
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
        let entry = m.compiled[&0x1000].tail_entry;
        let size = m.compiled[&0x1000].code_size;
        discover(&mut m, 0x1001);
        assert_eq!(m.compiled.len(), 2);
        assert_eq!(m.compiled[&0x1000].tail_entry, entry);
        assert_eq!(m.compiled[&0x1000].code_size, size);
        assert_eq!(m.entry_owner[&0x1000], 0x1000);
        assert_eq!(m.entry_owner[&0x1001], 0x1001);
        m.state.regs[0] = 7;
        assert_eq!(m.run(0x8000, 10).unwrap(), RunOutcome::ReachedStop);
        assert_eq!(m.state.regs[0], 8);
        m.write(0x1001, &[0x48]).unwrap();
        m.refresh().unwrap();
        assert_eq!(m.stats.invalidations, 2);
        assert_eq!(m.stats.compilations, 4);
        assert_eq!(
            m.stats.native_code_bytes,
            m.compiled.values().map(|c| c.code_size).sum::<usize>()
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
        let old_entry = m.compiled[&0x1300].tail_entry;
        write(&mut m, 0x1100, |a| {
            a.inc(eax).unwrap();
            a.jmp(0x1300u64).unwrap();
        });
        discover(&mut m, 0x1100);
        assert_eq!(m.regions[&0x1100].frontier, [0x1300]);
        assert_eq!(m.compiled[&0x1300].tail_entry, old_entry);
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
        assert!(!m.entry_owner.contains_key(&0x1100));
        assert!(m.regions[&0x1000].frontier.contains(&0x1100));
        discover(&mut m, 0x1100);
        m.state.eip = 0x1000;
        assert_eq!(m.run(0x8000, 10).unwrap(), RunOutcome::ReachedStop);
        assert_eq!(m.state.regs[0], 5);
        assert_eq!(m.compiled.len(), 2);
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
        assert!(m.regions[&0x1000].len < 512);
        assert!(m.regions[&0x1000].frontier.contains(&0x6000));
        discover(&mut m, 0x6000);
        m.state.eip = 0x1000;
        assert_eq!(m.run(0x8000, 100).unwrap(), RunOutcome::ReachedStop);
        assert_eq!(m.state.regs[0], 2);
        m.write(0x2000, &[0x40; 300]).unwrap();
        m.write(0x212c, &[0xc3]).unwrap();
        discover(&mut m, 0x2000);
        assert_eq!(m.regions[&0x2000].len, 256);
        assert!(m.regions[&0x2000].frontier.contains(&0x2100));
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
        assert!(m.region_pages[&0x1ffe].contains(&0x2000));
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
        let recent = m.compiled[&0x1000].tail_entry;
        let revision = m.cache_revision;
        discover(&mut m, 0x1400);
        assert_eq!(m.compiled.len(), 2);
        assert_eq!(m.stats.native_regions, 2);
        assert_eq!(m.stats.cache_evictions, 1);
        assert_eq!(m.compiled[&0x1000].tail_entry, recent);
        assert!(!m.entry_owner.contains_key(&0x1200));
        assert!(!m.region_pages.contains_key(&0x1200));
        assert!(!m.regions.contains_key(&0x1200));
        assert!(m.cache_revision > revision);
        assert_ne!(m.links_revision, m.cache_revision);
        m.set_code_cache_limits(0, 0);
        assert!(m.compiled.is_empty());
        assert!(m.entry_owner.is_empty());
        assert!(m.page_regions.is_empty());
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
        discover(&mut m, 0x1000);
        let size = m.stats.native_code_bytes;
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
        assert!(m.entry_owner.contains_key(&0x1000));
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
        assert!(m.regions.is_empty());
        assert!(m.page_regions.is_empty());
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
        let entry = m.compiled[&0x1000].tail_entry;
        m.write(0x1200, &[0xcc]).unwrap();
        m.refresh().unwrap();
        assert_eq!(m.stats.code_revalidations, 1);
        assert_eq!(m.stats.invalidations, 0);
        assert_eq!(m.compiled[&0x1000].tail_entry, entry);
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
            assert_eq!(m.compiled[&pc].tail_entry, entry);
        }
        assert!(m.memory.dirty_pages.is_empty());
        assert_eq!(
            m.stats.native_code_bytes,
            m.compiled.values().map(|c| c.code_size).sum::<usize>()
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
        assert!(!m.compiled.contains_key(&0x1ffe));
        m.map(0x1000, 4096, crate::READ | crate::WRITE | crate::EXEC)
            .unwrap();
        m.write(0x1ffe, &[0xb8, 9]).unwrap();
        m.refresh().unwrap();
        assert!(m.compiled.contains_key(&0x1ffe));
        assert!(m.page_regions[&0x2000].contains(&0x1ffe));
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
