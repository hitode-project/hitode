//! Guest machine core: address space, guest contexts, host-call dispatch, scheduler.
//!
//! * Host functions are plain `fn(&mut Env) -> Outcome` registered by name
//!   ([`thunk::Registry`]). The loader binds imports to hookable guest thunks.
//! * A thunk trap runs the handler inside the CPU hook. `Return` and `Call` complete in-hook
//!   (no emulation restart), except an optional real-time frame-boundary host yield.
//!   Blocking, exit and failure also park the guest. [`Machine::run`] owns re-entry.
//! * Each guest context (M = main/UI, G = game, W = work, plus any CreateThread) has its own
//!   stack, TEB (FS base), registers, and continuation stack.

pub mod clock;
pub mod ext;
pub mod layout;
pub mod loader;
pub mod module;
pub mod outcome;
pub mod patch;
pub mod sched;
pub mod scheduling;
pub mod stats;
pub mod thunk;
pub mod trace;
pub mod vm;
mod wake_queue;
pub use wake_queue::WakePriority;

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use adv_cpu::backend::asm::Asm;
use adv_cpu::backend::{
    Backend, Exit, Fault, Guest, GuestExt, MemFault, Prot, Reg, SegmentSetup, TrapAction,
    TrapHandler,
};

pub use clock::Clock;
pub use ext::Ext;
use layout::*;
pub use loader::{FileSource, InitCall, LoadError, Loader, TlsImage};
pub use module::{Module, ModuleId, ModuleKind, Modules};
pub use outcome::{Continuation, Outcome, Resume, WakeReason};
pub use patch::Detour;
use sched::{Context, Frame, Origin, ReadyResume, State};
pub use sched::{CtxId, Handle, Object, Objects, Wait};
pub use stats::{FrameRow, Stats};
pub use thunk::{Conv, Handler, Registry, ThunkTable};
pub use trace::{Trace, TraceRecord};
pub use vm::Vm;

/// Host-side machine state shared by all handlers.
pub struct Sys {
    pub clock: Clock,
    /// Real time: where round-robin resumes after a run returned to let the host wait.
    rr_last: Option<CtxId>,
    pub trace: Trace,
    pub thunks: ThunkTable,
    pub objects: Objects,
    pub vm: Vm,
    pub contexts: Vec<Context>,
    /// The context currently running (or last run).
    pub current: CtxId,
    /// Guest frame number for traces; advanced by the PAL layer.
    pub frame: u32,
    /// Complete presentations available to the frontend. Independent of the trace
    /// frame number, which PAL advances before running tasks and draw callbacks.
    pub completed_frames: u32,
    pub pid: u32,
    next_tid: u32,
    pub modules: Modules,
    /// Static TLS templates, by TLS index.
    pub tls_images: Vec<TlsImage>,
    /// Initializers still to run on the main context (see [`Machine::start_process`]).
    pub process_init: VecDeque<InitCall>,
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
    wakes: wake_queue::WakeQueue,
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
    pub cpu: &'a mut dyn Guest,
    pub sys: &'a mut Sys,
    pub ext: &'a mut Ext,
    /// Index of the thunk being handled.
    pub thunk: u32,
}

impl Env<'_> {
    /// `i`th stack argument of the current host call. Unreadable args read as 0 (and log).
    pub fn arg(&self, i: u32) -> u32 {
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
        &self.sys.contexts[self.sys.current as usize]
    }
}

impl Sys {
    /// Create a guest context that will start at `entry(args...)`. Its return ends the thread.
    pub fn spawn(
        &mut self,
        cpu: &mut dyn Guest,
        name: &str,
        entry: u32,
        args: &[u32],
        stack_size: u32,
    ) -> Result<CtxId, MemFault> {
        let id = self.contexts.len() as CtxId;
        assert!(id < MAX_CONTEXTS, "too many guest contexts");
        let stack_size = stack_size.max(0x1_0000);
        let base = self.vm.alloc_with_guard(cpu, stack_size, PAGE, Prot::RW)?;
        let (lo, hi) = (base + PAGE, base + PAGE + stack_size);
        let tid = self.next_tid;
        self.next_tid += 4;

        let teb = teb_addr(id);
        cpu.map(teb, PAGE, Prot::RW)?;
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

        self.setup_static_tls(cpu, teb)?;

        // Initial stack: args, then the return pad as the return address.
        let mut sp = hi - 0x10;
        for &a in args.iter().rev() {
            sp -= 4;
            cpu.write_u32(sp, a)?;
        }
        sp -= 4;
        cpu.write_u32(sp, thunk_addr(thunk::RETURN_PAD))?;

        let thread_handle = self.objects.insert(Object::Thread { ctx: id });
        self.contexts.push(Context {
            id,
            name: name.to_owned(),
            tid,
            teb,
            stack_lo: lo,
            stack_hi: hi,
            start: Some((sp, entry)),
            frames: Vec::new(),
            state: State::Ready { resume: None },
            yielded: false,
            thread_handle,
        });
        Ok(id)
    }

