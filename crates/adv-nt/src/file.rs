//! File system calls, after Wine's `unix/file.c`: the platform finds the
//! host file and the server opens it and holds the handle.

mod information;
mod io;

pub use information::{query, set};
pub use io::{
    cancel, cancel_ex, cancel_sync, complete_async, create_mailslot, create_named_pipe,
    device_control, flush, flush_ex, lock, notify_change, read, read_scatter, unlock, write_gather,
};

use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

use adv_machine::{Env, Outcome};

use crate::object::{Attributes, write_bytes, write_u32};
use crate::path::{self, Lookup};
use crate::protocol::{CreateFileRequest, OpenFileObjectRequest};
use crate::server;
use crate::status::{self, Error};

const FILE_OPEN: u32 = 1;
const FILE_OVERWRITE: u32 = 4;
const FILE_OPEN_BY_FILE_ID: u32 = 0x2000;

/// IO_STATUS_BLOCK.Information after a create.
const FILE_SUPERSEDED: u32 = 0;
const FILE_OPENED: u32 = 1;
const FILE_CREATED: u32 = 2;
const FILE_OVERWRITTEN: u32 = 3;

/// The host side of the process's drives.
pub struct Files {
    /// `<prefix>/dosdevices`, where each drive is a link.
    pub dosdevices: PathBuf,
}

const FILE_SYNCHRONOUS_IO: u32 = 0x30;
const FD_TYPE_FILE: i32 = 1;
const FILE_USE_FILE_POINTER_POSITION: i64 = -2;
const FILE_WRITE_TO_END_OF_FILE: i64 = -1;

/// NtWriteFile(handle, event, apc, apc_context, *io, *buffer, length,
/// *offset, *key), synchronous writes only.
pub fn write(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[
        handle,
        event,
        apc,
        apc_context,
        io,
        buffer,
        length,
        offset,
        _key,
    ] = args
    else {
        unreachable!()
    };
    let fd = match server::handle_fd(env, handle) {
        Ok(fd) => fd,
        Err(Error::Status(status::BAD_DEVICE_TYPE)) => return io::server_write(env, args),
        Err(error) => return status::finish(Err(error)),
    };
    let result = (|| {
        let asynchronous = fd.options & FILE_SYNCHRONOUS_IO == 0;
        let mut position = None;
        if fd.kind == FD_TYPE_FILE && offset != 0 {
            let mut bytes = [0; 8];
            env.cpu
                .read(offset, &mut bytes)
                .map_err(|_| status::ACCESS_VIOLATION)?;
            let off = i64::from_le_bytes(bytes);
            if off != FILE_USE_FILE_POINTER_POSITION {
                position = Some(off);
            }
        }
        if asynchronous && fd.kind == FD_TYPE_FILE && position.is_none() {
            if event != 0 {
                io::event_op(env, event, 2)?;
            }
            return Err(status::INVALID_PARAMETER.into());
        }
        let mut bytes = vec![0; length as usize];
        env.cpu
            .read(buffer, &mut bytes)
            .map_err(|_| status::ACCESS_VIOLATION)?;
        let result = (|| {
            if position == Some(FILE_WRITE_TO_END_OF_FILE) {
                // SAFETY: a zeroed stat is a valid buffer for fstat to fill.
                let mut stat: libc::stat = unsafe { std::mem::zeroed() };
                // SAFETY: a descriptor the server gave the process and a valid buffer.
                if unsafe { libc::fstat(fd.fd, &mut stat) } != 0 {
                    return Err(write_error());
                }
                position = Some(stat.st_size);
            }
            loop {
                // SAFETY: the descriptor is valid for this call, and bytes holds length bytes.
                let count = unsafe {
                    match position {
                        Some(off) => libc::pwrite(fd.fd, bytes.as_ptr().cast(), bytes.len(), off),
                        None => libc::write(fd.fd, bytes.as_ptr().cast(), bytes.len()),
                    }
                };
                if count >= 0 {
                    if let Some(off) = position
                        && !asynchronous
                    {
                        // SAFETY: the same regular file; Wine advances its synchronous position.
                        unsafe { libc::lseek(fd.fd, off + count as i64, libc::SEEK_SET) };
                    }
                    return Ok(count as u32);
                }
                if std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
                    return Err(write_error());
                }
            }
        })();
        drop(fd);
        write_u32(
            env,
            io,
            result.as_ref().err().copied().unwrap_or(status::SUCCESS),
        )?;
        write_u32(env, io + 4, result.as_ref().copied().unwrap_or(0))?;
        let status = result.as_ref().err().copied().unwrap_or(status::SUCCESS);
        let count = result?;
        if event != 0 {
            io::event_op(env, event, 1)?;
        }
        if apc != 0 {
            let outcome = crate::sync::queue_apc(env, &[u32::MAX - 1, apc, apc_context, io, 0]);
            if let Outcome::Return(code) = outcome {
                status::check(code)?;
            } else {
                return Err(Error::Unsupported("queuing file write APC".into()));
            }
        }
        if apc == 0 && apc_context != 0 && asynchronous {
            io::add_completion(env, handle, apc_context, status, count, true)?;
        }
        Ok(if asynchronous { 0x103 } else { status })
    })();
    match result {
        Ok(status) => Outcome::Return(status),
        Err(error) => status::finish(Err(error)),
    }
}

