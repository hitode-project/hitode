//! CPU backend adapter for a caller-owned trap dispatcher.
use crate::backend::{
    ArchitecturalState, Backend, Exit, Fault, FaultKind, FpuEnvironment, FpuEnvironmentError,
    Guest, MemFault, Prot, Reg, SegmentSetup, StateError, TrapAction, TrapHandler,
};
use crate::{
    CompileStrategy, CpuState, ExecutionContext, Executor, Memory, RunOutcome, Stats, X87Register,
};
use std::time::{Duration, Instant};

pub struct JitCpu {
    inner: Executor,
    park: u32,
    budget: Option<Duration>,
    jit: bool,
}
impl JitCpu {
    pub fn set_guest_workers(&mut self, count: usize) -> Result<(), String> {
        if count > 1 && !self.jit {
            return Err("parallel execution requires the JIT backend".into());
        }
        self.inner.set_guest_workers(count)
    }
    pub fn set_row_workers(&mut self, count: usize) -> Result<(), String> {
        self.inner.set_row_workers(count)
    }
    pub fn disable_row_batching(&mut self) {
        self.inner.disable_row_batching();
    }
    pub fn set_row_threshold(&mut self, work: u64) {
        self.inner.set_row_threshold(work);
    }
    pub fn row_parallel_stats(&self) -> crate::RowStats {
        self.inner.row_parallel_stats()
    }
    pub fn parallel_stats(&self) -> crate::ParallelStats {
        self.inner.parallel_stats()
    }
    pub fn new(park: u32, jit: bool) -> Result<Self, String> {
        let mut machine = Executor::address_space()?;
        machine.enable_auto_jit(jit);
        machine.set_async_compilation(jit);
        Ok(Self {
            inner: machine,
            park,
            budget: Some(Duration::from_secs(5)),
            jit,
        })
    }
    pub fn stats(&self) -> Stats {
        self.inner.stats.clone()
    }
    pub fn set_profiling(&mut self, enabled: bool) {
        self.inner.set_profiling(enabled);
    }
    /// Choose the compilation policy before running automatically discovered code.
    pub fn set_auto_compile_strategy(&mut self, strategy: CompileStrategy) {
        self.inner.set_auto_compile_strategy(strategy);
    }
    pub fn set_region_linking(&mut self, enabled: bool) {
        self.inner.set_region_linking(enabled);
    }
    pub fn set_shadow_returns(&mut self, enabled: bool) -> Result<(), String> {
        self.inner.set_shadow_returns(enabled)
    }
    pub fn set_async_compilation(&mut self, enabled: bool) {
        self.inner.set_async_compilation(enabled);
    }
    pub fn set_background_sender(
        &mut self,
        sender: adv_sync::background::LocalSender,
    ) -> Result<(), String> {
        let wake = sender.borrow().execution_wake();
        self.inner.set_background_sender(sender)?;
        self.set_waker(wake)
    }
    pub fn background_sender(&mut self) -> Result<adv_sync::background::LocalSender, String> {
        self.inner.background_sender()
    }
    /// Install the execution owner's retained event before the first guest run.
    /// Compiler completion and native/interpreted safepoints use the same wake.
    pub fn set_waker(&mut self, wake: adv_sync::WakeEvent) -> Result<(), String> {
        self.inner.set_wake_event(wake)
    }
    pub fn profile(&self) -> std::collections::BTreeMap<u32, crate::ProfileSample> {
        self.inner.profile.clone()
    }
    pub fn executor(&self) -> &Executor {
        &self.inner
    }
    pub fn executor_mut(&mut self) -> &mut Executor {
        &mut self.inner
    }
    fn view<T>(&mut self, f: impl FnOnce(&mut View<'_>) -> T) -> T {
        let m = &mut self.inner;
        let Executor { state, memory, .. } = m;
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
    fn next_mapped_page(&self, addr: u32, limit: u64) -> Result<Option<u32>, MemFault> {
        Ok(self.memory.next_mapped_page(addr, limit))
    }
    fn quiesce_memory(&mut self) {
        self.memory.quiesce_workers();
    }
    fn lock_pages(&mut self, addr: u32, len: u32) -> Result<(), super::PageLockError> {
        self.memory.lock_pages(addr, len)
    }
    fn unlock_pages(&mut self, addr: u32, len: u32) -> Result<(), super::PageLockError> {
        self.memory.unlock_pages(addr, len)
    }
    fn has_locked_pages(&self, addr: u32, len: u32) -> bool {
        self.memory.has_locked_pages(addr, len)
    }
    fn retire_locked_pages(&mut self) -> Result<(), super::PageLockError> {
        self.memory.retire_locked_pages()
    }
    fn capture_architecture(&mut self) -> Result<ArchitecturalState, StateError> {
        Ok(self.state.capture_architecture())
    }
    fn restore_architecture(&mut self, state: &ArchitecturalState) -> Result<(), StateError> {
        self.state.restore_architecture(state)
    }
    fn fpu_environment(&mut self) -> Result<FpuEnvironment, FpuEnvironmentError> {
        self.state.commit_mmx();
        Ok(FpuEnvironment {
            x87_control: self.state.x87.control,
            mxcsr: self.state.mxcsr,
        })
    }
    fn set_fpu_environment(
        &mut self,
        environment: FpuEnvironment,
    ) -> Result<(), FpuEnvironmentError> {
        environment.validate()?;
        self.state.commit_mmx();
        if self.state.x87.control != environment.x87_control {
            self.state.x87.set_control(environment.x87_control);
        }
        self.state.mxcsr = environment.mxcsr;
        self.state.refresh_mmx();
        Ok(())
    }
    fn last_indirect_branch(&self) -> Option<crate::IndirectBranch> {
        self.state.last_indirect_branch.observed()
    }
    fn file_page_copy_on_write(&self, addr: u32) -> Option<bool> {
        self.memory.file_page_copy_on_write(addr)
    }
    fn map_file_pages(
        &mut self,
        addr: u32,
        pages: &[std::rc::Rc<crate::backend::FilePage>],
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
    fn map_shared_audio(
        &mut self,
        addr: u32,
        len: u32,
    ) -> Result<crate::backend::SharedRegion, MemFault> {
        let read_alias = self
            .memory
            .map_shared_audio(addr, len as usize)
            .map_err(|_| fault(addr, len as usize))?;
        Ok(crate::backend::SharedRegion {
            guest_base: addr,
            len,
            read_alias,
        })
    }
    fn copy_memory(&mut self, src: u32, dst: u32, len: usize) -> Result<(), MemFault> {
        let len32 = u32::try_from(len).map_err(|_| fault(src, len))?;
        if !self.accessible(src, len32, Prot::READ) {
            return Err(fault(src, len));
        }
        if !self.accessible(dst, len32, Prot::WRITE) {
            return Err(fault(dst, len));
        }
        self.memory.copy(src, dst, len).map_err(|_| fault(dst, len))
    }
    fn push_f64(&mut self, value: f64) {
        self.state.commit_mmx();
        self.state
            .x87
            .push(X87Register(crate::backend::x87::encode_f64(value)));
        self.state.refresh_mmx();
    }
    fn pop_f64(&mut self) -> Option<f64> {
        self.state.commit_mmx();
        let value = self.state.x87.pop()?;
        Some(crate::backend::x87::decode_f64(&value.0))
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
    fn next_mapped_page(&self, addr: u32, limit: u64) -> Result<Option<u32>, MemFault> {
        Ok(self.inner.memory.next_mapped_page(addr, limit))
    }
    fn quiesce_memory(&mut self) {
        self.inner.memory.quiesce_workers();
    }
    fn lock_pages(&mut self, addr: u32, len: u32) -> Result<(), super::PageLockError> {
        self.view(|guest| guest.lock_pages(addr, len))
    }
    fn unlock_pages(&mut self, addr: u32, len: u32) -> Result<(), super::PageLockError> {
        self.view(|guest| guest.unlock_pages(addr, len))
    }
    fn has_locked_pages(&self, addr: u32, len: u32) -> bool {
        self.inner.memory.has_locked_pages(addr, len)
    }
    fn retire_locked_pages(&mut self) -> Result<(), super::PageLockError> {
        self.view(|guest| guest.retire_locked_pages())
    }
    fn capture_architecture(&mut self) -> Result<ArchitecturalState, StateError> {
        self.view(|g| g.capture_architecture())
    }
    fn restore_architecture(&mut self, state: &ArchitecturalState) -> Result<(), StateError> {
        self.view(|g| g.restore_architecture(state))
    }
    fn fpu_environment(&mut self) -> Result<FpuEnvironment, FpuEnvironmentError> {
        self.view(|g| g.fpu_environment())
    }
    fn set_fpu_environment(
        &mut self,
        environment: FpuEnvironment,
    ) -> Result<(), FpuEnvironmentError> {
        self.view(|g| g.set_fpu_environment(environment))
    }
    fn last_indirect_branch(&self) -> Option<crate::IndirectBranch> {
        self.inner.state.last_indirect_branch.observed()
    }
    fn file_page_copy_on_write(&self, addr: u32) -> Option<bool> {
        self.executor().memory.file_page_copy_on_write(addr)
    }
    fn map_file_pages(
        &mut self,
        addr: u32,
        pages: &[std::rc::Rc<crate::backend::FilePage>],
        prot: Prot,
        copy_on_write: bool,
    ) -> Result<(), MemFault> {
        self.view(|g| g.map_file_pages(addr, pages, prot, copy_on_write))
    }
    fn map_shared_audio(
        &mut self,
        addr: u32,
        len: u32,
    ) -> Result<crate::backend::SharedRegion, MemFault> {
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
        get(&self.inner.state, r)
    }
    fn set_reg(&mut self, r: Reg, v: u32) {
        set(&mut self.inner.state, r, v)
    }
    fn read(&self, addr: u32, buf: &mut [u8]) -> Result<(), MemFault> {
        self.inner
            .memory
            .read_into(addr, buf)
            .map_err(|_| fault(addr, buf.len()))
    }
    fn write(&mut self, addr: u32, b: &[u8]) -> Result<(), MemFault> {
        self.view(|g| g.write(addr, b))
    }
    fn accessible(&self, addr: u32, len: u32, prot: Prot) -> bool {
        let m = &self.inner;
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
impl crate::ExecutionTrap for Raw<'_> {
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
    fn frame_counters(&self) -> Option<crate::frame_counters::FrameCounters> {
        Some(self.inner.frame_counters())
    }
    fn new_process(&mut self) -> Result<Self, String> {
        let mut child = Self::new(self.park, self.jit)?;
        child.budget = self.budget;
        let sender = self.background_sender()?;
        child.inner.inherit_process_policy(&self.inner)?;
        child.set_background_sender(sender)?;
        Ok(child)
    }

    type Context = Option<ExecutionContext>;
    fn guest_workers(&self) -> usize {
        self.inner.guest_workers()
    }
    fn parallel_capacity_available(&self) -> bool {
        true // the controller's inline execution lane is always available
    }
    fn dispatch_ready_context(
        &mut self,
        context: &Self::Context,
        duration: Duration,
        guest_us: u64,
    ) -> Result<Option<u64>, String> {
        let Some(context) = context else {
            return Ok(None);
        };
        self.inner
            .dispatch_ready_context(context, self.park, duration, guest_us)
    }
    fn set_parallel_demand(&mut self, ready_peer: bool) {
        self.inner.parallel_demand = ready_peer;
    }
    fn poll_parallel(&mut self) -> Option<(u64, Self::Context)> {
        self.inner
            .poll_parallel()
            .map(|(token, context)| (token, Some(context)))
    }
    fn wait_parallel(&mut self, duration: Duration) -> Result<(), String> {
        self.inner.wait_parallel(duration)
    }
    fn setup_segments(&mut self, seg: SegmentSetup) -> Result<(), MemFault> {
        self.inner
            .setup_wow64_segments(seg.gdt_addr)
            .map_err(|_| fault(seg.gdt_addr, 4096))
    }
    fn set_fs_base(&mut self, base: u32) -> Result<(), MemFault> {
        self.inner.set_fs_base(base).map_err(|_| fault(base, 1))
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
                .save_context()
                .expect("save complete guest context"),
        );
    }
    fn restore(&mut self, c: &Self::Context) {
        self.inner
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
        self.inner.set_host_interrupts(enabled);
    }

    fn host_interrupt_pending(&self) -> bool {
        self.inner.host_interrupt_pending()
    }
    fn run(&mut self, handler: &mut dyn TrapHandler) -> Exit {
        let _activity = self.inner.inline_activity();
        let start = Instant::now();
        self.inner.deadline = self.budget.and_then(|b| start.checked_add(b));
        if let Some(stop) = self.inner.take_parallel_stop() {
            match stop {
                crate::parallel::Stop::Trap(vector) => {
                    self.inner.stats.trap_calls += 1;
                    let action = self.view(|view| handler.on_trap(view, vector));
                    if action == TrapAction::Park {
                        self.inner.state.eip = self.park;
                        return Exit::Parked;
                    }
                }
                crate::parallel::Stop::Fallback => self.inner.cold_parallel_retry(),
                // The immutable worker image can trail owner publication. A
                // lookup miss must redispatch at the same PC, not force an
                // interpreted instruction or discard useful execution history.
                crate::parallel::Stop::CacheMiss => {}
                // Completion was requeued by the machine; this selection is
                // already a fresh turn, not a second expiration of the old one.
                crate::parallel::Stop::Quantum => {}
                crate::parallel::Stop::Interrupted => {
                    return Exit::Interrupted {
                        pc: self.inner.state.eip,
                    };
                }
                crate::parallel::Stop::Parked => return Exit::Parked,
                crate::parallel::Stop::Failed(message) => {
                    return Exit::Fault(Fault {
                        pc: self.inner.state.eip,
                        kind: FaultKind::Other(message),
                    });
                }
            }
        }
        loop {
            let m = &mut self.inner;
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
            let result = m.run_with_traps(self.park, 32_768, &mut raw, !self.jit);
            match result {
                Ok(RunOutcome::NativeReady) => {
                    let remaining = self.budget.map_or(Duration::from_millis(1), |budget| {
                        budget.saturating_sub(start.elapsed())
                    });
                    if remaining.is_zero() {
                        return Exit::BudgetExceeded {
                            pc: m.state.eip,
                            budget: self.budget.unwrap(),
                        };
                    }
                    let now = handler.guest_time_us().unwrap_or(0);
                    match m.dispatch_parallel(
                        self.park,
                        remaining.min(Duration::from_millis(1)),
                        now,
                    ) {
                        Ok(Some(token)) => return Exit::Dispatched { token },
                        Ok(None) => continue,
                        Err(message) => {
                            return Exit::Fault(Fault {
                                pc: m.state.eip,
                                kind: FaultKind::Other(message),
                            });
                        }
                    }
                }
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
                        _ => FaultKind::Other("unexpected CPU execution outcome".into()),
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
        let m = &mut self.inner;
        if m.memory.range(addr, len as usize).is_ok() {
            m.memory.touch(addr, len as usize);
        }
    }
}

#[cfg(test)]
mod process_tests {
    use super::*;
    use crate::backend::GuestExt;
    use adv_sync::background::{Priority, Task};
    use std::rc::Rc;
    use std::sync::mpsc;

    const CODE: u32 = 0x400000;
    const CALLER: u32 = CODE + 0x100;
    const STOP: u32 = CODE + 0x2000;
    const DATA: u32 = 0x410000;
    const STACK: u32 = DATA + 0xf00;

    fn load(cpu: &mut JitCpu, increment: u8, marker: u32) {
        cpu.map(CODE, 4096, Prot::RW).unwrap();
        cpu.map(DATA, 4096, Prot::RW).unwrap();
        // add ebx,increment; ret
        cpu.write(CODE, &[0x83, 0xc3, increment, 0xc3]).unwrap();
        // top: call esi; dec ecx; jnz top; ret
        cpu.write(CALLER, &[0xff, 0xd6, 0x49, 0x75, 0xfb, 0xc3])
            .unwrap();
        cpu.write_u32(DATA, marker).unwrap();
        cpu.protect(CODE, 4096, Prot::RX).unwrap();
        start(cpu, 10_000_000);
    }

    fn start(cpu: &mut JitCpu, iterations: u32) {
        cpu.inner.state.eip = CALLER;
        cpu.inner.state.regs = [0, iterations, 0, 0, STACK, 0, CODE, 0];
        cpu.write_u32(STACK, STOP).unwrap();
    }

    fn run(cpu: &mut JitCpu, fuel: u64) -> RunOutcome {
        cpu.inner.run(STOP, fuel).unwrap()
    }

    fn queue_compilation(cpu: &mut JitCpu) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while cpu.stats().pending_compilations == 0 {
            assert_eq!(run(cpu, 1000), RunOutcome::Budget);
            assert!(
                Instant::now() < deadline,
                "child did not submit asynchronous work: {:?}",
                cpu.stats()
            );
        }
        assert_eq!(cpu.stats().compilations, 0, "the shared service is blocked");
    }

    fn promote_indirect_callee(cpu: &mut JitCpu) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let entries = cpu.stats().native_entries;
            assert_eq!(run(cpu, 3000), RunOutcome::Budget);
            let stats = cpu.stats();
            // Before tier-up the indirect call and return transfer between
            // native units. Capturing the observed callee keeps the whole
            // 3000-block run inside one unit, including its local returns.
            if stats.tier_up_publications != 0 && stats.native_entries - entries <= 2 {
                assert_eq!(stats.compile_failures, 0);
                return;
            }
            assert!(
                Instant::now() < deadline,
                "indirect callee never promoted: {stats:?}"
            );
            std::thread::yield_now();
        }
    }

    #[test]
    fn asynchronous_child_callees_keep_their_address_space_and_service_after_parent_drop() {
        let mut parent = JitCpu::new(STOP, true).unwrap();
        parent.disable_row_batching();
        parent.set_execution_budget(Some(Duration::from_millis(17)));
        parent.set_host_interrupts(false);
        parent.set_profiling(true);
        parent.set_auto_compile_strategy(CompileStrategy::Fast);
        load(&mut parent, 1, 0x11111111);
        promote_indirect_callee(&mut parent);
        assert!(!parent.profile().is_empty());

        let mut first = parent.new_process().unwrap();
        let mut second = parent.new_process().unwrap();
        let service = parent.background_sender().unwrap();
        assert!(Rc::ptr_eq(&service, &first.background_sender().unwrap()));
        assert!(Rc::ptr_eq(&service, &second.background_sender().unwrap()));
        for child in [&first, &second] {
            assert_eq!(child.execution_budget(), Some(Duration::from_millis(17)));
            assert!(!child.accessible(CODE, 1, Prot::READ));
            assert_eq!(child.stats().native_regions, 0);
            assert_eq!(child.stats().tier_up_requests, 0);
            assert!(child.profile().is_empty());
        }
        load(&mut first, 3, 0x22222222);
        load(&mut second, 7, 0x77777777);

        // Hold the shared worker at a known boundary so both children own
        // queued snapshots when their parent goes away. The timeout bounds a
        // broken service-lifetime implementation instead of hanging the test.
        let (entered, started) = mpsc::channel();
        let (release, unblock) = mpsc::channel();
        service
            .borrow_mut()
            .try_submit(
                Priority::Urgent,
                Task::new(move || {
                    entered.send(()).unwrap();
                    let _ = unblock.recv_timeout(Duration::from_secs(5));
                }),
            )
            .unwrap();
        started.recv_timeout(Duration::from_secs(5)).unwrap();
        queue_compilation(&mut first);
        queue_compilation(&mut second);

        parent.write(CODE, &[0x83, 0xc3, 11, 0xc3]).unwrap();
        parent.write_u32(DATA, 0xaaaaaaaa).unwrap();
        assert_eq!(first.read_u32(DATA).unwrap(), 0x22222222);
        assert_eq!(second.read_u32(DATA).unwrap(), 0x77777777);
        let dropped = Instant::now();
        drop(parent);
        assert!(
            dropped.elapsed() < Duration::from_secs(2),
            "parent joined the child's blocked compilation service"
        );
        release.send(()).unwrap();

        for (child, increment, marker) in
            [(&mut first, 3, 0x22222222), (&mut second, 7, 0x77777777)]
        {
            promote_indirect_callee(child);
            assert!(
                !child.profile().is_empty(),
                "profiling policy is inherited independently"
            );
            start(child, 10_000);
            assert_eq!(run(child, u64::MAX), RunOutcome::ReachedStop);
            assert_eq!(child.inner.state.regs[3], 10_000 * increment);
            assert_eq!(child.read_u32(DATA).unwrap(), marker);
            assert_eq!(
                child.stats().stale_compilations,
                0,
                "parent writes cannot invalidate a child's snapshot"
            );
        }
    }
}
