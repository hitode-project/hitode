//! Client-side Wine AFD socket I/O. The server owns socket state and queues;
//! only the NT client can read and write the buffers in guest memory.

use std::collections::HashMap;

use adv_cpu::backend::Prot;

use super::*;
use crate::object::{read_bytes, read_u32};
use crate::protocol::{RecvSocketRequest, SendSocketRequest, SetAsyncDirectResultRequest};
use crate::status::Status;

// wine/afd.h: IOCTL_AFD_WINE_*.
pub(super) const RECVMSG: u32 = 0x120334;
pub(super) const SENDMSG: u32 = 0x120338;
pub(super) const FIONREAD: u32 = 0x12034c;
const FD_TYPE_SOCKET: i32 = 3;
const ALERTED: Status = 0x101;
const DEVICE_NOT_READY: Status = 0xc000_00a3;
const BUFFER_OVERFLOW: Status = 0x8000_0005;
const INVALID_DEVICE_REQUEST: Status = 0xc000_0010;
const WS_AF_INET: u16 = 2;
const WS_AF_INET6: u16 = 23;

#[derive(Default)]
struct Pending(HashMap<u64, Transfer>);

struct Transfer {
    handle: u32,
    buffers: Vec<(u32, u32)>,
    length: u32,
    unix_flags: i32,
    operation: Operation,
}

enum Operation {
    Send {
        address: Option<(libc::sockaddr_storage, libc::socklen_t)>,
        sent: u32,
    },
    Receive {
        address: u32,
        length: u32,
        flags: u32,
    },
}

fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

// The socket error mapping differs from the one for file descriptors.
fn socket_status(error: i32) -> Status {
    match error {
        libc::EAGAIN | libc::EINPROGRESS => DEVICE_NOT_READY,
        libc::EBADF => status::INVALID_HANDLE,
        libc::ENOTSOCK => status::OBJECT_TYPE_MISMATCH,
        libc::EFAULT => status::ACCESS_VIOLATION,
        libc::EINVAL | libc::EADDRNOTAVAIL => status::INVALID_PARAMETER,
        libc::EPERM | libc::EACCES => 0xc000_0022, // STATUS_ACCESS_DENIED
        libc::EMSGSIZE => BUFFER_OVERFLOW,
        libc::EOPNOTSUPP | libc::EAFNOSUPPORT => status::NOT_SUPPORTED,
        libc::ENOBUFS => status::NO_MEMORY,
        libc::EPIPE | libc::ECONNRESET => 0xc000_020d, // STATUS_CONNECTION_RESET
        libc::ECONNREFUSED => 0xc000_0236,             // STATUS_CONNECTION_REFUSED
        libc::ECONNABORTED => 0xc000_0241,             // STATUS_CONNECTION_ABORTED
        libc::ENOTCONN | libc::EDESTADDRREQ => 0xc000_0140, // STATUS_INVALID_CONNECTION
        libc::ETIMEDOUT => 0xc000_00b5,                // STATUS_IO_TIMEOUT
        libc::ENETUNREACH => 0xc000_023c,              // STATUS_NETWORK_UNREACHABLE
        libc::EHOSTUNREACH => 0xc000_023d,             // STATUS_HOST_UNREACHABLE
        _ => 0xc000_0001,                              // STATUS_UNSUCCESSFUL
    }
}

fn socket_fd(env: &mut Env<'_>, handle: u32) -> Result<server::HandleFd, Error> {
    let fd = server::handle_fd(env, handle)?;
    if fd.kind != FD_TYPE_SOCKET {
        return Err(INVALID_DEVICE_REQUEST.into());
    }
    Ok(fd)
}

