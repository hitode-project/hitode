//! CRT thread entry/exit borrow the existing Win32 scheduler, thread handle,
//! DLL/TLS notifications and module/thread CRT error-cell cleanup.
use adv_abi::{
    Api, GuestPtr, Registry,
    guest::{check_range, memory_failure},
    win32,
};
use adv_cpu::backend::{GuestExt, Prot};
use adv_machine::{Env, Outcome};

use super::{floating, invalid_parameter_result};
use crate::util::set_last_error;

const MODULES: [&str; 3] = ["msvcrt.dll", "msvcr80.dll", "msvcr90.dll"];

#[cfg(test)]
mod tests;

pub(super) fn register(registry: &mut Registry) {
    for module in MODULES {
        registry.register(Api {
            module,
            ..begin_ABI
        });
        registry.register(Api { module, ..end_ABI });
    }
}

fn api(env: &Env<'_>) -> String {
    let thunk = env.sys.thunks.get(env.thunk);
    format!("{}!{}", thunk.module, thunk.name)
}

fn fail(env: &mut Env<'_>, errno: i32, dos: u32) -> Outcome {
    // The existing errno owner already contains the adjacent DOS-error DWORD.
    // No new CRT TLS table or thread reference is introduced here.
    let cell = floating::errno_address(env);
    if cell.is_null() {
        return Outcome::Fail("CRT thread errno allocation failed".into());
    }
    if let Err(error) = check_range(env.cpu, cell.addr(), 8, Prot::WRITE) {
        return memory_failure(error);
    }
    if let Err(error) = cell
        .write(env.cpu, errno)
        .and_then(|_| env.cpu.write_u32(cell.addr() + 4, dos))
    {
        return memory_failure(error);
    }
    set_last_error(env, dos);
    Outcome::Return(0)
}

fn unsupported(env: &mut Env<'_>, reason: &str) -> Outcome {
    adv_abi::warn_unavailable(env, &api(env), reason);
    fail(env, 22, 50)
}

#[win32(msvcrt, name = "_beginthreadex", level = implement)]
fn begin(
    env: &mut Env<'_>,
    security: GuestPtr<()>,
    stack_size: u32,
    start: GuestPtr<()>,
    argument: GuestPtr<()>,
    flags: u32,
    thread_id: GuestPtr<u32>,
) -> Outcome {
    // Authored VC90 oracle: success and NULL-entry validation set LE=0 while
    // preserving DOS errno. Legacy msvcrt instead preserves LE, has no returning
    // invalid-handler API and returns UINTPTR_MAX for its NULL-entry validation.
    let legacy = env.sys.thunks.get(env.thunk).module == "msvcrt.dll";
    if !legacy {
        set_last_error(env, 0);
    }
    if start.is_null() {
        if legacy {
            return match floating::errno_address(env).write(env.cpu, 22) {
                Ok(()) => Outcome::Return(u32::MAX),
                Err(error) => memory_failure(error),
            };
        }
        return invalid_parameter_result(env, "_beginthreadex", 22, 0);
    }
    if flags != 0 {
        return unsupported(
            env,
            "only immediate default-stack-mode CRT threads are supported; suspended/reservation/other initialization flags are unavailable",
        );
    }
    if !security.is_null() {
        return unsupported(
            env,
            "custom guest thread security attributes are unavailable",
        );
    }
    if !thread_id.is_null()
        && let Err(error) = check_range(env.cpu, thread_id.addr(), 4, Prot::WRITE)
    {
        adv_abi::warn_unavailable(
            env,
            &api(env),
            "thread-id output is not writable; rejected before allocating a guest thread",
        );
        return memory_failure(error);
    }
    // Protect the existing VM's page/guard rounding from a u32 overflow. Native
    // VC90 reports EINVAL/ERROR_INVALID_PARAMETER without its invalid callback.
    if stack_size
        .checked_add(0xffff)
        .and_then(|n| n.checked_add(4096))
        .is_none()
    {
        return fail(env, 22, 87);
    }
    let stack = if stack_size == 0 {
        0x10_0000
    } else {
        stack_size
    };
    let name = format!("CrtT{}", env.sys.contexts.len());
    let id = match crate::kernel32::spawn_thread(env, &name, start.addr(), argument.addr(), stack) {
        Ok(id) => id,
        Err(_) => {
            adv_abi::warn_unavailable(
                env,
                &api(env),
                "guest thread/stack/TLS allocation failed within the shared scheduler profile",
            );
            return fail(env, 12, 8);
        }
    };
    let context = env.sys.context(id);
    let (tid, handle) = (context.tid, context.thread_handle);
    if !thread_id.is_null()
        && let Err(error) = thread_id.write(env.cpu, tid)
    {
        // No guest callback or scheduling occurs after the complete output
        // preflight. A backend write failure must not leave a runnable orphan.
        env.sys.terminate_context(id, 0xc0000005);
        env.sys.objects.close(handle);
        return memory_failure(error);
    }
    Outcome::Return(handle)
}

#[win32(msvcrt, name = "_endthreadex", level = implement)]
fn end(env: &mut Env<'_>, code: u32) -> Outcome {
    // The caller retains the waitable Win32 handle. The existing exit path runs
    // DLL/TLS detach, CRT error/time/random cleanup and publishes the real code.
    crate::kernel32::exit_thread(env, code)
}
