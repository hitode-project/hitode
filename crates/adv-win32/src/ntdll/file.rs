//! Native synchronous FILE_OPEN shares the Win32 VFS and FileObj owner.
use std::io::{Seek, SeekFrom};

use adv_abi::{GuestPtr, Handle, Registry, guest::check_range, warn_unavailable, win32};
use adv_cpu::backend::Prot;
use adv_machine::Env;

use crate::{FileObj, Win32, util};

mod attributes;
pub(crate) mod enumeration;
pub(crate) mod metadata;
pub(crate) mod write;

const ACCESS_VIOLATION: i32 = 0xc000_0005u32 as i32;
const INVALID_HANDLE: i32 = 0xc000_0008u32 as i32;
const INVALID_PARAMETER: i32 = 0xc000_000du32 as i32;
const ACCESS_DENIED: i32 = 0xc000_0022u32 as i32;
const NAME_INVALID: i32 = 0xc000_0033u32 as i32;
const NAME_NOT_FOUND: i32 = 0xc000_0034u32 as i32;
const PATH_NOT_FOUND: i32 = 0xc000_003au32 as i32;
const SHARING_VIOLATION: i32 = 0xc000_0043u32 as i32;
const INSUFFICIENT_RESOURCES: i32 = 0xc000_009au32 as i32;
const FILE_IS_A_DIRECTORY: i32 = 0xc000_00bau32 as i32;
const NOT_SUPPORTED: i32 = 0xc000_00bbu32 as i32;
const SYNCHRONIZE: u32 = 0x0010_0000;
const GENERIC_READ: u32 = 0x8000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;
const READ_ACCESS: u32 = GENERIC_READ | SYNCHRONIZE | 0x0002_0000 | 0x89;
const WRITE_ACCESS: u32 = GENERIC_WRITE | SYNCHRONIZE | 0x0002_0000 | 0x116;

pub(super) fn register(registry: &mut Registry) {
    registry.register(create_ABI);
    registry.register(query_ABI);
    registry.register(set_ABI);
    registry.register(read_ABI);
    registry.register(close_ABI);
    attributes::register(registry);
    enumeration::register(registry);
    metadata::register(registry);
    write::register(registry);
    // Nt/Zw user-mode names have the same declarations and behavior.
    for (api, name) in [
        (create_ABI, "NtCreateFile"),
        (query_ABI, "NtQueryInformationFile"),
        (set_ABI, "NtSetInformationFile"),
        (read_ABI, "NtReadFile"),
    ] {
        registry.register(adv_abi::Api { name, ..api });
    }
}

#[win32(ntdll, name = "ZwClose", level = implement)]
pub(crate) fn close(env: &mut Env<'_>, handle: Handle) -> i32 {
    // Native pseudo-process/thread closes succeed without releasing an owned
    // reference. This does not apply to Win32's legacy stdio constants.
    if matches!(handle.0, 0xffff_ffff | 0xffff_fffe) {
        return 0;
    }
    // Win32 and NT key opens use the same Objects registry namespace. The
    // predefined Win32 HKEY constants are not native handles.
    if handle.0 & 0x8000_0000 == 0
        && env
            .ext
            .try_get::<adv_os::registry::VirtualRegistry>()
            .is_some_and(|registry| registry.close(&mut env.sys.objects, handle.0).is_ok())
    {
        return 0;
    }
    if env.sys.objects.close(handle.0).is_none() {
        return INVALID_HANDLE;
    }
    // Closing just one alias retains its payload; the final close drops it
    // before pruning weak mapping/synchronization names, as CloseHandle does.
    crate::kernel32::prune_file_mappings(env);
    0
}

fn completion(env: &mut Env<'_>, iosb: GuestPtr<()>, status: i32, count: u32) -> i32 {
    let mut bytes = [0; 8];
    bytes[..4].copy_from_slice(&status.to_le_bytes());
    bytes[4..].copy_from_slice(&count.to_le_bytes());
    if env.cpu.write(iosb.addr(), &bytes).is_err() {
        ACCESS_VIOLATION
    } else {
        status
    }
}

