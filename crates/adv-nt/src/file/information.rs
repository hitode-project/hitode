//! File information, after Wine's `unix/file.c`.

use std::os::fd::FromRawFd;
use std::os::unix::fs::{MetadataExt, PermissionsExt};

use super::*;
use crate::object::read_u32;
use crate::protocol::{
    GetFileInfoRequest, GetHandleUnixNameRequest, GetObjectNameRequest, OpenFileObjectRequest,
    SetFdDispInfoRequest, SetFdEofInfoRequest, SetFdNameInfoRequest,
};

const FILE_BASIC_INFORMATION: u32 = 4;
const FILE_STANDARD_INFORMATION: u32 = 5;
const FILE_INTERNAL_INFORMATION: u32 = 6;
const FILE_EA_INFORMATION: u32 = 7;
const FILE_ACCESS_INFORMATION: u32 = 8;
const FILE_NAME_INFORMATION: u32 = 9;
const FILE_RENAME_INFORMATION: u32 = 10;
const FILE_DISPOSITION_INFORMATION: u32 = 13;
const FILE_POSITION_INFORMATION: u32 = 14;
const FILE_MODE_INFORMATION: u32 = 16;
const FILE_ALIGNMENT_INFORMATION: u32 = 17;
const FILE_ALL_INFORMATION: u32 = 18;
const FILE_END_OF_FILE_INFORMATION: u32 = 20;
const FILE_PIPE_INFORMATION: u32 = 23;
const FILE_PIPE_LOCAL_INFORMATION: u32 = 24;
const FILE_NETWORK_OPEN_INFORMATION: u32 = 34;
const FILE_ATTRIBUTE_TAG_INFORMATION: u32 = 35;
const FILE_ID_INFORMATION: u32 = 59;
const FILE_STAT_INFORMATION: u32 = 68;
const FILE_DISPOSITION_INFORMATION_EX: u32 = 64;

static START_UMASK: std::sync::LazyLock<libc::mode_t> = std::sync::LazyLock::new(|| {
    // SAFETY: permission bits only; restore the process mask immediately, as Wine does.
    unsafe {
        let mask = libc::umask(0o777);
        libc::umask(mask);
        mask
    }
});

const BUFFER_OVERFLOW: u32 = 0x8000_0005;
const INVALID_PARAMETER_3: u32 = 0xc000_00f1;

pub(super) fn read_i64(env: &Env<'_>, address: u32) -> Result<i64, Error> {
    let mut bytes = [0; 8];
    env.cpu
        .read(address, &mut bytes)
        .map_err(|_| status::ACCESS_VIOLATION)?;
    Ok(i64::from_le_bytes(bytes))
}

/// NtQueryInformationFile(handle, *io, *buffer, length, class).
pub fn query(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[handle, io, buffer, length, class] = args else {
        unreachable!()
    };
    status::finish((|| {
        write_u32(env, io + 4, 0)?;
        let result = query_in(env, handle, buffer, length, class);
        match result {
            Ok((status, size)) => {
                write_u32(env, io + 4, size)?;
                write_u32(env, io, status)?;
                status::check(status)
            }
            Err(Error::Status(status)) => {
                write_u32(env, io, status)?;
                Err(status.into())
            }
            Err(error) => Err(error),
        }
    })())
}

fn query_server(
    env: &mut Env<'_>,
    handle: u32,
    buffer: u32,
    length: u32,
    class: u32,
) -> Result<(u32, u32), Error> {
    let mut request = GetFileInfoRequest {
        handle,
        info_class: class,
        ..Default::default()
    };
    let mut bytes = vec![0; length as usize];
    let (reply, size) = server::request(env, &mut request, &[], &mut bytes)?;
    write_bytes(env, buffer, &bytes[..size])?;
    Ok((reply.header.error, size as u32))
}

