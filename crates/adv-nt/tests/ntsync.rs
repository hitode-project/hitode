#![cfg(any(target_vendor = "apple", target_os = "android"))]
use adv_nt::ntsync::*;
use adv_wineserver as _;
use std::os::fd::{AsRawFd, FromRawFd};

fn device() -> Fd {
    let fd = hitode_ntsync_open();
    assert!(fd >= 0);
    // SAFETY: open returns a new owned descriptor.
    unsafe { Fd::from_raw_fd(fd) }
}
fn call(fd: &Fd, command: u32, value: [u32; 2]) -> Result<[u32; 2], i32> {
    let mut value = value;
    // SAFETY: all object commands use at most two u32 fields.
    unsafe { ioctl(fd.as_raw_fd(), command, value.as_mut_ptr()) }?;
    Ok(value)
}
fn create(device: &Fd, command: u32, mut value: [u32; 2]) -> Fd {
    // SAFETY: creation takes the two supplied u32 fields.
    let fd = unsafe { ioctl(device.as_raw_fd(), command, value.as_mut_ptr()) }.unwrap();
    // SAFETY: creation returns a new owned descriptor.
    unsafe { Fd::from_raw_fd(fd) }
}
fn wait(device: &Fd, objects: &[&Fd], owner: u32, alert: Option<&Fd>, all: bool) -> Pending {
    let objects: Vec<_> = objects.iter().map(|o| o.as_raw_fd()).collect();
    let args = WaitArgs {
        timeout: u64::MAX,
        objs: objects.as_ptr() as u64,
        count: objects.len() as u32,
        owner,
        alert: alert.map_or(0, |a| a.as_raw_fd() as u32),
        ..Default::default()
    };
    // SAFETY: objects is readable until registration completes.
    unsafe {
        wait_ioctl(
            device.as_raw_fd(),
            if all { WAIT_ALL } else { WAIT_ANY },
            &args,
        )
    }
    .unwrap()
}

// spec: SRV-SYNC-1, SRV-SYNC-2
#[test]
fn ioctls_acquire_atomically_and_report_errno() {
    let dev = device();
    assert_eq!(call(&dev, CREATE_SEM, [2, 1]), Err(libc::EINVAL));
    assert_eq!(call(&dev, CREATE_MUTEX, [1, 0]), Err(libc::EINVAL));
    assert_eq!(call(&dev, CREATE_MUTEX, [0, 1]), Err(libc::EINVAL));
    let sem = create(&dev, CREATE_SEM, [1, 2]);
    let event = create(&dev, CREATE_EVENT, [0, 0]);
    let mutex = create(&dev, CREATE_MUTEX, [7, 2]);
    assert_eq!(call(&sem, SEM_READ, [0; 2]), Ok([1, 2]));
    assert_eq!(call(&sem, SEM_RELEASE, [2, 0]), Err(libc::EOVERFLOW));
    assert_eq!(call(&sem, EVENT_READ, [0; 2]), Err(libc::ENOTTY));
    let all = wait(&dev, &[&sem, &event], 7, None, true);
    assert_eq!(all.result(), None);
    assert_eq!(call(&sem, SEM_READ, [0; 2]), Ok([1, 2]));
    assert_eq!(call(&event, EVENT_SET, [0; 2]), Ok([0, 0]));
    assert_eq!(all.result(), Some(Ok((0, false))));
    assert_eq!(call(&sem, SEM_READ, [0; 2]), Ok([0, 2]));
    assert_eq!(call(&event, EVENT_READ, [0; 2]), Ok([0, 0]));
    assert_eq!(
        wait(&dev, &[&mutex], 7, None, false).result(),
        Some(Ok((0, false)))
    );
    assert_eq!(call(&mutex, MUTEX_READ, [0; 2]), Ok([7, 3]));
    assert_eq!(call(&mutex, MUTEX_UNLOCK, [0, 0]), Err(libc::EINVAL));
    assert_eq!(call(&mutex, MUTEX_UNLOCK, [8, 0]), Err(libc::EPERM));
    assert_eq!(call(&mutex, MUTEX_UNLOCK, [7, 0]), Ok([7, 3]));
    let other = wait(&dev, &[&mutex], 8, None, false);
    assert_eq!(call(&mutex, MUTEX_KILL, [0, 0]), Err(libc::EINVAL));
    assert_eq!(call(&mutex, MUTEX_KILL, [8, 0]), Err(libc::EPERM));
    call(&mutex, MUTEX_KILL, [7, 0]).unwrap();
    assert_eq!(other.result(), Some(Ok((0, true))));
    assert_eq!(call(&mutex, MUTEX_READ, [0; 2]), Ok([8, 1]));
    call(&mutex, MUTEX_KILL, [8, 0]).unwrap();
    assert_eq!(call(&mutex, MUTEX_READ, [0; 2]), Err(libc::EOWNERDEAD));
    assert_eq!(call(&sem, SEM_RELEASE, [1, 0]), Ok([0, 0]));
    assert_eq!(
        wait(&dev, &[&event, &sem], 7, None, false).result(),
        Some(Ok((1, false)))
    );
    let alert = create(&dev, CREATE_EVENT, [1, 1]);
    assert_eq!(
        wait(&dev, &[&event], 7, Some(&alert), true).result(),
        Some(Ok((1, false)))
    );
    let objects = [event.as_raw_fd(), event.as_raw_fd()];
    let mut args = WaitArgs {
        timeout: 0,
        objs: objects.as_ptr() as u64,
        count: 2,
        owner: 7,
        ..Default::default()
    };
    // SAFETY: args points to two valid descriptors.
    unsafe {
        assert!(matches!(
            wait_ioctl(dev.as_raw_fd(), WAIT_ALL, &args),
            Err(libc::EINVAL)
        ));
        args.flags = 2;
        assert!(matches!(
            wait_ioctl(dev.as_raw_fd(), WAIT_ANY, &args),
            Err(libc::EINVAL)
        ));
        args.flags = 0;
        args.pad = 1;
        assert!(matches!(
            wait_ioctl(dev.as_raw_fd(), WAIT_ANY, &args),
            Err(libc::EINVAL)
        ));
        args.pad = 0;
        assert_eq!(
            wait_ioctl(dev.as_raw_fd(), WAIT_ANY, &args)
                .unwrap()
                .result(),
            Some(Err(libc::ETIMEDOUT))
        );
        assert_eq!(
            ioctl(sem.as_raw_fd(), SEM_READ, std::ptr::null_mut()),
            Err(libc::EFAULT)
        );
    }
    let foreign = device();
    let foreign_event = create(&foreign, CREATE_EVENT, [0, 1]);
    let foreign_fd = foreign_event.as_raw_fd();
    args.objs = &foreign_fd as *const i32 as u64;
    args.count = 1;
    // SAFETY: the descriptor value is stored until the end of this statement.
    assert!(matches!(
        unsafe { wait_ioctl(dev.as_raw_fd(), WAIT_ANY, &args) },
        Err(libc::EINVAL)
    ));
}