#[win32(ntdll, name = "ZwReadFile", level = implement)]
pub(crate) fn read(
    env: &mut Env<'_>,
    handle: Handle,
    event: Handle,
    apc: GuestPtr<()>,
    context: GuestPtr<()>,
    iosb: GuestPtr<()>,
    buffer: GuestPtr<()>,
    length: u32,
    offset: GuestPtr<()>,
    key: GuestPtr<u32>,
) -> i32 {
    if event.0 != 0 || !apc.is_null() || !context.is_null() || !key.is_null() {
        warn_unavailable(
            env,
            "ntdll.dll!ZwReadFile",
            "asynchronous/event/APC completion and range-lock keys are unavailable",
        );
        return NOT_SUPPORTED;
    }
    // Unlike SetInformationFile, native ReadFile probes both writable outputs
    // before consuming any bytes or changing the current-position cursor.
    if check_range(env.cpu, iosb.addr(), 8, Prot::WRITE).is_err()
        || check_range(env.cpu, buffer.addr(), length, Prot::WRITE).is_err()
    {
        return ACCESS_VIOLATION;
    }
    let requested = if offset.is_null() {
        None
    } else {
        if check_range(env.cpu, offset.addr(), 8, Prot::READ).is_err() {
            return ACCESS_VIOLATION;
        }
        let mut bytes = [0; 8];
        if env.cpu.read(offset.addr(), &mut bytes).is_err() {
            return ACCESS_VIOLATION;
        }
        match i64::from_le_bytes(bytes) {
            -2 => None, // FILE_USE_FILE_POINTER_POSITION, sign-extended LARGE_INTEGER
            value if value >= 0 => Some(value as u64),
            _ => return INVALID_PARAMETER,
        }
    };
    let Some(file) = env.sys.objects.host_mut::<FileObj>(handle.0) else {
        return if env.sys.objects.get(handle.0).is_some()
            || matches!(handle.0, 0xffff_ffff | 0xffff_fffe)
        {
            0xc000_0024u32 as i32
        } else {
            INVALID_HANDLE
        };
    };
    if file.access & (GENERIC_READ | 0x1000_0000 | 1) == 0 {
        return ACCESS_DENIED;
    }
    if length == 0 {
        return completion(env, iosb, 0, 0);
    }
    let old = match file.file.stream_position() {
        Ok(position) => position,
        Err(_) => return completion(env, iosb, 0xc000_0001u32 as i32, 0),
    };
    let start = requested.unwrap_or(old);
    let size = match file.file.metadata() {
        Ok(metadata) => metadata.len(),
        Err(_) => return completion(env, iosb, 0xc000_0001u32 as i32, 0),
    };
    if start >= size {
        return completion(env, iosb, 0xc000_0011u32 as i32, 0);
    }
    // Allocate only bytes that can be read, with fallible reservation before
    // seeking. A short file does not require a Length-sized host allocation.
    let capacity = u64::from(length).min(size - start) as usize;
    let mut data = Vec::new();
    if data.try_reserve_exact(capacity).is_err() {
        return INSUFFICIENT_RESOURCES;
    }
    data.resize(capacity, 0);
    if requested.is_some() && file.file.seek(SeekFrom::Start(start)).is_err() {
        return completion(env, iosb, INVALID_PARAMETER, 0);
    }
    let count =
        match crate::kernel32::read_coherent(file, &env.ext.get::<Win32>().vfs.sections, &mut data)
        {
            Ok(count) => count,
            Err(error) => {
                log::warn!("ZwReadFile shared stream read failed: {error}");
                return completion(env, iosb, 0xc000_0001u32 as i32, 0);
            }
        };
    if count == 0 {
        // A backing file truncated between metadata and read also reports EOF
        // without publishing an explicit offset as the current file position.
        let _ = file.file.seek(SeekFrom::Start(old));
        return completion(env, iosb, 0xc000_0011u32 as i32, 0);
    }
    if env.cpu.write(buffer.addr(), &data[..count]).is_err() {
        return ACCESS_VIOLATION;
    }
    completion(env, iosb, 0, count as u32)
}

