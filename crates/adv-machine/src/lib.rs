//! Guest machine core: address space, guest contexts, host-call dispatch, scheduler.
//!
//! * Host functions are plain `fn(&mut Env) -> Outcome`, found by name through a
//!   [`thunk::Resolver`] (the ABI layer's registry). The loader binds imports to hookable
//!   guest thunks.
//! * A thunk trap runs the handler inside the CPU hook. `Return` and `Call` complete in-hook
//!   (no emulation restart), except an optional real-time frame-boundary host yield.
//!   Blocking, exit and failure also park the guest. [`Machine::run`] owns re-entry.
//! * Each guest context has its own stack, TEB (FS base), registers, and continuation
//!   stack. Names are diagnostic labels; scheduling depends only on readiness and
//!   the urgency of the event that woke it, never an engine-specific thread role.

pub mod clock;
pub mod exception;
pub mod ext;
pub mod frame_budget;
mod invocation;
pub mod layout;
pub mod objects;
pub mod outcome;
pub mod patch;
pub mod process;
pub mod sched;
pub mod schedule_trace;
pub mod scheduling;
#[cfg(test)]
mod spawn_tests;
pub mod stats;
pub mod thunk;
pub mod trace;
pub mod vm;
pub mod wait_diagnostics;
mod wake_queue;
pub use wake_queue::WakePriority;

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use adv_cpu::backend::asm::Asm;
use adv_cpu::backend::{
    Backend, Exit, Guest, GuestExt, MemFault, Prot, Reg, SegmentSetup, TrapAction, TrapHandler,
};

pub use clock::{Clock, MAX_PASSIVE_STEP_US};
pub use ext::Ext;
use layout::*;
pub use objects::{Identity, Namespace, ObjectLimits};
pub use outcome::{Continuation, Outcome, Resume, WakeReason};
pub use patch::Detour;
pub use process::{Image, Images, InitCall, TlsImage};
use sched::{Context, ContextStorage, Frame, Origin, ReadyResume, State};
pub use sched::{CtxId, Handle, Object, Objects, Wait};
pub use stats::{FrameRow, Stats};
pub use thunk::{Conv, GuestCode, Handler, HostFn, Resolver, ThunkTable};
pub use trace::{Trace, TraceRecord};
pub use vm::Vm;

fn allocate_tls_block(vm: &mut Vm, cpu: &mut dyn Guest, image: &TlsImage) -> Result<u32, MemFault> {
    let size = u32::try_from(image.template.len())
        .ok()
        .and_then(|n| n.checked_add(image.zero_fill))
        .ok_or(MemFault {
            addr: 0,
            len: u32::MAX,
        })?;
    let block = vm.alloc(cpu, size.max(1), Prot::RW)?;
    if let Err(error) = cpu.write(block, &image.template) {
        let _ = vm.free(cpu, block);
        return Err(error);
    }
    Ok(block)
}

/// Host-side machine state shared by all handlers.
pub struct Sys {
    pub clock: Clock,
    /// Explicit arguments exist only while a host invocation or its continuation runs.
    invocation_arguments: Option<invocation::Arguments>,
    /// Real time: reusable scheduler slot where round-robin resumes after a host return.
    rr_last: Option<CtxId>,
    pub trace: Trace,
    pub thunks: ThunkTable,
    pub objects: Objects,
    pub vm: Vm,
    pub contexts: Vec<Context>,
    /// The context currently running (or last run).
    pub current: CtxId,
    /// Presentation sequence for diagnostics, advanced by the graphics front.
    pub frame: u32,
    /// Complete presentations available to the frontend. This counts committed
    /// device output, not a guest thread role or an engine-specific frame loop.
    pub completed_frames: u32,
    pub pid: u32,
    pub process_identity: std::sync::Arc<process::ProcessIdentity>,
    next_tid: u32,
    /// Mapped guest images, for backtraces and profiles; the OS layer's loader fills it.
    pub images: Images,
    /// Static TLS templates, by TLS index.
    pub tls_images: Vec<TlsImage>,
    /// Initializers run once on the main context before its requested execution starts.
    pub process_init: VecDeque<InitCall>,
    /// Frontend-supplied boundary calls after image initialization and before
    /// the application entry point (for example, releasing its startup gate).
    pub process_ready: Vec<InitCall>,
    /// Application code is separate from the one-shot process initialization queue.
    process_entry: Option<u32>,
    process_start_entry: Option<u32>,
    guest_return_entry: u32,
    thread_exception_entry: Option<u32>,
    /// Frontend termination policy when the process entry point returns.
    pub process_return: Option<fn(&mut Env<'_>, u32) -> Outcome>,
    /// Personality infrastructure that must exist before the initial context.
    pub process_prepare: Option<fn(&mut dyn Guest, &mut Sys, &mut Ext) -> Result<(), MemFault>>,
    /// Personality-supplied interpretation of precise CPU stops.
    pub exception_handler: Option<exception::ExceptionHandler>,
    /// First backend fault, retained across guest SEH execution and later slices.
    pub first_cpu_fault: Option<exception::CpuFaultSnapshot>,
    /// Most recent backend fault; recoverable faults do not hide later evidence.
    pub latest_cpu_fault: Option<exception::CpuFaultSnapshot>,
    /// Personality terminal status for an unhandled guest exception.
    pub guest_failure_code: Option<u32>,
    checkpoint_identity: std::sync::Arc<()>,
    next_frame_id: u64,
    /// The last host calls, for failure reports.
    pub recent: RecentCalls,
    /// Host calls handled so far.
    pub host_calls: u64,
    /// Stop with an error after this many host calls (a debugging aid for guest loops that
    /// never block).
    pub call_limit: Option<u64>,
    /// Host-side cost accounting; timings only when enabled.
    pub stats: Stats,
    /// Scheduler latency observations, independent of optional execution profiling.
    pub scheduling: scheduling::SchedulingStats,
    pub schedule_trace: schedule_trace::ScheduleTrace,
    wakes: sched::ActiveContexts,
    /// Stopped contexts whose execution storage still needs to be reclaimed.
    finished_contexts: Vec<CtxId>,
    #[cfg(test)]
    scheduler_work: sched::SchedulerWork,
}

/// Ring buffer of recent host calls: (context, thunk index, guest return address).
pub struct RecentCalls {
    buf: VecDeque<(CtxId, u32, u32)>,
    cap: usize,
}

impl RecentCalls {
    pub fn new(cap: usize) -> Self {
        Self {
            buf: VecDeque::with_capacity(cap),
            cap,
        }
    }

    pub fn push(&mut self, ctx: CtxId, thunk: u32, caller: u32) {
        if self.buf.len() == self.cap {
            self.buf.pop_front();
        }
        self.buf.push_back((ctx, thunk, caller));
    }

    pub fn iter(&self) -> impl Iterator<Item = &(CtxId, u32, u32)> {
        self.buf.iter()
    }
}

/// What a handler sees.
pub struct Env<'a> {
    /// Guest identity of this call; independent of the controller's diagnostic current slot.
    pub context: CtxId,
    pub cpu: &'a mut dyn Guest,
    pub sys: &'a mut Sys,
    pub ext: &'a mut Ext,
    /// Index of the thunk being handled.
    pub thunk: u32,
}

impl Env<'_> {
    /// `i`th stack argument of the current host call. Unreadable args read as 0 (and log).
    pub fn arg(&self, i: u32) -> u32 {
        if let Some(arguments) = &self.sys.invocation_arguments
            && arguments.context == self.context
            && arguments.thunk == self.thunk
        {
            return arguments.values.get(i as usize).copied().unwrap_or(0);
        }
        self.cpu.arg(i).unwrap_or_else(|e| {
            log::warn!("arg {i} unreadable: {e}");
            0
        })
    }

    /// Guest return address of the current host call.
    pub fn caller(&self) -> u32 {
        self.cpu.read_u32(self.cpu.reg(Reg::Esp)).unwrap_or(0)
    }

    pub fn ctx(&self) -> &Context {
        &self.sys.contexts[self.context as usize]
    }
}

