//! unicorn 2 (QEMU TCG) backend.

use std::collections::{BTreeMap, HashMap};
use std::ops::{Deref, DerefMut};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

mod guard;
mod watchdog;
pub use guard::Guarded;
use watchdog::Watchdog;

use unicorn_engine::unicorn_const::{Arch, HookType, MemType, Mode, Prot as UcProt, uc_error};
use unicorn_engine::{RegisterX86, Unicorn};

use adv_cpu::backend::{
    Backend, Exit, Fault, FaultKind, FilePage, FpuEnvironment, FpuEnvironmentError, Guest,
    MemFault, Prot, Reg, SegmentSetup, TrapAction, TrapHandler,
};

struct FileBinding {
    page: Rc<FilePage>,
    copy_source: Option<Rc<FilePage>>,
    protection: Prot,
    mapped: bool,
}

impl FileBinding {
    fn prepare(&self) -> Result<(), String> {
        self.refresh_read()?;
        self.page.prepare()
    }

    fn commit_write(&mut self) {
        self.copy_source = None;
        self.page.mark_dirty();
    }

    fn refresh_read(&self) -> Result<(), String> {
        if let Some(source) = &self.copy_source {
            // A COPY shadow has a stable pointer for Unicorn. Before its first
            // write it observes current shared bytes; afterwards it is private.
            self.page.write(0, &source.snapshot()?)?;
        }
        Ok(())
    }
}

/// GDT selectors. CS/DS/FS match what 32-bit code sees under WOW64. SS must be a DPL0
/// descriptor because unicorn runs at CPL0.
mod sel {
    pub const CS: u16 = 0x23; // index 4, RPL3
    pub const DS: u16 = 0x2b; // index 5, RPL3
    pub const SS: u16 = 0x30; // index 6, RPL0
    pub const FS: u16 = 0x53; // index 10, RPL3
    pub const GDT_ENTRIES: u32 = 16;
}

pub struct HookData {
    handler: Option<*mut (dyn TrapHandler + 'static)>,
    park: u32,
    last_mem_fault: Option<(MemType, u32)>,
    /// Executed blocks and guest code bytes, while the block hook is installed.
    blocks: (u64, u64),
    /// Per block start: (executions, size), while profiling.
    profile: Option<HashMap<u32, (u64, u32)>>,
    count_blocks: bool,
    budget_active: bool,
    budget_expired: Arc<AtomicBool>,
    file_pages: BTreeMap<u32, FileBinding>,
}

/// The raw engine as trap handlers see it. A local type, so the guest interface defined in
/// adv-cpu can be implemented for it; layout-identical to `Unicorn`.
#[repr(transparent)]
struct Engine<'a>(Guarded<'a, HookData>);

