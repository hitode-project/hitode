//! A client connects to the in-process server and receives its request pipe.
#![cfg(any(target_vendor = "apple", target_os = "android"))]

use std::cell::Cell;
use std::io::IoSliceMut;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use adv_wineserver::{Clock, Config, PROTOCOL_VERSION, Server, State};

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
fn receive_request_pipe(client: &UnixStream) -> (u32, OwnedFd) {
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
    let received = unsafe { libc::recvmsg(client.as_raw_fd(), &mut message, 0) };
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
    (u32::from_ne_bytes(version), fd)
}

// spec: WINE-SRV-1
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
    let client =
        UnixStream::connect(format!("/proc/self/fd/{}/socket", directory.as_raw_fd())).unwrap();
    assert!(matches!(server.step(&clock), State::Waiting(_)));
    let (version, _request_pipe) = receive_request_pipe(&client);
    assert_eq!(version, PROTOCOL_VERSION);

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
