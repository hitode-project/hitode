//! A client connects to the in-process server and receives its request pipe.
#![cfg(any(target_vendor = "apple", target_os = "android"))]

use std::cell::Cell;
use std::io::IoSliceMut;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use adv_wineserver::{Clock, Config, PROTOCOL_VERSION, Server, State};

#[path = "../../adv-nt/src/protocol.rs"]
mod protocol;
use protocol::Request;

unsafe extern "C" {
    fn hitode_pipe(fds: *mut i32) -> i32;
    fn hitode_channel_forget(fd: i32);
    fn hitode_read(fd: i32, buf: *mut libc::c_void, size: usize) -> isize;
    fn hitode_write(fd: i32, buf: *const libc::c_void, size: usize) -> isize;
    fn hitode_sendmsg(fd: i32, msg: *const libc::msghdr, flags: i32) -> isize;
    fn hitode_recvmsg(fd: i32, msg: *mut libc::msghdr, flags: i32) -> isize;
    fn hitode_channel_counts(queued: *mut u64, system: *mut u64);
}

struct ChannelFd(OwnedFd);
impl AsRawFd for ChannelFd {
    fn as_raw_fd(&self) -> i32 {
        self.0.as_raw_fd()
    }
}
impl Drop for ChannelFd {
    fn drop(&mut self) {
        // SAFETY: OwnedFd closes the descriptor after its registration is removed.
        unsafe { hitode_channel_forget(self.as_raw_fd()) };
    }
}
fn pipe() -> (ChannelFd, ChannelFd) {
    let mut fds = [0; 2];
    // SAFETY: fds has room for both ends; the returned descriptors are newly owned.
    unsafe {
        assert_eq!(hitode_pipe(fds.as_mut_ptr()), 0);
        (
            ChannelFd(OwnedFd::from_raw_fd(fds[0])),
            ChannelFd(OwnedFd::from_raw_fd(fds[1])),
        )
    }
}
fn send_fd(socket: &UnixStream, fd: &ChannelFd) {
    let data = [0u32, fd.as_raw_fd() as u32];
    let mut iov = libc::iovec {
        iov_base: data.as_ptr().cast_mut().cast(),
        iov_len: size_of_val(&data),
    };
    let mut control = [0usize; 8];
    // SAFETY: buffers outlive the call and hold one SCM_RIGHTS descriptor.
    unsafe {
        let mut message: libc::msghdr = std::mem::zeroed();
        message.msg_iov = &mut iov;
        message.msg_iovlen = 1;
        message.msg_control = control.as_mut_ptr().cast();
        message.msg_controllen = libc::CMSG_SPACE(size_of::<i32>() as _) as _;
        let header = libc::CMSG_FIRSTHDR(&message);
        (*header).cmsg_level = libc::SOL_SOCKET;
        (*header).cmsg_type = libc::SCM_RIGHTS;
        (*header).cmsg_len = libc::CMSG_LEN(size_of::<i32>() as _) as _;
        std::ptr::write_unaligned(libc::CMSG_DATA(header).cast::<i32>(), fd.as_raw_fd());
        assert_eq!(hitode_sendmsg(socket.as_raw_fd(), &message, 0), 8);
    }
}

const TICKS_PER_SECOND: u64 = 10_000_000;

/// Time stands still unless the test advances it.
struct TestClock(Cell<u64>);

impl Clock for TestClock {
    fn monotonic(&self) -> u64 {
        self.0.get()
    }
    fn unix(&self) -> u64 {
        // 2026-01-01 UTC.
        17_672_256_000_000_000 + self.0.get()
    }
}

/// Receives the server's first message: the protocol version, with the write
/// end of the request pipe attached.
fn receive_request_pipe(client: &UnixStream) -> (u32, ChannelFd) {
    let mut version = [0u8; 4];
    let mut iov = [IoSliceMut::new(&mut version)];
    let mut control = [0u8; 64];
    // SAFETY: a zeroed msghdr is valid; the buffers outlive the call.
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = iov.as_mut_ptr().cast();
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = control.len() as _;
    // SAFETY: as above.
    let received = unsafe { hitode_recvmsg(client.as_raw_fd(), &mut message, 0) };
    assert_eq!(received, 4, "{}", std::io::Error::last_os_error());
    // SAFETY: the kernel filled in the control buffer of `message`.
    let header = unsafe { &*libc::CMSG_FIRSTHDR(&message) };
    assert_eq!(
        (header.cmsg_level, header.cmsg_type),
        (libc::SOL_SOCKET, libc::SCM_RIGHTS)
    );
    // SAFETY: an SCM_RIGHTS message carries one descriptor we now own.
    let fd =
        unsafe { OwnedFd::from_raw_fd(std::ptr::read_unaligned(libc::CMSG_DATA(header).cast())) };
    (u32::from_ne_bytes(version), ChannelFd(fd))
}