/// Wine's errno_to_status for host file operations.
fn write_error() -> status::Status {
    match std::io::Error::last_os_error().raw_os_error() {
        Some(libc::EAGAIN) => 0xc000_0043, // STATUS_SHARING_VIOLATION
        Some(libc::EBADF) => status::INVALID_HANDLE,
        Some(libc::EBUSY) => 0x8000_0011,  // STATUS_DEVICE_BUSY
        Some(libc::ENOSPC) => 0xc000_007f, // STATUS_DISK_FULL
        Some(libc::EPERM | libc::EROFS | libc::EACCES) => 0xc000_0022, // STATUS_ACCESS_DENIED
        Some(libc::ENOTDIR) => status::OBJECT_PATH_NOT_FOUND,
        Some(libc::ENOENT) => status::OBJECT_NAME_NOT_FOUND,
        Some(libc::EMFILE | libc::ENFILE) => 0xc000_011f, // STATUS_TOO_MANY_OPENED_FILES
        Some(libc::ENOTEMPTY) => 0xc000_0101,             // STATUS_DIRECTORY_NOT_EMPTY
        Some(libc::ELOOP) => 0xc000_0280,                 // STATUS_REPARSE_POINT_NOT_RESOLVED
        Some(libc::EISDIR) => 0xc000_0010,                // STATUS_INVALID_DEVICE_REQUEST
        Some(libc::EINVAL) => status::INVALID_PARAMETER,
        Some(libc::EPIPE | libc::ECONNRESET) => 0xc000_00b0, // STATUS_PIPE_DISCONNECTED
        Some(libc::EIO) => 0xc000_00a3,                      // STATUS_DEVICE_NOT_READY
        Some(libc::ENXIO) => 0xc000_000e,                    // STATUS_NO_SUCH_DEVICE
        Some(libc::ENOTTY | libc::EOPNOTSUPP) => 0xc000_00bb, // STATUS_NOT_SUPPORTED
        Some(libc::EFAULT) => status::ACCESS_VIOLATION,
        Some(libc::ESPIPE) => 0xc000_00af, // STATUS_ILLEGAL_FUNCTION
        Some(libc::ETIME) => 0xc000_00b5,  // STATUS_IO_TIMEOUT
        _ => 0xc000_0001,                  // STATUS_UNSUCCESSFUL
    }
}

/// NtCreateFile(*handle, access, *attributes, *io, *allocation_size,
/// file_attributes, sharing, disposition, options, ea_buffer, ea_length)
pub fn create(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[
        handle,
        access,
        attributes,
        io,
        _,
        file_attributes,
        sharing,
        disposition,
        options,
        _,
        _,
    ] = args
    else {
        unreachable!()
    };
    let open = Open {
        handle,
        access,
        attributes,
        io,
        file_attributes,
        sharing,
        disposition,
        options,
    };
    status::finish(open.run(env))
}

