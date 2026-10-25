//! The guest CPU the machine layer drives, on the a64 engine: [`Cpu`]
//! implements `Backend` and `Engine` over [`Executor`] as `JitCpu` does
//! over the x64 executor. Translation is synchronous and cheap, so the
//! compiler policy of `EngineOptions` does not apply; the background
//! service exists only for the shell's other execution-owned work.

use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use adv_cpu_core::backend::{
    Backend, Exit, Fault, FaultKind, FpuEnvironment, FpuEnvironmentError, Guest, MemFault,
    PageLockError, Prot, Reg, SegmentSetup, SharedRegion, TrapAction, TrapHandler,
};
use adv_cpu_core::engine::{Engine, EngineCounters, EngineOptions, json_string as json};
use adv_cpu_core::{
    ArchitecturalState, CpuState, ExecutionTrap, Memory, RunOutcome, SegmentState, StateError,
    X87Register,
};
use adv_sync::WakeEvent;
use adv_sync::background::{Builder, LocalSender, Service};

use crate::{ExecutionContext, Executor};

/// Queue capacity of a background service the CPU starts itself.
const CAPACITY: usize = 8;

/// Instructions one executor call may complete; the deadline bounds a run instead.
const FUEL: u64 = u64::MAX >> 2;

pub struct Cpu {
    inner: Executor,
    park: u32,
    budget: Option<Duration>,
    native: bool,
    /// Guest instructions completed by finished runs.
    retired: u64,
    wake: Option<WakeEvent>,
    sender: Option<LocalSender>,
    service: Option<Rc<Service>>,
}

impl Cpu {
    /// A fresh 4 GiB address space; `native` false interprets everything
    /// and maps no code cache.
    pub fn new(park: u32, native: bool) -> Result<Self, String> {
        let inner = match native {
            true => Executor::address_space()?,
            false => Executor::interpreted_address_space()?,
        };
        Ok(Self {
            inner,
            park,
            budget: Some(Duration::from_secs(5)),
            native,
            retired: 0,
            wake: None,
            sender: None,
            service: None,
        })
    }

    pub fn executor(&self) -> &Executor {
        &self.inner
    }

    pub fn executor_mut(&mut self) -> &mut Executor {
        &mut self.inner
    }

    pub fn stats(&self) -> crate::Stats {
        self.inner.stats.clone()
    }

    /// Translation is always synchronous here, so there is nothing to turn
    /// off; callers shared with the x64 engine need no target cfg.
    pub fn set_async_compilation(&mut self, _enabled: bool) {}

    /// Install the execution owner's retained event before the first run.
    pub fn set_waker(&mut self, wake: WakeEvent) -> Result<(), String> {
        if let Some(sender) = &self.sender
            && !same_wake(&sender.borrow().execution_wake(), &wake)
        {
            return Err("wake event cannot change after background service registration".into());
        }
        self.inner.set_host_interrupt_flag(wake.interrupt_flag());
        self.wake = Some(wake);
        Ok(())
    }

    /// Inject the shell's background endpoint before its first use.
    pub fn set_background_sender(&mut self, sender: LocalSender) -> Result<(), String> {
        if self.sender.is_some() {
            return Err("background sender must be installed before first use".into());
        }
        let wake = sender.borrow().execution_wake();
        if self.wake.as_ref().is_some_and(|old| !same_wake(old, &wake)) {
            return Err("background service must use the execution owner's wake event".into());
        }
        self.set_waker(wake)?;
        self.sender = Some(sender);
        Ok(())
    }

    /// The execution owner's background endpoint, started on first use when
    /// the shell installed none.
    pub fn background_sender(&mut self) -> Result<LocalSender, String> {
        if let Some(sender) = &self.sender {
            return Ok(sender.clone());
        }
        let wake = match &self.wake {
            Some(wake) => wake.clone(),
            None => WakeEvent::new().map_err(|error| error.to_string())?,
        };
        let mut builder = Builder::new(wake.clone()).map_err(|error| error.to_string())?;
        let sender = builder
            .sender(CAPACITY, CAPACITY)
            .map_err(|error| error.to_string())?
            .into_local();
        self.service = Some(Rc::new(builder.start().map_err(|e| e.to_string())?));
        self.set_waker(wake)?;
        self.sender = Some(sender.clone());
        Ok(sender)
    }