impl Sys {
    /// Assign a distinct process identity before creating any guest context.
    pub fn set_process_identity(&mut self, pid: u32) -> Result<(), &'static str> {
        if !self.contexts.is_empty() || pid > u32::MAX - 0x1000 {
            return Err("process identity already published");
        }
        self.pid = pid;
        self.next_tid = pid + 4;
        self.process_identity = process::ProcessIdentity::new(pid);
        Ok(())
    }
    pub fn thread_identity(&self, id: CtxId) -> Option<process::ThreadIdentity> {
        let context = self.contexts.get(id as usize)?;
        Some(process::ThreadIdentity {
            process: self.process_identity.clone(),
            context: id,
            tid: context.tid,
            completion: context.completion.clone(),
        })
    }
    pub fn suspend_context(&mut self, id: CtxId) -> Option<u32> {
        let context = self.contexts.get_mut(id as usize)?;
        if context.is_done() || context.suspend_count == 127 {
            return None;
        }
        let old = context.suspend_count;
        context.suspend_count += 1;
        Some(old)
    }
    pub fn resume_context(&mut self, id: CtxId) -> Option<u32> {
        let context = self.contexts.get_mut(id as usize)?;
        if context.is_done() {
            return None;
        }
        let old = context.suspend_count;
        context.suspend_count = old.saturating_sub(1);
        Some(old)
    }
    /// Create a guest context that will start at `entry(args...)`. Its return ends the thread.
    pub fn spawn(
        &mut self,
        cpu: &mut dyn Guest,
        name: &str,
        entry: u32,
        args: &[u32],
        stack_size: u32,
    ) -> Result<CtxId, MemFault> {
        self.reclaim_finished_storage(cpu)?;
        let id = u32::try_from(self.contexts.len()).map_err(|_| MemFault {
            addr: 0,
            len: stack_size,
        })?;
        let slot = (0..MAX_CONTEXTS)
            .find(|slot| self.wakes.entries().iter().all(|entry| entry.slot != *slot));
        let Some(slot) = slot.filter(|_| self.objects.can_insert()) else {
            return Err(MemFault {
                addr: 0,
                len: stack_size,
            });
        };
        let stack_size = stack_size.max(0x1_0000);
        let base = self.vm.alloc_with_guard(cpu, stack_size, PAGE, Prot::RW)?;
        let (lo, hi) = (base + PAGE, base + PAGE + stack_size);
        let tid = self.next_tid;
        let teb = teb_addr(slot);
        let mut mapped_teb = false;
        let result = (|| {
            cpu.map(teb, PAGE, Prot::RW)?;
            mapped_teb = true;
            for (off, v) in [
                (teb::EXCEPTION_LIST, u32::MAX),
                (teb::STACK_BASE, hi),
                (teb::STACK_LIMIT, lo),
                (teb::SELF, teb),
                (teb::PROCESS_ID, self.pid),
                (teb::THREAD_ID, tid),
                (teb::PEB, PEB_ADDR),
            ] {
                cpu.write_u32(teb + off, v)?;
            }

            if let Some(handler) = self.thread_exception_entry {
                let frame = hi - 0x10;
                cpu.write_u32(frame, u32::MAX)?;
                cpu.write_u32(frame + 4, handler)?;
                cpu.write_u32(teb + teb::EXCEPTION_LIST, frame)?;
            }

            // Finish fallible stack writes before TLS acquires its storage.
            let mut sp = hi - 0x10;
            for &a in args.iter().rev() {
                sp -= 4;
                cpu.write_u32(sp, a)?;
            }
            sp -= 4;
            cpu.write_u32(sp, self.guest_return_entry)?;
            let (tls_directory, tls_blocks) = self.setup_static_tls(cpu, teb)?;
            Ok((sp, tls_directory, tls_blocks))
        })();
        let (sp, tls_directory, tls_blocks) = match result {
            Ok(sp) => sp,
            Err(error) => {
                if mapped_teb {
                    let _ = cpu.unmap(teb, PAGE);
                }
                let _ = self.vm.free(cpu, base);
                return Err(error);
            }
        };

        let thread_handle = self.objects.insert(Object::Thread { ctx: id });
        self.contexts.push(Context {
            suspend_count: 0,
            completion: std::sync::Arc::new(process::Completion::default()),
            id,
            name: name.to_owned(),
            tid,
            teb,
            stack_lo: lo,
            stack_hi: hi,
            start: Some((sp, entry)),
            initial_registers: Vec::new(),
            storage: Some(ContextStorage {
                slot,
                stack: base,
                tls_directory,
                tls_blocks,
            }),
            bootstrap_pending: None,
            frames: Vec::new(),
            state: State::Ready { resume: None },
            yielded: false,
            thread_handle,
        });
        self.wakes.insert(
            id,
            self.contexts[id as usize]
                .storage
                .as_ref()
                .expect("new context owns execution storage")
                .slot,
        );
        self.next_tid += 4;
        Ok(id)
    }

    /// Allocate this thread's static TLS blocks and point TEB.ThreadLocalStoragePointer at
    /// the per-image block array.
    fn setup_static_tls(
        &mut self,
        cpu: &mut dyn Guest,
        teb: u32,
    ) -> Result<(u32, Vec<(u32, u32)>), MemFault> {
        // The directory stays fixed across later image loads; individual blocks
        // likewise keep their addresses while other modules arrive or retire.
        let directory = self.vm.alloc(cpu, PAGE, Prot::RW)?;
        let mut allocated = vec![directory];
        let mut blocks = Vec::new();
        let result = (|| {
            for image in self.tls_images.iter().filter(|image| image.active) {
                let block = allocate_tls_block(&mut self.vm, cpu, image)?;
                allocated.push(block);
                blocks.push((image.index, block));
                cpu.write_u32(directory + 4 * image.index, block)?;
            }
            cpu.write_u32(teb + teb::TLS_POINTER, directory)
        })();
        if result.is_err() {
            for block in allocated {
                let _ = self.vm.free(cpu, block);
            }
        }
        result.map(|()| (directory, blocks))
    }

    /// Retire stopped execution storage without changing retained thread handles.
    /// Native leases are quiesced before any address can be allocated again.
    fn reclaim_finished_storage(&mut self, cpu: &mut dyn Guest) -> Result<(), MemFault> {
        if self.finished_contexts.is_empty() {
            return Ok(());
        }
        while let Some(&id) = self.finished_contexts.last() {
            let context = &mut self.contexts[id as usize];
            let storage = context
                .storage
                .as_mut()
                .expect("retired context owns storage");
            while let Some(&(_, block)) = storage.tls_blocks.last() {
                self.vm.free(cpu, block)?;
                storage.tls_blocks.pop();
            }
            if storage.tls_directory != 0 {
                self.vm.free(cpu, storage.tls_directory)?;
                storage.tls_directory = 0;
            }
            if storage.stack != 0 {
                self.vm.free(cpu, storage.stack)?;
                storage.stack = 0;
            }
            cpu.unmap(context.teb, PAGE)?;
            context.storage = None;
            context.initial_registers.clear();
            context.bootstrap_pending = None;
            context.start = None;
            self.finished_contexts.pop();
        }
        Ok(())
    }

    fn active_contexts(&self) -> impl Iterator<Item = &Context> {
        self.wakes
            .entries()
            .iter()
            .map(|entry| &self.contexts[entry.id as usize])
    }

    fn retire_context(&mut self, id: CtxId) {
        if self.wakes.retire(id) {
            self.finished_contexts.push(id);
        }
    }

    /// Personality-owned image entry for callback returns.
    pub fn set_guest_return_entry(&mut self, entry: u32) {
        self.guest_return_entry = entry;
    }
    pub fn guest_return_entry(&self) -> u32 {
        self.guest_return_entry
    }
    /// Personality-owned image entry for the ordinary main-thread wrapper.
    pub fn set_process_start_entry(&mut self, entry: u32) {
        self.process_start_entry = Some(entry);
    }
    pub fn set_thread_exception_entry(&mut self, entry: u32) {
        self.thread_exception_entry = Some(entry);
    }
    pub fn thread_exception_entry(&self) -> Option<u32> {
        self.thread_exception_entry
    }
    /// Seed an unmaterialized context's architecture without selecting it.
    pub fn set_initial_registers(&mut self, id: CtxId, registers: &[(Reg, u32)]) -> bool {
        let Some(context) = self.contexts.get_mut(id as usize) else {
            return false;
        };
        if context.start.is_none() || context.is_done() {
            return false;
        }
        context.initial_registers.extend_from_slice(registers);
        true
    }

    /// Publish a static TLS template to all existing contexts transactionally.
    /// This is process storage, not an OS callback or a second scheduler.
    pub fn add_static_tls(
        &mut self,
        cpu: &mut dyn Guest,
        mut image: TlsImage,
    ) -> Result<u32, MemFault> {
        let index = self
            .tls_images
            .iter()
            .position(|image| !image.active)
            .unwrap_or(self.tls_images.len()) as u32;
        if index >= PAGE / 4 {
            return Err(MemFault { addr: 0, len: PAGE });
        }
        image.index = index;
        let mut allocated = Vec::new();
        let result = (|| {
            for context in self.contexts.iter().filter(|context| !context.is_done()) {
                let directory = cpu.read_u32(context.teb + teb::TLS_POINTER)?;
                if directory == 0 {
                    return Err(MemFault {
                        addr: context.teb,
                        len: 4,
                    });
                }
                let block = allocate_tls_block(&mut self.vm, cpu, &image)?;
                allocated.push((context.id, directory + 4 * index, block));
            }
            for &(_, slot, block) in &allocated {
                cpu.write_u32(slot, block)?;
            }
            Ok(())
        })();
        if let Err(error) = result {
            for (_, slot, block) in allocated {
                let _ = cpu.write_u32(slot, 0);
                let _ = self.vm.free(cpu, block);
            }
            return Err(error);
        }
        for (id, _, block) in allocated {
            self.contexts[id as usize]
                .storage
                .as_mut()
                .expect("live context owns its execution storage")
                .tls_blocks
                .push((index, block));
        }
        if index as usize == self.tls_images.len() {
            self.tls_images.push(image);
        } else {
            self.tls_images[index as usize] = image;
        }
        Ok(index)
    }

    pub fn remove_static_tls(&mut self, cpu: &mut dyn Guest, index: u32) -> Result<(), MemFault> {
        for context in self
            .contexts
            .iter_mut()
            .filter(|context| !context.is_done())
        {
            let directory = cpu.read_u32(context.teb + teb::TLS_POINTER)?;
            let slot = directory + index * 4;
            let block = cpu.read_u32(slot)?;
            cpu.write_u32(slot, 0)?;
            if block != 0 {
                self.vm.free(cpu, block)?;
                if let Some(storage) = &mut context.storage {
                    storage.tls_blocks.retain(|&(_, owned)| owned != block);
                }
            }
        }
        if let Some(image) = self.tls_images.get_mut(index as usize) {
            image.active = false;
            image.template.clear();
        }
        Ok(())
    }

    /// Human-readable list of the most recent host calls.
    pub fn recent_calls(&self) -> Vec<String> {
        self.recent
            .iter()
            .map(|&(ctx, thunk, caller)| {
                let t = self.thunks.get(thunk);
                format!("[{ctx}] {}!{} from {caller:#010x}", t.module, t.name)
            })
            .collect()
    }

    pub fn context(&self, id: CtxId) -> &Context {
        &self.contexts[id as usize]
    }

    pub fn current_context(&self) -> &Context {
        &self.contexts[self.current as usize]
    }

    /// Stop peers before an OS frontend runs process termination callbacks.
    /// No callback policy is embedded here; suspended host continuations drop
    /// with their context and thread objects become signaled through State::Done.
    pub fn terminate_other_contexts(&mut self, exit_code: u32) {
        let mut index = 0;
        while index < self.wakes.entries().len() {
            let id = self.wakes.entries()[index].id;
            if id == self.current {
                index += 1;
            } else {
                self.terminate_context_stopped(id, exit_code);
            }
        }
    }

    /// TerminateThread: end another context where it stands. Its host
    /// continuations drop and its thread object becomes signaled through
    /// State::Done; no thread-detach work runs, as on Windows.
    pub fn terminate_context(&mut self, id: CtxId, exit_code: u32) -> bool {
        if id == self.current {
            return false;
        }
        self.terminate_context_stopped(id, exit_code)
    }

    /// Supervisor-only termination after all execution leases have returned.
    /// Unlike a guest trap, this boundary may retire the currently loaded context.
    pub fn terminate_context_stopped(&mut self, id: CtxId, exit_code: u32) -> bool {
        let Some(context) = self.contexts.get_mut(id as usize) else {
            return false;
        };
        if context.is_done() {
            return false;
        }
        context.completion.complete(exit_code);
        context.state = State::Done { exit_code };
        context.frames.clear();
        context.start = None;
        context.bootstrap_pending = None;
        context.initial_registers.clear();
        self.retire_context(id);
        if let Some(diagnostics) = &mut self.stats.wait_diagnostics {
            diagnostics.retire(id);
        }
        if self.wakes.entries().is_empty() {
            self.process_identity.completion.complete(exit_code);
        }
        true
    }

    /// Complete a process at a supervisor boundary, without guest detach calls.
    pub fn terminate_process_stopped(&mut self, exit_code: u32) {
        while let Some(entry) = self.wakes.entries().first() {
            let id = entry.id;
            self.terminate_context_stopped(id, exit_code);
        }
        self.process_identity.completion.complete(exit_code);
    }

    /// Grant already blocked event waits now, without running their continuations.
    /// ResetEvent and a later waiter cannot revoke or steal an existing grant.
    pub fn set_event(&mut self, h: Handle) -> bool {
        self.set_event_with_priority(h, WakePriority::Normal)
    }

    /// Device/input delivery grants the same wait as SetEvent, then gives its
    /// newly runnable consumers a bounded, event-derived dispatch preference.
    pub fn set_event_with_priority(&mut self, h: Handle, priority: WakePriority) -> bool {
        let valid = match self.objects.get_mut(h) {
            Some(Object::Event { signaled, .. }) => {
                *signaled = true;
                true
            }
            _ => false,
        };
        if valid {
            self.signal_waiters_with_priority(h, priority);
        }
        valid
    }

    /// Grant existing waits after a semaphore/mutex becomes available, before
    /// the signaling context can immediately reacquire it.
    pub fn signal_waiters(&mut self, h: Handle) {
        self.signal_waiters_with_priority(h, WakePriority::Normal);
    }
    pub fn prefer_ready_context(&mut self, context: CtxId, priority: WakePriority) {
        if self
            .contexts
            .get(context as usize)
            .is_some_and(|c| c.is_ready())
        {
            self.wakes
                .wake_as(context, self.clock.now_us(), None, priority);
        }
    }
    fn signal_waiters_with_priority(&mut self, h: Handle, priority: WakePriority) {
        let now = self.clock.now_us();
        for index in 0..self.wakes.entries().len() {
            let i = self.wakes.entries()[index].id as usize;
            let waiting = match &self.contexts[i].state {
                State::Blocked {
                    wait: Wait::Object { handle, .. },
                    ..
                } => self.objects.same_object(*handle, h),
                State::Blocked {
                    wait:
                        Wait::Objects {
                            handles,
                            notification,
                            ..
                        },
                    ..
                } => {
                    handles
                        .iter()
                        .any(|&handle| self.objects.same_object(handle, h))
                        || notification.is_some_and(|handle| self.objects.same_object(handle, h))
                }
                _ => false,
            };
            if waiting && let Some(reason) = self.poll_wait(i, now) {
                self.release_waiter(i, reason, now);
                self.wakes.wake_as(i as CtxId, now, None, priority);
            }
        }
    }

    /// Mark a fully committed frame, after every guest draw callback has returned.
    /// A host handler can then return normally or defer its following work.
    pub fn complete_frame(&mut self) {
        self.completed_frames = self.completed_frames.wrapping_add(1);
        if let Some(budget) = &mut self.stats.frame_budget {
            budget.mark_frame(u64::from(self.completed_frames));
        }
        self.schedule_trace.event(
            "frame",
            self.current,
            u64::from(self.completed_frames),
            0,
            0,
        );
    }

    pub fn reset_event(&mut self, h: Handle) -> bool {
        match self.objects.get_mut(h) {
            Some(Object::Event { signaled, .. }) => {
                *signaled = false;
                true
            }
            _ => false,
        }
    }

    /// Non-blocking check of a waitable object (consumes an auto-reset signal).
    pub fn try_acquire(&mut self, h: Handle) -> bool {
        self.wake();
        let cur = self.current;
        sched::poll_object(&mut self.objects, &self.contexts, h, cur, true)
    }

    /// Acquire a wait set without partially consuming an unsatisfied WaitAll.
    pub fn try_acquire_objects(&mut self, handles: &[Handle], all: bool) -> Option<WakeReason> {
        self.wake();
        sched::poll_objects(
            &mut self.objects,
            &self.contexts,
            handles,
            self.current,
            all,
        )
    }

    fn poll_wait(&mut self, i: usize, now: u64) -> Option<WakeReason> {
        #[cfg(test)]
        self.scheduler_work
            .polls
            .set(self.scheduler_work.polls.get() + 1);
        let State::Blocked { wait, .. } = &self.contexts[i].state else {
            return None;
        };
        match wait {
            Wait::Yield => Some(WakeReason::Timeout),
            Wait::Until(t) => (now >= *t).then_some(WakeReason::Timeout),
            Wait::Object { handle, deadline } => {
                let (handle, deadline) = (*handle, *deadline);
                if sched::poll_object(&mut self.objects, &self.contexts, handle, i as CtxId, true) {
                    Some(WakeReason::Signaled)
                } else {
                    deadline
                        .is_some_and(|d| now >= d)
                        .then_some(WakeReason::Timeout)
                }
            }
            Wait::Objects {
                handles,
                all,
                deadline,
                notification,
            } => sched::poll_objects(&mut self.objects, &self.contexts, handles, i as CtxId, *all)
                .or_else(|| {
                    notification
                        .filter(|&h| {
                            sched::poll_object(
                                &mut self.objects,
                                &self.contexts,
                                h,
                                i as CtxId,
                                true,
                            )
                        })
                        .map(|_| WakeReason::Notified)
                })
                .or_else(|| {
                    deadline
                        .is_some_and(|d| now >= d)
                        .then_some(WakeReason::Timeout)
                }),
        }
    }

    /// Wake every blocked context whose wait is satisfied, in context order.
    fn wake(&mut self) {
        let now = self.clock.now_us();
        self.objects.expire_timers(now);
        for index in 0..self.wakes.entries().len() {
            let i = self.wakes.entries()[index].id as usize;
            let reason = self.poll_wait(i, now);
            if let Some(reason) = reason {
                self.release_waiter(i, reason, now);
            }
        }
    }

    fn release_waiter(&mut self, i: usize, reason: WakeReason, now: u64) {
        let State::Blocked {
            wait, then, origin, ..
        } = std::mem::replace(&mut self.contexts[i].state, State::Ready { resume: None })
        else {
            unreachable!("only blocked contexts have a pending wait")
        };
        let deadline = if matches!(reason, WakeReason::Timeout) {
            match wait {
                Wait::Until(t) => Some(t),
                Wait::Object { deadline, .. } | Wait::Objects { deadline, .. } => deadline,
                Wait::Yield => None,
            }
        } else {
            None
        };
        if let Some(deadline) = deadline {
            self.scheduling.timed_wake(now, deadline);
        }
        if let Some(diagnostics) = &mut self.stats.wait_diagnostics {
            diagnostics.wake(i as CtxId, reason, now);
        }
        self.wakes.wake(i as CtxId, now, deadline);
        self.schedule_trace
            .event("wake", i as u32, u64::from(origin.thunk), 0, 0);
        self.contexts[i].state = State::Ready {
            resume: Some(ReadyResume::Woke {
                then,
                origin,
                reason,
            }),
        };
    }

    /// Virtual time: let every yielding context resume before recognizing a
    /// polling round. A single Sleep(0) or empty PeekMessage can precede a
    /// signal; it must not jump straight to another context's long timeout.
    fn advance_if_only_polling(&mut self) {
        if !self.clock.is_virtual()
            || !self.only_polling()
            || !self
                .wakes
                .entries()
                .iter()
                .all(|entry| !self.contexts[entry.id as usize].is_ready() || entry.polled)
        {
            return;
        }
        let soon = self.clock.now_us() + 1000;
        let t = self
            .next_deadline()
            .map_or(soon, |deadline| deadline.min(soon));
        self.stats.total.polls += 1;
        self.clock.advance_to(t);
        self.wakes.clear_polled();
        self.wake();
    }

    /// Some context is ready, and every ready one is only yielding (a polling loop).
    fn only_polling(&self) -> bool {
        let mut ready = self.active_contexts().filter(|c| c.is_ready()).peekable();
        ready.peek().is_some() && ready.all(|c| c.yielded)
    }

    fn next_deadline(&self) -> Option<u64> {
        self.active_contexts()
            .filter_map(|c| match &c.state {
                State::Blocked {
                    wait: Wait::Until(t),
                    ..
                } => Some(*t),
                State::Blocked {
                    wait: Wait::Object { deadline, .. } | Wait::Objects { deadline, .. },
                    ..
                } => *deadline,
                _ => None,
            })
            .chain(self.objects.next_timer_deadline())
            .min()
    }

    /// Arm a neutral waitable timer against the one guest clock. Set clears
    /// its signal, but cannot revoke a grant already made to a blocked waiter.
    pub fn arm_timer(&mut self, handle: Handle, deadline: u64, period_us: u64) -> bool {
        self.wake();
        if !self.objects.arm_timer(handle, deadline, period_us) {
            return false;
        }
        self.wake();
        true
    }

    /// Cancel future expirations without clearing an already available signal.
    pub fn cancel_timer(&mut self, handle: Handle) -> bool {
        self.wake();
        self.objects.cancel_timer(handle)
    }
}