/// NtOpenFile(*handle, access, *attributes, *io, sharing, options)
pub fn open(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[handle, access, attributes, io, sharing, options] = args else {
        unreachable!()
    };
    let open = Open {
        handle,
        access,
        attributes,
        io,
        file_attributes: 0,
        sharing,
        disposition: FILE_OPEN,
        options,
    };
    status::finish(open.run(env))
}

/// Opens an existing file by NT name for the platform itself.
pub fn open_path(
    env: &mut Env<'_>,
    name: Vec<u16>,
    access: u32,
    sharing: u32,
    options: u32,
) -> Result<u32, Error> {
    let attributes = Attributes {
        security: Vec::new(),
        root: 0,
        attributes: OBJ_CASE_INSENSITIVE,
        name: Some(name),
    };
    let dosdevices = &env.ext.get::<Files>().dosdevices;
    let host = match path::lookup(dosdevices, attributes.name.as_deref().unwrap_or_default())? {
        Lookup::Found(host) => host,
        Lookup::Missing(_) => return Err(status::OBJECT_NAME_NOT_FOUND.into()),
        Lookup::Device => return Err(Error::Unsupported("opening a device".into())),
    };
    let mut request = CreateFileRequest {
        access,
        sharing,
        create: FILE_OPEN as i32,
        options,
        ..Default::default()
    };
    let objattr = attributes.wire();
    let host = host.as_os_str().as_bytes();
    let (reply, _) = server::request(env, &mut request, &[&objattr, host], &mut [])?;
    status::check(reply.header.error)?;
    Ok(reply.handle)
}

const OBJ_CASE_INSENSITIVE: u32 = 0x40;

struct Open {
    handle: u32,
    access: u32,
    attributes: u32,
    io: u32,
    file_attributes: u32,
    sharing: u32,
    disposition: u32,
    options: u32,
}

impl Open {
    fn run(&self, env: &mut Env<'_>) -> Result<(), Error> {
        write_u32(env, self.handle, 0)?;
        let result = self.open(env);
        // Like Wine, the status block carries the status of every attempt.
        let status = match &result {
            Ok(_) => status::SUCCESS,
            Err(Error::Status(status)) => *status,
            Err(Error::Unsupported(_)) => return result.map(drop),
        };
        write_u32(env, self.io, status)?;
        let information = result?;
        write_u32(env, self.io + 4, information)?;
        Ok(())
    }

    /// Opens the file and returns the Information value.
    fn open(&self, env: &mut Env<'_>) -> Result<u32, Error> {
        let attributes = Attributes::read(env, self.attributes)?;
        if attributes.name.is_none() {
            return Err(status::INVALID_PARAMETER.into());
        }
        if self.options & FILE_OPEN_BY_FILE_ID != 0 {
            return Err(Error::Unsupported("opening a file by its id".into()));
        }
        let lookup = lookup_attributes(env, &attributes)?;
        let (host, created) = match lookup {
            Lookup::Found(host) => (host, false),
            Lookup::Missing(_) if matches!(self.disposition, FILE_OPEN | FILE_OVERWRITE) => {
                return Err(status::OBJECT_NAME_NOT_FOUND.into());
            }
            Lookup::Missing(host) => (host, true),
            Lookup::Device => {
                let mut request = OpenFileObjectRequest {
                    access: self.access,
                    attributes: attributes.attributes,
                    rootdir: attributes.root,
                    sharing: self.sharing,
                    options: self.options,
                    ..Default::default()
                };
                let (reply, _) =
                    server::request(env, &mut request, &[&attributes.name_bytes()], &mut [])?;
                status::check(reply.header.error)?;
                write_u32(env, self.handle, reply.handle)?;
                return Ok(FILE_OPENED);
            }
        };

        let mut request = CreateFileRequest {
            access: self.access,
            sharing: self.sharing,
            create: self.disposition as i32,
            options: self.options,
            attrs: self.file_attributes,
            ..Default::default()
        };
        let attributes = if attributes.root != 0 {
            Attributes {
                security: attributes.security,
                root: 0,
                attributes: attributes.attributes,
                name: Some(
                    path::nt_name(&env.ext.get::<Files>().dosdevices, &host)
                        .encode_utf16()
                        .collect(),
                ),
            }
        } else {
            attributes
        };
        let objattr = attributes.wire();
        let host = host.as_os_str().as_bytes();
        let (reply, _) = server::request(env, &mut request, &[&objattr, host], &mut [])?;
        status::check(reply.header.error)?;
        write_u32(env, self.handle, reply.handle)?;
        Ok(match self.disposition {
            _ if created => FILE_CREATED,
            0 => FILE_SUPERSEDED,
            2 => FILE_CREATED,
            4 | 5 => FILE_OVERWRITTEN,
            _ => FILE_OPENED,
        })
    }
}

