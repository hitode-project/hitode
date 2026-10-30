//! In-process implementation of the Linux ntsync UAPI. The execution owner
//! serializes device operations; pending acquisitions park in the machine.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::rc::Rc;

#[cfg(target_vendor = "apple")]
const WRITE: u32 = 0x8000_0000;
#[cfg(not(target_vendor = "apple"))]
const WRITE: u32 = 0x4000_0000;
#[cfg(target_vendor = "apple")]
const READ: u32 = 0x4000_0000;
#[cfg(not(target_vendor = "apple"))]
const READ: u32 = 0x8000_0000;
const fn command(n: u32, size: u32, direction: u32) -> u32 {
    direction | (size << 16) | (0x4e << 8) | n
}
pub const CREATE_SEM: u32 = command(0x80, 8, WRITE);
pub const SEM_RELEASE: u32 = command(0x81, 4, READ | WRITE);
pub const WAIT_ANY: u32 = command(0x82, 40, READ | WRITE);
pub const WAIT_ALL: u32 = command(0x83, 40, READ | WRITE);
pub const CREATE_MUTEX: u32 = command(0x84, 8, WRITE);
pub const MUTEX_UNLOCK: u32 = command(0x85, 8, READ | WRITE);
pub const MUTEX_KILL: u32 = command(0x86, 4, WRITE);
pub const CREATE_EVENT: u32 = command(0x87, 8, WRITE);
pub const EVENT_SET: u32 = command(0x88, 4, READ);
pub const EVENT_RESET: u32 = command(0x89, 4, READ);
pub const EVENT_PULSE: u32 = command(0x8a, 4, READ);
pub const SEM_READ: u32 = command(0x8b, 8, READ);
pub const MUTEX_READ: u32 = command(0x8c, 8, READ);
pub const EVENT_READ: u32 = command(0x8d, 8, READ);

#[derive(Clone, Copy, Default)]
#[repr(C)]
pub struct WaitArgs {
    pub timeout: u64,
    pub objs: u64,
    pub count: u32,
    pub index: u32,
    pub flags: u32,
    pub owner: u32,
    pub alert: u32,
    pub pad: u32,
}

#[derive(Clone, Copy)]
enum Kind {
    Device,
    Semaphore {
        count: u32,
        max: u32,
    },
    Mutex {
        owner: u32,
        count: u32,
        abandoned: bool,
    },
    Event {
        manual: bool,
        signaled: bool,
    },
}
struct Object {
    instance: u64,
    kind: Cell<Kind>,
    exported: RefCell<Option<OwnedFd>>,
    waiters: Cell<usize>,
}
type ObjectRef = Rc<Object>;
pub(crate) type Completion = Cell<Option<Result<(u32, bool), i32>>>;
struct Waiter {
    objects: Vec<ObjectRef>,
    alert: Option<ObjectRef>,
    owner: u32,
    all: bool,
    result: Rc<Completion>,
    timeout: u64,
    realtime: bool,
}
#[derive(Default)]
struct Device {
    next: u64,
    monotonic: u64,
    unix: u64,
    fds: HashMap<RawFd, ObjectRef>,
    local: Vec<Slot>,
    free: Option<usize>,
    exported: HashMap<RawFd, std::rc::Weak<Object>>,
    transfers: Vec<((libc::dev_t, libc::ino_t), ObjectRef)>,
    waiters: Vec<Waiter>,
}
const LOCAL_FD: i32 = 1 << 28;

enum Slot {
    Live(ObjectRef),
    Free(Option<usize>),
}
impl Device {
    fn object(&self, fd: RawFd) -> Option<&ObjectRef> {
        if fd < LOCAL_FD {
            return self.fds.get(&fd);
        }
        match self.local.get((fd - LOCAL_FD) as usize)? {
            Slot::Live(object) => Some(object),
            Slot::Free(_) => None,
        }
    }
    fn forget(&mut self, fd: RawFd) {
        if fd < LOCAL_FD {
            self.fds.remove(&fd);
            return;
        }
        let index = (fd - LOCAL_FD) as usize;
        if matches!(self.local.get(index), Some(Slot::Live(_))) {
            self.local[index] = Slot::Free(self.free);
            self.free = Some(index);
        }
    }
}

thread_local! {
    static DEVICE: RefCell<Device> = RefCell::new(Device::default());
}

