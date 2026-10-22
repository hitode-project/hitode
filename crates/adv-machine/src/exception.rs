//! Neutral delivery of precise CPU stops through the existing continuation stack.
//! Personalities own exception records, handler search and context layouts.

use crate::{Env, Outcome, Sys, sched::CtxId};
use adv_cpu::backend::{
    ArchitecturalState, Fault, FaultKind, Guest, GuestExt, Prot, Reg, StateError,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Exception {
    Trap { vector: u8, pc: u32 },
    Fault(Fault),
}

pub type ExceptionHandler = fn(&mut Env<'_>, Exception, Checkpoint) -> Outcome;

/// Original backend fault evidence, retained before any guest exception handler
/// can replace registers, stack contents or the last indirect transfer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CpuFaultSnapshot {
    pub context: CtxId,
    pub fault: Fault,
    pub last_indirect_branch: Option<adv_cpu::IndirectBranch>,
    pub registers: [(Reg, u32); 9],
    pub stack_address: u32,
    /// At most 16 consecutive readable DWORDs; an unreadable word ends capture.
    pub stack: Vec<u32>,
}

impl CpuFaultSnapshot {
    pub fn kind_name(&self) -> &'static str {
        match self.fault.kind {
            FaultKind::Read(_) => "read",
            FaultKind::Write(_) => "write",
            FaultKind::Fetch(_) => "fetch",
            FaultKind::InvalidInstruction => "invalid_instruction",
            FaultKind::Other(_) => "other",
        }
    }

    pub fn access(&self) -> Option<(&'static str, u32)> {
        match self.fault.kind {
            FaultKind::Read(address) => Some(("read", address)),
            FaultKind::Write(address) => Some(("write", address)),
            FaultKind::Fetch(address) => Some(("execute", address)),
            _ => None,
        }
    }

    pub fn reason(&self) -> Option<&str> {
        match &self.fault.kind {
            FaultKind::Other(reason) => Some(reason),
            _ => None,
        }
    }

    pub(crate) fn capture(cpu: &dyn Guest, context: CtxId, fault: &Fault) -> Self {
        let stack_address = cpu.reg(Reg::Esp);
        let mut stack = Vec::with_capacity(16);
        for index in 0..16 {
            let Some(address) = stack_address.checked_add(index * 4) else {
                break;
            };
            if !cpu.accessible(address, 4, Prot::READ) {
                break;
            }
            let Ok(value) = cpu.read_u32(address) else {
                break;
            };
            stack.push(value);
        }
        Self {
            context,
            fault: fault.clone(),
            last_indirect_branch: cpu.last_indirect_branch(),
            registers: [
                Reg::Eip,
                Reg::Esp,
                Reg::Ebp,
                Reg::Eax,
                Reg::Ebx,
                Reg::Ecx,
                Reg::Edx,
                Reg::Esi,
                Reg::Edi,
            ]
            .map(|register| (register, cpu.reg(register))),
            stack_address,
            stack,
        }
    }
}

/// One-use architectural image tied to a context and its live host-call ancestry.
/// No scheduler, clock, fuel, cache or interrupt state belongs in this image.
pub struct Checkpoint {
    pub state: Box<ArchitecturalState>,
    machine: std::sync::Arc<()>,
    context: CtxId,
    depth: usize,
    frame: Option<u64>,
    unwind_descendants: bool,
}

impl Env<'_> {
    pub fn checkpoint(&mut self) -> Result<Checkpoint, StateError> {
        Checkpoint::capture(self.cpu, self.sys)
    }

    /// Capture a nonlocal resume after personality-owned cleanup has completed.
    /// Only callbacks explicitly declared unwindable may be retired. Retained
    /// ancestry remains pinned and is checked again when the checkpoint resumes.
    pub fn checkpoint_for_stack_unwind(&mut self, stack: u32) -> Result<Checkpoint, String> {
        let context = self.sys.current_context();
        if stack < context.stack_lo || stack >= context.stack_hi || stack < self.cpu.reg(Reg::Esp) {
            return Err("stack unwind target is outside the active upward stack range".into());
        }
        let mut depth = context.frames.len();
        while let Some(frame) = depth.checked_sub(1).and_then(|i| context.frames.get(i)) {
            if stack < frame.stack {
                break;
            }
            if !frame.unwindable {
                return Err("stack unwind crosses a non-unwindable continuation".into());
            }
            depth -= 1;
        }
        let ancestor = depth.checked_sub(1).map(|i| context.frames[i].id);
        let mut checkpoint = Checkpoint::capture(self.cpu, self.sys).map_err(|e| e.to_string())?;
        checkpoint.depth = depth;
        checkpoint.frame = ancestor;
        checkpoint.unwind_descendants = true;
        checkpoint.state.regs[4] = stack;
        Ok(checkpoint)
    }
}

