//! Directory creation, mutation and enumeration share one backing/cursor owner.
use super::enumeration::DirectoryObj;
use super::*;
use adv_os::vfs::FileStream;

pub(crate) fn contains(env: &Env<'_>, handle: u32) -> bool {
    env.sys.objects.host::<DirectoryObj>(handle).is_some()
}
fn failure(error: &std::io::Error) -> i32 {
    match error.raw_os_error().filter(|_| cfg!(windows)) {
        Some(32 | 33) => SHARING_VIOLATION,
        Some(123) => NAME_INVALID,
        Some(80 | 183) => 0xc0000035u32 as i32,
        Some(145) => 0xc0000101u32 as i32,
        Some(17) => 0xc00000d4u32 as i32,
        Some(303) => 0xc0000056u32 as i32,
        _ => match error.kind() {
            std::io::ErrorKind::NotFound => NAME_NOT_FOUND,
            std::io::ErrorKind::PermissionDenied => ACCESS_DENIED,
            std::io::ErrorKind::AlreadyExists => 0xc0000035u32 as i32,
            std::io::ErrorKind::DirectoryNotEmpty => 0xc0000101u32 as i32,
            std::io::ErrorKind::Unsupported => NOT_SUPPORTED,
            std::io::ErrorKind::OutOfMemory => INSUFFICIENT_RESOURCES,
            _ => 0xc0000001u32 as i32,
        },
    }
}
fn mutable(env: &mut Env<'_>, path: &str) -> Result<(), i32> {
    let vfs = &env.ext.get::<Win32>().vfs;
    if vfs.is_optical_path(path) {
        return Err(0xc00000a2u32 as i32);
    }
    if vfs.has_read_only_underlay(path) {
        return Err(unavailable_for(
            env,
            "ntdll.dll!<directory mutation>",
            "directory mutation over read-only VFS layers requires persistent whiteouts",
        ));
    }
    Ok(())
}
pub(crate) fn open(
    env: &mut Env<'_>,
    path: &str,
    access: u32,
    share: u32,
    disposition: u32,
    options: u32,
    output: GuestPtr<u32>,
    iosb: GuestPtr<()>,
) -> i32 {
    if !matches!(disposition, 1..=3)
        || options & !0x204021 != 0
        || (options & 0x20 == 0 && !(disposition == 1 && access & 0x10000 != 0))
        || disposition != 1 && options & 1 == 0
        || access & !0x80130089 != 0
    {
        return unavailable_for(
            env,
            "ntdll.dll!NtCreateFile",
            "only synchronous directory OPEN/CREATE/OPEN_IF with list/attributes/delete rights are implemented",
        );
    }
    if !env.sys.objects.can_insert() {
        return INSUFFICIENT_RESOURCES;
    }
    #[cfg(windows)]
    if options & 0x200000 != 0 || access & 0x10000 != 0 {
        use std::os::windows::fs::MetadataExt;
        if env.ext.get::<Win32>().vfs.lookup(path).is_some_and(|host| {
            std::fs::symlink_metadata(host)
                .is_ok_and(|metadata| metadata.file_attributes() & 0x400 != 0)
        }) {
            return unavailable_for(
                env,
                "ntdll.dll!NtCreateFile",
                "opened reparse-point directory mutation is unavailable",
            );
        }
    }
    let exists = env.ext.get::<Win32>().vfs.is_dir(path)
        || env.ext.get::<Win32>().vfs.lookup(path).is_some();
    let is_directory = env.ext.get::<Win32>().vfs.is_dir(path);
    if exists && disposition == 2 {
        return 0xc0000035u32 as i32;
    }
    if exists && !is_directory {
        return 0xc0000103u32 as i32;
    }
    if !exists && disposition == 1 {
        let parent = path.rsplit_once('\\').map_or("", |(parent, _)| parent);
        let parent = if parent.len() == 2 {
            format!("{parent}\\")
        } else {
            parent.into()
        };
        return if env.ext.get::<Win32>().vfs.is_dir(&parent) {
            NAME_NOT_FOUND
        } else {
            PATH_NOT_FOUND
        };
    }
    if access & 0x10000 != 0
        && let Err(status) = mutable(env, path)
    {
        return status;
    }
    if env
        .sys
        .objects
        .all_hosts::<DirectoryObj>()
        .any(|(_, owner)| {
            owner.path.eq_ignore_ascii_case(path)
                && owner.backing.as_ref().is_some_and(|backing| {
                    backing
                        .standard_information()
                        .is_ok_and(|info| info.delete_pending)
                })
        })
    {
        return 0xc0000056u32 as i32;
    }
    // The same real identity enforces sharing even across independent opens.
    let identity = env.ext.get::<Win32>().vfs.lookup(path).and_then(|host| {
        FileStream::open_directory_instance(&host, 0x100080)
            .ok()
            .and_then(|file| file.cached_identity())
    });
    if env
        .sys
        .objects
        .all_hosts::<DirectoryObj>()
        .any(|(_, owner)| {
            let same = identity.is_some_and(|id| {
                owner.backing.as_ref().and_then(FileStream::cached_identity) == Some(id)
            }) || owner.path.eq_ignore_ascii_case(path);
            same && (share_access(access) & !owner.share != 0
                || share_access(owner.access) & !share != 0)
        })
    {
        return SHARING_VIOLATION;
    }
    let created = !exists;
    if created {
        if let Err(code) = crate::kernel32::create_directory_path(env, path) {
            return match code {
                2 | 3 => PATH_NOT_FOUND,
                5 => ACCESS_DENIED,
                8 => INSUFFICIENT_RESOURCES,
                183 => 0xc0000035u32 as i32,
                19 => 0xc00000a2u32 as i32,
                123 => NAME_INVALID,
                _ => 0xc0000001u32 as i32,
            };
        }
    }
    let backing = match env.ext.get::<Win32>().vfs.lookup(path) {
        Some(host) => match FileStream::open_directory_instance(&host, access) {
            Ok(file) => Some(file),
            Err(error) => return failure(&error),
        },
        None if access & 0x10000 == 0 && !created => None,
        None => return NOT_SUPPORTED,
    };
    let handle = env.sys.objects.insert_host(DirectoryObj {
        path: path.into(),
        entries: None,
        next: 0,
        access,
        share,
        backing,
    });
    if handle == 0 {
        return INSUFFICIENT_RESOURCES;
    }
    if output.write(env.cpu, handle).is_err()
        || completion(env, iosb, 0, if created { 2 } else { 1 }) != 0
    {
        env.sys.objects.close(handle);
        return ACCESS_VIOLATION;
    }
    0
}