/// A descriptor registration, removed before the kernel descriptor closes.
pub struct Fd {
    raw: RawFd,
    _owned: Option<OwnedFd>,
}
impl AsRawFd for Fd {
    fn as_raw_fd(&self) -> RawFd {
        self.raw
    }
}
impl From<OwnedFd> for Fd {
    fn from(fd: OwnedFd) -> Self {
        Self {
            raw: fd.as_raw_fd(),
            _owned: Some(fd),
        }
    }
}
impl FromRawFd for Fd {
    unsafe fn from_raw_fd(fd: RawFd) -> Self {
        Self {
            raw: fd,
            _owned: (fd < LOCAL_FD).then(|| unsafe { OwnedFd::from_raw_fd(fd) }),
        }
    }
}
impl Drop for Fd {
    fn drop(&mut self) {
        hitode_ntsync_forget(self.as_raw_fd());
    }
}
fn identity(fd: RawFd) -> (libc::dev_t, libc::ino_t) {
    // SAFETY: fstat initializes this buffer for a live transferred descriptor.
    unsafe {
        let mut st = std::mem::zeroed();
        libc::fstat(fd, &mut st);
        (st.st_dev, st.st_ino)
    }
}
fn descriptor(device: &mut Device, instance: u64, kind: Kind) -> Result<i32, i32> {
    let object = Rc::new(Object {
        instance,
        kind: Cell::new(kind),
        exported: RefCell::new(None),
        waiters: Cell::new(0),
    });
    let index = if let Some(index) = device.free {
        let Slot::Free(next) = device.local[index] else {
            unreachable!()
        };
        device.free = next;
        device.local[index] = Slot::Live(object);
        index
    } else {
        device.local.push(Slot::Live(object));
        device.local.len() - 1
    };
    Ok(LOCAL_FD + index as i32)
}

/// Materialize a kernel identity only when SCM_RIGHTS needs one.
#[unsafe(no_mangle)]
pub extern "C" fn hitode_ntsync_export(fd: i32) -> i32 {
    if fd < LOCAL_FD {
        return fd;
    }
    let result = DEVICE.with_borrow_mut(|device| {
        let object = device.object(fd).cloned().ok_or(libc::EBADF)?;
        let mut exported = object.exported.borrow_mut();
        if exported.is_none() {
            let mut fds = [0; 2];
            // SAFETY: fds receives the new kernel identity's pipe ends.
            if unsafe { libc::pipe(fds.as_mut_ptr()) } < 0 {
                return Err(errno());
            }
            // SAFETY: the write end is unused; the read end is newly owned.
            unsafe {
                libc::close(fds[1]);
                libc::fcntl(fds[0], libc::F_SETFD, libc::FD_CLOEXEC);
            }
            *exported = Some(unsafe { OwnedFd::from_raw_fd(fds[0]) });
        }
        let raw = exported.as_ref().unwrap().as_raw_fd();
        device.exported.insert(raw, Rc::downgrade(&object));
        Ok(raw)
    });
    ffi_result(result)
}

/// True means the descriptor was entirely host state, with no kernel close.
#[unsafe(no_mangle)]
pub extern "C" fn hitode_ntsync_close(fd: i32) -> i32 {
    hitode_ntsync_forget(fd);
    i32::from(fd >= LOCAL_FD)
}

fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap()
}

