//! Invoke an existing host binding without manufacturing a guest callback frame.
//! Personalities validate their argument source; this layer owns continuation
//! composition, binding identity and the temporary explicit argument view.
use crate::{Continuation, CtxId, Env, Outcome, Resume, TraceRecord};
use adv_cpu::backend::Reg;
use std::rc::Rc;

#[cfg(test)]
mod tests;

pub(crate) struct Arguments {
    pub context: CtxId,
    pub thunk: u32,
    pub values: Rc<[u32]>,
}

struct Invocation {
    binding: u32,
    arguments: Rc<[u32]>,
    then: Continuation,
    trace: bool,
    esp: u32,
    eip: u32,
    caller: u32,
}

impl Env<'_> {
    /// Invoke the registered host implementation with validated, owned arguments.
    /// This deliberately does not execute the binding's mutable guest entry.
    /// A scalar return runs `then`; a nonlocal resume or terminal result abandons
    /// it. The original architectural checkpoint ancestry is left unchanged.
    pub fn invoke_host_u32(
        &mut self,
        binding: u32,
        arguments: Vec<u32>,
        then: impl FnOnce(&mut Env<'_>, Resume) -> Outcome + 'static,
    ) -> Outcome {
        let function = self.sys.thunks.get(binding).f;
        let count = usize::from(function.conv.argc());
        let valid = match function.conv {
            crate::Conv::CdeclVariadic { .. } => arguments.len() >= count,
            _ => arguments.len() == count,
        };
        if !valid {
            return Outcome::Fail("host invocation argument count disagrees with binding".into());
        }
        self.sys.host_calls += 1;
        self.sys.stats.total.host_calls += 1;
        if self
            .sys
            .call_limit
            .is_some_and(|limit| self.sys.host_calls >= limit)
        {
            return Outcome::Fail("host call limit reached during explicit invocation".into());
        }
        let call = Invocation {
            binding,
            arguments: arguments.into(),
            then: Box::new(then),
            trace: self.sys.trace.enabled() && (function.trace || self.sys.trace.all),
            esp: self.cpu.reg(Reg::Esp),
            eip: self.cpu.reg(Reg::Eip),
            caller: self.caller(),
        };
        self.sys.recent.push(self.context, binding, call.caller);
        let outcome = call.scope(self, function.handler);
        call.complete(self, outcome)
    }
}

impl Invocation {
    fn scope(&self, env: &mut Env<'_>, task: impl FnOnce(&mut Env<'_>) -> Outcome) -> Outcome {
        let previous = env.sys.invocation_arguments.replace(Arguments {
            context: env.context,
            thunk: self.binding,
            values: self.arguments.clone(),
        });
        let result = task(&mut Env {
            context: env.context,
            cpu: env.cpu,
            sys: env.sys,
            ext: env.ext,
            thunk: self.binding,
        });
        env.sys.invocation_arguments = previous;
        // The enclosing dispatcher already times this work. Timing it again
        // here would double-charge nested handler and continuation execution.
        result
    }

    fn record(&self, env: &mut Env<'_>, result: u32, terminal: Option<&str>) {
        if !self.trace {
            return;
        }
        let binding = env.sys.thunks.get(self.binding);
        let record = TraceRecord {
            ctx: env.context,
            frame: env.sys.frame,
            module: &binding.module,
            name: &binding.name,
            args: &self.arguments,
            ret: u64::from(result),
        };
        if let Some(terminal) = terminal {
            env.sys
                .trace
                .terminal(&record, terminal, self.esp, self.eip, Some(self.caller));
        } else {
            env.sys.trace.record(&record);
        }
    }

    fn complete(self, env: &mut Env<'_>, outcome: Outcome) -> Outcome {
        match outcome {
            Outcome::Return(value) => {
                self.record(env, value, None);
                (self.then)(env, Resume::Returned(value))
            }
            Outcome::Call {
                target,
                args,
                then,
                stack,
                unwindable,
            } => Outcome::Call {
                target,
                args,
                stack,
                unwindable,
                then: Box::new(move |env, value| {
                    let outcome = self.scope(env, |env| then(env, value));
                    self.complete(env, outcome)
                }),
            },
            Outcome::Block {
                wait,
                then,
                allow_time_skip,
            } => Outcome::Block {
                wait,
                allow_time_skip,
                then: Box::new(move |env, value| {
                    let outcome = self.scope(env, |env| then(env, value));
                    self.complete(env, outcome)
                }),
            },
            Outcome::Defer { then } => Outcome::defer(move |env| {
                let outcome = self.scope(env, then);
                self.complete(env, outcome)
            }),
            Outcome::HostRequest { then } => Outcome::host_request(move |env| {
                let outcome = self.scope(env, then);
                self.complete(env, outcome)
            }),
            Outcome::ResumeExecution(checkpoint) => {
                self.record(env, 0, Some("resume"));
                Outcome::ResumeExecution(checkpoint)
            }
            Outcome::Raise(fault) => {
                self.record(env, 0, Some("fault"));
                Outcome::Raise(fault)
            }
            Outcome::ReturnFromCall(value) => {
                self.record(env, value, Some("callback return"));
                Outcome::ReturnFromCall(value)
            }
            Outcome::ExitThread(value) => {
                self.record(env, value, Some("thread"));
                Outcome::ExitThread(value)
            }
            Outcome::ExitProcess(value) => {
                self.record(env, value, Some("process"));
                Outcome::ExitProcess(value)
            }
            Outcome::Fail(reason) => {
                self.record(env, 0, Some("failure"));
                Outcome::Fail(reason)
            }
            Outcome::Return64(_) | Outcome::ReturnF32(_) | Outcome::ReturnF64(_) => {
                Outcome::Fail("scalar host invocation returned a non-u32 result".into())
            }
        }
    }
}
