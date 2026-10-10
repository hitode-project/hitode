//! Test-only Unicorn oracle. Production execution never imports this module.
#![allow(dead_code)]

mod registers;

use adv_cpu::{CpuState, Memory, RunOutcome};
use adv_unicorn::Guarded;
use std::collections::{BTreeMap, BTreeSet};
use std::ops::{Deref, DerefMut};
use unicorn_engine::{
    RegisterX86 as R,
    unicorn_const::{Arch, HookType, MemType, Mode, Prot, uc_error},
};

const GDT: u32 = 0xffff_0000;
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

#[derive(Default)]
struct HookData {
    fault: Option<(MemType, u32)>,
    trap: Option<u32>,
    dirty: BTreeSet<u32>,
}

/// An independent reference executor with its own memory and architectural state.
pub struct Oracle {
    // Unicorn must drop before the stable allocation used by mem_map_ptr.
    uc: Guarded<'static, HookData>,
    memory: Memory,
    pub state: CpuState,
    pages: BTreeMap<u32, u8>,
    fpu: registers::FpuAdapter,
}

fn error(e: impl std::fmt::Display) -> String {
    e.to_string()
}
fn protection(bits: u8) -> Prot {
    let mut result = Prot::NONE;
    for (bit, permission) in [(1, Prot::READ), (2, Prot::WRITE), (4, Prot::EXEC)] {
        if bits & bit != 0 {
            result |= permission;
        }
    }
    result
}
fn descriptor(base: u32, code: bool) -> u64 {
    let base = u64::from(base);
    0xffff
        | ((base & 0xffff) << 16)
        | (((base >> 16) & 0xff) << 32)
        | ((if code { 0x9b } else { 0x93 }) << 40)
        | (0xcf << 48)
        | ((base >> 24) << 56)
}

