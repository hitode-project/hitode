//! Adapter to the existing PE loader, Win32 handlers and cooperative scheduler.
use crate::machine::{HookData, uc_prot};
use crate::{CompileStrategy, CpuState, ExecutionContext, Machine, Memory, RunOutcome, Stats};
use pal_cpu::{
    Backend, Exit, Fault, FaultKind, Guest, MemFault, Prot, Reg, SegmentSetup, TrapAction,
    TrapHandler,
};
use std::cell::{Ref, RefCell, RefMut};
use std::time::{Duration, Instant};
use unicorn_engine::{RegisterX86 as R, Unicorn};

pub struct JitCpu {
    inner: RefCell<Machine>,
    park: u32,
    budget: Option<Duration>,
    jit: bool,
}
impl JitCpu {
    pub fn new(park: u32, jit: bool) -> Result<Self, String> {
        let mut machine = Machine::address_space()?;
        machine.enable_auto_jit(jit);
        Ok(Self {
            inner: RefCell::new(machine),
            park,
            budget: Some(Duration::from_secs(5)),
            jit,
        })
    }
    pub fn stats(&self) -> Stats {
        self.inner.borrow().stats.clone()
    }
    pub fn set_profiling(&mut self, enabled: bool) {
        self.inner.get_mut().set_profiling(enabled);
    }
    /// Choose the compilation policy before running automatically discovered code.
    pub fn set_auto_compile_strategy(&mut self, strategy: CompileStrategy) {
        self.inner.get_mut().set_auto_compile_strategy(strategy);
    }
    pub fn set_x87_residency(&mut self, enabled: bool) {
        self.inner.get_mut().set_x87_residency(enabled);
    }
    pub fn profile(&self) -> std::collections::BTreeMap<u32, crate::ProfileSample> {
        self.inner.borrow().profile.clone()
    }
    pub fn machine(&self) -> Ref<'_, Machine> {
        self.inner.borrow()
    }
    pub fn machine_mut(&mut self) -> RefMut<'_, Machine> {
        self.inner.borrow_mut()
    }
    fn view<T>(&mut self, f: impl FnOnce(&mut View<'_>) -> T) -> T {
        let m = self.inner.get_mut();
        let Machine {
            state, memory, uc, ..
        } = m;
        f(&mut View { state, memory, uc })
    }
}

struct View<'a> {
    state: &'a mut CpuState,
    memory: &'a mut Memory,
    uc: &'a mut Unicorn<'static, HookData>,
}
fn fault(addr: u32, len: usize) -> MemFault {
    MemFault {
        addr,
        len: len.min(u32::MAX as usize) as u32,
    }
}
fn get(s: &CpuState, r: Reg) -> u32 {
    match r {
        Reg::Eip => s.eip,
        Reg::Eflags => s.eflags,
        _ => s.regs[r as usize],
    }
}
fn set(s: &mut CpuState, r: Reg, v: u32) {
    match r {
        Reg::Eip => s.eip = v,
        Reg::Eflags => s.eflags = v,
        _ => s.regs[r as usize] = v,
    }
}