#[unsafe(no_mangle)]
pub extern "C" fn hitode_ntsync_open() -> i32 {
    let result = DEVICE.with_borrow_mut(|device| {
        device.next += 1;
        descriptor(device, device.next, Kind::Device)
    });
    ffi_result(result)
}
#[unsafe(no_mangle)]
pub extern "C" fn hitode_ntsync_forget(fd: i32) {
    DEVICE.with_borrow_mut(|device| {
        device.forget(fd);
    });
}
#[unsafe(no_mangle)]
pub extern "C" fn hitode_ntsync_known(fd: i32) -> i32 {
    DEVICE.with_borrow(|device| i32::from(device.object(fd).is_some()))
}
/// Holds references while SCM_RIGHTS owns descriptors, including after close.
#[unsafe(no_mangle)]
pub extern "C" fn hitode_ntsync_transfer(fd: i32, operation: i32) {
    DEVICE.with_borrow_mut(|device| {
        let key = identity(fd);
        if operation == 0 {
            if let Some(object) = device
                .object(fd)
                .cloned()
                .or_else(|| device.exported.get(&fd).and_then(std::rc::Weak::upgrade))
            {
                device.transfers.push((key, object));
            }
        } else if let Some(index) = device.transfers.iter().position(|(id, _)| *id == key) {
            let (_, object) = device.transfers.remove(index);
            if operation > 0 {
                device.fds.insert(fd, object);
            }
        }
    });
}
fn ffi_result(result: Result<i32, i32>) -> i32 {
    match result {
        Ok(value) => value,
        Err(error) => {
            // SAFETY: libc returns this thread's errno location.
            unsafe {
                #[cfg(target_vendor = "apple")]
                {
                    *libc::__error() = error;
                }
                #[cfg(target_os = "android")]
                {
                    *libc::__errno() = error;
                }
                #[cfg(not(any(target_vendor = "apple", target_os = "android")))]
                {
                    *libc::__errno_location() = error;
                }
            }
            -1
        }
    }
}
#[unsafe(no_mangle)]
unsafe extern "C" fn hitode_ntsync_ioctl(fd: i32, request: libc::c_ulong, arg: *mut u32) -> i32 {
    // SAFETY: the shim passes the UAPI buffer supplied by Wine.
    ffi_result(unsafe { ioctl(fd, request as u32, arg) })
}

/// Executes a nonblocking object ioctl with a host UAPI buffer.
/// # Safety
/// `arg` must address the command's readable/writable UAPI structure.
pub unsafe fn ioctl(fd: i32, request: u32, arg: *mut u32) -> Result<i32, i32> {
    if arg.is_null() {
        return Err(libc::EFAULT);
    }
    DEVICE.with_borrow_mut(|device| {
        let object = device.object(fd).cloned().ok_or(libc::EBADF)?;
        // SAFETY: the caller supplies the structure selected by request.
        let first = if matches!(
            request,
            CREATE_SEM | CREATE_MUTEX | CREATE_EVENT | SEM_RELEASE | MUTEX_UNLOCK | MUTEX_KILL
        ) {
            unsafe { *arg }
        } else {
            0
        };
        let kind = object.kind.get();
        let mut second = None;
        let mut previous = first;
        let next = match (kind, request) {
            (Kind::Device, CREATE_SEM | CREATE_MUTEX | CREATE_EVENT) => {
                // SAFETY: creation commands take two u32 fields.
                let other = unsafe { *arg.add(1) };
                let kind = match request {
                    CREATE_SEM if first <= other => Kind::Semaphore {
                        count: first,
                        max: other,
                    },
                    CREATE_MUTEX if (first == 0) == (other == 0) => Kind::Mutex {
                        owner: first,
                        count: other,
                        abandoned: false,
                    },
                    CREATE_EVENT => Kind::Event {
                        manual: first != 0,
                        signaled: other != 0,
                    },
                    _ => return Err(libc::EINVAL),
                };
                return descriptor(device, object.instance, kind);
            }
            (Kind::Semaphore { count, max }, SEM_RELEASE) => {
                let count_new = count
                    .checked_add(first)
                    .filter(|&n| n <= max)
                    .ok_or(libc::EOVERFLOW)?;
                previous = count;
                Kind::Semaphore {
                    count: count_new,
                    max,
                }
            }
            (Kind::Semaphore { count, max }, SEM_READ) => {
                previous = count;
                second = Some(max);
                kind
            }
            (
                Kind::Mutex {
                    owner,
                    count,
                    abandoned,
                },
                MUTEX_UNLOCK | MUTEX_KILL,
            ) => {
                if first == 0 {
                    return Err(libc::EINVAL);
                }
                if first != owner {
                    return Err(libc::EPERM);
                }
                if request == MUTEX_KILL {
                    Kind::Mutex {
                        owner: 0,
                        count: 0,
                        abandoned: true,
                    }
                } else {
                    second = Some(count);
                    Kind::Mutex {
                        owner: if count == 1 { 0 } else { owner },
                        count: count - 1,
                        abandoned,
                    }
                }
            }
            (
                Kind::Mutex {
                    owner,
                    count,
                    abandoned,
                },
                MUTEX_READ,
            ) => {
                // SAFETY: MUTEX_READ writes both fields even for EOWNERDEAD.
                unsafe {
                    *arg = owner;
                    *arg.add(1) = count;
                }
                return if abandoned {
                    Err(libc::EOWNERDEAD)
                } else {
                    Ok(0)
                };
            }
            (Kind::Event { manual, signaled }, EVENT_SET | EVENT_RESET | EVENT_PULSE) => {
                previous = u32::from(signaled);
                Kind::Event {
                    manual,
                    signaled: request != EVENT_RESET,
                }
            }
            (Kind::Event { manual, signaled }, EVENT_READ) => {
                previous = u32::from(manual);
                second = Some(u32::from(signaled));
                kind
            }
            _ => return Err(libc::ENOTTY),
        };
        object.kind.set(next);
        // Read/reset do not make an acquisition possible.
        if matches!(
            request,
            SEM_RELEASE | MUTEX_UNLOCK | MUTEX_KILL | EVENT_SET | EVENT_PULSE
        ) && object.waiters.get() != 0
        {
            device.wake();
        }
        if request == EVENT_PULSE
            && let Kind::Event { manual, .. } = next
        {
            object.kind.set(Kind::Event {
                manual,
                signaled: false,
            });
        }
        if request != MUTEX_KILL {
            // SAFETY: output fields belong to the command's UAPI buffer.
            unsafe {
                *arg = previous;
                if let Some(value) = second {
                    *arg.add(1) = value;
                }
            }
        }
        Ok(0)
    })
}
impl Object {
    fn signaled(&self, owner: u32) -> bool {
        match self.kind.get() {
            Kind::Semaphore { count, .. } => count != 0,
            Kind::Mutex {
                owner: current,
                count,
                ..
            } => (current == 0 || current == owner) && count != u32::MAX,
            Kind::Event { signaled, .. } => signaled,
            Kind::Device => false,
        }
    }
    fn acquire(&self, owner: u32) -> bool {
        let mut dead = false;
        self.kind.set(match self.kind.get() {
            Kind::Semaphore { count, max } => Kind::Semaphore {
                count: count - 1,
                max,
            },
            Kind::Mutex {
                count, abandoned, ..
            } => {
                dead = abandoned;
                Kind::Mutex {
                    owner,
                    count: count + 1,
                    abandoned: false,
                }
            }
            Kind::Event { manual, .. } => Kind::Event {
                manual,
                signaled: manual,
            },
            Kind::Device => unreachable!(),
        });
        dead
    }
}
impl Drop for Waiter {
    fn drop(&mut self) {
        for object in self.objects.iter().chain(self.alert.iter()) {
            object.waiters.set(object.waiters.get() - 1);
        }
    }
}