#[win32(ntdll, name = "ZwSetInformationFile", level = implement)]
pub(crate) fn set(
    env: &mut Env<'_>,
    handle: Handle,
    iosb: GuestPtr<()>,
    input: GuestPtr<()>,
    length: u32,
    class: i32,
) -> i32 {
    if class == 4 || class == 20 {
        return metadata::set(env, handle, iosb, input, length, class);
    }
    if class != 14 {
        warn_unavailable(
            env,
            "ntdll.dll!ZwSetInformationFile",
            "only basic/position/end-of-file information is implemented",
        );
        return if class == 0 {
            0xc000_0002u32 as i32
        } else {
            NOT_SUPPORTED
        };
    }
    if length < 8 {
        return 0xc000_0004u32 as i32; // STATUS_INFO_LENGTH_MISMATCH
    }
    if check_range(env.cpu, input.addr(), length, Prot::READ).is_err()
        || check_range(env.cpu, iosb.addr(), 8, Prot::READ).is_err()
    {
        return ACCESS_VIOLATION;
    }
    let mut bytes = [0; 8];
    if env.cpu.read(input.addr(), &mut bytes).is_err() {
        return ACCESS_VIOLATION;
    }
    let position = i64::from_le_bytes(bytes);
    if position < 0 {
        return INVALID_PARAMETER;
    }
    let Some(file) = env.sys.objects.host_mut::<FileObj>(handle.0) else {
        return if env.sys.objects.get(handle.0).is_some()
            || matches!(handle.0, 0xffff_ffff | 0xffff_fffe)
        {
            0xc000_0024u32 as i32 // STATUS_OBJECT_TYPE_MISMATCH
        } else {
            INVALID_HANDLE
        };
    };
    // Read-only file contents do not prevent seeking. The native position
    // setter also accepts attributes-only/overlapped handles. Every alias
    // shares the same FileStream position; no file data or length is changed.
    if let Err(error) = file.file.seek(SeekFrom::Start(position as u64)) {
        log::warn!("ZwSetInformationFile position update failed: {error}");
        return match error.kind() {
            std::io::ErrorKind::InvalidInput => INVALID_PARAMETER,
            std::io::ErrorKind::PermissionDenied => ACCESS_DENIED,
            _ => 0xc000_0001u32 as i32, // STATUS_UNSUCCESSFUL
        };
    }
    // Native completes the seek before reporting a write fault on a mapped
    // read-only IOSB. Preserve those bytes and report AV without bypassing
    // guest page protection. NULL/unmapped IOSB was rejected before seeking.
    if check_range(env.cpu, iosb.addr(), 8, Prot::WRITE).is_err()
        || env.cpu.write(iosb.addr(), &[0; 8]).is_err()
    {
        return ACCESS_VIOLATION;
    }
    0
}

/// The file position belongs to the shared file object, including duplicate
/// Win32 handles and virtual System32 streams. Querying never moves it.
#[win32(ntdll, name = "ZwQueryInformationFile", level = implement)]
pub(crate) fn query(
    env: &mut Env<'_>,
    handle: Handle,
    iosb: GuestPtr<()>,
    output: GuestPtr<()>,
    length: u32,
    class: i32,
) -> i32 {
    if class == 4 || class == 6 {
        return metadata::query(env, handle, iosb, output, length, class);
    }
    let required = match class {
        5 => 24,
        14 => 8,
        _ => {
            warn_unavailable(
                env,
                "ntdll.dll!ZwQueryInformationFile",
                "only basic/standard/internal/position information is implemented",
            );
            return if class == 0 {
                0xc000_0002u32 as i32
            } else {
                NOT_SUPPORTED
            };
        }
    };
    if length < required {
        return 0xc000_0004u32 as i32; // STATUS_INFO_LENGTH_MISMATCH
    }
    if check_range(env.cpu, iosb.addr(), 8, Prot::WRITE).is_err()
        || check_range(env.cpu, output.addr(), length, Prot::WRITE).is_err()
    {
        return ACCESS_VIOLATION;
    }
    let Some(file) = env.sys.objects.host::<FileObj>(handle.0) else {
        return if env.sys.objects.get(handle.0).is_some()
            || matches!(handle.0, 0xffff_ffff | 0xffff_fffe)
        {
            0xc000_0024u32 as i32 // STATUS_OBJECT_TYPE_MISMATCH
        } else {
            INVALID_HANDLE
        };
    };
    // Native also accepts attributes-only and overlapped handles for this
    // query. Do not manufacture a second per-front position or mode table.
    let mut information = [0; 24];
    if class == 14 {
        let position = match file.file.stream_position() {
            Ok(position) => position,
            Err(error) => {
                log::warn!("ZwQueryInformationFile position query failed: {error}");
                return if error.kind() == std::io::ErrorKind::PermissionDenied {
                    ACCESS_DENIED
                } else {
                    0xc000_0001u32 as i32 // STATUS_UNSUCCESSFUL
                };
            }
        };
        information[..8].copy_from_slice(&position.to_le_bytes());
    } else {
        let standard = match file.file.standard_information() {
            Ok(standard) => standard,
            Err(error) => {
                log::warn!("ZwQueryInformationFile standard query failed: {error}");
                return match error.kind() {
                    std::io::ErrorKind::PermissionDenied => ACCESS_DENIED,
                    std::io::ErrorKind::Unsupported => unavailable_for(
                        env,
                        "ntdll.dll!ZwQueryInformationFile",
                        "opened-file standard metadata is unavailable",
                    ),
                    _ => 0xc000_0001u32 as i32, // STATUS_UNSUCCESSFUL
                };
            }
        };
        information[..8].copy_from_slice(&standard.allocation_size.to_le_bytes());
        information[8..16].copy_from_slice(&standard.end_of_file.to_le_bytes());
        information[16..20].copy_from_slice(&standard.number_of_links.to_le_bytes());
        information[20] = u8::from(standard.delete_pending);
        information[21] = u8::from(standard.directory);
    }
    let mut completion = [0; 8];
    completion[4..].copy_from_slice(&required.to_le_bytes());
    if env
        .cpu
        .write(output.addr(), &information[..required as usize])
        .and_then(|()| env.cpu.write(iosb.addr(), &completion))
        .is_err()
    {
        return ACCESS_VIOLATION;
    }
    0
}