impl Guest for View<'_> {
    fn shared_bytes(&self, addr: u32, len: usize) -> Option<&[u8]> {
        self.memory.read(addr, len).ok()
    }
    fn copy_memory(&mut self, src: u32, dst: u32, len: usize) -> Result<(), MemFault> {
        crate::ApiMemory { inner: self.memory }
            .copy(src, dst, len)
            .map_err(|_| fault(dst, len))
    }
    fn push_f64(&mut self, value: f64) {
        let sw = self.uc.reg_read(R::FPSW).expect("x87 status");
        let top = ((sw >> 11).wrapping_sub(1)) & 7;
        self.uc
            .reg_write(R::FPSW, (sw & !(7 << 11)) | (top << 11))
            .expect("x87 top");
        let tag = self.uc.reg_read(R::FPTAG).expect("x87 tag");
        self.uc
            .reg_write(
                R::FPTAG,
                (tag & !(3 << (top * 2))) | (pal_cpu::x87::tag_f64(value) << (top * 2)),
            )
            .expect("x87 tag");
        self.uc
            .reg_write_long(R::ST0, &pal_cpu::x87::encode_f64(value))
            .expect("x87 result");
    }
    fn pop_f64(&mut self) -> Option<f64> {
        let sw = self.uc.reg_read(R::FPSW).expect("x87 status");
        let top = (sw >> 11) & 7;
        let tag = self.uc.reg_read(R::FPTAG).expect("x87 tag");
        if (tag >> (top * 2)) & 3 == 3 {
            return None;
        }
        let bytes = self.uc.reg_read_long(R::ST0).expect("x87 value");
        self.uc
            .reg_write(R::FPTAG, tag | (3 << (top * 2)))
            .expect("x87 tag");
        self.uc
            .reg_write(R::FPSW, (sw & !(7 << 11)) | (((top + 1) & 7) << 11))
            .expect("x87 top");
        Some(pal_cpu::x87::decode_f64(&bytes))
    }
    fn reg(&self, r: Reg) -> u32 {
        get(self.state, r)
    }
    fn set_reg(&mut self, r: Reg, v: u32) {
        set(self.state, r, v)
    }
    fn read(&self, addr: u32, buf: &mut [u8]) -> Result<(), MemFault> {
        buf.copy_from_slice(
            self.memory
                .read(addr, buf.len())
                .map_err(|_| fault(addr, buf.len()))?,
        );
        Ok(())
    }
    fn write(&mut self, addr: u32, data: &[u8]) -> Result<(), MemFault> {
        self.memory
            .write(addr, data)
            .map_err(|_| fault(addr, data.len()))
    }
    fn accessible(&self, addr: u32, len: u32, prot: Prot) -> bool {
        let Ok(r) = self.memory.committed(addr, len as usize) else {
            return false;
        };
        r.is_empty()
            || self.memory.perms[r.start / 4096..=(r.end - 1) / 4096]
                .iter()
                .all(|p| p & prot.0 == prot.0)
    }
    fn map(&mut self, addr: u32, size: u32, prot: Prot) -> Result<(), MemFault> {
        let err = fault(addr, size as usize);
        self.memory
            .map(addr, size as usize, prot.0)
            .map_err(|_| err)?;
        // SAFETY: the new committed range stays at this address for its whole mapping.
        if unsafe {
            self.uc.mem_map_ptr(
                addr.into(),
                size.into(),
                uc_prot(prot.0),
                self.memory
                    .ptr
                    .as_ptr()
                    .add((addr - self.memory.base) as usize)
                    .cast(),
            )
        }
        .is_err()
        {
            let _ = self.memory.unmap(addr, size as usize);
            return Err(err);
        }
        self.uc.get_data_mut().perms.clone_from(&self.memory.perms);
        Ok(())
    }
    fn protect(&mut self, addr: u32, size: u32, prot: Prot) -> Result<(), MemFault> {
        let err = fault(addr, size as usize);
        self.memory.pages(addr, size as usize).map_err(|_| err)?;
        self.memory
            .committed(addr, size as usize)
            .map_err(|_| err)?;
        if prot.0 & !7 != 0 {
            return Err(err);
        }
        self.uc
            .mem_protect(addr.into(), size.into(), uc_prot(prot.0))
            .map_err(|_| err)?;
        self.memory
            .protect(addr, size as usize, prot.0)
            .map_err(|_| err)?;
        self.uc.get_data_mut().perms.clone_from(&self.memory.perms);
        Ok(())
    }
    fn unmap(&mut self, addr: u32, size: u32) -> Result<(), MemFault> {
        let err = fault(addr, size as usize);
        self.memory.pages(addr, size as usize).map_err(|_| err)?;
        self.memory
            .committed(addr, size as usize)
            .map_err(|_| err)?;
        self.uc
            .mem_unmap(addr.into(), size.into())
            .map_err(|_| err)?;
        self.memory.unmap(addr, size as usize).map_err(|_| err)?;
        self.uc.get_data_mut().perms.clone_from(&self.memory.perms);
        Ok(())
    }
}

impl Guest for JitCpu {
    fn push_f64(&mut self, v: f64) {
        let m = self.inner.get_mut();
        crate::machine::export_mmx(&mut m.uc, &mut m.state).expect("MMX/x87 boundary");
        self.view(|g| g.push_f64(v));
        let m = self.inner.get_mut();
        crate::machine::import_mmx(&m.uc, &mut m.state).expect("MMX/x87 result");
    }
    fn pop_f64(&mut self) -> Option<f64> {
        let m = self.inner.get_mut();
        crate::machine::export_mmx(&mut m.uc, &mut m.state).expect("MMX/x87 boundary");
        let value = self.view(|g| g.pop_f64());
        let m = self.inner.get_mut();
        crate::machine::import_mmx(&m.uc, &mut m.state).expect("MMX/x87 result");
        value
    }
    fn reg(&self, r: Reg) -> u32 {
        get(&self.inner.borrow().state, r)
    }
    fn set_reg(&mut self, r: Reg, v: u32) {
        set(&mut self.inner.get_mut().state, r, v)
    }
    fn read(&self, addr: u32, buf: &mut [u8]) -> Result<(), MemFault> {
        buf.copy_from_slice(
            self.inner
                .borrow()
                .memory
                .read(addr, buf.len())
                .map_err(|_| fault(addr, buf.len()))?,
        );
        Ok(())
    }
    fn write(&mut self, addr: u32, b: &[u8]) -> Result<(), MemFault> {
        self.view(|g| g.write(addr, b))
    }
    fn accessible(&self, addr: u32, len: u32, prot: Prot) -> bool {
        let m = self.inner.borrow();
        let Ok(r) = m.memory.committed(addr, len as usize) else {
            return false;
        };
        r.is_empty()
            || m.memory.perms[r.start / 4096..=(r.end - 1) / 4096]
                .iter()
                .all(|p| p & prot.0 == prot.0)
    }
    fn map(&mut self, addr: u32, size: u32, prot: Prot) -> Result<(), MemFault> {
        self.view(|g| g.map(addr, size, prot))?;
        self.inner
            .get_mut()
            .update_write_hook()
            .map_err(|_| fault(addr, size as usize))
    }
    fn protect(&mut self, addr: u32, size: u32, prot: Prot) -> Result<(), MemFault> {
        self.view(|g| g.protect(addr, size, prot))?;
        self.inner
            .get_mut()
            .update_write_hook()
            .map_err(|_| fault(addr, size as usize))
    }
    fn unmap(&mut self, addr: u32, size: u32) -> Result<(), MemFault> {
        self.view(|g| g.unmap(addr, size))?;
        self.inner
            .get_mut()
            .update_write_hook()
            .map_err(|_| fault(addr, size as usize))
    }
}

