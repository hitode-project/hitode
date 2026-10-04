//! Neutral delivery of precise CPU stops through the existing continuation stack.
//! Personalities own exception records, handler search and context layouts.

use crate::{Env, Outcome, Sys, sched::CtxId};
use adv_cpu::backend::{ArchitecturalState, Fault, Guest, StateError};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Exception {
    Trap { vector: u8, pc: u32 },
    Fault(Fault),
}

pub type ExceptionHandler = fn(&mut Env<'_>, Exception, Checkpoint) -> Outcome;

/// One-use architectural image tied to a context and its live host-call ancestry.
/// No scheduler, clock, fuel, cache or interrupt state belongs in this image.
pub struct Checkpoint {
    pub state: Box<ArchitecturalState>,
    machine: std::sync::Arc<()>,
    context: CtxId,
    depth: usize,
    frame: Option<u64>,
}

impl Env<'_> {
    pub fn checkpoint(&mut self) -> Result<Checkpoint, StateError> {
        Checkpoint::capture(self.cpu, self.sys)
    }
}

impl Checkpoint {
    pub(crate) fn capture(cpu: &mut dyn Guest, sys: &Sys) -> Result<Self, StateError> {
        let context = sys.current_context();
        Ok(Self {
            state: Box::new(cpu.capture_architecture()?),
            machine: sys.checkpoint_identity.clone(),
            context: context.id,
            depth: context.frames.len(),
            frame: context.frames.last().map(|frame| frame.id),
        })
    }

    pub(crate) fn restore(self, cpu: &mut dyn Guest, sys: &Sys) -> Result<(), String> {
        let context = sys.current_context();
        if !std::sync::Arc::ptr_eq(&self.machine, &sys.checkpoint_identity)
            || self.context != context.id
            || self.depth != context.frames.len()
            || self.frame != context.frames.last().map(|frame| frame.id)
        {
            return Err("architectural resume crossed its live continuation boundary".into());
        }
        // Crossing a host callback requires an explicit unwind/cancellation protocol.
        if context
            .frames
            .last()
            .is_some_and(|frame| self.state.regs[4] >= frame.stack)
        {
            return Err("architectural resume requires unsupported continuation unwind".into());
        }
        cpu.restore_architecture(&self.state)
            .map_err(|error| error.to_string())
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
    fn different_context_or_replaced_ancestor_cannot_reuse_a_checkpoint() {
        let mut machine = machine();
        machine.cpu.set_reg(Reg::Eax, 0x1234);
        let mut checkpoint = Checkpoint::capture(&mut machine.cpu, &machine.sys).unwrap();
        checkpoint.state.regs[0] = 0xdead;
        machine.sys.current = 1;
        assert!(
            checkpoint
                .restore(&mut machine.cpu, &machine.sys)
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
                .restore(&mut machine.cpu, &machine.sys)
                .unwrap_err()
                .contains("boundary")
        );
        assert_eq!(machine.cpu.reg(Reg::Eax), 0x1234);
    }
    #[test]
    fn nonlocal_stack_escape_requires_explicit_continuation_unwind() {
        let mut machine = machine();
        machine.sys.contexts[0].frames.push(frame(1));
        let mut checkpoint = Checkpoint::capture(&mut machine.cpu, &machine.sys).unwrap();
        checkpoint.state.regs[4] = 0x1000;
        let before = machine.cpu.capture_architecture().unwrap();
        assert!(
            checkpoint
                .restore(&mut machine.cpu, &machine.sys)
                .unwrap_err()
                .contains("unwind")
        );
        assert_eq!(machine.cpu.capture_architecture().unwrap(), before);
    }
    #[test]
    fn equal_context_numbers_in_another_machine_do_not_authorize_resume() {
        let mut source = machine();
        let mut other = machine();
        let checkpoint = Checkpoint::capture(&mut source.cpu, &source.sys).unwrap();
        let before = other.cpu.capture_architecture().unwrap();
        assert!(
            checkpoint
                .restore(&mut other.cpu, &other.sys)
                .unwrap_err()
                .contains("boundary")
        );
        assert_eq!(other.cpu.capture_architecture().unwrap(), before);
    }
}
