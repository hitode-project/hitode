//! Win32 process creation marshals owned arguments to the runtime supervisor.
use std::{cell::RefCell, rc::Rc, sync::Arc};

use adv_abi::{AnsiStr, GuestPtr, Handle, Registry, WideStr, types::SECURITY_ATTRIBUTES, win32};
use adv_cpu::backend::{GuestExt, Prot};
use adv_machine::{Env, Object, Outcome, process::ProcessIdentity};
use adv_os::processes::{
    self, Error, InheritedHandle, InheritedObject, Reply, Request, Services, Spawn,
};

use crate::{Win32, util::set_last_error};

pub(super) fn process_target(
    env: &Env<'_>,
    handle: u32,
    access: u32,
) -> Result<Arc<ProcessIdentity>, Error> {
    if handle == u32::MAX {
        return Ok(env.sys.process_identity.clone());
    }
    match env.sys.objects.get(handle) {
        Some(Object::Process {
            identity,
            access: granted,
        }) if granted & access == access => Ok(identity.clone()),
        Some(Object::Process { .. }) => Err(Error::new(5, "process handle access denied")),
        _ => Err(Error::new(6, "invalid process handle")),
    }
}

fn fail(env: &mut Env<'_>, code: u32) -> Outcome {
    set_last_error(env, code);
    Outcome::Return(0)
}
fn string(env: &Env<'_>, address: u32, wide: bool) -> Result<String, u32> {
    if wide {
        let units = WideStr(GuestPtr::new(address))
            .read(env.cpu, 32768)
            .map_err(|_| 998u32)?;
        String::from_utf16(&units).map_err(|_| 87)
    } else {
        let bytes = AnsiStr(GuestPtr::new(address))
            .read(env.cpu, 32768)
            .map_err(|_| 998u32)?;
        Ok(env.ext.get::<Win32>().codec.decode(&bytes))
    }
}

/// The module token has Windows' special first-argument quoting rules. Unquoted
/// paths containing spaces are probed at each space boundary, in native order.
fn executable(env: &Env<'_>, application: Option<&str>, command: &str) -> Result<String, u32> {
    let w = env.ext.get::<Win32>();
    let mut candidates = Vec::new();
    if let Some(application) = application {
        candidates.push(application.to_owned());
    } else {
        let command = command.trim_start();
        if let Some(rest) = command.strip_prefix('"') {
            let end = rest.find('"').ok_or(87u32)?;
            candidates.push(rest[..end].into());
        } else {
            for (at, c) in command.char_indices() {
                if c.is_whitespace() && at != 0 {
                    candidates.push(command[..at].into());
                }
            }
            candidates.push(command.into());
        }
    }
    for mut candidate in candidates {
        if candidate.is_empty() {
            continue;
        }
        let leaf = candidate.rsplit(['\\', '/']).next().unwrap();
        if !leaf.contains('.') {
            candidate.push_str(".exe");
        }
        let mut paths = vec![w.vfs.normalize(&candidate)];
        if !candidate.contains(['\\', '/', ':']) {
            if let Some((directory, _)) = w.exe_path().rsplit_once('\\') {
                paths.insert(0, format!("{directory}\\{candidate}"));
            }
            paths.extend([
                format!("C:\\Windows\\System32\\{candidate}"),
                format!("C:\\Windows\\{candidate}"),
            ]);
        }
        if let Some(path) = paths
            .into_iter()
            .find(|path| w.vfs.lookup(path).is_some_and(|p| p.is_file()))
        {
            return Ok(path);
        }
    }
    Err(2)
}

fn environment(env: &mut Env<'_>, address: u32, wide: bool) -> Result<Vec<String>, u32> {
    if address == 0 {
        return super::environment_entries(env)
            .into_iter()
            .map(|v| String::from_utf16(&v).map_err(|_| 87))
            .collect();
    }
    let mut entries = Vec::new();
    let mut offset = 0u32;
    // Keep the entire copied environment bounded, including malicious unterminated blocks.
    while offset < 2 * 1024 * 1024 {
        let at = address.checked_add(offset).ok_or(998u32)?;
        let entry = string(env, at, wide)?;
        if entry.is_empty() {
            return Ok(entries);
        }
        if !entry[usize::from(entry.starts_with('='))..].contains('=') {
            return Err(87);
        }
        let bytes = if wide {
            entry.encode_utf16().count() * 2
        } else {
            env.ext.get::<Win32>().codec.encode(&entry).len()
        };
        offset = offset
            .checked_add(bytes as u32 + if wide { 2 } else { 1 })
            .ok_or(998u32)?;
        entries.push(entry);
    }
    Err(8)
}

