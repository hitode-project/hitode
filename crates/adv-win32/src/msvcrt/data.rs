//! CRT data descriptors point at the Win32 process command-line owner.
use super::*;
use adv_cpu::backend::{Backend, MemFault};

const MODULES: [&str; 3] = ["msvcrt.dll", "msvcr80.dll", "msvcr90.dll"];

pub(super) fn register(registry: &mut Registry) {
    for module in MODULES {
        for name in ["_acmdln", "_wcmdln"] {
            registry
                .register_data(module, name, &[0; 4])
                .expect("CRT data width matches its authoritative declaration");
        }
    }
    registry
        .register_data("msvcrt.dll", "_adjust_fdiv", &[0; 4])
        .expect("legacy data width matches its authoritative declaration");
}

pub(crate) fn initialize<B: Backend>(
    machine: &mut adv_machine::Machine<B>,
) -> Result<(), MemFault> {
    // Win32-only personalities do not gain CRT exports or allocate CRT strings.
    let bindings: Vec<_> = (machine.sys.thunks.resolver() as &dyn std::any::Any)
        .downcast_ref::<Registry>()
        .map(|registry| {
            MODULES
                .into_iter()
                .flat_map(|module| {
                    ["_acmdln", "_wcmdln"]
                        .into_iter()
                        .filter_map(move |name| registry.data(module, name).map(|_| (module, name)))
                })
                .collect()
        })
        .unwrap_or_default();
    if bindings.is_empty() {
        return Ok(());
    }
    let mut env = Env {
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk: 0,
    };
    for wide in [false, true] {
        let name = if wide { "_wcmdln" } else { "_acmdln" };
        if !bindings.iter().any(|(_, binding)| *binding == name) {
            continue;
        }
        let pointer = crate::kernel32::command_line(&mut env, wide);
        if pointer == 0 {
            return Err(MemFault { addr: 0, len: 4 });
        }
        let registry = env
            .sys
            .thunks
            .resolver_mut::<Registry>()
            .expect("registry was checked before command-line initialization");
        for &(module, binding) in &bindings {
            if binding == name {
                registry
                    .set_data(module, binding, &pointer.to_le_bytes())
                    .expect("registered pointer cell retains its declared width");
            }
        }
    }
    Ok(())
}
