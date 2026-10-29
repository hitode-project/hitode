//! The Unix file calls of the C library, on host descriptors.
//!
//! Files open for reading only, as Wine's Unix libraries open fonts and
//! data. A descriptor the guest receives is the host's own; the guest may
//! use and close only those it opened. A mapping is a read-only view that
//! shares the file's pages. Failures set the guest's `errno` to Linux's
//! number, which matches the host's for the errors below 35.

use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::os::fd::BorrowedFd;
use std::os::unix::ffi::OsStrExt;

use adv_cpu::backend::GuestExt;
use adv_machine::{Conv, CtxId, Env, Outcome};
use adv_nt::memory::{self, Memory, PAGE_READONLY};

use crate::Function;

const O_ACCMODE: u32 = 3;
const O_RDONLY: u32 = 0;
/// The flags that change nothing for a read-only open.
const O_IGNORED: u32 = 0x8000 /* O_LARGEFILE */ | 0x80000 /* O_CLOEXEC */ | 0x800 /* O_NONBLOCK */;
const O_DIRECTORY: u32 = 0x1_0000;
const F_GETFD: u32 = 1;
const F_SETFD: u32 = 2;
const F_GETFL: u32 = 3;
const PROT_READ: u32 = 1;
const MAP_PRIVATE: u32 = 2;
const MAP_SHARED: u32 = 1;
const MAP_FAILED: u32 = u32::MAX;
const EBADF: u32 = 9;
const EACCES: u32 = 13;
const EINVAL: u32 = 22;
const EIO: u32 = 5;
const ENOMEM: u32 = 12;
const EOVERFLOW: u32 = 75;
const LINUX_ERRNO_LIMIT: i32 = 35;

pub fn function(name: &str) -> Option<Function> {
    let cdecl = |argc| Conv::Cdecl { argc };
    Some(match name {
        "open" | "open64" => (cdecl(3), open),
        "close" => (cdecl(1), close),
        "read" => (cdecl(3), read),
        "fstat" => (cdecl(2), fstat),
        "__fstat64_time64" => (cdecl(2), fstat64_time64),
        "__stat64_time64" => (cdecl(2), stat64_time64),
        "fcntl" | "fcntl64" => (cdecl(3), fcntl),
        "mmap" => (cdecl(6), mmap),
        "mmap64" => (cdecl(7), mmap64),
        "munmap" => (cdecl(2), munmap),
        "__errno_location" => (cdecl(0), errno_location),
        "strerror" => (cdecl(1), strerror),
        _ => return None,
    })
}

/// The descriptors the guest opened.
#[derive(Default)]
struct Descriptors(HashSet<i32>);

/// Each thread's `errno`, by context, and the page the next one goes in.
#[derive(Default)]
struct Errno {
    slots: HashMap<CtxId, u32>,
    next: u32,
}

const PAGE: u32 = 0x1000;

fn errno_slot(env: &mut Env<'_>) -> Option<u32> {
    let context = env.context;
    if let Some(&slot) = env.ext.get_or_default::<Errno>().slots.get(&context) {
        return Some(slot);
    }
    let mut next = env.ext.get_or_default::<Errno>().next;
    if next.is_multiple_of(PAGE) {
        let memory = env.ext.get_or_default::<Memory>();
        next = memory::allocate_committed(env.cpu, &mut env.sys.vm, memory, PAGE)?;
    }
    let errno = env.ext.get_or_default::<Errno>();
    errno.slots.insert(context, next);
    errno.next = next + 4;
    Some(next)
}

fn errno_location(env: &mut Env<'_>) -> Outcome {
    match errno_slot(env) {
        Some(slot) => Outcome::Return(slot),
        None => Outcome::Fail("__errno_location: no memory".into()),
    }
}

/// The buffer `strerror` returns, which each call overwrites.
struct Message(u32);

