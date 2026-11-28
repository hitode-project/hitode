//! Periodic registry saves offload disk I/O off the execution thread without
//! invalidating guest name caches, while flush and shutdown persist atomically.
#![cfg(any(target_vendor = "apple", target_os = "android"))]

use std::cell::Cell;
use std::io::IoSliceMut;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
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
    fn hitode_registry_wait();
    fn hitode_registry_async_writes() -> u64;
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

const TICKS_PER_SECOND: u64 = 10_000_000;

struct TestClock(Cell<u64>);
impl Clock for TestClock {
    fn monotonic(&self) -> u64 {
        self.0.get()
    }
    fn unix(&self) -> u64 {
        17_672_256_000_000_000 + self.0.get()
    }
}

fn utf16_bytes(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

fn utf16_nul_bytes(text: &str) -> Vec<u8> {
    text.encode_utf16()
        .chain(std::iter::once(0))
        .flat_map(u16::to_le_bytes)
        .collect()
}

fn send_request<R: Request + Copy>(
    server: &mut Server,
    clock: &TestClock,
    request_pipe: &ChannelFd,
    reply_pipe: &ChannelFd,
    mut request: R,
    data: &[u8],
) -> [u8; protocol::FIXED_SIZE] {
    request.header().req = R::NUMBER;
    request.header().request_size = data.len() as u32;
    let mut fixed = [0u8; protocol::FIXED_SIZE];
    // SAFETY: request is repr(C) and fits in FIXED_SIZE; pipes and buffers are live.
    unsafe {
        std::ptr::copy_nonoverlapping(
            (&raw const request).cast::<u8>(),
            fixed.as_mut_ptr(),
            size_of::<R>(),
        );
        assert_eq!(
            hitode_write(request_pipe.as_raw_fd(), fixed.as_ptr().cast(), fixed.len()),
            fixed.len() as isize
        );
        if !data.is_empty() {
            assert_eq!(
                hitode_write(request_pipe.as_raw_fd(), data.as_ptr().cast(), data.len()),
                data.len() as isize
            );
        }
        assert!(matches!(server.step(clock), State::Waiting(_)));
        assert_eq!(
            hitode_read(
                reply_pipe.as_raw_fd(),
                fixed.as_mut_ptr().cast(),
                fixed.len()
            ),
            fixed.len() as isize
        );
    }
    fixed
}

fn set_string_value(
    server: &mut Server,
    clock: &TestClock,
    request_pipe: &ChannelFd,
    reply_pipe: &ChannelFd,
    hkey: u32,
    name: &str,
    value: &str,
) {
    let name_bytes = utf16_bytes(name);
    let value_bytes = utf16_nul_bytes(value);
    let mut payload = Vec::with_capacity(name_bytes.len() + value_bytes.len());
    payload.extend_from_slice(&name_bytes);
    payload.extend_from_slice(&value_bytes);
    let reply = send_request(
        server,
        clock,
        request_pipe,
        reply_pipe,
        protocol::SetKeyValueRequest {
            hkey,
            r#type: 1, // REG_SZ
            namelen: name_bytes.len() as u32,
            ..Default::default()
        },
        &payload,
    );
    // SAFETY: reply begins with ReplyHeader.
    let answer =
        unsafe { std::ptr::read_unaligned(reply.as_ptr().cast::<protocol::SetKeyValueReply>()) };
    assert_eq!(answer.header.error, 0, "set_key_value failed");
}

fn assert_no_temp_registry_files(prefix: &Path) {
    let temps: Vec<_> = std::fs::read_dir(prefix)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("reg") && name.ends_with(".tmp"))
        .collect();
    assert!(temps.is_empty(), "leftover registry temp files: {temps:?}");
}

