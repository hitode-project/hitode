//! Read-only Wine session observations for the shell's live debugger.
use super::*;
use adv_cpu::backend::GuestExt;

pub(super) fn counters(machine: &Machine<Cpu>) -> [u64; 3] {
    use crate::Engine;
    let cpu = machine.cpu.counters();
    [
        machine.sys.host_calls,
        cpu.native_entries,
        cpu.interpreter_entries,
    ]
}

impl Session {
    /// Cumulative family counters, including processes that have exited.
    pub fn debug_counters(&self) -> [u64; 3] {
        let mut totals = self.retired_debug;
        self.debug_processes(|machine| {
            for (total, count) in totals.iter_mut().zip(counters(machine)) {
                *total += count;
            }
        });
        totals
    }

    pub fn debug_modules(&self) -> Result<Vec<adv_nt::process::LoadedModule>, String> {
        adv_nt::process::loaded_modules(&self.machine.cpu, self.machine.sys.peb)
    }

    pub fn guest_place(&self, address: u32) -> String {
        let place = self
            .debug_modules()
            .ok()
            .and_then(|modules| {
                modules
                    .into_iter()
                    .find(|m| address.wrapping_sub(m.base) < m.size)
            })
            .map_or_else(
                || format!("{address:#010x}"),
                |m| format!("{}+{:#x}", m.name, address - m.base),
            );
        format!("pid {:#x} {place}", self.machine.sys.pid)
    }

    pub fn modules_report(&self) -> String {
        let mut text = String::new();
        self.debug_processes(|machine| {
            text.push_str(&format!(
                "pid {:#x}{}:\n",
                machine.sys.pid,
                if machine.sys.pid == self.machine.sys.pid {
                    " (presented)"
                } else {
                    ""
                }
            ));
            match adv_nt::process::loaded_modules(&machine.cpu, machine.sys.peb) {
                Ok(mut modules) => {
                    modules.sort_by_key(|m| m.base);
                    for m in modules {
                        text.push_str(&format!(
                            "{:#010x} {:#010x} Image    {}\n",
                            m.base, m.size, m.name
                        ));
                    }
                }
                Err(error) => text.push_str(&format!("modules unavailable: {error}\n")),
            }
        });
        text
    }

    pub fn contexts_report(&self) -> String {
        let mut text =
            String::from("Wine guest contexts (machine waits; server objects are owned by Wine):");
        self.debug_processes(|machine| {
            for context in &machine.sys.contexts {
                text.push_str(&format!(
                    "\npid {:#x} [{}] tid {:#x}: {}",
                    machine.sys.pid,
                    context.id,
                    context.tid,
                    context.state_name()
                ));
                if let Some(wait) = context.waiting_on() {
                    text.push_str(&format!(" {wait:?}"));
                }
                if let Some(thunk) = context.blocked_thunk() {
                    let call = machine.sys.thunks.get(thunk);
                    text.push_str(&format!(" in {}!{}", call.module, call.name));
                }
                if let Some(args) = context.blocked_args() {
                    text.push_str(&format!(" args {args:x?}"));
                }
                if let Some(caller) = context
                    .blocked_stack()
                    .and_then(|sp| machine.cpu.read_u32(sp).ok())
                {
                    text.push_str(&format!(" caller {caller:#010x}"));
                }
                if let Some(exit) = context.exit_code() {
                    text.push_str(&format!(" exit {exit}"));
                }
            }
        });
        text
    }

    fn debug_processes(&self, mut visit: impl FnMut(&Machine<Cpu>)) {
        visit(&self.machine);
        for machine in self.shared.driver.borrow().parked.values() {
            visit(machine);
        }
    }
}