#[allow(clippy::too_many_arguments)]
fn create(
    env: &mut Env<'_>,
    application: u32,
    command: u32,
    process_security: u32,
    thread_security: u32,
    inherit: i32,
    flags: u32,
    environment_pointer: u32,
    directory: u32,
    startup: u32,
    info: u32,
    wide: bool,
) -> Outcome {
    if !env.cpu.accessible(info, 16, Prot::WRITE) || info == 0 {
        return fail(env, 998);
    }
    if env.cpu.write(info, &[0; 16]).is_err() {
        return fail(env, 998);
    }
    if startup == 0 || !env.cpu.accessible(startup, 68, Prot::READ) {
        return fail(env, 998);
    }
    if env.cpu.read_u32(startup).unwrap_or(0) < 68 {
        return fail(env, 87);
    }
    // Unsupported semantics fail before a process is published. In particular,
    // copying arbitrary process-local handle payloads is not handle inheritance.
    const FLAGS: u32 = 4
        | 8
        | 0x10
        | 0x20
        | 0x40
        | 0x80
        | 0x100
        | 0x200
        | 0x400
        | 0x4000
        | 0x8000
        | 0x0400_0000
        | 0x0800_0000;
    if flags & !FLAGS != 0 || env.cpu.read_u32(startup + 44).unwrap_or(0) & 0x100 != 0 {
        return fail(env, 50);
    }
    let mut inherit_returns = [false; 2];
    for (index, security) in [process_security, thread_security].into_iter().enumerate() {
        if security != 0 {
            if !env.cpu.accessible(security, 12, Prot::READ) {
                return fail(env, 998);
            }
            if env.cpu.read_u32(security).unwrap_or(0) != 12 {
                return fail(env, 87);
            }
            if env.cpu.read_u32(security + 4).unwrap_or(1) != 0 {
                return fail(env, 50);
            }
            inherit_returns[index] = env.cpu.read_u32(security + 8).unwrap_or(0) != 0;
        }
    }
    let args = (|| -> Result<Spawn, u32> {
        let mut inherited = Vec::new();
        if inherit != 0 {
            for handle in env.sys.objects.inheritable_handles() {
                if inherited.len() >= 1024 {
                    return Err(8);
                }
                let object = match env.sys.objects.get(handle) {
                    Some(Object::Process { identity, access }) => {
                        InheritedObject::Process(identity.clone(), *access)
                    }
                    Some(Object::RemoteThread { identity, access }) => {
                        InheritedObject::Thread(identity.clone(), *access)
                    }
                    Some(Object::Thread { ctx }) => InheritedObject::Thread(
                        env.sys.thread_identity(*ctx).ok_or(6u32)?,
                        0x001f_ffff,
                    ),
                    _ => return Err(50),
                };
                inherited.push(InheritedHandle {
                    handle,
                    flags: env.sys.objects.handle_flags(handle).unwrap_or(0),
                    object,
                });
            }
        }
        let application = if application == 0 {
            None
        } else {
            Some(string(env, application, wide)?)
        };
        let command_line = if command == 0 {
            application
                .clone()
                .map(|p| format!("\"{p}\""))
                .ok_or(87u32)?
        } else {
            string(env, command, wide)?
        };
        let exe = executable(env, application.as_deref(), &command_line)?;
        let directory = if directory == 0 {
            env.ext.get::<Win32>().vfs.cwd.clone()
        } else {
            let path = string(env, directory, wide)?;
            let w = env.ext.get::<Win32>();
            let path = w.vfs.normalize(&path);
            if !w.vfs.is_dir(&path) {
                return Err(267);
            }
            path
        };
        let environment = environment(env, environment_pointer, flags & 0x400 != 0)?;
        let mut config = env.ext.get::<Win32>().cfg.os.clone();
        config.exe_name = exe;
        Ok(Spawn {
            parent: env.sys.pid,
            config,
            command_line,
            directory,
            environment,
            suspended: flags & 4 != 0,
            inherited,
        })
    })();
    let spec = match args {
        Ok(spec) => spec,
        Err(code) => return fail(env, code),
    };
    let Some(services) = env.ext.try_get::<Services>().cloned() else {
        return fail(env, 50);
    };
    let reply: Reply<processes::Created> = Rc::new(RefCell::new(None));
    if let Err(error) = services.push(Request::Spawn {
        spec: Box::new(spec),
        reply: reply.clone(),
    }) {
        return fail(env, error.code);
    }
    Outcome::host_request(move |env| {
        let created = match reply.borrow_mut().take() {
            Some(Ok(created)) => created,
            Some(Err(error)) => return fail(env, error.code),
            None => return fail(env, 50),
        };
        let process = env.sys.objects.insert(Object::Process {
            identity: created.process.clone(),
            access: 0x001f_ffff,
        });
        let thread = env.sys.objects.insert(Object::RemoteThread {
            identity: created.thread.clone(),
            access: 0x001f_ffff,
        });
        if process == 0 || thread == 0 {
            if process != 0 {
                env.sys.objects.close(process);
            }
            if thread != 0 {
                env.sys.objects.close(thread);
            }
            // Never leak a runnable child when publishing PROCESS_INFORMATION fails.
            let pid = created.process.pid;
            return processes::visit(
                env,
                pid,
                None,
                |target| {
                    target.sys.terminate_process_stopped(8);
                    Ok(())
                },
                |env, _| fail(env, 8),
            );
        };
        let values = [process, thread, created.process.pid, created.thread.tid];
        let bytes: Vec<_> = values.into_iter().flat_map(u32::to_le_bytes).collect();
        if !env.cpu.accessible(info, 16, Prot::WRITE) || env.cpu.write(info, &bytes).is_err() {
            env.sys.objects.close(process);
            env.sys.objects.close(thread);
            return processes::visit(
                env,
                created.process.pid,
                None,
                |target| {
                    target.sys.terminate_process_stopped(998);
                    Ok(())
                },
                |env, _| fail(env, 998),
            );
        }
        env.sys
            .objects
            .set_handle_flags(process, 1, u32::from(inherit_returns[0]));
        env.sys
            .objects
            .set_handle_flags(thread, 1, u32::from(inherit_returns[1]));
        created.publication.set(processes::Publication::Published);
        Outcome::Return(1)
    })
}

