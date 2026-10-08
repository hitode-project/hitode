//! Open-instance file metadata and mutations through the shared FileObj owner.
use super::*;
use adv_machine::Outcome;

fn failure(env: &mut Env<'_>, error: std::io::Error) -> i32 {
    match error.kind() {
        std::io::ErrorKind::PermissionDenied => ACCESS_DENIED,
        std::io::ErrorKind::InvalidInput => INVALID_PARAMETER,
        std::io::ErrorKind::OutOfMemory => INSUFFICIENT_RESOURCES,
        std::io::ErrorKind::Unsupported => unavailable_for(
            env,
            "ntdll.dll!<file metadata>",
            "opened-instance metadata is unavailable",
        ),
        _ => 0xc000_0001u32 as i32,
    }
}

fn invalid(env: &Env<'_>, handle: u32) -> i32 {
    if env.sys.objects.get(handle).is_some() {
        0xc000_0024u32 as i32
    } else {
        INVALID_HANDLE
    }
}

fn time(value: std::io::Result<std::time::SystemTime>) -> u64 {
    value
        .ok()
        .and_then(|time| time.duration_since(std::time::SystemTime::UNIX_EPOCH).ok())
        .map_or(0, |duration| {
            116_444_736_000_000_000 + duration.as_micros() as u64 * 10
        })
}

