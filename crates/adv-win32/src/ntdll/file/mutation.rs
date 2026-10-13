//! NT rename/disposition mutate the existing FileObj and backing instance.
use super::*;

fn failure(env: &mut Env<'_>, error: std::io::Error) -> i32 {
    match error.raw_os_error().filter(|_| cfg!(windows)) {
        Some(32 | 33) => return SHARING_VIOLATION,
        Some(80 | 183) => return 0xc0000035u32 as i32,
        Some(145) => return 0xc0000101u32 as i32,
        Some(17) => return 0xc00000d4u32 as i32,
        Some(303) => return 0xc0000056u32 as i32,
        _ => {}
    }
    match error.kind() {
        std::io::ErrorKind::PermissionDenied => ACCESS_DENIED,
        std::io::ErrorKind::NotFound => NAME_NOT_FOUND,
        std::io::ErrorKind::InvalidInput => INVALID_PARAMETER,
        std::io::ErrorKind::AlreadyExists => 0xc0000035u32 as i32,
        std::io::ErrorKind::OutOfMemory => INSUFFICIENT_RESOURCES,
        std::io::ErrorKind::Unsupported => unavailable_for(
            env,
            "ntdll.dll!NtSetInformationFile",
            "opened-instance rename/delete-pending is unavailable on this host platform",
        ),
        _ => 0xc0000001u32 as i32,
    }
}

fn writable(env: &mut Env<'_>, path: &str) -> Result<(), i32> {
    let vfs = &env.ext.get::<Win32>().vfs;
    if vfs.is_optical_path(path) {
        return Err(0xc00000a2u32 as i32);
    }
    if vfs.has_read_only_underlay(path) {
        return Err(unavailable_for(
            env,
            "ntdll.dll!<file mutation>",
            "deleting or renaming read-only VFS layers requires persistent whiteouts",
        ));
    }
    Ok(())
}

pub(super) fn open(
    env: &mut Env<'_>,
    path: &str,
    access: u32,
    share: u32,
    output: GuestPtr<u32>,
    iosb: GuestPtr<()>,
) -> i32 {
    if let Err(status) = writable(env, path) {
        return status;
    }
    if !env.sys.objects.can_insert() {
        return INSUFFICIENT_RESOURCES;
    }
    let vfs = &env.ext.get::<Win32>().vfs;
    let Some(host) = vfs.lookup(path) else {
        return if path
            .rsplit_once('\\')
            .is_some_and(|(parent, _)| vfs.is_dir(parent))
        {
            NAME_NOT_FOUND
        } else {
            PATH_NOT_FOUND
        };
    };
    // Reparse targets need an explicit link-instance model; following one and
    // deleting its target would silently change the requested operation.
    let metadata = match std::fs::symlink_metadata(&host) {
        Ok(metadata) => metadata,
        Err(error) => return failure(env, error),
    };
    #[cfg(windows)]
    let reparse = {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    };
    #[cfg(not(windows))]
    let reparse = metadata.file_type().is_symlink();
    if reparse {
        return unavailable_for(
            env,
            "ntdll.dll!NtOpenFile",
            "reparse-point mutation instances are unavailable",
        );
    }
    let file = match adv_os::vfs::FileStream::open_for_mutation(&host, access) {
        Ok(file) => file,
        Err(error) => {
            if error.kind() == std::io::ErrorKind::PermissionDenied
                && env.sys.objects.all_hosts::<FileObj>().any(|(_, file)| {
                    file.guest_path.eq_ignore_ascii_case(path)
                        && file
                            .file
                            .standard_information()
                            .is_ok_and(|info| info.delete_pending)
                })
            {
                return 0xc0000056u32 as i32; // STATUS_DELETE_PENDING
            }
            return failure(env, error);
        }
    };
    // Hardlink/alias names must still honor every existing backing owner.
    let identity = file.cached_identity();
    if env.sys.objects.all_hosts::<FileObj>().any(|(_, owner)| {
        identity.is_some()
            && owner.file.cached_identity() == identity
            && (share_access(access) & !owner.share != 0
                || share_access(owner.access) & !share != 0)
    }) {
        return SHARING_VIOLATION;
    }
    let handle = env.sys.objects.insert_host(FileObj {
        file,
        guest_path: path.into(),
        access,
        share,
    });
    if handle == 0 {
        return INSUFFICIENT_RESOURCES;
    }
    if output.write(env.cpu, handle).is_err() || completion(env, iosb, 0, 1) != 0 {
        env.sys.objects.close(handle);
        return ACCESS_VIOLATION;
    }
    0
}