    fn view<T>(&mut self, f: impl FnOnce(&mut View<'_>) -> T) -> T {
        f(&mut View {
            state: &mut self.inner.state,
            memory: &mut self.inner.memory,
            images: &self.inner.images,
        })
    }
}

fn same_wake(a: &WakeEvent, b: &WakeEvent) -> bool {
    std::sync::Arc::ptr_eq(&a.interrupt_flag(), &b.interrupt_flag())
}

/// The guest as trap handlers see it: the CPU state and its memory.
struct View<'a> {
    images: &'a Rc<std::cell::RefCell<Vec<(u32, u32)>>>,
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

fn accessible(memory: &Memory, addr: u32, len: u32, prot: Prot) -> bool {
    let Ok(r) = memory.committed(addr, len as usize) else {
        return false;
    };
    r.is_empty()
        || memory.perms[r.start / 4096..=(r.end - 1) / 4096]
            .iter()
            .all(|p| p & prot.0 == prot.0)
}

impl Guest for View<'_> {
    fn wants_code_images(&self) -> bool {
        cfg!(native)
    }
    fn register_code_image(&mut self, image: adv_cpu_core::AotImage) {
        self.images.borrow_mut().push((image.base, image.size));
    }
    fn next_mapped_page(&self, addr: u32, limit: u64) -> Result<Option<u32>, MemFault> {
        Ok(self.memory.next_mapped_page(addr, limit))
    }
    fn lock_pages(&mut self, addr: u32, len: u32) -> Result<(), PageLockError> {
        self.memory.lock_pages(addr, len)
    }
    fn unlock_pages(&mut self, addr: u32, len: u32) -> Result<(), PageLockError> {
        self.memory.unlock_pages(addr, len)
    }
    fn has_locked_pages(&self, addr: u32, len: u32) -> bool {
        self.memory.has_locked_pages(addr, len)
    }
    fn retire_locked_pages(&mut self) -> Result<(), PageLockError> {
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
    fn last_indirect_branch(&self) -> Option<adv_cpu_core::IndirectBranch> {
        self.state.last_indirect_branch.observed()
    }
    fn file_page_copy_on_write(&self, addr: u32) -> Option<bool> {
        self.memory.file_page_copy_on_write(addr)
    }
    fn private_copy_epoch(&self) -> Option<u64> {
        Some(self.memory.private_copy_epoch())
    }
    fn map_private_copy_pages(
        &mut self,
        addr: u32,
        bytes: &[u8],
        prot: Prot,
    ) -> Result<(), MemFault> {
        self.memory
            .map_private_copy_pages(addr, bytes, prot.0)
            .map_err(|_| fault(addr, bytes.len()))
    }
    fn privatize_copy_pages(&mut self, addr: u32, len: u32) -> Result<(), MemFault> {
        self.memory
            .privatize_copy_pages(addr, len as usize)
            .map_err(|_| fault(addr, len as usize))
    }
    #[cfg(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "ios"
    ))]
    fn map_shared_file(
        &mut self,
        addr: u32,
        len: u32,
        fd: std::os::fd::BorrowedFd<'_>,
        offset: u64,
        prot: Prot,
    ) -> Result<(), MemFault> {
        let writable = prot.contains(Prot::WRITE);
        adv_cpu_core::shared_memory::SharedAllocation::map_file(fd, offset, len as usize, writable)
            .and_then(|storage| {
                self.memory
                    .map_shared_file(addr, len as usize, storage, prot.0)
            })
            .map_err(|_| fault(addr, len as usize))
    }
    fn map_file_pages(
        &mut self,
        addr: u32,
        pages: &[Rc<adv_cpu_core::backend::FilePage>],
        prot: Prot,
        copy_on_write: bool,
    ) -> Result<(), MemFault> {
        self.memory
            .map_file_pages(addr, pages, prot.0, copy_on_write)
            .map_err(|_| fault(addr, pages.len() * 4096))
    }
    fn cpuid_features(&self) -> u32 {
        adv_cpu_core::cpu_profile::cpuid(1, 0)[3]
    }
    fn shared_bytes(&self, addr: u32, len: usize) -> Option<&[u8]> {
        self.memory.read(addr, len).ok()
    }
    fn map_shared_audio(&mut self, addr: u32, len: u32) -> Result<SharedRegion, MemFault> {
        let read_alias = self
            .memory
            .map_shared_audio(addr, len as usize)
            .map_err(|_| fault(addr, len as usize))?;
        Ok(SharedRegion {
            guest_base: addr,
            len,
            read_alias,
        })
    }
    fn map_shared_counters(
        &mut self,
        addr: u32,
        len: u32,
    ) -> Result<Rc<dyn adv_cpu_core::backend::CounterStorage>, MemFault> {
        let storage = self
            .memory
            .map_shared_audio(addr, len as usize)
            .map_err(|_| fault(addr, len as usize))?;
        Ok(Rc::new(storage))
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
    fn fill_memory(&mut self, addr: u32, byte: u8, len: usize) -> Result<(), MemFault> {
        let len32 = u32::try_from(len).map_err(|_| fault(addr, len))?;
        if !self.accessible(addr, len32, Prot::WRITE) {
            return Err(fault(addr, len));
        }
        self.memory
            .fill(addr, &[byte], len)
            .map_err(|_| fault(addr, len))
    }
    fn push_f64(&mut self, value: f64) {
        self.state.commit_mmx();
        self.state
            .x87
            .push(X87Register(adv_cpu_core::backend::x87::encode_f64(value)));
        self.state.refresh_mmx();
    }
    fn pop_f64(&mut self) -> Option<f64> {
        self.state.commit_mmx();
        let value = self.state.x87.pop()?;
        Some(adv_cpu_core::backend::x87::decode_f64(&value.0))
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
        accessible(self.memory, addr, len, prot)
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
        self.images.borrow_mut().retain(|&(base, len)| {
            u64::from(base) >= u64::from(addr) + u64::from(size)
                || u64::from(addr) >= u64::from(base) + u64::from(len)
        });
        self.memory
            .unmap(addr, size as usize)
            .map_err(|_| fault(addr, size as usize))
    }
}

