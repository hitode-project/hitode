//! Thread frontends marshal a real NT call over the same stopped process owner.
use super::*;
use adv_abi::guest::check_range;
use adv_machine::process::ThreadIdentity;

const ACCESS_VIOLATION: u32 = 0xc0000005;
const NOT_SUPPORTED: u32 = 0xc00000bb;
type Done = Box<dyn FnOnce(&mut Env<'_>, u32) -> Outcome>;

fn status(error: &processes::Error) -> u32 {
    match error.code {
        5 => 0xc0000022,
        6 => 0xc0000008,
        8 => 0xc0000017,
        50 => NOT_SUPPORTED,
        87 => 0xc000000d,
        998 => ACCESS_VIOLATION,
        _ => 0xc0000001,
    }
}
fn front_status(env: &mut Env<'_>, status: u32, success: u32, failure: u32) -> Outcome {
    if status & 0x80000000 == 0 {
        return Outcome::Return(success);
    }
    set_last_error(env, super::super::file::native::win32_error(status));
    Outcome::Return(failure)
}
fn scratch(env: &mut Env<'_>, size: u32) -> Result<u32, Outcome> {
    let pointer = env
        .cpu
        .reg(Reg::Esp)
        .checked_sub(size)
        .ok_or_else(|| Outcome::Fail("thread boundary stack underflow".into()))?;
    // A guest may legitimately pivot ESP to another writable allocation. The
    // transport follows the actual call frame, just like Outcome::Call does.
    if check_range(env.cpu, pointer, size, Prot::RW).is_err() {
        return Err(Outcome::Fail(
            "thread boundary scratch is not writable".into(),
        ));
    }
    env.cpu
        .write(pointer, &vec![0; size as usize])
        .map_err(|error| Outcome::Fail(error.to_string()))?;
    Ok(pointer)
}

pub(super) fn context_front(env: &mut Env<'_>, set: bool) -> Outcome {
    let name = if set {
        "NtSetContextThread"
    } else {
        "NtGetContextThread"
    };
    let args = [env.arg(0), env.arg(1)];
    match crate::native::overridden(env, name) {
        Ok(Some(target)) => Outcome::call(target, args, |env, value| {
            front_status(env, value.value(), 1, 0)
        }),
        Ok(None) => thread_context_api(env, set),
        Err(_) => crate::native::unavailable(env, name),
    }
}

pub(super) fn suspend_front(env: &mut Env<'_>, suspend: bool) -> Outcome {
    let handle = env.arg(0);
    let name = if suspend {
        "NtSuspendThread"
    } else {
        "NtResumeThread"
    };
    match crate::native::overridden(env, name) {
        Ok(Some(target)) => {
            let output = match scratch(env, 4) {
                Ok(output) => output,
                Err(error) => return error,
            };
            Outcome::call_on_stack(target, [handle, output], output, move |env, value| {
                let status = value.value();
                let previous = env.cpu.read_u32(output).unwrap_or(0);
                front_status(env, status, previous, u32::MAX)
            })
        }
        Ok(None) => change_suspend_count(env, suspend),
        Err(_) => crate::native::unavailable(env, name),
    }
}

struct Created {
    identity: ThreadIdentity,
    handle: u32,
}

pub(crate) fn create_native(env: &mut Env<'_>, args: [u32; 11]) -> Outcome {
    create(env, args, Box::new(|_, status| Outcome::Return(status)))
}

fn create(env: &mut Env<'_>, args: [u32; 11], then: Done) -> Outcome {
    let [
        output,
        desired,
        attrs,
        process,
        start,
        parameter,
        flags,
        zero_bits,
        commit,
        reserve,
        attributes,
    ] = args;
    if check_range(env.cpu, output, 4, Prot::WRITE).is_err() {
        return then(env, ACCESS_VIOLATION);
    }
    let mut inherit = false;
    if attrs != 0 {
        if check_range(env.cpu, attrs, 24, Prot::READ).is_err() {
            return then(env, ACCESS_VIOLATION);
        }
        let fields: [u32; 6] =
            std::array::from_fn(|i| env.cpu.read_u32(attrs + i as u32 * 4).unwrap_or(0));
        if fields[0] != 24 || fields[1] != 0 || fields[2] != 0 || fields[3] & !0x1ff2 != 0 {
            return then(env, 0xc000000d);
        }
        if fields[4] != 0 || fields[5] != 0 {
            adv_abi::warn_unavailable(
                env,
                "ntdll.dll!NtCreateThreadEx",
                "thread security descriptors and impersonation quality are unavailable",
            );
            return then(env, NOT_SUPPORTED);
        }
        inherit = fields[3] & 2 != 0;
    }
    if flags & !1 != 0 || zero_bits != 0 || attributes != 0 {
        adv_abi::warn_unavailable(
            env,
            "ntdll.dll!NtCreateThreadEx",
            "extended creation flags, ZeroBits and PS_ATTRIBUTE_LIST are unavailable",
        );
        return then(env, NOT_SUPPORTED);
    }
    let access = match super::super::thread_access(desired) {
        Ok(access) => access,
        Err(error) => return then(env, error),
    };
    let identity = match super::super::child_process::process_target(env, process, 2) {
        Ok(identity) => identity,
        Err(error) => return then(env, status(&error)),
    };
    if identity.completion.exit_status().is_some() {
        return then(env, 0xc000010a);
    }
    let pid = identity.pid;
    let stack = reserve.max(commit).max(0x10000);
    let stack = if reserve == 0 && commit == 0 {
        0x100000
    } else {
        stack
    };
    processes::visit(
        env,
        pid,
        None,
        move |target| {
            if target
                .sys
                .process_identity
                .completion
                .exit_status()
                .is_some()
            {
                return Err(processes::Error::new(5, "target process is terminating"));
            }
            let name = format!("T{}", target.sys.contexts.len());
            let id = super::super::module::spawn_thread(target, &name, start, parameter, stack)
                .map_err(|error| processes::Error::new(8, error.to_string()))?;
            // Publication may need a second stopped-machine visit. The new thread
            // remains gated until its handle and output have both been published.
            target.sys.suspend_context(id);
            Ok(Created {
                identity: target.sys.thread_identity(id).unwrap(),
                handle: target.sys.context(id).thread_handle,
            })
        },
        move |env, result| {
            let created = match result {
                Ok(created) => created,
                Err(error) => return then(env, status(&error)),
            };
            let id = created.identity.context;
            let original = created.handle;
            let local = pid == env.sys.pid;
            let handle = if local {
                if access != 0x1fffff {
                    *env.sys.objects.get_mut(original).unwrap() = Object::RemoteThread {
                        identity: created.identity.clone(),
                        access,
                    };
                }
                super::super::handle::set_inherit(env, original, inherit);
                Ok(original)
            } else {
                super::super::publish_thread(env, created.identity, access, inherit)
            };
            let handle = match handle {
                Ok(handle)
                    if check_range(env.cpu, output, 4, Prot::WRITE).is_ok()
                        && env.cpu.write_u32(output, handle).is_ok() =>
                {
                    handle
                }
                handle => {
                    let failure = match handle {
                        Ok(handle) => {
                            env.sys.objects.close(handle);
                            ACCESS_VIOLATION
                        }
                        Err(error) => status(&error),
                    };
                    return processes::visit(
                        env,
                        pid,
                        None,
                        move |target| {
                            target.sys.terminate_context_stopped(id, failure);
                            target.sys.objects.close(original);
                            Ok(())
                        },
                        move |env, _| then(env, failure),
                    );
                }
            };
            processes::visit(
                env,
                pid,
                None,
                move |target| {
                    if target.sys.context(id).is_done() {
                        return Err(processes::Error::new(
                            5,
                            "new thread terminated before publication",
                        ));
                    }
                    if !local {
                        target.sys.objects.close(original);
                    }
                    if flags & 1 == 0 {
                        target.sys.resume_context(id);
                    }
                    Ok(())
                },
                move |env, result| match result {
                    Ok(()) => then(env, 0),
                    Err(error) => {
                        env.sys.objects.close(handle);
                        let _ = env.cpu.write_u32(output, 0);
                        then(env, status(&error))
                    }
                },
            )
        },
    )
}

pub(crate) fn rtl_create_native(env: &mut Env<'_>, args: [u32; 10]) -> Outcome {
    let [
        process,
        security,
        suspended,
        zero_bits,
        reserve,
        commit,
        start,
        parameter,
        output,
        client,
    ] = args;
    if (output != 0 && check_range(env.cpu, output, 4, Prot::WRITE).is_err())
        || (client != 0 && check_range(env.cpu, client, 8, Prot::WRITE).is_err())
    {
        return Outcome::Return(ACCESS_VIOLATION);
    }
    if security != 0 {
        adv_abi::warn_unavailable(
            env,
            "ntdll.dll!RtlCreateUserThread",
            "explicit thread security descriptors are unavailable",
        );
        return Outcome::Return(NOT_SUPPORTED);
    }
    let buffer = match scratch(env, 4) {
        Ok(buffer) => buffer,
        Err(error) => return error,
    };
    let native_args = [
        buffer,
        0x1fffff,
        0,
        process,
        start,
        parameter,
        u32::from(suspended as u8 != 0),
        zero_bits,
        commit,
        reserve,
        0,
    ];
    let done: Done = Box::new(move |env, status| {
        if status & 0x80000000 != 0 {
            return Outcome::Return(status);
        }
        let handle = env.cpu.read_u32(buffer).unwrap_or(0);
        let identity = match thread_target(env, handle, 0x40) {
            Ok(identity) => identity,
            Err(error) => return Outcome::Return(self::status(&error)),
        };
        if (output != 0 && check_range(env.cpu, output, 4, Prot::WRITE).is_err())
            || (client != 0 && check_range(env.cpu, client, 8, Prot::WRITE).is_err())
        {
            env.sys.objects.close(handle);
            return Outcome::Return(ACCESS_VIOLATION);
        }
        if output != 0 {
            let _ = env.cpu.write_u32(output, handle);
        }
        if client != 0 {
            let _ = env.cpu.write_u32(client, identity.process.pid);
            let _ = env.cpu.write_u32(client + 4, identity.tid);
        }
        if output == 0 {
            env.sys.objects.close(handle);
        }
        Outcome::Return(0)
    });
    match crate::native::overridden(env, "NtCreateThreadEx") {
        Ok(Some(target)) => {
            Outcome::call_on_stack(target, native_args, buffer, move |env, value| {
                done(env, value.value())
            })
        }
        Ok(None) => create(env, native_args, done),
        Err(_) => crate::native::unavailable(env, "NtCreateThreadEx"),
    }
}

pub(crate) fn terminate_native(env: &mut Env<'_>, handle: u32, code: u32) -> Outcome {
    let identity = match thread_target(env, handle, 1) {
        Ok(identity) => identity,
        Err(error) => return Outcome::Return(status(&error)),
    };
    if identity.completion.exit_code().is_some() {
        return Outcome::Return(0);
    }
    if identity.process.pid == env.sys.pid {
        if identity.context == env.context {
            return Outcome::ExitThread(code);
        }
        env.cpu.quiesce_memory();
        env.sys.terminate_context(identity.context, code);
        return Outcome::Return(0);
    }
    processes::visit(
        env,
        identity.process.pid,
        None,
        move |target| {
            target.sys.terminate_context_stopped(identity.context, code);
            Ok(())
        },
        |_, result| Outcome::Return(result.as_ref().err().map_or(0, status)),
    )
}

pub(super) fn terminate_front(env: &mut Env<'_>) -> Outcome {
    let args = [env.arg(0), env.arg(1)];
    match crate::native::overridden(env, "NtTerminateThread") {
        Ok(Some(target)) => Outcome::call(target, args, |env, value| {
            front_status(env, value.value(), 1, 0)
        }),
        Ok(None) => terminate_thread_legacy(env),
        Err(_) => crate::native::unavailable(env, "NtTerminateThread"),
    }
}

pub(super) fn exit_front(env: &mut Env<'_>) -> Outcome {
    let code = env.arg(0);
    match crate::native::overridden(env, "RtlExitUserThread") {
        Ok(Some(target)) => Outcome::call(target, [code], |_, _| {
            Outcome::Fail("RtlExitUserThread returned to its caller".into())
        }),
        Ok(None) => super::super::module::exit_thread(env, code),
        Err(_) => crate::native::unavailable(env, "RtlExitUserThread"),
    }
}

pub(super) fn create_front(env: &mut Env<'_>, remote: bool) -> Outcome {
    let shift = u32::from(remote);
    let process = if remote { env.arg(0) } else { u32::MAX };
    let (security, stack, start, parameter, flags, tid) = (
        env.arg(shift),
        env.arg(shift + 1),
        env.arg(shift + 2),
        env.arg(shift + 3),
        env.arg(shift + 4),
        env.arg(shift + 5),
    );
    let inherit = match super::super::handle::security_inherit(env, security) {
        Ok(inherit) => inherit,
        Err(code) => {
            return api_error(
                env,
                processes::Error::new(code, "invalid thread security attributes"),
                0,
            );
        }
    };
    if flags & !(4 | 0x10000) != 0
        || (tid != 0 && check_range(env.cpu, tid, 4, Prot::WRITE).is_err())
    {
        return api_error(
            env,
            processes::Error::new(
                if flags & !(4 | 0x10000) != 0 { 87 } else { 998 },
                "invalid thread creation flags or output",
            ),
            0,
        );
    }
    let buffer = match scratch(env, 32) {
        Ok(buffer) => buffer,
        Err(error) => return error,
    };
    let attrs = buffer + 8;
    let _ = env.cpu.write_u32(attrs, 24);
    let _ = env.cpu.write_u32(attrs + 12, if inherit { 2 } else { 0 });
    let args = [
        buffer,
        0x1fffff,
        attrs,
        process,
        start,
        parameter,
        u32::from(flags & 4 != 0),
        0,
        if flags & 0x10000 == 0 { stack } else { 0 },
        if flags & 0x10000 != 0 { stack } else { 0 },
        0,
    ];
    let done: Done = Box::new(move |env, status| {
        if status & 0x80000000 != 0 {
            return front_status(env, status, 0, 0);
        }
        let handle = env.cpu.read_u32(buffer).unwrap_or(0);
        if handle == 0 {
            return api_error(
                env,
                processes::Error::new(6, "native creation did not publish a thread handle"),
                0,
            );
        }
        if tid != 0 {
            let identity = match thread_target(env, handle, 0x40) {
                Ok(identity) => identity,
                Err(error) => return api_error(env, error, 0),
            };
            if check_range(env.cpu, tid, 4, Prot::WRITE).is_err()
                || env.cpu.write_u32(tid, identity.tid).is_err()
            {
                env.sys.objects.close(handle);
                return api_error(
                    env,
                    processes::Error::new(998, "thread-id publication failed"),
                    0,
                );
            }
        }
        Outcome::Return(handle)
    });
    match crate::native::overridden(env, "NtCreateThreadEx") {
        Ok(Some(target)) => Outcome::call_on_stack(target, args, buffer, move |env, value| {
            done(env, value.value())
        }),
        Ok(None) => create(env, args, done),
        Err(_) => crate::native::unavailable(env, "NtCreateThreadEx"),
    }
}