fn destination(env: &mut Env<'_>, name: &str, source: &str) -> Result<String, i32> {
    let dos = if let Some(path) = name.strip_prefix(r"\??\") {
        if path
            .get(..4)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(r"UNC\"))
        {
            format!(r"\\{}", &path[4..])
        } else if path.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
            && path.as_bytes().get(1..3) == Some(b":\\")
        {
            path.to_owned()
        } else {
            return Err(unavailable_for(
                env,
                "ntdll.dll!NtSetInformationFile",
                "relative NT rename names outside RootDirectory are unavailable",
            ));
        }
    } else if !name.contains(['\\', ':']) {
        let parent = source.rsplit_once('\\').map_or("", |(parent, _)| parent);
        format!(r"{parent}\{name}")
    } else {
        return Err(unavailable_for(
            env,
            "ntdll.dll!NtSetInformationFile",
            "rename destinations require an absolute DOS NT name or a simple sibling name",
        ));
    };
    if dos.contains(['\0', '/', '*', '?'])
        || dos
            .split('\\')
            .any(|part| matches!(part, "." | "..") || part.ends_with([' ', '.']))
    {
        return Err(NAME_INVALID);
    }
    Ok(env.ext.get::<Win32>().vfs.normalize(&dos))
}

pub(super) fn set(
    env: &mut Env<'_>,
    handle: Handle,
    iosb: GuestPtr<()>,
    input: GuestPtr<()>,
    length: u32,
    class: i32,
) -> i32 {
    let minimum = match class {
        13 => 1,
        64 => 4,
        _ => 12,
    };
    if length < minimum {
        return 0xc0000004u32 as i32;
    }
    if check_range(env.cpu, input.addr(), length, Prot::READ).is_err()
        || check_range(env.cpu, iosb.addr(), 8, Prot::WRITE).is_err()
    {
        return ACCESS_VIOLATION;
    }
    let Some(owner) = env.sys.objects.host::<FileObj>(handle.0) else {
        return if env.sys.objects.get(handle.0).is_some() {
            0xc0000024u32 as i32
        } else {
            INVALID_HANDLE
        };
    };
    if owner.access & 0x10000 == 0 {
        return ACCESS_DENIED;
    }
    let source = owner.guest_path.clone();
    let identity = owner.file.cached_identity();
    if identity.is_none() {
        return unavailable_for(
            env,
            "ntdll.dll!NtSetInformationFile",
            "mutation requires the opened backing identity",
        );
    }
    if let Err(status) = writable(env, &source) {
        return status;
    }
    let mut prefix = [0; 12];
    if env
        .cpu
        .read(input.addr(), &mut prefix[..minimum as usize])
        .is_err()
    {
        return ACCESS_VIOLATION;
    }
    if class == 13 {
        let result = owner_delete(env, handle.0, prefix[0] != 0);
        return match result {
            Ok(()) => completion(env, iosb, 0, 0),
            Err(error) => failure(env, error),
        };
    }
    if class == 64 {
        let flags = u32::from_le_bytes(prefix[..4].try_into().unwrap());
        // DO_NOT_DELETE, DELETE and POSIX_SEMANTICS share the directory
        // owner's subset; on-close and read-only overrides need their own model.
        if flags & !3 != 0 {
            return unavailable_for(
                env,
                "ntdll.dll!NtSetInformationFile",
                "extended disposition supports DELETE and POSIX_SEMANTICS only",
            );
        }
        let result = env
            .sys
            .objects
            .host::<FileObj>(handle.0)
            .unwrap()
            .file
            .set_delete_disposition_ex(flags);
        return match result {
            Ok(()) => completion(env, iosb, 0, 0),
            Err(error) => failure(env, error),
        };
    }
    if u32::from_le_bytes(prefix[4..8].try_into().unwrap()) != 0 {
        return unavailable_for(
            env,
            "ntdll.dll!NtSetInformationFile",
            "RootDirectory-relative rename destinations are unavailable",
        );
    }
    let bytes = u32::from_le_bytes(prefix[8..12].try_into().unwrap());
    if bytes > 65534 {
        return 0xc0000106u32 as i32; // STATUS_NAME_TOO_LONG
    }
    if bytes == 0 || bytes & 1 != 0 || bytes > length - 12 {
        return INVALID_PARAMETER;
    }
    let mut data = vec![0; bytes as usize];
    if env.cpu.read(input.addr() + 12, &mut data).is_err() {
        return ACCESS_VIOLATION;
    }
    let units: Vec<_> = data
        .chunks_exact(2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .collect();
    let name = match String::from_utf16(&units) {
        Ok(name) => name,
        Err(_) => return NAME_INVALID,
    };
    let target = match destination(env, &name, &source) {
        Ok(path) => path,
        Err(status) => return status,
    };
    let volume = |path: &str| {
        if let Some(unc) = path.strip_prefix(r"\\") {
            unc.split('\\')
                .take(2)
                .collect::<Vec<_>>()
                .join("\\")
                .to_ascii_lowercase()
        } else {
            path.get(..2).unwrap_or("").to_ascii_lowercase()
        }
    };
    if volume(&source) != volume(&target) {
        return 0xc00000d4u32 as i32; // STATUS_NOT_SAME_DEVICE
    }
    let vfs = &env.ext.get::<Win32>().vfs;
    if prefix[0] == 0 && vfs.lookup(&target).is_some() && !source.eq_ignore_ascii_case(&target) {
        return 0xc0000035u32 as i32;
    }
    let parent = target.rsplit_once('\\').map_or("", |(parent, _)| parent);
    let parent = if parent.len() == 2 {
        format!(r"{parent}\")
    } else {
        parent.into()
    };
    if !vfs.is_dir(&parent) {
        return PATH_NOT_FOUND;
    }
    if let Err(status) = writable(env, &target) {
        return status;
    }
    let Some(mut host) = env.ext.get::<Win32>().vfs.write_path(&target) else {
        return PATH_NOT_FOUND;
    };
    // Preserve the requested leaf spelling for a case-only rename; write_path
    // deliberately reuses existing spelling for ordinary opens.
    if let Some(leaf) = target.rsplit('\\').next() {
        host.set_file_name(leaf);
    }
    let result = env
        .sys
        .objects
        .host::<FileObj>(handle.0)
        .unwrap()
        .file
        .rename_instance(&host, prefix[0] != 0);
    if let Err(error) = result {
        return failure(env, error);
    }
    for (_, file) in env.sys.objects.all_hosts_mut::<FileObj>() {
        if file.guest_path.eq_ignore_ascii_case(&source) && file.file.cached_identity() == identity
        {
            file.guest_path = target.clone();
        }
    }
    completion(env, iosb, 0, 0)
}
fn owner_delete(env: &Env<'_>, handle: u32, delete: bool) -> std::io::Result<()> {
    env.sys
        .objects
        .host::<FileObj>(handle)
        .unwrap()
        .file
        .set_delete_disposition(delete)
}