pub(crate) fn query(
    env: &mut Env<'_>,
    handle: Handle,
    iosb: GuestPtr<()>,
    output: GuestPtr<()>,
    length: u32,
    class: i32,
) -> i32 {
    if class != 5 {
        return unavailable_for(
            env,
            "ntdll.dll!NtQueryInformationFile",
            "directory information currently supports standard opened-instance metadata",
        );
    }
    if length < 24 {
        return 0xc0000004u32 as i32;
    }
    if check_range(env.cpu, iosb.addr(), 8, Prot::WRITE).is_err()
        || check_range(env.cpu, output.addr(), 24, Prot::WRITE).is_err()
    {
        return ACCESS_VIOLATION;
    }
    let owner = env.sys.objects.host::<DirectoryObj>(handle.0).unwrap();
    let Some(backing) = &owner.backing else {
        return unavailable_for(
            env,
            "ntdll.dll!NtQueryInformationFile",
            "virtual directories have no opened-instance standard information",
        );
    };
    let standard = match backing.standard_information() {
        Ok(info) => info,
        Err(error) => return failure(&error),
    };
    let mut bytes = [0; 24];
    bytes[..8].copy_from_slice(&standard.allocation_size.to_le_bytes());
    bytes[8..16].copy_from_slice(&standard.end_of_file.to_le_bytes());
    bytes[16..20].copy_from_slice(&standard.number_of_links.to_le_bytes());
    bytes[20] = u8::from(standard.delete_pending);
    bytes[21] = u8::from(standard.directory);
    if env.cpu.write(output.addr(), &bytes).is_err() {
        return ACCESS_VIOLATION;
    }
    completion(env, iosb, 0, 24)
}