#[win32(kernel32, name = "CreateProcessA", level = implement)]
#[allow(clippy::too_many_arguments)]
fn create_a(
    env: &mut Env<'_>,
    application: AnsiStr,
    command: AnsiStr,
    process_security: GuestPtr<SECURITY_ATTRIBUTES>,
    thread_security: GuestPtr<SECURITY_ATTRIBUTES>,
    inherit: i32,
    flags: u32,
    environment: GuestPtr<()>,
    directory: AnsiStr,
    startup: GuestPtr<()>,
    info: GuestPtr<()>,
) -> Outcome {
    create(
        env,
        application.0.addr(),
        command.0.addr(),
        process_security.addr(),
        thread_security.addr(),
        inherit,
        flags,
        environment.addr(),
        directory.0.addr(),
        startup.addr(),
        info.addr(),
        false,
    )
}
#[win32(kernel32, name = "CreateProcessW", level = implement)]
#[allow(clippy::too_many_arguments)]
fn create_w(
    env: &mut Env<'_>,
    application: WideStr,
    command: WideStr,
    process_security: GuestPtr<SECURITY_ATTRIBUTES>,
    thread_security: GuestPtr<SECURITY_ATTRIBUTES>,
    inherit: i32,
    flags: u32,
    environment: GuestPtr<()>,
    directory: WideStr,
    startup: GuestPtr<()>,
    info: GuestPtr<()>,
) -> Outcome {
    create(
        env,
        application.0.addr(),
        command.0.addr(),
        process_security.addr(),
        thread_security.addr(),
        inherit,
        flags,
        environment.addr(),
        directory.0.addr(),
        startup.addr(),
        info.addr(),
        true,
    )
}
#[win32(kernel32, name = "CreateProcessInternalW", level = implement)]
#[allow(clippy::too_many_arguments)]
fn create_internal_w(
    env: &mut Env<'_>,
    token: Handle,
    application: WideStr,
    command: WideStr,
    process_security: GuestPtr<SECURITY_ATTRIBUTES>,
    thread_security: GuestPtr<SECURITY_ATTRIBUTES>,
    inherit: i32,
    flags: u32,
    environment: GuestPtr<()>,
    directory: WideStr,
    startup: GuestPtr<()>,
    info: GuestPtr<()>,
    _new_token: GuestPtr<Handle>,
) -> Outcome {
    if token.0 != 0 {
        adv_abi::warn_unavailable(
            env,
            "kernel32.dll!CreateProcessInternalW",
            "creation under an explicit user token is unavailable",
        );
        return fail(env, 50);
    }
    // With a NULL input token the native call leaves hNewToken untouched,
    // including failed creation. It does not publish a synthetic token handle.
    create(
        env,
        application.0.addr(),
        command.0.addr(),
        process_security.addr(),
        thread_security.addr(),
        inherit,
        flags,
        environment.addr(),
        directory.0.addr(),
        startup.addr(),
        info.addr(),
        true,
    )
}

pub fn register(registry: &mut Registry) {
    registry.register(create_a_ABI);
    registry.register(create_w_ABI);
    registry.register(create_internal_w_ABI);
}