/// strerror(errno), in the host's words: the numbers below
/// `LINUX_ERRNO_LIMIT` mean the same on every host.
fn strerror(env: &mut Env<'_>) -> Outcome {
    let errno = env.arg(0) as i32;
    let text = if (0..LINUX_ERRNO_LIMIT).contains(&errno) {
        std::io::Error::from_raw_os_error(errno).to_string()
    } else {
        format!("Unknown error {errno}")
    };
    let buffer = match env.ext.try_get::<Message>() {
        Some(message) => message.0,
        None => {
            let memory = env.ext.get_or_default::<Memory>();
            let Some(page) = memory::allocate_committed(env.cpu, &mut env.sys.vm, memory, PAGE)
            else {
                return Outcome::Fail("strerror: no memory".into());
            };
            env.ext.insert(Message(page));
            page
        }
    };
    let mut bytes = text.into_bytes();
    bytes.truncate(PAGE as usize - 1);
    bytes.push(0);
    match env.cpu.write(buffer, &bytes) {
        Ok(()) => Outcome::Return(buffer),
        Err(error) => Outcome::Fail(format!("strerror: {error}")),
    }
}

/// Sets `errno` and returns `value`, the call's failure.
fn fail(env: &mut Env<'_>, errno: u32, value: u32) -> Outcome {
    match errno_slot(env).map(|slot| env.cpu.write_u32(slot, errno)) {
        Some(Ok(())) => Outcome::Return(value),
        _ => Outcome::Fail("errno is not writable".into()),
    }
}

/// The host's `errno` as Linux numbers it.
fn host_errno() -> u32 {
    match std::io::Error::last_os_error().raw_os_error() {
        Some(errno) if errno < LINUX_ERRNO_LIMIT => errno as u32,
        _ => EIO,
    }
}

/// The guest's descriptor `fd`, if it opened it.
fn descriptor(env: &mut Env<'_>, fd: u32) -> Option<i32> {
    let fd = fd as i32;
    env.ext
        .get_or_default::<Descriptors>()
        .0
        .contains(&fd)
        .then_some(fd)
}

/// open(path, flags, mode)
fn open(env: &mut Env<'_>) -> Outcome {
    let (path, flags) = (env.arg(0), env.arg(1));
    let Ok(path) = env.cpu.read_cstr(path, ::libc::PATH_MAX as usize) else {
        return Outcome::Fail(format!("open: unreadable path at {path:#x}"));
    };
    if flags & O_ACCMODE != O_RDONLY || flags & !(O_ACCMODE | O_IGNORED | O_DIRECTORY) != 0 {
        log::debug!(
            "open {}: flags {flags:#x}",
            OsStr::from_bytes(&path).display()
        );
        return fail(env, EACCES, u32::MAX);
    }
    let Ok(path) = std::ffi::CString::new(path) else {
        return fail(env, EINVAL, u32::MAX);
    };
    let mut host = ::libc::O_RDONLY | ::libc::O_CLOEXEC;
    if flags & O_DIRECTORY != 0 {
        host |= ::libc::O_DIRECTORY;
    }
    // SAFETY: a NUL-terminated path.
    let fd = unsafe { ::libc::open(path.as_ptr(), host) };
    if fd < 0 {
        return fail(env, host_errno(), u32::MAX);
    }
    env.ext.get_or_default::<Descriptors>().0.insert(fd);
    Outcome::Return(fd as u32)
}

fn close(env: &mut Env<'_>) -> Outcome {
    let Some(fd) = descriptor(env, env.arg(0)) else {
        return fail(env, EBADF, u32::MAX);
    };
    env.ext.get_or_default::<Descriptors>().0.remove(&fd);
    // SAFETY: a descriptor the guest opened and no longer uses.
    unsafe { ::libc::close(fd) };
    Outcome::Return(0)
}

/// read(fd, buffer, count)
fn read(env: &mut Env<'_>) -> Outcome {
    let (buffer, count) = (env.arg(1), env.arg(2));
    let Some(fd) = descriptor(env, env.arg(0)) else {
        return fail(env, EBADF, u32::MAX);
    };
    let mut fallback = None;
    let bytes = match env.cpu.writable_bytes(buffer, count as usize) {
        Some(bytes) => bytes,
        None => fallback.insert(vec![0; count as usize]).as_mut_slice(),
    };
    // SAFETY: bytes holds count writable bytes.
    let read = unsafe { ::libc::read(fd, bytes.as_mut_ptr().cast(), bytes.len()) };
    if read < 0 {
        return fail(env, host_errno(), u32::MAX);
    }
    match fallback.as_ref().map_or(Ok(()), |bytes| {
        env.cpu.write(buffer, &bytes[..read as usize])
    }) {
        Ok(()) => Outcome::Return(read as u32),
        Err(_) => fail(env, 14 /* EFAULT */, u32::MAX),
    }
}

