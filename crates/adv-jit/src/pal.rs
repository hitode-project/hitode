//! Adapter to the existing PE loader, Win32 handlers and cooperative scheduler.
use crate::{
    CompileStrategy, CpuState, ExecutionContext, Machine, Memory, RunOutcome, Stats, X87Register,
};
use pal_cpu::{
    Backend, Exit, Fault, FaultKind, Guest, MemFault, Prot, Reg, SegmentSetup, TrapAction,
    TrapHandler,
};
use std::cell::{Ref, RefCell, RefMut};
use std::time::{Duration, Instant};

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
        machine.set_async_compilation(jit);
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
    pub fn set_region_linking(&mut self, enabled: bool) {
        self.inner.get_mut().set_region_linking(enabled);
    }
    pub fn set_async_compilation(&mut self, enabled: bool) {
        self.inner.get_mut().set_async_compilation(enabled);
    }
    /// Install the execution owner's retained event before the first guest run.
    /// Compiler completion and native/interpreted safepoints use the same wake.
    pub fn set_waker(&mut self, wake: adv_sync::WakeEvent) -> Result<(), String> {
        let m = self.inner.get_mut();
        let flag = wake.interrupt_flag();
        m.set_completion_waker(std::sync::Arc::new(move || wake.signal()))?;
        m.set_host_interrupt_flag(flag);
        Ok(())
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
        let Machine { state, memory, .. } = m;
        f(&mut View { state, memory })
    }
}

struct View<'a> {
    state: &'a mut CpuState,
    memory: &'a mut Memory,
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
    fn file_page_copy_on_write(&self, addr: u32) -> Option<bool> {
        self.memory.file_page_copy_on_write(addr)
    }
    fn map_file_pages(
        &mut self,
        addr: u32,
        pages: &[std::rc::Rc<pal_cpu::FilePage>],
        prot: Prot,
        copy_on_write: bool,
    ) -> Result<(), MemFault> {
        self.memory
            .map_file_pages(addr, pages, prot.0, copy_on_write)
            .map_err(|_| fault(addr, pages.len() * 4096))
    }
    fn cpuid_features(&self) -> u32 {
        crate::cpu_profile::cpuid(1, 0)[3]
    }
    fn shared_bytes(&self, addr: u32, len: usize) -> Option<&[u8]> {
        self.memory.read(addr, len).ok()
    }
    fn map_shared_audio(&mut self, addr: u32, len: u32) -> Result<pal_cpu::SharedRegion, MemFault> {
        let read_alias = self
            .memory
            .map_shared_audio(addr, len as usize)
            .map_err(|_| fault(addr, len as usize))?;
        Ok(pal_cpu::SharedRegion {
            guest_base: addr,
            len,
            read_alias,
        })
    }
    fn copy_memory(&mut self, src: u32, dst: u32, len: usize) -> Result<(), MemFault> {
        crate::ApiMemory { inner: self.memory }
            .copy(src, dst, len)
            .map_err(|_| fault(dst, len))
    }
    fn push_f64(&mut self, value: f64) {
        self.state.commit_mmx();
        self.state
            .x87
            .push(X87Register(pal_cpu::x87::encode_f64(value)));
        self.state.refresh_mmx();
    }
    fn pop_f64(&mut self) -> Option<f64> {
        self.state.commit_mmx();
        let value = self.state.x87.pop()?;
        Some(pal_cpu::x87::decode_f64(&value.0))
    }
    fn reg(&self, r: Reg) -> u32 {
        get(self.state, r)
    }
    fn set_reg(&mut self, r: Reg, v: u32) {
        set(self.state, r, v)
    }
    fn read(&self, addr: u32, buf: &mut [u8]) -> Result<(), MemFault> {
        self.memory
            .read_into(addr, buf)
            .map_err(|_| fault(addr, buf.len()))
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
        self.memory
            .map(addr, size as usize, prot.0)
            .map_err(|_| fault(addr, size as usize))
    }
    fn protect(&mut self, addr: u32, size: u32, prot: Prot) -> Result<(), MemFault> {
        self.memory
            .protect(addr, size as usize, prot.0)
            .map_err(|_| fault(addr, size as usize))
    }
    fn unmap(&mut self, addr: u32, size: u32) -> Result<(), MemFault> {
        self.memory
            .unmap(addr, size as usize)
            .map_err(|_| fault(addr, size as usize))
    }
}

