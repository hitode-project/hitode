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
    DupHandleRequest, GetObjectInfoRequest, GetThreadInfoRequest, NewThreadRequest,
    ResumeThreadRequest, SuspendThreadRequest, TerminateThreadRequest,
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
        crate::information::current_process(env, process)?;
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
    let mut request = GetThreadInfoRequest {
        handle,
        ..Default::default()
    };
    let (mut info, _) = server::request(env, &mut request, &[], &mut [])?;
    if info.header.error == 0xc000_0022 {
        // Suspend, resume and context rights do not imply query rights.
        // The caller checks the operation's required access separately.
        let mut duplicate = DupHandleRequest {
            src_process: CURRENT_PROCESS,
            src_handle: handle,
            dst_process: CURRENT_PROCESS,
            access: 0x800,
            ..Default::default()
        };
        let (duplicate, _) = server::request(env, &mut duplicate, &[], &mut [])?;
        status::check(duplicate.header.error)?;
        request.handle = duplicate.handle;
        info = server::request(env, &mut request, &[], &mut [])?.0;
        server::close(env, duplicate.handle)?;
    }
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

fn context_target(env: &mut Env<'_>, thread: u32, access: u32) -> Result<CtxId, Error> {
    let target = if thread == CURRENT_THREAD {
        Some(env.context)
    } else {
        let mut info = GetObjectInfoRequest {
            handle: thread,
            ..Default::default()
        };
        let (info, _) = server::request(env, &mut info, &[], &mut [])?;
        status::check(info.header.error)?;
        let target = context_of(env, thread)?;
        if info.access & access == 0 {
            return Err(0xc000_0022.into());
        }
        target
    };
    target.ok_or_else(|| Error::Unsupported("contexts of another process".into()))
}

/// An unstarted suspended thread has its Windows context at the top of
/// its stack, consumed by LdrInitializeThunk. Once materialized, its
/// registers belong to the machine's saved execution context.
fn initial_context(env: &mut Env<'_>, thread: u32, access: u32) -> Result<Option<u32>, Error> {
    let target = context_target(env, thread, access)?;
    if target == env.context {
        return Ok(None);
    }
    if env.sys.contexts[target as usize].suspend_count == 0
        || !env.sys.set_initial_registers(target, &[])
    {
        return Err(Error::Unsupported(
            "registers of a materialized machine context".into(),
        ));
    }
    Ok(Some(
        env.sys.contexts[target as usize].stack_hi - 16 - crate::context::SIZE as u32,
    ))
}

/// NtGetContextThread(thread, *context)
pub fn get_context(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[thread, output] = args else {
        unreachable!()
    };
    status::finish((|| {
        let flags = read_u32(env, output)?;
        if flags & !0x10010 == 0 {
            let target = context_target(env, thread, 8)?;
            let mut image = [0; 28];
            crate::context::load_debug(env, target, &mut image);
            if flags & 0x10 != 0 {
                write_bytes(env, output + 4, &image[4..])?;
            }
            write_u32(env, output, flags | 0x10000)?;
            return Ok(());
        }
        let mut image = if let Some(input) = initial_context(env, thread, 8)? {
            let mut image = [0; crate::context::SIZE];
            env.cpu
                .read(input, &mut image)
                .map_err(|_| status::ACCESS_VIOLATION)?;
            image
        } else {
            let mut state = env
                .cpu
                .capture_architecture()
                .map_err(|error| Error::Unsupported(format!("thread context: {error}")))?;
            state.eip = read_u32(env, state.regs[4])?;
            state.regs[4] += 4;
            crate::context::encode(&state)
        };
        let target = if thread == CURRENT_THREAD {
            env.context
        } else {
            context_of(env, thread)?.ok_or(status::INVALID_HANDLE)?
        };
        crate::context::load_debug(env, target, &mut image);
        if flags & 8 != 0 {
            let tag = u32::from_le_bytes(image[0x24..0x28].try_into().unwrap()) | 0xffff_0000;
            let status = u32::from_le_bytes(image[0x20..0x24].try_into().unwrap()) | 0xffff_0000;
            image[0x24..0x28].copy_from_slice(&tag.to_le_bytes());
            image[0x88..0x8c].copy_from_slice(&status.to_le_bytes());
        }
        // Only requested parts are written, as Wine's syscall-frame copy.
        for (part, start, end) in [
            (1, 0xb4, 0xcc),
            (2, 0x9c, 0xb4),
            (4, 0x8c, 0x9c),
            (8, 0x1c, 0x8c),
            (0x10, 4, 0x1c),
            (0x20, 0xcc, crate::context::SIZE),
        ] {
            if flags & part != 0 {
                write_bytes(env, output + start as u32, &image[start..end])?;
            }
        }
        write_u32(env, output, flags | 0x10000)?;
        Ok(())
    })())
}