pub(super) fn query(
    env: &mut Env<'_>,
    handle: Handle,
    iosb: GuestPtr<()>,
    output: GuestPtr<()>,
    length: u32,
    class: i32,
) -> i32 {
    let required = if class == 4 { 40 } else { 8 };
    if length < required {
        return 0xc000_0004u32 as i32;
    }
    if check_range(env.cpu, output.addr(), length, Prot::WRITE).is_err()
        || check_range(env.cpu, iosb.addr(), 8, Prot::WRITE).is_err()
    {
        return ACCESS_VIOLATION;
    }
    let Some(file) = env.sys.objects.host::<FileObj>(handle.0) else {
        return invalid(env, handle.0);
    };
    let mut data = [0; 40];
    if class == 6 {
        let Some(backing) = file.file.host() else {
            return unavailable_for(
                env,
                "ntdll.dll!NtQueryInformationFile",
                "virtual-file internal identity is unavailable",
            );
        };
        let fields = match crate::kernel32::file::information::backing_information(backing) {
            Ok(fields) => fields,
            Err(error) => return failure(env, error),
        };
        data[..4].copy_from_slice(&fields[12].to_le_bytes());
        data[4..8].copy_from_slice(&fields[11].to_le_bytes());
    } else {
        let metadata = match file.file.metadata() {
            Ok(metadata) => metadata,
            Err(error) => return failure(env, error),
        };
        let attributes = if let Some(backing) = file.file.host() {
            env.ext.get::<Win32>().vfs.file_handle_attributes(backing)
        } else {
            Ok(0x21)
        };
        let attributes = match attributes {
            Ok(attributes) => attributes,
            Err(error) => return failure(env, error),
        };
        for (offset, value) in [
            (0, time(metadata.created())),
            (8, time(metadata.accessed())),
            (16, time(metadata.modified())),
            (24, time(metadata.modified())),
        ] {
            data[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
        }
        data[32..36].copy_from_slice(&attributes.to_le_bytes());
    }
    if env
        .cpu
        .write(output.addr(), &data[..required as usize])
        .is_err()
    {
        return ACCESS_VIOLATION;
    }
    completion(env, iosb, 0, required)
}

pub(super) fn set(
    env: &mut Env<'_>,
    handle: Handle,
    iosb: GuestPtr<()>,
    input: GuestPtr<()>,
    length: u32,
    class: i32,
) -> i32 {
    let required = if class == 4 { 40 } else { 8 };
    if length < required {
        return 0xc000_0004u32 as i32;
    }
    if check_range(env.cpu, input.addr(), length, Prot::READ).is_err()
        || check_range(env.cpu, iosb.addr(), 8, Prot::WRITE).is_err()
    {
        return ACCESS_VIOLATION;
    }
    let mut data = [0; 40];
    if env
        .cpu
        .read(input.addr(), &mut data[..required as usize])
        .is_err()
    {
        return ACCESS_VIOLATION;
    }
    let Some(file) = env.sys.objects.host::<FileObj>(handle.0) else {
        return invalid(env, handle.0);
    };
    if class == 4 {
        if file.access & (GENERIC_WRITE | 0x1000_0000 | 0x100) == 0 {
            return ACCESS_DENIED;
        }
        let flags = u32::from_le_bytes(data[32..36].try_into().unwrap());
        if data[..32].iter().any(|byte| *byte != 0) || !matches!(flags, 0 | 0x80) {
            return unavailable_for(
                env,
                "ntdll.dll!NtSetInformationFile",
                "only unchanged timestamps and preserved/NORMAL attributes are supported",
            );
        }
        if flags != 0 {
            let Some(backing) = file.file.host() else {
                return ACCESS_DENIED;
            };
            let result = env.ext.get::<Win32>().vfs.set_file_handle_normal(backing);
            if let Err(error) = result {
                return failure(env, error);
            }
        }
    } else {
        if file.access & (GENERIC_WRITE | 0x1000_0000 | 2) == 0 {
            return ACCESS_DENIED;
        }
        let length = i64::from_le_bytes(data[..8].try_into().unwrap());
        if length < 0 {
            return INVALID_PARAMETER;
        }
        let section = file
            .file
            .cached_identity()
            .and_then(|identity| env.ext.get::<Win32>().vfs.sections.lookup(identity));
        if let Some(section) = section
            && adv_os::try_modules(env.ext)
                .and_then(|modules| modules.minimum_data_file_length(&section))
                .is_some_and(|minimum| (length as u64) < minimum)
        {
            return 0xc000_0243u32 as i32;
        }
        if let Err(error) = env
            .sys
            .objects
            .host_mut::<FileObj>(handle.0)
            .unwrap()
            .file
            .set_len(length as u64)
        {
            return failure(env, error);
        }
    }
    completion(env, iosb, 0, 0)
}

pub(crate) fn volume(
    env: &mut Env<'_>,
    handle: Handle,
    iosb: GuestPtr<()>,
    output: GuestPtr<()>,
    length: u32,
    class: i32,
) -> i32 {
    if class != 1 {
        return unavailable_for(
            env,
            "ntdll.dll!NtQueryVolumeInformationFile",
            "only FileFsVolumeInformation is implemented",
        );
    }
    if length < 20 {
        return 0xc000_0004u32 as i32;
    }
    if check_range(env.cpu, output.addr(), length, Prot::WRITE).is_err()
        || check_range(env.cpu, iosb.addr(), 8, Prot::WRITE).is_err()
    {
        return ACCESS_VIOLATION;
    }
    let Some(file) = env.sys.objects.host::<FileObj>(handle.0) else {
        return invalid(env, handle.0);
    };
    let Some(backing) = file.file.host() else {
        return unavailable_for(
            env,
            "ntdll.dll!NtQueryVolumeInformationFile",
            "virtual-file volume identity is unavailable",
        );
    };
    let fields = match crate::kernel32::file::information::backing_information(backing) {
        Ok(fields) => fields,
        Err(error) => return failure(env, error),
    };
    let mut data = [0; 20];
    data[8..12].copy_from_slice(&fields[7].to_le_bytes());
    if env.cpu.write(output.addr(), &data).is_err() {
        return ACCESS_VIOLATION;
    }
    completion(env, iosb, 0, 20)
}

pub(crate) fn flush(env: &mut Env<'_>, handle: Handle, iosb: GuestPtr<()>) -> i32 {
    if check_range(env.cpu, iosb.addr(), 8, Prot::WRITE).is_err() {
        return ACCESS_VIOLATION;
    }
    let Some(file) = env.sys.objects.host::<FileObj>(handle.0) else {
        return invalid(env, handle.0);
    };
    if file.access & (GENERIC_WRITE | 0x10000000 | 2 | 4) == 0 {
        return ACCESS_DENIED;
    }
    let Some(backing) = file.file.host() else {
        return ACCESS_DENIED;
    };
    if let Err(error) = backing.sync_all() {
        return failure(env, error);
    }
    completion(env, iosb, 0, 0)
}

pub(super) fn register(registry: &mut Registry) {
    // Official ntifs.h: NTSTATUS (HANDLE FileHandle, PIO_STATUS_BLOCK IoStatusBlock).
    // https://learn.microsoft.com/windows-hardware/drivers/ddi/ntifs/nf-ntifs-zwflushbuffersfile
    for name in ["NtFlushBuffersFile", "ZwFlushBuffersFile"] {
        crate::stdcall(registry, "ntdll.dll", name, 2, |env| {
            Outcome::Return(flush(env, Handle(env.arg(0)), GuestPtr::new(env.arg(1))) as u32)
        });
    }
    // Official ntifs.h contract: HANDLE, IOSB*, PVOID, ULONG, FS_INFORMATION_CLASS.
    // https://learn.microsoft.com/windows-hardware/drivers/ddi/ntifs/nf-ntifs-zwqueryvolumeinformationfile
    for name in [
        "NtQueryVolumeInformationFile",
        "ZwQueryVolumeInformationFile",
    ] {
        crate::stdcall(registry, "ntdll.dll", name, 5, |env| {
            let args: [u32; 5] = std::array::from_fn(|index| env.arg(index as u32));
            Outcome::Return(volume(
                env,
                Handle(args[0]),
                GuestPtr::new(args[1]),
                GuestPtr::new(args[2]),
                args[3],
                args[4] as i32,
            ) as u32)
        });
    }
}
