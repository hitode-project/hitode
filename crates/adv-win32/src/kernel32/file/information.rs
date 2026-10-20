//! File information from the opened backing instance, independent of its name.
use super::*;
use adv_abi::{GuestPtr, Handle, win32};

const INFORMATION_SIZE: u32 = 52;

#[cfg(windows)]
pub(crate) fn backing_information(file: &std::fs::File) -> std::io::Result<[u32; 13]> {
    use std::os::windows::io::AsRawHandle;
    #[repr(C)]
    struct Information {
        fields: [u32; 13],
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetFileInformationByHandle(
            handle: *mut std::ffi::c_void,
            information: *mut Information,
        ) -> i32;
    }
    let mut information = Information { fields: [0; 13] };
    // SAFETY: the file owns a live native handle and the all-DWORD structure
    // has the documented 52-byte layout and remains writable for the call.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut information) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(information.fields)
}

#[cfg(unix)]
pub(crate) fn backing_information(file: &std::fs::File) -> std::io::Result<[u32; 13]> {
    use std::os::unix::fs::MetadataExt;
    let metadata = file.metadata()?;
    let filetime = |time: std::io::Result<SystemTime>| -> u64 {
        const UNIX_EPOCH: u64 = 116_444_736_000_000_000;
        time.ok()
            .and_then(|time| match time.duration_since(SystemTime::UNIX_EPOCH) {
                Ok(duration) => UNIX_EPOCH.checked_add((duration.as_nanos() / 100) as u64),
                Err(error) => UNIX_EPOCH.checked_sub((error.duration().as_nanos() / 100) as u64),
            })
            .unwrap_or(0)
    };
    let created = filetime(metadata.created().or_else(|_| metadata.modified()));
    let accessed = filetime(metadata.accessed());
    let modified = filetime(metadata.modified());
    let links = u32::try_from(metadata.nlink()).map_err(|_| std::io::ErrorKind::Unsupported)?;
    Ok([
        0,
        created as u32,
        (created >> 32) as u32,
        accessed as u32,
        (accessed >> 32) as u32,
        modified as u32,
        (modified >> 32) as u32,
        metadata.dev() as u32,
        (metadata.len() >> 32) as u32,
        metadata.len() as u32,
        links,
        (metadata.ino() >> 32) as u32,
        metadata.ino() as u32,
    ])
}

#[cfg(not(any(windows, unix)))]
pub(crate) fn backing_information(_: &std::fs::File) -> std::io::Result<[u32; 13]> {
    Err(std::io::ErrorKind::Unsupported.into())
}

#[win32(kernel32, name = "GetFileInformationByHandle", level = implement)]
fn get_file_information(env: &mut Env<'_>, handle: Handle, output: GuestPtr<u8>) -> Outcome {
    if native::metadata::active(
        env,
        &["NtQueryInformationFile", "NtQueryVolumeInformationFile"],
    ) {
        return native::metadata::information(env, handle.0, output.addr());
    }
    let Some(file) = env.sys.objects.host::<FileObj>(handle.0) else {
        set_last_error(env, err::INVALID_HANDLE);
        return Outcome::Return(0);
    };
    // The volume query comes first, and the null device has no volume.
    if file.file.is_null_device() {
        set_last_error(env, 1); // ERROR_INVALID_FUNCTION
        return Outcome::Return(0);
    }
    if output.addr() == 0 {
        set_last_error(env, err::INVALID_PARAMETER);
        return Outcome::Return(0);
    }
    if adv_abi::guest::check_range(
        env.cpu,
        output.addr(),
        INFORMATION_SIZE,
        adv_cpu::backend::Prot::WRITE,
    )
    .is_err()
    {
        set_last_error(env, 998); // ERROR_NOACCESS
        return Outcome::Return(0);
    }
    let Some(backing) = file.file.host() else {
        adv_abi::warn_unavailable(
            env,
            "kernel32.dll!GetFileInformationByHandle",
            "file identity and link metadata for virtual file handles are unavailable",
        );
        set_last_error(env, 50); // ERROR_NOT_SUPPORTED
        return Outcome::Return(0);
    };
    let information = backing_information(backing).and_then(|mut information| {
        // Attribute state belongs to the opened instance, just like size,
        // timestamps, link count, and ID. A path lookup can target a new file.
        information[0] = env.ext.get::<Win32>().vfs.file_handle_attributes(backing)?;
        Ok(information)
    });
    let information = match information {
        Ok(information) => information,
        Err(error) => {
            if error.kind() == std::io::ErrorKind::Unsupported {
                adv_abi::warn_unavailable(
                    env,
                    "kernel32.dll!GetFileInformationByHandle",
                    "backing-file identity or link metadata is unavailable",
                );
                set_last_error(env, 50);
            } else {
                io_error(env, &error);
            }
            return Outcome::Return(0);
        }
    };
    let mut bytes = [0; INFORMATION_SIZE as usize];
    for (field, output) in information
        .iter()
        .zip(bytes.as_chunks_mut::<4>().0.iter_mut())
    {
        output.copy_from_slice(&field.to_le_bytes());
    }
    if env.cpu.write(output.addr(), &bytes).is_err() {
        set_last_error(env, 998);
        return Outcome::Return(0);
    }
    crate::native::diagnose_bypass(
        env,
        "kernel32.dll!GetFileInformationByHandle",
        &["NtQueryInformationFile"],
    );
    Outcome::Return(1)
}