impl Waiter {
    fn acquire(&self) -> Option<(u32, bool)> {
        if self.all {
            if self
                .objects
                .iter()
                .all(|object| object.signaled(self.owner))
            {
                let dead = self
                    .objects
                    .iter()
                    .fold(false, |dead, object| object.acquire(self.owner) | dead);
                return Some((0, dead));
            }
        } else if let Some(index) = self
            .objects
            .iter()
            .position(|object| object.signaled(self.owner))
        {
            return Some((index as u32, self.objects[index].acquire(self.owner)));
        }
        self.alert
            .as_ref()
            .filter(|alert| alert.signaled(self.owner))
            .map(|alert| (self.objects.len() as u32, alert.acquire(self.owner)))
    }
}
impl Device {
    fn wake(&mut self) {
        self.waiters.retain(|waiter| {
            let now = if waiter.realtime {
                self.unix
            } else {
                self.monotonic
            };
            if waiter.timeout != u64::MAX && waiter.timeout <= now {
                waiter.result.set(Some(Err(libc::ETIMEDOUT)));
                return false;
            }
            if let Some(result) = waiter.acquire() {
                waiter.result.set(Some(Ok(result)));
                false
            } else {
                true
            }
        });
    }
}

/// A queued atomic acquisition. Dropping it cancels the device waiter.
pub struct Pending {
    result: Rc<Completion>,
}
impl Pending {
    pub fn result(&self) -> Option<Result<(u32, bool), i32>> {
        self.result.get()
    }
    pub(crate) fn completion(&self) -> std::rc::Weak<Completion> {
        Rc::downgrade(&self.result)
    }
}
impl Drop for Pending {
    fn drop(&mut self) {
        DEVICE.with_borrow_mut(|device| {
            device
                .waiters
                .retain(|w| !Rc::ptr_eq(&w.result, &self.result))
        });
    }
}
/// Registers WAIT_ANY/WAIT_ALL without blocking the execution owner. Timeout
/// expiry is supplied by the machine, in the same clock domain as the guest.
/// # Safety
/// `args.objs` must point to `args.count` readable host descriptors.
pub unsafe fn wait_ioctl(fd: i32, request: u32, args: &WaitArgs) -> Result<Pending, i32> {
    if !matches!(request, WAIT_ANY | WAIT_ALL) {
        return Err(libc::ENOTTY);
    }
    if args.flags & !1 != 0 || args.pad != 0 || args.count > 64 || args.owner == 0 {
        return Err(libc::EINVAL);
    }
    if args.count != 0 && args.objs == 0 {
        return Err(libc::EFAULT);
    }
    // SAFETY: the caller supplies count descriptors, checked for the empty case.
    let objects = if args.count == 0 {
        &[][..]
    } else {
        unsafe { std::slice::from_raw_parts(args.objs as *const i32, args.count as usize) }
    };
    let (owner, alert, all) = (args.owner, args.alert as i32, request == WAIT_ALL);
    DEVICE.with_borrow_mut(|device| {
        let instance = device
            .object(fd)
            .filter(|o| matches!(o.kind.get(), Kind::Device))
            .ok_or(libc::EBADF)?
            .instance;
        let get = |fd| {
            device
                .object(fd)
                .filter(|o| o.instance == instance && !matches!(o.kind.get(), Kind::Device))
                .cloned()
                .ok_or(libc::EINVAL)
        };
        let objects = objects
            .iter()
            .map(|&fd| get(fd))
            .collect::<Result<Vec<_>, _>>()?;
        let alert = if alert == 0 {
            None
        } else {
            let object = get(alert)?;
            if !matches!(object.kind.get(), Kind::Event { .. }) {
                return Err(libc::EINVAL);
            }
            Some(object)
        };
        if all {
            let entries: Vec<_> = objects.iter().chain(alert.iter()).collect();
            for (index, object) in entries.iter().enumerate() {
                if entries[..index]
                    .iter()
                    .any(|other| Rc::ptr_eq(object, other))
                {
                    return Err(libc::EINVAL);
                }
            }
        }
        let result = Rc::new(Cell::new(None));
        for object in objects.iter().chain(alert.iter()) {
            object.waiters.set(object.waiters.get() + 1);
        }
        let waiter = Waiter {
            objects,
            alert,
            owner,
            all,
            result: result.clone(),
            timeout: args.timeout,
            realtime: args.flags & 1 != 0,
        };
        if let Some(acquired) = waiter.acquire() {
            result.set(Some(Ok(acquired)));
        } else if args.timeout != u64::MAX
            && args.timeout
                <= if args.flags & 1 != 0 {
                    device.unix
                } else {
                    device.monotonic
                }
        {
            result.set(Some(Err(libc::ETIMEDOUT)));
        } else {
            device.waiters.push(waiter);
        }
        Ok(Pending { result })
    })
}