/// Module name of machine-internal host functions.
pub const INTERNAL_MODULE: &str = "<machine>";

/// Run initialization independently of the main context's requested instruction pointer.
fn process_bootstrap(env: &mut Env<'_>, checkpoint: exception::Checkpoint) -> Outcome {
    fn next(env: &mut Env<'_>, mut checkpoint: exception::Checkpoint) -> Outcome {
        match env.sys.process_init.pop_front() {
            None => {
                // Startup calls may establish the application's floating environment.
                // Restore requested control/integer state without discarding those effects.
                let floating = match env.cpu.capture_architecture() {
                    Ok(state) => state,
                    Err(error) => {
                        return Outcome::Fail(format!(
                            "capture initialized floating state: {error}"
                        ));
                    }
                };
                checkpoint.state.x87 = floating.x87;
                checkpoint.state.xmm = floating.xmm;
                checkpoint.state.mxcsr = floating.mxcsr;
                Outcome::ResumeExecution(checkpoint)
            }
            Some(c) => {
                log::debug!("process init: {} at {:#010x}", c.what, c.target);
                if let Some((begin, token)) = c.begin {
                    begin(env, token);
                }
                let failure_exit = c.failure_exit;
                Outcome::call(c.target, c.args, move |env, r| {
                    let v = r.value();
                    if v == 0
                        && let Some(status) = failure_exit
                    {
                        return match env.sys.process_return {
                            Some(finish) => finish(env, status),
                            None => Outcome::ExitProcess(status),
                        };
                    }
                    next(env, checkpoint)
                })
            }
        }
    }
    env.sys.process_init.extend(env.sys.process_ready.drain(..));
    next(env, checkpoint)
}

/// The ordinary application entry wrapper; redirected contexts do not enter it.
pub fn process_entry(env: &mut Env<'_>) -> Outcome {
    let Some(entry) = env.sys.process_entry else {
        return Outcome::Fail("application entry was already consumed".into());
    };
    process_entry_at(env, entry)
}

/// Enter the main application at the personality's requested target after bootstrap.
pub fn process_entry_at(env: &mut Env<'_>, entry: u32) -> Outcome {
    if env.sys.process_entry.take().is_none() {
        return Outcome::Fail("application entry was already consumed".into());
    }
    Outcome::call(entry, [], |env, result| match env.sys.process_return {
        Some(finish) => finish(env, result.value()),
        None => Outcome::ExitProcess(result.value()),
    })
}

/// Why a context stopped running inside a trap.
enum Parked {
    HostRequest {
        then: outcome::DeferredContinuation,
        origin: Origin,
    },
    /// Return to the host after committing a frame-producing trap, without a guest wait.
    HostYield {
        resume_eip: u32,
    },
    Deferred {
        then: outcome::DeferredContinuation,
        origin: Origin,
    },
    Block {
        wait: Wait,
        then: Continuation,
        origin: Origin,
        allow_time_skip: bool,
    },
    ExitThread(u32),
    ExitProcess(u32),
    Fail(String),
    Exception(u8),
}