impl Checkpoint {
    /// Permit retirement of explicitly unwindable callbacks created after capture.
    /// The captured ancestor and owning machine/context must still be live.
    pub fn unwind_callbacks(mut self) -> Self {
        self.unwind_descendants = true;
        self
    }

    pub(crate) fn capture(cpu: &mut dyn Guest, sys: &Sys) -> Result<Self, StateError> {
        let context = sys.current_context();
        Ok(Self {
            state: Box::new(cpu.capture_architecture()?),
            machine: sys.checkpoint_identity.clone(),
            context: context.id,
            depth: context.frames.len(),
            frame: context.frames.last().map(|frame| frame.id),
            unwind_descendants: false,
        })
    }

    pub(crate) fn restore(self, cpu: &mut dyn Guest, sys: &mut Sys) -> Result<(), String> {
        let context = sys.current_context();
        if !std::sync::Arc::ptr_eq(&self.machine, &sys.checkpoint_identity)
            || self.context != context.id
            || self.depth > context.frames.len()
            || self.frame
                != self
                    .depth
                    .checked_sub(1)
                    .and_then(|i| context.frames.get(i))
                    .map(|frame| frame.id)
            || (!self.unwind_descendants && self.depth != context.frames.len())
            || context.frames[self.depth..]
                .iter()
                .any(|frame| !frame.unwindable)
        {
            return Err("architectural resume crossed its live continuation boundary".into());
        }
        // Crossing a host callback requires an explicit unwind/cancellation protocol.
        if context
            .frames
            .get(self.depth.wrapping_sub(1))
            .is_some_and(|frame| self.state.regs[4] >= frame.stack)
        {
            return Err("architectural resume requires unsupported continuation unwind".into());
        }
        cpu.restore_architecture(&self.state)
            .map_err(|error| error.to_string())?;
        sys.contexts[sys.current as usize]
            .frames
            .truncate(self.depth);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Clock, Machine,
        layout::PARK_ADDR,
        sched::{Frame, Origin},
    };
    use adv_cpu::{JitCpu, backend::Reg};
    struct Empty;
    impl crate::Resolver for Empty {
        fn host_fn(&self, _: &str, _: &str) -> Option<crate::HostFn> {
            None
        }
        fn provides(&self, _: &str) -> bool {
            false
        }
    }
    fn machine() -> Machine<JitCpu> {
        let mut machine = Machine::new(
            JitCpu::new(PARK_ADDR, false).unwrap(),
            Empty,
            Clock::virtual_time(),
        )
        .unwrap();
        machine.spawn("first", 0x400000, &[], 0x10000).unwrap();
        machine.spawn("second", 0x400000, &[], 0x10000).unwrap();
        machine
    }
    fn frame(id: u64) -> Frame {
        Frame {
            id,
            stack: 0x1000,
            return_floor: 0xffc,
            unwindable: false,
            then: Box::new(|_, _| Outcome::Return(0)),
            origin: Origin {
                esp: 0x1000,
                ret_eip: 0,
                thunk: 0,
                trace_args: None,
                interrupted: false,
            },
        }
    }
    #[test]
    fn explicit_unwind_retires_only_descendants_of_the_captured_live_ancestor() {
        let mut machine = machine();
        machine.sys.contexts[0].frames.push(frame(1));
        machine.cpu.set_reg(Reg::Esp, 0x800);
        let mut checkpoint = Checkpoint::capture(&mut machine.cpu, &machine.sys).unwrap();
        checkpoint.state.regs[0] = 0x1234;
        let mut child = frame(2);
        child.unwindable = true;
        machine.sys.contexts[0].frames.push(child);
        checkpoint
            .unwind_callbacks()
            .restore(&mut machine.cpu, &mut machine.sys)
            .unwrap();
        assert_eq!(machine.cpu.reg(Reg::Eax), 0x1234);
        assert_eq!(machine.sys.contexts[0].frames.len(), 1);
        assert_eq!(machine.sys.contexts[0].frames[0].id, 1);

        let checkpoint = Checkpoint::capture(&mut machine.cpu, &machine.sys).unwrap();
        machine.sys.contexts[0].frames.push(frame(3));
        assert!(
            checkpoint
                .unwind_callbacks()
                .restore(&mut machine.cpu, &mut machine.sys)
                .is_err()
        );
        assert_eq!(machine.sys.contexts[0].frames.len(), 2);
        machine.sys.contexts[0].frames.pop();

        let checkpoint = Checkpoint::capture(&mut machine.cpu, &machine.sys).unwrap();
        machine.sys.contexts[0].frames[0] = frame(4);
        assert!(
            checkpoint
                .unwind_callbacks()
                .restore(&mut machine.cpu, &mut machine.sys)
                .is_err()
        );
        assert_eq!(machine.sys.contexts[0].frames[0].id, 4);
    }

