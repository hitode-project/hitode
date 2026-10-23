//! File reads and pipe creation, after Wine's `unix/file.c`.

use adv_cpu::backend::Prot;

use super::information::read_i64;
use super::*;
use crate::protocol::{
    AddFdCompletionRequest, CreateNamedPipeRequest, EventOpRequest, GetFileInfoRequest,
    QueueApcRequest, ReadRequest, WriteRequest,
};

const PENDING: u32 = 0x103;
const END_OF_FILE: u32 = 0xc000_0011;

/// NtReadFile(handle, event, apc, context, *io, *buffer, length, *offset, *key).
pub fn read(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[
        handle,
        event,
        apc,
        context,
        io,
        buffer,
        length,
        offset,
        _key,
    ] = args
    else {
        unreachable!()
    };
    if io == 0 {
        return Outcome::Return(status::ACCESS_VIOLATION);
    }
    let fd = match server::handle_fd(env, handle) {
        Ok(fd) => fd,
        Err(Error::Status(status::BAD_DEVICE_TYPE)) => return server_read(env, args),
        Err(error) => return status::finish(Err(error)),
    };
    let result = (|| {
        let mut request = GetFileInfoRequest {
            handle,
            info_class: 8,
            ..Default::default()
        };
        let mut access = [0; 4];
        let (reply, _) = server::request(env, &mut request, &[], &mut access)?;
        status::check(reply.header.error)?;
        if u32::from_le_bytes(access) & 1 == 0 {
            return Err(0xc000_0022u32.into());
        }
        if !env.cpu.accessible(buffer, length, Prot::WRITE) {
            return Err(status::ACCESS_VIOLATION.into());
        }
        let asynchronous = fd.options & FILE_SYNCHRONOUS_IO == 0;
        let position = if offset != 0 {
            Some(read_i64(env, offset)?)
        } else {
            None
        };
        if matches!(fd.kind, FD_TYPE_FILE | 4 | 6)
            && asynchronous
            && position.is_none_or(|off| off < 0)
        {
            drop(fd);
            if event != 0 {
                event_op(env, event, 2)?;
            }
            if apc == 0 && context != 0 {
                add_completion(env, handle, context, status::INVALID_PARAMETER, 0, false)?;
            }
            return Err(status::INVALID_PARAMETER.into());
        }
        let mut bytes = vec![0; length as usize];
        let position = if fd.kind == FD_TYPE_FILE {
            position.filter(|off| *off != FILE_USE_FILE_POINTER_POSITION)
        } else {
            None
        };
        let mut total = 0;
        let count = loop {
            // SAFETY: a live server descriptor and a writable buffer of length bytes.
            let count = unsafe {
                match position {
                    Some(off) => libc::pread(fd.fd, bytes.as_mut_ptr().cast(), bytes.len(), off),
                    None => libc::read(
                        fd.fd,
                        bytes[total..].as_mut_ptr().cast(),
                        bytes.len() - total,
                    ),
                }
            };
            if count >= 0 {
                total += count as usize;
                if position.is_none()
                    && fd.kind == FD_TYPE_FILE
                    && count != 0
                    && total < bytes.len()
                {
                    continue;
                }
                break total as u32;
            }
            if std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            if total != 0 {
                break total as u32;
            }
            let status = write_error();
            drop(fd);
            if event != 0 {
                event_op(env, event, 2)?;
            }
            if position.is_some() && asynchronous && apc == 0 && context != 0 {
                add_completion(env, handle, context, status, 0, false)?;
            }
            return Err(status.into());
        };
        if let Some(off) = position
            && !asynchronous
        {
            // SAFETY: the same live regular file; Wine advances its synchronous position.
            unsafe {
                libc::lseek(fd.fd, off + i64::from(count), libc::SEEK_SET);
            }
        }
        let kind = fd.kind;
        drop(fd);
        let status = if count != 0 || length == 0 {
            status::SUCCESS
        } else if matches!(kind, FD_TYPE_FILE | 5 | 6) {
            END_OF_FILE
        } else {
            return Err(0xc000_014bu32.into());
        }; // STATUS_PIPE_BROKEN
        write_bytes(env, buffer, &bytes[..count as usize])?;
        write_u32(env, io, status)?;
        write_u32(env, io + 4, count)?;
        if event != 0 {
            event_op(env, event, 1)?;
        }
        if apc != 0 && (status == status::SUCCESS || asynchronous) {
            let mut request = QueueApcRequest {
                handle: u32::MAX - 1,
                ..Default::default()
            };
            let mut call = [0u8; 64];
            call[0..4].copy_from_slice(&1u32.to_le_bytes());
            call[8..16].copy_from_slice(&u64::from(apc).to_le_bytes());
            call[16..24].copy_from_slice(&u64::from(context).to_le_bytes());
            call[24..32].copy_from_slice(&u64::from(io).to_le_bytes());
            server::request(env, &mut request, &[&call], &mut [])?;
        }
        let ret = if asynchronous && kind == FD_TYPE_FILE {
            PENDING
        } else {
            status
        };
        if apc == 0 && context != 0 && asynchronous {
            add_completion(env, handle, context, status, count, ret == PENDING)?;
        }
        Ok(ret)
    })();
    match result {
        Ok(status) => Outcome::Return(status),
        Err(error) => status::finish(Err(error)),
    }
}