pub(crate) fn set_time(time: crate::server::Time) {
    DEVICE.with_borrow_mut(|device| {
        device.monotonic = time.monotonic.saturating_mul(100);
        device.unix = time.unix.saturating_mul(100);
    });
}

/// A service may signal only when no acquisition needs scheduler delivery.
pub(crate) fn service_signal(fd: i32, command: u32, input: u32) -> Option<Result<[u32; 2], i32>> {
    let would_wake = DEVICE.with_borrow(|device| {
        let Some(object) = device.object(fd) else {
            return false;
        };
        if object.waiters.get() == 0 {
            return false;
        }
        let before = object.kind.get();
        let after = match before {
            Kind::Event { manual, .. } if command == EVENT_SET => Kind::Event {
                manual,
                signaled: true,
            },
            Kind::Semaphore { count, max }
                if command == SEM_RELEASE && input <= max.saturating_sub(count) =>
            {
                Kind::Semaphore {
                    count: count + input,
                    max,
                }
            }
            Kind::Mutex {
                owner,
                count,
                abandoned,
            } if command == MUTEX_UNLOCK && owner == input && count != 0 => Kind::Mutex {
                owner: if count == 1 { 0 } else { owner },
                count: count - 1,
                abandoned,
            },
            _ => return false,
        };
        object.kind.set(after);
        let ready = device.waiters.iter().any(|waiter| {
            (if waiter.all {
                waiter.objects.iter().all(|o| o.signaled(waiter.owner))
            } else {
                waiter.objects.iter().any(|o| o.signaled(waiter.owner))
            }) || waiter
                .alert
                .as_ref()
                .is_some_and(|o| o.signaled(waiter.owner))
        });
        object.kind.set(before);
        ready
    });
    if would_wake {
        return None;
    }
    let mut values = [input, 0];
    // SAFETY: all supported signal commands use at most these two words.
    Some(unsafe { ioctl(fd, command, values.as_mut_ptr()) }.map(|_| values))
}

