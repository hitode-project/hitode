//! Runtime diagnostics use the existing Windows process owner. The virtual
//! process has no debugger or console; a debugger notification has no recipient.
use super::*;

pub(super) fn register(registry: &mut Registry) {
    versions::register(
        registry,
        &[
            ("abort", abort),
            ("_amsg_exit", runtime_error),
            ("_crt_debugger_hook", debugger),
        ],
    );
}

pub(super) fn abort(env: &mut Env<'_>) -> Outcome {
    // The default SIGABRT action ends without draining CRT atexit callbacks.
    crate::kernel32::exit_process(env, 3)
}
fn runtime_error(env: &mut Env<'_>) -> Outcome {
    // Native VC90 reports the runtime code, then takes its immediate exit path
    // with 255. The configured process has no console diagnostic destination.
    crate::kernel32::exit_process(env, 255)
}
fn debugger(env: &mut Env<'_>) -> Outcome {
    // Internal x86 ABI: void __cdecl _crt_debugger_hook(int). Native VC90
    // clears a private debugger word and returns without inspecting its input.
    // No debugger is installed, so no exposed object or callback is fabricated.
    Outcome::Return(env.cpu.reg(Reg::Eax))
}
