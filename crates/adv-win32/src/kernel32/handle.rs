//! CloseHandle and DuplicateHandle, once for every kernel object in the machine's table.
use adv_abi::Registry;
use adv_cpu::backend::{GuestExt, Prot};
use adv_machine::{Env, Object, Outcome};
use adv_os::processes;

use super::K32;
use crate::util::{err, set_last_error};
use crate::{STD_ERROR, STD_INPUT, STD_OUTPUT, stdcall};

const CURRENT_PROCESS: u32 = u32::MAX;
const CURRENT_THREAD: u32 = u32::MAX - 1;
const DUPLICATE_CLOSE_SOURCE: u32 = 1;
const DUPLICATE_SAME_ACCESS: u32 = 2;
pub(super) const INHERIT: u32 = 1;
const PROTECT_FROM_CLOSE: u32 = 2;

/// Read x86 SECURITY_ATTRIBUTES before creating any kernel payload. Security
/// descriptors are outside the guest security profile; inheritance is supported.
pub(super) fn security_inherit(env: &Env<'_>, pointer: u32) -> Result<bool, u32> {
    if pointer == 0 {
        return Ok(false);
    }
    if adv_abi::guest::check_range(env.cpu, pointer, 12, Prot::READ).is_err() {
        return Err(998);
    }
    if env.cpu.read_u32(pointer).map_err(|_| 998u32)? != 12 {
        return Err(err::INVALID_PARAMETER);
    }
    if env.cpu.read_u32(pointer + 4).map_err(|_| 998u32)? != 0 {
        return Err(50);
    }
    Ok(env.cpu.read_u32(pointer + 8).map_err(|_| 998u32)? != 0)
}

pub(super) fn set_inherit(env: &mut Env<'_>, handle: u32, inherit: bool) {
    let _ = env
        .sys
        .objects
        .set_handle_flags(handle, INHERIT, u32::from(inherit));
}

fn get_handle_information(env: &mut Env<'_>) -> Outcome {
    let (handle, out) = (env.arg(0), env.arg(1));
    let Some(flags) = env.sys.objects.handle_flags(handle) else {
        set_last_error(env, err::INVALID_HANDLE);
        return Outcome::Return(0);
    };
    if out == 0
        || adv_abi::guest::check_range(env.cpu, out, 4, Prot::WRITE).is_err()
        || env.cpu.write_u32(out, flags).is_err()
    {
        set_last_error(env, 998);
        return Outcome::Return(0);
    }
    Outcome::Return(1)
}

fn set_handle_information(env: &mut Env<'_>) -> Outcome {
    let (handle, mask, flags) = (env.arg(0), env.arg(1), env.arg(2));
    // Windows ignores unknown mask bits, including callers which pass MAXDWORD.
    if !env
        .sys
        .objects
        .set_handle_flags(handle, mask & (INHERIT | PROTECT_FROM_CLOSE), flags)
    {
        set_last_error(env, err::INVALID_HANDLE);
        return Outcome::Return(0);
    }
    Outcome::Return(1)
}

/// Legacy current-process marker retained for existing NT compatibility callers.
/// Newly opened and duplicated process handles use `Object::Process` identities.
pub struct ProcessHandle;

fn close_handle(env: &mut Env<'_>) -> Outcome {
    let h = env.arg(0);
    if env
        .sys
        .objects
        .handle_flags(h)
        .is_some_and(|flags| flags & PROTECT_FROM_CLOSE != 0)
    {
        set_last_error(env, err::INVALID_HANDLE);
        return Outcome::Return(0);
    }
    // The last handle drops the object: files close, sections lose a handle.
    let object = env.sys.objects.close(h).is_some();
    super::file_mapping::prune(env);
    if object || matches!(h, STD_INPUT | STD_OUTPUT | STD_ERROR) || h >= CURRENT_THREAD {
        return Outcome::Return(1);
    }
    log::debug!("CloseHandle({h:#x}): unknown handle");
    set_last_error(env, err::INVALID_HANDLE);
    Outcome::Return(0)
}