/// Apply a handler's outcome to the guest. `Err` means the context must park.
fn apply(
    cpu: &mut dyn Guest,
    sys: &mut Sys,
    ext: &mut Ext,
    mut out: Outcome,
    origin: Origin,
    defer_work: bool,
) -> Result<(), Parked> {
    // A deferred boundary is a host continuation, never a Sleep or an object wake.
    // Keep unsliced and virtual execution inline, with no extra context switch.
    while let Outcome::Defer { then } = out {
        if defer_work {
            return Err(Parked::Deferred { then, origin });
        }
        let thunk = origin.thunk;
        out = then(&mut Env {
            context: sys.current,
            cpu: &mut *cpu,
            sys: &mut *sys,
            ext: &mut *ext,
            thunk,
        });
    }
    match out {
        Outcome::Return(_)
        | Outcome::Return64(_)
        | Outcome::ReturnF32(_)
        | Outcome::ReturnF64(_)
            if origin.interrupted =>
        {
            Err(Parked::Fail(
                "CPU interruption returned without an architectural resume".into(),
            ))
        }
        Outcome::Return(v) => {
            finish(cpu, sys, origin, u64::from(v), false);
            Ok(())
        }
        Outcome::Return64(v) => {
            finish(cpu, sys, origin, v, true);
            Ok(())
        }
        Outcome::ReturnF32(v) => {
            let eax = cpu.reg(Reg::Eax);
            cpu.push_f32(v);
            finish(cpu, sys, origin, u64::from(v.to_bits()), false);
            cpu.set_reg(Reg::Eax, eax);
            Ok(())
        }
        Outcome::ReturnF64(v) => {
            let (eax, edx) = (cpu.reg(Reg::Eax), cpu.reg(Reg::Edx));
            cpu.push_f64(v);
            finish(cpu, sys, origin, v.to_bits(), true);
            cpu.set_reg(Reg::Eax, eax);
            cpu.set_reg(Reg::Edx, edx);
            Ok(())
        }
        Outcome::Call {
            target,
            args,
            then,
            stack,
            unwindable,
        } => {
            let stack = stack.unwrap_or(origin.esp);
            let Some(bytes) = args
                .len()
                .checked_add(1)
                .and_then(|n| u32::try_from(n).ok())
                .and_then(|n| n.checked_mul(4))
            else {
                return Err(Parked::Fail(
                    "guest callback arguments exceed address space".into(),
                ));
            };
            let Some(bottom) = stack.checked_sub(bytes) else {
                return Err(Parked::Fail("guest callback stack underflow".into()));
            };
            if stack > origin.esp || !cpu.accessible(bottom, bytes, Prot::WRITE) {
                return Err(Parked::Fail(
                    "guest callback stack is not writable below its origin".into(),
                ));
            }
            let Some(id) = sys.next_frame_id.checked_add(1) else {
                return Err(Parked::Fail("continuation identity exhausted".into()));
            };
            let push = |cpu: &mut dyn Guest, sp: &mut u32, v: u32| {
                *sp -= 4;
                cpu.write_u32(*sp, v)
            };
            let mut sp = stack;
            let pushed = args
                .iter()
                .rev()
                .try_for_each(|&a| push(cpu, &mut sp, a))
                .and_then(|()| push(cpu, &mut sp, sys.guest_return_entry));
            if let Err(e) = pushed {
                return Err(Parked::Fail(format!(
                    "guest call to {target:#x}: stack write failed: {e}"
                )));
            }
            cpu.set_reg(Reg::Esp, sp);
            cpu.set_reg(Reg::Eip, target);
            sys.next_frame_id = id;
            sys.contexts[sys.current as usize].frames.push(Frame {
                id,
                stack,
                return_floor: bottom + 4,
                unwindable,
                then,
                origin,
            });
            Ok(())
        }
        Outcome::ResumeExecution(checkpoint) => checkpoint.restore(cpu, sys).map_err(Parked::Fail),
        Outcome::Block {
            wait,
            then,
            allow_time_skip,
        } => Err(Parked::Block {
            wait,
            then,
            origin,
            allow_time_skip,
        }),
        Outcome::Defer { .. } => unreachable!("deferred outcomes were resolved above"),
        Outcome::HostRequest { then } => Err(Parked::HostRequest { then, origin }),
        Outcome::ExitThread(c) => {
            trace_terminal(cpu, sys, &origin, "thread", c);
            Err(Parked::ExitThread(c))
        }
        Outcome::ExitProcess(c) => {
            trace_terminal(cpu, sys, &origin, "process", c);
            Err(Parked::ExitProcess(c))
        }
        Outcome::Fail(msg) => Err(Parked::Fail(msg)),
    }
}

fn trace_terminal(cpu: &dyn Guest, sys: &mut Sys, origin: &Origin, kind: &str, code: u32) {
    if !sys.trace.enabled() {
        return;
    }
    let binding = sys.thunks.get(origin.thunk);
    let record = TraceRecord {
        ctx: sys.current,
        frame: sys.frame,
        module: &binding.module,
        name: &binding.name,
        args: origin.trace_args.as_deref().unwrap_or(&[]),
        ret: u64::from(code),
    };
    sys.trace.terminal(
        &record,
        kind,
        origin.esp,
        origin.ret_eip,
        cpu.read_u32(origin.esp).ok(),
    );
}

/// Return from the host call described by `origin`.
fn finish(cpu: &mut dyn Guest, sys: &mut Sys, origin: Origin, v: u64, wide: bool) {
    if let Some(args) = &origin.trace_args {
        let t = sys.thunks.get(origin.thunk);
        if let Conv::CdeclVariadic { fixed } = t.f.conv {
            sys.trace.note_variadic(&t.module, &t.name, fixed);
        }
        let rec = TraceRecord {
            ctx: sys.current,
            frame: sys.frame,
            module: &t.module,
            name: &t.name,
            args,
            ret: v,
        };
        sys.trace.record(&rec);
    }
    cpu.set_reg(Reg::Eax, v as u32);
    if wide {
        cpu.set_reg(Reg::Edx, (v >> 32) as u32);
    }
    // In the common case (a direct thunk trap) both already match; writing EIP would force
    // a translation restart, so only touch what changed.
    if cpu.reg(Reg::Esp) != origin.esp {
        cpu.set_reg(Reg::Esp, origin.esp);
    }
    if cpu.reg(Reg::Eip) != origin.ret_eip {
        cpu.set_reg(Reg::Eip, origin.ret_eip);
    }
}

struct Dispatch<'a> {
    sys: &'a mut Sys,
    ext: &'a mut Ext,
    parked: Option<Parked>,
    yield_on_frame: bool,
    defer_work: bool,
}

/// Return addresses found by scanning the guest stack: values inside an image that follow
/// a call instruction (`call rel32` or `call [mem]`). Heuristic, for diagnostics.
pub fn guess_backtrace(cpu: &dyn Guest, images: &Images, esp: u32) -> Vec<u32> {
    let mut out = Vec::new();
    for i in 0..0x800 {
        let Ok(v) = cpu.read_u32(esp + 4 * i) else {
            break;
        };
        if images.containing(v).is_none() {
            continue;
        }
        let mut b = [0u8; 6];
        if cpu.read(v.wrapping_sub(6), &mut b).is_err() {
            continue;
        }
        if b[1] == 0xe8 || (b[0] == 0xff && b[1] == 0x15) || (b[4] == 0xff && b[5] & 0x38 == 0x10) {
            out.push(v);
            if out.len() == 16 {
                break;
            }
        }
    }
    out
}

impl Dispatch<'_> {
    /// Drop callback frames the guest has left by nonlocal unwind. A native
    /// exception dispatcher never regains control from a handler that resumes
    /// in an `__except` block or catch continuation, so frames declared
    /// unwindable are abandoned and their continuations dropped. Escaping any
    /// other frame still fails, naming the host call that frame belongs to.
    fn unwind_escaped(
        &mut self,
        cpu: &dyn Guest,
        escaped: impl Fn(&Frame) -> bool,
        what: &str,
    ) -> Result<(), Parked> {
        let vm = &self.sys.vm;
        let context = &mut self.sys.contexts[self.sys.current as usize];
        let thread_stack = (context.stack_lo, context.stack_hi);
        let frames = &mut context.frames;
        while let Some(frame) = frames.last().filter(|frame| escaped(frame)) {
            if !frame.unwindable {
                let host = self.sys.thunks.get(frame.origin.thunk);
                let esp = cpu.reg(Reg::Esp);
                let reservation = |address| {
                    vm.containing(address).map_or_else(
                        || "unreserved".to_owned(),
                        |(base, size)| format!("{base:#010x}+{size:#x}"),
                    )
                };
                return Err(Parked::Fail(format!(
                    "guest {what} escaped its continuation stack; nonlocal unwind is unsupported \
                     across {}!{}; current_pc={:#010x}, current_esp={esp:#010x}, \
                     frame_id={}, frame_stack={:#010x}, return_floor={:#010x}, \
                     origin_esp={:#010x}, thread_stack={:#010x}..{:#010x}, \
                     current_reservation={}, frame_reservation={}, origin_reservation={}",
                    host.module,
                    host.name,
                    cpu.reg(Reg::Eip),
                    frame.id,
                    frame.stack,
                    frame.return_floor,
                    frame.origin.esp,
                    thread_stack.0,
                    thread_stack.1,
                    reservation(esp),
                    reservation(frame.stack),
                    reservation(frame.origin.esp)
                )));
            }
            frames.pop();
        }
        Ok(())
    }

    fn remember_cpu_fault(&mut self, snapshot: exception::CpuFaultSnapshot) {
        self.sys
            .first_cpu_fault
            .get_or_insert_with(|| snapshot.clone());
        self.sys.latest_cpu_fault = Some(snapshot);
    }

    fn interrupt_snapshot(
        &self,
        cpu: &dyn Guest,
        vector: u8,
        pc: u32,
    ) -> exception::CpuFaultSnapshot {
        exception::CpuFaultSnapshot::capture(
            cpu,
            self.sys.current,
            &adv_cpu::backend::Fault {
                pc,
                kind: adv_cpu::backend::FaultKind::Other(format!(
                    "unhandled CPU exception/interrupt {vector:#x}"
                )),
            },
        )
    }

    fn exception(
        &mut self,
        cpu: &mut dyn Guest,
        event: exception::Exception,
    ) -> Result<(), Parked> {
        // A backend fault is an input event, not a completed host call. Emit it
        // before unwinding or invoking handlers; traps already have raw_interrupt.
        if let exception::Exception::Fault(fault) = &event {
            self.sys
                .trace
                .cpu_fault(self.sys.current, self.sys.frame, fault);
        }
        let esp = cpu.reg(Reg::Esp);
        self.unwind_escaped(cpu, |frame| esp >= frame.stack, "exception")?;
        // Retain the original stop before a personality can edit registers or
        // a parked backend replaces EIP. Handled traps do not publish a fault.
        let trap_snapshot = match &event {
            exception::Exception::Trap { vector, pc } => {
                Some(self.interrupt_snapshot(cpu, *vector, *pc))
            }
            exception::Exception::Fault(_) => None,
        };
        if let exception::Exception::Fault(fault) = &event {
            let snapshot = exception::CpuFaultSnapshot::capture(cpu, self.sys.current, fault);
            self.remember_cpu_fault(snapshot);
        }
        let Some(handler) = self.sys.exception_handler else {
            if let Some(snapshot) = trap_snapshot {
                self.remember_cpu_fault(snapshot);
            }
            return Err(Parked::Fail(format!("unhandled CPU stop: {event:?}")));
        };
        let checkpoint = exception::Checkpoint::capture(cpu, self.sys)
            .map_err(|error| Parked::Fail(format!("capture exception context: {error}")))?;
        let origin = Origin {
            esp: checkpoint.state.regs[4],
            ret_eip: checkpoint.state.eip,
            thunk: thunk::EXCEPTION_DISPATCH,
            trace_args: None,
            interrupted: true,
        };
        let out = handler(
            &mut Env {
                context: self.sys.current,
                cpu: &mut *cpu,
                sys: &mut *self.sys,
                ext: &mut *self.ext,
                thunk: origin.thunk,
            },
            event,
            checkpoint,
        );
        if matches!(&out, Outcome::Fail(_))
            && let Some(snapshot) = trap_snapshot
        {
            self.remember_cpu_fault(snapshot);
        }
        apply(cpu, self.sys, self.ext, out, origin, self.defer_work)
    }

    fn host_call(&mut self, cpu: &mut dyn Guest, index: u32) -> Result<(), Parked> {
        let f = self.sys.thunks.get(index).f;
        let esp = cpu.reg(Reg::Esp);
        self.unwind_escaped(cpu, |frame| esp >= frame.stack, "callback")?;
        let trace_args = (self.sys.trace.enabled() && (f.trace || self.sys.trace.all)).then(|| {
            (0..u32::from(f.conv.argc()))
                .map(|i| cpu.arg(i).unwrap_or(0))
                .collect()
        });
        let origin = Origin {
            esp,
            ret_eip: cpu.reg(Reg::Eip),
            thunk: index,
            trace_args,
            interrupted: false,
        };
        self.sys
            .recent
            .push(self.sys.current, index, cpu.read_u32(esp).unwrap_or(0));
        self.sys.host_calls += 1;
        self.sys.stats.total.host_calls += 1;
        if self
            .sys
            .call_limit
            .is_some_and(|l| self.sys.host_calls >= l)
        {
            let frames = guess_backtrace(cpu, &self.sys.images, esp);
            let frames: Vec<String> = frames.iter().map(|a| format!("{a:#010x}")).collect();
            return Err(Parked::Fail(format!(
                "host call limit reached; likely guest return addresses: {}",
                frames.join(" ")
            )));
        }
        let t0 = self.sys.stats.start();
        let out = (f.handler)(&mut Env {
            context: self.sys.current,
            cpu: &mut *cpu,
            sys: &mut *self.sys,
            ext: &mut *self.ext,
            thunk: index,
        });
        let r = apply(cpu, self.sys, self.ext, out, origin, self.defer_work);
        self.sys.stats.host(t0, index, true, &self.sys.thunks);
        r
    }

    fn guest_return(&mut self, cpu: &mut dyn Guest) -> Result<(), Parked> {
        let eax = cpu.reg(Reg::Eax);
        let returned_sp = cpu.reg(Reg::Esp);
        self.unwind_escaped(cpu, |frame| returned_sp > frame.stack, "return")?;
        if self
            .sys
            .current_context()
            .frames
            .last()
            .is_some_and(|frame| returned_sp < frame.return_floor || returned_sp > frame.stack)
        {
            return Err(Parked::Fail(
                "guest return escaped its continuation stack; nonlocal unwind is unsupported"
                    .into(),
            ));
        }
        let Some(Frame { then, origin, .. }) =
            self.sys.contexts[self.sys.current as usize].frames.pop()
        else {
            // The context's entry function returned.
            return Err(Parked::ExitThread(eax));
        };
        cpu.set_reg(Reg::Esp, origin.esp);
        let thunk = origin.thunk;
        let t0 = self.sys.stats.start();
        let out = then(
            &mut Env {
                context: self.sys.current,
                cpu: &mut *cpu,
                sys: &mut *self.sys,
                ext: &mut *self.ext,
                thunk,
            },
            Resume::Returned(eax),
        );
        let r = apply(cpu, self.sys, self.ext, out, origin, self.defer_work);
        self.sys.stats.host(t0, thunk, true, &self.sys.thunks);
        r
    }
}