fn add_completion(
    env: &mut Env<'_>,
    handle: u32,
    value: u32,
    status: u32,
    count: u32,
    asynchronous: bool,
) -> Result<(), Error> {
    let mut request = AddFdCompletionRequest {
        handle,
        cvalue: u64::from(value),
        status,
        information: u64::from(count),
        r#async: i32::from(asynchronous),
        ..Default::default()
    };
    server::request(env, &mut request, &[], &mut [])?;
    Ok(())
}

fn event_op(env: &mut Env<'_>, handle: u32, op: i32) -> Result<(), Error> {
    let mut request = EventOpRequest {
        handle,
        op,
        ..Default::default()
    };
    server::request(env, &mut request, &[], &mut [])?;
    Ok(())
}

fn server_read(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let result = (|| {
        let &[handle, event, apc, context, io, buffer, length, offset, _] = args else {
            unreachable!()
        };
        if !env.cpu.accessible(buffer, length, Prot::WRITE) {
            return Err(status::ACCESS_VIOLATION.into());
        }
        let mut request = ReadRequest {
            pos: if offset == 0 {
                0
            } else {
                read_i64(env, offset)? as u64
            },
            ..Default::default()
        };
        request.r#async[..4].copy_from_slice(&handle.to_le_bytes());
        request.r#async[4..8].copy_from_slice(&event.to_le_bytes());
        request.r#async[8..16].copy_from_slice(&u64::from(io).to_le_bytes());
        request.r#async[16..24].copy_from_slice(&u64::from(io).to_le_bytes());
        request.r#async[24..32].copy_from_slice(&u64::from(apc).to_le_bytes());
        request.r#async[32..40].copy_from_slice(&u64::from(context).to_le_bytes());
        let mut bytes = vec![0; length as usize];
        let (reply, size) = server::request(env, &mut request, &[], &mut bytes)?;
        write_bytes(env, buffer, &bytes[..size])?;
        if reply.wait != 0 && reply.header.error != PENDING {
            write_u32(env, io, reply.header.error)?;
            write_u32(env, io + 4, size as u32)?;
        }
        Ok((reply.header.error, reply.wait, reply.options))
    })();
    match result {
        Ok((status, 0, _)) => Outcome::Return(status),
        Ok((_, wait, options)) => {
            crate::sync::wait_single(env, &[wait, u32::from(options & 0x10 != 0), 0])
        }
        Err(error) => status::finish(Err(error)),
    }
}

pub(super) fn server_write(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let result = (|| {
        let &[handle, event, apc, context, io, buffer, length, offset, _] = args else {
            unreachable!()
        };
        if io == 0 {
            return Err(status::ACCESS_VIOLATION.into());
        }
        let mut request = WriteRequest {
            pos: if offset == 0 {
                0
            } else {
                read_i64(env, offset)? as u64
            },
            ..Default::default()
        };
        request.r#async[..4].copy_from_slice(&handle.to_le_bytes());
        request.r#async[4..8].copy_from_slice(&event.to_le_bytes());
        request.r#async[8..16].copy_from_slice(&u64::from(io).to_le_bytes());
        request.r#async[16..24].copy_from_slice(&u64::from(io).to_le_bytes());
        request.r#async[24..32].copy_from_slice(&u64::from(apc).to_le_bytes());
        request.r#async[32..40].copy_from_slice(&u64::from(context).to_le_bytes());
        let mut bytes = vec![0; length as usize];
        env.cpu
            .read(buffer, &mut bytes)
            .map_err(|_| status::ACCESS_VIOLATION)?;
        let (reply, _) = server::request(env, &mut request, &[&bytes], &mut [])?;
        if reply.wait != 0 && reply.header.error != PENDING {
            write_u32(env, io, reply.header.error)?;
            write_u32(env, io + 4, reply.size)?;
        }
        Ok((reply.header.error, reply.wait, reply.options))
    })();
    match result {
        Ok((status, 0, _)) => Outcome::Return(status),
        Ok((_, wait, options)) => {
            crate::sync::wait_single(env, &[wait, u32::from(options & 0x10 != 0), 0])
        }
        Err(error) => status::finish(Err(error)),
    }
}

/// NtCreateNamedPipeFile(*handle, access, *attributes, *io, sharing,
/// disposition, options, type, read_mode, completion_mode, max_instances,
/// inbound_quota, outbound_quota, *timeout).
pub fn create_named_pipe(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[
        handle,
        access,
        attributes,
        io,
        sharing,
        disposition,
        options,
        kind,
        read_mode,
        completion_mode,
        maxinstances,
        insize,
        outsize,
        timeout,
    ] = args
    else {
        unreachable!()
    };
    status::finish((|| {
        write_u32(env, handle, 0)?;
        let attributes = Attributes::read(env, attributes)?;
        let mut request = CreateNamedPipeRequest {
            access,
            options,
            sharing,
            disposition,
            maxinstances,
            insize,
            outsize,
            timeout: if timeout == 0 {
                0
            } else {
                read_i64(env, timeout)?
            },
            flags: u32::from(kind != 0)
                | (u32::from(read_mode != 0) << 1)
                | (u32::from(completion_mode != 0) << 2),
            ..Default::default()
        };
        let (reply, _) = server::request(env, &mut request, &[&attributes.wire()], &mut [])?;
        write_u32(env, io, reply.header.error)?;
        status::check(reply.header.error)?;
        write_u32(env, handle, reply.handle)?;
        write_u32(
            env,
            io + 4,
            if reply.created != 0 {
                FILE_CREATED
            } else {
                FILE_OPENED
            },
        )?;
        Ok(())
    })())
}