fn unix_address(bytes: &[u8]) -> Result<(libc::sockaddr_storage, libc::socklen_t), Status> {
    // SAFETY: zero initializes the sockaddr, including any platform padding.
    let mut storage: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
    let size = match bytes {
        [2, 0, p0, p1, a0, a1, a2, a3, ..] if bytes.len() >= 16 => {
            // SAFETY: sockaddr_storage holds and aligns sockaddr_in.
            let addr =
                unsafe { &mut *std::ptr::from_mut(&mut storage).cast::<libc::sockaddr_in>() };
            addr.sin_family = libc::AF_INET as _;
            addr.sin_port = u16::from_ne_bytes([*p0, *p1]);
            addr.sin_addr.s_addr = u32::from_ne_bytes([*a0, *a1, *a2, *a3]);
            #[cfg(target_vendor = "apple")]
            {
                addr.sin_len = size_of::<libc::sockaddr_in>() as u8;
            }
            size_of::<libc::sockaddr_in>()
        }
        [23, 0, p0, p1, ..] if bytes.len() >= 28 => {
            // SAFETY: sockaddr_storage holds and aligns sockaddr_in6.
            let addr =
                unsafe { &mut *std::ptr::from_mut(&mut storage).cast::<libc::sockaddr_in6>() };
            addr.sin6_family = libc::AF_INET6 as _;
            addr.sin6_port = u16::from_ne_bytes([*p0, *p1]);
            addr.sin6_flowinfo = u32::from_ne_bytes(bytes[4..8].try_into().unwrap());
            addr.sin6_addr.s6_addr.copy_from_slice(&bytes[8..24]);
            addr.sin6_scope_id = u32::from_le_bytes(bytes[24..28].try_into().unwrap());
            #[cfg(target_vendor = "apple")]
            {
                addr.sin6_len = size_of::<libc::sockaddr_in6>() as u8;
            }
            size_of::<libc::sockaddr_in6>()
        }
        _ => return Err(status::ACCESS_VIOLATION),
    };
    Ok((storage, size as libc::socklen_t))
}

fn guest_address(storage: &libc::sockaddr_storage) -> ([u8; 28], usize) {
    let mut bytes = [0; 28];
    let len = match i32::from(storage.ss_family) {
        libc::AF_INET => {
            // SAFETY: recvmsg filled an AF_INET address in sockaddr_storage.
            let addr = unsafe { &*std::ptr::from_ref(storage).cast::<libc::sockaddr_in>() };
            bytes[..2].copy_from_slice(&WS_AF_INET.to_le_bytes());
            bytes[2..4].copy_from_slice(&addr.sin_port.to_ne_bytes());
            bytes[4..8].copy_from_slice(&addr.sin_addr.s_addr.to_ne_bytes());
            16
        }
        libc::AF_INET6 => {
            // SAFETY: recvmsg filled an AF_INET6 address in sockaddr_storage.
            let addr = unsafe { &*std::ptr::from_ref(storage).cast::<libc::sockaddr_in6>() };
            bytes[..2].copy_from_slice(&WS_AF_INET6.to_le_bytes());
            bytes[2..4].copy_from_slice(&addr.sin6_port.to_ne_bytes());
            bytes[4..8].copy_from_slice(&addr.sin6_flowinfo.to_ne_bytes());
            bytes[8..24].copy_from_slice(&addr.sin6_addr.s6_addr);
            bytes[24..28].copy_from_slice(&addr.sin6_scope_id.to_le_bytes());
            28
        }
        _ => 0,
    };
    (bytes, len)
}