fn query_in(
    env: &mut Env<'_>,
    handle: u32,
    buffer: u32,
    length: u32,
    class: u32,
) -> Result<(u32, u32), Error> {
    let size = match class {
        FILE_BASIC_INFORMATION => 40,
        FILE_STANDARD_INFORMATION => 24,
        FILE_INTERNAL_INFORMATION | FILE_POSITION_INFORMATION | FILE_END_OF_FILE_INFORMATION => 8,
        FILE_EA_INFORMATION | FILE_ALIGNMENT_INFORMATION => 4,
        FILE_NAME_INFORMATION => 8,
        FILE_ALL_INFORMATION => 104,
        FILE_STAT_INFORMATION => 72,
        FILE_PIPE_INFORMATION => 8,
        FILE_PIPE_LOCAL_INFORMATION => 40,
        FILE_NETWORK_OPEN_INFORMATION => 56,
        FILE_ATTRIBUTE_TAG_INFORMATION => 8,
        FILE_ID_INFORMATION => 24,
        FILE_ACCESS_INFORMATION | FILE_MODE_INFORMATION => {
            return query_server(env, handle, buffer, length, class);
        }
        0 | 84.. => return Err(status::INVALID_INFO_CLASS.into()),
        _ => {
            return Err(Error::Unsupported(format!(
                "file information class {class}"
            )));
        }
    };
    if length < size {
        return Err(status::INFO_LENGTH_MISMATCH.into());
    }
    let fd = match server::handle_fd(env, handle) {
        Ok(fd) => fd,
        Err(Error::Status(status::BAD_DEVICE_TYPE)) => {
            return query_server(env, handle, buffer, length, class);
        }
        Err(error) => return Err(error),
    };
    if matches!(class, FILE_PIPE_INFORMATION | FILE_PIPE_LOCAL_INFORMATION) {
        drop(fd);
        return query_server(env, handle, buffer, length, class);
    }
    if class == FILE_ALIGNMENT_INFORMATION {
        // Wine checks the size and fd, then returns STATUS_NOT_IMPLEMENTED.
        return Err(status::NOT_IMPLEMENTED.into());
    }
    if class == FILE_NAME_INFORMATION {
        drop(fd);
        return name_info(env, handle, buffer, length - 4).map(|(status, size)| (status, size + 4));
    }
    let mut bytes = vec![0; size as usize];
    env.cpu
        .read(buffer, &mut bytes)
        .map_err(|_| status::ACCESS_VIOLATION)?;
    if class == FILE_POSITION_INFORMATION {
        // SAFETY: the server's descriptor is live for this call.
        let position = unsafe { libc::lseek(fd.fd, 0, libc::SEEK_CUR) };
        if position == -1 {
            return Err(write_error().into());
        }
        bytes[..8].copy_from_slice(&position.to_le_bytes());
    } else if class == FILE_EA_INFORMATION {
        bytes.fill(0);
    } else {
        // SAFETY: ManuallyDrop borrows the descriptor without closing its owner's fd.
        let file = std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(fd.fd) });
        let metadata = file.metadata().map_err(|_| write_error())?;
        if class == FILE_BASIC_INFORMATION && !metadata.is_file() && !metadata.is_dir() {
            return Err(status::INVALID_INFO_CLASS.into());
        }
        let mounts = volume::mounts(env);
        let optical = mounts.metadata(&metadata);
        let mut basic = optical.as_ref().map_or_else(
            || basic_information(std::path::Path::new(""), &metadata, &metadata),
            |entry| entry.basic(),
        );
        let mut attributes =
            u32::from_le_bytes(basic[32..36].try_into().unwrap()) | dos_attributes(fd.fd);
        let mut reparse_tag = 0;
        let mut reparse = [0; 16_384];
        let reparse_size = get_xattr(fd.fd, c"user.WINEREPARSE", &mut reparse);
        if reparse_size >= 4 {
            attributes |= FILE_ATTRIBUTE_REPARSE_POINT;
            reparse_tag = u32::from_le_bytes(reparse[..4].try_into().unwrap());
        }
        if fd.options & 0x20_0000 != 0 && metadata.is_dir() {
            // SAFETY: stat is a valid output buffer and fd names a live directory.
            let mut parent: libc::stat = unsafe { std::mem::zeroed() };
            // SAFETY: a live directory descriptor and NUL-terminated relative path.
            if unsafe { libc::fstatat(fd.fd, c"..".as_ptr(), &mut parent, 0) } == 0
                && (parent.st_dev as u64 != metadata.dev() || parent.st_ino == metadata.ino())
            {
                attributes |= FILE_ATTRIBUTE_REPARSE_POINT;
                reparse_tag = 0xa000_0003u32;
            }
        }
        basic[32..36].copy_from_slice(&attributes.to_le_bytes());
        let mut standard = [0; 24];
        if !metadata.is_dir() {
            standard[..8].copy_from_slice(
                &optical
                    .as_ref()
                    .map_or(metadata.blocks() * 512, |entry| entry.allocated)
                    .to_le_bytes(),
            );
            standard[8..16].copy_from_slice(&metadata.size().to_le_bytes());
        }
        let links = if metadata.is_dir() {
            1
        } else {
            metadata.nlink() as u32
        };
        standard[16..20].copy_from_slice(&links.to_le_bytes());
        standard[21] = u8::from(metadata.is_dir());
        match class {
            FILE_BASIC_INFORMATION => bytes[..36].copy_from_slice(&basic[..36]),
            FILE_STANDARD_INFORMATION => bytes[..22].copy_from_slice(&standard[..22]),
            FILE_INTERNAL_INFORMATION => bytes.copy_from_slice(&metadata.ino().to_le_bytes()),
            FILE_END_OF_FILE_INFORMATION => bytes.copy_from_slice(&standard[8..16]),
            FILE_ALL_INFORMATION => {
                bytes[..36].copy_from_slice(&basic[..36]);
                bytes[40..62].copy_from_slice(&standard[..22]);
                bytes[64..72].copy_from_slice(&metadata.ino().to_le_bytes());
                bytes[72..80].fill(0);
                bytes[88..92].fill(0);
                // SAFETY: the server's descriptor is live for this call.
                let position = unsafe { libc::lseek(fd.fd, 0, libc::SEEK_CUR) };
                bytes[80..88].copy_from_slice(&position.to_le_bytes());
                bytes[92..96].copy_from_slice(&1u32.to_le_bytes());
            }
            FILE_NETWORK_OPEN_INFORMATION => {
                bytes[..32].copy_from_slice(&basic[..32]);
                bytes[32..48].copy_from_slice(&standard[..16]);
                bytes[48..52].copy_from_slice(&basic[32..36]);
            }
            FILE_ATTRIBUTE_TAG_INFORMATION => {
                bytes[..4].copy_from_slice(&basic[32..36]);
                bytes[4..8].copy_from_slice(&reparse_tag.to_le_bytes());
            }
            FILE_STAT_INFORMATION => {
                bytes[..8].copy_from_slice(&metadata.ino().to_le_bytes());
                bytes[8..40].copy_from_slice(&basic[..32]);
                bytes[40..56].copy_from_slice(&standard[..16]);
                bytes[56..60].copy_from_slice(&basic[32..36]);
                bytes[60..64].copy_from_slice(&reparse_tag.to_le_bytes());
                bytes[64..68].copy_from_slice(&links.to_le_bytes());
                bytes[68..72].copy_from_slice(&0x1f01ffu32.to_le_bytes());
            }
            FILE_ID_INFORMATION => {
                bytes.fill(0);
                bytes[..8].copy_from_slice(
                    &mounts
                        .info(fd.fd)
                        .map(|info| u64::from(info.serial))
                        .map_or_else(|| volume_serial(env), Ok)?
                        .to_le_bytes(),
                );
                bytes[8..16].copy_from_slice(&metadata.ino().to_le_bytes());
            }
            _ => unreachable!(),
        }
    }
    drop(fd);
    write_bytes(env, buffer, &bytes)?;
    if class == FILE_ALL_INFORMATION {
        let (status, size) = name_info(env, handle, buffer + 96, length - 100)?;
        return Ok((status, size + 100));
    }
    Ok((status::SUCCESS, size))
}