impl TrapHandler for Dispatch<'_> {
    fn guest_time_us(&mut self) -> Option<u64> {
        Some(self.sys.clock.guest_now_us())
    }

    fn on_trap(&mut self, cpu: &mut dyn Guest, vector: u8) -> TrapAction {
        let frame_before = self.sys.completed_frames;
        let eip = cpu.reg(Reg::Eip);
        let res = if vector != adv_cpu::backend::asm::TRAP_VECTOR {
            self.sys
                .trace
                .raw_interrupt(self.sys.current, self.sys.frame, vector, cpu, None);
            self.exception(cpu, exception::Exception::Trap { vector, pc: eip })
        } else if eip == self.sys.guest_return_entry + 2 || eip == thunk_addr(thunk::RETURN_PAD) + 2
        {
            self.guest_return(cpu)
        } else {
            // Hooks may reconstruct the opaque token and move the INT away from
            // its PUSH. Only an exact registered transport word authorizes dispatch;
            // ordinary numeric services and coincidental API indices remain raw.
            let esp = cpu.reg(Reg::Esp);
            let word = cpu.read_u32(esp).ok();
            if let Some(index) = word.and_then(|word| self.sys.thunks.transport_index(word)) {
                cpu.set_reg(Reg::Esp, esp.wrapping_add(4));
                self.host_call(cpu, index)
            } else {
                let candidate = word
                    .filter(|i| *i > thunk::EXCEPTION_DISPATCH && *i < self.sys.thunks.len())
                    .map(|i| {
                        let binding = self.sys.thunks.get(i);
                        (binding.module.as_str(), binding.name.as_str())
                    });
                self.sys.trace.raw_interrupt(
                    self.sys.current,
                    self.sys.frame,
                    vector,
                    cpu,
                    candidate,
                );
                if self.sys.exception_handler.is_some() {
                    // A raw guest interrupt never borrows the host thunk token.
                    // Its OS personality owns system-service or exception policy.
                    self.exception(cpu, exception::Exception::Trap { vector, pc: eip })
                } else {
                    let snapshot = self.interrupt_snapshot(cpu, vector, eip);
                    self.remember_cpu_fault(snapshot);
                    Err(Parked::Exception(vector))
                }
            }
        };
        match res {
            Ok(()) if self.yield_on_frame && self.sys.completed_frames != frame_before => {
                // Return/Call has already committed its stack and continuation state.
                // Parking here does not fabricate a Sleep or re-execute the host call.
                self.parked = Some(Parked::HostYield {
                    resume_eip: cpu.reg(Reg::Eip),
                });
                TrapAction::Park
            }
            Ok(()) => TrapAction::Continue,
            Err(p) => {
                self.parked = Some(p);
                TrapAction::Park
            }
        }
    }
}

#[derive(Debug)]
pub enum RunResult {
    /// An API has queued process-supervisor work; no guest wait or exit occurred.
    HostRequest,
    /// Every live context is blocked. `next_deadline` is the earliest timed wake-up, if any.
    Idle {
        next_deadline: Option<u64>,
    },
    /// Explicit process exit, or every guest context has ended.
    Exited(u32),
    Failed(MachineError),
}

/// A resumable host time slice, distinct from a guest wait or process termination.
#[derive(Debug)]
pub enum SliceResult {
    /// Live contexts remain runnable. Call again after servicing the host event loop.
    Yielded,
    Complete(RunResult),
}

#[derive(Debug, Clone)]
pub struct MachineError {
    pub ctx: CtxId,
    pub pc: u32,
    pub message: String,
    /// The last host calls before the failure, oldest first.
    pub recent: Vec<String>,
}

impl std::fmt::Display for MachineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "context {} at {:#010x}: {}",
            self.ctx, self.pc, self.message
        )
    }
}

pub struct Machine<B: Backend> {
    pub cpu: B,
    pub sys: Sys,
    pub ext: Ext,
    regs: Vec<Option<B::Context>>,
    blank: B::Context,
    /// Context whose registers are currently in the CPU.
    loaded: Option<CtxId>,
    /// None inherits the backend default; Some(None) explicitly disables the watchdog.
    /// Configured limits cover one scheduler invocation across context switches.
    execution_budget: Option<Option<Duration>>,
    /// Backend allowance captured before the first soft slice of a contiguous run.
    slice_backend_budget: Option<Option<Duration>>,
    /// Active wall time accumulated across soft slices of one contiguous run.
    slice_elapsed: Duration,
    /// Only used inside real-time run_slice; None restores legacy slice-only scheduling.
    scheduling_quantum: Option<Duration>,
    /// One-shot wake preference, demand-driven quantum and frame-boundary host returns.
    latency_scheduling: bool,
    /// Legacy same-thread consumers require each completed output to return to
    /// the host. An independent mailbox consumer does not need that handoff.
    frame_handoff: bool,
    /// Non-blocking live user intent, owned by the host input path. Never inferred
    /// from context names, frame rates or guest engine behavior.
    time_skip_source: Option<std::sync::Arc<dyn Fn(bool) -> bool + Send + Sync>>,
}

impl<B: Backend> Machine<B> {
    /// Maps the host regions and installs segments. `cpu` must have been created with
    /// [`PARK_ADDR`] as its park address.
    pub fn new(mut cpu: B, resolver: impl Resolver, clock: Clock) -> Result<Self, MemFault> {
        let mut thunks = ThunkTable::new(Box::new(resolver));
        thunks.add_internal(
            INTERNAL_MODULE,
            "process_entry",
            HostFn {
                conv: Conv::Cdecl { argc: 0 },
                handler: process_entry,
                data: 0,
                trace: false,
                guest: None,
            },
        );
        assert_eq!(
            cpu.park_addr(),
            PARK_ADDR,
            "backend park address must be layout::PARK_ADDR"
        );
        cpu.map(THUNK_BASE, THUNK_SIZE, Prot::RX)?;
        cpu.map(GUEST_CODE_BASE, GUEST_CODE_SIZE, Prot::RX)?;
        let mut pad = Asm::new(thunk_addr(thunk::RETURN_PAD));
        pad.trap().hlt();
        cpu.write(thunk_addr(thunk::RETURN_PAD), &pad.bytes)?;
        cpu.map(PARK_ADDR, PAGE, Prot::RX)?;
        cpu.write(PARK_ADDR, &[0xf4])?;
        cpu.map(GDT_ADDR, PAGE, Prot::RW)?;
        cpu.setup_segments(SegmentSetup { gdt_addr: GDT_ADDR })?;
        cpu.map(PEB_ADDR, PAGE, Prot::RW)?;
        // Native Windows user threads start with IF enabled. Capture it in
        // the shared initial template so every backend and new thread agrees.
        cpu.set_reg(Reg::Eflags, cpu.reg(Reg::Eflags) | 0x200);
        let mut blank = cpu.new_context();
        cpu.save(&mut blank);
        let mut vm = Vm::new();
        for &(addr, size) in RESERVED {
            assert!(vm.reserve_at(addr, size));
        }
        let sys = Sys {
            clock,
            invocation_arguments: None,
            trace: Trace::default(),
            thunks,
            objects: Objects::default(),
            vm,
            contexts: Vec::new(),
            current: 0,
            frame: 0,
            completed_frames: 0,
            pid: 0x1000,
            process_identity: process::ProcessIdentity::new(0x1000),
            next_tid: 0x1004,
            images: Images::default(),
            tls_images: Vec::new(),
            process_init: VecDeque::new(),
            process_ready: Vec::new(),
            process_entry: None,
            process_start_entry: None,
            guest_return_entry: thunk_addr(thunk::RETURN_PAD),
            thread_exception_entry: None,
            process_return: None,
            process_prepare: None,
            exception_handler: None,
            first_cpu_fault: None,
            latest_cpu_fault: None,
            guest_failure_code: None,
            checkpoint_identity: std::sync::Arc::new(()),
            next_frame_id: 0,
            recent: RecentCalls::new(32),
            host_calls: 0,
            call_limit: None,
            stats: Stats::new(),
            schedule_trace: schedule_trace::ScheduleTrace::default(),
            scheduling: scheduling::SchedulingStats::default(),
            wakes: sched::ActiveContexts::default(),
            finished_contexts: Vec::new(),
            rr_last: None,
            #[cfg(test)]
            scheduler_work: sched::SchedulerWork::default(),
        };
        Ok(Self {
            cpu,
            sys,
            ext: Ext::default(),
            regs: Vec::new(),
            blank,
            loaded: None,
            execution_budget: None,
            slice_backend_budget: None,
            slice_elapsed: Duration::ZERO,
            scheduling_quantum: Some(scheduling::DEFAULT_QUANTUM),
            latency_scheduling: true,
            frame_handoff: true,
            time_skip_source: None,
        })
    }