const FILE_FS_VOLUME_INFORMATION: u32 = 1;
const FILE_FS_DEVICE_INFORMATION: u32 = 4;
const FILE_FS_ATTRIBUTE_INFORMATION: u32 = 5;
const FILE_CASE_PRESERVED_NAMES: u32 = 0x02;
const FILE_PERSISTENT_ACLS: u32 = 0x08;
/// Every volume's serial number, fixed as the machine's identity is.
const VOLUME_SERIAL: u32 = 0x4854_4f44;
const FILE_DEVICE_DISK: u32 = 0x07;
const FILE_DEVICE_NAMED_PIPE: u32 = 0x11;
const FILE_DEVICE_NULL: u32 = 0x15;
const FILE_DEVICE_UNKNOWN: u32 = 0x22;

/// NtQueryVolumeInformationFile(handle, *io, *buffer, length, class)
pub fn query_volume(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[handle, io, buffer, length, class] = args else {
        unreachable!()
    };
    status::finish(query_volume_in(env, handle, io, buffer, length, class))
}

fn query_volume_in(
    env: &mut Env<'_>,
    handle: u32,
    io: u32,
    buffer: u32,
    length: u32,
    class: u32,
) -> Result<(), Error> {
    let bytes = match class {
        // A session has no mount manager, so every drive is the volume one
        // reports for a directory: NTFS, without a label.
        FILE_FS_VOLUME_INFORMATION => {
            if length < 24 {
                return Err(status::INFO_LENGTH_MISMATCH.into());
            }
            server::handle_fd(env, handle)?;
            let mut bytes = [0u8; 18];
            bytes[8..12].copy_from_slice(&VOLUME_SERIAL.to_le_bytes());
            bytes.to_vec()
        }
        FILE_FS_ATTRIBUTE_INFORMATION => {
            if length < 16 {
                return Err(status::INFO_LENGTH_MISMATCH.into());
            }
            server::handle_fd(env, handle)?;
            let name: Vec<u8> = "NTFS".encode_utf16().flat_map(u16::to_le_bytes).collect();
            let name = &name[..name.len().min(length as usize - 12)];
            let mut bytes = Vec::with_capacity(12 + name.len());
            for word in [
                FILE_CASE_PRESERVED_NAMES | FILE_PERSISTENT_ACLS,
                255,
                name.len() as u32,
            ] {
                bytes.extend_from_slice(&word.to_le_bytes());
            }
            bytes.extend_from_slice(name);
            bytes
        }
        FILE_FS_DEVICE_INFORMATION => {
            if length < 8 {
                return Err(status::INFO_LENGTH_MISMATCH.into());
            }
            let fd = server::handle_fd(env, handle)?;
            let device = device_type(fd.fd);
            drop(fd);
            // No removable or read-only media.
            [device, 0]
                .iter()
                .flat_map(|word| word.to_le_bytes())
                .collect()
        }
        _ => {
            return Err(Error::Unsupported(format!(
                "volume information class {class}"
            )));
        }
    };
    write_bytes(env, buffer, &bytes)?;
    write_u32(env, io, status::SUCCESS)?;
    write_u32(env, io + 4, bytes.len() as u32)?;
    Ok(())
}