struct Raw<'a> {
    handler: &'a mut dyn TrapHandler,
    uc: *mut Unicorn<'static, HookData>,
}
impl crate::host::RawTrap for Raw<'_> {
    fn bind(&mut self, uc: *mut Unicorn<'static, HookData>) {
        self.uc = uc;
    }
    fn call(
        &mut self,
        state: &mut CpuState,
        memory: &mut Memory,
        vector: u32,
    ) -> Result<bool, String> {
        // SAFETY: uc is a disjoint Machine field; Unicorn is stopped at every raw call.
        let uc = unsafe { &mut *self.uc };
        crate::machine::export_mmx(uc, state)?;
        let action = self
            .handler
            .on_trap(&mut View { state, memory, uc }, vector as u8);
        crate::machine::import_mmx(uc, state)?;
        Ok(action == TrapAction::Park)
    }
}

impl Backend for JitCpu {
    type Context = Option<ExecutionContext>;
    fn setup_segments(&mut self, seg: SegmentSetup) -> Result<(), MemFault> {
        self.inner
            .get_mut()
            .setup_wow64_segments(seg.gdt_addr)
            .map_err(|_| fault(seg.gdt_addr, 4096))
    }
    fn set_fs_base(&mut self, base: u32) -> Result<(), MemFault> {
        self.inner.get_mut().state.fs_base = base;
        Ok(())
    }
    fn park_addr(&self) -> u32 {
        self.park
    }
    fn new_context(&self) -> Self::Context {
        None
    }
    fn save(&self, c: &mut Self::Context) {
        *c = Some(
            self.inner
                .borrow_mut()
                .save_context()
                .expect("save complete guest context"),
        );
    }
    fn restore(&mut self, c: &Self::Context) {
        self.inner
            .get_mut()
            .restore_context(c.as_ref().expect("initialized context"))
            .expect("restore complete guest context");
    }
    fn set_execution_budget(&mut self, b: Option<Duration>) {
        self.budget = b;
    }
    fn execution_budget(&self) -> Option<Duration> {
        self.budget
    }
    fn run(&mut self, handler: &mut dyn TrapHandler) -> Exit {
        let start = Instant::now();
        self.inner.get_mut().deadline = self.budget.and_then(|b| start.checked_add(b));
        loop {
            let m = self.inner.get_mut();
            if let Some(budget) = self.budget
                && start.elapsed() >= budget
            {
                return Exit::BudgetExceeded {
                    pc: m.state.eip,
                    budget,
                };
            }
            let mut raw = Raw {
                handler,
                uc: std::ptr::null_mut(),
            };
            let result = m.run_traps(self.park, 32_768, &mut raw, !self.jit);
            match result {
                Ok(RunOutcome::Parked | RunOutcome::ReachedStop) => {
                    m.state.eip = self.park;
                    return Exit::Parked;
                }
                Ok(RunOutcome::Budget) => continue,
                other => {
                    let kind = match other {
                        Ok(RunOutcome::ReadFault { address, .. }) => FaultKind::Read(address),
                        Ok(RunOutcome::WriteFault { address, .. }) => FaultKind::Write(address),
                        Ok(RunOutcome::FetchFault { address, .. }) => FaultKind::Fetch(address),
                        Ok(RunOutcome::InvalidInstruction { .. }) => FaultKind::InvalidInstruction,
                        Err(e) => FaultKind::Other(e),
                        _ => FaultKind::Other("unexpected standalone wait".into()),
                    };
                    return Exit::Fault(Fault {
                        kind,
                        pc: m.state.eip,
                    });
                }
            }
        }
    }
    fn invalidate_code(&mut self, addr: u32, len: u32) {
        let m = self.inner.get_mut();
        if m.memory.range(addr, len as usize).is_ok() {
            m.memory.touch(addr, len as usize);
        }
    }
}