fn destination(env: &mut Env<'_>, name: &str, source: &str) -> Result<String, i32> {
    let path = if let Some(dos) = name.strip_prefix(r"\??\") {
        if dos
            .get(..4)
            .is_some_and(|p| p.eq_ignore_ascii_case(r"UNC\"))
        {
            format!(r"\\{}", &dos[4..])
        } else if dos.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
            && dos.as_bytes().get(1..3) == Some(b":\\")
        {
            dos.into()
        } else {
            return Err(NAME_INVALID);
        }
    } else if !name.contains(['\\', ':']) {
        format!(
            "{}\\{name}",
            source.rsplit_once('\\').map_or("", |(parent, _)| parent)
        )
    } else {
        return Err(unavailable_for(
            env,
            "ntdll.dll!NtSetInformationFile",
            "directory rename requires an absolute DOS NT name or simple sibling name",
        ));
    };
    if path.contains(['\0', '/', '*', '?'])
        || path
            .split('\\')
            .any(|part| matches!(part, "." | "..") || part.ends_with([' ', '.']))
    {
        return Err(NAME_INVALID);
    }
    Ok(env.ext.get::<Win32>().vfs.normalize(&path))
}
fn retarget(path: &str, source: &str, target: &str) -> Option<String> {
    if path.eq_ignore_ascii_case(source) {
        return Some(target.into());
    }
    if path
        .get(..source.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(source))
        && path.as_bytes().get(source.len()) == Some(&b'\\')
    {
        return Some(format!("{target}{}", &path[source.len()..]));
    }
    None
}
pub(crate) fn set(
    env: &mut Env<'_>,
    handle: Handle,
    iosb: GuestPtr<()>,
    input: GuestPtr<()>,
    length: u32,
    class: i32,
) -> i32 {
    let minimum = if class == 13 {
        1
    } else if class == 64 {
        4
    } else if class == 10 {
        12
    } else {
        return NOT_SUPPORTED;
    };
    if length < minimum {
        return 0xc0000004u32 as i32;
    }
    if check_range(env.cpu, iosb.addr(), 8, Prot::WRITE).is_err()
        || check_range(env.cpu, input.addr(), length, Prot::READ).is_err()
    {
        return ACCESS_VIOLATION;
    }
    let owner = env.sys.objects.host::<DirectoryObj>(handle.0).unwrap();
    if owner.access & 0x10000 == 0 {
        return ACCESS_DENIED;
    }
    let source = owner.path.clone();
    if owner.backing.is_none() {
        return unavailable_for(
            env,
            "ntdll.dll!NtSetInformationFile",
            "directory mutation requires its opened backing instance",
        );
    }
    if let Err(status) = mutable(env, &source) {
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
    if matches!(class, 13 | 64) {
        let flags = if class == 64 {
            u32::from_le_bytes(prefix[..4].try_into().unwrap())
        } else {
            u32::from(prefix[0] != 0)
        };
        if class == 64 && flags & !3 != 0 {
            return unavailable_for(
                env,
                "ntdll.dll!NtSetInformationFile",
                "directory Ex disposition supports DELETE and POSIX_SEMANTICS only",
            );
        }
        if flags & 1 != 0
            && source
                .trim_end_matches('\\')
                .eq_ignore_ascii_case(env.ext.get::<Win32>().vfs.cwd.trim_end_matches('\\'))
        {
            return SHARING_VIOLATION;
        }
        let backing = env
            .sys
            .objects
            .host::<DirectoryObj>(handle.0)
            .unwrap()
            .backing
            .as_ref()
            .unwrap();
        let result = if class == 64 {
            backing.set_delete_disposition_ex(flags)
        } else {
            backing.set_delete_disposition(flags & 1 != 0)
        };
        let status = match result {
            Ok(()) => 0,
            Err(error) => failure(&error),
        };
        // The authored native nonempty-directory control completes IOSB too.
        return completion(env, iosb, status, 0);
    }
    if prefix[0] != 0 || u32::from_le_bytes(prefix[4..8].try_into().unwrap()) != 0 {
        return unavailable_for(
            env,
            "ntdll.dll!NtSetInformationFile",
            "directory rename replacement and relative RootDirectory are unavailable",
        );
    }
    let count = u32::from_le_bytes(prefix[8..12].try_into().unwrap());
    if count > 65534 {
        return 0xc0000106u32 as i32;
    }
    if count == 0 || count & 1 != 0 || count > length - 12 {
        return INVALID_PARAMETER;
    }
    let mut bytes = vec![0; count as usize];
    if env.cpu.read(input.addr() + 12, &mut bytes).is_err() {
        return ACCESS_VIOLATION;
    }
    let units: Vec<_> = bytes
        .chunks_exact(2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .collect();
    let name = match String::from_utf16(&units) {
        Ok(name) => name,
        Err(_) => return NAME_INVALID,
    };
    let target = match destination(env, &name, &source) {
        Ok(target) => target,
        Err(status) => return status,
    };
    if target != source && retarget(&target, &source, &source).is_some() {
        return INVALID_PARAMETER;
    }
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
        return 0xc00000d4u32 as i32;
    }
    if env.ext.get::<Win32>().vfs.lookup(&target).is_some() && !source.eq_ignore_ascii_case(&target)
    {
        return 0xc0000035u32 as i32;
    }
    let parent = target.rsplit_once('\\').map_or("", |(parent, _)| parent);
    let parent = if parent.len() == 2 {
        format!(r"{parent}\")
    } else {
        parent.into()
    };
    if !env.ext.get::<Win32>().vfs.is_dir(&parent) {
        return PATH_NOT_FOUND;
    }
    if let Err(status) = mutable(env, &target) {
        return status;
    }
    let Some(mut host) = env.ext.get::<Win32>().vfs.write_path(&target) else {
        return PATH_NOT_FOUND;
    };
    if let Some(leaf) = target.rsplit('\\').next() {
        host.set_file_name(leaf);
    }
    let host = match host
        .parent()
        .and_then(|parent| std::fs::canonicalize(parent).ok())
        .zip(host.file_name())
    {
        Some((parent, leaf)) => parent.join(leaf),
        None => return PATH_NOT_FOUND,
    };
    let result = env
        .sys
        .objects
        .host::<DirectoryObj>(handle.0)
        .unwrap()
        .backing
        .as_ref()
        .unwrap()
        .rename_instance(&host);
    if let Err(error) = result {
        return completion(env, iosb, failure(&error), 0);
    }
    retarget_owners(env, &source, &target);
    completion(env, iosb, 0, 0)
}

pub(crate) fn retarget_owners(env: &mut Env<'_>, source: &str, target: &str) {
    for (_, owner) in env.sys.objects.all_hosts_mut::<DirectoryObj>() {
        if let Some(path) = retarget(&owner.path, &source, &target) {
            owner.path = path;
        }
    }
    for (_, owner) in env.sys.objects.all_hosts_mut::<FileObj>() {
        if let Some(path) = retarget(&owner.guest_path, &source, &target) {
            owner.guest_path = path;
        }
    }
    if let Some(path) = retarget(&env.ext.get::<Win32>().vfs.cwd, &source, &target) {
        env.ext.get_mut::<Win32>().vfs.cwd = path;
    }
}