fn volume_serial(env: &mut Env<'_>) -> Result<u64, Error> {
    let name: Vec<u8> = r"\Device\MountPointManager"
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    let mut request = OpenFileObjectRequest {
        access: 0x8010_0000,
        attributes: OBJ_CASE_INSENSITIVE,
        sharing: 3,
        options: 0x20,
        ..Default::default()
    };
    let (reply, _) = server::request(env, &mut request, &[&name], &mut [])?;
    if reply.header.error != status::SUCCESS {
        // Wine leaves the serial zero when mountmgr cannot be opened.
        return Ok(0);
    }
    server::close(env, reply.handle)?;
    Err(Error::Unsupported(
        "mount manager filesystem information".into(),
    ))
}

fn name_info(
    env: &mut Env<'_>,
    handle: u32,
    buffer: u32,
    length: u32,
) -> Result<(u32, u32), Error> {
    let mut capacity = 1024;
    let name = loop {
        let mut request = GetObjectNameRequest {
            handle,
            ..Default::default()
        };
        let mut bytes = vec![0; capacity];
        let (reply, size) = server::request(env, &mut request, &[], &mut bytes)?;
        if matches!(
            reply.header.error,
            status::INFO_LENGTH_MISMATCH | BUFFER_OVERFLOW
        ) {
            capacity = reply.total as usize;
            continue;
        }
        status::check(reply.header.error)?;
        bytes.truncate(size);
        break bytes;
    };
    let mut units = name
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| u16::from_le_bytes(*b))
        .peekable();
    for _ in 0..2 {
        while units.peek() == Some(&92) {
            units.next();
        }
        while units.peek().is_some_and(|c| *c != 92) {
            units.next();
        }
    }
    let name: Vec<u8> = units.flat_map(u16::to_le_bytes).collect();
    write_u32(env, buffer, name.len() as u32)?;
    let size = length.min(name.len() as u32);
    write_bytes(env, buffer + 4, &name[..size as usize])?;
    let status = if length < name.len() as u32 {
        BUFFER_OVERFLOW
    } else if name.is_empty() {
        status::INVALID_INFO_CLASS
    } else {
        status::SUCCESS
    };
    Ok((
        status,
        if status == status::INVALID_INFO_CLASS {
            length
        } else {
            size
        },
    ))
}

