//! What a host handler asks the machine to do next.
//!
//! Handlers never call guest code and never block the host thread. They return an
//! [`Outcome`]; calls into the guest and waits are expressed as continuations stored per
//! guest context. So a guest context that sleeps inside a callback leaves nothing on the host
//! stack, and any other context can run meanwhile.

use crate::Env;
use crate::sched::Wait;

/// Continuation run when a guest call returns or a wait ends.
pub type Continuation = Box<dyn FnOnce(&mut Env<'_>, Resume) -> Outcome>;

/// Host work to resume after a completed presentation is handed to the frontend.
/// This continuation does not represent a guest wait or advance guest time.
pub type DeferredContinuation = Box<dyn FnOnce(&mut Env<'_>) -> Outcome>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resume {
    /// A guest call made with [`Outcome::Call`] returned this EAX.
    Returned(u32),
    /// A wait ended.
    Woke(WakeReason),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WakeReason {
    /// The deadline passed (or the wait was a plain sleep/yield).
    Timeout,
    /// The awaited object was signaled.
    Signaled,
    /// A multiple-object wait acquired its selected object(s).
    Objects { index: u32, abandoned: bool },
    /// Wait criteria changed; the continuation must recompute them.
    Notified,
}

impl Resume {
    /// EAX of a returned guest call.
    pub fn value(self) -> u32 {
        match self {
            Resume::Returned(v) => v,
            Resume::Woke(_) => panic!("expected a guest return, got {self:?}"),
        }
    }
}

pub enum Outcome {
    /// Finish the host call: EAX = value; the thunk's `ret[ n]` pops the caller's frame.
    Return(u32),
    /// Finish the host call with EDX:EAX.
    Return64(u64),
    /// Finish a cdecl floating return in x87 ST(0); trace stores the f32 bit pattern.
    ReturnF32(f32),
    /// Finish a cdecl double return in x87 ST(0), retaining the full f64 precision.
    ReturnF64(f64),
    /// Call guest `target(args...)` on the current context. `then` receives its EAX.
    /// The stack is reset afterwards, so the guest callee's convention doesn't matter.
    Call {
        target: u32,
        args: Vec<u32>,
        then: Continuation,
        /// Optional lower stack boundary, leaving caller-owned guest scratch above it.
        stack: Option<u32>,
        /// The callee may leave by nonlocal unwind instead of returning, as a
        /// Windows exception handler does when it resumes in an `__except`
        /// block or catch continuation. Its continuation is then dropped.
        unwindable: bool,
    },
    /// Resume a checked architectural checkpoint without an API return epilogue.
    ResumeExecution(crate::exception::Checkpoint),
    /// Raise the guest exception `fault` would, as if the function's first
    /// instruction at `fault.pc` faulted.
    Raise(adv_cpu::backend::Fault),
    /// End the innermost guest call on this context as if its callee had
    /// returned this EAX, abandoning the current host call and the guest
    /// frames between them (NtCallbackReturn). The current host call must be
    /// nested inside that guest call.
    ReturnFromCall(u32),
    /// Suspend the current context until `wait` is satisfied, then run `then`.
    Block {
        wait: Wait,
        then: Continuation,
        allow_time_skip: bool,
    },
    /// Continue host work after handing a completed frame to the frontend. Real-time
    /// sliced execution may defer `then`; other modes run it inline.
    Defer { then: DeferredContinuation },
    /// Leave the CPU and return to the process supervisor before resuming this call.
    HostRequest { then: DeferredContinuation },
    /// The current context's thread ends (ExitThread).
    ExitThread(u32),
    /// The process ends (ExitProcess).
    ExitProcess(u32),
    /// Stop the machine with an error (unimplemented API, bad arguments, ...).
    Fail(String),
}

impl Outcome {
    pub fn host_request(then: impl FnOnce(&mut Env<'_>) -> Outcome + 'static) -> Self {
        Self::HostRequest {
            then: Box::new(then),
        }
    }

    pub fn call(
        target: u32,
        args: impl Into<Vec<u32>>,
        then: impl FnOnce(&mut Env<'_>, Resume) -> Outcome + 'static,
    ) -> Self {
        Outcome::Call {
            target,
            args: args.into(),
            then: Box::new(then),
            stack: None,
            unwindable: false,
        }
    }

    pub fn call_on_stack(
        target: u32,
        args: impl Into<Vec<u32>>,
        stack: u32,
        then: impl FnOnce(&mut Env<'_>, Resume) -> Outcome + 'static,
    ) -> Self {
        Self::Call {
            target,
            args: args.into(),
            then: Box::new(then),
            stack: Some(stack),
            unwindable: false,
        }
    }

    /// [`Self::call`] for a callee built for the i386 System V ABI, whose
    /// arguments start on a 16-byte boundary; Windows code needs only 4.
    pub fn call_aligned(
        env: &Env<'_>,
        target: u32,
        args: impl Into<Vec<u32>>,
        then: impl FnOnce(&mut Env<'_>, Resume) -> Outcome + 'static,
    ) -> Self {
        let args = args.into();
        let esp = env.cpu.reg(adv_cpu::backend::Reg::Esp);
        let bytes = 4 * args.len() as u32;
        let stack = (esp.wrapping_sub(bytes) & !15).wrapping_add(bytes);
        Self::call_on_stack(target, args, stack, then)
    }

    /// Mark a guest call as one native code may abandon by nonlocal unwind;
    /// other outcomes are unchanged.
    pub fn unwindable(mut self) -> Self {
        if let Self::Call { unwindable, .. } = &mut self {
            *unwindable = true;
        }
        self
    }

    pub fn block(wait: Wait, then: impl FnOnce(&mut Env<'_>, Resume) -> Outcome + 'static) -> Self {
        Outcome::Block {
            allow_time_skip: matches!(wait, Wait::Until(_)),
            wait,
            then: Box::new(then),
        }
    }

    /// A time-based wait which can also be interrupted by input/objects. Ordinary
    /// synchronization waits must use block: an external producer may need real time.
    pub fn block_timed(
        wait: Wait,
        then: impl FnOnce(&mut Env<'_>, Resume) -> Outcome + 'static,
    ) -> Self {
        Outcome::Block {
            wait,
            then: Box::new(then),
            allow_time_skip: true,
        }
    }

    /// Block, then return `value` from the host call.
    pub fn block_then_return(wait: Wait, value: u32) -> Self {
        Self::block(wait, move |_, _| Outcome::Return(value))
    }

    /// Use after `Sys::complete_frame` to defer the following host work, preserving
    /// the current guest call without fabricating a guest wait.
    pub fn defer(then: impl FnOnce(&mut Env<'_>) -> Outcome + 'static) -> Self {
        Self::Defer {
            then: Box::new(then),
        }
    }
}
