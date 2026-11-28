//! Periodic registry saves offload disk I/O off the execution thread without
//! invalidating guest name caches, while flush and shutdown persist atomically.
#![cfg(any(target_vendor = "apple", target_os = "android"))]

use std::cell::Cell;
use std::io::IoSliceMut;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

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

fn write_request_without_step<R: Request + Copy>(
    request_pipe: &ChannelFd,
    mut request: R,
    data: &[u8],
) {
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
    }
}

fn read_reply(reply_pipe: &ChannelFd) -> [u8; protocol::FIXED_SIZE] {
    let mut fixed = [0u8; protocol::FIXED_SIZE];
    // SAFETY: reply_pipe is live and receives one fixed-size reply header.
    unsafe {
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

fn create_key(
    server: &mut Server,
    clock: &TestClock,
    request_pipe: &ChannelFd,
    reply_pipe: &ChannelFd,
    path: &str,
) -> u32 {
    let key_name = utf16_bytes(path);
    let mut objattr = Vec::with_capacity(16 + key_name.len());
    objattr.extend_from_slice(&0u32.to_le_bytes()); // rootdir
    objattr.extend_from_slice(&0x40u32.to_le_bytes()); // OBJ_CASE_INSENSITIVE
    objattr.extend_from_slice(&0u32.to_le_bytes()); // sd_len
    objattr.extend_from_slice(&(key_name.len() as u32).to_le_bytes()); // name_len
    objattr.extend_from_slice(&key_name);

    let reply = send_request(
        server,
        clock,
        request_pipe,
        reply_pipe,
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
    assert_eq!(key_reply.header.error, 0, "create_key failed for {path}");
    assert_ne!(key_reply.hkey, 0);
    key_reply.hkey
}

#[allow(clippy::too_many_arguments)]
fn set_raw_value(
    server: &mut Server,
    clock: &TestClock,
    request_pipe: &ChannelFd,
    reply_pipe: &ChannelFd,
    hkey: u32,
    name: &str,
    value_type: i32,
    value_bytes: &[u8],
) {
    let name_bytes = utf16_bytes(name);
    let mut payload = Vec::with_capacity(name_bytes.len() + value_bytes.len());
    payload.extend_from_slice(&name_bytes);
    payload.extend_from_slice(value_bytes);
    let reply = send_request(
        server,
        clock,
        request_pipe,
        reply_pipe,
        protocol::SetKeyValueRequest {
            hkey,
            r#type: value_type,
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

fn set_string_value(
    server: &mut Server,
    clock: &TestClock,
    request_pipe: &ChannelFd,
    reply_pipe: &ChannelFd,
    hkey: u32,
    name: &str,
    value: &str,
) {
    set_raw_value(
        server,
        clock,
        request_pipe,
        reply_pipe,
        hkey,
        name,
        1, // REG_SZ
        &utf16_nul_bytes(value),
    );
}

fn queue_string_value_without_step(request_pipe: &ChannelFd, hkey: u32, name: &str, value: &str) {
    let name_bytes = utf16_bytes(name);
    let value_bytes = utf16_nul_bytes(value);
    let mut payload = Vec::with_capacity(name_bytes.len() + value_bytes.len());
    payload.extend_from_slice(&name_bytes);
    payload.extend_from_slice(&value_bytes);
    write_request_without_step(
        request_pipe,
        protocol::SetKeyValueRequest {
            hkey,
            r#type: 1, // REG_SZ
            namelen: name_bytes.len() as u32,
            ..Default::default()
        },
        &payload,
    );
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

    // Create persistent keys in both system.reg (HKLM) and userdef.reg (HKU\.Default).
    let hkey = create_key(
        &mut server,
        &clock,
        &request_pipe,
        &reply_pipe,
        "\\Registry\\Machine\\Software\\HitodeAsyncRegTest",
    );
    let userdef_key = create_key(
        &mut server,
        &clock,
        &request_pipe,
        &reply_pipe,
        "\\Registry\\User\\.Default\\HitodeStaggerTest",
    );

    // Exercise formatting fidelity (DWORD, binary line wrapping, Unicode & control escapes)
    // and populate a moderately large synthetic subtree so periodic save latency is measurable.
    set_raw_value(
        &mut server,
        &clock,
        &request_pipe,
        &reply_pipe,
        hkey,
        "DwordVal",
        4, // REG_DWORD
        &0x1234_abcd_u32.to_le_bytes(),
    );
    let binary_blob: Vec<u8> = (0u8..96).collect();
    set_raw_value(
        &mut server,
        &clock,
        &request_pipe,
        &reply_pipe,
        hkey,
        "BinaryWrap",
        3, // REG_BINARY
        &binary_blob,
    );
    set_string_value(
        &mut server,
        &clock,
        &request_pipe,
        &reply_pipe,
        hkey,
        "Escaped\"Name",
        "Line1\nTab\tUni\u{3042}0\\End",
    );
    for idx in 0..400u32 {
        let sub = create_key(
            &mut server,
            &clock,
            &request_pipe,
            &reply_pipe,
            &format!("\\Registry\\Machine\\Software\\HitodeAsyncRegTest\\Sub{idx:04}"),
        );
        set_string_value(
            &mut server,
            &clock,
            &request_pipe,
            &reply_pipe,
            sub,
            "Entry",
            "SyntheticPayloadValueForFastSerializer",
        );
    }

    // 1. Periodic 30 s save offloads disk write without bumping names_revision,
    //    staggers multiple dirty branches across ticks, and completes rapidly.
    set_string_value(
        &mut server,
        &clock,
        &request_pipe,
        &reply_pipe,
        hkey,
        "Phase",
        "PeriodicValue",
    );
    set_string_value(
        &mut server,
        &clock,
        &request_pipe,
        &reply_pipe,
        userdef_key,
        "Stagger",
        "SecondBranchValue",
    );
    let rev_before = adv_wineserver::names_revision();
    // SAFETY: reads an internal atomic/mutex-guarded counter.
    let writes_before = unsafe { hitode_registry_async_writes() };
    clock.0.set(clock.0.get() + 31 * TICKS_PER_SECOND);
    let t0 = Instant::now();
    let after_first_tick = server.step(&clock);
    let first_tick_elapsed = t0.elapsed();
    assert!(
        matches!(after_first_tick, State::Waiting(Some(delay)) if delay <= Duration::from_secs(2)),
        "periodic save with multiple dirty branches should stagger remaining branch (~1s), got {after_first_tick:?}"
    );
    assert!(
        first_tick_elapsed < Duration::from_millis(16),
        "fast serializer periodic tick took too long: {first_tick_elapsed:?}"
    );
    assert_eq!(
        adv_wineserver::names_revision(),
        rev_before,
        "periodic registry save must not invalidate guest name caches"
    );
    // SAFETY: waits for the background writer to finish the first staggered write.
    unsafe { hitode_registry_wait() };
    // SAFETY: reads the completed async write counter.
    let writes_after_first = unsafe { hitode_registry_async_writes() };
    assert_eq!(
        writes_after_first,
        writes_before + 1,
        "periodic save must serialize at most one dirty branch per tick"
    );

    // Second staggered tick saves the second dirty branch (userdef.reg).
    clock.0.set(clock.0.get() + 2 * TICKS_PER_SECOND);
    assert!(matches!(server.step(&clock), State::Waiting(_)));
    // SAFETY: waits for the background writer to finish the second staggered write.
    unsafe { hitode_registry_wait() };
    // SAFETY: reads the completed async write counter.
    let writes_after_second = unsafe { hitode_registry_async_writes() };
    assert_eq!(
        writes_after_second,
        writes_after_first + 1,
        "second staggered tick must offload strictly one remaining dirty branch"
    );

    // Third staggered tick drains user.reg (dirty from fresh prefix init), and a
    // subsequent 2 s step performs no extra writes once all branches are clean.
    clock.0.set(clock.0.get() + 2 * TICKS_PER_SECOND);
    assert!(matches!(server.step(&clock), State::Waiting(_)));
    // SAFETY: waits for any remaining staggered write.
    unsafe { hitode_registry_wait() };
    // SAFETY: reads the completed async write counter.
    let writes_after_third = unsafe { hitode_registry_async_writes() };
    clock.0.set(clock.0.get() + 2 * TICKS_PER_SECOND);
    assert!(matches!(server.step(&clock), State::Waiting(_)));
    // SAFETY: waits for background writer idle.
    unsafe { hitode_registry_wait() };
    // SAFETY: reads the completed async write counter.
    assert_eq!(
        unsafe { hitode_registry_async_writes() },
        writes_after_third,
        "clean branches must return to the 30 s periodic cadence"
    );
    let system_reg = std::fs::read_to_string(prefix.join("system.reg")).unwrap();
    assert!(
        system_reg.contains("HitodeAsyncRegTest")
            && system_reg.contains("PeriodicValue")
            && system_reg.contains("\"DwordVal\"=dword:1234abcd\n")
            && system_reg.contains("\"BinaryWrap\"=hex:00,01,02,")
            && system_reg.contains("\\\n  ")
            && system_reg.contains("\"Escaped\\\"Name\"=\"Line1\\nTab\\tUni\\x30420\\\\End\"\n"),
        "periodic save did not persist expected formatted registry content"
    );
    let userdef_reg = std::fs::read_to_string(prefix.join("userdef.reg")).unwrap();
    assert!(
        userdef_reg.contains("HitodeStaggerTest") && userdef_reg.contains("SecondBranchValue"),
        "staggered second branch did not persist expected registry content"
    );
    assert_no_temp_registry_files(&prefix);

    // 2. Regression: a pending (un-stepped) registry write queued right before
    //    Server::flush() must be drained FIRST and then flushed atomically (WINE-SES-10).
    queue_string_value_without_step(&request_pipe, hkey, "Phase", "QueuedBeforeFlush");
    let rev_before_flush = adv_wineserver::names_revision();
    server.flush(&clock);
    let reply = read_reply(&reply_pipe);
    // SAFETY: reply begins with ReplyHeader.
    let answer =
        unsafe { std::ptr::read_unaligned(reply.as_ptr().cast::<protocol::SetKeyValueReply>()) };
    assert_eq!(answer.header.error, 0);
    assert_eq!(
        adv_wineserver::names_revision(),
        rev_before_flush,
        "registry flush must not invalidate guest name caches"
    );
    let system_reg = std::fs::read_to_string(prefix.join("system.reg")).unwrap();
    assert!(
        system_reg.contains("QueuedBeforeFlush"),
        "Server::flush must drain pending requests BEFORE flushing registry to disk"
    );
    assert_no_temp_registry_files(&prefix);

    // 3. Regression: writing a registry key right before Server::drop() (well before
    //    the 30 s periodic timer and without calling flush()) still persists atomically.
    clock.0.set(clock.0.get() + TICKS_PER_SECOND);
    set_string_value(
        &mut server,
        &clock,
        &request_pipe,
        &reply_pipe,
        hkey,
        "Phase",
        "DropBeforeTimerValue",
    );
    drop((request_pipe, reply_pipe, reply_write, wait_write, client));
    drop(server);
    let system_reg = std::fs::read_to_string(prefix.join("system.reg")).unwrap();
    assert!(
        system_reg.contains("DropBeforeTimerValue"),
        "Server::drop must flush remaining dirty registry keys before shutdown"
    );
    assert_no_temp_registry_files(&prefix);
    std::fs::remove_dir_all(root).unwrap();
}