impl Oracle {
    pub fn new(base: u32, len: usize) -> Result<Self, String> {
        Self::create(base, len, false)
    }
    pub fn address_space() -> Result<Self, String> {
        Self::create(0, GDT as usize, true)
    }
    fn create(base: u32, len: usize, sparse: bool) -> Result<Self, String> {
        let memory = if sparse {
            Memory::reserve(base, len)?
        } else {
            Memory::new(base, len)?
        };
        let mut uc =
            Guarded::new_with_data(Arch::X86, Mode::MODE_32, HookData::default()).map_err(error)?;
        let mut pages = BTreeMap::new();
        if !sparse {
            let pointer = memory.read(base, len)?.as_ptr().cast_mut().cast();
            // SAFETY: Oracle owns this stable allocation until after its engine drops.
            unsafe { uc.mem_map_ptr(u64::from(base), len as u64, Prot::NONE, pointer) }
                .map_err(error)?;
            for offset in (0..len).step_by(4096) {
                pages.insert(base + offset as u32, 0);
            }
        }
        uc.mem_map(u64::from(GDT), 4096, Prot::READ | Prot::WRITE)
            .map_err(error)?;
        let mut gdt = [0; 32];
        for (slot, code) in [(1, true), (2, false), (3, false)] {
            gdt[slot * 8..slot * 8 + 8].copy_from_slice(&descriptor(0, code).to_le_bytes());
        }
        uc.mem_write(u64::from(GDT), &gdt).map_err(error)?;
        let mut gdtr = [0; 24];
        gdtr[8..16].copy_from_slice(&u64::from(GDT).to_le_bytes());
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
        uc.add_intr_hook(|uc, vector| {
            uc.get_data_mut().trap = Some(vector);
            uc.emu_stop().unwrap();
        })
        .map_err(error)?;
        let bad = HookType::MEM_READ_UNMAPPED
            | HookType::MEM_WRITE_UNMAPPED
            | HookType::MEM_FETCH_UNMAPPED
            | HookType::MEM_READ_PROT
            | HookType::MEM_WRITE_PROT
            | HookType::MEM_FETCH_PROT;
        uc.add_mem_hook(bad, 1, 0, |uc, kind, address, _, _| {
            uc.get_data_mut().fault = Some((kind, address as u32));
            false
        })
        .map_err(error)?;
        uc.add_mem_hook(HookType::MEM_WRITE, 1, 0, |uc, _, address, size, _| {
            let first = address as u32 & !4095;
            let last = (address + size.saturating_sub(1) as u64) as u32 & !4095;
            for page in (first..=last).step_by(4096) {
                uc.get_data_mut().dirty.insert(page);
            }
            false
        })
        .map_err(error)?;
        Ok(Self {
            uc,
            memory,
            state: CpuState::default(),
            pages,
            fpu: registers::FpuAdapter::default(),
        })
    }
    pub fn write(&mut self, address: u32, bytes: &[u8]) -> Result<(), String> {
        self.memory.write(address, bytes)?;
        if !bytes.is_empty() {
            self.uc
                .ctl_remove_cache(u64::from(address), u64::from(address) + bytes.len() as u64)
                .map_err(error)?;
        }
        Ok(())
    }
    pub fn protect(&mut self, address: u32, len: usize, bits: u8) -> Result<(), String> {
        self.memory.protect(address, len, bits)?;
        self.uc
            .mem_protect(u64::from(address), len as u64, protection(bits))
            .map_err(error)?;
        for offset in (0..len).step_by(4096) {
            self.pages.insert(address + offset as u32, bits);
        }
        Ok(())
    }
    pub fn map(&mut self, address: u32, len: usize, bits: u8) -> Result<(), String> {
        self.memory.map(address, len, bits)?;
        let pointer = self.memory.read(address, len)?.as_ptr().cast_mut().cast();
        // SAFETY: the committed mapping remains stable for Oracle's lifetime.
        unsafe {
            self.uc
                .mem_map_ptr(u64::from(address), len as u64, protection(bits), pointer)
        }
        .map_err(error)?;
        for offset in (0..len).step_by(4096) {
            self.pages.insert(address + offset as u32, bits);
        }
        Ok(())
    }
    pub fn unmap(&mut self, address: u32, len: usize) -> Result<(), String> {
        self.uc
            .mem_unmap(u64::from(address), len as u64)
            .map_err(error)?;
        self.memory.unmap(address, len)?;
        for offset in (0..len).step_by(4096) {
            self.pages.remove(&(address + offset as u32));
        }
        Ok(())
    }
    fn export_cpu(&mut self) -> Result<(), String> {
        self.fpu
            .export(&mut self.uc, &mut self.state)
            .map_err(error)?;
        for (reg, value) in XMM.into_iter().zip(self.state.xmm) {
            self.uc.reg_write_long(reg, &value).map_err(error)?;
        }
        for (reg, value) in REGS.into_iter().zip(self.state.regs) {
            self.uc.reg_write(reg, u64::from(value)).map_err(error)?;
        }
        self.uc
            .reg_write(R::EIP, u64::from(self.state.eip))
            .map_err(error)?;
        self.uc
            .reg_write(R::EFLAGS, u64::from(self.state.eflags))
            .map_err(error)?;
        self.uc
            .mem_write(
                u64::from(GDT) + 24,
                &descriptor(self.state.fs_base, false).to_le_bytes(),
            )
            .map_err(error)?;
        self.uc.reg_write(R::FS, 24).map_err(error)
    }
    fn import_cpu(&mut self) -> Result<(), String> {
        self.fpu.import(&self.uc, &mut self.state).map_err(error)?;
        for (reg, value) in REGS.into_iter().zip(&mut self.state.regs) {
            *value = self.uc.reg_read(reg).map_err(error)? as u32;
        }
        self.state.eip = self.uc.reg_read(R::EIP).map_err(error)? as u32;
        self.state.eflags = self.uc.reg_read(R::EFLAGS).map_err(error)? as u32;
        Ok(())
    }
    fn result(&mut self, result: Result<(), uc_error>, stop: u32) -> Result<RunOutcome, String> {
        if let Some((kind, address)) = self.uc.get_data_mut().fault.take() {
            let pc = self.state.eip;
            return Ok(match kind {
                MemType::READ_UNMAPPED | MemType::READ_PROT => {
                    RunOutcome::ReadFault { pc, address }
                }
                MemType::WRITE_UNMAPPED | MemType::WRITE_PROT => {
                    RunOutcome::WriteFault { pc, address }
                }
                _ => RunOutcome::FetchFault { pc, address },
            });
        }
        if let Err(e) = result {
            if e == uc_error::INSN_INVALID {
                return Ok(RunOutcome::InvalidInstruction { pc: self.state.eip });
            }
            if e != uc_error::EXCEPTION || self.uc.get_data().trap.is_none() {
                return Err(error(e));
            }
        }
        if let Some(vector) = self.uc.get_data().trap {
            return Ok(RunOutcome::Trap {
                pc: self.state.eip,
                vector: vector as u8,
            });
        }
        Ok(if self.state.eip == stop {
            RunOutcome::ReachedStop
        } else {
            RunOutcome::Budget
        })
    }
    pub fn run_unicorn(&mut self, stop: u32, budget: usize) -> Result<RunOutcome, String> {
        self.uc.get_data_mut().fault = None;
        self.uc.get_data_mut().trap = None;
        self.export_cpu()?;
        let result = self
            .uc
            .emu_start(u64::from(self.state.eip), u64::from(stop), 0, budget);
        self.import_cpu()?;
        self.result(result, stop)
    }
    pub fn run_unicorn_unbounded(&mut self, stop: u32) -> Result<RunOutcome, String> {
        self.run_unicorn(stop, 0)
    }
}