/// The NT device type of a host descriptor, as Wine's `get_device_info`
/// decides it on macOS.
fn device_type(fd: std::os::fd::RawFd) -> u32 {
    // SAFETY: a zeroed stat is a valid buffer for fstat to fill.
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: a descriptor the server gave the process, and a valid buffer.
    if unsafe { libc::fstat(fd, &mut stat) } != 0 {
        return FILE_DEVICE_UNKNOWN;
    }
    match stat.st_mode & libc::S_IFMT {
        // /dev/null is character device 3, 2.
        libc::S_IFCHR if stat.st_rdev == libc::makedev(3, 2) => FILE_DEVICE_NULL,
        libc::S_IFCHR => FILE_DEVICE_UNKNOWN,
        libc::S_IFIFO | libc::S_IFSOCK => FILE_DEVICE_NAMED_PIPE,
        _ => FILE_DEVICE_DISK,
    }
}

const FILE_ATTRIBUTE_READONLY: u32 = 0x01;
const FILE_ATTRIBUTE_HIDDEN: u32 = 0x02;
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
const FILE_ATTRIBUTE_ARCHIVE: u32 = 0x20;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
/// Seconds from 1601, where NT time starts, to 1970.
const SECONDS_1601_TO_1970: i64 = 11_644_473_600;

/// NtQueryAttributesFile(*attributes, *info): FILE_BASIC_INFORMATION of a
/// named file, as Wine's `get_file_info`.
pub fn query_attributes(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[attributes, info] = args else {
        unreachable!()
    };
    status::finish((|| {
        let (host, link, metadata) = named_file(env, attributes)?;
        let bytes = basic_information(&host, &link, &metadata);
        write_bytes(env, info, &bytes)?;
        Ok(())
    })())
}

/// NtQueryFullAttributesFile(*attributes, *info):
/// FILE_NETWORK_OPEN_INFORMATION, the times, sizes and attributes.
pub fn query_full_attributes(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[attributes, info] = args else {
        unreachable!()
    };
    status::finish((|| {
        use std::os::unix::fs::MetadataExt;
        let (host, link, metadata) = named_file(env, attributes)?;
        let basic = basic_information(&host, &link, &metadata);
        let mut bytes = [0u8; 56];
        bytes[..32].copy_from_slice(&basic[..32]);
        bytes[32..40].copy_from_slice(&(metadata.blocks() * 512).to_le_bytes());
        bytes[40..48].copy_from_slice(&metadata.size().to_le_bytes());
        bytes[48..52].copy_from_slice(&basic[32..36]);
        write_bytes(env, info, &bytes)?;
        Ok(())
    })())
}

/// The host file the OBJECT_ATTRIBUTES at `attributes` name, with its own
/// and its target's metadata.
fn named_file(
    env: &mut Env<'_>,
    attributes: u32,
) -> Result<(PathBuf, std::fs::Metadata, std::fs::Metadata), Error> {
    let attributes = Attributes::read(env, attributes)?;
    let Some(name) = &attributes.name else {
        return Err(status::INVALID_PARAMETER.into());
    };
    let host = match lookup_attributes(env, &attributes)? {
        Lookup::Found(host) => host,
        Lookup::Missing(_) => return Err(status::OBJECT_NAME_NOT_FOUND.into()),
        Lookup::Device => {
            return Err(Error::Unsupported(format!(
                "querying device {}",
                String::from_utf16_lossy(name)
            )));
        }
    };
    let link = std::fs::symlink_metadata(&host).map_err(|_| status::OBJECT_NAME_NOT_FOUND)?;
    let metadata = std::fs::metadata(&host).map_err(|_| status::OBJECT_NAME_NOT_FOUND)?;
    if !metadata.is_file() && !metadata.is_dir() {
        return Err(status::INVALID_INFO_CLASS.into());
    }
    Ok((host, link, metadata))
}

