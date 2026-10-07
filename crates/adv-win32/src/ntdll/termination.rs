//! Native process termination shares the Win32 process/supervisor owner.
use adv_abi::{Handle, Registry, warn_unavailable, win32};
use adv_machine::{Env, Object, Outcome};

#[win32(ntdll, name = "NtTerminateProcess", level = implement)]
fn terminate(env: &mut Env<'_>, process: Handle, exit_status: i32) -> Outcome {
    let code = exit_status as u32;
    if process.0 == 0 {
        let completion = &env.sys.process_identity.completion;
        if !completion.begin_termination(code) {
            return Outcome::ExitProcess(code);
        }
        // NULL publishes termination and retires peers while preserving the
        // caller. A second NULL call terminates that remaining caller.
        env.cpu.quiesce_memory();
        env.sys.terminate_other_contexts(code);
        return Outcome::Return(0);
    }
    if process.0 != u32::MAX
        && !matches!(env.sys.objects.get(process.0), Some(Object::Process { .. }))
    {
        return Outcome::Return(
            if process.0 == u32::MAX - 1 || env.sys.objects.get(process.0).is_some() {
                0xc0000024 // STATUS_OBJECT_TYPE_MISMATCH
            } else {
                0xc0000008 // STATUS_INVALID_HANDLE
            },
        );
    }
    let target = match crate::kernel32::child_process::process_target(env, process.0, 1) {
        Ok(target) => target,
        Err(error) => {
            return Outcome::Return(if error.code == 5 {
                0xc0000022
            } else {
                0xc0000008
            });
        }
    };
    if target.completion.exit_code().is_some()
        || (target.pid != env.sys.pid && target.completion.exit_status().is_some())
    {
        return Outcome::Return(0xc000010a); // STATUS_PROCESS_IS_TERMINATING
    }
    crate::kernel32::terminate_process_owner(env, target.pid, code, |env, result| {
        let status = match result {
            Ok(_) => 0,
            Err(error) => match error.code {
                5 | 87 => 0xc000010a,
                6 => 0xc0000008,
                8 => 0xc0000017,
                50 => {
                    warn_unavailable(env, "ntdll.dll!NtTerminateProcess", &error.message);
                    0xc0000002
                }
                _ => 0xc0000001,
            },
        };
        Outcome::Return(status)
    })
}

pub(super) fn register(registry: &mut Registry) {
    registry.register(terminate_ABI);
    registry.register(adv_abi::Api {
        name: "ZwTerminateProcess",
        ..terminate_ABI
    });
}

#[cfg(test)]
mod tests;