// spec: SRV-SYNC-1, SRV-SYNC-2
#[test]
fn events_wake_one_or_all_and_pulse_resets() {
    let dev = device();
    for manual in [0, 1] {
        let event = create(&dev, CREATE_EVENT, [manual, 0]);
        let a = wait(&dev, &[&event], 1, None, false);
        let b = wait(&dev, &[&event], 2, None, false);
        call(&event, EVENT_PULSE, [0; 2]).unwrap();
        assert_eq!(a.result(), Some(Ok((0, false))));
        assert_eq!(b.result().is_some(), manual != 0);
        assert_eq!(call(&event, EVENT_READ, [0; 2]), Ok([manual, 0]));
        call(&event, EVENT_SET, [0; 2]).unwrap();
        assert!(b.result().is_some());
        assert_eq!(call(&event, EVENT_RESET, [0; 2]).unwrap()[0], manual);
    }
}

// spec: SRV-SYNC-3
#[test]
fn descriptors_survive_rights_and_close() {
    use std::os::unix::net::UnixStream;
    unsafe extern "C" {
        fn hitode_sendmsg(fd: i32, msg: *const libc::msghdr, flags: i32) -> isize;
        fn hitode_recvmsg(fd: i32, msg: *mut libc::msghdr, flags: i32) -> isize;
    }
    let dev = device();
    let event = create(&dev, CREATE_EVENT, [1, 1]);
    let old = event.as_raw_fd();
    let (a, b) = UnixStream::pair().unwrap();
    // SAFETY: message, control storage and payload remain live across both calls.
    unsafe {
        let mut payload = 0u32;
        let mut iov = libc::iovec {
            iov_base: (&raw mut payload).cast(),
            iov_len: 4,
        };
        let mut control = [0usize; 8];
        let mut msg: libc::msghdr = std::mem::zeroed();
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = control.as_mut_ptr().cast();
        msg.msg_controllen = libc::CMSG_SPACE(4) as _;
        let header = libc::CMSG_FIRSTHDR(&msg);
        (*header).cmsg_level = libc::SOL_SOCKET;
        (*header).cmsg_type = libc::SCM_RIGHTS;
        (*header).cmsg_len = libc::CMSG_LEN(4) as _;
        std::ptr::write_unaligned(libc::CMSG_DATA(header).cast::<i32>(), old);
        assert_eq!(hitode_sendmsg(a.as_raw_fd(), &msg, 0), 4);
        drop(event);
        assert_eq!(hitode_ntsync_known(old), 0);
        assert_eq!(hitode_recvmsg(b.as_raw_fd(), &mut msg, 0), 4);
        let copy =
            std::ptr::read_unaligned(libc::CMSG_DATA(libc::CMSG_FIRSTHDR(&msg)).cast::<i32>());
        let copy = Fd::from_raw_fd(copy);
        assert_eq!(call(&copy, EVENT_READ, [0; 2]), Ok([1, 1]));
        let raw = copy.as_raw_fd();
        drop(copy);
        assert_eq!(hitode_ntsync_known(raw), 0);
        assert_eq!(
            ioctl(raw, EVENT_READ, [0u32; 2].as_mut_ptr()),
            Err(libc::EBADF)
        );
    }
}