// spec: WINE-SRV-1, WINE-SRV-4
#[test]
fn client_receives_request_pipe_and_server_exits_on_its_clock() {
    let root = std::env::temp_dir().join(format!("adv-wineserver-{}", std::process::id()));
    let prefix = root.join("prefix");
    let install = root.join("install");
    let nls = install.join("share/wine/nls");
    std::fs::create_dir_all(&prefix).unwrap();
    std::fs::create_dir_all(&nls).unwrap();
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    std::fs::copy(
        manifest.join("../../vendor/wine/nls/l_intl.nls"),
        nls.join("l_intl.nls"),
    )
    .unwrap();

    let clock = TestClock(Cell::new(0));
    let config = Config {
        prefix: prefix.clone(),
        server_dir: root.join("server"),
        install,
        debug: 0,
    };
    let mut server = Server::start(&config, &clock).unwrap();
    // The server refreshes the shared user data clock every 16 ms.
    assert_eq!(
        server.state(),
        State::Waiting(Some(Duration::from_millis(16)))
    );
    assert!(matches!(
        Server::start(&config, &clock),
        Err(adv_wineserver::Error::AlreadyStarted)
    ));

    #[cfg(target_vendor = "apple")]
    let directory = std::fs::File::open(server.socket_dir().unwrap()).unwrap();
    #[cfg(target_vendor = "apple")]
    let client = std::thread::spawn(move || {
        unsafe extern "C" {
            fn pthread_fchdir_np(fd: libc::c_int) -> libc::c_int;
        }
        // SAFETY: directory is live; the thread exits after connect.
        assert_eq!(unsafe { pthread_fchdir_np(directory.as_raw_fd()) }, 0);
        UnixStream::connect("socket").unwrap()
    })
    .join()
    .unwrap();
    #[cfg(not(target_vendor = "apple"))]
    let client = UnixStream::connect(server.socket_dir().unwrap().join("socket")).unwrap();
    assert!(matches!(server.step(&clock), State::Waiting(_)));
    let (version, request_pipe) = receive_request_pipe(&client);
    assert_eq!(version, PROTOCOL_VERSION);

    let (reply, reply_write) = pipe();
    let (_wait, wait_write) = pipe();
    send_fd(&client, &reply_write);
    send_fd(&client, &wait_write);
    let init = protocol::InitFirstThreadRequest {
        header: protocol::RequestHeader {
            req: protocol::InitFirstThreadRequest::NUMBER,
            ..Default::default()
        },
        unix_pid: 1,
        unix_tid: 1,
        reply_fd: reply_write.as_raw_fd(),
        wait_fd: wait_write.as_raw_fd(),
        ..Default::default()
    };
    let mut fixed = [0u8; protocol::FIXED_SIZE];
    let (mut queued_before, mut system_before, mut queued_after, mut system_after) = (0, 0, 0, 0);
    // SAFETY: generated repr(C) request fits fixed; all descriptors and buffers are live.
    unsafe {
        std::ptr::copy_nonoverlapping(
            (&raw const init).cast::<u8>(),
            fixed.as_mut_ptr(),
            size_of_val(&init),
        );
        hitode_channel_counts(&mut queued_before, &mut system_before);
        assert_eq!(
            hitode_write(request_pipe.as_raw_fd(), fixed.as_ptr().cast(), fixed.len()),
            64
        );
        assert!(matches!(server.step(&clock), State::Waiting(_)));
        assert_eq!(
            hitode_read(reply.as_raw_fd(), fixed.as_mut_ptr().cast(), fixed.len()),
            64
        );
        hitode_channel_counts(&mut queued_after, &mut system_after);
        let answer =
            std::ptr::read_unaligned(fixed.as_ptr().cast::<protocol::InitFirstThreadReply>());
        assert_eq!(answer.header.error, 0);
        assert_ne!(answer.tid, 0);
    }
    assert_eq!(queued_after - queued_before, 4);
    assert_eq!(
        system_after - system_before,
        0,
        "request round trip used read/write system calls"
    );
    drop((request_pipe, reply, reply_write, wait_write));
    drop(client);
    // With its only client gone, the server stops once its own timeouts,
    // read from the test clock, have passed.
    let mut state = server.step(&clock);
    while let State::Waiting(Some(timeout)) = state {
        assert!(
            clock.0.get() < 10 * TICKS_PER_SECOND,
            "server still running"
        );
        clock.0.set(clock.0.get() + timeout.as_nanos() as u64 / 100);
        state = server.step(&clock);
    }
    assert_eq!(state, State::Exited(0));
    std::fs::remove_dir_all(root).unwrap();
}
