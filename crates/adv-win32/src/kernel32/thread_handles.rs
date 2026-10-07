//! Thread opens resolve retained guest kernel owners, never host thread IDs.
use std::collections::VecDeque;

use adv_abi::{Registry, warn_unavailable, win32};
use adv_machine::{Env, Object, Outcome, process::ThreadIdentity};
use adv_os::processes::{self, Error};

use crate::util::{err, set_last_error};

/// Live contexts own their running thread. Completed diagnostic context slots
/// alone are insufficient: only retained kernel payloads keep them openable.
fn owned_thread(env: &Env<'_>, pid: u32, tid: u32) -> Option<ThreadIdentity> {
    for object in env.sys.objects.kernel_objects() {
        let identity = match object {
            Object::RemoteThread { identity, .. } => Some(identity.clone()),
            Object::Thread { ctx } => env.sys.thread_identity(*ctx),
            _ => None,
        };
        if let Some(identity) = identity {
            if identity.tid == tid && (pid == 0 || identity.process.pid == pid) {
                return Some(identity);
            }
        }
    }
    if pid == 0 || pid == env.sys.pid {
        let context = env
            .sys
            .contexts
            .iter()
            .find(|context| context.tid == tid && !context.is_done())?;
        return env.sys.thread_identity(context.id);
    }
    None
}

pub(crate) fn find_thread(
    env: &mut Env<'_>,
    pid: u32,
    tid: u32,
    complete: impl FnOnce(&mut Env<'_>, Result<ThreadIdentity, Error>) -> Outcome + 'static,
) -> Outcome {
    if tid == 0 {
        return complete(
            env,
            Err(Error::new(err::INVALID_PARAMETER, "invalid thread id")),
        );
    }
    if let Some(identity) = owned_thread(env, pid, tid) {
        return complete(env, Ok(identity));
    }
    let family = env
        .ext
        .try_get::<processes::Services>()
        .map(|services| {
            services
                .live_processes()
                .into_iter()
                .filter_map(|(target, _)| (target != env.sys.pid).then_some(target))
                .collect()
        })
        .unwrap_or_default();
    search_family(env, pid, tid, family, Box::new(complete))
}

type Complete = Box<dyn FnOnce(&mut Env<'_>, Result<ThreadIdentity, Error>) -> Outcome>;

fn search_family(
    env: &mut Env<'_>,
    pid: u32,
    tid: u32,
    mut family: VecDeque<u32>,
    complete: Complete,
) -> Outcome {
    let Some(target) = family.pop_front() else {
        return complete(
            env,
            Err(Error::new(err::INVALID_PARAMETER, "unknown thread id")),
        );
    };
    processes::visit(
        env,
        target,
        None,
        move |env| Ok(owned_thread(env, pid, tid)),
        move |env, result| match result {
            Ok(Some(identity)) => complete(env, Ok(identity)),
            Ok(None) => search_family(env, pid, tid, family, complete),
            // The target may finish after the live catalog was captured.
            Err(error) if matches!(error.code, err::INVALID_HANDLE | err::INVALID_PARAMETER) => {
                search_family(env, pid, tid, family, complete)
            }
            Err(error) => complete(env, Err(error)),
        },
    )
}

/// Windows' thread generic mapping, with limited rights implied by full query
/// and set rights. All virtual family threads share the modeled user identity.
pub(crate) fn thread_access(mut access: u32) -> Result<u32, u32> {
    if access & 0x0100_0000 != 0 {
        return Err(0xc000_0061); // STATUS_PRIVILEGE_NOT_HELD
    }
    if access & 0x8000_0000 != 0 {
        access |= 0x0002_0848;
    }
    if access & 0x4000_0000 != 0 {
        access |= 0x0002_1437;
    }
    if access & 0x2000_0000 != 0 {
        access |= 0x0012_1800;
    }
    if access & (0x1000_0000 | 0x0200_0000) != 0 {
        access |= 0x001f_ffff;
    }
    access &= !0xf200_0000;
    if access & 0x40 != 0 {
        access |= 0x800;
    }
    if access & 0x20 != 0 {
        access |= 0x400;
    }
    if access == 0 {
        return Err(0xc000_0022); // STATUS_ACCESS_DENIED
    }
    Ok(access)
}

pub(crate) fn publish_thread(
    env: &mut Env<'_>,
    identity: ThreadIdentity,
    access: u32,
    inherit: bool,
) -> Result<u32, Error> {
    // Opened local and remote aliases both carry their actual access mask.
    let handle = env
        .sys
        .objects
        .insert(Object::RemoteThread { identity, access });
    if handle == 0 {
        return Err(Error::new(
            err::NOT_ENOUGH_MEMORY,
            "thread handle table full",
        ));
    }
    if inherit {
        env.sys.objects.set_handle_flags(handle, 1, 1);
    }
    Ok(handle)
}

#[win32(kernel32, name = "OpenThread", level = implement)]
fn open(env: &mut Env<'_>, desired_access: u32, inherit: i32, tid: u32) -> Outcome {
    find_thread(env, 0, tid, move |env, result| {
        let result = result.and_then(|identity| {
            let access = thread_access(desired_access).map_err(|status| {
                Error::new(
                    if status == 0xc000_0061 {
                        1314
                    } else {
                        err::ACCESS_DENIED
                    },
                    "thread access denied",
                )
            })?;
            publish_thread(env, identity, access, inherit != 0)
        });
        match result {
            Ok(handle) => Outcome::Return(handle),
            Err(error) => {
                if error.code == 50 {
                    warn_unavailable(env, "kernel32.dll!OpenThread", &error.message);
                }
                set_last_error(env, error.code);
                Outcome::Return(0)
            }
        }
    })
}

pub(super) fn register(registry: &mut Registry) {
    registry.register(open_ABI);
}
