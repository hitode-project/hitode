//! The client side of the wineserver protocol.
//!
//! As in Wine, a process talks to the server over a Unix socket, which also
//! carries file descriptors, and each thread over a request pipe and a reply
//! pipe. The server runs in the same host process and never at the same time
//! as its clients, so the client never blocks on a descriptor: when it needs
//! the server to act, it asks the runtime for [`Progress`] and tries again.

use std::io;
use std::mem::{size_of, zeroed};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;
use std::path::Path;

use crate::protocol::{FIXED_SIZE, InitFirstThreadRequest, PROTOCOL_VERSION, Request};

/// i386, the machine of Hitode's prefixes.
pub const MACHINE_I386: u16 = 0x014c;

/// How often the client lets the server run without seeing an answer before
/// it decides that none is coming.
const PATIENCE: u32 = 4;

/// Lets the server handle what its clients have sent so far.
pub trait Progress {
    fn run_server(&mut self);
}

/// A process's connection with what drives its server, kept in the
/// machine's extensions.
pub struct Link {
    pub connection: Connection,
    pub progress: Box<dyn Progress>,
}

impl Link {
    pub fn call_raw(
        &mut self,
        fixed: &mut [u8; FIXED_SIZE],
        data: &[&[u8]],
        reply_data: &mut [u8],
    ) -> Result<usize, Error> {
        self.connection
            .call_raw(&mut *self.progress, fixed, data, reply_data)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("the server speaks protocol version {0}")]
    Version(u32),
    /// The server did not answer a request that it answers at once.
    #[error("the server did not answer")]
    NoReply,
    #[error("the server closed the connection")]
    Closed,
}

/// What the server told the first thread of a new process.
#[derive(Debug)]
pub struct FirstThread {
    pub pid: u32,
    pub tid: u32,
    pub session_id: u32,
    /// The server's start time, in 100 ns ticks since 1601.
    pub server_start: i64,
    /// The size of the startup information the process can fetch.
    pub info_size: u32,
    /// The machines the server supports, native first.
    pub machines: Vec<u16>,
}

/// One process's connection, used by its first thread.
pub struct Connection {
    socket: UnixStream,
    request: OwnedFd,
    reply: OwnedFd,
    /// The server writes here to end a wait the thread started with `select`.
    _wait: OwnedFd,
}

