//! Native path queries use VFS metadata directly, never a Win32 guest hook.
use super::*;
use adv_os::vfs::DirEntry;
use std::io;
use std::time::SystemTime;

#[cfg(test)]
mod tests;

pub(super) fn register(registry: &mut Registry) {
    registry.register(basic_ABI);
    registry.register(full_ABI);
}

fn file_status(env: &mut Env<'_>, path: &str, api: &str, error: io::Error) -> i32 {
    match error.kind() {
        io::ErrorKind::NotFound => {
            let vfs = &env.ext.get::<Win32>().vfs;
            let parent = path
                .trim_end_matches('\\')
                .rsplit_once('\\')
                .map(|(parent, _)| {
                    if parent.len() == 2 {
                        format!("{parent}\\")
                    } else {
                        parent.to_owned()
                    }
                });
            if parent.is_some_and(|parent| vfs.is_dir(&parent)) {
                NAME_NOT_FOUND
            } else {
                PATH_NOT_FOUND
            }
        }
        io::ErrorKind::PermissionDenied => ACCESS_DENIED,
        io::ErrorKind::OutOfMemory => INSUFFICIENT_RESOURCES,
        io::ErrorKind::InvalidInput => INVALID_PARAMETER,
        io::ErrorKind::Unsupported => {
            unavailable_for(env, api, "backing-file metadata is unavailable")
        }
        _ => 0xc000_0001u32 as i32, // STATUS_UNSUCCESSFUL
    }
}

fn metadata(env: &mut Env<'_>, path: &str, api: &str) -> Result<DirEntry, i32> {
    // Synthetic files retain the same immutable SystemFiles owner used by
    // ordinary file opens. Probing it does not acquire a module reference.
    let image = env
        .ext
        .get_or_default::<adv_os::system_files::SystemFiles>()
        .open(env.cpu, env.sys, path)
        .map_err(|_| unavailable_for(env, api, "synthetic file metadata is unavailable"))?;
    if let Some(image) = image {
        return Ok(DirEntry {
            name: String::new(),
            is_dir: false,
            size: image.len() as u64,
            created: Some(SystemTime::UNIX_EPOCH),
            accessed: Some(SystemTime::UNIX_EPOCH),
            modified: Some(SystemTime::UNIX_EPOCH),
            attributes: 0x21, // Shared read-only synthetic-file profile.
        });
    }
    let result = (|| {
        let vfs = &env.ext.get::<Win32>().vfs;
        if vfs.is_optical_path(path) {
            return vfs.optical_entry(path);
        }
        let host = vfs.lookup(path).ok_or(io::ErrorKind::NotFound)?;
        let metadata = std::fs::metadata(&host)?;
        Ok(DirEntry {
            name: String::new(),
            is_dir: metadata.is_dir(),
            size: metadata.len(),
            created: metadata.created().or_else(|_| metadata.modified()).ok(),
            accessed: metadata.accessed().ok(),
            modified: metadata.modified().ok(),
            attributes: vfs.file_attributes(path)?,
        })
    })();
    result.map_err(|error| file_status(env, path, api, error))
}

fn query_path(
    env: &mut Env<'_>,
    attributes: GuestPtr<()>,
    output: GuestPtr<()>,
    size: u32,
    api: &str,
) -> Result<DirEntry, i32> {
    // Validate the entire output before any lookup; a failed query publishes
    // no partially filled record and never changes Win32 LastError.
    check_range(env.cpu, output.addr(), size, Prot::WRITE).map_err(|_| ACCESS_VIOLATION)?;
    let oa = ObjectAttributes::read(env, attributes.addr())?;
    oa.scope(env, api)?;
    let path = oa.path(env, api)?;
    metadata(env, &path, api)
}

fn timestamp(time: Option<SystemTime>) -> u64 {
    const UNIX_EPOCH: u64 = 116_444_736_000_000_000;
    let Some(time) = time else { return 0 };
    match time.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(delta) => u64::try_from(delta.as_nanos() / 100)
            .ok()
            .and_then(|ticks| UNIX_EPOCH.checked_add(ticks))
            .unwrap_or(0),
        Err(before) => u64::try_from(before.duration().as_nanos() / 100)
            .ok()
            .and_then(|ticks| UNIX_EPOCH.checked_sub(ticks))
            .unwrap_or(0),
    }
}

#[win32(ntdll, name = "NtQueryAttributesFile", level = implement)]
fn basic(env: &mut Env<'_>, attributes: GuestPtr<()>, output: GuestPtr<()>) -> i32 {
    let metadata = match query_path(
        env,
        attributes,
        output,
        40,
        "ntdll.dll!NtQueryAttributesFile",
    ) {
        Ok(metadata) => metadata,
        Err(status) => return status,
    };
    // FILE_BASIC_INFORMATION: four aligned LARGE_INTEGERs, ULONG attributes,
    // and four bytes of trailing alignment. ChangeTime is zero because the
    // existing metadata owner does not provide a separate change timestamp.
    let mut bytes = [0; 40];
    for (index, time) in [metadata.created, metadata.accessed, metadata.modified]
        .into_iter()
        .enumerate()
    {
        bytes[index * 8..index * 8 + 8].copy_from_slice(&timestamp(time).to_le_bytes());
    }
    bytes[32..36].copy_from_slice(&metadata.attributes.to_le_bytes());
    if env.cpu.write(output.addr(), &bytes).is_err() {
        ACCESS_VIOLATION
    } else {
        0
    }
}

#[win32(ntdll, name = "NtQueryFullAttributesFile", level = safe_fail)]
fn full(env: &mut Env<'_>, attributes: GuestPtr<()>, output: GuestPtr<()>) -> i32 {
    if let Err(status) = query_path(
        env,
        attributes,
        output,
        56,
        "ntdll.dll!NtQueryFullAttributesFile",
    ) {
        return status;
    }
    // FILE_NETWORK_OPEN_INFORMATION also promises AllocationSize. File length
    // alone cannot supply that value for sparse/compressed/read-only sources.
    unavailable_for(
        env,
        "ntdll.dll!NtQueryFullAttributesFile",
        "file allocation size is not modeled by the shared metadata owner",
    )
}