impl Guest for JitCpu {
    fn file_page_copy_on_write(&self, addr: u32) -> Option<bool> {
        self.machine().memory.file_page_copy_on_write(addr)
    }
    fn map_file_pages(
        &mut self,
        addr: u32,
        pages: &[std::rc::Rc<pal_cpu::FilePage>],
        prot: Prot,
        copy_on_write: bool,
    ) -> Result<(), MemFault> {
        self.view(|g| g.map_file_pages(addr, pages, prot, copy_on_write))
    }
    fn map_shared_audio(&mut self, addr: u32, len: u32) -> Result<pal_cpu::SharedRegion, MemFault> {
        self.view(|g| g.map_shared_audio(addr, len))
    }
    fn cpuid_features(&self) -> u32 {
        crate::cpu_profile::cpuid(1, 0)[3]
    }
    fn push_f64(&mut self, v: f64) {
        self.view(|g| g.push_f64(v));
    }
    fn pop_f64(&mut self) -> Option<f64> {
        self.view(|g| g.pop_f64())
    }
    fn reg(&self, r: Reg) -> u32 {
        get(&self.inner.borrow().state, r)
    }
    fn set_reg(&mut self, r: Reg, v: u32) {
        set(&mut self.inner.get_mut().state, r, v)
    }
    fn read(&self, addr: u32, buf: &mut [u8]) -> Result<(), MemFault> {
        self.inner
            .borrow()
            .memory
            .read_into(addr, buf)
            .map_err(|_| fault(addr, buf.len()))
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
        self.view(|g| g.map(addr, size, prot))
    }
    fn protect(&mut self, addr: u32, size: u32, prot: Prot) -> Result<(), MemFault> {
        self.view(|g| g.protect(addr, size, prot))
    }
    fn unmap(&mut self, addr: u32, size: u32) -> Result<(), MemFault> {
        self.view(|g| g.unmap(addr, size))
    }
}

struct Raw<'a> {
    handler: &'a mut dyn TrapHandler,
}
impl crate::host::RawTrap for Raw<'_> {
    fn guest_time_us(&mut self) -> Option<u64> {
        self.handler.guest_time_us()
    }
    fn call(
        &mut self,
        state: &mut CpuState,
        memory: &mut Memory,
        vector: u32,
    ) -> Result<bool, String> {
        state.commit_mmx();
        let action = self
            .handler
            .on_trap(&mut View { state, memory }, vector as u8);
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
        self.inner
            .get_mut()
            .set_fs_base(base)
            .map_err(|_| fault(base, 1))
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
    fn set_host_interrupts(&mut self, enabled: bool) {
        self.inner.get_mut().set_host_interrupts(enabled);
    }
    fn run(&mut self, handler: &mut dyn TrapHandler) -> Exit {
        let start = Instant::now();
        self.inner.get_mut().deadline = self.budget.and_then(|b| start.checked_add(b));
        loop {
            let m = self.inner.get_mut();
            if m.state.eip == self.park {
                return Exit::Parked;
            }
            if let Some(budget) = self.budget
                && start.elapsed() >= budget
            {
                return Exit::BudgetExceeded {
                    pc: m.state.eip,
                    budget,
                };
            }
            let mut raw = Raw { handler };
            let result = m.run_traps(self.park, 32_768, &mut raw, !self.jit);
            match result {
                Ok(RunOutcome::Parked | RunOutcome::ReachedStop) => {
                    m.state.eip = self.park;
                    return Exit::Parked;
                }
                Ok(RunOutcome::Budget) => continue,
                Ok(RunOutcome::Interrupted) => {
                    if let Some(budget) = self.budget
                        && start.elapsed() >= budget
                    {
                        return Exit::BudgetExceeded {
                            pc: m.state.eip,
                            budget,
                        };
                    }
                    return Exit::Interrupted { pc: m.state.eip };
                }
                other => {
                    let kind = match other {
                        Ok(RunOutcome::ReadFault { address, .. }) => FaultKind::Read(address),
                        Ok(RunOutcome::WriteFault { address, .. }) => FaultKind::Write(address),
                        Ok(RunOutcome::FetchFault { address, .. }) => FaultKind::Fetch(address),
                        Ok(RunOutcome::InvalidInstruction { .. }) => FaultKind::InvalidInstruction,
                        Ok(RunOutcome::UnsupportedInstruction { pc, code }) => FaultKind::Other(
                            format!("unsupported guest instruction {code:?} at {pc:#x}"),
                        ),
                        Ok(RunOutcome::Trap { vector, .. }) => {
                            FaultKind::Other(format!("unhandled guest exception {vector}"))
                        }
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