/// A new handle to what `source` names in this process.
fn duplicate(env: &mut Env<'_>, source: u32) -> Result<u32, u32> {
    match source {
        CURRENT_PROCESS => match env.sys.objects.insert(Object::Process {
            identity: env.sys.process_identity.clone(),
            access: 0x1f_ffff,
        }) {
            0 => Err(err::NOT_ENOUGH_MEMORY),
            handle => Ok(handle),
        },
        CURRENT_THREAD => {
            let ctx = env.context;
            let thread = env.sys.context(ctx).thread_handle;
            // The guest may have closed the handle CreateThread returned.
            Ok(match env.sys.objects.identity(thread) {
                Some(identity) => env
                    .sys
                    .objects
                    .alias_identity(identity)
                    .ok_or(err::NOT_ENOUGH_MEMORY)?,
                None => match env.sys.objects.insert(Object::Thread { ctx }) {
                    0 => return Err(err::NOT_ENOUGH_MEMORY),
                    handle => handle,
                },
            })
        }
        STD_INPUT | STD_OUTPUT | STD_ERROR => Ok(source),
        _ => {
            let identity = env
                .sys
                .objects
                .identity(source)
                .ok_or(err::INVALID_HANDLE)?;
            env.sys
                .objects
                .alias_identity(identity)
                .ok_or(err::NOT_ENOUGH_MEMORY)
        }
    }
}

enum Transfer {
    Process(std::sync::Arc<adv_machine::process::ProcessIdentity>, u32),
    Thread(adv_machine::process::ThreadIdentity, u32),
}

fn transfer(env: &Env<'_>, source: u32, desired: u32, same: bool) -> Result<Transfer, u32> {
    let (value, granted) = match env.sys.objects.get(source) {
        _ if source == CURRENT_PROCESS => (
            Transfer::Process(env.sys.process_identity.clone(), 0),
            0x1f_ffff,
        ),
        _ if source == CURRENT_THREAD => (
            Transfer::Thread(
                env.sys
                    .thread_identity(env.context)
                    .ok_or(err::INVALID_HANDLE)?,
                0,
            ),
            0x1f_ffff,
        ),
        Some(Object::Process { identity, access }) => {
            (Transfer::Process(identity.clone(), 0), *access)
        }
        Some(Object::Thread { ctx }) => (
            Transfer::Thread(env.sys.thread_identity(*ctx).ok_or(err::INVALID_HANDLE)?, 0),
            0x1f_ffff,
        ),
        Some(Object::RemoteThread { identity, access }) => {
            (Transfer::Thread(identity.clone(), 0), *access)
        }
        Some(_) => return Err(50), // Nonidentity payloads remain owned by their object table.
        None => return Err(err::INVALID_HANDLE),
    };
    let mut access = if same { granted } else { desired };
    if access & 0x1000_0000 != 0 {
        access = 0x1f_ffff;
    }
    match value {
        Transfer::Process(_, _) if access & 0x400 != 0 => access |= 0x1000,
        _ => {}
    }
    if access & !granted != 0 {
        return Err(err::ACCESS_DENIED);
    }
    Ok(match value {
        Transfer::Process(identity, _) => Transfer::Process(identity, access),
        Transfer::Thread(identity, _) => Transfer::Thread(identity, access),
    })
}

fn insert_transfer(
    env: &mut Env<'_>,
    value: Transfer,
    inherit: bool,
) -> Result<u32, processes::Error> {
    let object = match value {
        Transfer::Process(identity, access) => Object::Process { identity, access },
        Transfer::Thread(identity, access) => Object::RemoteThread { identity, access },
    };
    let handle = env.sys.objects.insert(object);
    if handle == 0 {
        Err(processes::Error::new(
            err::NOT_ENOUGH_MEMORY,
            "target handle table full",
        ))
    } else {
        set_inherit(env, handle, inherit);
        Ok(handle)
    }
}

fn finish_duplicate(
    env: &mut Env<'_>,
    result: Result<u32, processes::Error>,
    out: u32,
    owner: u32,
) -> Outcome {
    match result {
        Ok(handle) => {
            if out != 0
                && (adv_abi::guest::check_range(env.cpu, out, 4, Prot::WRITE).is_err()
                    || env.cpu.write_u32(out, handle).is_err())
            {
                // Publishing is a second transaction boundary: another caller
                // thread may have reprotected this output during HostRequest.
                if handle != 0 && owner != env.sys.pid {
                    return processes::visit(
                        env,
                        owner,
                        None,
                        move |target| {
                            target.sys.objects.close(handle);
                            super::file_mapping::prune(target);
                            Ok(())
                        },
                        move |env, _| {
                            set_last_error(env, 998);
                            Outcome::Return(0)
                        },
                    );
                }
                env.sys.objects.close(handle);
                super::file_mapping::prune(env);
                set_last_error(env, 998);
                return Outcome::Return(0);
            }
            Outcome::Return(1)
        }
        Err(error) => {
            set_last_error(env, error.code);
            Outcome::Return(0)
        }
    }
}