pub(super) fn unix_name(env: &mut Env<'_>, handle: u32) -> Result<PathBuf, Error> {
    let mut capacity = 1024;
    loop {
        let mut request = GetHandleUnixNameRequest {
            handle,
            ..Default::default()
        };
        let mut bytes = vec![0; capacity];
        let (reply, size) = server::request(env, &mut request, &[], &mut bytes)?;
        if reply.header.error == BUFFER_OVERFLOW {
            capacity = reply.name_len as usize;
            continue;
        }
        status::check(reply.header.error)?;
        bytes.truncate(size);
        return Ok(std::ffi::OsStr::from_bytes(&bytes).into());
    }
}

/// NtSetInformationFile(handle, *io, *buffer, length, class).
pub fn set(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[handle, io, buffer, length, class] = args else {
        unreachable!()
    };
    status::finish((|| {
        let result = set_in(env, handle, buffer, length, class);
        let status = match &result {
            Ok(()) => status::SUCCESS,
            Err(Error::Status(status)) => *status,
            Err(Error::Unsupported(_)) => return result,
        };
        write_u32(env, io, status)?;
        write_u32(env, io + 4, 0)?;
        result
    })())
}

fn set_in(
    env: &mut Env<'_>,
    handle: u32,
    buffer: u32,
    length: u32,
    class: u32,
) -> Result<(), Error> {
    let size = match class {
        FILE_BASIC_INFORMATION => 40,
        30 => 8,
        FILE_POSITION_INFORMATION | FILE_END_OF_FILE_INFORMATION => 8,
        FILE_DISPOSITION_INFORMATION => 1,
        FILE_DISPOSITION_INFORMATION_EX => 4,
        FILE_RENAME_INFORMATION => 16,
        _ => {
            return Err(Error::Unsupported(format!(
                "set file information class {class}"
            )));
        }
    };
    if length < size {
        return Err(INVALID_PARAMETER_3.into());
    }
    if !matches!(class, FILE_POSITION_INFORMATION | 30) {
        let fd = server::handle_fd(env, handle)?;
        if volume::mounts(env).info(fd.fd).is_some() {
            return Err(volume::WRITE_PROTECTED.into());
        }
    }
    match class {
        30 => {
            let mut request = crate::protocol::SetCompletionInfoRequest {
                handle,
                chandle: read_u32(env, buffer)?,
                ckey: u64::from(read_u32(env, buffer + 4)?),
                ..Default::default()
            };
            let (reply, _) = server::request(env, &mut request, &[], &mut [])?;
            status::check(reply.header.error)?;
        }
        FILE_POSITION_INFORMATION => {
            let offset = read_i64(env, buffer)?;
            let fd = server::handle_fd(env, handle)?;
            // SAFETY: a live descriptor, with an offset supplied by the guest.
            if unsafe { libc::lseek(fd.fd, offset, libc::SEEK_SET) } == -1 {
                return Err(write_error().into());
            }
        }
        FILE_END_OF_FILE_INFORMATION => {
            let mut request = SetFdEofInfoRequest {
                handle,
                eof: read_i64(env, buffer)? as u64,
                ..Default::default()
            };
            let (reply, _) = server::request(env, &mut request, &[], &mut [])?;
            status::check(reply.header.error)?;
        }
        FILE_DISPOSITION_INFORMATION | FILE_DISPOSITION_INFORMATION_EX => {
            let flags = if class == FILE_DISPOSITION_INFORMATION {
                let mut byte = [0];
                env.cpu
                    .read(buffer, &mut byte)
                    .map_err(|_| status::ACCESS_VIOLATION)?;
                u32::from(byte[0] != 0)
            } else {
                read_u32(env, buffer)?
            };
            if let Ok(host) = unix_name(env, handle) {
                path::invalidate(&host);
            }
            let mut request = SetFdDispInfoRequest {
                handle,
                flags,
                ..Default::default()
            };
            let (reply, _) = server::request(env, &mut request, &[], &mut [])?;
            status::check(reply.header.error)?;
        }
        FILE_BASIC_INFORMATION => {
            let atime = read_i64(env, buffer + 8)?;
            let mtime = read_i64(env, buffer + 16)?;
            let attr = read_u32(env, buffer + 32)?;
            let fd = server::handle_fd(env, handle)?;
            let times = [atime, mtime].map(|time| {
                if time == 0 || time == -1 {
                    libc::timespec {
                        tv_sec: 0,
                        tv_nsec: libc::UTIME_OMIT,
                    }
                } else {
                    libc::timespec {
                        tv_sec: time / 10_000_000 - SECONDS_1601_TO_1970,
                        tv_nsec: (time % 10_000_000 * 100) as _,
                    }
                }
            });
            // SAFETY: a live descriptor and two valid timespecs.
            if times.iter().any(|time| time.tv_nsec != libc::UTIME_OMIT)
                && unsafe { libc::futimens(fd.fd, times.as_ptr()) } == -1
            {
                return Err(write_error().into());
            }
            // SAFETY: the File borrows fd and will not close it.
            let file = std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(fd.fd) });
            let metadata = file.metadata().map_err(|_| write_error())?;
            let mut mode = metadata.permissions().mode();
            if attr & FILE_ATTRIBUTE_READONLY != 0 {
                if !metadata.is_dir() {
                    mode &= !0o222;
                }
            } else {
                mode |= (0o600 | ((mode & 0o44) >> 1)) & !u32::from(*START_UMASK);
            }
            // SAFETY: the descriptor remains live.
            if unsafe { libc::fchmod(fd.fd, mode as _) } == -1 {
                return Err(write_error().into());
            }
            let hidden_name = match unix_name(env, handle) {
                Ok(name) => name.file_name().is_some_and(|name| {
                    name.as_bytes().starts_with(b".") && name != "." && name != ".."
                }),
                Err(Error::Status(_)) => false,
                Err(error) => return Err(error),
            };
            set_dos_attributes(fd.fd, attr, hidden_name || metadata.nlink() > 1);
        }
        FILE_RENAME_INFORMATION => {
            let flags = read_u32(env, buffer)? & 0xff;
            let rootdir = read_u32(env, buffer + 4)?;
            let name_len = read_u32(env, buffer + 8)?;
            let mut name = vec![0; name_len as usize];
            env.cpu
                .read(buffer + 12, &mut name)
                .map_err(|_| status::ACCESS_VIOLATION)?;
            let units: Vec<_> = name
                .as_chunks::<2>()
                .0
                .iter()
                .map(|b| u16::from_le_bytes(*b))
                .collect();
            let host = if rootdir != 0 {
                let root = unix_name(env, rootdir)?;
                let nt = super::nt_name(env, &root);
                let full: Vec<_> = (nt + "\\" + &String::from_utf16_lossy(&units))
                    .encode_utf16()
                    .collect();
                lookup_rename(env, &full)?
            } else {
                lookup_rename(env, &units)?
            };
            if let Ok(source) = unix_name(env, handle) {
                path::invalidate(&source);
            }
            path::invalidate(&host);
            let mut request = SetFdNameInfoRequest {
                handle,
                rootdir,
                namelen: name_len,
                link: 0,
                flags: u32::from(flags != 0),
                ..Default::default()
            };
            let (reply, _) = server::request(
                env,
                &mut request,
                &[&name, host.as_os_str().as_bytes()],
                &mut [],
            )?;
            status::check(reply.header.error)?;
        }
        _ => unreachable!(),
    }
    Ok(())
}