/// Forwards each method to the `View` of the CPU.
macro_rules! forward {
    ($(fn $name:ident(&mut self $(, $arg:ident: $ty:ty)*) -> $ret:ty;)*) => {
        $(fn $name(&mut self $(, $arg: $ty)*) -> $ret {
            self.view(|guest| guest.$name($($arg),*))
        })*
    };
}

impl Guest for Cpu {
    fn wants_code_images(&self) -> bool {
        cfg!(native) && self.native
    }
    #[cfg(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "ios"
    ))]
    fn map_shared_file(
        &mut self,
        addr: u32,
        len: u32,
        fd: std::os::fd::BorrowedFd<'_>,
        offset: u64,
        prot: Prot,
    ) -> Result<(), MemFault> {
        self.view(|guest| guest.map_shared_file(addr, len, fd, offset, prot))
    }
    forward! {
        fn register_code_image(&mut self, image: adv_cpu_core::AotImage) -> ();
        fn lock_pages(&mut self, addr: u32, len: u32) -> Result<(), PageLockError>;
        fn unlock_pages(&mut self, addr: u32, len: u32) -> Result<(), PageLockError>;
        fn retire_locked_pages(&mut self) -> Result<(), PageLockError>;
        fn capture_architecture(&mut self) -> Result<ArchitecturalState, StateError>;
        fn restore_architecture(&mut self, state: &ArchitecturalState) -> Result<(), StateError>;
        fn fpu_environment(&mut self) -> Result<FpuEnvironment, FpuEnvironmentError>;
        fn set_fpu_environment(&mut self, environment: FpuEnvironment)
            -> Result<(), FpuEnvironmentError>;
        fn map_private_copy_pages(&mut self, addr: u32, bytes: &[u8], prot: Prot)
            -> Result<(), MemFault>;
        fn privatize_copy_pages(&mut self, addr: u32, len: u32) -> Result<(), MemFault>;
        fn map_file_pages(
            &mut self,
            addr: u32,
            pages: &[Rc<adv_cpu_core::backend::FilePage>],
            prot: Prot,
            copy_on_write: bool
        ) -> Result<(), MemFault>;
        fn map_shared_audio(&mut self, addr: u32, len: u32) -> Result<SharedRegion, MemFault>;
        fn map_shared_counters(&mut self, addr: u32, len: u32)
            -> Result<Rc<dyn adv_cpu_core::backend::CounterStorage>, MemFault>;
        fn copy_memory(&mut self, src: u32, dst: u32, len: usize) -> Result<(), MemFault>;
        fn fill_memory(&mut self, addr: u32, byte: u8, len: usize) -> Result<(), MemFault>;
        fn push_f64(&mut self, value: f64) -> ();
        fn pop_f64(&mut self) -> Option<f64>;
        fn write(&mut self, addr: u32, data: &[u8]) -> Result<(), MemFault>;
        fn map(&mut self, addr: u32, size: u32, prot: Prot) -> Result<(), MemFault>;
        fn protect(&mut self, addr: u32, size: u32, prot: Prot) -> Result<(), MemFault>;
        fn unmap(&mut self, addr: u32, size: u32) -> Result<(), MemFault>;
    }
    fn next_mapped_page(&self, addr: u32, limit: u64) -> Result<Option<u32>, MemFault> {
        Ok(self.inner.memory.next_mapped_page(addr, limit))
    }
    fn has_locked_pages(&self, addr: u32, len: u32) -> bool {
        self.inner.memory.has_locked_pages(addr, len)
    }
    fn last_indirect_branch(&self) -> Option<adv_cpu_core::IndirectBranch> {
        self.inner.state.last_indirect_branch.observed()
    }
    fn file_page_copy_on_write(&self, addr: u32) -> Option<bool> {
        self.inner.memory.file_page_copy_on_write(addr)
    }
    fn private_copy_epoch(&self) -> Option<u64> {
        Some(self.inner.memory.private_copy_epoch())
    }
    fn cpuid_features(&self) -> u32 {
        adv_cpu_core::cpu_profile::cpuid(1, 0)[3]
    }
    fn shared_bytes(&self, addr: u32, len: usize) -> Option<&[u8]> {
        self.inner.memory.read(addr, len).ok()
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
    fn accessible(&self, addr: u32, len: u32, prot: Prot) -> bool {
        accessible(&self.inner.memory, addr, len, prot)
    }
}