impl Transfer {
    fn read(env: &Env<'_>, args: &[u32], send: bool) -> Result<(Self, bool), Status> {
        let size = if send { 32 } else { 48 };
        if args[7] < size {
            return Err(status::BUFFER_TOO_SMALL);
        }
        let params = read_bytes(env, args[6], size)?;
        let word = |at| u32::from_le_bytes(params[at..at + 4].try_into().unwrap());
        let pointer = |at| {
            if word(at + 4) == 0 {
                Ok(word(at))
            } else {
                Err(status::ACCESS_VIOLATION)
            }
        };
        let (operation, winsock_flags, force_async, count, list) = if send {
            let address = pointer(0)?;
            let address = if address == 0 {
                None
            } else {
                Some(unix_address(&read_bytes(env, address, word(8).min(28))?)?)
            };
            (
                Operation::Send { address, sent: 0 },
                word(12),
                word(16),
                word(20),
                pointer(24)?,
            )
        } else {
            if pointer(0)? != 0 {
                return Err(status::NOT_SUPPORTED);
            }
            let flags = pointer(24)?;
            (
                Operation::Receive {
                    address: pointer(8)?,
                    length: pointer(16)?,
                    flags,
                },
                read_u32(env, flags)?,
                word(32),
                word(36),
                pointer(40)?,
            )
        };
        let unix_flags = (if winsock_flags & 1 != 0 {
            libc::MSG_OOB
        } else {
            0
        }) | (if !send && winsock_flags & 2 != 0 {
            libc::MSG_PEEK
        } else {
            0
        });
        let mut length = 0u32;
        let mut buffers = Vec::new();
        let list_len = count.checked_mul(8).ok_or(status::INVALID_PARAMETER)?;
        if !env.cpu.accessible(list, list_len, Prot::READ) {
            return Err(status::ACCESS_VIOLATION);
        }
        buffers
            .try_reserve_exact(count as usize)
            .map_err(|_| status::NO_MEMORY)?;
        for offset in (0..list_len).step_by(8) {
            let at = list + offset;
            let len = read_u32(env, at)?;
            buffers.push((read_u32(env, at + 4)?, len));
            length = length.checked_add(len).ok_or(status::INVALID_PARAMETER)?;
        }
        Ok((
            Self {
                handle: args[0],
                buffers,
                length,
                unix_flags,
                operation,
            },
            force_async != 0,
        ))
    }

    fn run(&mut self, env: &mut Env<'_>, fd: &server::HandleFd) -> Result<(Status, u32), Status> {
        let send = matches!(self.operation, Operation::Send { .. });
        let protection = if send { Prot::READ } else { Prot::WRITE };
        if self
            .buffers
            .iter()
            .any(|&(addr, len)| len != 0 && !env.cpu.accessible(addr, len, protection))
        {
            return Err(status::ACCESS_VIOLATION);
        }
        let mut data = Vec::new();
        data.try_reserve_exact(self.length as usize)
            .map_err(|_| status::NO_MEMORY)?;
        data.resize(self.length as usize, 0);
        match &mut self.operation {
            Operation::Send { address, sent } => {
                let mut start = 0;
                for &(addr, len) in &self.buffers {
                    let end = start + len as usize;
                    if len != 0 {
                        env.cpu
                            .read(addr, &mut data[start..end])
                            .map_err(|_| status::ACCESS_VIOLATION)?;
                    }
                    start = end;
                }
                let remaining = &data[*sent as usize..];
                let (addr, len) = address
                    .as_ref()
                    .map_or((std::ptr::null(), 0), |(addr, len)| {
                        (std::ptr::from_ref(addr).cast(), *len)
                    });
                let flags = self.unix_flags | libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL;
                loop {
                    // SAFETY: the socket and both host buffers remain live for sendto.
                    let result = unsafe {
                        libc::sendto(
                            fd.fd,
                            remaining.as_ptr().cast(),
                            remaining.len(),
                            flags,
                            addr,
                            len,
                        )
                    };
                    if result >= 0 {
                        *sent += result as u32;
                        return Ok((
                            if *sent == self.length {
                                status::SUCCESS
                            } else {
                                DEVICE_NOT_READY
                            },
                            *sent,
                        ));
                    }
                    let error = errno();
                    if error != libc::EINTR {
                        return Ok((socket_status(error), *sent));
                    }
                }
            }
            Operation::Receive {
                address,
                length,
                flags,
            } => {
                let capacity = if *address == 0 {
                    0
                } else {
                    read_u32(env, *length)?
                };
                if !env.cpu.accessible(*flags, 4, Prot::WRITE)
                    || (*address != 0
                        && (!env.cpu.accessible(*length, 4, Prot::WRITE)
                            || !env.cpu.accessible(*address, capacity.min(28), Prot::WRITE)))
                {
                    return Err(status::ACCESS_VIOLATION);
                }
                // SAFETY: zero initializes the optional fields and the address storage.
                let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
                let mut sender: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
                if *address != 0 {
                    msg.msg_name = std::ptr::from_mut(&mut sender).cast();
                    msg.msg_namelen = size_of::<libc::sockaddr_storage>() as _;
                }
                let mut iovec = libc::iovec {
                    iov_base: data.as_mut_ptr().cast(),
                    iov_len: data.len(),
                };
                msg.msg_iov = &mut iovec;
                msg.msg_iovlen = 1;
                let count = loop {
                    // SAFETY: the socket, address storage and writable data are live.
                    let result = unsafe {
                        libc::recvmsg(fd.fd, &mut msg, self.unix_flags | libc::MSG_DONTWAIT)
                    };
                    if result >= 0 {
                        break result as usize;
                    }
                    let error = errno();
                    if error != libc::EINTR {
                        let code = if error == libc::EINVAL && self.unix_flags & libc::MSG_OOB != 0
                        {
                            DEVICE_NOT_READY
                        } else {
                            socket_status(error)
                        };
                        return Ok((code, 0));
                    }
                };
                let mut offset = 0;
                for &(addr, len) in &self.buffers {
                    let end = (offset + len as usize).min(count);
                    if end > offset {
                        write_bytes(env, addr, &data[offset..end])?;
                    }
                    offset = end;
                }
                if *address != 0 && msg.msg_namelen != 0 {
                    let (bytes, size) = guest_address(&sender);
                    write_bytes(env, *address, &bytes[..size.min(capacity as usize)])?;
                    write_u32(env, *length, size as u32)?;
                }
                write_u32(env, *flags, 0)?;
                Ok((
                    if msg.msg_flags & libc::MSG_TRUNC != 0 {
                        BUFFER_OVERFLOW
                    } else {
                        status::SUCCESS
                    },
                    count as u32,
                ))
            }
        }
    }
}