/// FILE_BASIC_INFORMATION: creation, access, write and change times, then
/// the attributes.
pub(crate) fn basic_information(
    host: &std::path::Path,
    link: &std::fs::Metadata,
    metadata: &std::fs::Metadata,
) -> [u8; 40] {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let ticks = |seconds: i64, nanoseconds: i64| {
        (seconds + SECONDS_1601_TO_1970) * 10_000_000 + nanoseconds / 100
    };
    let write = ticks(metadata.mtime(), metadata.mtime_nsec());
    let creation = metadata
        .created()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(write, |since| {
            ticks(since.as_secs() as i64, i64::from(since.subsec_nanos()))
        });
    let times = [
        creation,
        ticks(metadata.atime(), metadata.atime_nsec()),
        write,
        ticks(metadata.ctime(), metadata.ctime_nsec()),
    ];

    let mut attributes = if metadata.is_dir() {
        FILE_ATTRIBUTE_DIRECTORY
    } else {
        FILE_ATTRIBUTE_ARCHIVE
    };
    if metadata.permissions().mode() & 0o222 == 0 {
        attributes |= FILE_ATTRIBUTE_READONLY;
    }
    // A link to a directory is a reparse point.
    if link.file_type().is_symlink() && metadata.is_dir() {
        attributes |= FILE_ATTRIBUTE_REPARSE_POINT;
    }
    if let Some(dos) = information::path_dos_attributes(host) {
        attributes |= dos;
    } else if host
        .file_name()
        .is_some_and(|name| name.as_bytes().starts_with(b".") && name != "." && name != "..")
    {
        attributes |= FILE_ATTRIBUTE_HIDDEN;
    }

    let mut bytes = [0u8; 40];
    for (index, time) in times.into_iter().enumerate() {
        bytes[8 * index..8 * index + 8].copy_from_slice(&time.to_le_bytes());
    }
    bytes[32..36].copy_from_slice(&attributes.to_le_bytes());
    bytes
}

const FSCTL_LOCK_VOLUME: u32 = 0x9_0018;
const FSCTL_UNLOCK_VOLUME: u32 = 0x9_001c;
const FSCTL_IS_VOLUME_MOUNTED: u32 = 0x9_0028;
const FSCTL_GET_OBJECT_ID: u32 = 0x9_009c;
const FSCTL_SET_SPARSE: u32 = 0x9_00c4;
/// FILE_OBJECTID_BUFFER: the object id and three more ids.
const OBJECT_ID_BUFFER_SIZE: u32 = 64;

/// NtFsControlFile(handle, event, apc, apc_context, *io, code, *in, in_size,
/// *out, out_size)
pub fn control(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[handle, _, _, _, io, code, _, _, out, out_size] = args else {
        unreachable!()
    };
    if io == 0 {
        return Outcome::Return(status::INVALID_PARAMETER);
    }
    let (status, size) = match code {
        FSCTL_GET_OBJECT_ID if out_size < OBJECT_ID_BUFFER_SIZE => {
            return Outcome::Return(status::BUFFER_TOO_SMALL);
        }
        FSCTL_GET_OBJECT_ID => match object_id(env, handle, out) {
            Ok(()) => (status::SUCCESS, OBJECT_ID_BUFFER_SIZE),
            Err(error) => return status::finish(Err(error)),
        },
        // Wine succeeds without doing anything.
        FSCTL_LOCK_VOLUME | FSCTL_UNLOCK_VOLUME | FSCTL_IS_VOLUME_MOUNTED | FSCTL_SET_SPARSE => {
            (status::SUCCESS, 0)
        }
        _ => return io::server_ioctl(env, args),
    };
    status::finish((|| {
        write_u32(env, io, status)?;
        write_u32(env, io + 4, size)?;
        Ok(())
    })())
}

/// The host device and inode identify the file, as in Wine.
fn object_id(env: &mut Env<'_>, handle: u32, out: u32) -> Result<(), Error> {
    let fd = server::handle_fd(env, handle)?;
    // SAFETY: a zeroed stat is a valid buffer for fstat to fill.
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: a descriptor the server gave the process, and a valid buffer.
    if unsafe { libc::fstat(fd.fd, &mut stat) } != 0 {
        return Err(Error::Unsupported("fstat failed on a server file".into()));
    }
    drop(fd);
    let mut bytes = [0u8; OBJECT_ID_BUFFER_SIZE as usize];
    let device = stat.st_dev.to_le_bytes();
    bytes[..device.len()].copy_from_slice(&device);
    bytes[8..16].copy_from_slice(&stat.st_ino.to_le_bytes());
    Ok(write_bytes(env, out, &bytes)?)
}