    #[test]
    fn different_context_or_replaced_ancestor_cannot_reuse_a_checkpoint() {
        let mut machine = machine();
        machine.cpu.set_reg(Reg::Eax, 0x1234);
        let mut checkpoint = Checkpoint::capture(&mut machine.cpu, &machine.sys).unwrap();
        checkpoint.state.regs[0] = 0xdead;
        machine.sys.current = 1;
        assert!(
            checkpoint
                .restore(&mut machine.cpu, &mut machine.sys)
                .unwrap_err()
                .contains("boundary")
        );
        assert_eq!(machine.cpu.reg(Reg::Eax), 0x1234);
        machine.sys.current = 0;
        machine.sys.contexts[0].frames.push(frame(1));
        let checkpoint = Checkpoint::capture(&mut machine.cpu, &machine.sys).unwrap();
        machine.sys.contexts[0].frames[0] = frame(2);
        assert!(
            checkpoint
                .restore(&mut machine.cpu, &mut machine.sys)
                .unwrap_err()
                .contains("boundary")
        );
        assert_eq!(machine.cpu.reg(Reg::Eax), 0x1234);
    }
    // spec: PROC-MACH-3
    #[test]
    fn nonlocal_stack_escape_requires_explicit_continuation_unwind() {
        let mut machine = machine();
        machine.sys.contexts[0].frames.push(frame(1));
        let mut checkpoint = Checkpoint::capture(&mut machine.cpu, &machine.sys).unwrap();
        checkpoint.state.regs[4] = 0x1000;
        let before = machine.cpu.capture_architecture().unwrap();
        assert!(
            checkpoint
                .restore(&mut machine.cpu, &mut machine.sys)
                .unwrap_err()
                .contains("unwind")
        );
        assert_eq!(machine.cpu.capture_architecture().unwrap(), before);
    }
    // spec: PROC-MACH-3
    #[test]
    fn equal_context_numbers_in_another_machine_do_not_authorize_resume() {
        let mut source = machine();
        let mut other = machine();
        let checkpoint = Checkpoint::capture(&mut source.cpu, &source.sys).unwrap();
        let before = other.cpu.capture_architecture().unwrap();
        assert!(
            checkpoint
                .restore(&mut other.cpu, &mut other.sys)
                .unwrap_err()
                .contains("boundary")
        );
        assert_eq!(other.cpu.capture_architecture().unwrap(), before);
    }

    // spec: PROC-MACH-11
    #[test]
    fn stack_unwind_retires_only_explicitly_unwindable_callbacks_above_the_live_ancestor() {
        let mut machine = machine();
        let high = machine.sys.contexts[0].stack_hi;
        for (id, offset, unwindable) in [(1, 0x40, false), (2, 0x100, false), (3, 0x200, true)] {
            let mut f = frame(id);
            f.stack = high - offset;
            f.unwindable = unwindable;
            machine.sys.contexts[0].frames.push(f);
        }
        machine.cpu.set_reg(Reg::Esp, high - 0x300);
        let checkpoint = Env {
            context: 0,
            cpu: &mut machine.cpu,
            sys: &mut machine.sys,
            ext: &mut machine.ext,
            thunk: 0,
        }
        .checkpoint_for_stack_unwind(high - 0x180)
        .unwrap();
        assert_eq!(machine.sys.contexts[0].frames.len(), 3);
        checkpoint
            .restore(&mut machine.cpu, &mut machine.sys)
            .unwrap();
        assert_eq!(machine.sys.contexts[0].frames.len(), 2);
        assert_eq!(machine.sys.contexts[0].frames[1].id, 2);
        assert_eq!(machine.cpu.reg(Reg::Esp), high - 0x180);
    }

    // spec: PROC-MACH-11
    #[test]
    fn stack_unwind_rejects_nonunwindable_frames_and_invalid_targets_before_any_mutation() {
        let mut machine = machine();
        let high = machine.sys.contexts[0].stack_hi;
        let low = machine.sys.contexts[0].stack_lo;
        let mut f = frame(1);
        f.stack = high - 0x200;
        machine.sys.contexts[0].frames.push(f);
        machine.cpu.set_reg(Reg::Esp, high - 0x300);
        let before = machine.cpu.capture_architecture().unwrap();
        for target in [high - 0x100, high, low - 1, high - 0x400] {
            let mut env = Env {
                context: 0,
                cpu: &mut machine.cpu,
                sys: &mut machine.sys,
                ext: &mut machine.ext,
                thunk: 0,
            };
            assert!(env.checkpoint_for_stack_unwind(target).is_err());
            assert_eq!(env.cpu.capture_architecture().unwrap(), before);
            assert_eq!(env.ctx().frames.len(), 1);
        }
    }
}
