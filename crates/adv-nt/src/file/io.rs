//! Descriptor I/O and server file requests, after Wine's `unix/file.c`.

use adv_cpu::backend::Prot;

use super::information::read_i64;
use super::*;
use crate::object::read_bytes;
use crate::protocol::{
    AddFdCompletionRequest, CreateNamedPipeRequest, GetFileInfoRequest, IoctlRequest,
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
            let count = if let Some(reader) = volume::mounts(env).reader(fd.fd) {
                let reader = reader.map_err(|_| 0xc000_0185u32)?;
                let at =
                    position.unwrap_or_else(|| unsafe { libc::lseek(fd.fd, 0, libc::SEEK_CUR) });
                let at_unsigned = u64::try_from(at).map_err(|_| status::INVALID_PARAMETER)?;
                let count = reader
                    .read_at(at_unsigned, &mut bytes[total..])
                    .map_err(|_| 0xc000_0185u32)?;
                if position.is_none() {
                    unsafe {
                        libc::lseek(fd.fd, at + count as i64, libc::SEEK_SET);
                    }
                }
                count as isize
            } else {
                unsafe {
                    match position {
                        Some(off) => {
                            libc::pread(fd.fd, bytes.as_mut_ptr().cast(), bytes.len(), off)
                        }
                        None => libc::read(
                            fd.fd,
                            bytes[total..].as_mut_ptr().cast(),
                            bytes.len() - total,
                        ),
                    }
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

pub(super) fn add_completion(
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

/// Signal an I/O event as NtSetEvent and NtResetEvent would; like the
/// server's own reply, an event that rejects the operation is no I/O failure.
pub(super) fn event_op(env: &mut Env<'_>, handle: u32, op: i32) -> Result<(), Error> {
    match crate::sync::signal(env, handle, op) {
        Ok(_) | Err(Error::Status(_)) => Ok(()),
        Err(error) => Err(error),
    }
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
        if reply.header.error == PENDING {
            env.ext
                .get_or_default::<AsyncReads>()
                .0
                .insert(io, (buffer, length));
        }
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

/// A control the server carries out, as Wine's `server_ioctl_file`:
/// `args` are NtFsControlFile's or NtDeviceIoControlFile's.
pub(super) fn server_ioctl(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let result = (|| {
        let &[
            handle,
            event,
            apc,
            context,
            io,
            code,
            input,
            input_size,
            out,
            out_size,
        ] = args
        else {
            unreachable!()
        };
        let mut request = IoctlRequest {
            code,
            ..Default::default()
        };
        request.r#async[..4].copy_from_slice(&handle.to_le_bytes());
        request.r#async[4..8].copy_from_slice(&event.to_le_bytes());
        request.r#async[8..16].copy_from_slice(&u64::from(io).to_le_bytes());
        request.r#async[16..24].copy_from_slice(&u64::from(io).to_le_bytes());
        request.r#async[24..32].copy_from_slice(&u64::from(apc).to_le_bytes());
        request.r#async[32..40].copy_from_slice(&u64::from(context).to_le_bytes());
        let input = read_bytes(env, input, input_size)?;
        // Only METHOD_BUFFERED leaves the output buffer out of the request.
        let output = if code & 3 != 0 {
            read_bytes(env, out, out_size)?
        } else {
            Vec::new()
        };
        let mut bytes = vec![0; out_size as usize];
        let (reply, size) = server::request(env, &mut request, &[&input, &output], &mut bytes)?;
        if reply.header.error == PENDING {
            env.ext
                .get_or_default::<AsyncReads>()
                .0
                .insert(io, (out, out_size));
        }
        write_bytes(env, out, &bytes[..size])?;
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

#[derive(Default)]
struct AsyncReads(std::collections::HashMap<u32, (u32, u32)>);

/// The system APC completing server-side I/O or a directory notification.
pub fn complete_async(env: &mut Env<'_>, call: &[u8; 64]) -> Result<[u8; 40], Error> {
    let word = |offset| u32::from_le_bytes(call[offset..offset + 4].try_into().unwrap());
    let mut status = word(4);
    let user = u64::from_le_bytes(call[8..16].try_into().unwrap());
    let iosb = word(16);
    let mut total = word(24);
    let changes = env
        .ext
        .get_or_default::<DirectoryChanges>()
        .pending
        .remove(&user);
    if let Some((handle, buffer, length)) = changes {
        total = 0;
        if status == 0x101 {
            (status, total) = read_changes(env, handle, buffer, length)?;
        }
    } else {
        let read = env
            .ext
            .get_or_default::<AsyncReads>()
            .0
            .remove(&(user as u32));
        if status == 0x101 {
            let (buffer, length) = read.unwrap_or((0, 0));
            let mut bytes = vec![0; length as usize];
            let mut request = crate::protocol::GetAsyncResultRequest {
                user_arg: user,
                ..Default::default()
            };
            let (reply, size) = server::request(env, &mut request, &[], &mut bytes)?;
            status = reply.header.error;
            write_bytes(env, buffer, &bytes[..size])?;
        }
    }
    if iosb != 0 {
        set_iosb(env, iosb, status, total)?;
    }
    let mut result = [0; 40];
    result[..4].copy_from_slice(&2u32.to_le_bytes());
    result[4..8].copy_from_slice(&status.to_le_bytes());
    result[8..12].copy_from_slice(&total.to_le_bytes());
    Ok(result)
}

/// NtCreateMailslotFile(*handle, access, *attributes, *io, options,
/// quota, max_message_size, *timeout). Wine leaves the status block alone.
pub fn create_mailslot(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    status::finish((|| {
        write_u32(env, args[0], 0)?;
        let attributes = Attributes::read(env, args[2])?;
        let mut request = crate::protocol::CreateMailslotRequest {
            access: args[1],
            options: args[4],
            max_msgsize: args[6],
            read_timeout: if args[7] == 0 {
                -1
            } else {
                read_i64(env, args[7])?
            },
            ..Default::default()
        };
        let (reply, _) = server::request(env, &mut request, &[&attributes.wire()], &mut [])?;
        status::check(reply.header.error)?;
        write_u32(env, args[0], reply.handle)?;
        Ok(())
    })())
}

fn set_iosb(env: &mut Env<'_>, io: u32, code: u32, count: u32) -> Result<(), Error> {
    write_u32(env, io + 4, count)?;
    write_u32(env, io, code)?;
    Ok(())
}

/// NtCancelIoFile(handle, *io).
pub fn cancel(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    cancel_io(env, args[0], 0, args[1], true)
}

/// NtCancelIoFileEx(handle, *request_io, *io).
pub fn cancel_ex(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    cancel_io(env, args[0], args[1], args[2], false)
}

fn cancel_io(env: &mut Env<'_>, handle: u32, io: u32, output: u32, only_thread: bool) -> Outcome {
    let mut request = crate::protocol::CancelAsyncRequest {
        handle,
        iosb: u64::from(io),
        only_thread: i32::from(only_thread),
        ..Default::default()
    };
    let reply = match server::request(env, &mut request, &[], &mut []) {
        Ok((reply, _)) => reply,
        Err(error) => return status::finish(Err(error)),
    };
    let code = reply.header.error;
    if code == status::INVALID_HANDLE {
        return Outcome::Return(code);
    }
    if code == 0 && reply.cancel_handle != 0 {
        return crate::sync::wait_then_alertable(
            env,
            reply.cancel_handle,
            true,
            Box::new(move |env, _| {
                status::finish((|| {
                    server::close(env, reply.cancel_handle)?;
                    set_iosb(env, output, code, 0)
                })())
            }),
        );
    }
    match set_iosb(env, output, code, 0) {
        Ok(()) => Outcome::Return(code),
        Err(error) => status::finish(Err(error)),
    }
}

/// NtCancelSynchronousIoFile(thread, *request_io, *io).
pub fn cancel_sync(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let result = (|| {
        let mut request = crate::protocol::CancelSyncRequest {
            handle: args[0],
            iosb: u64::from(args[1]),
            ..Default::default()
        };
        let (reply, _) = server::request(env, &mut request, &[], &mut [])?;
        set_iosb(env, args[2], reply.header.error, 0)?;
        status::check(reply.header.error)
    })();
    status::finish(result)
}

/// Wine checks descriptor access even when the host descriptor permits more.
fn fd_access(env: &mut Env<'_>, handle: u32, wanted: u32) -> Result<server::HandleFd, Error> {
    let fd = server::handle_fd(env, handle)?;
    let mut request = GetFileInfoRequest {
        handle,
        info_class: 8,
        ..Default::default()
    };
    let mut access = [0; 4];
    let (reply, _) = server::request(env, &mut request, &[], &mut access)?;
    status::check(reply.header.error)?;
    if u32::from_le_bytes(access) & wanted != wanted {
        return Err(0xc000_0022u32.into());
    }
    Ok(fd)
}

/// NtFlushBuffersFile(handle, *io).
pub fn flush(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    flush_ex(env, &[args[0], 0, 0, 0, args[1]])
}

/// NtFlushBuffersFileEx(handle, flags, params, size, *io).
/// Wine ignores flags and parameters.
pub fn flush_ex(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let handle = args[0];
    let io = args[4];
    if io == 0 || !env.cpu.accessible(io, 8, Prot::WRITE) {
        return Outcome::Return(status::ACCESS_VIOLATION);
    }
    let fd = match fd_access(env, handle, 2) {
        Err(Error::Status(0xc000_0022)) => fd_access(env, handle, 4),
        other => other,
    };
    match fd {
        Ok(fd) if matches!(fd.kind, 1 | 2 | 5) => {
            // SAFETY: the server descriptor is live for this call.
            let code = if unsafe { libc::fsync(fd.fd) } == 0 {
                0
            } else {
                write_error()
            };
            drop(fd);
            match set_iosb(env, io, code, 0) {
                Ok(()) => Outcome::Return(code),
                Err(error) => status::finish(Err(error)),
            }
        }
        Err(Error::Status(0xc000_0022)) => Outcome::Return(0xc000_0022),
        Err(Error::Unsupported(what)) => Outcome::Fail(what),
        _ => {
            let result = (|| {
                let mut request = crate::protocol::FlushRequest {
                    r#async: async_data(handle, 0, 0, 0, io),
                    ..Default::default()
                };
                let (reply, _) = server::request(env, &mut request, &[], &mut [])?;
                if reply.event != 0 && reply.header.error != PENDING {
                    set_iosb(env, io, reply.header.error, 0)?;
                }
                Ok((reply.header.error, reply.event))
            })();
            match result {
                Ok((code, 0)) => Outcome::Return(code),
                Ok((_, event)) => crate::sync::wait_single(env, &[event, 0, 0]),
                Err(error) => status::finish(Err(error)),
            }
        }
    }
}

fn async_data(handle: u32, event: u32, apc: u32, context: u32, io: u32) -> [u8; 40] {
    let mut data = [0; 40];
    data[..4].copy_from_slice(&handle.to_le_bytes());
    data[4..8].copy_from_slice(&event.to_le_bytes());
    for (offset, value) in [(8, io), (16, io), (24, apc), (32, context)] {
        data[offset..offset + 8].copy_from_slice(&u64::from(value).to_le_bytes());
    }
    data
}

/// NtDeviceIoControlFile: disk-family requests first try Wine's CD-ROM path.
pub fn device_control(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    if args[0] == u32::MAX {
        return Outcome::Return(status::INVALID_HANDLE);
    }
    if let Ok(fd) = server::handle_fd(env, args[0])
        && volume::mounts(env).info(fd.fd).is_some()
    {
        return Outcome::Return(status::NOT_SUPPORTED);
    }
    if matches!(args[5] >> 16, 2 | 4 | 7 | 0x2d | 0x33) {
        let result = (|| {
            let fd = server::handle_fd(env, args[0])?;
            // CDROM_Open fstats first. With no host disk devices, Apple's
            // get_parent_device rejects ordinary descriptors before dispatch.
            // SAFETY: a live descriptor and a writable stat buffer.
            let mut stat: libc::stat = unsafe { std::mem::zeroed() };
            if unsafe { libc::fstat(fd.fd, &mut stat) } != 0 {
                return Err(write_error().into());
            }
            let devices = &mut env.ext.get_or_default::<CdromDevices>().0;
            let identity = (stat.st_dev as u64, stat.st_ino);
            let mut empty = None;
            let mut found = false;
            for (index, value) in devices.iter().enumerate() {
                if empty.is_none() && *value == (0, 0) {
                    empty = Some(index);
                } else if *value == identity {
                    found = true;
                    break;
                }
            }
            if !found {
                let Some(empty) = empty else {
                    return Err(status::NOT_IMPLEMENTED.into());
                };
                devices[empty] = identity;
            }
            if stat.st_mode & libc::S_IFMT != libc::S_IFCHR {
                return Err(status::OBJECT_TYPE_MISMATCH.into());
            }
            Err(status::NO_SUCH_FILE.into())
        })();
        match result {
            Err(Error::Status(status::NOT_SUPPORTED | status::BAD_DEVICE_TYPE)) => {}
            other => return status::finish(other),
        }
    }
    server_ioctl(env, args)
}

#[derive(Default)]
struct CdromDevices([(u64, u64); 5]);

/// NtLockFile(file, event, apc, context, *io, *offset, *count, *key,
/// dont_wait, exclusive).
pub fn lock(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    if args[2] != 0 || args[4] != 0 || args[7] != 0 {
        return Outcome::Return(status::NOT_IMPLEMENTED);
    }
    let result = (|| {
        Ok(crate::protocol::LockFileRequest {
            handle: args[0],
            offset: read_i64(env, args[5])? as u64,
            count: read_i64(env, args[6])? as u64,
            shared: i32::from(args[9] as u8 == 0),
            wait: i32::from(args[8] as u8 == 0),
            ..Default::default()
        })
    })();
    match result {
        Ok(request) => lock_request(env, request, args[1]),
        Err(error) => status::finish(Err(error)),
    }
}

fn lock_request(
    env: &mut Env<'_>,
    mut request: crate::protocol::LockFileRequest,
    event: u32,
) -> Outcome {
    let reply = match server::request(env, &mut request, &[], &mut []) {
        Ok((reply, _)) => reply,
        Err(error) => return status::finish(Err(error)),
    };
    if reply.header.error != PENDING {
        if reply.header.error == 0 && event != 0 {
            return status::finish(event_op(env, event, 1));
        }
        return Outcome::Return(reply.header.error);
    }
    if reply.overlapped != 0 {
        if reply.handle != 0 {
            let _ = server::close(env, reply.handle);
        }
        return Outcome::Return(PENDING);
    }
    if reply.handle != 0 {
        crate::sync::wait_then(env, reply.handle, move |env, _| {
            if let Err(error) = server::close(env, reply.handle) {
                return status::finish(Err(error));
            }
            lock_request(env, request, event)
        })
    } else {
        // Wine retries a host Unix lock conflict after 100 ms. There is no
        // server wake for that conflict; the delay belongs to the machine.
        let deadline = env.sys.clock.now_us().saturating_add(100_000);
        Outcome::block(adv_machine::Wait::Until(deadline), move |env, _| {
            lock_request(env, request, event)
        })
    }
}

/// NtUnlockFile(handle, *io, *offset, *count, *key).
pub fn unlock(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    status::finish((|| {
        write_u32(env, args[1] + 4, 0)?;
        let code = if args[4] != 0 {
            status::NOT_IMPLEMENTED
        } else {
            let mut request = crate::protocol::UnlockFileRequest {
                handle: args[0],
                offset: read_i64(env, args[2])? as u64,
                count: read_i64(env, args[3])? as u64,
                ..Default::default()
            };
            server::request(env, &mut request, &[], &mut [])?
                .0
                .header
                .error
        };
        write_u32(env, args[1], code)?;
        status::check(code)
    })())
}

/// NtNotifyChangeDirectoryFile(handle, event, apc, context, *io, buffer,
/// size, filter, subtree).
pub fn notify_change(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    if args[4] == 0 {
        return Outcome::Return(status::ACCESS_VIOLATION);
    }
    if args[7] == 0 || args[7] & !0x17f != 0 {
        return Outcome::Return(status::INVALID_PARAMETER);
    }
    let result = (|| {
        // Wine allocates separate async state even when FindFirstChangeNotification
        // shares its dummy IOSB. Tokens above the guest address space stay distinct.
        let changes = env.ext.get_or_default::<DirectoryChanges>();
        let user = 0x1_0000_0000 + changes.next;
        changes.next += 1;
        let mut data = async_data(args[0], args[1], args[2], args[3], args[4]);
        data[16..24].copy_from_slice(&user.to_le_bytes());
        let mut request = crate::protocol::ReadDirectoryChangesRequest {
            filter: args[7],
            subtree: i32::from(args[8] as u8 != 0),
            want_data: i32::from(args[5] != 0),
            r#async: data,
            ..Default::default()
        };
        let (reply, _) = server::request(env, &mut request, &[], &mut [])?;
        if reply.header.error == PENDING {
            env.ext
                .get_or_default::<DirectoryChanges>()
                .pending
                .insert(user, (args[0], args[5], args[6]));
        }
        status::check(reply.header.error)
    })();
    status::finish(result)
}

#[derive(Default)]
struct DirectoryChanges {
    next: u64,
    pending: std::collections::HashMap<u64, (u32, u32, u32)>,
}

fn read_changes(
    env: &mut Env<'_>,
    handle: u32,
    buffer: u32,
    length: u32,
) -> Result<(u32, u32), Error> {
    let mut request = crate::protocol::ReadChangeRequest {
        handle,
        ..Default::default()
    };
    let mut data = vec![0; length.max(4096) as usize];
    let (reply, size) = server::request(env, &mut request, &[], &mut data)?;
    if reply.header.error != 0 || buffer == 0 {
        return Ok((0x10c, 0)); // STATUS_NOTIFY_ENUM_DIR
    }
    let mut offset = 0;
    let mut used = 0;
    let mut last = None;
    while offset < size && length - used >= 16 {
        let action = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap());
        let count = u32::from_le_bytes(data[offset + 4..offset + 8].try_into().unwrap()) as usize;
        let name =
            String::from_utf8_lossy(&data[offset + 8..offset + 8 + count]).replace('/', "\\");
        let available = (length - used - 12) / 2;
        let name: Vec<u8> = name
            .encode_utf16()
            .take(available as usize)
            .flat_map(u16::to_le_bytes)
            .collect();
        let chars = name.len() as u32 / 2;
        write_u32(env, buffer + used + 4, action)?;
        write_u32(env, buffer + used + 8, chars)?;
        write_bytes(env, buffer + used + 12, &name)?;
        if chars == available {
            break;
        }
        let entry_size = 12 + name.len() as u32;
        write_u32(env, buffer + used + 8, chars * 2)?;
        write_u32(env, buffer + used, entry_size)?;
        last = Some(buffer + used);
        used += entry_size;
        offset += (8 + count).next_multiple_of(4);
    }
    if offset != size {
        return Ok((0x10c, 0));
    }
    if let Some(last) = last {
        write_u32(env, last, 0)?;
    }
    Ok((0, used))
}

/// NtReadFileScatter and NtWriteFileGather use one guest page per segment.
pub fn read_scatter(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    segmented_io(env, args, false)
}

pub fn write_gather(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    segmented_io(env, args, true)
}

fn segmented_io(env: &mut Env<'_>, args: &[u32], writing: bool) -> Outcome {
    let &[handle, event, apc, context, io, segments, length, offset, _] = args else {
        unreachable!()
    };
    if writing && !length.is_multiple_of(4096) {
        return Outcome::Return(status::INVALID_PARAMETER);
    }
    if io == 0 {
        return Outcome::Return(status::ACCESS_VIOLATION);
    }
    let fd = match fd_access(env, handle, if writing { 2 } else { 1 }) {
        Ok(fd) => fd,
        Err(error) => return status::finish(Err(error)),
    };
    let result = (|| {
        if fd.kind != FD_TYPE_FILE || fd.options & FILE_SYNCHRONOUS_IO != 0 || fd.options & 8 == 0 {
            if event != 0 {
                event_op(env, event, 2)?;
            }
            return Ok(status::INVALID_PARAMETER);
        }
        let position = if offset == 0 {
            None
        } else {
            Some(read_i64(env, offset)?)
        }
        .filter(|off| *off != FILE_USE_FILE_POINTER_POSITION);
        let mut remaining = length;
        let mut pos = 0;
        let mut total = 0;
        let mut segment = segments;
        let mut code = 0;
        while remaining != 0 {
            let address = crate::object::read_u32(env, segment)?.wrapping_add(pos);
            let count = if writing {
                4096 - pos
            } else {
                remaining.wrapping_sub(pos).min(4096 - pos)
            };
            let mut bytes = vec![0; count as usize];
            if writing && env.cpu.read(address, &mut bytes).is_err() {
                code = 0xc000_00e8; // STATUS_INVALID_USER_BUFFER
                break;
            }
            if !writing && !env.cpu.accessible(address, count, Prot::WRITE) {
                code = status::ACCESS_VIOLATION;
                break;
            }
            // SAFETY: a live regular file and a buffer of count bytes.
            let result = unsafe {
                match (writing, position) {
                    (true, Some(off)) => libc::pwrite(
                        fd.fd,
                        bytes.as_ptr().cast(),
                        bytes.len(),
                        off + i64::from(total),
                    ),
                    (true, None) => libc::write(fd.fd, bytes.as_ptr().cast(), bytes.len()),
                    (false, Some(off)) => libc::pread(
                        fd.fd,
                        bytes.as_mut_ptr().cast(),
                        bytes.len(),
                        off + i64::from(total),
                    ),
                    (false, None) => libc::read(fd.fd, bytes.as_mut_ptr().cast(), bytes.len()),
                }
            };
            if result < 0 {
                if std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
                code = write_error();
                break;
            }
            if result == 0 {
                if writing {
                    code = 0xc000_007f;
                }
                break;
            }
            let result = result as u32;
            if !writing {
                write_bytes(env, address, &bytes[..result as usize])?;
            }
            total += result;
            remaining = remaining.wrapping_sub(result);
            pos += result;
            if pos == 4096 {
                pos = 0;
                segment += 8;
            }
        }
        if !writing && total == 0 {
            code = END_OF_FILE;
        }
        if !writing || code == 0 {
            set_iosb(env, io, code, total)?;
            if event != 0 {
                event_op(env, event, 1)?;
            }
            if apc != 0 {
                let outcome = crate::sync::queue_apc(env, &[u32::MAX - 1, apc, context, io, 0]);
                if let Outcome::Return(code) = outcome {
                    status::check(code)?;
                }
            } else if context != 0 {
                add_completion(env, handle, context, code, total, !writing)?;
            }
        } else if event != 0 {
            event_op(env, event, 2)?;
        }
        Ok(if writing { code } else { PENDING })
    })();
    match result {
        Ok(code) => Outcome::Return(code),
        Err(error) => status::finish(Err(error)),
    }
}