pub(super) fn transfer(env: &mut Env<'_>, args: &[u32], send: bool) -> Outcome {
    let result = (|| {
        let [handle, event, apc, context, iosb] = args[..5].try_into().unwrap();
        let fd = socket_fd(env, handle)?;
        if !env.cpu.accessible(iosb, 8, Prot::WRITE) {
            return Err(status::ACCESS_VIOLATION.into());
        }
        let (mut transfer, force_async) = Transfer::read(env, args, send)?;
        let data = io::async_data(handle, event, apc, context, iosb);
        let (mut code, mut wait, options, nonblocking) = if send {
            let mut request = SendSocketRequest {
                flags: u32::from(force_async),
                r#async: data,
                ..Default::default()
            };
            let (reply, _) = server::request(env, &mut request, &[], &mut [])?;
            (
                reply.header.error,
                reply.wait,
                reply.options,
                reply.nonblocking,
            )
        } else {
            let mut request = RecvSocketRequest {
                oob: i32::from(transfer.unix_flags & libc::MSG_OOB != 0),
                force_async: i32::from(force_async),
                r#async: data,
                ..Default::default()
            };
            let (reply, _) = server::request(env, &mut request, &[], &mut [])?;
            (
                reply.header.error,
                reply.wait,
                reply.options,
                reply.nonblocking,
            )
        };
        if code == ALERTED {
            let (mut result, count) = transfer.run(env, &fd).unwrap_or_else(|code| (code, 0));
            if result == DEVICE_NOT_READY {
                if force_async || nonblocking == 0 {
                    result = status::PENDING;
                } else if count != 0 {
                    result = status::SUCCESS;
                }
            }
            if result >> 30 != 3 && result != status::PENDING {
                io::set_iosb(env, iosb, result, count)?;
            }
            let mut request = SetAsyncDirectResultRequest {
                handle: wait,
                status: result,
                information: u64::from(count),
                ..Default::default()
            };
            let (reply, _) = server::request(env, &mut request, &[], &mut [])?;
            status::check(reply.header.error)?;
            wait = reply.handle;
            code = result;
        }
        if code == status::PENDING {
            env.ext
                .get_or_default::<Pending>()
                .0
                .insert(u64::from(iosb), transfer);
        }
        Ok((code, wait, options))
    })();
    match result {
        Ok((code, 0, _)) => Outcome::Return(code),
        Ok((_, wait, options)) => {
            crate::sync::wait_single(env, &[wait, u32::from(options & 0x10 != 0), 0])
        }
        Err(error) => status::finish(Err(error)),
    }
}

