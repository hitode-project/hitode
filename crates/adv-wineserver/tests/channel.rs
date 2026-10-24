//! The client and server share pipe queues while retaining real descriptors.
#![cfg(any(target_vendor = "apple", target_os = "android"))]

use std::ffi::{c_int, c_void};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;

unsafe extern "C" {
    fn hitode_pipe(fds: *mut c_int) -> c_int;
    fn hitode_open(path: *const libc::c_char, flags: c_int, ...) -> c_int;
    fn hitode_ioctl(fd: c_int, request: libc::c_ulong, ...) -> c_int;
    fn hitode_close(fd: c_int) -> c_int;
    fn hitode_read(fd: c_int, buf: *mut c_void, size: usize) -> isize;
    fn hitode_write(fd: c_int, buf: *const c_void, size: usize) -> isize;
    fn hitode_readv(fd: c_int, iov: *const libc::iovec, count: c_int) -> isize;
    fn hitode_writev(fd: c_int, iov: *const libc::iovec, count: c_int) -> isize;
    fn hitode_poll(fds: *mut libc::pollfd, count: libc::nfds_t, timeout: c_int) -> c_int;
    fn hitode_sendmsg(fd: c_int, msg: *const libc::msghdr, flags: c_int) -> isize;
    fn hitode_recvmsg(fd: c_int, msg: *mut libc::msghdr, flags: c_int) -> isize;
    fn hitode_channel_counts(queued: *mut u64, system: *mut u64);
    fn hitode_channel_watch(
        fd: c_int,
        notify: extern "C" fn(*mut c_void, usize),
        owner: *mut c_void,
        token: usize,
    );
}

struct Fd(c_int);
impl Drop for Fd {
    fn drop(&mut self) {
        // SAFETY: this value owns the descriptor.
        assert_eq!(unsafe { hitode_close(self.0) }, 0);
    }
}
fn pipe() -> (Fd, Fd) {
    let mut fds = [0; 2];
    // SAFETY: fds has space for both descriptors.
    assert_eq!(unsafe { hitode_pipe(fds.as_mut_ptr()) }, 0);
    (Fd(fds[0]), Fd(fds[1]))
}
fn read(fd: &Fd, buf: &mut [u8]) -> isize {
    // SAFETY: the descriptor and buffer are live.
    unsafe { hitode_read(fd.0, buf.as_mut_ptr().cast(), buf.len()) }
}
fn write(fd: &Fd, buf: &[u8]) -> isize {
    // SAFETY: the descriptor and buffer are live.
    unsafe { hitode_write(fd.0, buf.as_ptr().cast(), buf.len()) }
}
fn counts() -> (u64, u64) {
    let (mut queued, mut system) = (0, 0);
    // SAFETY: both outputs are writable.
    unsafe { hitode_channel_counts(&mut queued, &mut system) };
    (queued, system)
}
fn readiness(fd: &Fd, events: i16) -> i16 {
    let mut poll = libc::pollfd {
        fd: fd.0,
        events,
        revents: 0,
    };
    // SAFETY: poll is a valid single entry; timeout zero never yields.
    let ready = unsafe { hitode_poll(&mut poll, 1, 0) };
    assert_eq!(ready, c_int::from(poll.revents != 0));
    poll.revents
}
fn transfer(fd: Fd) -> Fd {
    let (sender, receiver) = UnixStream::pair().unwrap();
    let mut byte = 0u8;
    let mut iov = libc::iovec {
        iov_base: (&raw mut byte).cast(),
        iov_len: 1,
    };
    let mut control = [0usize; 8];
    // SAFETY: the buffers cover one byte and one SCM_RIGHTS descriptor.
    unsafe {
        let mut msg: libc::msghdr = std::mem::zeroed();
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = control.as_mut_ptr().cast();
        msg.msg_controllen = libc::CMSG_SPACE(size_of::<c_int>() as _) as _;
        let header = libc::CMSG_FIRSTHDR(&msg);
        (*header).cmsg_level = libc::SOL_SOCKET;
        (*header).cmsg_type = libc::SCM_RIGHTS;
        (*header).cmsg_len = libc::CMSG_LEN(size_of::<c_int>() as _) as _;
        std::ptr::write_unaligned(libc::CMSG_DATA(header).cast::<c_int>(), fd.0);
        assert_eq!(hitode_sendmsg(sender.as_raw_fd(), &msg, 0), 1);
        drop(fd);
        assert_eq!(hitode_recvmsg(receiver.as_raw_fd(), &mut msg, 0), 1);
        Fd(std::ptr::read_unaligned(
            libc::CMSG_DATA(libc::CMSG_FIRSTHDR(&msg)).cast(),
        ))
    }
}
extern "C" fn notified(owner: *mut c_void, token: usize) {
    // SAFETY: the test keeps this counter live until the read end closes.
    unsafe { *owner.cast::<usize>() += token };
}