    pub fn spawn(
        &mut self,
        name: &str,
        entry: u32,
        args: &[u32],
        stack_size: u32,
    ) -> Result<CtxId, MemFault> {
        self.sys.spawn(&mut self.cpu, name, entry, args, stack_size)
    }

    /// Install a non-blocking, live skip policy (normally atomic key state). It is
    /// told whether the machine is still starting up: no frame completed yet, so
    /// nothing a skipped wait would hurry has been shown.
    pub fn set_time_skip_source(
        &mut self,
        source: Option<std::sync::Arc<dyn Fn(bool) -> bool + Send + Sync>>,
    ) {
        self.time_skip_source = source;
    }

    fn skip_idle_time(&mut self) -> bool {
        let Some(source) = &self.time_skip_source else {
            return false;
        };
        let startup = self.sys.completed_frames == 0;
        if !source(startup) || self.sys.active_contexts().any(|context| context.is_ready()) {
            return false;
        }
        let Some(deadline) = self.sys.next_deadline() else {
            return false;
        };
        let eligible = self.sys.active_contexts().any(|context| {
            let State::Blocked {
                wait,
                allow_time_skip: true,
                ..
            } = &context.state
            else {
                return false;
            };
            match wait {
                Wait::Until(at) => *at == deadline,
                Wait::Object { deadline: at, .. } | Wait::Objects { deadline: at, .. } => {
                    *at == Some(deadline)
                }
                Wait::Yield => false,
            }
        });
        // Re-read live intent at the commit point; an up event never has to drain
        // a guest message queue before stopping time skipping.
        if !eligible || !source(startup) {
            return false;
        }
        let advanced = self.sys.clock.skip_to(deadline);
        if advanced == 0 {
            return false;
        }
        self.sys.scheduling.active_time_skips += 1;
        if let Some(diagnostics) = &mut self.sys.stats.wait_diagnostics {
            diagnostics.skip(advanced);
        }
        self.sys.scheduling.active_time_skip_us = self
            .sys
            .scheduling
            .active_time_skip_us
            .saturating_add(advanced);
        true
    }

    /// Limit one `run` invocation using host wall time, independently of `Clock`.
    /// Expiration returns `RunResult::Failed` with the stopped context, PC and recent calls;
    /// it never forcibly switches contexts. `None` disables the limit. Until configured,
    /// the backend's own default applies. Host calls and all context switches inside
    /// the scheduler invocation count toward this shared limit.
    /// Across `run_slice` calls the allowance persists until Idle/exit/failure. Explicitly
    /// setting it starts a new allowance, useful for an absolute host deadline's remainder.
    pub fn set_execution_budget(&mut self, budget: Option<Duration>) {
        self.execution_budget = Some(budget);
        self.slice_backend_budget = None;
        self.slice_elapsed = Duration::ZERO;
        self.cpu.set_execution_budget(budget);
    }