fn lookup_rename(env: &mut Env<'_>, name: &[u16]) -> Result<PathBuf, Error> {
    let lookup = super::lookup_name(env, name)?;
    if let Lookup::Found(host) | Lookup::Missing(host) = &lookup
        && volume::mounts(env).read_only(host)
    {
        return Err(volume::WRITE_PROTECTED.into());
    }
    match lookup {
        Lookup::Found(host) | Lookup::Missing(host) => Ok(host),
        Lookup::Device => Err(Error::Unsupported("renaming a device".into())),
    }
}

const DOS_ATTRIB: &std::ffi::CStr = c"user.DOSATTRIB";

fn get_xattr(fd: i32, name: &std::ffi::CStr, bytes: &mut [u8]) -> isize {
    // SAFETY: a live descriptor, a NUL-terminated name, and a writable buffer.
    #[cfg(target_vendor = "apple")]
    unsafe {
        libc::fgetxattr(
            fd,
            name.as_ptr(),
            bytes.as_mut_ptr().cast(),
            bytes.len(),
            0,
            0,
        )
    }
    #[cfg(not(target_vendor = "apple"))]
    unsafe {
        libc::fgetxattr(fd, name.as_ptr(), bytes.as_mut_ptr().cast(), bytes.len())
    }
}

fn dos_attributes(fd: i32) -> u32 {
    let mut bytes = [0; 64];
    let size = get_xattr(fd, DOS_ATTRIB, &mut bytes);
    if size < 0 {
        return 0;
    }
    parse_dos_attributes(&bytes[..size as usize])
}