// spec: SRV-CHAN-1, WINE-THR-1, SRV-EVENT-1
#[test]
fn registered_pipes_round_trip_wake_close_and_reuse() {
    // Link the facade supplying the shim's execution-owner services.
    let _ = adv_wineserver::PROTOCOL_VERSION;
    let (request, client_request) = pipe();
    let client_request = transfer(client_request);
    let (client_reply, reply) = pipe();
    let reply = transfer(reply);
    let (wait, wake) = pipe();
    let wake = transfer(wake);
    let mut notifications = 0usize;
    // SAFETY: notifications remains live until wait closes below.
    unsafe { hitode_channel_watch(wait.0, notified, (&raw mut notifications).cast(), 1) };
    let mut buffer = [0u8; 32];
    assert_eq!(read(&request, &mut buffer), -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::EAGAIN)
    );
    assert_eq!(readiness(&request, libc::POLLIN), 0);
    assert_eq!(readiness(&client_request, libc::POLLOUT), libc::POLLOUT);

    let before = counts();
    assert_eq!(write(&client_request, b"request"), 7);
    assert_eq!(readiness(&request, libc::POLLIN), libc::POLLIN);
    assert_eq!(read(&request, &mut buffer[..3]), 3);
    assert_eq!(&buffer[..3], b"req");
    let vectors = [
        libc::iovec {
            iov_base: buffer.as_mut_ptr().cast(),
            iov_len: 2,
        },
        libc::iovec {
            iov_base: buffer[2..].as_mut_ptr().cast(),
            iov_len: 8,
        },
    ];
    // SAFETY: vectors point at disjoint writable regions of buffer.
    assert_eq!(unsafe { hitode_readv(request.0, vectors.as_ptr(), 2) }, 4);
    assert_eq!(&buffer[..4], b"uest");
    let vectors = [
        libc::iovec {
            iov_base: b"rep".as_ptr().cast_mut().cast(),
            iov_len: 3,
        },
        libc::iovec {
            iov_base: b"ly".as_ptr().cast_mut().cast(),
            iov_len: 2,
        },
    ];
    // SAFETY: both source buffers outlive the write.
    assert_eq!(unsafe { hitode_writev(reply.0, vectors.as_ptr(), 2) }, 5);
    assert_eq!(read(&client_reply, &mut buffer), 5);
    assert_eq!(&buffer[..5], b"reply");
    assert_eq!(write(&wake, &[7; 16]), 16);
    assert_eq!(notifications, 1);
    assert_eq!(read(&wait, &mut buffer), 16);
    assert_eq!(write(&wake, &[8; 16]), 16);
    assert_eq!(notifications, 2);
    assert_eq!(read(&wait, &mut buffer), 16);
    let after = counts();
    assert_eq!(after.1 - before.1, 0, "channel read/write system calls");
    assert_eq!(after.0 - before.0, 11, "queue read/write calls");

    assert_eq!(write(&client_request, b"tail"), 4);
    drop(client_request);
    assert_eq!(
        readiness(&request, libc::POLLIN),
        libc::POLLIN | libc::POLLHUP
    );
    assert_eq!(read(&request, &mut buffer), 4);
    assert_eq!(read(&request, &mut buffer), 0);
    #[cfg(target_vendor = "apple")]
    assert_eq!(
        readiness(&request, libc::POLLIN),
        libc::POLLIN | libc::POLLHUP
    );
    #[cfg(not(target_vendor = "apple"))]
    assert_eq!(readiness(&request, libc::POLLIN), libc::POLLHUP);
    drop(client_reply);
    #[cfg(target_vendor = "apple")]
    assert_eq!(readiness(&reply, libc::POLLOUT), libc::POLLHUP);
    #[cfg(not(target_vendor = "apple"))]
    assert_eq!(
        readiness(&reply, libc::POLLOUT),
        libc::POLLERR | libc::POLLOUT
    );
    assert_eq!(write(&reply, b"closed"), -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::EPIPE)
    );
    let reused = wait.0;
    drop(wait);
    let mut ordinary = [0; 2];
    // SAFETY: ordinary has space for both descriptors; dup2 installs the read end at reused.
    unsafe {
        assert_eq!(libc::pipe(ordinary.as_mut_ptr()), 0);
        assert_eq!(libc::dup2(ordinary[0], reused), reused);
        if ordinary[0] != reused {
            libc::close(ordinary[0]);
        }
    }
    let (ordinary_read, ordinary_write) = (Fd(reused), Fd(ordinary[1]));
    assert_eq!(write(&ordinary_write, b"kernel"), 6);
    assert_eq!(readiness(&ordinary_read, libc::POLLIN), libc::POLLIN);
    assert_eq!(read(&ordinary_read, &mut buffer), 6);
    assert_eq!(&buffer[..6], b"kernel");
    assert_eq!(write(&wake, b"closed"), -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::EPIPE)
    );
    #[cfg(target_vendor = "apple")]
    channel_events_preserve_filters_enablement_data_and_close();
}