fn lookup_attributes(env: &mut Env<'_>, attributes: &Attributes) -> Result<Lookup, Error> {
    let name = attributes.name.as_deref().unwrap_or_default();
    if attributes.root == 0 {
        return Ok(path::lookup(&env.ext.get::<Files>().dosdevices, name)?);
    }
    if name.first() == Some(&(b'\\' as u16)) {
        return Err(status::INVALID_PARAMETER.into());
    }
    path::validate_components(&String::from_utf16_lossy(name), false)?;
    let root = information::unix_name(env, attributes.root)?;
    let dosdevices = &env.ext.get::<Files>().dosdevices;
    let mut full = path::nt_name(dosdevices, &root)
        .encode_utf16()
        .collect::<Vec<_>>();
    full.push(b'\\' as u16);
    full.extend_from_slice(name);
    Ok(path::lookup(dosdevices, &full)?)
}

/// Opens NtLoadKey's hive with Wine's read access and sharing mode.
pub(crate) fn open_registry_file(env: &mut Env<'_>, address: u32) -> Result<u32, Error> {
    let mut attributes = Attributes::read(env, address)?;
    let host = match lookup_attributes(env, &attributes)? {
        Lookup::Found(host) => host,
        Lookup::Missing(_) => return Err(status::OBJECT_NAME_NOT_FOUND.into()),
        Lookup::Device => return Err(status::BAD_DEVICE_TYPE.into()),
    };
    if attributes.root != 0 {
        attributes.root = 0;
        attributes.name = Some(
            path::nt_name(&env.ext.get::<Files>().dosdevices, &host)
                .encode_utf16()
                .collect(),
        );
    }
    let mut request = CreateFileRequest {
        access: 0x8010_0000, // GENERIC_READ | SYNCHRONIZE
        create: FILE_OPEN as i32,
        ..Default::default()
    };
    let (reply, _) = server::request(
        env,
        &mut request,
        &[&attributes.wire(), host.as_os_str().as_bytes()],
        &mut [],
    )?;
    status::check(reply.header.error)?;
    Ok(reply.handle)
}

/// NtDeleteFile(*attributes): open with delete-on-close, then close.
pub fn delete(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    status::finish((|| {
        let attributes = Attributes::read(env, args[0])?;
        let host = match lookup_attributes(env, &attributes)? {
            Lookup::Found(host) => host,
            Lookup::Missing(_) => return Err(status::OBJECT_NAME_NOT_FOUND.into()),
            Lookup::Device => return Err(status::BAD_DEVICE_TYPE.into()),
        };
        let mut request = CreateFileRequest {
            access: 0xc001_0000, // GENERIC_READ | GENERIC_WRITE | DELETE
            sharing: 7,
            create: FILE_OPEN as i32,
            options: 0x1000, // FILE_DELETE_ON_CLOSE
            ..Default::default()
        };
        let attributes = Attributes {
            root: 0,
            name: Some(
                path::nt_name(&env.ext.get::<Files>().dosdevices, &host)
                    .encode_utf16()
                    .collect(),
            ),
            ..attributes
        };
        let (reply, _) = server::request(
            env,
            &mut request,
            &[&attributes.wire(), host.as_os_str().as_bytes()],
            &mut [],
        )?;
        status::check(reply.header.error)?;
        server::close(env, reply.handle)
    })())
}

/// Wine's EA query semi-stub validates the handle and clears the buffer.
pub fn query_ea(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let result = (|| {
        server::handle_fd(env, args[0])?;
        if args[2] != 0 && args[3] != 0 {
            write_bytes(env, args[2], &vec![0; args[3] as usize])?;
        }
        Err(0xc000_0052u32.into()) // STATUS_NO_EAS_ON_FILE
    })();
    status::finish(result)
}

/// Wine's NtSetEaFile stub.
pub fn set_ea(_: &mut Env<'_>, _: &[u32]) -> Outcome {
    Outcome::Return(0xc000_0022) // STATUS_ACCESS_DENIED
}

/// Wine's NtSetVolumeInformationFile stub leaves the status block untouched.
pub fn set_volume(_: &mut Env<'_>, _: &[u32]) -> Outcome {
    Outcome::Return(status::SUCCESS)
}