/// NtSetContextThread(thread, *context)
pub fn set_context(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[thread, input] = args else {
        unreachable!()
    };
    let result = (|| {
        let mut image = [0; crate::context::SIZE];
        env.cpu
            .read(input, &mut image)
            .map_err(|_| status::ACCESS_VIOLATION)?;
        let flags = read_u32(env, input)?;
        if flags & !0x10010 == 0 {
            let target = context_target(env, thread, 0x10)?;
            crate::context::save_debug(env, target, &image);
            return Ok(None);
        }
        let initial = initial_context(env, thread, 0x10)?;
        let mut checkpoint = env
            .checkpoint()
            .map_err(|error| Error::Unsupported(format!("thread context: {error}")))?;
        let state = &mut checkpoint.state;
        if let Some(initial) = initial {
            let mut original = [0; crate::context::SIZE];
            env.cpu
                .read(initial, &mut original)
                .map_err(|_| status::ACCESS_VIOLATION)?;
            crate::context::decode(&original, state).map_err(Error::Unsupported)?;
        } else {
            state.eip = read_u32(env, state.regs[4])?;
            state.regs[4] += 4;
        }
        // The CPU decoder consumes the architecture tag; Wine accepts flags
        // without it at this interface.
        let flags = u32::from_le_bytes(image[..4].try_into().unwrap()) | 0x10000;
        image[..4].copy_from_slice(&(flags & !8).to_le_bytes());
        crate::context::decode(&image, state).map_err(Error::Unsupported)?;
        if flags & 0x20 != 0 {
            state.x87.status &= state.x87.control | 0xff80;
        } else if flags & 8 != 0 {
            let word =
                |offset: usize| u32::from_le_bytes(image[offset..offset + 4].try_into().unwrap());
            state.x87.control = word(0x1c) as u16;
            state.x87.status = word(0x20) as u16;
            state.x87.tag = word(0x24) as u16;
            state.x87.ip = word(0x28);
            state.x87.cs = word(0x2c) as u16;
            state.x87.opcode = (word(0x2c) >> 16) as u16;
            state.x87.dp = word(0x30);
            state.x87.ds = word(0x34) as u16;
            for logical in 0..8 {
                state.x87.regs[(state.x87.top() + logical) & 7] = adv_cpu::X87Register(
                    image[0x38 + logical * 10..0x42 + logical * 10]
                        .try_into()
                        .unwrap(),
                );
            }
        }
        let target = if thread == CURRENT_THREAD {
            env.context
        } else {
            context_of(env, thread)?.ok_or(status::INVALID_HANDLE)?
        };
        crate::context::save_debug(env, target, &image);
        if let Some(initial) = initial {
            write_bytes(env, initial, &crate::context::encode(state))?;
            Ok(None)
        } else {
            state.regs[0] = status::SUCCESS;
            Ok(Some(checkpoint))
        }
    })();
    match result {
        Ok(Some(checkpoint)) => Outcome::ResumeExecution(checkpoint),
        Ok(None) => Outcome::Return(status::SUCCESS),
        Err(error) => status::finish(Err(error)),
    }
}

/// Wine's NtCreateThread, NtAlertThread and terminate-port registration stubs.
pub fn not_implemented(_: &mut Env<'_>, _: &[u32]) -> Outcome {
    Outcome::Return(status::NOT_IMPLEMENTED)
}

/// The guest exposes one logical processor.
pub fn processor_number(_: &mut Env<'_>, _: &[u32]) -> Outcome {
    Outcome::Return(0)
}

/// Reflects the server's thread suspend counts after a process operation.
pub(crate) fn refresh_suspension(env: &mut Env<'_>) -> Result<(), Error> {
    let contexts: Vec<_> = env
        .sys
        .contexts
        .iter()
        .enumerate()
        .filter(|(_, context)| !context.is_done())
        .map(|(id, context)| (id as CtxId, context.teb))
        .collect();
    for (context, teb) in contexts {
        let mut open = crate::protocol::OpenThreadRequest {
            tid: read_u32(env, teb + 0x24)?,
            access: 0x800,
            ..Default::default()
        };
        let (opened, _) = server::request(env, &mut open, &[], &mut [])?;
        status::check(opened.header.error)?;
        let mut query = GetThreadInfoRequest {
            handle: opened.handle,
            ..Default::default()
        };
        let result = server::request(env, &mut query, &[], &mut []);
        server::close(env, opened.handle)?;
        let (reply, _) = result?;
        status::check(reply.header.error)?;
        let suspended = env.sys.contexts[context as usize].suspend_count != 0;
        if reply.suspend_count != 0 && !suspended {
            env.sys.suspend_context(context);
        } else if reply.suspend_count == 0 && suspended {
            env.sys.resume_context(context);
        }
    }
    Ok(())
}
