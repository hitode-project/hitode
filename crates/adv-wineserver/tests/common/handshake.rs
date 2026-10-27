//! A client connects to the in-process server and receives its request pipe.
#![cfg(any(target_vendor = "apple", target_os = "android"))]

use std::cell::Cell;
use std::io::IoSliceMut;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use adv_wineserver::{Clock, Config, PROTOCOL_VERSION, Server, State};

#[path = "../../../adv-nt/src/protocol.rs"]
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
    fn hitode_event_counts(waits: *mut u64, events: *mut u64, poll_fds: *mut u64);
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
fn send_fd(socket: &UnixStream, fd: i32) {
    let data = [0u32, fd as u32];
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
        std::ptr::write_unaligned(libc::CMSG_DATA(header).cast::<i32>(), fd);
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

pub fn handshake(many_files: bool) {
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
    let (wait, wait_write) = pipe();
    send_fd(&client, reply_write.as_raw_fd());
    send_fd(&client, wait_write.as_raw_fd());
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
    if many_files {
        let file = std::fs::File::create(prefix.join("ordinary")).unwrap();
        for _ in 0..200 {
            send_fd(&client, file.as_raw_fd());
            let request = protocol::AllocFileHandleRequest {
                header: protocol::RequestHeader {
                    req: protocol::AllocFileHandleRequest::NUMBER,
                    ..Default::default()
                },
                access: 0x0010_0001, // SYNCHRONIZE | FILE_READ_DATA
                fd: file.as_raw_fd(),
                ..Default::default()
            };
            // SAFETY: request fits fixed; all buffers and descriptors are live.
            unsafe {
                fixed.fill(0);
                std::ptr::copy_nonoverlapping(
                    (&raw const request).cast::<u8>(),
                    fixed.as_mut_ptr(),
                    size_of_val(&request),
                );
                assert_eq!(
                    hitode_write(request_pipe.as_raw_fd(), fixed.as_ptr().cast(), fixed.len()),
                    64
                );
                assert!(matches!(server.step(&clock), State::Waiting(_)));
                assert_eq!(
                    hitode_read(reply.as_raw_fd(), fixed.as_mut_ptr().cast(), fixed.len()),
                    64
                );
                let answer = std::ptr::read_unaligned(
                    fixed.as_ptr().cast::<protocol::AllocFileHandleReply>(),
                );
                assert_eq!(answer.header.error, 0);
                assert_ne!(answer.handle, 0);
            }
        }
        // Finish the last descriptor transfer before measuring a quiet request.
        // The watcher thread can report the transfers' readiness after the
        // step that consumed it, so wait until it stays quiet.
        server.step(&clock);
        let mut quiet = 0;
        while quiet < 2 {
            std::thread::sleep(Duration::from_millis(20));
            if server.kernel_ready() {
                server.step(&clock);
                quiet = 0;
            } else {
                quiet += 1;
            }
        }
        let (mut waits, mut events, mut polls) = (0, 0, 0);
        let (mut after_waits, mut after_events, mut after_polls) = (0, 0, 0);
        // SAFETY: the request header occupies fixed; all outputs and descriptors are live.
        unsafe {
            hitode_event_counts(&mut waits, &mut events, &mut polls);
            fixed.fill(0);
            let request = protocol::GetThreadInfoRequest {
                header: protocol::RequestHeader {
                    req: protocol::GetThreadInfoRequest::NUMBER,
                    ..Default::default()
                },
                handle: 0xffff_fffe, // Current thread pseudo-handle.
                ..Default::default()
            };
            std::ptr::copy_nonoverlapping(
                (&raw const request).cast::<u8>(),
                fixed.as_mut_ptr(),
                size_of_val(&request),
            );
            assert_eq!(
                hitode_write(request_pipe.as_raw_fd(), fixed.as_ptr().cast(), fixed.len()),
                64
            );
            assert!(matches!(server.step(&clock), State::Waiting(_)));
            assert_eq!(
                hitode_read(reply.as_raw_fd(), fixed.as_mut_ptr().cast(), fixed.len()),
                64
            );
            let answer =
                std::ptr::read_unaligned(fixed.as_ptr().cast::<protocol::GetThreadInfoReply>());
            assert_eq!(answer.header.error, 0);
            assert_ne!(answer.tid, 0);
            hitode_event_counts(&mut after_waits, &mut after_events, &mut after_polls);
        }
        assert_eq!(after_waits - waits, 0, "kernel event waits per request");
        assert_eq!(
            after_events - events,
            0,
            "ordinary descriptors returned by kernel"
        );
        assert_eq!(after_polls - polls, 0, "descriptors passed to kernel poll");
    }
    if many_files {
        quiet_events(&mut server, &clock, &client, &request_pipe, &reply, &wait);
    }
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
        clock
            .0
            .set(clock.0.get() + timeout.max(Duration::from_millis(1)).as_nanos() as u64 / 100);
        state = server.step(&clock);
    }
    assert_eq!(state, State::Exited(0));
    std::fs::remove_dir_all(root).unwrap();
}

fn event_counts() -> (u64, u64) {
    let (mut waits, mut events, mut polls) = (0, 0, 0);
    // SAFETY: all three outputs are writable.
    unsafe { hitode_event_counts(&mut waits, &mut events, &mut polls) };
    (waits, events)
}