/// Immediate acquisition has no waiter allocation and never schedules.
pub(crate) fn service_wait(fds: &[i32], owner: u32, all: bool) -> Result<Option<(u32, bool)>, i32> {
    DEVICE.with_borrow(|device| {
        for &fd in fds {
            if device.object(fd).is_none() {
                return Err(libc::EINVAL);
            }
        }
        if all {
            if !fds
                .iter()
                .all(|&fd| device.object(fd).unwrap().signaled(owner))
            {
                return Ok(None);
            }
            let dead = fds.iter().fold(false, |dead, &fd| {
                device.object(fd).unwrap().acquire(owner) | dead
            });
            Ok(Some((0, dead)))
        } else {
            Ok(fds
                .iter()
                .position(|&fd| device.object(fd).unwrap().signaled(owner))
                .map(|index| {
                    (
                        index as u32,
                        device.object(fds[index]).unwrap().acquire(owner),
                    )
                }))
        }
    })
}

pub(crate) fn service_alert(fd: i32, owner: u32) -> bool {
    DEVICE.with_borrow(|device| {
        device
            .object(fd)
            .is_some_and(|object| object.signaled(owner))
    })
}
#[cfg(test)]
mod service_tests {
    use super::*;

    // spec: EXEC-CALL-3
    #[test]
    fn closing_an_unexported_object_has_no_kernel_descriptor() {
        let device = hitode_ntsync_open();
        let mut event = [0, 0];
        // SAFETY: CREATE_EVENT receives its two-word structure.
        let fd = unsafe { ioctl(device, CREATE_EVENT, event.as_mut_ptr()) }.unwrap();
        DEVICE
            .with_borrow(|device| assert!(device.object(fd).unwrap().exported.borrow().is_none()));
        assert_eq!(hitode_ntsync_close(fd), 1);
        assert_eq!(hitode_ntsync_known(fd), 0);
        assert_eq!(hitode_ntsync_close(device), 1);
    }

    // spec: NAT-BODY-18
    #[test]
    fn service_signals_leave_wakeups_and_nonzero_waits_to_the_machine() {
        let device = hitode_ntsync_open();
        let mut value = [0, 0];
        // SAFETY: two event-creation words.
        let event = unsafe { ioctl(device, CREATE_EVENT, value.as_mut_ptr()) }.unwrap();
        assert_eq!(service_wait(&[event], 7, false), Ok(None));
        assert_eq!(service_signal(event, EVENT_SET, 0), Some(Ok([0, 0])));
        assert_eq!(service_wait(&[event], 7, false), Ok(Some((0, false))));
        let fds = [event];
        let args = WaitArgs {
            timeout: u64::MAX,
            objs: fds.as_ptr() as u64,
            count: 1,
            owner: 7,
            ..Default::default()
        };
        // SAFETY: args retains its live descriptor array through registration.
        let pending = unsafe { wait_ioctl(device, WAIT_ANY, &args) }.unwrap();
        assert_eq!(service_signal(event, EVENT_SET, 0), None);
        assert_eq!(pending.result(), None);
        assert_eq!(service_signal(event, EVENT_RESET, 0), Some(Ok([0, 0])));
        // SAFETY: ioctl uses this two-word event buffer.
        unsafe { ioctl(event, EVENT_SET, value.as_mut_ptr()) }.unwrap();
        assert_eq!(pending.result(), Some(Ok((0, false))));
        drop(pending);
        hitode_ntsync_close(event);
        hitode_ntsync_close(device);
    }
}