#[cfg(target_vendor = "apple")]
fn channel_events_preserve_filters_enablement_data_and_close() {
    unsafe extern "C" {
        fn hitode_kevent(
            queue: c_int,
            changes: *const libc::kevent,
            count: c_int,
            events: *mut libc::kevent,
            capacity: c_int,
            timeout: *const libc::timespec,
        ) -> c_int;
    }
    let _ = adv_wineserver::PROTOCOL_VERSION;
    // SAFETY: descriptors are owned, buffers cover their counts, and zero timeouts never yield.
    unsafe {
        let queue = Fd(libc::kqueue());
        assert!(queue.0 >= 0);
        let change = |fd: i32, filter: i16, flags: u16, token: usize| {
            let event = libc::kevent {
                ident: fd as _,
                filter,
                flags,
                fflags: 0,
                data: 0,
                udata: token as _,
            };
            assert_eq!(
                hitode_kevent(
                    queue.0,
                    &event,
                    1,
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null()
                ),
                0
            );
        };
        let ready = || {
            let mut events: [libc::kevent; 4] = std::mem::zeroed();
            let zero = libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            };
            let count = hitode_kevent(queue.0, std::ptr::null(), 0, events.as_mut_ptr(), 4, &zero);
            assert!(count >= 0);
            events[..count as usize].to_vec()
        };
        let (reader, writer) = pipe();
        change(
            reader.0,
            libc::EVFILT_READ,
            libc::EV_ADD | libc::EV_DISABLE,
            11,
        );
        change(writer.0, libc::EVFILT_WRITE, libc::EV_ADD, 22);
        let events = ready();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].filter, libc::EVFILT_WRITE);
        assert_eq!(events[0].udata as usize, 22);
        change(writer.0, libc::EVFILT_WRITE, libc::EV_DISABLE, 22);
        assert_eq!(write(&writer, b"data"), 4);
        assert!(ready().is_empty());
        change(reader.0, libc::EVFILT_READ, libc::EV_ENABLE, 11);
        let events = ready();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].filter, libc::EVFILT_READ);
        assert_eq!(events[0].udata as usize, 11);
        assert_eq!({ events[0].data }, 4);
        drop(writer);
        let events = ready();
        assert_eq!(events.len(), 1);
        assert_ne!(events[0].flags & libc::EV_EOF, 0);
        assert_eq!({ events[0].data }, 4);
        assert_eq!(read(&reader, &mut [0; 4]), 4);
        assert_eq!({ ready()[0].data }, 0);
        change(reader.0, libc::EVFILT_READ, libc::EV_DELETE, 11);
        assert!(ready().is_empty());
        change(reader.0, libc::EVFILT_READ, libc::EV_ADD, 33);
        assert_eq!(ready()[0].udata as usize, 33);
        drop(reader);
        assert!(ready().is_empty());
        let (reader, writer) = pipe();
        change(writer.0, libc::EVFILT_WRITE, libc::EV_ADD, 44);
        drop(reader);
        let events = ready();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].filter, libc::EVFILT_WRITE);
        assert_ne!(events[0].flags & libc::EV_EOF, 0);
    }
}

// spec: SRV-SYNC-3
#[test]
fn non_device_open_ioctl_and_close_reach_libc() {
    let _ = adv_wineserver::PROTOCOL_VERSION;
    // SAFETY: paths and buffers are live; every created descriptor closes here.
    unsafe {
        let fd = hitode_open(c"/dev/null".as_ptr(), libc::O_RDONLY);
        assert!(fd >= 0);
        assert_eq!(libc::read(fd, [0u8; 1].as_mut_ptr().cast(), 1), 0);
        assert_eq!(hitode_close(fd), 0);
        let mut fds = [0; 2];
        assert_eq!(libc::pipe(fds.as_mut_ptr()), 0);
        assert_eq!(libc::write(fds[1], [1u8].as_ptr().cast(), 1), 1);
        let mut available = 0i32;
        assert_eq!(hitode_ioctl(fds[0], libc::FIONREAD, &raw mut available), 0);
        assert_eq!(available, 1);
        assert_eq!(hitode_close(fds[0]), 0);
        assert_eq!(hitode_close(fds[1]), 0);
    }
}