    /// Bound a guest context within a real-time host slice (default 1 ms), also
    /// stopping at the earliest timed wake. None disables both internal limits for
    /// A/B measurement. Unsliced and virtual-time execution retain their old policy.
    /// This never changes the hard watchdog allowance or any guest Sleep duration.
    pub fn set_scheduling_quantum(
        &mut self,
        quantum: Option<Duration>,
    ) -> Result<(), &'static str> {
        if quantum.is_some_and(|q| q.is_zero()) {
            return Err("scheduling quantum must be nonzero");
        }
        self.scheduling_quantum = quantum;
        Ok(())
    }

    /// Disable the latency policy to compare against fixed-quantum round-robin.
    /// The quantum/deadline bounds, watchdog and polling-round fix remain enabled.
    /// Neither this switch nor the policy changes guest Sleep durations or guest time.
    pub fn set_latency_scheduling(&mut self, enabled: bool) {
        self.latency_scheduling = enabled;
    }

    /// Select whether a completed frame requires the host driver to consume it.
    /// Disable for an independent consumer, retaining wake priority, context
    /// quantum, watchdog, interrupts and real waits. Per-frame Stats collection
    /// still hands off so its rows retain the original measurement boundaries.
    /// Inherit execution policy, never guest state or accumulated measurements.
    pub fn inherit_policy(&mut self, parent: &Self) {
        self.execution_budget = parent.execution_budget;
        self.scheduling_quantum = parent.scheduling_quantum;
        self.latency_scheduling = parent.latency_scheduling;
        self.frame_handoff = parent.frame_handoff;
        self.time_skip_source = parent.time_skip_source.clone();
        self.sys.stats.enabled = parent.sys.stats.enabled;
        self.sys.call_limit = parent.sys.call_limit;
        self.sys.trace = parent.sys.trace.clone();
    }

    pub fn set_frame_handoff(&mut self, required: bool) {
        self.frame_handoff = required;
    }

    /// Create the main context for the exe mapped at `base`: it runs the queued
    /// initializers (TLS callbacks, DllMain of guest DLLs, dependencies first), then the
    /// exe's TLS callbacks and `entry`, and ends the process with the entry point's return
    /// value. The OS layer's `start_process` finds these in its module table.
    pub fn start_main(
        &mut self,
        base: u32,
        entry: u32,
        stack: u32,
        name: &str,
    ) -> Result<CtxId, MemFault> {
        if let Some(prepare) = self.sys.process_prepare {
            prepare(&mut self.cpu, &mut self.sys, &mut self.ext)?;
        }
        self.cpu.write_u32(PEB_ADDR + peb::IMAGE_BASE, base)?;
        let start = self
            .sys
            .thunks
            .resolve(&mut self.cpu, INTERNAL_MODULE, "process_entry")?;
        let main = self.spawn(
            "main",
            self.sys.process_start_entry.unwrap_or(start),
            &[],
            stack.max(DEFAULT_STACK),
        )?;
        self.sys
            .set_initial_registers(main, &[(Reg::Eax, entry), (Reg::Ebx, 0)]);
        self.sys.process_entry = Some(entry);
        self.sys.contexts[main as usize].bootstrap_pending =
            Some((start - THUNK_BASE) / THUNK_STRIDE);
        log::debug!("application entry: {name} at {entry:#010x}");
        Ok(main)
    }

    /// Resolve `module!name` to a thunk address (for the loader).
    pub fn resolve_import(&mut self, module: &str, name: &str) -> Result<u32, MemFault> {
        self.sys.thunks.resolve(&mut self.cpu, module, name)
    }

    /// Supervisor access after all CPU leases have returned. Selecting a context
    /// materializes its own registers; the next scheduler selection saves them.
    pub fn visit_context<R>(
        &mut self,
        context: Option<CtxId>,
        task: impl FnOnce(&mut Env<'_>) -> R,
    ) -> Result<R, String> {
        let prior = self.loaded;
        if let Some(id) = context {
            if self
                .sys
                .contexts
                .get(id as usize)
                .is_none_or(|c| c.is_done())
            {
                return Err("invalid target context".into());
            }
            self.switch_to(id).map_err(|e| e.to_string())?;
        }
        let result = task(&mut Env {
            context: self.sys.current,
            cpu: &mut self.cpu,
            sys: &mut self.sys,
            ext: &mut self.ext,
            thunk: 0,
        });
        if let Some(prior) = prior.filter(|&id| !self.sys.contexts[id as usize].is_done()) {
            self.switch_to(prior).map_err(|e| e.to_string())?;
        }
        Ok(result)
    }

    fn switch_to(&mut self, id: CtxId) -> Result<(), MemFault> {
        self.sys.current = id;
        if self.loaded == Some(id) {
            return Ok(());
        }
        if let Some(prev) = self
            .loaded
            .filter(|&prev| !self.sys.contexts[prev as usize].is_done())
        {
            let mut slot = self.regs[prev as usize]
                .take()
                .unwrap_or_else(|| self.cpu.new_context());
            self.cpu.save(&mut slot);
            self.regs[prev as usize] = Some(slot);
        } else if let Some(prev) = self.loaded
            && let Some(slot) = self.regs.get_mut(prev as usize)
        {
            *slot = None;
        }
        self.regs.resize_with(self.sys.contexts.len(), || None);
        let ctx = &mut self.sys.contexts[id as usize];
        match (ctx.start.take(), &self.regs[id as usize]) {
            (Some((esp, eip)), _) => {
                self.cpu.restore(&self.blank);
                self.cpu.set_fs_base(ctx.teb)?;
                self.cpu.set_reg(Reg::Esp, esp);
                self.cpu.set_reg(Reg::Eip, eip);
                for (register, value) in ctx.initial_registers.drain(..) {
                    self.cpu.set_reg(register, value);
                }
            }
            (None, Some(r)) => self.cpu.restore(r),
            (None, None) => {
                unreachable!("context {id} has neither start state nor saved registers")
            }
        }
        self.loaded = Some(id);
        Ok(())
    }

    fn fail(&self, message: String) -> RunResult {
        RunResult::Failed(MachineError {
            ctx: self.sys.current,
            pc: self.cpu.reg(Reg::Eip),
            message,
            recent: self.sys.recent_calls(),
        })
    }

    fn complete_process_exit(&mut self, code: u32) -> RunResult {
        let code = self
            .sys
            .process_identity
            .completion
            .exit_status()
            .unwrap_or(code);
        // Reports retain guest memory after exit, but physical pins must end
        // with the guest process rather than wait for the executor's Drop.
        match self.cpu.retire_locked_pages() {
            Ok(()) => RunResult::Exited(code),
            Err(error) => {
                let message = format!("process exit could not release locked pages: {error:?}");
                log::warn!("{message}");
                self.fail(message)
            }
        }
    }

    /// Record why the current context parked. `Some` ends the run.
    fn park(&mut self, p: Parked) -> Option<RunResult> {
        let id = self.sys.current;
        let ctx = &mut self.sys.contexts[id as usize];
        match p {
            Parked::HostRequest { then, origin } => {
                ctx.state = State::Ready {
                    resume: Some(ReadyResume::Deferred { then, origin }),
                };
                ctx.yielded = false;
                return Some(RunResult::HostRequest);
            }
            Parked::HostYield { resume_eip } => {
                // Both current backends park by replacing EIP with PARK_ADDR.
                // Other registers already contain the committed trap result.
                self.cpu.set_reg(Reg::Eip, resume_eip);
                ctx.yielded = false;
            }
            Parked::Deferred { then, origin } => {
                ctx.state = State::Ready {
                    resume: Some(ReadyResume::Deferred { then, origin }),
                };
                ctx.yielded = false;
            }
            Parked::Block {
                wait: Wait::Yield,
                then,
                origin,
                ..
            } => {
                if let Some(diagnostics) = &mut self.sys.stats.wait_diagnostics {
                    let now = self.sys.clock.now_us();
                    diagnostics.begin(
                        id,
                        origin.thunk,
                        &self.sys.thunks,
                        &self.sys.objects,
                        &Wait::Yield,
                        now,
                    );
                    diagnostics.wake(id, WakeReason::Timeout, now);
                }
                ctx.state = State::Ready {
                    resume: Some(ReadyResume::Woke {
                        then,
                        origin,
                        reason: WakeReason::Timeout,
                    }),
                };
                ctx.yielded = true;
                self.sys.schedule_trace.event("yield", id, 0, 0, 0);
            }
            Parked::Block {
                wait,
                then,
                origin,
                allow_time_skip,
            } => {
                if let Some(diagnostics) = &mut self.sys.stats.wait_diagnostics {
                    diagnostics.begin(
                        id,
                        origin.thunk,
                        &self.sys.thunks,
                        &self.sys.objects,
                        &wait,
                        self.sys.clock.now_us(),
                    );
                }
                let (kind, resource) = match &wait {
                    Wait::Until(at) => (1, *at),
                    Wait::Object { handle, .. } => (2, u64::from(*handle)),
                    Wait::Objects { .. } => (3, 0),
                    Wait::Yield => unreachable!(),
                };
                self.sys
                    .schedule_trace
                    .event("block", id, kind, resource, u64::from(origin.thunk));
                ctx.state = State::Blocked {
                    wait,
                    then,
                    origin,
                    allow_time_skip,
                };
                ctx.yielded = false;
            }
            Parked::ExitThread(code) => {
                ctx.completion.complete(code);
                ctx.state = State::Done { exit_code: code };
                ctx.frames.clear();
                ctx.bootstrap_pending = None;
                ctx.start = None;
                self.sys.retire_context(id);
                if let Some(diagnostics) = &mut self.sys.stats.wait_diagnostics {
                    diagnostics.retire(id);
                }
                if self.sys.wakes.entries().is_empty() {
                    self.sys.process_identity.completion.complete(code);
                    return Some(self.complete_process_exit(code));
                }
            }
            Parked::ExitProcess(code) => {
                self.sys.process_identity.completion.complete(code);
                ctx.completion.complete(code);
                ctx.state = State::Done { exit_code: code };
                ctx.frames.clear();
                ctx.bootstrap_pending = None;
                ctx.start = None;
                self.sys.retire_context(id);
                if let Some(diagnostics) = &mut self.sys.stats.wait_diagnostics {
                    diagnostics.retire(id);
                }
                self.sys.terminate_other_contexts(code);
                return Some(self.complete_process_exit(code));
            }
            Parked::Fail(msg) => return Some(self.fail(msg)),
            Parked::Exception(v) => {
                return Some(self.fail(format!("unhandled CPU exception/interrupt {v:#x}")));
            }
        }
        None
    }

    /// Run guest contexts until all are blocked, the process exits, or something fails.
    pub fn run(&mut self) -> RunResult {
        if let Some(previous) = self.slice_backend_budget.take() {
            self.cpu
                .set_execution_budget(self.execution_budget.unwrap_or(previous));
        }
        self.slice_elapsed = Duration::ZERO;
        // Without a Machine policy, preserve the original per-backend-run default.
        let hard_budget = self
            .execution_budget
            .unwrap_or_else(|| self.cpu.execution_budget());
        let result = match self.run_scheduler(None, Instant::now(), hard_budget) {
            SliceResult::Complete(result) => result,
            SliceResult::Yielded => unreachable!("an unsliced run cannot soft-yield"),
        };
        // The scheduler leaves the last per-run allowance in the backend.
        self.cpu.set_execution_budget(hard_budget);
        result
    }

    /// Return to the host after approximately `slice` of active wall time, preserving
    /// runnable contexts and their complete CPU state. A soft yield resumes round-robin
    /// on the normal lane; one-shot wake preference never rewinds its cursor.
    /// No guest time advances and no guest wait is invented.
    /// The configured execution budget remains a hard limit across consecutive slices.
    /// Without a Machine policy, inherit and later restore the backend's allowance.
    /// Host handlers and synchronous compilation can exceed the requested slice length.
    pub fn run_slice(&mut self, slice: Duration) -> SliceResult {
        let previous = *self
            .slice_backend_budget
            .get_or_insert_with(|| self.cpu.execution_budget());
        let hard_budget = self.execution_budget.unwrap_or(previous);
        let started = Instant::now();
        let result = self.run_scheduler(Some(slice), started, hard_budget);
        match result {
            SliceResult::Yielded => {
                self.slice_elapsed = self.slice_elapsed.saturating_add(started.elapsed())
            }
            SliceResult::Complete(_) => {
                self.slice_elapsed = Duration::ZERO;
                self.slice_backend_budget = None;
                self.cpu.set_execution_budget(hard_budget);
            }
        }
        result
    }

    fn run_scheduler(
        &mut self,
        slice: Option<Duration>,
        started: Instant,
        hard_budget: Option<Duration>,
    ) -> SliceResult {
        if let Some(budget) = &mut self.sys.stats.frame_budget {
            budget.enter_scheduler();
        }
        let result = self.run_scheduler_inner(slice, started, hard_budget);
        if let Some(budget) = &mut self.sys.stats.frame_budget {
            budget.leave_scheduler();
        }
        result
    }

    fn run_scheduler_inner(
        &mut self,
        slice: Option<Duration>,
        started: Instant,
        hard_budget: Option<Duration>,
    ) -> SliceResult {
        self.sys.clock.begin_work();
        // Only a sliced host driver can service external channels on a soft
        // return. The synchronous run() API retains its run-until-blocked contract.
        self.cpu.set_host_interrupts(slice.is_some());
        self.sys.stats.total.sched += 1;
        let mut last = self.sys.rr_last.take();
        let latency_mode = self.latency_scheduling
            && self.scheduling_quantum.is_some()
            && slice.is_some()
            && !self.sys.clock.is_virtual();
        let frame_handoff = latency_mode && (self.frame_handoff || self.sys.stats.enabled);
        // Do not sleep the host after only one poller ran. Every currently ready
        // context must first get a turn in this scheduler invocation.
        self.sys.wakes.begin_slice();
        loop {
            if let Some(budget) = hard_budget {
                let remaining =
                    budget.saturating_sub(self.slice_elapsed.saturating_add(started.elapsed()));
                if remaining.is_zero() {
                    self.sys.trace.flush();
                    return SliceResult::Complete(self.fail(format!(
                        "guest execution budget exhausted ({budget:?} of host wall time)"
                    )));
                }
            }
            if slice.is_some_and(|limit| started.elapsed() >= limit) {
                self.sys.rr_last = last;
                self.sys.trace.flush();
                return SliceResult::Yielded;
            }
            if slice.is_some() && self.cpu.host_interrupt_pending() {
                self.sys.rr_last = last;
                self.sys.trace.flush();
                self.sys.scheduling.external_interrupts += 1;
                return SliceResult::Yielded;
            }
            self.sys.wake();
            self.sys.advance_if_only_polling();
            // Real time: polling contexts would spin the host. Hand control back so the host
            // can wait (at most 1 ms) like the OS would schedule them.
            if !self.sys.clock.is_virtual()
                && self.sys.only_polling()
                && self
                    .sys
                    .wakes
                    .entries()
                    .iter()
                    .all(|entry| !self.sys.contexts[entry.id as usize].is_ready() || entry.ran)
            {
                self.sys.scheduling.polling_rounds += 1;
                self.sys.rr_last = last;
                let soon = self.sys.clock.now_us() + 1000;
                let t = self.sys.next_deadline().map_or(soon, |d| d.min(soon));
                self.sys.clock.begin_wait(Some(t));
                return SliceResult::Complete(RunResult::Idle {
                    next_deadline: Some(t),
                });
            }
            let contexts = &self.sys.contexts;
            let selection = self.sys.wakes.pick(
                &mut last,
                MAX_CONTEXTS as usize,
                |id| {
                    #[cfg(test)]
                    self.sys
                        .scheduler_work
                        .selections
                        .set(self.sys.scheduler_work.selections.get() + 1);
                    contexts[id as usize].is_ready()
                },
                latency_mode,
            );
            let Some(selection) = selection else {
                if slice.is_some() && self.cpu.host_interrupt_pending() {
                    self.sys.rr_last = last;
                    self.sys.trace.flush();
                    self.sys.scheduling.external_interrupts += 1;
                    return SliceResult::Yielded;
                }
                if self.skip_idle_time() {
                    continue;
                }
                self.sys.trace.flush();
                let next_deadline = self.sys.next_deadline();
                self.sys.clock.begin_wait(next_deadline);
                return SliceResult::Complete(RunResult::Idle { next_deadline });
            };
            let id = selection.id;
            self.sys
                .wakes
                .mark_turn(id, self.sys.contexts[id as usize].yielded);
            if let Err(e) = self.switch_to(id) {
                return SliceResult::Complete(self.fail(format!("context switch: {e}")));
            }

            if let Some(diagnostics) = &mut self.sys.stats.wait_diagnostics {
                diagnostics.dispatch(id);
            }

            self.sys.schedule_trace.event(
                "pick",
                id,
                u64::from(self.sys.contexts[id as usize].yielded),
                0,
                0,
            );

            // Visiting or editing a context only materializes registers. Initialization
            // begins here, after readiness/suspension checks and before guest execution.
            let mut resume = match &mut self.sys.contexts[id as usize].state {
                State::Ready { resume } => resume.take(),
                _ => unreachable!(),
            };
            if let Some(thunk) = self.sys.contexts[id as usize].bootstrap_pending.take() {
                if resume.is_some() {
                    return SliceResult::Complete(
                        self.fail("process bootstrap crossed a pending continuation".into()),
                    );
                }
                let checkpoint = match exception::Checkpoint::capture(&mut self.cpu, &self.sys) {
                    Ok(checkpoint) => checkpoint,
                    Err(error) => {
                        return SliceResult::Complete(
                            self.fail(format!("capture process startup context: {error}")),
                        );
                    }
                };
                let origin = Origin {
                    esp: checkpoint.state.regs[4],
                    ret_eip: checkpoint.state.eip,
                    thunk,
                    trace_args: None,
                    interrupted: true,
                };
                resume = Some(ReadyResume::Deferred {
                    then: Box::new(move |env| process_bootstrap(env, checkpoint)),
                    origin,
                });
            }

            self.sys.scheduling.dispatch(
                selection.wake,
                self.sys.clock.now_us(),
                selection.promoted,
            );
            let frame_before = self.sys.completed_frames;
            // A woken context first finishes the host call it blocked in.
            if let Some(resume) = resume {
                let t0 = self.sys.stats.start();
                let (out, origin) = match resume {
                    ReadyResume::Woke {
                        then,
                        origin,
                        reason,
                    } => {
                        let thunk = origin.thunk;
                        let out = then(
                            &mut Env {
                                context: self.sys.current,
                                cpu: &mut self.cpu,
                                sys: &mut self.sys,
                                ext: &mut self.ext,
                                thunk,
                            },
                            Resume::Woke(reason),
                        );
                        (out, origin)
                    }
                    ReadyResume::Deferred { then, origin } => {
                        let thunk = origin.thunk;
                        let out = then(&mut Env {
                            context: self.sys.current,
                            cpu: &mut self.cpu,
                            sys: &mut self.sys,
                            ext: &mut self.ext,
                            thunk,
                        });
                        (out, origin)
                    }
                };
                let thunk = origin.thunk;
                let applied = apply(
                    &mut self.cpu,
                    &mut self.sys,
                    &mut self.ext,
                    out,
                    origin,
                    latency_mode,
                );
                self.sys.stats.host(t0, thunk, false, &self.sys.thunks);
                self.note_frame();
                if let Err(p) = applied {
                    if let Some(r) = self.park(p) {
                        return SliceResult::Complete(r);
                    }
                    if frame_handoff && self.sys.completed_frames != frame_before {
                        return self.frame_boundary(last, started, hard_budget);
                    }
                    continue;
                }
                if frame_handoff && self.sys.completed_frames != frame_before {
                    return self.frame_boundary(last, started, hard_budget);
                }
            }

            // Continuations may have consumed wall time since the scheduler check.
            let hard_remaining = hard_budget.map(|budget| {
                budget.saturating_sub(self.slice_elapsed.saturating_add(started.elapsed()))
            });
            let soft_remaining = slice.map(|limit| limit.saturating_sub(started.elapsed()));
            if hard_remaining.is_some_and(|remaining| remaining.is_zero()) {
                self.sys.trace.flush();
                return SliceResult::Complete(self.fail(format!(
                    "guest execution budget exhausted ({:?} of host wall time)",
                    hard_budget.unwrap()
                )));
            }
            if soft_remaining.is_some_and(|remaining| remaining.is_zero()) {
                self.sys.contexts[id as usize].yielded = false;
                self.sys.rr_last = last;
                self.sys.trace.flush();
                return SliceResult::Yielded;
            }
            // The frontend's slice is not a guest thread's timeslice. A compute
            // thread must not keep a sleeping renderer unobserved until the entire
            // host slice ends. Virtual time is deliberately not mapped to host time.
            let reschedule = self
                .scheduling_quantum
                .filter(|_| {
                    slice.is_some()
                        && !self.sys.clock.is_virtual()
                        && self.sys.active_contexts().any(|c| c.id != id)
                })
                .and_then(|quantum| {
                    let wake = self.sys.next_deadline().map(|deadline| {
                        Duration::from_micros(deadline.saturating_sub(self.sys.clock.now_us()))
                    });
                    if !latency_mode {
                        Some(wake.map_or(quantum, |wake| quantum.min(wake)))
                    } else {
                        let competitive = self.sys.active_contexts().any(|c| {
                            c.id != id
                                && matches!(
                                    c.state,
                                    State::Ready { .. }
                                        | State::Blocked {
                                            wait: Wait::Object { .. } | Wait::Objects { .. },
                                            ..
                                        }
                                )
                        });
                        // Object signals may be produced inside a non-parking host call.
                        // Keep the quantum guard for those; pure timed sleepers have a
                        // known deadline and need no intervening context polls.
                        scheduling::reschedule_after(quantum, competitive, wake)
                    }
                });
            if reschedule.is_some_and(|remaining| remaining.is_zero()) {
                // A continuation used up the time until a wake. Recheck waits before
                // executing more guest code; never pass a zero timeout to a backend.
                continue;
            }
            let allowance =
                scheduling::Allowance::choose(hard_remaining, soft_remaining, reschedule);
            if let Some(allowance) = allowance {
                self.cpu.set_execution_budget(Some(allowance.duration));
            } else {
                // A former peer may have exited. Do not inherit its expired
                // quantum when the final context has no watchdog or deadline.
                self.cpu.set_execution_budget(hard_budget);
            }

            let run_started = Instant::now();
            let hook_before = self.sys.stats.total.hook_ns;
            if self.sys.schedule_trace.enabled() {
                self.sys.schedule_trace.event(
                    "run_start",
                    id,
                    u64::from(self.cpu.reg(Reg::Eip)),
                    0,
                    0,
                );
            }
            // A polling/yielded peer is not competing for this run.
            let ready_peer = self
                .sys
                .active_contexts()
                .any(|c| c.id != id && c.is_ready() && !c.yielded);
            let calls_before = self.sys.host_calls;
            let t0 = self.sys.stats.start();
            let frame_before = self.sys.completed_frames;
            let mut d = Dispatch {
                sys: &mut self.sys,
                ext: &mut self.ext,
                parked: None,
                yield_on_frame: frame_handoff,
                // Explicit deferred work still returns to this scheduler to
                // recheck deadlines/watchdog, without forcing a host handoff.
                defer_work: latency_mode,
            };
            let exit = self.cpu.run(&mut d);
            let parked = d.parked.take();
            let run_elapsed = run_started.elapsed();
            if self.sys.schedule_trace.enabled() {
                self.sys.schedule_trace.event(
                    "run_end",
                    id,
                    u64::from(self.cpu.reg(Reg::Eip)),
                    run_elapsed.as_nanos() as u64,
                    self.sys.stats.total.hook_ns.saturating_sub(hook_before),
                );
            }
            self.sys.scheduling.context_run(
                id as usize,
                run_elapsed,
                self.sys.host_calls.saturating_sub(calls_before),
                ready_peer,
            );
            if let Some(allowance) = allowance {
                self.sys.scheduling.cpu_run(allowance.duration, run_elapsed);
            }
            if let Some(t0) = t0 {
                self.sys.stats.total.run_ns += t0.elapsed().as_nanos() as u64;
            }
            self.sys.stats.total.cpu_runs += 1;
            self.note_frame();
            match exit {
                Exit::Interrupted { pc } => {
                    self.sys.trace.flush();
                    if let Some(hard) = hard_budget
                        && self.slice_elapsed.saturating_add(started.elapsed()) >= hard
                    {
                        return SliceResult::Complete(RunResult::Failed(MachineError {
                            ctx: id,
                            pc,
                            message: format!(
                                "guest execution budget exhausted ({hard:?} of host wall time)"
                            ),
                            recent: self.sys.recent_calls(),
                        }));
                    }
                    self.sys.contexts[id as usize].yielded = false;
                    self.sys.rr_last = last;
                    self.sys.scheduling.external_interrupts += 1;
                    return SliceResult::Yielded;
                }
                Exit::Parked => {
                    let Some(reason) = parked else {
                        return SliceResult::Complete(self.fail(
                            "guest reached the park address without a host continuation".into(),
                        ));
                    };
                    if let Some(r) = self.park(reason) {
                        self.sys.trace.flush();
                        return SliceResult::Complete(r);
                    }
                    if frame_handoff && self.sys.completed_frames != frame_before {
                        return self.frame_boundary(last, started, hard_budget);
                    }
                }
                Exit::Fault(fault) => {
                    let result = Dispatch {
                        sys: &mut self.sys,
                        ext: &mut self.ext,
                        parked: None,
                        yield_on_frame: frame_handoff,
                        defer_work: latency_mode,
                    }
                    .exception(&mut self.cpu, exception::Exception::Fault(fault));
                    if let Err(parked) = result
                        && let Some(result) = self.park(parked)
                    {
                        self.sys.trace.flush();
                        return SliceResult::Complete(result);
                    }
                }
                Exit::BudgetExceeded { pc, budget } => {
                    self.sys.trace.flush();
                    let hard_expired = hard_budget.is_some_and(|hard| {
                        self.slice_elapsed.saturating_add(started.elapsed()) >= hard
                    });
                    if slice.is_some()
                        && !hard_expired
                        && allowance.is_some_and(|a| a.limit != scheduling::Limit::Hard)
                    {
                        self.sys.contexts[id as usize].yielded = false;
                        if allowance.is_some_and(|a| a.limit == scheduling::Limit::Reschedule)
                            && slice.is_none_or(|limit| started.elapsed() < limit)
                        {
                            self.sys.scheduling.internal_preemptions += 1;
                            // Preserve CPU, continuation and watchdog state. Wake and
                            // select another guest context without sleeping the host.
                            continue;
                        }
                        self.sys.rr_last = last;
                        return SliceResult::Yielded;
                    }
                    return SliceResult::Complete(RunResult::Failed(MachineError {
                        ctx: id,
                        pc,
                        message: format!(
                            "guest execution budget exhausted ({:?} of host wall time)",
                            hard_budget.unwrap_or(budget)
                        ),
                        recent: self.sys.recent_calls(),
                    }));
                }
            }
        }
    }

    /// Finish a frame handoff without masking hard failure or inventing a runnable
    /// context. An object signaled by the completed handler must be observed before
    /// deciding that the machine is idle.
    fn frame_boundary(
        &mut self,
        last: Option<CtxId>,
        started: Instant,
        hard_budget: Option<Duration>,
    ) -> SliceResult {
        self.sys.rr_last = last;
        if let Some(budget) = hard_budget
            && self.slice_elapsed.saturating_add(started.elapsed()) >= budget
        {
            self.sys.trace.flush();
            return SliceResult::Complete(self.fail(format!(
                "guest execution budget exhausted ({budget:?} of host wall time)"
            )));
        }
        self.sys.scheduling.frame_host_yields += 1;
        self.sys.wake();
        if self.sys.active_contexts().any(|c| c.is_ready()) {
            SliceResult::Yielded
        } else {
            let next_deadline = self.sys.next_deadline();
            self.sys.clock.begin_wait(next_deadline);
            SliceResult::Complete(RunResult::Idle { next_deadline })
        }
    }

    /// Close the stats row when the presentation sequence moves on.
    fn note_frame(&mut self) {
        let guest_us = self.sys.clock.now_us();
        if self
            .sys
            .stats
            .frame_budget
            .as_ref()
            .is_some_and(|b| b.pending())
        {
            let cpu = self.cpu.frame_counters();
            // Stats' CSV clock is optional; frame diagnostics need the current
            // clock even when only --frame-budget is collecting timings.
            let total = crate::stats::FrameRow {
                guest_us,
                ..self.sys.stats.total
            };
            self.sys
                .stats
                .frame_budget
                .as_mut()
                .unwrap()
                .finish(total, cpu);
        }
        let blocks = self.cpu.executed_blocks();
        self.sys.stats.note_frame(self.sys.frame, guest_us, blocks);
    }

    /// Count executed translation blocks for [`Stats`] (a measurement mode; it slows guest
    /// execution, so do not combine it with timings you want to trust).
    pub fn set_block_counting(&mut self, on: bool) {
        self.cpu.set_block_counting(on);
    }

    /// Count executions per guest block (implies [`Machine::set_block_counting`]).
    pub fn set_block_profile(&mut self, on: bool) {
        self.cpu.set_block_profile(on);
    }

    /// The `top` guest blocks by executed code bytes, as `module+rva` lines.
    pub fn block_profile_report(&self, top: usize) -> String {
        let mut p = self.cpu.block_profile();
        let total: u64 = p.iter().map(|&(_, n, size)| n * u64::from(size)).sum();
        p.sort_by_key(|&(_, n, size)| std::cmp::Reverse(n * u64::from(size)));
        let mut s = format!(
            "guest blocks: {} distinct, {:.1} MB of code executed\n",
            p.len(),
            total as f64 / 1e6
        );
        for (addr, n, size) in p.into_iter().take(top) {
            let place = self.sys.images.containing(addr).map_or_else(
                || format!("{addr:#010x}"),
                |m| format!("{}+{:#x} ({addr:#010x})", m.name, addr - m.base),
            );
            let bytes = n * u64::from(size);
            s += &format!(
                "  {:5.2}% {:12} runs x {:4} B  {place}\n",
                100.0 * bytes as f64 / total.max(1) as f64,
                n,
                size
            );
        }
        s
    }

    /// Virtual-time driver: run, and whenever everything sleeps, jump the clock to the next
    /// deadline. Stops at exit, failure, deadlock (`Idle { next_deadline: None }`), or when
    /// the next wake-up is more than `budget_us` of guest time from now.
    pub fn run_virtual(&mut self, budget_us: u64) -> RunResult {
        assert!(self.sys.clock.is_virtual());
        let until_us = self.sys.clock.now_us() + budget_us;
        loop {
            match self.run() {
                RunResult::Idle {
                    next_deadline: Some(t),
                } if t <= until_us => self.sys.clock.advance_to(t),
                other => return other,
            }
        }
    }
}