// spec: SRV-EVENT-2
fn quiet_events(
    server: &mut Server,
    clock: &TestClock,
    client: &UnixStream,
    request: &ChannelFd,
    reply: &ChannelFd,
    wait: &ChannelFd,
) {
    let call = |server: &mut Server, number: i32, words: &[u32], data: &[u8]| {
        let mut fixed = [0u8; 64];
        fixed[..4].copy_from_slice(&number.to_ne_bytes());
        fixed[4..8].copy_from_slice(&(data.len() as u32).to_ne_bytes());
        for (i, word) in words.iter().enumerate() {
            fixed[12 + i * 4..16 + i * 4].copy_from_slice(&word.to_ne_bytes());
        }
        // SAFETY: live channels and buffers; the fixed request holds its declared data size.
        unsafe {
            assert_eq!(
                hitode_write(request.as_raw_fd(), fixed.as_ptr().cast(), 64),
                64
            );
            if !data.is_empty() {
                assert_eq!(
                    hitode_write(request.as_raw_fd(), data.as_ptr().cast(), data.len()),
                    data.len() as isize
                );
            }
            server.step(clock);
            assert_eq!(
                hitode_read(reply.as_raw_fd(), fixed.as_mut_ptr().cast(), 64),
                64
            );
        }
        assert_eq!(
            u32::from_ne_bytes(fixed[..4].try_into().unwrap()),
            if number == protocol::SelectRequest::NUMBER {
                0x103
            } else {
                0
            },
            "request {}",
            protocol::REQUEST_NAMES[number as usize]
        );
        fixed
    };
    // A timeout wake uses only the memory wait channel, just like a signaled wait.
    let before = event_counts();
    let mut data = [0u8; 44]; // apc_result, then SELECT_NONE.
    data[40..].copy_from_slice(&0u32.to_ne_bytes());
    let timeout = -10_000i64; // One millisecond in monotonic ticks.
    let words = [2, 0x1234, 0, timeout as u32, (timeout >> 32) as u32, 4, 0];
    let answer = call(server, protocol::SelectRequest::NUMBER, &words, &data);
    assert_eq!(u32::from_ne_bytes(answer[12..16].try_into().unwrap()), 0);
    let generation = server.input_generation();
    clock.0.set(clock.0.get() + 10_000);
    server.step(clock);
    let mut wake = [0u8; 16];
    // SAFETY: the wait channel and destination buffer are live.
    assert_eq!(
        unsafe { hitode_read(wait.as_raw_fd(), wake.as_mut_ptr().cast(), 16) },
        16
    );
    assert_eq!(u64::from_ne_bytes(wake[..8].try_into().unwrap()), 0x1234);
    assert_eq!(u32::from_ne_bytes(wake[8..12].try_into().unwrap()), 0x102);
    assert_eq!(
        server.input_generation(),
        generation,
        "server output counted as input"
    );
    assert_eq!(
        event_counts(),
        before,
        "quiet request and wake queried the kernel"
    );

    let number = |name: &str| {
        protocol::REQUEST_NAMES
            .iter()
            .position(|&n| n == name)
            .unwrap() as i32
    };
    let handle = |fixed: [u8; 64]| u32::from_ne_bytes(fixed[8..12].try_into().unwrap());
    let name: Vec<u8> = "quiet".encode_utf16().flat_map(u16::to_ne_bytes).collect();
    let station_name: Vec<u8> = "\\quiet"
        .encode_utf16()
        .flat_map(u16::to_ne_bytes)
        .collect();
    let station = handle(call(
        server,
        number("create_winstation"),
        &[0, 0x000f_037f, 0, 0],
        &station_name,
    ));
    call(server, number("set_process_winstation"), &[station], &[]);
    let desktop = handle(call(
        server,
        number("create_desktop"),
        &[0x4000_0000, 0x000f_01ff, 0],
        &name,
    ));
    call(server, number("set_thread_desktop"), &[desktop], &[]);
    let (reader, mut writer) = UnixStream::pair().unwrap();
    send_fd(client, reader.as_raw_fd());
    let answer = call(
        server,
        protocol::AllocFileHandleRequest::NUMBER,
        &[0x0010_0001, 0, reader.as_raw_fd() as u32],
        &[],
    );
    let handle = u32::from_ne_bytes(answer[8..12].try_into().unwrap());
    call(server, protocol::SetQueueFdRequest::NUMBER, &[handle], &[]);
    server.step(clock); // Drain registration readiness before waiting for new bytes.
    assert!(!server.kernel_ready());
    let (send, receive) = std::sync::mpsc::channel();
    server.set_waker(Box::new(move || {
        send.send(()).unwrap();
    }));
    let before = event_counts();
    std::io::Write::write_all(&mut writer, b"ready").unwrap();
    receive.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(server.kernel_ready(), "watcher did not retain readiness");
    server.step(clock);
    assert!(
        event_counts().1 > before.1,
        "server did not handle socket readiness"
    );
    let handled = event_counts().1;
    server.step(clock);
    assert_eq!(
        event_counts().1,
        handled,
        "queue fd should be disabled after delivery"
    );
    server.set_waker(Box::new(|| {}));
}