impl Connection {
    /// Connects a new process to the server and registers its first thread.
    pub fn connect(
        socket: &Path,
        progress: &mut dyn Progress,
    ) -> Result<(Connection, FirstThread), Error> {
        let socket = UnixStream::connect(socket)?;
        socket.set_nonblocking(true)?;
        let (version, request) = receive_fd(&socket, progress)?;
        if version != PROTOCOL_VERSION {
            return Err(Error::Version(version));
        }
        let (reply, reply_write) = pipe()?;
        let (wait, wait_write) = pipe()?;
        set_nonblocking(&request)?;
        set_nonblocking(&reply)?;
        // The first thread has no id yet; 0 names it.
        send_fd(&socket, 0, reply_write.as_raw_fd())?;
        send_fd(&socket, 0, wait_write.as_raw_fd())?;
        let mut connection = Connection {
            socket,
            request,
            reply,
            _wait: wait,
        };

        let mut init = InitFirstThreadRequest {
            unix_pid: std::process::id() as i32,
            reply_fd: reply_write.as_raw_fd(),
            wait_fd: wait_write.as_raw_fd(),
            ..Default::default()
        };
        let mut machines = [0u8; 16];
        let (answer, size) = connection.call(progress, &mut init, &[], &mut machines)?;
        // The server holds its own copies now, as in Wine's client.
        drop((reply_write, wait_write));
        if answer.header.error != 0 {
            return Err(Error::Io(io::Error::other(format!(
                "init_first_thread failed with status {:#x}",
                answer.header.error
            ))));
        }
        let machines = machines[..size]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&pair| u16::from_ne_bytes(pair))
            .collect();
        let first = FirstThread {
            pid: answer.pid,
            tid: answer.tid,
            session_id: answer.session_id,
            server_start: answer.server_start,
            info_size: answer.info_size,
            machines,
        };
        Ok((connection, first))
    }

    /// Sends a request with its variable data and returns the fixed reply and
    /// how many bytes of reply data were written to `reply_data`. The status
    /// is in the reply header.
    pub fn call<R: Request>(
        &mut self,
        progress: &mut dyn Progress,
        request: &mut R,
        data: &[&[u8]],
        reply_data: &mut [u8],
    ) -> Result<(R::Reply, usize), Error> {
        const { assert!(size_of::<R>() <= FIXED_SIZE && size_of::<R::Reply>() <= FIXED_SIZE) };
        request.header().req = R::NUMBER;
        let mut fixed = [0u8; FIXED_SIZE];
        // SAFETY: `R` is a plain repr(C) layout no larger than the buffer.
        unsafe {
            std::ptr::copy_nonoverlapping(
                (request as *const R).cast(),
                fixed.as_mut_ptr(),
                size_of::<R>(),
            )
        };
        let size = self.call_raw(progress, &mut fixed, data, reply_data)?;
        // SAFETY: the reply is a plain repr(C) layout no larger than the buffer.
        let reply: R::Reply = unsafe { std::ptr::read_unaligned(fixed.as_ptr().cast()) };
        Ok((reply, size))
    }

    /// Sends a request whose fixed part, with its number, is in `fixed`, and
    /// replaces it with the fixed reply. The sizes in the header are set
    /// here. Returns how many bytes of reply data were written.
    pub fn call_raw(
        &mut self,
        progress: &mut dyn Progress,
        fixed: &mut [u8; FIXED_SIZE],
        data: &[&[u8]],
        reply_data: &mut [u8],
    ) -> Result<usize, Error> {
        let request_size: u32 = data.iter().map(|part| part.len() as u32).sum();
        fixed[4..8].copy_from_slice(&request_size.to_ne_bytes());
        fixed[8..12].copy_from_slice(&(reply_data.len() as u32).to_ne_bytes());
        self.write(progress, fixed)?;
        for part in data {
            self.write(progress, part)?;
        }
        progress.run_server();

        self.read(progress, fixed)?;
        // SAFETY: every reply starts with the reply header.
        let header: crate::protocol::ReplyHeader =
            unsafe { std::ptr::read_unaligned(fixed.as_ptr().cast()) };
        let size = (header.reply_size as usize).min(reply_data.len());
        self.read(progress, &mut reply_data[..size])?;
        Ok(size)
    }

    /// The process socket, over which the server sends file descriptors.
    pub fn socket(&self) -> &UnixStream {
        &self.socket
    }

    fn write(&mut self, progress: &mut dyn Progress, mut bytes: &[u8]) -> Result<(), Error> {
        let mut idle = 0;
        while !bytes.is_empty() {
            // SAFETY: a valid descriptor and buffer.
            let written = unsafe {
                libc::write(self.request.as_raw_fd(), bytes.as_ptr().cast(), bytes.len())
            };
            if written > 0 {
                bytes = &bytes[written as usize..];
                idle = 0;
                continue;
            }
            let error = io::Error::last_os_error();
            match error.raw_os_error() {
                Some(libc::EAGAIN) if idle < PATIENCE => {
                    progress.run_server();
                    idle += 1;
                }
                Some(libc::EAGAIN) => return Err(Error::NoReply),
                Some(libc::EPIPE) => return Err(Error::Closed),
                Some(libc::EINTR) => {}
                _ => return Err(error.into()),
            }
        }
        Ok(())
    }

    fn read(&mut self, progress: &mut dyn Progress, mut buffer: &mut [u8]) -> Result<(), Error> {
        let mut idle = 0;
        while !buffer.is_empty() {
            // SAFETY: a valid descriptor and buffer.
            let read = unsafe {
                libc::read(
                    self.reply.as_raw_fd(),
                    buffer.as_mut_ptr().cast(),
                    buffer.len(),
                )
            };
            if read > 0 {
                buffer = &mut buffer[read as usize..];
                idle = 0;
                continue;
            }
            if read == 0 {
                return Err(Error::Closed);
            }
            let error = io::Error::last_os_error();
            match error.raw_os_error() {
                Some(libc::EAGAIN) if idle < PATIENCE => {
                    progress.run_server();
                    idle += 1;
                }
                Some(libc::EAGAIN) => return Err(Error::NoReply),
                Some(libc::EINTR) => {}
                _ => return Err(error.into()),
            }
        }
        Ok(())
    }
}