    /// Allocate this thread's static TLS blocks and point TEB.ThreadLocalStoragePointer at
    /// the per-image block array.
    fn setup_static_tls(&mut self, cpu: &mut dyn Guest, teb: u32) -> Result<(), MemFault> {
        if self.tls_images.is_empty() {
            return Ok(());
        }
        let n = self.tls_images.len() as u32;
        let mut offsets = Vec::with_capacity(n as usize);
        let mut size = n * 4;
        for t in &self.tls_images {
            size = vm::align_up(size, 16);
            offsets.push(size);
            size += t.template.len() as u32 + t.zero_fill;
        }
        let block = self.vm.alloc(cpu, size, Prot::RW)?;
        for (t, off) in self.tls_images.iter().zip(&offsets) {
            cpu.write_u32(block + 4 * t.index, block + off)?;
            cpu.write(block + off, &t.template)?;
        }
        cpu.write_u32(teb + teb::TLS_POINTER, block)
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
            .is_some_and(|c| matches!(c.state, State::Ready { .. }))
        {
            self.wakes
                .wake_as(context, self.clock.now_us(), None, priority);
        }
    }
    fn signal_waiters_with_priority(&mut self, h: Handle, priority: WakePriority) {
        let now = self.clock.now_us();
        for i in 0..self.contexts.len() {
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
        let cur = self.current;
        sched::poll_object(&mut self.objects, &self.contexts, h, cur, true)
    }

    /// Acquire a wait set without partially consuming an unsatisfied WaitAll.
    pub fn try_acquire_objects(&mut self, handles: &[Handle], all: bool) -> Option<WakeReason> {
        sched::poll_objects(
            &mut self.objects,
            &self.contexts,
            handles,
            self.current,
            all,
        )
    }

    fn poll_wait(&mut self, i: usize, now: u64) -> Option<WakeReason> {
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
        for i in 0..self.contexts.len() {
            let reason = self.poll_wait(i, now);
            if let Some(reason) = reason {
                self.release_waiter(i, reason, now);
            }
        }
    }

    fn release_waiter(&mut self, i: usize, reason: WakeReason, now: u64) {
        let State::Blocked { wait, then, origin } =
            std::mem::replace(&mut self.contexts[i].state, State::Ready { resume: None })
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
        self.wakes.wake(i as CtxId, now, deadline);
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
    fn advance_if_only_polling(&mut self, polled: &mut [bool]) {
        if !self.clock.is_virtual()
            || !self.only_polling()
            || !self.contexts.iter().enumerate().all(|(i, c)| {
                !matches!(c.state, State::Ready { .. }) || polled.get(i) == Some(&true)
            })
        {
            return;
        }
        let soon = self.clock.now_us() + 1000;
        let t = self
            .next_deadline()
            .map_or(soon, |deadline| deadline.min(soon));
        self.stats.total.polls += 1;
        self.clock.advance_to(t);
        polled.fill(false);
        self.wake();
    }

    /// Some context is ready, and every ready one is only yielding (a polling loop).
    fn only_polling(&self) -> bool {
        let mut ready = self
            .contexts
            .iter()
            .filter(|c| matches!(c.state, State::Ready { .. }))
            .peekable();
        ready.peek().is_some() && ready.all(|c| c.yielded)
    }

    fn next_deadline(&self) -> Option<u64> {
        self.contexts
            .iter()
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
            .min()
    }
}

/// Module name of machine-internal host functions.
pub const INTERNAL_MODULE: &str = "<machine>";

/// Main-context entry: run `process_init` in order, returning the last call's result
/// (the exe's entry point) as the thread's exit code.
fn process_start(env: &mut Env<'_>) -> Outcome {
    fn next(env: &mut Env<'_>, last: u32) -> Outcome {
        match env.sys.process_init.pop_front() {
            None => Outcome::Return(last),
            Some(c) => {
                log::debug!("process init: {} at {:#010x}", c.what, c.target);
                let what = c.what;
                Outcome::call(c.target, c.args, move |env, r| {
                    let v = r.value();
                    if what.starts_with("DllMain") && v == 0 {
                        log::warn!("{what} returned FALSE");
                    }
                    next(env, v)
                })
            }
        }
    }
    next(env, 0)
}

/// Why a context stopped running inside a trap.
enum Parked {
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
    defer_frames: bool,
) -> Result<(), Parked> {
    // A deferred boundary is a host continuation, never a Sleep or an object wake.
    // Keep unsliced and virtual execution inline, with no extra context switch.
    while let Outcome::Defer { then } = out {
        if defer_frames {
            return Err(Parked::Deferred { then, origin });
        }
        let thunk = origin.thunk;
        out = then(&mut Env {
            cpu: &mut *cpu,
            sys: &mut *sys,
            ext: &mut *ext,
            thunk,
        });
    }
    match out {
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
        Outcome::Call { target, args, then } => {
            let push = |cpu: &mut dyn Guest, sp: &mut u32, v: u32| {
                *sp -= 4;
                cpu.write_u32(*sp, v)
            };
            let mut sp = origin.esp;
            let pushed = args
                .iter()
                .rev()
                .try_for_each(|&a| push(cpu, &mut sp, a))
                .and_then(|()| push(cpu, &mut sp, thunk_addr(thunk::RETURN_PAD)));
            if let Err(e) = pushed {
                return Err(Parked::Fail(format!(
                    "guest call to {target:#x}: stack write failed: {e}"
                )));
            }
            cpu.set_reg(Reg::Esp, sp);
            cpu.set_reg(Reg::Eip, target);
            sys.contexts[sys.current as usize]
                .frames
                .push(Frame { then, origin });
            Ok(())
        }
        Outcome::Block { wait, then } => Err(Parked::Block { wait, then, origin }),
        Outcome::Defer { .. } => unreachable!("deferred outcomes were resolved above"),
        Outcome::ExitThread(c) => Err(Parked::ExitThread(c)),
        Outcome::ExitProcess(c) => Err(Parked::ExitProcess(c)),
        Outcome::Fail(msg) => Err(Parked::Fail(msg)),
    }
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
}

/// Return addresses found by scanning the guest stack: values inside an image that follow
/// a call instruction (`call rel32` or `call [mem]`). Heuristic, for diagnostics.
pub fn guess_backtrace(cpu: &dyn Guest, modules: &Modules, esp: u32) -> Vec<u32> {
    let mut out = Vec::new();
    for i in 0..0x800 {
        let Ok(v) = cpu.read_u32(esp + 4 * i) else {
            break;
        };
        if !modules.iter().any(|(_, m)| m.contains(v)) {
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
    fn host_call(&mut self, cpu: &mut dyn Guest, index: u32) -> Result<(), Parked> {
        let f = self.sys.thunks.get(index).f;
        let esp = cpu.reg(Reg::Esp);
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
            let frames = guess_backtrace(cpu, &self.sys.modules, esp);
            let frames: Vec<String> = frames.iter().map(|a| format!("{a:#010x}")).collect();
            return Err(Parked::Fail(format!(
                "host call limit reached; likely guest return addresses: {}",
                frames.join(" ")
            )));
        }
        let t0 = self.sys.stats.start();
        let out = (f.handler)(&mut Env {
            cpu: &mut *cpu,
            sys: &mut *self.sys,
            ext: &mut *self.ext,
            thunk: index,
        });
        let r = apply(cpu, self.sys, self.ext, out, origin, self.yield_on_frame);
        self.sys.stats.host(t0, index, true);
        r
    }

    fn guest_return(&mut self, cpu: &mut dyn Guest) -> Result<(), Parked> {
        let eax = cpu.reg(Reg::Eax);
        let Some(Frame { then, origin }) =
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
                cpu: &mut *cpu,
                sys: &mut *self.sys,
                ext: &mut *self.ext,
                thunk,
            },
            Resume::Returned(eax),
        );
        let r = apply(cpu, self.sys, self.ext, out, origin, self.yield_on_frame);
        self.sys.stats.host(t0, thunk, true);
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
            Err(Parked::Exception(vector))
        } else if eip == thunk_addr(thunk::RETURN_PAD) + 2 {
            self.guest_return(cpu)
        } else {
            // Validate the executed push-token/int instruction pair, including in a
            // relocated trampoline. An API address lookup alone never runs a handler.
            let mut instruction = [0; 7];
            let esp = cpu.reg(Reg::Esp);
            let index = cpu.read_u32(esp).ok();
            let valid = cpu.read(eip.wrapping_sub(7), &mut instruction).is_ok()
                && instruction[0] == 0x68
                && instruction[5..] == [0xcd, adv_cpu::backend::asm::TRAP_VECTOR]
                && index.is_some_and(|i| {
                    i != thunk::RETURN_PAD
                        && i < self.sys.thunks.len()
                        && instruction[1..5] == i.to_le_bytes()
                });
            if valid {
                cpu.set_reg(Reg::Esp, esp.wrapping_add(4));
                self.host_call(cpu, index.unwrap())
            } else {
                Err(Parked::Exception(vector))
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
    /// Every live context is blocked. `next_deadline` is the earliest timed wake-up, if any.
    Idle {
        next_deadline: Option<u64>,
    },
    /// ExitProcess, or the main context returned.
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

#[derive(Debug)]
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
}

impl<B: Backend> Machine<B> {
    /// Maps the host regions and installs segments. `cpu` must have been created with
    /// [`PARK_ADDR`] as its park address.
    pub fn new(mut cpu: B, mut registry: Registry, clock: Clock) -> Result<Self, MemFault> {
        registry.add(
            INTERNAL_MODULE,
            "process_start",
            Conv::Cdecl { argc: 0 },
            process_start,
        );
        assert_eq!(
            cpu.park_addr(),
            PARK_ADDR,
            "backend park address must be layout::PARK_ADDR"
        );
        cpu.map(THUNK_BASE, THUNK_SIZE, Prot::RX)?;
        let mut pad = Asm::new(thunk_addr(thunk::RETURN_PAD));
        pad.trap().hlt();
        cpu.write(thunk_addr(thunk::RETURN_PAD), &pad.bytes)?;
        cpu.map(PARK_ADDR, PAGE, Prot::RX)?;
        cpu.write(PARK_ADDR, &[0xf4])?;
        cpu.map(GDT_ADDR, PAGE, Prot::RW)?;
        cpu.setup_segments(SegmentSetup { gdt_addr: GDT_ADDR })?;
        cpu.map(PEB_ADDR, PAGE, Prot::RW)?;
        let mut blank = cpu.new_context();
        cpu.save(&mut blank);
        let mut vm = Vm::new();
        for &(addr, size) in RESERVED {
            assert!(vm.reserve_at(addr, size));
        }
        let sys = Sys {
            clock,
            trace: Trace::default(),
            thunks: ThunkTable::new(registry),
            objects: Objects::default(),
            vm,
            contexts: Vec::new(),
            current: 0,
            frame: 0,
            completed_frames: 0,
            pid: 0x1000,
            next_tid: 0x1004,
            modules: Modules::default(),
            tls_images: Vec::new(),
            process_init: VecDeque::new(),
            recent: RecentCalls::new(32),
            host_calls: 0,
            call_limit: None,
            stats: Stats::new(),
            scheduling: scheduling::SchedulingStats::default(),
            wakes: wake_queue::WakeQueue::default(),
            rr_last: None,
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

    /// Load an image and its dependencies.
    /// [`Machine::start_process`].
    pub fn load(&mut self, files: &dyn FileSource, path: &str) -> Result<ModuleId, LoadError> {
        let mut l = Loader::new(&mut self.cpu, &mut self.sys, files);
        let id = l.load(path)?;
        let inits = std::mem::take(&mut l.inits);
        self.sys.process_init.extend(inits);
        Ok(id)
    }

    /// Create the main context for `exe`: it runs the queued initializers (TLS callbacks,
    /// DllMain of guest DLLs, dependencies first), then the exe's TLS callbacks and entry
    /// point, and ends the process with the entry point's return value.
    pub fn start_process(&mut self, exe: ModuleId) -> Result<CtxId, MemFault> {
        let m = self.sys.modules.get(exe);
        let (base, entry, stack, name) = (m.base, m.entry, m.stack_reserve, m.name.clone());
        self.cpu.write_u32(PEB_ADDR + peb::IMAGE_BASE, base)?;
        self.sys.process_init.push_back(InitCall {
            target: entry,
            args: vec![],
            what: format!("entry of {name}"),
        });
        let start = self
            .sys
            .thunks
            .resolve(&mut self.cpu, INTERNAL_MODULE, "process_start")?;
        self.spawn("M", start, &[], stack.max(DEFAULT_STACK))
    }

    /// Resolve `module!name` to a thunk address (for the loader).
    pub fn resolve_import(&mut self, module: &str, name: &str) -> Result<u32, MemFault> {
        self.sys.thunks.resolve(&mut self.cpu, module, name)
    }

    fn switch_to(&mut self, id: CtxId) -> Result<(), MemFault> {
        self.sys.current = id;
        if self.loaded == Some(id) {
            return Ok(());
        }
        if let Some(prev) = self.loaded {
            let mut slot = self.regs[prev as usize]
                .take()
                .unwrap_or_else(|| self.cpu.new_context());
            self.cpu.save(&mut slot);
            self.regs[prev as usize] = Some(slot);
        }
        self.regs.resize_with(self.sys.contexts.len(), || None);
        let ctx = &mut self.sys.contexts[id as usize];
        match (ctx.start.take(), &self.regs[id as usize]) {
            (Some((esp, eip)), _) => {
                self.cpu.restore(&self.blank);
                self.cpu.set_fs_base(ctx.teb)?;
                self.cpu.set_reg(Reg::Esp, esp);
                self.cpu.set_reg(Reg::Eip, eip);
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

    /// Record why the current context parked. `Some` ends the run.
    fn park(&mut self, p: Parked) -> Option<RunResult> {
        let id = self.sys.current;
        let ctx = &mut self.sys.contexts[id as usize];
        match p {
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
            } => {
                ctx.state = State::Ready {
                    resume: Some(ReadyResume::Woke {
                        then,
                        origin,
                        reason: WakeReason::Timeout,
                    }),
                };
                ctx.yielded = true;
            }
            Parked::Block { wait, then, origin } => {
                ctx.state = State::Blocked { wait, then, origin };
                ctx.yielded = false;
            }
            Parked::ExitThread(code) => {
                ctx.state = State::Done { exit_code: code };
                ctx.frames.clear();
                if id == 0 {
                    return Some(RunResult::Exited(code));
                }
            }
            Parked::ExitProcess(code) => return Some(RunResult::Exited(code)),
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
        match self.run_scheduler(None, Instant::now(), self.execution_budget.flatten()) {
            SliceResult::Complete(result) => result,
            SliceResult::Yielded => unreachable!("an unsliced run cannot soft-yield"),
        }
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
        // Only a sliced host driver can service external channels on a soft
        // return. The synchronous run() API retains its run-until-blocked contract.
        self.cpu.set_host_interrupts(slice.is_some());
        self.sys.stats.total.sched += 1;
        let mut last = self.sys.rr_last.take();
        let latency_mode = self.latency_scheduling
            && self.scheduling_quantum.is_some()
            && slice.is_some()
            && !self.sys.clock.is_virtual();
        // Do not sleep the host after only one poller ran. Every currently ready
        // context must first get a turn in this scheduler invocation.
        let mut ran = vec![false; self.sys.contexts.len()];
        let mut polled = vec![false; self.sys.contexts.len()];
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
            self.sys.wake();
            self.sys.advance_if_only_polling(&mut polled);
            // Real time: polling contexts would spin the host. Hand control back so the host
            // can wait (at most 1 ms) like the OS would schedule them.
            if !self.sys.clock.is_virtual()
                && self.sys.only_polling()
                && self.sys.contexts.iter().enumerate().all(|(i, c)| {
                    !matches!(c.state, State::Ready { .. }) || ran.get(i) == Some(&true)
                })
            {
                self.sys.scheduling.polling_rounds += 1;
                self.sys.rr_last = last;
                let soon = self.sys.clock.now_us() + 1000;
                let t = self.sys.next_deadline().map_or(soon, |d| d.min(soon));
                return SliceResult::Complete(RunResult::Idle {
                    next_deadline: Some(t),
                });
            }
            let contexts = &self.sys.contexts;
            let selection = self.sys.wakes.pick(
                &mut last,
                contexts.len(),
                |id| matches!(contexts[id as usize].state, State::Ready { .. }),
                latency_mode,
            );
            let Some(selection) = selection else {
                self.sys.trace.flush();
                return SliceResult::Complete(RunResult::Idle {
                    next_deadline: self.sys.next_deadline(),
                });
            };
            let id = selection.id;
            ran.resize(self.sys.contexts.len(), false);
            ran[id as usize] = true;
            polled.resize(self.sys.contexts.len(), false);
            polled[id as usize] = self.sys.contexts[id as usize].yielded;
            if let Err(e) = self.switch_to(id) {
                return SliceResult::Complete(self.fail(format!("context switch: {e}")));
            }

            self.sys.scheduling.dispatch(
                selection.wake,
                self.sys.clock.now_us(),
                selection.promoted,
            );
            let frame_before = self.sys.completed_frames;
            // A woken context first finishes the host call it blocked in.
            let resume = match &mut self.sys.contexts[id as usize].state {
                State::Ready { resume } => resume.take(),
                _ => unreachable!(),
            };
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
                self.sys.stats.host(t0, thunk, false);
                self.note_frame();
                if let Err(p) = applied {
                    if let Some(r) = self.park(p) {
                        return SliceResult::Complete(r);
                    }
                    if latency_mode && self.sys.completed_frames != frame_before {
                        return self.frame_boundary(last, started, hard_budget);
                    }
                    continue;
                }
                if latency_mode && self.sys.completed_frames != frame_before {
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
                        && self.sys.contexts.iter().any(|c| c.id != id && !c.is_done())
                })
                .and_then(|quantum| {
                    let wake = self.sys.next_deadline().map(|deadline| {
                        Duration::from_micros(deadline.saturating_sub(self.sys.clock.now_us()))
                    });
                    if !latency_mode {
                        Some(wake.map_or(quantum, |wake| quantum.min(wake)))
                    } else {
                        let competitive = self.sys.contexts.iter().any(|c| {
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
            }

            let run_started = Instant::now();
            let t0 = self.sys.stats.start();
            let frame_before = self.sys.completed_frames;
            let mut d = Dispatch {
                sys: &mut self.sys,
                ext: &mut self.ext,
                parked: None,
                yield_on_frame: latency_mode,
            };
            let exit = self.cpu.run(&mut d);
            let parked = d.parked.take();
            if let Some(allowance) = allowance {
                self.sys
                    .scheduling
                    .cpu_run(allowance.duration, run_started.elapsed());
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
                    if let Some(r) = self.park(parked.expect("parked without a reason")) {
                        self.sys.trace.flush();
                        return SliceResult::Complete(r);
                    }
                    if latency_mode && self.sys.completed_frames != frame_before {
                        return self.frame_boundary(last, started, hard_budget);
                    }
                }
                Exit::Fault(Fault { kind, pc }) => {
                    self.sys.trace.flush();
                    return SliceResult::Complete(RunResult::Failed(MachineError {
                        ctx: id,
                        pc,
                        message: format!("guest fault: {kind:?}"),
                        recent: self.sys.recent_calls(),
                    }));
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
                            && slice.is_some_and(|limit| started.elapsed() < limit)
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
        if self
            .sys
            .contexts
            .iter()
            .any(|c| matches!(c.state, State::Ready { .. }))
        {
            SliceResult::Yielded
        } else {
            SliceResult::Complete(RunResult::Idle {
                next_deadline: self.sys.next_deadline(),
            })
        }
    }

    /// Close the stats row when the PAL frame moved on.
    fn note_frame(&mut self) {
        let blocks = self.cpu.executed_blocks();
        self.sys
            .stats
            .note_frame(self.sys.frame, self.sys.clock.now_us(), blocks);
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
            let place = self
                .sys
                .modules
                .iter()
                .find(|(_, m)| m.contains(addr))
                .map_or_else(
                    || format!("{addr:#010x}"),
                    |(_, m)| format!("{}+{:#x} ({addr:#010x})", m.name, addr - m.base),
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