pub(super) fn complete(
    env: &mut Env<'_>,
    user: u64,
    code: Status,
) -> Result<Option<(Status, u32)>, Error> {
    let Some(mut transfer) = env.ext.get_or_default::<Pending>().0.remove(&user) else {
        return Ok(None);
    };
    if code != ALERTED {
        let count = match transfer.operation {
            Operation::Send { sent, .. } => sent,
            Operation::Receive { .. } => 0,
        };
        return Ok(Some((code, count)));
    }
    let (code, count) = match socket_fd(env, transfer.handle) {
        Ok(fd) => transfer.run(env, &fd).unwrap_or_else(|code| (code, 0)),
        Err(Error::Status(code)) => (code, 0),
        Err(error) => return Err(error),
    };
    if code == DEVICE_NOT_READY {
        env.ext.get_mut::<Pending>().0.insert(user, transfer);
        return Ok(Some((status::PENDING, 0)));
    }
    Ok(Some((code, count)))
}

pub(super) fn available(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    status::finish((|| {
        let [handle, event, apc, context, iosb, _, _, _, out, len] = args.try_into().unwrap();
        if len < 4 {
            return Err(status::BUFFER_TOO_SMALL.into());
        }
        let fd = socket_fd(env, handle)?;
        let mut count = 0;
        let mut listening = 0;
        let mut size = size_of::<libc::c_int>() as libc::socklen_t;
        // SAFETY: the live socket and the two writable option fields match getsockopt.
        let listener = unsafe {
            libc::getsockopt(
                fd.fd,
                libc::SOL_SOCKET,
                libc::SO_ACCEPTCONN,
                std::ptr::from_mut(&mut listening).cast(),
                &mut size,
            ) == 0
                && listening != 0
        };
        if !listener {
            // Darwin FIONREAD includes sockaddr bytes on datagram sockets.
            #[cfg(target_vendor = "apple")]
            // SAFETY: SO_NREAD writes a c_int and its length.
            let result = unsafe {
                libc::getsockopt(
                    fd.fd,
                    libc::SOL_SOCKET,
                    libc::SO_NREAD,
                    std::ptr::from_mut(&mut count).cast(),
                    &mut size,
                )
            };
            #[cfg(not(target_vendor = "apple"))]
            // SAFETY: FIONREAD writes one integer to count on this live socket.
            let result = unsafe { libc::ioctl(fd.fd, libc::FIONREAD, &mut count) };
            if result != 0 {
                return Err(socket_status(errno()).into());
            }
        }
        write_u32(env, out, count as u32)?;
        io::set_iosb(env, iosb, status::SUCCESS, 0)?;
        if event != 0 {
            io::event_op(env, event, 1)?;
        }
        if apc != 0 {
            if let Outcome::Return(code) =
                crate::sync::queue_apc(env, &[u32::MAX - 1, apc, context, iosb, 0])
            {
                status::check(code)?;
            }
        } else if context != 0 && fd.options & FILE_SYNCHRONOUS_IO == 0 {
            io::add_completion(env, handle, context, status::SUCCESS, 0, false)?;
        }
        Ok(())
    })())
}