/// Asserts about the x64 engine's execution shape (native entries, compiled
/// units, bulk counters). Skipped when the tests drive the a64 engine, so the
/// semantic checks around them still run there.
#[allow(unused_macros)]
macro_rules! x64_shape {
    ($($body:tt)*) => {
        // Still type-checked under a64, so its operands stay in use.
        if !cfg!(all(test, feature = "test-a64")) {
            $($body)*
        }
    };
}
#[allow(unused_imports)]
pub(crate) use x64_shape;

/// The engine under test; `--features test-a64` selects the a64 executor for
/// integration tests. Examples share this module and keep the x64 executor.
#[cfg(not(all(test, feature = "test-a64")))]
pub type Engine = adv_cpu::Executor;
#[cfg(all(test, feature = "test-a64"))]
pub type Engine = adv_cpu_a64::Executor;

/// Keeps existing differential fixtures' construction API while isolating UC entirely
/// in their dev-only reference. Ordinary run dereferences to production.
pub struct Executor {
    inner: Engine,
    oracle: Oracle,
    oracle_needs_sync: bool,
}
impl Deref for Executor {
    type Target = Engine;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}
impl DerefMut for Executor {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}
impl Executor {
    pub fn new(base: u32, len: usize) -> Result<Self, String> {
        Ok(Self {
            inner: Engine::new(base, len)?,
            oracle: Oracle::new(base, len)?,
            oracle_needs_sync: false,
        })
    }
    pub fn address_space() -> Result<Self, String> {
        Ok(Self {
            inner: Engine::address_space()?,
            oracle: Oracle::address_space()?,
            oracle_needs_sync: false,
        })
    }
    pub fn write(&mut self, address: u32, bytes: &[u8]) -> Result<(), String> {
        self.inner.write(address, bytes)?;
        self.oracle.write(address, bytes)
    }
    pub fn protect(&mut self, address: u32, len: usize, bits: u8) -> Result<(), String> {
        self.inner.protect(address, len, bits)?;
        self.oracle.protect(address, len, bits)
    }
    pub fn map(&mut self, address: u32, len: usize, bits: u8) -> Result<(), String> {
        self.inner.map(address, len, bits)?;
        self.oracle.map(address, len, bits)
    }
    pub fn unmap(&mut self, address: u32, len: usize) -> Result<(), String> {
        self.inner.unmap(address, len)?;
        self.oracle.unmap(address, len)
    }
    pub fn run(&mut self, stop: u32, budget: u64) -> Result<RunOutcome, String> {
        self.oracle_needs_sync = true;
        self.inner.run(stop, budget)
    }
    fn before_oracle(&mut self) -> Result<(), String> {
        if self.oracle_needs_sync {
            for &page in self.oracle.pages.keys() {
                let current = self.inner.memory().read(page, 4096)?;
                if current != self.oracle.memory.read(page, 4096)? {
                    self.oracle.memory.write(page, current)?;
                    self.oracle
                        .uc
                        .ctl_remove_cache(u64::from(page), u64::from(page) + 4096)
                        .map_err(error)?;
                }
            }
            self.oracle_needs_sync = false;
        }
        self.oracle.state = self.inner.state.clone();
        Ok(())
    }
    fn after_oracle(&mut self) -> Result<(), String> {
        self.inner.state = self.oracle.state.clone();
        for page in std::mem::take(&mut self.oracle.uc.get_data_mut().dirty) {
            if self.oracle.pages.contains_key(&page) {
                self.inner
                    .write(page, self.oracle.memory.read(page, 4096)?)?;
            }
        }
        Ok(())
    }
    pub fn run_unicorn(&mut self, stop: u32, budget: usize) -> Result<RunOutcome, String> {
        self.before_oracle()?;
        let result = self.oracle.run_unicorn(stop, budget);
        self.after_oracle()?;
        result
    }
    pub fn run_unicorn_unbounded(&mut self, stop: u32) -> Result<RunOutcome, String> {
        self.before_oracle()?;
        let result = self.oracle.run_unicorn_unbounded(stop);
        self.after_oracle()?;
        result
    }
}

/// x64 tuning has no a64 counterpart; fixtures that set it keep compiling.
#[cfg(all(test, feature = "test-a64"))]
impl Executor {
    pub fn enable_auto_jit(&mut self, _enabled: bool) {}
    pub fn set_region_linking(&mut self, _enabled: bool) {}
    pub fn set_tier_verification(&mut self, _mode: Option<bool>) {}
    pub fn set_profiling(&mut self, _enabled: bool) {}
    pub fn set_auto_compile_strategy(&mut self, _strategy: adv_cpu::CompileStrategy) {}
}