impl<'a> Engine<'a> {
    /// The view a hook gets of the engine it runs in. Only borrowed, so never dropped.
    fn from_mut<'b>(uc: &'b mut Unicorn<'a, HookData>) -> &'b mut Self {
        // SAFETY: `Engine` and `Guarded` are `repr(transparent)` wrappers around `Unicorn`.
        unsafe { &mut *(uc as *mut Unicorn<'a, HookData>).cast::<Self>() }
    }
}

impl<'a> Deref for Engine<'a> {
    type Target = Guarded<'a, HookData>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for Engine<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

pub struct UnicornCpu {
    // Fields drop in declaration order: join the timer before closing the engine.
    watchdog: Option<Watchdog>,
    uc: Engine<'static>,
    park: u32,
    gdt: Option<u32>,
    budget: Option<Duration>,
    counted: bool,
}

impl UnicornCpu {
    /// `park` must lie in memory the caller maps (any mapped address that guest code never
    /// legitimately reaches). Each run defaults to a five-second host wall-clock budget;
    /// change or disable it with [`Backend::set_execution_budget`].
    pub fn new(park: u32) -> Result<Self, uc_error> {
        let data = HookData {
            handler: None,
            park,
            last_mem_fault: None,
            blocks: (0, 0),
            profile: None,
            count_blocks: false,
            budget_active: false,
            budget_expired: Arc::default(),
            file_pages: BTreeMap::new(),
        };
        let mut uc = Guarded::new_with_data(Arch::X86, Mode::MODE_32, data)?;
        // Keep one hook for the lifetime of the engine. Adding/removing it per slice
        // invalidates Unicorn's translated blocks and causes cache churn.
        uc.add_block_hook(1, 0, |uc, addr, size| {
            let d = uc.get_data_mut();
            if d.budget_active && d.budget_expired.load(Ordering::Acquire) {
                // Stop before executing any instruction in this block. An asynchronous
                // uc_emu_stop can leave EIP at the block start after prefix stores ran.
                uc.emu_stop()
                    .expect("stop guest at a resumable block boundary");
                return;
            }
            if d.count_blocks {
                d.blocks.0 += 1;
                d.blocks.1 += u64::from(size);
                if let Some(p) = &mut d.profile {
                    p.entry(addr as u32).or_insert((0, size)).0 += 1;
                }
            }
        })?;
        uc.add_intr_hook(|uc, intno| {
            let h = uc
                .get_data()
                .handler
                .expect("guest trap outside Backend::run");
            let park = uc.get_data().park;
            // SAFETY: `handler` is set only for the duration of `run`, which holds the
            // `&mut dyn TrapHandler` it was created from. The handler is disjoint from the CPU.
            let action = unsafe { &mut *h }.on_trap(Engine::from_mut(uc), intno as u8);
            if action == TrapAction::Park {
                // Writing PC from a hook restarts translation at `park`, which is the
                // `until` address of emu_start, so emulation stops before executing it.
                uc.set_pc(park.into()).expect("set pc");
            }
        })?;
        let bad = HookType::MEM_READ_UNMAPPED
            | HookType::MEM_WRITE_UNMAPPED
            | HookType::MEM_FETCH_UNMAPPED
            | HookType::MEM_READ_PROT
            | HookType::MEM_WRITE_PROT
            | HookType::MEM_FETCH_PROT;
        uc.add_mem_hook(bad, 1, 0, |uc, ty, addr, _size, _value| {
            if matches!(ty, MemType::READ_UNMAPPED | MemType::WRITE_UNMAPPED)
                && materialize_file_page(uc, addr as u32, ty == MemType::WRITE_UNMAPPED).is_ok()
            {
                return true;
            }
            uc.get_data_mut().last_mem_fault = Some((ty, addr as u32));
            false
        })?;
        uc.add_mem_hook(
            HookType::MEM_READ | HookType::MEM_WRITE,
            1,
            0,
            |uc, ty, addr, size, _| {
                let start = (addr as u32) & !4095;
                let end = (addr + size as u64).min(1u64 << 32);
                for (&page, binding) in uc
                    .get_data()
                    .file_pages
                    .range(start..)
                    .take_while(|(page, _)| u64::from(**page) < end)
                {
                    if (ty == MemType::WRITE && !binding.protection.contains(Prot::WRITE))
                        || binding.prepare().is_err()
                    {
                        uc.get_data_mut().last_mem_fault = Some((
                            if ty == MemType::WRITE {
                                MemType::WRITE_UNMAPPED
                            } else {
                                MemType::READ_UNMAPPED
                            },
                            page,
                        ));
                        let _ = uc.emu_stop();
                        return false;
                    }
                }
                if ty == MemType::WRITE {
                    for (_, binding) in uc
                        .get_data_mut()
                        .file_pages
                        .range_mut(start..)
                        .take_while(|(page, _)| u64::from(**page) < end)
                    {
                        binding.commit_write();
                    }
                }
                true
            },
        )?;
        // Unicorn starts with both control words zero: every FP exception unmasked and x87
        // precision control at 24 bits. A new Windows thread has 53-bit precision, round to
        // nearest and everything masked. Contexts start from a snapshot of this state.
        uc.reg_write(RegisterX86::FPSW, 0)?;
        uc.reg_write(RegisterX86::FPTAG, 0xffff)?;
        uc.reg_write(RegisterX86::FPCW, 0x27f)?;
        uc.reg_write(RegisterX86::MXCSR, 0x1f80)?;
        Ok(Self {
            watchdog: None,
            uc: Engine(uc),
            park,
            gdt: None,
            budget: Some(Duration::from_secs(5)),
            counted: false,
        })
    }

    fn write_gdt_entry(&mut self, index: u32, desc: u64) -> Result<(), MemFault> {
        let gdt = self.gdt.expect("setup_segments not called");
        Guest::write(self, gdt + index * 8, &desc.to_le_bytes())
    }
}

fn uc_reg(r: Reg) -> RegisterX86 {
    match r {
        Reg::Eax => RegisterX86::EAX,
        Reg::Ecx => RegisterX86::ECX,
        Reg::Edx => RegisterX86::EDX,
        Reg::Ebx => RegisterX86::EBX,
        Reg::Esp => RegisterX86::ESP,
        Reg::Ebp => RegisterX86::EBP,
        Reg::Esi => RegisterX86::ESI,
        Reg::Edi => RegisterX86::EDI,
        Reg::Eip => RegisterX86::EIP,
        Reg::Eflags => RegisterX86::EFLAGS,
    }
}

fn uc_prot(p: Prot) -> UcProt {
    let mut v = UcProt::NONE;
    if p.contains(Prot::READ) {
        v |= UcProt::READ;
    }
    if p.contains(Prot::WRITE) {
        v |= UcProt::WRITE;
    }
    if p.contains(Prot::EXEC) {
        v |= UcProt::EXEC;
    }
    v
}

fn materialize_file_page(
    uc: &mut Unicorn<'_, HookData>,
    address: u32,
    write: bool,
) -> Result<(), ()> {
    let base = address & !4095;
    let Some(binding) = uc.get_data().file_pages.get(&base) else {
        return Err(());
    };
    if binding.mapped
        || !binding
            .protection
            .contains(if write { Prot::WRITE } else { Prot::READ })
    {
        return Err(());
    }
    let (page, protection) = (binding.page.clone(), binding.protection);
    // SAFETY: the development-only engine and Rc-confined UnsafeCell storage
    // operate on one worker. HookData retains the stable allocation until
    // unmapping; no Rust references to its data are exposed.
    let pointer = unsafe { page.foreign_pointer() }.map_err(|_| ())?;
    unsafe { uc.mem_map_ptr(base.into(), 4096, uc_prot(protection), pointer.cast()) }
        .map_err(|_| ())?;
    uc.get_data_mut().file_pages.get_mut(&base).unwrap().mapped = true;
    Ok(())
}

/// A present, 32-bit, byte- or page-granular code/data segment descriptor.
fn descriptor(base: u32, limit: u32, code: bool, dpl: u64) -> u64 {
    let (limit, gran) = if limit > 0xfffff {
        (limit >> 12, 1u64)
    } else {
        (limit, 0)
    };
    let (base, limit) = (u64::from(base), u64::from(limit));
    let typ: u64 = if code { 0xb } else { 0x3 };
    let access = typ | 1 << 4 | dpl << 5 | 1 << 7;
    let flags = 1 << 2 | gran << 3; // D/B=32-bit, G
    (limit & 0xffff)
        | (base & 0xffff) << 16
        | (base >> 16 & 0xff) << 32
        | access << 40
        | (limit >> 16 & 0xf) << 48
        | flags << 52
        | (base >> 24) << 56
}

// Implemented on the raw engine so trap handlers can receive it directly as `&mut dyn Guest`.
impl Guest for Engine<'_> {
    fn fpu_environment(&mut self) -> Result<FpuEnvironment, FpuEnvironmentError> {
        let error = |e: uc_error| FpuEnvironmentError::Backend(e.to_string());
        Ok(FpuEnvironment {
            x87_control: self.reg_read(RegisterX86::FPCW).map_err(error)? as u16,
            mxcsr: self.reg_read(RegisterX86::MXCSR).map_err(error)? as u32,
        })
    }
    fn set_fpu_environment(
        &mut self,
        environment: FpuEnvironment,
    ) -> Result<(), FpuEnvironmentError> {
        environment.validate()?;
        let error = |e: uc_error| FpuEnvironmentError::Backend(e.to_string());
        let previous = self.fpu_environment()?;
        let status = self.reg_read(RegisterX86::FPSW).map_err(error)?;
        let pending = status & !u64::from(environment.x87_control) & 0x3f != 0;
        let updated_status = if pending {
            status | 0x8080
        } else {
            status & !0x8080
        };
        let result = (|| {
            self.reg_write(RegisterX86::FPCW, u64::from(environment.x87_control))?;
            self.reg_write(RegisterX86::MXCSR, u64::from(environment.mxcsr))?;
            self.reg_write(RegisterX86::FPSW, updated_status)
        })();
        if let Err(e) = result {
            // A valid register write is supported by this x86 engine. Preserve
            // the original environment if a backend error nevertheless occurs.
            let restored = self
                .reg_write(RegisterX86::FPCW, u64::from(previous.x87_control))
                .and_then(|()| self.reg_write(RegisterX86::MXCSR, u64::from(previous.mxcsr)))
                .and_then(|()| self.reg_write(RegisterX86::FPSW, status));
            return Err(FpuEnvironmentError::Backend(format!(
                "{e}; restore={restored:?}"
            )));
        }
        Ok(())
    }
    fn file_page_copy_on_write(&self, addr: u32) -> Option<bool> {
        self.get_data()
            .file_pages
            .get(&(addr & !4095))
            .map(|page| page.copy_source.is_some())
    }
    fn cpuid_features(&self) -> u32 {
        // The default Unicorn x86 model supports the legacy features exposed by PAL.
        (1 << 0) | (1 << 4) | (1 << 8) | (1 << 15) | (1 << 23) | (1 << 24) | (1 << 25) | (1 << 26)
    }
    fn push_f64(&mut self, value: f64) {
        let sw = self.reg_read(RegisterX86::FPSW).expect("x87 status");
        let top = ((sw >> 11).wrapping_sub(1)) & 7;
        self.reg_write(RegisterX86::FPSW, (sw & !(7 << 11)) | (top << 11))
            .expect("x87 top");
        let tag = self.reg_read(RegisterX86::FPTAG).expect("x87 tag");
        self.reg_write(
            RegisterX86::FPTAG,
            (tag & !(3 << (top * 2))) | (adv_cpu::backend::x87::tag_f64(value) << (top * 2)),
        )
        .expect("x87 tag");
        self.reg_write_long(RegisterX86::ST0, &adv_cpu::backend::x87::encode_f64(value))
            .expect("x87 return");
    }
    fn pop_f64(&mut self) -> Option<f64> {
        let sw = self.reg_read(RegisterX86::FPSW).expect("x87 status");
        let top = (sw >> 11) & 7;
        let tag = self.reg_read(RegisterX86::FPTAG).expect("x87 tag");
        if (tag >> (top * 2)) & 3 == 3 {
            return None;
        }
        let bytes = self.reg_read_long(RegisterX86::ST0).expect("x87 value");
        self.reg_write(RegisterX86::FPTAG, tag | (3 << (top * 2)))
            .expect("x87 tag");
        self.reg_write(
            RegisterX86::FPSW,
            (sw & !(7 << 11)) | (((top + 1) & 7) << 11),
        )
        .expect("x87 top");
        Some(adv_cpu::backend::x87::decode_f64(&bytes))
    }
    fn reg(&self, r: Reg) -> u32 {
        self.reg_read(uc_reg(r)).expect("reg read") as u32
    }
    fn set_reg(&mut self, r: Reg, v: u32) {
        self.reg_write(uc_reg(r), v.into()).expect("reg write");
    }
    fn read(&self, addr: u32, buf: &mut [u8]) -> Result<(), MemFault> {
        let fault = MemFault {
            addr,
            len: buf.len() as u32,
        };
        if u64::from(addr) + buf.len() as u64 > 1u64 << 32 {
            return Err(fault);
        }
        let mut offset = 0;
        while offset < buf.len() {
            let address = addr + offset as u32;
            let within = (address & 4095) as usize;
            let count = (4096 - within).min(buf.len() - offset);
            if let Some(binding) = self.get_data().file_pages.get(&(address & !4095)) {
                let page = binding.copy_source.as_ref().unwrap_or(&binding.page);
                page.read_into(within, &mut buf[offset..offset + count])
                    .map_err(|_| fault)?;
            } else {
                self.mem_read(address.into(), &mut buf[offset..offset + count])
                    .map_err(|_| fault)?;
            }
            offset += count;
        }
        Ok(())
    }
    fn write(&mut self, addr: u32, data: &[u8]) -> Result<(), MemFault> {
        let fault = MemFault {
            addr,
            len: data.len() as u32,
        };
        if u64::from(addr) + data.len() as u64 > 1u64 << 32
            || !self.accessible(addr, fault.len, Prot::default())
        {
            return Err(fault);
        }
        // Resolve all first-touch reads and COPY shadows before modifying any
        // page. Host writes bypass protection, but never mapping validation.
        let end = u64::from(addr) + data.len() as u64;
        for (_, binding) in self
            .get_data()
            .file_pages
            .range((addr & !4095)..)
            .take_while(|(page, _)| u64::from(**page) < end)
        {
            binding.prepare().map_err(|_| fault)?;
        }
        let mut offset = 0;
        while offset < data.len() {
            let address = addr + offset as u32;
            let within = (address & 4095) as usize;
            let count = (4096 - within).min(data.len() - offset);
            if let Some(binding) = self.get_data_mut().file_pages.get_mut(&(address & !4095)) {
                binding.commit_write();
                binding
                    .page
                    .write(within, &data[offset..offset + count])
                    .map_err(|_| fault)?;
            } else {
                self.mem_write(address.into(), &data[offset..offset + count])
                    .map_err(|_| fault)?;
            }
            offset += count;
        }
        // Host-side writes can patch an already translated instruction. Unicorn's
        // mem_write does not evict its translation cache; match the Guest write
        // contract of the production backend before resuming guest execution.
        if !data.is_empty() {
            self.ctl_remove_cache(u64::from(addr), end)
                .map_err(|_| fault)?;
        }
        Ok(())
    }
    fn map_file_pages(
        &mut self,
        addr: u32,
        pages: &[Rc<FilePage>],
        prot: Prot,
        copy: bool,
    ) -> Result<(), MemFault> {
        let size = pages
            .len()
            .checked_mul(4096)
            .and_then(|v| u32::try_from(v).ok())
            .ok_or(MemFault {
                addr,
                len: u32::MAX,
            })?;
        let fault = MemFault { addr, len: size };
        if !addr.is_multiple_of(4096)
            || size == 0
            || prot.0 & !3 != 0
            || u64::from(addr) + u64::from(size) > 1u64 << 32
        {
            return Err(fault);
        }
        let regions = self.mem_regions().map_err(|_| fault)?;
        if regions
            .iter()
            .any(|r| r.begin < u64::from(addr) + u64::from(size) && r.end >= u64::from(addr))
            || self
                .get_data()
                .file_pages
                .range(addr..)
                .next()
                .is_some_and(|(&p, _)| u64::from(p) < u64::from(addr) + u64::from(size))
        {
            return Err(fault);
        }
        for (index, backing) in pages.iter().enumerate() {
            let address = addr + index as u32 * 4096;
            let page = if copy {
                let source = backing.clone();
                FilePage::lazy(move || source.snapshot())
            } else {
                backing.clone()
            };
            self.get_data_mut().file_pages.insert(
                address,
                FileBinding {
                    page,
                    copy_source: copy.then(|| backing.clone()),
                    protection: prot,
                    mapped: false,
                },
            );
        }
        Ok(())
    }
    fn accessible(&self, addr: u32, len: u32, prot: Prot) -> bool {
        let end = u64::from(addr) + u64::from(len);
        if end > 1u64 << 32 {
            return false;
        }
        let Ok(regions) = self.mem_regions() else {
            return false;
        };
        let mut address = u64::from(addr);
        while address < end {
            let base = (address as u32) & !4095;
            let limit = (u64::from(base) + 4096).min(end);
            if let Some(binding) = self.get_data().file_pages.get(&base) {
                if !binding.protection.contains(prot) {
                    return false;
                }
            } else if !regions.iter().any(|r| {
                r.begin <= address
                    && r.end + 1 >= limit
                    && r.perms & u32::from(prot.0) == u32::from(prot.0)
            }) {
                return false;
            }
            address = limit;
        }
        true
    }
    fn map(&mut self, addr: u32, size: u32, prot: Prot) -> Result<(), MemFault> {
        if self
            .get_data()
            .file_pages
            .range(addr..)
            .next()
            .is_some_and(|(&p, _)| u64::from(p) < u64::from(addr) + u64::from(size))
        {
            return Err(MemFault { addr, len: size });
        }
        self.mem_map(addr.into(), size.into(), uc_prot(prot))
            .map_err(|_| MemFault { addr, len: size })
    }
    fn protect(&mut self, addr: u32, size: u32, prot: Prot) -> Result<(), MemFault> {
        let fault = MemFault { addr, len: size };
        if !addr.is_multiple_of(4096)
            || size == 0
            || !size.is_multiple_of(4096)
            || !self.accessible(addr, size, Prot::default())
        {
            return Err(fault);
        }
        if prot.contains(Prot::EXEC)
            && self
                .get_data()
                .file_pages
                .range(addr..)
                .next()
                .is_some_and(|(&p, _)| u64::from(p) < u64::from(addr) + u64::from(size))
        {
            return Err(fault);
        }
        for i in 0..size / 4096 {
            let page = addr + i * 4096;
            if let Some(binding) = self.get_data().file_pages.get(&page) {
                if binding.mapped {
                    self.mem_protect(page.into(), 4096, uc_prot(prot))
                        .map_err(|_| fault)?;
                }
                self.get_data_mut()
                    .file_pages
                    .get_mut(&page)
                    .unwrap()
                    .protection = prot;
            } else {
                self.mem_protect(page.into(), 4096, uc_prot(prot))
                    .map_err(|_| fault)?;
            }
        }
        Ok(())
    }
    fn unmap(&mut self, addr: u32, size: u32) -> Result<(), MemFault> {
        let fault = MemFault { addr, len: size };
        if !addr.is_multiple_of(4096)
            || size == 0
            || !size.is_multiple_of(4096)
            || !self.accessible(addr, size, Prot::default())
        {
            return Err(fault);
        }
        for i in 0..size / 4096 {
            let page = addr + i * 4096;
            if let Some(binding) = self.get_data().file_pages.get(&page) {
                if binding.mapped {
                    self.mem_unmap(page.into(), 4096).map_err(|_| fault)?;
                }
                self.get_data_mut().file_pages.remove(&page);
            } else {
                self.mem_unmap(page.into(), 4096).map_err(|_| fault)?;
            }
        }
        Ok(())
    }
}

impl Guest for UnicornCpu {
    fn fpu_environment(&mut self) -> Result<FpuEnvironment, FpuEnvironmentError> {
        self.uc.fpu_environment()
    }
    fn set_fpu_environment(
        &mut self,
        environment: FpuEnvironment,
    ) -> Result<(), FpuEnvironmentError> {
        self.uc.set_fpu_environment(environment)
    }
    fn file_page_copy_on_write(&self, addr: u32) -> Option<bool> {
        self.uc.file_page_copy_on_write(addr)
    }
    fn map_file_pages(
        &mut self,
        addr: u32,
        pages: &[Rc<FilePage>],
        prot: Prot,
        copy: bool,
    ) -> Result<(), MemFault> {
        self.uc.map_file_pages(addr, pages, prot, copy)
    }
    fn cpuid_features(&self) -> u32 {
        self.uc.cpuid_features()
    }
    fn push_f64(&mut self, value: f64) {
        self.uc.push_f64(value);
    }
    fn pop_f64(&mut self) -> Option<f64> {
        self.uc.pop_f64()
    }
    fn reg(&self, r: Reg) -> u32 {
        self.uc.reg(r)
    }
    fn set_reg(&mut self, r: Reg, v: u32) {
        self.uc.set_reg(r, v)
    }
    fn read(&self, addr: u32, buf: &mut [u8]) -> Result<(), MemFault> {
        self.uc.read(addr, buf)
    }
    fn write(&mut self, addr: u32, data: &[u8]) -> Result<(), MemFault> {
        self.uc.write(addr, data)
    }
    fn accessible(&self, addr: u32, len: u32, prot: Prot) -> bool {
        self.uc.accessible(addr, len, prot)
    }
    fn map(&mut self, addr: u32, size: u32, prot: Prot) -> Result<(), MemFault> {
        self.uc.map(addr, size, prot)
    }
    fn protect(&mut self, addr: u32, size: u32, prot: Prot) -> Result<(), MemFault> {
        self.uc.protect(addr, size, prot)
    }
    fn unmap(&mut self, addr: u32, size: u32) -> Result<(), MemFault> {
        self.uc.unmap(addr, size)
    }
}

pub struct UcContext(unicorn_engine::Context);

impl Backend for UnicornCpu {
    type Context = UcContext;

    fn setup_segments(&mut self, seg: SegmentSetup) -> Result<(), MemFault> {
        self.gdt = Some(seg.gdt_addr);
        let zero = [0u8; (sel::GDT_ENTRIES * 8) as usize];
        self.write(seg.gdt_addr, &zero)?;
        self.write_gdt_entry(u32::from(sel::CS >> 3), descriptor(0, 0xffff_ffff, true, 3))?;
        self.write_gdt_entry(
            u32::from(sel::DS >> 3),
            descriptor(0, 0xffff_ffff, false, 3),
        )?;
        self.write_gdt_entry(
            u32::from(sel::SS >> 3),
            descriptor(0, 0xffff_ffff, false, 0),
        )?;
        self.write_gdt_entry(u32::from(sel::FS >> 3), descriptor(0, 0xfff, false, 3))?;

        // struct uc_x86_mmr { u16 selector; u64 base; u32 limit; u32 flags; } (C layout, 24 bytes)
        let mut mmr = [0u8; 24];
        mmr[8..16].copy_from_slice(&u64::from(seg.gdt_addr).to_le_bytes());
        mmr[16..20].copy_from_slice(&(sel::GDT_ENTRIES * 8 - 1).to_le_bytes());
        let fault = MemFault {
            addr: seg.gdt_addr,
            len: sel::GDT_ENTRIES * 8,
        };
        self.uc
            .reg_write_long(RegisterX86::GDTR, &mmr)
            .map_err(|_| fault)?;
        for (r, s) in [
            (RegisterX86::SS, sel::SS),
            (RegisterX86::CS, sel::CS),
            (RegisterX86::DS, sel::DS),
            (RegisterX86::ES, sel::DS),
            (RegisterX86::FS, sel::FS),
        ] {
            self.uc.reg_write(r, s.into()).map_err(|_| fault)?;
        }
        Ok(())
    }

    fn set_fs_base(&mut self, base: u32) -> Result<(), MemFault> {
        self.write_gdt_entry(u32::from(sel::FS >> 3), descriptor(base, 0xfff, false, 3))?;
        // Reloading the selector refreshes the hidden base from the GDT.
        self.uc
            .reg_write(RegisterX86::FS, sel::FS.into())
            .map_err(|_| MemFault {
                addr: base,
                len: 0x1000,
            })
    }

    fn park_addr(&self) -> u32 {
        self.park
    }

    fn new_context(&self) -> UcContext {
        UcContext(self.uc.context_alloc().expect("context alloc"))
    }

    fn save(&self, ctx: &mut UcContext) {
        self.uc.context_save(&mut ctx.0).expect("context save");
    }

    fn restore(&mut self, ctx: &UcContext) {
        self.uc.context_restore(&ctx.0).expect("context restore");
    }

    fn set_execution_budget(&mut self, budget: Option<Duration>) {
        self.budget = budget;
        if budget.is_none() {
            self.watchdog = None;
        }
    }

    fn execution_budget(&self) -> Option<Duration> {
        self.budget
    }

    fn run(&mut self, handler: &mut dyn TrapHandler) -> Exit {
        // SAFETY: erases the handler's lifetime for storage in HookData; it is cleared again
        // before this function returns, so the pointer never outlives the borrow.
        let h: *mut (dyn TrapHandler + 'static) = unsafe { std::mem::transmute(handler) };
        let d = self.uc.get_data_mut();
        d.handler = Some(h);
        d.last_mem_fault = None;
        d.budget_active = false;
        let pc = self.uc.reg(Reg::Eip);
        if let Some(budget) = self.budget {
            if budget.is_zero() {
                self.uc.get_data_mut().handler = None;
                return Exit::BudgetExceeded { pc, budget };
            }
            let flag = self.uc.get_data().budget_expired.clone();
            let watchdog = self.watchdog.get_or_insert_with(|| Watchdog::new(flag));
            watchdog.arm(budget);
            self.uc.get_data_mut().budget_active = true;
        }
        let res = self.uc.emu_start(pc.into(), self.park.into(), 0, 0);
        let expired = self.watchdog.as_ref().is_some_and(Watchdog::disarm);
        self.uc.get_data_mut().handler = None;
        self.uc.get_data_mut().budget_active = false;
        let pc = self.uc.reg(Reg::Eip);
        match res {
            Ok(()) if pc == self.park => Exit::Parked,
            Ok(()) if expired => Exit::BudgetExceeded {
                pc,
                budget: self.budget.unwrap(),
            },
            Ok(()) => Exit::Fault(Fault {
                kind: FaultKind::Other("emulation stopped".into()),
                pc,
            }),
            Err(e) => {
                let addr = self
                    .uc
                    .get_data()
                    .last_mem_fault
                    .map(|(_, a)| a)
                    .unwrap_or(0);
                let kind = match e {
                    uc_error::READ_UNMAPPED | uc_error::READ_PROT | uc_error::READ_UNALIGNED => {
                        FaultKind::Read(addr)
                    }
                    uc_error::WRITE_UNMAPPED | uc_error::WRITE_PROT | uc_error::WRITE_UNALIGNED => {
                        FaultKind::Write(addr)
                    }
                    uc_error::FETCH_UNMAPPED | uc_error::FETCH_PROT | uc_error::FETCH_UNALIGNED => {
                        FaultKind::Fetch(addr)
                    }
                    uc_error::INSN_INVALID => FaultKind::InvalidInstruction,
                    other => FaultKind::Other(format!("{other:?}")),
                };
                Exit::Fault(Fault { kind, pc })
            }
        }
    }

    fn invalidate_code(&mut self, addr: u32, len: u32) {
        let _ = self
            .uc
            .ctl_remove_cache(addr.into(), u64::from(addr) + u64::from(len));
    }

    fn set_block_counting(&mut self, on: bool) {
        self.uc.get_data_mut().count_blocks = on;
        self.counted |= on;
    }

    fn executed_blocks(&self) -> Option<(u64, u64)> {
        self.counted.then(|| self.uc.get_data().blocks)
    }

    fn set_block_profile(&mut self, on: bool) {
        let p = &mut self.uc.get_data_mut().profile;
        if on {
            p.get_or_insert_with(HashMap::new);
        } else {
            *p = None;
        }
        self.set_block_counting(on);
    }

    fn block_profile(&self) -> Vec<(u32, u64, u32)> {
        let p = self.uc.get_data().profile.iter().flatten();
        p.map(|(&addr, &(n, size))| (addr, n, size)).collect()
    }
}

#[cfg(test)]
mod file_page_tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn cross_page_host_write_is_atomic_on_lazy_io_failure_and_retries() {
        for copy in [false, true] {
            let fail = Rc::new(Cell::new(true));
            let injected = fail.clone();
            let first = FilePage::new([0x11; 4096]);
            let second = FilePage::lazy(move || {
                if injected.get() {
                    Err("transient read failure".into())
                } else {
                    Ok([0x22; 4096])
                }
            });
            let mut cpu = UnicornCpu::new(0x8000).unwrap();
            cpu.map_file_pages(
                0x10000,
                &[first.clone(), second.clone()],
                Prot::READ | Prot::WRITE,
                copy,
            )
            .unwrap();
            assert!(cpu.write(0x10ffe, &[1, 2, 3, 4]).is_err());
            let mut prefix = [0; 2];
            cpu.read(0x10ffe, &mut prefix).unwrap();
            assert_eq!(prefix, [0x11; 2]);
            assert_eq!(cpu.file_page_copy_on_write(0x10000), Some(copy));
            assert_eq!(cpu.file_page_copy_on_write(0x11000), Some(copy));
            assert!(!first.dirty());
            assert!(!second.dirty());
            fail.set(false);
            cpu.write(0x10ffe, &[1, 2, 3, 4]).unwrap();
            let mut output = [0; 4];
            cpu.read(0x10ffe, &mut output).unwrap();
            assert_eq!(output, [1, 2, 3, 4]);
            assert_eq!(
                &first.snapshot().unwrap()[4094..],
                if copy { &[0x11, 0x11] } else { &[1, 2] }
            );
        }
    }

    #[test]
    fn host_write_validates_later_ordinary_mapping_before_detaching_copy_page() {
        let page = FilePage::new([0x55; 4096]);
        let mut cpu = UnicornCpu::new(0x8000).unwrap();
        cpu.map_file_pages(0x10000, &[page], Prot::READ | Prot::WRITE, true)
            .unwrap();
        assert!(cpu.write(0x10ffe, &[1, 2, 3, 4]).is_err());
        assert_eq!(cpu.file_page_copy_on_write(0x10000), Some(true));
        let mut prefix = [0; 2];
        cpu.read(0x10ffe, &mut prefix).unwrap();
        assert_eq!(prefix, [0x55; 2]);
    }
}
