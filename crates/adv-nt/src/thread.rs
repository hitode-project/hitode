//! Threads, after Wine's `unix/thread.c`.
//!
//! Each guest thread is a machine context with its own pipes to the
//! server. Wine registers a new thread from its own host thread when that
//! first runs; here guest threads share the host thread, so the creating
//! system call registers it, and a thread the server holds suspended waits
//! in the machine until it is resumed. Wine stops and kills other threads
//! with signals; here the system call that asks suspends or ends the
//! thread's context directly.

use adv_machine::{CtxId, Env, Outcome};

use crate::object::{Attributes, read_u32, write_bytes, write_u32};
use crate::process;
use crate::protocol::{
    GetThreadInfoRequest, NewThreadRequest, ResumeThreadRequest, SuspendThreadRequest,
    TerminateThreadRequest,
};
use crate::server;
use crate::status::{self, Error};

const CURRENT_PROCESS: u32 = 0xffff_ffff;
const CURRENT_THREAD: u32 = 0xffff_fffe;
const THREAD_ALL_ACCESS: u32 = 0x1f_ffff;
const THREAD_CREATE_FLAGS_CREATE_SUSPENDED: u32 = 1;

/// PS_ATTRIBUTE_LIST (i386): its length, then 16-byte attributes of an
/// id, a size, a value or pointer, and a pointer to the returned size.
const PS_ATTRIBUTE_SIZE: u32 = 16;
const PS_ATTRIBUTE_CLIENT_ID: u32 = 0x1_0003;
const PS_ATTRIBUTE_TEB_ADDRESS: u32 = 0x1_0004;
const PS_ATTRIBUTE_GROUP_AFFINITY: u32 = 0x2_000b;

/// NtCreateThreadEx(*handle, access, *attributes, process, start, param,
/// flags, zero_bits, stack_commit, stack_reserve, *attribute_list)
pub fn create(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[
        handle,
        access,
        attributes,
        process,
        start,
        param,
        flags,
        _,
        _,
        stack_reserve,
        attribute_list,
    ] = args
    else {
        unreachable!()
    };
    status::finish((|| {
        if process != CURRENT_PROCESS {
            return Err(Error::Unsupported("threads of another process".into()));
        }
        if flags & !THREAD_CREATE_FLAGS_CREATE_SUSPENDED != 0 {
            return Err(Error::Unsupported(format!(
                "thread creation flags {flags:#x}"
            )));
        }
        let objattr = match attributes {
            0 => Vec::new(),
            attributes => Attributes::read(env, attributes)?.wire(),
        };
        let new = server::prepare_thread(env)?;
        let mut request = NewThreadRequest {
            process,
            access: if access == 0 {
                THREAD_ALL_ACCESS
            } else {
                access
            },
            flags,
            request_fd: new.request_fd(),
            ..Default::default()
        };
        let (reply, _) = server::request(env, &mut request, &[&objattr], &mut [])?;
        status::check(reply.header.error)?;
        write_u32(env, handle, reply.handle)?;

        let context = process::create_thread(env, reply.tid, start, param, stack_reserve)
            .map_err(|error| Error::Unsupported(format!("thread stack: {error}")))?;
        let teb = env.sys.contexts[context as usize].teb;
        if server::init_thread(env, new, context, reply.tid, teb, start)? {
            env.sys.suspend_context(context);
        }
        if attribute_list != 0 {
            update_attribute_list(env, attribute_list, reply.tid, teb)?;
        }
        Ok(())
    })())
}

/// Reports the new thread's ids and TEB through the attributes that ask
/// for them, as Wine's `update_attr_list`.
fn update_attribute_list(env: &mut Env<'_>, list: u32, tid: u32, teb: u32) -> Result<(), Error> {
    let count = read_u32(env, list)?.saturating_sub(4) / PS_ATTRIBUTE_SIZE;
    let client_id: Vec<u8> = [env.sys.pid, tid]
        .iter()
        .flat_map(|id| id.to_le_bytes())
        .collect();
    for index in 0..count {
        let attribute = list + 4 + index * PS_ATTRIBUTE_SIZE;
        let value = match read_u32(env, attribute)? {
            PS_ATTRIBUTE_CLIENT_ID => client_id.clone(),
            PS_ATTRIBUTE_TEB_ADDRESS => teb.to_le_bytes().to_vec(),
            PS_ATTRIBUTE_GROUP_AFFINITY => {
                return Err(Error::Unsupported("thread group affinity".into()));
            }
            _ => continue,
        };
        let size = (read_u32(env, attribute + 4)? as usize).min(value.len());
        write_bytes(env, read_u32(env, attribute + 8)?, &value[..size])?;
        let returned = read_u32(env, attribute + 12)?;
        if returned != 0 {
            write_u32(env, returned, size as u32)?;
        }
    }
    Ok(())
}

/// The context of the thread `handle` names, if it is one of ours.
fn context_of(env: &mut Env<'_>, handle: u32) -> Result<Option<CtxId>, Error> {
    let mut info = GetThreadInfoRequest {
        handle,
        ..Default::default()
    };
    let (info, _) = server::request(env, &mut info, &[], &mut [])?;
    status::check(info.header.error)?;
    Ok(server::context_of(env, info.tid))
}

/// NtSuspendThread(thread, *previous_count)
pub fn suspend(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[thread, previous] = args else {
        unreachable!()
    };
    status::finish((|| {
        let mut request = SuspendThreadRequest {
            handle: thread,
            ..Default::default()
        };
        let (reply, _) = server::request(env, &mut request, &[], &mut [])?;
        status::check(reply.header.error)?;
        if reply.count == 0
            && let Some(context) = context_of(env, thread)?
        {
            if context == env.context {
                return Err(Error::Unsupported("a thread suspending itself".into()));
            }
            env.sys.suspend_context(context);
        }
        if previous != 0 {
            write_u32(env, previous, reply.count as u32)?;
        }
        Ok(())
    })())
}

/// NtResumeThread(thread, *previous_count)
pub fn resume(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[thread, previous] = args else {
        unreachable!()
    };
    status::finish((|| {
        let mut request = ResumeThreadRequest {
            handle: thread,
            ..Default::default()
        };
        let (reply, _) = server::request(env, &mut request, &[], &mut [])?;
        status::check(reply.header.error)?;
        if reply.count == 1
            && let Some(context) = context_of(env, thread)?
        {
            env.sys.resume_context(context);
        }
        if previous != 0 {
            write_u32(env, previous, reply.count as u32)?;
        }
        Ok(())
    })())
}

/// NtTerminateThread(thread, exit_code)
pub fn terminate(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[thread, exit_code] = args else {
        unreachable!()
    };
    let mut current = thread == CURRENT_THREAD;
    if !current || exit_code != 0 {
        let mut request = TerminateThreadRequest {
            handle: thread,
            exit_code: exit_code as i32,
            ..Default::default()
        };
        let reply = match server::request(env, &mut request, &[], &mut []) {
            Ok((reply, _)) => reply,
            Err(error) => return status::finish(Err(error)),
        };
        if reply.header.error != status::SUCCESS {
            return Outcome::Return(reply.header.error);
        }
        current = reply.self_ != 0;
    }
    if !current {
        // The server has killed the thread; its context ends where it stands.
        return status::finish((|| {
            if let Some(context) = context_of(env, thread)? {
                server::end_thread(env, context);
                env.sys.terminate_context(context, exit_code);
            }
            Ok(())
        })());
    }
    server::end_thread(env, env.context);
    Outcome::ExitThread(exit_code)
}
