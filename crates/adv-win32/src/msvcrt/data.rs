//! CRT globals use generated storage descriptors and the Win32 command-line owner.
use super::*;
use adv_cpu::backend::{Backend, MemFault};

pub(super) fn register(registry: &mut Registry) {
    let huge = f64::INFINITY.to_bits().to_le_bytes();
    versions::register_data(
        registry,
        &[
            ("_acmdln", &[0; 4]),
            ("_wcmdln", &[0; 4]),
            ("_adjust_fdiv", &[0; 4]),
            ("_HUGE", &huge),
        ],
    );
}

pub(crate) fn initialize<B: Backend>(
    machine: &mut adv_machine::Machine<B>,
) -> Result<(), MemFault> {
    // Win32-only personalities do not gain CRT exports or allocate CRT strings.
    let bindings: Vec<_> = (machine.sys.thunks.resolver() as &dyn std::any::Any)
        .downcast_ref::<Registry>()
        .map(|registry| {
            versions::PROFILES
                .iter()
                .flat_map(|profile| {
                    let module = profile.module;
                    ["_acmdln", "_wcmdln"]
                        .into_iter()
                        .filter_map(move |name| registry.data(module, name).map(|_| (module, name)))
                })
                .collect()
        })
        .unwrap_or_default();
    let mut env = Env {
        context: machine.sys.current,
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
    legacy_stdio::initialize(&mut env)
}