fn duplicate_handle(env: &mut Env<'_>) -> Outcome {
    let (source_process, source, target_process, out) =
        (env.arg(0), env.arg(1), env.arg(2), env.arg(3));
    let options = env.arg(6);
    let desired = env.arg(4);
    let inherit = env.arg(5) != 0;
    let same = options & DUPLICATE_SAME_ACCESS != 0;
    let close = options & DUPLICATE_CLOSE_SOURCE != 0;
    let source_identity = match super::child_process::process_target(env, source_process, 0x40) {
        Ok(identity) => identity,
        Err(error) => {
            set_last_error(env, error.code);
            return Outcome::Return(0);
        }
    };
    let target_identity = super::child_process::process_target(env, target_process, 0x40);
    let validation = target_identity.and_then(|identity| {
        if options & !3 != 0 {
            return Err(processes::Error::new(
                err::INVALID_PARAMETER,
                "invalid duplication options",
            ));
        }
        if out != 0 && adv_abi::guest::check_range(env.cpu, out, 4, Prot::WRITE).is_err() {
            return Err(processes::Error::new(998, "unwritable handle output"));
        }
        if source_identity.pid != env.sys.pid && source == CURRENT_THREAD {
            return Err(processes::Error::new(
                err::INVALID_HANDLE,
                "foreign current-thread pseudo-handle",
            ));
        }
        Ok(identity)
    });
    if source_identity.pid == env.sys.pid
        && validation
            .as_ref()
            .is_ok_and(|target| target.pid == env.sys.pid)
    {
        let result = (|| {
            if out == 0 {
                return Ok(0);
            }
            // Keep aliases for ordinary local kernel payloads. Identity objects
            // receive an independent access mask when rights are restricted.
            if same
                || !matches!(
                    env.sys.objects.get(source),
                    Some(
                        Object::Process { .. }
                            | Object::RemoteThread { .. }
                            | Object::Thread { .. }
                    )
                ) && source < CURRENT_THREAD
            {
                if inherit && matches!(source, STD_INPUT | STD_OUTPUT | STD_ERROR) {
                    return Err(processes::Error::new(
                        50,
                        "inheritable synthetic standard handle is unsupported",
                    ));
                }
                let handle = duplicate(env, source).map_err(|code| {
                    processes::Error::new(code, "local handle duplication failed")
                })?;
                set_inherit(env, handle, inherit);
                Ok(handle)
            } else {
                let value = transfer(env, source, desired, same).map_err(|code| {
                    processes::Error::new(code, "identity handle duplication failed")
                })?;
                insert_transfer(env, value, inherit)
            }
        })();
        if close
            && env
                .sys
                .objects
                .handle_flags(source)
                .is_none_or(|flags| flags & PROTECT_FROM_CLOSE == 0)
        {
            env.sys.objects.close(source);
            super::file_mapping::prune(env);
        }
        return finish_duplicate(env, result, out, env.sys.pid);
    }
    let source_pid = source_identity.pid;
    processes::visit(
        env,
        source_pid,
        None,
        move |source_env| {
            let result = validation.and_then(|target| {
                if out == 0 {
                    return Ok((target.pid, None));
                }
                transfer(source_env, source, desired, same)
                    .map(|value| (target.pid, Some(value)))
                    .map_err(|code| {
                        processes::Error::new(code, "cross-process handle duplication failed")
                    })
            });
            // Windows closes the source even if another duplication argument fails.
            if close
                && source_env
                    .sys
                    .objects
                    .handle_flags(source)
                    .is_none_or(|flags| flags & PROTECT_FROM_CLOSE == 0)
            {
                source_env.sys.objects.close(source);
                super::file_mapping::prune(source_env);
            }
            result
        },
        move |env, result| match result {
            Ok((pid, Some(value))) => processes::visit(
                env,
                pid,
                None,
                move |target| insert_transfer(target, value, inherit),
                move |env, result| finish_duplicate(env, result, out, pid),
            ),
            Ok((_, None)) => Outcome::Return(1),
            Err(error) => finish_duplicate(env, Err(error), out, env.sys.pid),
        },
    )
}

pub(super) fn register(reg: &mut Registry) {
    stdcall(reg, K32, "CloseHandle", 1, close_handle);
    stdcall(reg, K32, "DuplicateHandle", 7, duplicate_handle);
    stdcall(reg, K32, "GetHandleInformation", 2, get_handle_information);
    stdcall(reg, K32, "SetHandleInformation", 3, set_handle_information);
}

#[cfg(test)]
mod tests;