// spec: WINE-SES-10, WINE-SYS-14, WINE-SYS-15
#[test]
fn periodic_save_offloads_without_cache_invalidation_and_flush_persists_atomically() {
    let root = std::env::temp_dir().join(format!("adv-wineserver-reg-{}", std::process::id()));
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

    let (reply_pipe, reply_write) = pipe();
    let (_wait_pipe, wait_write) = pipe();
    send_fd(&client, reply_write.as_raw_fd());
    send_fd(&client, wait_write.as_raw_fd());

    let reply = send_request(
        &mut server,
        &clock,
        &request_pipe,
        &reply_pipe,
        protocol::InitFirstThreadRequest {
            unix_pid: 1,
            unix_tid: 1,
            reply_fd: reply_write.as_raw_fd(),
            wait_fd: wait_write.as_raw_fd(),
            ..Default::default()
        },
        &[],
    );
    // SAFETY: reply holds InitFirstThreadReply.
    let init_reply = unsafe {
        std::ptr::read_unaligned(reply.as_ptr().cast::<protocol::InitFirstThreadReply>())
    };
    assert_eq!(init_reply.header.error, 0);

    // Create a persistent key under HKLM\Software.
    let key_name = utf16_bytes("\\Registry\\Machine\\Software\\HitodeAsyncRegTest");
    let mut objattr = Vec::with_capacity(16 + key_name.len());
    objattr.extend_from_slice(&0u32.to_le_bytes()); // rootdir
    objattr.extend_from_slice(&0x40u32.to_le_bytes()); // OBJ_CASE_INSENSITIVE
    objattr.extend_from_slice(&0u32.to_le_bytes()); // sd_len
    objattr.extend_from_slice(&(key_name.len() as u32).to_le_bytes()); // name_len
    objattr.extend_from_slice(&key_name);

    let reply = send_request(
        &mut server,
        &clock,
        &request_pipe,
        &reply_pipe,
        protocol::CreateKeyRequest {
            access: 0x000f_003f, // KEY_ALL_ACCESS
            options: 0,
            ..Default::default()
        },
        &objattr,
    );
    // SAFETY: reply holds CreateKeyReply.
    let key_reply =
        unsafe { std::ptr::read_unaligned(reply.as_ptr().cast::<protocol::CreateKeyReply>()) };
    assert_eq!(key_reply.header.error, 0, "create_key failed");
    let hkey = key_reply.hkey;
    assert_ne!(hkey, 0);

    // 1. Periodic 30 s save offloads disk write without bumping names_revision.
    set_string_value(
        &mut server,
        &clock,
        &request_pipe,
        &reply_pipe,
        hkey,
        "Phase",
        "PeriodicValue",
    );
    let rev_before = adv_wineserver::names_revision();
    // SAFETY: reads an internal atomic/mutex-guarded counter.
    let writes_before = unsafe { hitode_registry_async_writes() };
    clock.0.set(clock.0.get() + 31 * TICKS_PER_SECOND);
    assert!(matches!(server.step(&clock), State::Waiting(_)));
    assert_eq!(
        adv_wineserver::names_revision(),
        rev_before,
        "periodic registry save must not invalidate guest name caches"
    );
    // SAFETY: waits for the background writer to finish the periodic write.
    unsafe { hitode_registry_wait() };
    // SAFETY: reads the completed async write counter.
    assert!(
        unsafe { hitode_registry_async_writes() } > writes_before,
        "periodic save must offload writes to the background writer"
    );
    let system_reg = std::fs::read_to_string(prefix.join("system.reg")).unwrap();
    assert!(
        system_reg.contains("HitodeAsyncRegTest") && system_reg.contains("PeriodicValue"),
        "periodic save did not persist expected registry content"
    );
    assert_no_temp_registry_files(&prefix);

    // 2. Explicit Server::flush() waits for atomic persistence before returning (WINE-SES-10).
    set_string_value(
        &mut server,
        &clock,
        &request_pipe,
        &reply_pipe,
        hkey,
        "Phase",
        "FlushedValue",
    );
    let rev_before_flush = adv_wineserver::names_revision();
    server.flush(&clock);
    assert_eq!(
        adv_wineserver::names_revision(),
        rev_before_flush,
        "registry flush must not invalidate guest name caches"
    );
    let system_reg = std::fs::read_to_string(prefix.join("system.reg")).unwrap();
    assert!(
        system_reg.contains("FlushedValue"),
        "Server::flush must persist modified registry keys before returning"
    );
    assert_no_temp_registry_files(&prefix);

    // 3. Server exit persists final changes before returning State::Exited(0).
    set_string_value(
        &mut server,
        &clock,
        &request_pipe,
        &reply_pipe,
        hkey,
        "Phase",
        "ShutdownValue",
    );
    drop((request_pipe, reply_pipe, reply_write, wait_write, client));
    let mut state = server.step(&clock);
    let start_ticks = clock.0.get();
    while let State::Waiting(Some(timeout)) = state {
        assert!(
            clock.0.get() - start_ticks < 10 * TICKS_PER_SECOND,
            "server still running"
        );
        clock
            .0
            .set(clock.0.get() + timeout.max(Duration::from_millis(1)).as_nanos() as u64 / 100);
        state = server.step(&clock);
    }
    assert_eq!(state, State::Exited(0));
    let system_reg = std::fs::read_to_string(prefix.join("system.reg")).unwrap();
    assert!(
        system_reg.contains("ShutdownValue"),
        "server exit must persist final registry changes before returning Exited"
    );
    assert_no_temp_registry_files(&prefix);
    drop(server);
    std::fs::remove_dir_all(root).unwrap();
}
