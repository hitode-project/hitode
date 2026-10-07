//! Native synchronous FILE_OPEN shares the Win32 VFS and FileObj owner.
use std::io::{Seek, SeekFrom};

use adv_abi::{GuestPtr, Handle, Registry, guest::check_range, warn_unavailable, win32};
use adv_cpu::backend::Prot;
use adv_machine::Env;

use crate::{FileObj, Win32, util};

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
const READ_ACCESS: u32 = GENERIC_READ | SYNCHRONIZE | 0x0002_0000 | 0x89;

pub(super) fn register(registry: &mut Registry) {
    registry.register(create_ABI);
    registry.register(query_ABI);
    registry.register(set_ABI);
    registry.register(read_ABI);
    registry.register(close_ABI);
    // Nt/Zw user-mode names have the same declarations and behavior.
    for (api, name) in [
        (create_ABI, "NtCreateFile"),
        (query_ABI, "NtQueryInformationFile"),
        (read_ABI, "NtReadFile"),
    ] {
        registry.register(adv_abi::Api { name, ..api });
    }
}

#[win32(ntdll, name = "ZwClose", level = implement)]
pub(super) fn close(env: &mut Env<'_>, handle: Handle) -> i32 {
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
fn read(
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
fn set(
    env: &mut Env<'_>,
    handle: Handle,
    iosb: GuestPtr<()>,
    input: GuestPtr<()>,
    length: u32,
    class: i32,
) -> i32 {
    if class != 14 {
        warn_unavailable(
            env,
            "ntdll.dll!ZwSetInformationFile",
            "only FilePositionInformation is implemented",
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
fn query(
    env: &mut Env<'_>,
    handle: Handle,
    iosb: GuestPtr<()>,
    output: GuestPtr<()>,
    length: u32,
    class: i32,
) -> i32 {
    if class != 14 {
        warn_unavailable(
            env,
            "ntdll.dll!ZwQueryInformationFile",
            "only FilePositionInformation is implemented",
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
    let mut completion = [0; 8];
    completion[4..].copy_from_slice(&8u32.to_le_bytes());
    if env
        .cpu
        .write(output.addr(), &position.to_le_bytes())
        .and_then(|()| env.cpu.write(iosb.addr(), &completion))
        .is_err()
    {
        return ACCESS_VIOLATION;
    }
    0
}

fn unavailable(env: &mut Env<'_>, reason: &str) -> i32 {
    warn_unavailable(env, "ntdll.dll!ZwCreateFile", reason);
    NOT_SUPPORTED
}

fn share_access(access: u32) -> u32 {
    // Generic mapping applies to file access, not to share-mode bits.
    let read = access & (GENERIC_READ | 0x1000_0000 | 0x21) != 0;
    let write = access & (0x4000_0000 | 0x1000_0000 | 0x6) != 0;
    let delete = access & (0x1000_0000 | 0x0001_0000) != 0;
    u32::from(read) | (u32::from(write) << 1) | (u32::from(delete) << 2)
}

#[win32(ntdll, name = "ZwCreateFile", level = implement)]
fn create(
    env: &mut Env<'_>,
    output: GuestPtr<u32>,
    access: u32,
    attributes: GuestPtr<()>,
    iosb: GuestPtr<()>,
    allocation: GuestPtr<()>,
    _file_attributes: u32,
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
    let mut oa = [0u8; 24];
    if env.cpu.read(attributes.addr(), &mut oa).is_err() {
        return ACCESS_VIOLATION;
    }
    let field = |index: usize| u32::from_le_bytes(oa[index * 4..index * 4 + 4].try_into().unwrap());
    if field(0) != 24 {
        return INVALID_PARAMETER;
    }
    if output.write(env.cpu, 0).is_err() {
        return ACCESS_VIOLATION;
    }
    if share & !7 != 0 || disposition > 5 || options & 0x30 == 0x30 {
        return INVALID_PARAMETER;
    }
    if options & 0x30 != 0 && access & SYNCHRONIZE == 0 {
        return INVALID_PARAMETER;
    }
    if field(1) != 0 {
        return if env.sys.objects.identity(field(1)).is_none() {
            INVALID_HANDLE
        } else {
            unavailable(
                env,
                "RootDirectory-relative native opens require directory handles",
            )
        };
    }
    if field(3) != 0x40 || field(4) != 0 || field(5) != 0 {
        return unavailable(
            env,
            "only OBJ_CASE_INSENSITIVE with default object security and no inheritance",
        );
    }
    if disposition != 1 || options & 0x30 == 0 || options & !0x70 != 0 {
        return unavailable(
            env,
            "only synchronous FILE_OPEN of ordinary non-directory files",
        );
    }
    if access & !READ_ACCESS != 0 || access & (GENERIC_READ | 1) == 0 {
        return unavailable(
            env,
            "only file read access and its synchronization/attribute rights",
        );
    }
    if !ea_buffer.is_null() || ea_length != 0 {
        return unavailable(env, "extended attributes are unavailable");
    }
    if !allocation.is_null() && check_range(env.cpu, allocation.addr(), 8, Prot::READ).is_err() {
        return ACCESS_VIOLATION;
    }
    // AllocationSize and FileAttributes do not change an existing FILE_OPEN.
    let name = field(2);
    if check_range(env.cpu, name, 8, Prot::READ).is_err() {
        return ACCESS_VIOLATION;
    }
    let mut descriptor = [0u8; 8];
    if env.cpu.read(name, &mut descriptor).is_err() {
        return ACCESS_VIOLATION;
    }
    let length = u16::from_le_bytes(descriptor[..2].try_into().unwrap()) as u32;
    let buffer = u32::from_le_bytes(descriptor[4..].try_into().unwrap());
    if length == 0 || length & 1 != 0 {
        return NAME_INVALID;
    }
    if check_range(env.cpu, buffer, length, Prot::READ).is_err() {
        return ACCESS_VIOLATION;
    }
    let mut bytes = vec![0; length as usize];
    if env.cpu.read(buffer, &mut bytes).is_err() {
        return ACCESS_VIOLATION;
    }
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .collect();
    let Ok(name) = String::from_utf16(&units) else {
        return unavailable(env, "unpaired UTF-16 surrogate file names");
    };
    let Some(dos) = name.strip_prefix(r"\??\") else {
        return unavailable(
            env,
            "native device namespaces outside the guest DOS filesystem",
        );
    };
    if dos.contains(['\0', '/', '*', '?']) {
        return NAME_INVALID;
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
        return unavailable(env, "DOS device names or relative NT names");
    };
    // The common VFS normalizes Win32 names. Do not silently normalize a
    // native name into a different file when that distinction is material.
    if path
        .split('\\')
        .any(|part| matches!(part, "." | "..") || part.ends_with([' ', '.']))
        || path.get(2..).is_some_and(|p| p.contains(':'))
    {
        return unavailable(
            env,
            "literal native dot components, trailing dots/spaces, or streams",
        );
    }
    let vfs = &env.ext.get::<Win32>().vfs;
    let guest = vfs.normalize(&path);
    if vfs.is_dir(&guest) {
        return if options & 0x40 != 0 {
            FILE_IS_A_DIRECTORY
        } else {
            unavailable(env, "native directory handles are unavailable")
        };
    }
    let requested = share_access(access);
    if env.sys.objects.hosts::<FileObj>().any(|(_, file)| {
        file.guest_path.eq_ignore_ascii_case(&guest)
            && (requested & !file.share != 0 || share_access(file.access) & !share != 0)
    }) {
        return SHARING_VIOLATION;
    }
    let previous_error = util::last_error(env);
    let handle = crate::kernel32::open_with_share(env, &guest, access, 3, share);
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
            32 => SHARING_VIOLATION,
            _ => unavailable(
                env,
                "the common file opener reported an unmapped native error",
            ),
        };
    }
    let mut status = [0; 8];
    status[4..].copy_from_slice(&1u32.to_le_bytes()); // FILE_OPENED
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
