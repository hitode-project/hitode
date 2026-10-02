use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use unicorn_engine::unicorn_const::{Arch, HookType, MemType, Mode, Prot, uc_error};
use unicorn_engine::{RegisterX86 as R, UcHookId, Unicorn};

use crate::compile::{self, Compiled};
use crate::host::{Bridge, RejectServices};
use crate::{ApiMemory, ApiOutcome, ApiSpec, Convention, HostServices, Resume, Wait};
use crate::{CompileStrategy, CpuState, EXEC, Exit, Memory, Region};

mod registers;

const GDT: u64 = 0xffff_0000;
const XMM: [R; 8] = [
    R::XMM0,
    R::XMM1,
    R::XMM2,
    R::XMM3,
    R::XMM4,
    R::XMM5,
    R::XMM6,
    R::XMM7,
];
const REGS: [R; 8] = [
    R::EAX,
    R::ECX,
    R::EDX,
    R::EBX,
    R::ESP,
    R::EBP,
    R::ESI,
    R::EDI,
];
const ST: [R; 8] = [
    R::ST0,
    R::ST1,
    R::ST2,
    R::ST3,
    R::ST4,
    R::ST5,
    R::ST6,
    R::ST7,
];

pub(crate) fn export_mmx(
    uc: &mut Unicorn<'static, HookData>,
    state: &mut CpuState,
) -> Result<(), String> {
    if state.mmx_mode == 0 {
        return Ok(());
    }
    if state.mmx_mode & 1 != 0 {
        let sw = uc.reg_read(R::FPSW).map_err(error)?;
        let top = ((sw >> 11) & 7) as usize;
        for (index, bits) in state.mmx.iter().enumerate() {
            let reg = ST[(index + 8 - top) & 7];
            let mut extended = registers::read_st(uc, reg).map_err(error)?;
            extended[..8].copy_from_slice(&bits.to_le_bytes());
            uc.reg_write_long(reg, &extended).map_err(error)?;
        }
        uc.reg_write(R::FPSW, sw & !(7 << 11)).map_err(error)?;
    }
    uc.reg_write(R::FPTAG, if state.mmx_mode & 2 != 0 { 0xffff } else { 0 })
        .map_err(error)?;
    state.mmx_mode = 0;
    Ok(())
}

pub(crate) fn import_mmx(
    uc: &Unicorn<'static, HookData>,
    state: &mut CpuState,
) -> Result<(), String> {
    let top = ((uc.reg_read(R::FPSW).map_err(error)? >> 11) & 7) as usize;
    for (index, bits) in state.mmx.iter_mut().enumerate() {
        let extended = registers::read_st(uc, ST[(index + 8 - top) & 7]).map_err(error)?;
        *bits = u64::from_le_bytes(extended[..8].try_into().unwrap());
    }
    state.mmx_mode = 0;
    Ok(())
}

#[derive(Default, Clone, Debug)]
pub struct Stats {
    pub compilations: u64,
    pub compile_time: Duration,
    pub native_code_bytes: usize,
    pub native_entries: u64,
    pub native_blocks: u64,
    pub fallback_entries: u64,
    pub fallback_instructions: u64,
    pub invalidations: u64,
    pub host_calls: u64,
    pub callbacks: u64,
    pub waits: u64,
    pub bulk_calls: u64,
    pub bulk_bytes: u64,
    pub x87_short_handoffs: u64,
    pub x87_resident_runs: u64,
    pub x87_resident_instructions: u64,
}