/// The executor's raw trap interface over a machine-layer handler.
struct Raw<'a> {
    images: &'a Rc<std::cell::RefCell<Vec<(u32, u32)>>>,
    handler: &'a mut dyn TrapHandler,
}

impl ExecutionTrap for Raw<'_> {
    fn native_region(
        &self,
        pc: u32,
        code: &[u8],
    ) -> Vec<adv_cpu_core::native_region::NativeRegion> {
        self.handler.native_region(pc, code)
    }

    fn leaf_call(&self, pc: u32, word: u32) -> bool {
        self.handler.leaf_call(pc, word)
    }

    fn redirect(&self, pc: u32) -> Option<(u32, &[u8])> {
        self.handler.redirect(pc)
    }

    fn leaf(
        &mut self,
        state: &mut CpuState,
        memory: &mut Memory,
        pc: u32,
        word: u32,
    ) -> Option<u64> {
        let images = self.images;
        self.handler.on_leaf(
            &mut View {
                state,
                memory,
                images,
            },
            pc,
            word,
        )
    }

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
        let images = self.images;
        let action = self.handler.on_trap(
            &mut View {
                state,
                memory,
                images,
            },
            vector as u8,
        );
        Ok(action == TrapAction::Park)
    }
}

impl Backend for Cpu {
    fn new_process(&mut self) -> Result<Self, String> {
        let mut child = Self::new(self.park, self.native)?;
        child.budget = self.budget;
        let sender = self.background_sender()?;
        child.service = self.service.clone();
        child.set_background_sender(sender)?;
        Ok(child)
    }

    type Context = Option<ExecutionContext>;
    fn setup_segments(&mut self, seg: SegmentSetup) -> Result<(), MemFault> {
        let Executor { state, memory, .. } = &mut self.inner;
        state.segments =
            SegmentState::wow64(memory, seg.gdt_addr).map_err(|_| fault(seg.gdt_addr, 4096))?;
        Ok(())
    }
    fn set_fs_base(&mut self, base: u32) -> Result<(), MemFault> {
        let Executor { state, memory, .. } = &mut self.inner;
        state
            .segments
            .set_fs_base(memory, base)
            .map_err(|_| fault(base, 1))?;
        state.fs_base = base;
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
        let start = Instant::now();
        self.inner
            .set_deadline(self.budget.and_then(|b| start.checked_add(b)));
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
            // The deadline bounds the run, not the fuel: a short fuel would
            // leave units that do not fit it to the interpreter.
            let images = Rc::clone(&m.images);
            let mut raw = Raw {
                handler,
                images: &images,
            };
            let result = m.run_with_traps(self.park, FUEL, &mut raw, !self.native);
            self.retired += FUEL - m.state.fuel;
            let kind = match result {
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
                Ok(RunOutcome::ReadFault { address, .. }) => FaultKind::Read(address),
                Ok(RunOutcome::WriteFault { address, .. }) => FaultKind::Write(address),
                Ok(RunOutcome::FetchFault { address, .. }) => FaultKind::Fetch(address),
                Ok(RunOutcome::InvalidInstruction { .. }) => FaultKind::InvalidInstruction,
                Ok(RunOutcome::UnsupportedInstruction { pc, code }) => {
                    FaultKind::Other(format!("unsupported guest instruction {code:?} at {pc:#x}"))
                }
                Ok(RunOutcome::Trap { vector, .. }) => {
                    FaultKind::Other(format!("unhandled guest exception {vector}"))
                }
                Err(e) => FaultKind::Other(e),
            };
            return Exit::Fault(Fault {
                kind,
                pc: m.state.eip,
            });
        }
    }
    fn retired_instructions(&self) -> u64 {
        self.retired
    }
    fn invalidate_code(&mut self, addr: u32, len: u32) {
        let memory = &mut self.inner.memory;
        if memory.range(addr, len as usize).is_ok() {
            memory.touch(addr, len as usize);
        }
    }
}