fn host_stat(env: &mut Env<'_>) -> Result<::libc::stat, u32> {
    let fd = descriptor(env, env.arg(0)).ok_or(EBADF)?;
    // SAFETY: a zeroed stat is a valid buffer for fstat to fill.
    let mut stat: ::libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: a descriptor the guest opened, and a valid buffer.
    if unsafe { ::libc::fstat(fd, &mut stat) } != 0 {
        return Err(host_errno());
    }
    Ok(stat)
}

/// fstat(fd, *stat): glibc's i386 `struct stat`, with 32-bit sizes and
/// times.
fn fstat(env: &mut Env<'_>) -> Outcome {
    let buffer = env.arg(1);
    let stat = match host_stat(env) {
        Ok(stat) => stat,
        Err(errno) => return fail(env, errno, u32::MAX),
    };
    let Ok(size) = i32::try_from(stat.st_size) else {
        return fail(env, EOVERFLOW, u32::MAX);
    };
    let mut bytes = [0u8; 88];
    bytes[0..8].copy_from_slice(&(stat.st_dev as u64).to_le_bytes());
    bytes[12..16].copy_from_slice(&(stat.st_ino as u32).to_le_bytes());
    common_fields(&mut bytes, &stat);
    bytes[44..48].copy_from_slice(&size.to_le_bytes());
    bytes[48..52].copy_from_slice(&(stat.st_blksize as u32).to_le_bytes());
    bytes[52..56].copy_from_slice(&(stat.st_blocks as u32).to_le_bytes());
    for (at, (seconds, nanoseconds)) in [56, 64, 72].into_iter().zip(times(&stat)) {
        bytes[at..at + 4].copy_from_slice(&(seconds as u32).to_le_bytes());
        bytes[at + 4..at + 8].copy_from_slice(&(nanoseconds as u32).to_le_bytes());
    }
    store_stat(env, buffer, &bytes)
}

/// __fstat64_time64(fd, *stat): glibc's i386 `struct __stat64_t64`.
fn fstat64_time64(env: &mut Env<'_>) -> Outcome {
    let stat = host_stat(env);
    store_stat64(env, stat)
}

/// __stat64_time64(path, *stat), as [`fstat64_time64`].
fn stat64_time64(env: &mut Env<'_>) -> Outcome {
    let path = env.arg(0);
    let Ok(path) = env.cpu.read_cstr(path, ::libc::PATH_MAX as usize) else {
        return Outcome::Fail(format!("stat: unreadable path at {path:#x}"));
    };
    let Ok(path) = std::ffi::CString::new(path) else {
        return fail(env, EINVAL, u32::MAX);
    };
    // SAFETY: a zeroed stat is a valid buffer for stat to fill.
    let mut stat: ::libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: a NUL-terminated path and a valid buffer.
    let stat = match unsafe { ::libc::stat(path.as_ptr(), &mut stat) } {
        0 => Ok(stat),
        _ => Err(host_errno()),
    };
    store_stat64(env, stat)
}

#[allow(
    clippy::unnecessary_cast,
    reason = "the types of libc's stat fields differ between hosts"
)]
fn store_stat64(env: &mut Env<'_>, stat: Result<::libc::stat, u32>) -> Outcome {
    let buffer = env.arg(1);
    let stat = match stat {
        Ok(stat) => stat,
        Err(errno) => return fail(env, errno, u32::MAX),
    };
    let mut bytes = [0u8; 108];
    bytes[0..8].copy_from_slice(&(stat.st_dev as u64).to_le_bytes());
    bytes[8..16].copy_from_slice(&(stat.st_ino as u64).to_le_bytes());
    common_fields(&mut bytes, &stat);
    bytes[40..48].copy_from_slice(&(stat.st_size as i64).to_le_bytes());
    bytes[48..52].copy_from_slice(&(stat.st_blksize as u32).to_le_bytes());
    bytes[52..60].copy_from_slice(&(stat.st_blocks as i64).to_le_bytes());
    for (at, (seconds, nanoseconds)) in [60, 76, 92].into_iter().zip(times(&stat)) {
        bytes[at..at + 8].copy_from_slice(&seconds.to_le_bytes());
        bytes[at + 8..at + 12].copy_from_slice(&(nanoseconds as u32).to_le_bytes());
    }
    store_stat(env, buffer, &bytes)
}