fn parse_dos_attributes(bytes: &[u8]) -> u32 {
    std::str::from_utf8(bytes)
        .ok()
        .and_then(|s| s.strip_prefix("0x"))
        .and_then(|s| u32::from_str_radix(s, 16).ok())
        .unwrap_or(0)
        & 6
}

pub(super) fn path_dos_attributes(path: &std::path::Path) -> Option<u32> {
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut bytes = [0; 64];
    // SAFETY: two NUL-terminated strings and a writable buffer.
    #[cfg(target_vendor = "apple")]
    let size = unsafe {
        libc::getxattr(
            path.as_ptr(),
            DOS_ATTRIB.as_ptr(),
            bytes.as_mut_ptr().cast(),
            bytes.len(),
            0,
            0,
        )
    };
    #[cfg(not(target_vendor = "apple"))]
    let size = unsafe {
        libc::getxattr(
            path.as_ptr(),
            DOS_ATTRIB.as_ptr(),
            bytes.as_mut_ptr().cast(),
            bytes.len(),
        )
    };
    (size >= 0).then(|| parse_dos_attributes(&bytes[..size as usize]))
}

fn set_dos_attributes(fd: i32, attr: u32, force: bool) {
    if attr & 6 == 0 && !force {
        // SAFETY: a live descriptor and NUL-terminated attribute name.
        #[cfg(target_vendor = "apple")]
        unsafe {
            libc::fremovexattr(fd, DOS_ATTRIB.as_ptr(), 0);
        }
        #[cfg(not(target_vendor = "apple"))]
        unsafe {
            libc::fremovexattr(fd, DOS_ATTRIB.as_ptr());
        }
        return;
    }
    let data = format!("0x{:x}", attr & 6);
    // Wine treats failure to persist DOS attributes as nonfatal.
    // SAFETY: a live descriptor, a NUL-terminated name, and readable data.
    #[cfg(target_vendor = "apple")]
    unsafe {
        libc::fsetxattr(
            fd,
            DOS_ATTRIB.as_ptr(),
            data.as_ptr().cast(),
            data.len(),
            0,
            0,
        );
    }
    #[cfg(not(target_vendor = "apple"))]
    unsafe {
        libc::fsetxattr(fd, DOS_ATTRIB.as_ptr(), data.as_ptr().cast(), data.len(), 0);
    }
}