impl Engine for Cpu {
    fn create(park: u32, native: bool) -> Result<Self, String> {
        Self::new(park, native)
    }

    /// Translation stays synchronous; diagnostics may count exact Tier 0 retirement.
    fn configure(&mut self, options: &EngineOptions) -> Result<(), String> {
        self.inner
            .count_tier0(options.profiling || options.frame_timing);
        Ok(())
    }

    fn tier0_instructions(&self) -> Option<u64> {
        self.inner.tier0_instructions()
    }

    fn set_waker(&mut self, wake: WakeEvent) -> Result<(), String> {
        Cpu::set_waker(self, wake)
    }

    fn set_background_sender(&mut self, sender: LocalSender) -> Result<(), String> {
        Cpu::set_background_sender(self, sender)
    }

    fn background_sender(&mut self) -> Result<LocalSender, String> {
        Cpu::background_sender(self)
    }

    fn counters(&self) -> EngineCounters {
        let stats = &self.inner.stats;
        EngineCounters {
            native_entries: stats.native_entries,
            interpreter_entries: stats.interpreter_entries,
        }
    }

    fn report_fields(&self) -> String {
        let stats = &self.inner.stats;
        let forms = |which| {
            self.inner
                .interpreted_forms(which)
                .iter()
                .take(64)
                .map(|(form, count)| format!("{}: {count}", json(&format!("{form:?}"))))
                .collect::<Vec<_>>()
                .join(", ")
        };
        format!(
            concat!(
                "\"engine\": \"a64\",\n  \"native_entries\": {},\n  \"interpreter_entries\": {},\n",
                "  \"interpreter_instructions\": {},\n  \"compilations\": {},\n",
                "  \"native_code_bytes\": {},\n  \"tier_verification\": {},\n",
                "  \"a64_diagnostics\": {},\n",
                "  \"interpreted_forms\": {{{}}},
  \"fault_forms\": {{{}}},
",
                "  \"helper_forms\": {{{}}}"
            ),
            stats.native_entries,
            stats.interpreter_entries,
            stats.interpreter_instructions,
            stats.compilations,
            stats.native_code_bytes,
            stats.verification.json(),
            json(&self.inner.diagnostics()),
            forms(0),
            forms(1),
            forms(2),
        )
    }

    fn profile(&self) -> BTreeMap<u32, adv_cpu_core::ProfileSample> {
        BTreeMap::new()
    }
}

#[cfg(test)]
mod baseline_tests {
    use super::*;
    // spec: EXEC-CORE-5
    #[test]
    fn exact_tier0_counting_is_diagnostic_and_counts_rep_once() {
        let mut cpu = Cpu::new(0x12000, false).unwrap();
        assert_eq!(cpu.tier0_instructions(), None);
        cpu.configure(&EngineOptions {
            profiling: true,
            ..EngineOptions::default()
        })
        .unwrap();
        let executor = cpu.executor_mut();
        executor.map(0x10000, 4096, 7).unwrap();
        executor.map(0x20000, 4096, 3).unwrap();
        executor.write(0x10000, &[0xf3, 0xaa]).unwrap(); // authored REP STOSB
        executor.state.eip = 0x10000;
        executor.state.regs[1] = 10;
        executor.state.regs[7] = 0x20000;
        assert_eq!(executor.run(0x10002, 100).unwrap(), RunOutcome::ReachedStop);
        assert_eq!(executor.stats.interpreter_instructions, 10);
        assert_eq!(cpu.tier0_instructions(), Some(1));
    }
}