fn unavailable(env: &mut Env<'_>, reason: &str) -> i32 {
    unavailable_for(env, "ntdll.dll!ZwCreateFile", reason)
}

fn unavailable_for(env: &mut Env<'_>, api: &str, reason: &str) -> i32 {
    warn_unavailable(env, api, reason);
    NOT_SUPPORTED
}

struct ObjectAttributes([u32; 6]);

impl ObjectAttributes {
    fn read(env: &Env<'_>, pointer: u32) -> Result<Self, i32> {
        check_range(env.cpu, pointer, 24, Prot::READ).map_err(|_| ACCESS_VIOLATION)?;
        let mut bytes = [0; 24];
        env.cpu
            .read(pointer, &mut bytes)
            .map_err(|_| ACCESS_VIOLATION)?;
        let fields = std::array::from_fn(|i| {
            u32::from_le_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap())
        });
        if fields[0] != 24 {
            return Err(INVALID_PARAMETER);
        }
        Ok(Self(fields))
    }

    fn scope(&self, env: &mut Env<'_>, api: &str) -> Result<(), i32> {
        if self.0[1] != 0 {
            return Err(if env.sys.objects.identity(self.0[1]).is_none() {
                INVALID_HANDLE
            } else {
                unavailable_for(
                    env,
                    api,
                    "RootDirectory-relative native paths require directory handles",
                )
            });
        }
        if self.0[3] != 0x40 || self.0[4] != 0 || self.0[5] != 0 {
            return Err(unavailable_for(
                env,
                api,
                "only OBJ_CASE_INSENSITIVE with default object security and no inheritance",
            ));
        }
        Ok(())
    }

    fn path(&self, env: &mut Env<'_>, api: &str) -> Result<String, i32> {
        let name = self.0[2];
        check_range(env.cpu, name, 8, Prot::READ).map_err(|_| ACCESS_VIOLATION)?;
        let mut descriptor = [0; 8];
        env.cpu
            .read(name, &mut descriptor)
            .map_err(|_| ACCESS_VIOLATION)?;
        let length = u16::from_le_bytes(descriptor[..2].try_into().unwrap()) as u32;
        let buffer = u32::from_le_bytes(descriptor[4..].try_into().unwrap());
        // This is a counted input; MaximumLength and any terminator beyond
        // Length do not extend the name or the memory range being probed.
        if length == 0 || length & 1 != 0 {
            return Err(NAME_INVALID);
        }
        check_range(env.cpu, buffer, length, Prot::READ).map_err(|_| ACCESS_VIOLATION)?;
        let mut bytes = vec![0; length as usize];
        env.cpu
            .read(buffer, &mut bytes)
            .map_err(|_| ACCESS_VIOLATION)?;
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .collect();
        let name = String::from_utf16(&units)
            .map_err(|_| unavailable_for(env, api, "unpaired UTF-16 surrogate file names"))?;
        let dos = name.strip_prefix(r"\??\").ok_or_else(|| {
            unavailable_for(
                env,
                api,
                "native device namespaces outside the guest DOS filesystem",
            )
        })?;
        if dos.contains(['\0', '/', '*', '?']) {
            return Err(NAME_INVALID);
        }
        let path = if dos
            .get(..4)
            .is_some_and(|p| p.eq_ignore_ascii_case(r"UNC\"))
        {
            format!(r"\\{}", &dos[4..])
        } else if dos.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
            && dos.as_bytes().get(1..3) == Some(b":\\")
        {
            dos.to_owned()
        } else {
            return Err(unavailable_for(
                env,
                api,
                "DOS device names or relative NT names",
            ));
        };
        // The VFS normalizes Win32 names. Reject unsupported literal native
        // distinctions before normalization could silently select another file.
        if path
            .split('\\')
            .any(|part| matches!(part, "." | "..") || part.ends_with([' ', '.']))
            || path.get(2..).is_some_and(|p| p.contains(':'))
        {
            return Err(unavailable_for(
                env,
                api,
                "literal native dot components, trailing dots/spaces, or streams",
            ));
        }
        Ok(env.ext.get::<Win32>().vfs.normalize(&path))
    }
}