/// The mode, link count, owner, group and device at the offsets both
/// layouts share. The file type bits are the same on every host.
fn common_fields(bytes: &mut [u8], stat: &::libc::stat) {
    bytes[16..20].copy_from_slice(&u32::from(stat.st_mode).to_le_bytes());
    bytes[20..24].copy_from_slice(&(stat.st_nlink as u32).to_le_bytes());
    bytes[24..28].copy_from_slice(&stat.st_uid.to_le_bytes());
    bytes[28..32].copy_from_slice(&stat.st_gid.to_le_bytes());
    bytes[32..40].copy_from_slice(&(stat.st_rdev as u64).to_le_bytes());
}

/// Access, modification and change times.
#[allow(
    clippy::unnecessary_cast,
    reason = "the types of libc's stat fields differ between hosts"
)]
fn times(stat: &::libc::stat) -> [(i64, i64); 3] {
    [
        (stat.st_atime, stat.st_atime_nsec),
        (stat.st_mtime, stat.st_mtime_nsec),
        (stat.st_ctime, stat.st_ctime_nsec),
    ]
    .map(|(seconds, nanoseconds)| (seconds as i64, nanoseconds as i64))
}

fn store_stat(env: &mut Env<'_>, buffer: u32, bytes: &[u8]) -> Outcome {
    match env.cpu.write(buffer, bytes) {
        Ok(()) => Outcome::Return(0),
        Err(_) => fail(env, 14 /* EFAULT */, u32::MAX),
    }
}

/// fcntl(fd, cmd, arg): a read-only descriptor's flags; close-on-exec has
/// no meaning in the guest.
fn fcntl(env: &mut Env<'_>) -> Outcome {
    let command = env.arg(1);
    if descriptor(env, env.arg(0)).is_none() {
        return fail(env, EBADF, u32::MAX);
    }
    match command {
        F_GETFD | F_SETFD | F_GETFL => Outcome::Return(O_RDONLY),
        _ => fail(env, EINVAL, u32::MAX),
    }
}

/// mmap(addr, length, prot, flags, fd, offset)
fn mmap(env: &mut Env<'_>) -> Outcome {
    let offset = env.arg(5);
    map(env, offset.into())
}

/// mmap64(addr, length, prot, flags, fd, offset), with a 64-bit offset.
fn mmap64(env: &mut Env<'_>) -> Outcome {
    let offset = u64::from(env.arg(5)) | u64::from(env.arg(6)) << 32;
    map(env, offset)
}

fn map(env: &mut Env<'_>, offset: u64) -> Outcome {
    let (length, prot, flags) = (env.arg(1), env.arg(2), env.arg(3));
    if prot != PROT_READ || !matches!(flags, MAP_PRIVATE | MAP_SHARED) {
        return Outcome::Fail(format!("mmap with protection {prot:#x}, flags {flags:#x}"));
    }
    let Some(fd) = descriptor(env, env.arg(4)) else {
        return fail(env, EBADF, MAP_FAILED);
    };
    if length == 0 || !offset.is_multiple_of(u64::from(PAGE)) {
        return fail(env, EINVAL, MAP_FAILED);
    }
    // SAFETY: a descriptor the guest opened, open for this call.
    let fd = unsafe { BorrowedFd::borrow_raw(fd) };
    let memory = env.ext.get_or_default::<Memory>();
    match memory::map_shared_view(
        env.cpu,
        &mut env.sys.vm,
        memory,
        fd,
        offset,
        length,
        PAGE_READONLY,
    ) {
        Ok(base) => Outcome::Return(base),
        Err(_) => fail(env, ENOMEM, MAP_FAILED),
    }
}

/// munmap(addr, length): whole mappings only.
fn munmap(env: &mut Env<'_>) -> Outcome {
    let address = env.arg(0);
    let memory = env.ext.get_or_default::<Memory>();
    if memory::view_base(memory, address) != Some(address) {
        return fail(env, EINVAL, u32::MAX);
    }
    memory::remove_view(env.cpu, &mut env.sys.vm, memory, address);
    Outcome::Return(0)
}