fn pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0; 2];
    // SAFETY: `fds` has room for both ends.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    for fd in fds {
        // SAFETY: a descriptor we just created.
        unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
    }
    // SAFETY: both descriptors are new and owned by nobody else.
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

fn set_nonblocking(fd: &OwnedFd) -> io::Result<()> {
    // SAFETY: a valid descriptor.
    let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
    // SAFETY: as above.
    if flags < 0
        || unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Sends `fd` to the server for thread `tid`; a later request names it by
/// its number in this process.
fn send_fd(socket: &UnixStream, tid: u32, fd: RawFd) -> io::Result<()> {
    let data = [tid, fd as u32];
    let mut iov = libc::iovec {
        iov_base: data.as_ptr() as *mut _,
        iov_len: size_of_val(&data),
    };
    let mut control = [0u8; 64];
    // SAFETY: a zeroed msghdr is valid; the buffers outlive the call, and the
    // control buffer has room for one descriptor.
    unsafe {
        let mut message: libc::msghdr = zeroed();
        message.msg_iov = &mut iov;
        message.msg_iovlen = 1;
        message.msg_control = control.as_mut_ptr().cast();
        message.msg_controllen = libc::CMSG_SPACE(size_of::<RawFd>() as u32) as _;
        let header = libc::CMSG_FIRSTHDR(&message);
        (*header).cmsg_level = libc::SOL_SOCKET;
        (*header).cmsg_type = libc::SCM_RIGHTS;
        (*header).cmsg_len = libc::CMSG_LEN(size_of::<RawFd>() as u32) as _;
        std::ptr::write_unaligned(libc::CMSG_DATA(header).cast::<RawFd>(), fd);
        if libc::sendmsg(socket.as_raw_fd(), &message, 0) < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Receives a descriptor the server sent with a handle value.
fn receive_fd(socket: &UnixStream, progress: &mut dyn Progress) -> Result<(u32, OwnedFd), Error> {
    let mut idle = 0;
    loop {
        let mut handle = 0u32;
        let mut iov = libc::iovec {
            iov_base: (&raw mut handle).cast(),
            iov_len: size_of::<u32>(),
        };
        let mut control = [0u8; 64];
        // SAFETY: as in `send_fd`.
        let mut message: libc::msghdr = unsafe { zeroed() };
        message.msg_iov = &mut iov;
        message.msg_iovlen = 1;
        message.msg_control = control.as_mut_ptr().cast();
        message.msg_controllen = control.len() as _;
        // SAFETY: as above.
        let received = unsafe { libc::recvmsg(socket.as_raw_fd(), &mut message, 0) };
        if received == size_of::<u32>() as isize {
            // SAFETY: the kernel filled in the control buffer of `message`.
            let header = unsafe { libc::CMSG_FIRSTHDR(&message) };
            // SAFETY: as above; an SCM_RIGHTS message carries a descriptor we now own.
            unsafe {
                if header.is_null() || (*header).cmsg_type != libc::SCM_RIGHTS {
                    return Err(Error::Io(io::Error::other(
                        "server message without a descriptor",
                    )));
                }
                let fd = std::ptr::read_unaligned(libc::CMSG_DATA(header).cast::<RawFd>());
                return Ok((handle, OwnedFd::from_raw_fd(fd)));
            }
        }
        if received == 0 {
            return Err(Error::Closed);
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::EAGAIN) if idle < PATIENCE => {
                progress.run_server();
                idle += 1;
            }
            Some(libc::EAGAIN) => return Err(Error::NoReply),
            Some(libc::EINTR) => {}
            _ => return Err(error.into()),
        }
    }
}