fn share_access(access: u32) -> u32 {
    // Generic mapping applies to file access, not to share-mode bits.
    let read = access & (GENERIC_READ | 0x1000_0000 | 0x21) != 0;
    let write = access & (0x4000_0000 | 0x1000_0000 | 0x6) != 0;
    let delete = access & (0x1000_0000 | 0x0001_0000) != 0;
    u32::from(read) | (u32::from(write) << 1) | (u32::from(delete) << 2)
}

#[win32(ntdll, name = "ZwCreateFile", level = implement)]
pub(crate) fn create(
    env: &mut Env<'_>,
    output: GuestPtr<u32>,
    access: u32,
    attributes: GuestPtr<()>,
    iosb: GuestPtr<()>,
    allocation: GuestPtr<()>,
    file_attributes: u32,
    share: u32,
    disposition: u32,
    options: u32,
    ea_buffer: GuestPtr<()>,
    ea_length: u32,
) -> i32 {
    // Probe every output before a filesystem operation. Native parameter/name
    // failures preserve IOSB; an invalid OBJECT_ATTRIBUTES length also leaves
    // FileHandle untouched. Subsequent failures clear only FileHandle.
    if check_range(env.cpu, output.addr(), 4, Prot::WRITE).is_err()
        || check_range(env.cpu, iosb.addr(), 8, Prot::WRITE).is_err()
        || check_range(env.cpu, attributes.addr(), 24, Prot::READ).is_err()
    {
        return ACCESS_VIOLATION;
    }
    let oa = match ObjectAttributes::read(env, attributes.addr()) {
        Ok(oa) => oa,
        Err(status) => return status,
    };
    if output.write(env.cpu, 0).is_err() {
        return ACCESS_VIOLATION;
    }
    if share & !7 != 0 || disposition > 5 || options & 0x30 == 0x30 {
        return INVALID_PARAMETER;
    }
    if options & 0x30 != 0 && access & SYNCHRONIZE == 0 {
        return INVALID_PARAMETER;
    }
    if let Err(status) = oa.scope(env, "ntdll.dll!ZwCreateFile") {
        return status;
    }
    if options & 1 != 0 {
        if options & !0x21 != 0
            || options & 0x20 == 0
            || disposition != 1
            || access & !0x8012_0089 != 0
            || access & (GENERIC_READ | 1) == 0
        {
            return unavailable(
                env,
                "only synchronous read/list FILE_OPEN directory handles",
            );
        }
        if !allocation.is_null() || !ea_buffer.is_null() || ea_length != 0 {
            return unavailable(
                env,
                "directory allocation and extended attributes are unavailable",
            );
        }
        let path = match oa.path(env, "ntdll.dll!ZwCreateFile") {
            Ok(path) => path,
            Err(status) => return status,
        };
        return enumeration::open(env, &path, output, iosb);
    }
    if options & 0x30 == 0 || options & !0x874 != 0 {
        return unavailable(env, "only synchronous ordinary non-directory files");
    }
    let mapped_disposition = match disposition {
        1 => 3, // FILE_OPEN -> OPEN_EXISTING
        2 => 1, // FILE_CREATE -> CREATE_NEW
        3 => 4, // FILE_OPEN_IF -> OPEN_ALWAYS
        4 => 5, // FILE_OVERWRITE -> TRUNCATE_EXISTING
        5 => 2, // FILE_OVERWRITE_IF -> CREATE_ALWAYS
        _ => return unavailable(env, "FILE_SUPERSEDE replacement semantics are unavailable"),
    };
    if access & !(READ_ACCESS | WRITE_ACCESS) != 0
        || access & (GENERIC_READ | GENERIC_WRITE | 0x183) == 0
    {
        return unavailable(
            env,
            "only file read/write access and synchronization/attribute rights",
        );
    }
    if access & 4 != 0 && access & (GENERIC_WRITE | 2) == 0 {
        return unavailable(env, "append-only file handles require append write support");
    }
    if matches!(disposition, 4 | 5) && access & (GENERIC_WRITE | 2) == 0 {
        return ACCESS_DENIED;
    }
    if disposition != 1 && file_attributes & !0x80 != 0 {
        return unavailable(
            env,
            "only default attributes for created or overwritten files",
        );
    }
    if !ea_buffer.is_null() || ea_length != 0 {
        return unavailable(env, "extended attributes are unavailable");
    }
    if !allocation.is_null() && check_range(env.cpu, allocation.addr(), 8, Prot::READ).is_err() {
        return ACCESS_VIOLATION;
    }
    if !allocation.is_null() && disposition != 1 {
        let mut bytes = [0; 8];
        if env.cpu.read(allocation.addr(), &mut bytes).is_err() {
            return ACCESS_VIOLATION;
        }
        match i64::from_le_bytes(bytes) {
            0 => {}
            value if value < 0 => return INVALID_PARAMETER,
            _ => return unavailable(env, "initial file allocation reservations are unavailable"),
        }
    }
    // AllocationSize and FileAttributes do not change an existing FILE_OPEN.
    let guest = match oa.path(env, "ntdll.dll!ZwCreateFile") {
        Ok(path) => path,
        Err(status) => return status,
    };
    let vfs = &env.ext.get::<Win32>().vfs;
    if vfs.is_dir(&guest) {
        return if options & 0x40 != 0 {
            FILE_IS_A_DIRECTORY
        } else {
            unavailable(env, "native directory handles are unavailable")
        };
    }
    if disposition != 1 {
        let parent = guest.rsplit_once('\\').map_or("", |(parent, _)| parent);
        let parent = if parent.len() == 2 {
            format!("{parent}\\")
        } else {
            parent.to_owned()
        };
        if !vfs.is_dir(&parent) {
            return PATH_NOT_FOUND;
        }
    }
    let requested = share_access(access);
    if env.sys.objects.hosts::<FileObj>().any(|(_, file)| {
        file.guest_path.eq_ignore_ascii_case(&guest)
            && (requested & !file.share != 0 || share_access(file.access) & !share != 0)
    }) {
        return SHARING_VIOLATION;
    }
    let previous_error = util::last_error(env);
    // The common opener selects copy-up from generic access. Keep that host
    // selection separate from the native handle's actual requested rights.
    let mapped_access = access
        | if access & 1 != 0 { GENERIC_READ } else { 0 }
        | if access & 2 != 0 { GENERIC_WRITE } else { 0 };
    let handle =
        crate::kernel32::open_with_share(env, &guest, mapped_access, mapped_disposition, share);
    let error = util::last_error(env);
    util::set_last_error(env, previous_error);
    if handle == util::INVALID_HANDLE_VALUE {
        return match error {
            2 | 3 => {
                let parent = guest.rsplit_once('\\').map_or("", |(p, _)| p);
                if env.ext.get::<Win32>().vfs.is_dir(parent) {
                    NAME_NOT_FOUND
                } else {
                    PATH_NOT_FOUND
                }
            }
            5 => ACCESS_DENIED,
            8 => INSUFFICIENT_RESOURCES,
            19 => 0xc000_00a2u32 as i32, // STATUS_MEDIA_WRITE_PROTECTED
            32 => SHARING_VIOLATION,
            80 => 0xc000_0035u32 as i32, // STATUS_OBJECT_NAME_COLLISION
            1224 => 0xc000_0243u32 as i32, // STATUS_USER_MAPPED_FILE
            _ => unavailable(
                env,
                "the common file opener reported an unmapped native error",
            ),
        };
    }
    if let Some(file) = env.sys.objects.host_mut::<FileObj>(handle) {
        file.access = access;
    }
    let mut status = [0; 8];
    let information = match disposition {
        1 => 1u32, // FILE_OPENED
        2 => 2,    // FILE_CREATED
        3 if error == 183 => 1,
        4 => 3, // FILE_OVERWRITTEN
        5 if error == 183 => 3,
        _ => 2,
    };
    status[4..].copy_from_slice(&information.to_le_bytes());
    if env
        .cpu
        .write(iosb.addr(), &status)
        .and_then(|()| output.write(env.cpu, handle))
        .is_err()
    {
        env.sys.objects.close(handle);
        return ACCESS_VIOLATION;
    }
    0
}