pub(super) fn register(registry: &mut Registry) {
    registry.register(get_file_information_ABI);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel32::file::locking::tests::{args, env, fixture, returned};

    #[cfg(windows)]
    fn sparse(file: &std::fs::File) {
        use std::os::windows::io::AsRawHandle;
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn DeviceIoControl(
                handle: *mut std::ffi::c_void,
                code: u32,
                input: *mut std::ffi::c_void,
                input_size: u32,
                output: *mut std::ffi::c_void,
                output_size: u32,
                returned: *mut u32,
                overlapped: *mut std::ffi::c_void,
            ) -> i32;
        }
        let mut returned = 0;
        // SAFETY: the authored fixture owns the live handle; FSCTL_SET_SPARSE
        // has no input/output buffer and receives one writable DWORD.
        let result = unsafe {
            DeviceIoControl(
                file.as_raw_handle(),
                0x900c4,
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                0,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        assert_ne!(result, 0, "{}", std::io::Error::last_os_error());
    }

    fn query(machine: &mut adv_machine::Machine<adv_cpu::JitCpu>, handle: u32) -> [u32; 13] {
        args(machine, &[handle, 0x410100]);
        assert_eq!(
            returned((get_file_information_ABI.handler)(&mut env(machine))),
            1
        );
        std::array::from_fn(|index| machine.cpu.read_u32(0x410100 + index as u32 * 4).unwrap())
    }

    #[test]
    fn file_information_tracks_opened_instances_attributes_links_and_64bit_sizes() {
        let (mut machine, root) = fixture("information");
        let path = root.join("game/data.bin");
        std::fs::write(&path, b"original data").unwrap();
        let first = open(
            &mut env(&mut machine),
            "data.bin",
            0x8000_0000,
            OPEN_EXISTING,
        );
        let second = open(
            &mut env(&mut machine),
            "DATA.BIN",
            0x8000_0000,
            OPEN_EXISTING,
        );
        assert_ne!(first, INVALID_HANDLE_VALUE);
        assert_ne!(second, INVALID_HANDLE_VALUE);
        let baseline = query(&mut machine, first);
        assert_eq!(baseline[0], FILE_ATTRIBUTE_ARCHIVE);
        assert_eq!((baseline[8], baseline[9]), (0, 13));
        assert_eq!(baseline[10], 1);
        assert_eq!(query(&mut machine, second)[7..], baseline[7..]);
        let alias = machine.sys.objects.duplicate(first).unwrap();
        assert_eq!(query(&mut machine, alias)[7..], baseline[7..]);
        machine
            .ext
            .get::<Win32>()
            .vfs
            .set_file_normal("data.bin")
            .unwrap();
        assert_eq!(query(&mut machine, first)[0], 0x80);
        // The adb shell SELinux domain on Android denies links; the product never links.
        if let Err(error) = std::fs::hard_link(&path, root.join("game/alias.bin")) {
            assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
            return;
        }
        adv_os::vfs::host::changed(&root.join("game/alias.bin"));
        let linked = open(
            &mut env(&mut machine),
            "alias.bin",
            0x8000_0000,
            OPEN_EXISTING,
        );
        let linked_information = query(&mut machine, linked);
        assert_eq!(linked_information[10], 2);
        assert_eq!(linked_information[7], baseline[7]);
        assert_eq!(linked_information[11..], baseline[11..]);
        // Renaming the backing file and recreating its old name must not make
        // the existing handle query the replacement instance.
        adv_os::vfs::host::rename(&path, &root.join("game/renamed.bin")).unwrap();
        adv_os::vfs::host::write(&path, b"other").unwrap();
        assert_eq!(query(&mut machine, first)[9], 13);
        assert_eq!(query(&mut machine, first)[0], 0x80);
        let replacement = open(&mut env(&mut machine), "data.bin", 0, OPEN_EXISTING);
        let replacement_information = query(&mut machine, replacement);
        assert_eq!(replacement_information[0], FILE_ATTRIBUTE_ARCHIVE);
        assert_eq!(replacement_information[9], 5);
        assert_ne!(replacement_information[11..], baseline[11..]);
        let large = open(
            &mut env(&mut machine),
            "large.bin",
            GENERIC_WRITE,
            CREATE_NEW,
        );
        assert_ne!(large, INVALID_HANDLE_VALUE);
        #[cfg(windows)]
        sparse(
            machine
                .sys
                .objects
                .host::<FileObj>(large)
                .unwrap()
                .file
                .host()
                .unwrap(),
        );
        machine
            .sys
            .objects
            .host::<FileObj>(large)
            .unwrap()
            .file
            .set_len((1u64 << 32) + 17)
            .unwrap();
        let large_information = query(&mut machine, large);
        assert_eq!((large_information[8], large_information[9]), (1, 17));
        assert_ne!(large_information[11..], baseline[11..]);
        drop(machine);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn file_information_failure_preserves_output_and_success_preserves_last_error() {
        let (mut machine, root) = fixture("information-validation");
        std::fs::write(root.join("game/data.bin"), b"data").unwrap();
        let file = open(&mut env(&mut machine), "data.bin", 0, OPEN_EXISTING);
        assert_ne!(file, INVALID_HANDLE_VALUE);
        machine.cpu.write(0x410100, &[0xab; 52]).unwrap();
        machine
            .cpu
            .map(0x420000, 4096, adv_cpu::backend::Prot::READ)
            .unwrap();
        for (handle, output, error) in [
            (INVALID_HANDLE_VALUE, 0x410100, 6),
            (STD_OUTPUT, 0x410100, 6),
            (file, 0, 87),
            (file, 0x410ff0, 998),
            (file, u32::MAX - 7, 998),
            (file, 0x420000, 998),
        ] {
            args(&mut machine, &[handle, output]);
            assert_eq!(
                returned((get_file_information_ABI.handler)(&mut env(&mut machine))),
                0
            );
            assert_eq!(crate::util::last_error(&env(&mut machine)), error);
            let mut bytes = [0; 52];
            machine.cpu.read(0x410100, &mut bytes).unwrap();
            assert_eq!(bytes, [0xab; 52]);
        }
        set_last_error(&mut env(&mut machine), 0x1234);
        assert_eq!(query(&mut machine, file)[9], 4);
        assert_eq!(crate::util::last_error(&env(&mut machine)), 0x1234);
        let virtual_file = machine.sys.objects.insert_host(FileObj {
            file: adv_os::vfs::FileStream::read_only(std::sync::Arc::from(*b"memory")),
            guest_path: "C:\\Game\\memory.bin".into(),
            access: 0x8000_0000,
            share: 7,
        });
        args(&mut machine, &[virtual_file, 0x410100]);
        assert_eq!(
            returned((get_file_information_ABI.handler)(&mut env(&mut machine))),
            0
        );
        assert_eq!(crate::util::last_error(&env(&mut machine)), 50);
        drop(machine);
        std::fs::remove_dir_all(root).unwrap();
    }
}