#[derive(Default, Clone, Debug)]
pub struct ProfileSample {
    pub native_entries: u64,
    pub native_ns: u64,
    pub fallback_entries: u64,
    pub fallback_instructions: u64,
    pub fallback_ns: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum RunOutcome {
    ReachedStop,
    Budget,
    ReadFault { pc: u32, address: u32 },
    WriteFault { pc: u32, address: u32 },
    FetchFault { pc: u32, address: u32 },
    InvalidInstruction { pc: u32 },
    Waiting(Wait),
    Parked,
}

pub(crate) struct HookData {
    base: u32,
    pub(crate) perms: Vec<u8>,
    dirty: bool,
    dirty_pages: BTreeSet<u32>,
    hot: BTreeSet<u32>,
    skip_first: bool,
    resume_pc: u32,
    resident_left: u64,
    resident_active: bool,
    deadline: Option<Instant>,
    return_pad: Option<u32>,
    stop: u32,
    remaining: u64,
    executed: u64,
    trap: Option<u32>,
    auto_hot: bool,
    hits: std::collections::HashMap<u32, u32>,
    request: Option<u32>,
    attempted: BTreeSet<u32>,
    last_mem_fault: Option<(MemType, u32)>,
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

struct X87Site {
    version: u64,
    short_handoffs: u8,
}

pub struct Machine {
    // Drop Unicorn before the allocation referenced by mem_map_ptr.
    pub(crate) uc: Unicorn<'static, HookData>,
    pub(crate) memory: Memory,
    compiled: BTreeMap<u32, Compiled>,
    regions: BTreeMap<u32, Region>,
    entry_owner: BTreeMap<u32, u32>,
    auto_jit: bool,
    auto_compile_strategy: CompileStrategy,
    x87_residency: bool,
    x87_sites: BTreeMap<u32, X87Site>,
    cache_revision: u64,
    hot_revision: u64,
    hot_unicorn: bool,
    observed_epoch: u64,
    profiling: bool,
    pub(crate) deadline: Option<Instant>,
    pub profile: BTreeMap<u32, ProfileSample>,
    write_hook: Option<UcHookId>,
    hook_epoch: u64,
    gdt_addr: u32,
    fs_selector: u16,
    fs_dpl: u64,
    hot_hook: Option<UcHookId>,
    fallback_hook: Option<UcHookId>,
    uc_epoch: u64,
    deferred_fallback: Option<u32>,
    apis: BTreeMap<u32, ApiSpec>,
    return_pad: Option<u32>,
    callbacks: Vec<CallbackFrame>,
    suspended: Option<Suspended>,
    wake: Option<Resume>,
    pub state: CpuState,
    pub stats: Stats,
}

/// Full per-thread execution state, including Unicorn's x87/MMX/MXCSR and host continuations.
pub struct ExecutionContext {
    cpu: CpuState,
    unicorn: unicorn_engine::Context,
    callbacks: Vec<CallbackFrame>,
    suspended: Option<Suspended>,
    wake: Option<Resume>,
    deferred_fallback: Option<u32>,
}

fn error(e: impl std::fmt::Display) -> String {
    e.to_string()
}

pub(crate) fn uc_prot(p: u8) -> Prot {
    let mut out = Prot::NONE;
    if p & 1 != 0 {
        out |= Prot::READ;
    }
    if p & 2 != 0 {
        out |= Prot::WRITE;
    }
    if p & 4 != 0 {
        out |= Prot::EXEC;
    }
    out
}

impl Machine {
    pub fn save_context(&mut self) -> Result<ExecutionContext, String> {
        self.export_cpu()?;
        Ok(ExecutionContext {
            cpu: self.state.clone(),
            unicorn: self.uc.context_init().map_err(error)?,
            callbacks: self.callbacks.clone(),
            suspended: self.suspended.clone(),
            wake: self.wake,
            deferred_fallback: self.deferred_fallback,
        })
    }
    pub fn restore_context(&mut self, context: &ExecutionContext) -> Result<(), String> {
        self.uc.context_restore(&context.unicorn).map_err(error)?;
        self.state = context.cpu.clone();
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
        unicorn_only: bool,
    ) -> Result<RunOutcome, String> {
        let apis = BTreeMap::new();
        let mut bridge = Bridge {
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
        let result = self.run_inner(stop, budget, &mut bridge, unicorn_only);
        self.stats.host_calls += bridge.calls;
        self.stats.bulk_calls += bridge.bulk_calls;
        self.stats.bulk_bytes += bridge.bulk_bytes;
        self.update_write_hook()?;
        result
    }
    pub fn new(base: u32, len: usize) -> Result<Self, String> {
        Self::create(base, len, false)
    }

    /// Reserve a stable 32-bit address space, committing only mapped guest pages.
    pub fn address_space() -> Result<Self, String> {
        Self::create(0, GDT as usize, true)
    }

    fn create(base: u32, len: usize, sparse: bool) -> Result<Self, String> {
        if u64::from(base) + len as u64 > GDT {
            return Err("window overlaps reserved GDT".into());
        }
        let memory = if sparse {
            Memory::reserve(base, len)?
        } else {
            Memory::new(base, len)?
        };
        let data = HookData {
            base,
            perms: memory.perms.clone(),
            dirty: false,
            dirty_pages: BTreeSet::new(),
            hot: BTreeSet::new(),
            skip_first: false,
            resume_pc: 0,
            resident_left: 0,
            resident_active: false,
            deadline: None,
            return_pad: None,
            stop: 0,
            remaining: 0,
            executed: 0,
            trap: None,
            auto_hot: false,
            hits: std::collections::HashMap::new(),
            request: None,
            attempted: BTreeSet::new(),
            last_mem_fault: None,
        };
        let mut uc = Unicorn::new_with_data(Arch::X86, Mode::MODE_32, data).map_err(error)?;
        // SAFETY: page-aligned allocation remains alive and stable until uc is dropped.
        if !sparse {
            unsafe {
                uc.mem_map_ptr(
                    u64::from(base),
                    len as u64,
                    Prot::NONE,
                    memory.ptr.as_ptr().cast(),
                )
            }
            .map_err(error)?;
        }
        uc.mem_map(GDT, 4096, Prot::READ | Prot::WRITE)
            .map_err(error)?;
        let mut gdt = [0u8; 32];
        gdt[8..16].copy_from_slice(&descriptor(0, true).to_le_bytes());
        gdt[16..24].copy_from_slice(&descriptor(0, false).to_le_bytes());
        gdt[24..32].copy_from_slice(&descriptor(0, false).to_le_bytes());
        uc.mem_write(GDT, &gdt).map_err(error)?;
        let mut gdtr = [0u8; 24];
        gdtr[8..16].copy_from_slice(&GDT.to_le_bytes());
        gdtr[16..20].copy_from_slice(&31u32.to_le_bytes());
        uc.reg_write_long(R::GDTR, &gdtr).map_err(error)?;
        for (reg, selector) in [
            (R::CS, 8),
            (R::DS, 16),
            (R::ES, 16),
            (R::SS, 16),
            (R::FS, 24),
        ] {
            uc.reg_write(reg, selector).map_err(error)?;
        }
        uc.reg_write(R::FPCW, 0x27f).map_err(error)?;
        uc.reg_write(R::FPSW, 0).map_err(error)?;
        uc.reg_write(R::FPTAG, 0xffff).map_err(error)?;
        uc.reg_write(R::MXCSR, 0x1f80).map_err(error)?;
        uc.add_intr_hook(|uc, vector| {
            uc.get_data_mut().trap = Some(vector);
            uc.emu_stop().expect("stop guest interrupt");
        })
        .map_err(error)?;
        let bad = HookType::MEM_READ_UNMAPPED
            | HookType::MEM_WRITE_UNMAPPED
            | HookType::MEM_FETCH_UNMAPPED
            | HookType::MEM_READ_PROT
            | HookType::MEM_WRITE_PROT
            | HookType::MEM_FETCH_PROT;
        uc.add_mem_hook(bad, 1, 0, |uc, ty, addr, _, _| {
            uc.get_data_mut().last_mem_fault = Some((ty, addr as u32));
            false
        })
        .map_err(error)?;
        Ok(Self {
            uc,
            memory,
            compiled: BTreeMap::new(),
            regions: BTreeMap::new(),
            entry_owner: BTreeMap::new(),
            auto_jit: false,
            auto_compile_strategy: CompileStrategy::Speed,
            x87_residency: false,
            x87_sites: BTreeMap::new(),
            cache_revision: 1,
            hot_revision: 0,
            hot_unicorn: false,
            observed_epoch: 0,
            profiling: false,
            deadline: None,
            profile: BTreeMap::new(),
            write_hook: None,
            hook_epoch: u64::MAX,
            gdt_addr: GDT as u32,
            fs_selector: 24,
            fs_dpl: 0,
            hot_hook: None,
            fallback_hook: None,
            uc_epoch: 0,
            deferred_fallback: None,
            apis: BTreeMap::new(),
            return_pad: None,
            callbacks: Vec::new(),
            suspended: None,
            wake: None,
            state: CpuState::default(),
            stats: Stats::default(),
        })
    }

    pub(crate) fn update_write_hook(&mut self) -> Result<(), String> {
        if self.hook_epoch == self.memory.epoch {
            return Ok(());
        }
        let needed = self
            .memory
            .perms
            .iter()
            .any(|p| p & (EXEC | crate::WRITE) == EXEC | crate::WRITE);
        if !needed {
            if let Some(hook) = self.write_hook.take() {
                self.uc.remove_hook(hook).map_err(error)?;
            }
        } else if self.write_hook.is_none() {
            self.write_hook = Some(
                self.uc
                    .add_mem_hook(
                        HookType::MEM_WRITE,
                        u64::from(self.memory.base),
                        u64::from(self.memory.base) + self.memory.len as u64 - 1,
                        |uc, _, addr, size, _| {
                            let d = uc.get_data_mut();
                            if let Some(offset) = addr.checked_sub(u64::from(d.base)) {
                                let first = offset as usize / 4096;
                                let last = (offset as usize + size.saturating_sub(1)) / 4096;
                                if (first..=last)
                                    .any(|page| d.perms.get(page).is_some_and(|p| p & EXEC != 0))
                                {
                                    d.dirty = true;
                                    for page in first..=last {
                                        if d.perms.get(page).is_some_and(|p| p & EXEC != 0) {
                                            d.dirty_pages.insert(d.base + page as u32 * 4096);
                                        }
                                    }
                                }
                            }
                            false
                        },
                    )
                    .map_err(error)?,
            );
        }
        self.hook_epoch = self.memory.epoch;
        Ok(())
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
        self.uc.mem_write(u64::from(addr), &gdt).map_err(error)?;
        let mut gdtr = [0u8; 24];
        gdtr[8..16].copy_from_slice(&u64::from(addr).to_le_bytes());
        gdtr[16..20].copy_from_slice(&127u32.to_le_bytes());
        self.uc.reg_write_long(R::GDTR, &gdtr).map_err(error)?;
        for (reg, selector) in [
            (R::CS, 0x23),
            (R::DS, 0x2b),
            (R::ES, 0x2b),
            (R::SS, 0x30),
            (R::FS, 0x53),
        ] {
            self.uc.reg_write(reg, selector).map_err(error)?;
        }
        self.gdt_addr = addr;
        self.fs_selector = 0x53;
        self.fs_dpl = 3;
        Ok(())
    }

    pub fn memory(&self) -> &Memory {
        &self.memory
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
        self.apis.insert(spec.id, spec);
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

    /// Loader/debugger write, intentionally independent of guest page permissions.
    pub fn write(&mut self, addr: u32, bytes: &[u8]) -> Result<(), String> {
        self.memory.write(addr, bytes)?;
        if !bytes.is_empty() {
            self.uc
                .ctl_remove_cache(u64::from(addr), u64::from(addr) + bytes.len() as u64)
                .map_err(error)?;
        }
        Ok(())
    }

    pub fn protect(&mut self, addr: u32, len: usize, perms: u8) -> Result<(), String> {
        // Validate before either executor's permissions change.
        self.memory.range(addr, len)?;
        if !addr.is_multiple_of(4096) || len == 0 || !len.is_multiple_of(4096) || perms & !7 != 0 {
            return Err("invalid page permissions or alignment".into());
        }
        let mut uc_perms = Prot::NONE;
        if perms & crate::READ != 0 {
            uc_perms |= Prot::READ;
        }
        if perms & crate::WRITE != 0 {
            uc_perms |= Prot::WRITE;
        }
        if perms & EXEC != 0 {
            uc_perms |= Prot::EXEC;
        }
        self.uc
            .mem_protect(u64::from(addr), len as u64, uc_perms)
            .map_err(error)?;
        self.memory.protect(addr, len, perms)?;
        self.uc.get_data_mut().perms.clone_from(&self.memory.perms);
        self.update_write_hook()
    }

    /// Compile explicitly selected hot code. Cold code is always available in Unicorn.
    pub fn set_region(&mut self, region: Region) -> Result<(), String> {
        self.compiled.clear();
        self.regions.clear();
        self.entry_owner.clear();
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
        self.entry_owner.retain(|_, owner| *owner != region.start);
        for &pc in &compiled.entries {
            self.entry_owner.insert(pc, region.start);
        }
        self.compiled.insert(region.start, compiled);
        self.regions.insert(region.start, region);
        self.stats.native_code_bytes = self.compiled.values().map(|c| c.code_size).sum();
        self.cache_revision += 1;
        Ok(())
    }

    fn refresh(&mut self) -> Result<(), String> {
        if self.observed_epoch == self.memory.epoch {
            return Ok(());
        }
        self.observed_epoch = self.memory.epoch;
        let stale: Vec<_> = self
            .compiled
            .iter()
            .filter(|(_, c)| !c.valid(&self.memory))
            .map(|(&pc, _)| pc)
            .collect();
        for pc in stale {
            self.compiled.remove(&pc);
            self.entry_owner.retain(|_, owner| *owner != pc);
            self.stats.invalidations += 1;
            self.cache_revision += 1;
            if let Some(region) = self.regions.get(&pc) {
                for entry in &region.entries {
                    self.uc.get_data_mut().attempted.remove(entry);
                }
            }
        }
        let missing: Vec<_> = self
            .regions
            .values()
            .filter(|r| {
                !self.compiled.contains_key(&r.start) && self.memory.executable(r.start, r.len)
            })
            .cloned()
            .collect();
        for region in missing {
            self.add_region(region)?;
        }
        Ok(())
    }

    pub fn enable_auto_jit(&mut self, enabled: bool) {
        self.auto_jit = enabled;
        if enabled && self.hot_hook.is_none() {
            self.hot_hook = Some(
                self.uc
                    .add_block_hook(1, 0, |uc, addr, _| {
                        let d = uc.get_data_mut();
                        let pc = addr as u32;
                        if d.auto_hot
                            && !d.hot.contains(&pc)
                            && !d.attempted.contains(&pc)
                            && d.request.is_none()
                        {
                            let hits = d.hits.entry(pc).or_default();
                            *hits = hits.saturating_add(1);
                            if *hits >= 32 {
                                d.request = Some(pc);
                                uc.reg_write(R::EIP, addr).expect("hot block PC");
                                uc.emu_stop().expect("promote hot guest block");
                            }
                        }
                    })
                    .expect("install hot block counter"),
            );
        }
    }
    /// Select the policy for newly discovered automatic regions. Existing regions
    /// retain their policy when invalidated or expanded with another entry point.
    pub fn set_auto_compile_strategy(&mut self, strategy: CompileStrategy) {
        self.auto_compile_strategy = strategy;
    }
    /// Keep observed dense x87 loops in Unicorn for a bounded instruction window.
    /// Disabled by default; arithmetic and memory semantics remain Unicorn's.
    pub fn set_x87_residency(&mut self, enabled: bool) {
        self.x87_residency = enabled;
        if !enabled {
            self.x87_sites.clear();
        }
    }
    pub fn set_profiling(&mut self, enabled: bool) {
        self.profiling = enabled;
    }

    fn discover(&mut self) -> Result<(), String> {
        let pc = self.state.eip;
        if !self.auto_jit || self.entry_owner.contains_key(&pc) || !self.memory.executable(pc, 1) {
            return Ok(());
        }
        if self.uc.get_data_mut().request.take() != Some(pc) {
            return Ok(());
        }
        self.uc.get_data_mut().attempted.insert(pc);
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
        self.add_region(region)
    }

    pub fn map(&mut self, addr: u32, len: usize, perms: u8) -> Result<(), String> {
        self.memory.map(addr, len, perms)?;
        let offset = (addr - self.memory.base) as usize;
        // SAFETY: reservation is stable and committed for the entire mapping.
        if let Err(e) = unsafe {
            self.uc.mem_map_ptr(
                u64::from(addr),
                len as u64,
                uc_prot(perms),
                self.memory.ptr.as_ptr().add(offset).cast(),
            )
        } {
            self.memory.unmap(addr, len)?;
            return Err(error(e));
        }
        self.uc.get_data_mut().perms.clone_from(&self.memory.perms);
        self.update_write_hook()
    }

    pub fn unmap(&mut self, addr: u32, len: usize) -> Result<(), String> {
        self.memory.pages(addr, len)?;
        self.memory.committed(addr, len)?;
        self.uc
            .mem_unmap(u64::from(addr), len as u64)
            .map_err(error)?;
        self.memory.unmap(addr, len)?;
        self.uc.get_data_mut().perms.clone_from(&self.memory.perms);
        self.update_write_hook()
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

    /// Same ABI, callback and wait machinery, with guest instructions executed by Unicorn.
    pub fn run_unicorn_with_services(
        &mut self,
        stop: u32,
        budget: u64,
        services: &mut dyn HostServices,
    ) -> Result<RunOutcome, String> {
        self.run_services(stop, budget, services, true)
    }

    fn run_services(
        &mut self,
        stop: u32,
        budget: u64,
        services: &mut dyn HostServices,
        unicorn_only: bool,
    ) -> Result<RunOutcome, String> {
        let apis = self.apis.clone();
        let mut bridge = Bridge {
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
        let result = self.run_inner(stop, budget, &mut bridge, unicorn_only);
        self.stats.host_calls += bridge.calls;
        self.stats.bulk_calls += bridge.bulk_calls;
        self.stats.bulk_bytes += bridge.bulk_bytes;
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
        unicorn_only: bool,
    ) -> Result<RunOutcome, String> {
        if self.regions.values().any(|r| {
            u64::from(stop) >= u64::from(r.start)
                && u64::from(stop) < u64::from(r.start) + r.len as u64
        }) {
            return Err("stop address must be outside the compiled region".into());
        }
        self.state.fuel = budget;
        loop {
            if bridge.parked {
                return Ok(RunOutcome::Parked);
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
            self.refresh()?;
            if !unicorn_only {
                self.discover()?;
            }
            self.uc.get_data_mut().auto_hot = self.auto_jit && !unicorn_only;
            if self.hot_revision != self.cache_revision || self.hot_unicorn != unicorn_only {
                self.uc.get_data_mut().hot = if unicorn_only {
                    BTreeSet::new()
                } else {
                    self.entry_owner.keys().copied().collect()
                };
                self.hot_revision = self.cache_revision;
                self.hot_unicorn = unicorn_only;
            }
            if let Some(pad) = self.return_pad {
                self.uc.get_data_mut().hot.insert(pad);
            }
            let native = self
                .entry_owner
                .get(&self.state.eip)
                .and_then(|owner| self.compiled.get(owner))
                .filter(|c| {
                    !unicorn_only
                        && c.entries.contains(&self.state.eip)
                        && self.deferred_fallback != Some(self.state.eip)
                });
            if let Some(native) = native {
                let before = self.state.fuel;
                let pc = self.state.eip;
                let timer = self.profiling.then(Instant::now);
                if let Some(raw) = bridge.raw.as_mut() {
                    raw.bind(&mut self.uc);
                }
                let result = native.run(&mut self.state, &mut self.memory, bridge);
                if let Some(timer) = timer {
                    let p = self.profile.entry(pc).or_default();
                    p.native_entries += 1;
                    p.native_ns += timer.elapsed().as_nanos() as u64;
                }
                self.stats.native_entries += 1;
                self.stats.native_blocks += before - self.state.fuel;
                match result {
                    n if n == Exit::Host as u32 => {
                        if bridge.raw.is_some() {
                            self.update_write_hook()?;
                        }
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
                    n if n == Exit::Budget as u32 => return Ok(RunOutcome::Budget),
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
                    }
                    _ => return Err(format!("invalid native exit {result}")),
                }
            }
            let pc = self.state.eip;
            let timer = self.profiling.then(Instant::now);
            let before = self.stats.fallback_instructions;
            let fallback_result = self.fallback(stop);
            if let Some(timer) = timer {
                let p = self.profile.entry(pc).or_default();
                p.fallback_entries += 1;
                p.fallback_instructions += self.stats.fallback_instructions - before;
                p.fallback_ns += timer.elapsed().as_nanos() as u64;
            }
            if let Some(outcome) = fallback_result? {
                return Ok(outcome);
            }
            if let Some(vector) = self.uc.get_data_mut().trap.take() {
                if vector != 0x2e && bridge.raw.is_none() {
                    return Err(format!("guest exception {vector} at {:#x}", self.state.eip));
                }
                bridge.memory = &mut self.memory;
                if let Some(raw) = bridge.raw.as_mut() {
                    raw.bind(&mut self.uc);
                }
                // SAFETY: same live bridge/state contract as the native call site.
                if bridge.raw.is_some() {
                    crate::host::call_raw(bridge, &mut self.state, vector);
                } else {
                    unsafe {
                        crate::host::host_call((bridge as *mut Bridge<'_>).cast(), &mut self.state);
                    }
                }
                if bridge.raw.is_some() {
                    self.update_write_hook()?;
                }
                if let Some(outcome) = bridge.pending.take() {
                    let origin = Origin {
                        esp: self.state.regs[4],
                        eip: self.state.eip,
                    };
                    self.apply_host(outcome?, origin)?;
                }
            }
            if self.state.eip != pc {
                self.deferred_fallback = None;
            }
        }
    }

    pub(crate) fn export_cpu(&mut self) -> Result<(), String> {
        export_mmx(&mut self.uc, &mut self.state)?;
        // Includes writes made by resumed callbacks and waits, not just API calls.
        if self.uc_epoch != self.memory.epoch {
            self.invalidate_guest_pages()?;
        }
        for (reg, value) in XMM.iter().zip(&self.state.xmm) {
            self.uc.reg_write_long(*reg, value).map_err(error)?;
        }
        for (reg, value) in REGS.iter().zip(self.state.regs) {
            self.uc.reg_write(*reg, u64::from(value)).map_err(error)?;
        }
        self.uc
            .reg_write(R::EIP, u64::from(self.state.eip))
            .map_err(error)?;
        self.uc
            .reg_write(R::EFLAGS, u64::from(self.state.eflags))
            .map_err(error)?;
        self.uc
            .mem_write(
                u64::from(self.gdt_addr) + u64::from(self.fs_selector >> 3) * 8,
                &descriptor_dpl(self.state.fs_base, false, self.fs_dpl).to_le_bytes(),
            )
            .map_err(error)?;
        self.uc
            .reg_write(R::FS, u64::from(self.fs_selector))
            .map_err(error)?;
        Ok(())
    }

    fn import_cpu(&mut self) -> Result<(), String> {
        import_mmx(&self.uc, &mut self.state)?;
        for (reg, value) in XMM.iter().zip(&mut self.state.xmm) {
            *value = registers::read_xmm(&self.uc, *reg).map_err(error)?;
        }
        for (reg, value) in REGS.iter().zip(&mut self.state.regs) {
            *value = self.uc.reg_read(*reg).map_err(error)? as u32;
        }
        self.state.eip = self.uc.reg_read(R::EIP).map_err(error)? as u32;
        self.state.eflags = self.uc.reg_read(R::EFLAGS).map_err(error)? as u32;
        if self.uc.get_data().dirty {
            let dirty = std::mem::take(&mut self.uc.get_data_mut().dirty_pages);
            for page in dirty {
                self.memory.touch(page, 4096);
            }
            self.uc.get_data_mut().dirty = false;
            self.invalidate_guest_pages()?;
        }
        Ok(())
    }

    fn invalidate_guest_pages(&mut self) -> Result<(), String> {
        // Unicorn translates the first guest address to its own RAM offset before
        // invalidating a range. A sparse window can start unmapped, and adjacent
        // guest mappings need not have adjacent RAM offsets, so invalidate per page.
        let pages = std::mem::take(&mut self.memory.dirty_pages);
        for page in pages {
            if self.memory.executable(page, 4096) {
                self.uc
                    .ctl_remove_cache(u64::from(page), u64::from(page) + 4096)
                    .map_err(error)?;
            }
        }
        self.uc_epoch = self.memory.epoch;
        Ok(())
    }

    fn fallback(&mut self, stop: u32) -> Result<Option<RunOutcome>, String> {
        let pc = self.state.eip;
        let site_version = if self.x87_residency && !self.uc.get_data().hot.is_empty() {
            self.x87_site_version(pc)
        } else {
            None
        };
        let resident = site_version.is_some_and(|version| {
            let site = self.x87_sites.entry(pc).or_insert(X87Site {
                version,
                short_handoffs: 0,
            });
            if site.version != version {
                site.version = version;
                site.short_handoffs = 0;
            }
            site.short_handoffs >= 32
        });
        self.export_cpu()?;
        let d = self.uc.get_data_mut();
        d.stop = stop;
        d.skip_first = true;
        d.resume_pc = self.state.eip;
        d.resident_active = resident;
        d.resident_left = if resident { 128 } else { 0 };
        d.deadline = self.deadline;
        d.return_pad = self.return_pad;
        d.remaining = self.state.fuel;
        d.executed = 0;
        d.last_mem_fault = None;
        if self.fallback_hook.is_none() {
            self.fallback_hook = Some(
                self.uc
                    .add_code_hook(1, 0, |uc, addr, _| {
                        let d = uc.get_data_mut();
                        let first = std::mem::take(&mut d.skip_first);
                        // Unicorn may restart the writing instruction after invalidating its own
                        // translation. Wait for its next PC before handing control back to native.
                        let handoff = if d.dirty {
                            addr as u32 != d.resume_pc
                        } else {
                            d.resident_left == 0 && d.hot.contains(&(addr as u32))
                        };
                        let restart = !first && d.dirty && addr as u32 == d.resume_pc;
                        let expired = d.resident_active
                            && d.executed.is_multiple_of(32)
                            && d.deadline.is_some_and(|at| Instant::now() >= at);
                        if expired {
                            d.remaining = 0;
                        }
                        if !restart
                            && (d.remaining == 0
                                || addr as u32 == d.stop
                                || d.return_pad == Some(addr as u32)
                                || (!first && handoff))
                        {
                            uc.emu_stop().expect("stop fallback");
                        } else if !restart {
                            d.remaining -= 1;
                            d.executed += 1;
                            d.resident_left = d.resident_left.saturating_sub(1);
                        }
                    })
                    .map_err(error)?,
            );
        }
        let result = self
            .uc
            .emu_start(u64::from(self.state.eip), u64::from(stop), 0, 0);
        self.state.fuel = self.uc.get_data().remaining;
        self.stats.fallback_entries += 1;
        self.stats.fallback_instructions += self.uc.get_data().executed;
        let executed = self.uc.get_data().executed;
        let clean = !self.uc.get_data().dirty && self.uc.get_data().trap.is_none();
        if resident {
            self.stats.x87_resident_runs += 1;
            self.stats.x87_resident_instructions += executed;
        }
        self.import_cpu()?;
        if site_version.is_some()
            && result.is_ok()
            && clean
            && (1..=16).contains(&executed)
            && self.entry_owner.contains_key(&self.state.eip)
        {
            let site = self.x87_sites.get_mut(&pc).unwrap();
            site.short_handoffs = site.short_handoffs.saturating_add(1);
            self.stats.x87_short_handoffs += 1;
        }
        self.unicorn_result(result)
    }

    fn x87_site_version(&self, pc: u32) -> Option<u64> {
        if !self.memory.executable(pc, 1) {
            return None;
        }
        for offset in 0..15 {
            let at = pc.checked_add(offset)?;
            let opcode = *self.memory.read(at, 1).ok()?.first()?;
            if (0xd8..=0xdf).contains(&opcode) {
                let page = (pc - self.memory.base) as usize / 4096;
                return Some(self.memory.versions[page]);
            }
            if !matches!(
                opcode,
                0x26 | 0x2e | 0x36 | 0x3e | 0x64 | 0x65 | 0x66 | 0x67
            ) {
                return None;
            }
        }
        None
    }

    fn unicorn_result(
        &mut self,
        result: Result<(), uc_error>,
    ) -> Result<Option<RunOutcome>, String> {
        let Err(err) = result else {
            return Ok(None);
        };
        if let Some((ty, address)) = self.uc.get_data_mut().last_mem_fault.take() {
            let pc = self.state.eip;
            return Ok(Some(match ty {
                MemType::READ_UNMAPPED | MemType::READ_PROT => {
                    RunOutcome::ReadFault { pc, address }
                }
                MemType::WRITE_UNMAPPED | MemType::WRITE_PROT => {
                    RunOutcome::WriteFault { pc, address }
                }
                _ => RunOutcome::FetchFault { pc, address },
            }));
        }
        if err == uc_error::INSN_INVALID {
            return Ok(Some(RunOutcome::InvalidInstruction { pc: self.state.eip }));
        }
        Err(format!("Unicorn at {:#x}: {err}", self.state.eip))
    }

    /// Differential/performance reference: no per-instruction hot-entry hook.
    pub fn run_unicorn(
        &mut self,
        stop: u32,
        instruction_budget: usize,
    ) -> Result<RunOutcome, String> {
        if instruction_budget == 0 {
            return Ok(RunOutcome::Budget);
        }
        self.run_unicorn_inner(stop, instruction_budget)
    }

    /// Benchmark known terminating code without Unicorn's instruction-count hook.
    pub fn run_unicorn_unbounded(&mut self, stop: u32) -> Result<RunOutcome, String> {
        self.run_unicorn_inner(stop, 0)
    }

    fn run_unicorn_inner(
        &mut self,
        stop: u32,
        instruction_budget: usize,
    ) -> Result<RunOutcome, String> {
        if let Some(hook) = self.fallback_hook.take() {
            self.uc.remove_hook(hook).map_err(error)?;
        }
        self.uc.get_data_mut().auto_hot = false;
        self.uc.get_data_mut().last_mem_fault = None;
        self.export_cpu()?;
        let result = self.uc.emu_start(
            u64::from(self.state.eip),
            u64::from(stop),
            0,
            instruction_budget,
        );
        self.import_cpu()?;
        if let Some(outcome) = self.unicorn_result(result)? {
            return Ok(outcome);
        }
        Ok(if self.state.eip == stop {
            RunOutcome::ReachedStop
        } else {
            RunOutcome::Budget
        })
    }
}

fn descriptor(base: u32, code: bool) -> u64 {
    descriptor_dpl(base, code, 0)
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
