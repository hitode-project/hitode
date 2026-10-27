//! The readiness watcher retains level events without polling quiet sets.
#![cfg(any(target_vendor = "apple", target_os = "android"))]

use std::ffi::{c_int, c_void};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::mpsc::{Sender, channel};
use std::time::Duration;

unsafe extern "C" {
    fn hitode_event_watch(
        queue: c_int,
        notify: extern "C" fn(*mut c_void),
        owner: *mut c_void,
    ) -> *mut c_void;
    fn hitode_event_unwatch(watch: *mut c_void);
    fn hitode_server_running(running: c_int);
    fn hitode_server_ready() -> c_int;
    fn hitode_close(fd: c_int) -> c_int;
    #[cfg(target_vendor = "apple")]
    fn hitode_kevent(
        queue: c_int,
        changes: *const libc::kevent,
        count: c_int,
        events: *mut libc::kevent,
        capacity: c_int,
        timeout: *const libc::timespec,
    ) -> c_int;
    #[cfg(target_os = "android")]
    fn hitode_epoll_ctl(queue: c_int, op: c_int, fd: c_int, event: *mut libc::epoll_event)
    -> c_int;
    #[cfg(target_os = "android")]
    fn hitode_epoll_wait(
        queue: c_int,
        events: *mut libc::epoll_event,
        capacity: c_int,
        timeout: c_int,
    ) -> c_int;
}

extern "C" fn notify(owner: *mut c_void) {
    // SAFETY: the sender stays pinned until the watcher joins.
    unsafe { &*owner.cast::<Sender<()>>() }.send(()).unwrap();
}

// spec: SRV-EVENT-2
#[test]
fn readiness_wakes_and_remains_reported_until_drained() {
    let _ = adv_wineserver::PROTOCOL_VERSION;
    let (mut reader, mut writer) = UnixStream::pair().unwrap();
    let (sender, receiver) = channel::<()>();
    let sender = Box::new(sender);
    // SAFETY: each event array covers its count; all descriptors and the sender
    // remain live until the watcher stops, then the queue closes.
    unsafe {
        hitode_server_running(1);
        #[cfg(target_vendor = "apple")]
        let queue = {
            let queue = libc::kqueue();
            assert!(queue >= 0);
            let change = libc::kevent {
                ident: reader.as_raw_fd() as _,
                filter: libc::EVFILT_READ,
                flags: libc::EV_ADD,
                fflags: 0,
                data: 0,
                udata: 73usize as _,
            };
            assert_eq!(
                hitode_kevent(queue, &change, 1, std::ptr::null_mut(), 0, std::ptr::null()),
                0
            );
            queue
        };
        #[cfg(target_os = "android")]
        let queue = {
            let queue = libc::epoll_create1(libc::EPOLL_CLOEXEC);
            assert!(queue >= 0);
            let mut change = libc::epoll_event {
                events: libc::EPOLLIN as _,
                u64: 73,
            };
            assert_eq!(
                hitode_epoll_ctl(queue, libc::EPOLL_CTL_ADD, reader.as_raw_fd(), &mut change),
                0
            );
            queue
        };
        hitode_server_running(0);
        let watch = hitode_event_watch(queue, notify, (&raw const *sender).cast_mut().cast());
        assert!(!watch.is_null());
        let query = || {
            #[cfg(target_vendor = "apple")]
            {
                let mut event: libc::kevent = std::mem::zeroed();
                let zero = libc::timespec {
                    tv_sec: 0,
                    tv_nsec: 0,
                };
                let count = hitode_kevent(queue, std::ptr::null(), 0, &mut event, 1, &zero);
                if count == 1 {
                    assert_eq!(event.udata as usize, 73);
                }
                count
            }
            #[cfg(target_os = "android")]
            {
                let mut event: libc::epoll_event = std::mem::zeroed();
                let count = hitode_epoll_wait(queue, &mut event, 1, 0);
                if count == 1 {
                    assert_eq!({ event.u64 }, 73);
                }
                count
            }
        };
        assert_eq!(query(), 0);
        assert_eq!(hitode_server_ready(), 0);
        std::io::Write::write_all(&mut writer, b"x").unwrap();
        receiver.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_ne!(hitode_server_ready(), 0);
        assert_eq!(query(), 1);
        assert_ne!(hitode_server_ready(), 0);
        assert_eq!(query(), 1, "level readiness was lost after the first step");
        let mut byte = [0];
        std::io::Read::read_exact(&mut reader, &mut byte).unwrap();
        assert_eq!(query(), 0);
        hitode_event_unwatch(watch);
        assert_eq!(hitode_close(queue), 0);
    }
}
