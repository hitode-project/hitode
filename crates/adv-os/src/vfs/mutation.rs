//! Instance-based host mutation adapters. Unsupported platforms fail explicitly.
use super::FileStream;
use std::{io, path::Path};

/// Open the existing backing instance with DELETE, never copy it or create it.
pub(super) fn open(path: &Path, access: u32) -> io::Result<FileStream> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // Guest share policy is enforced against all FileObj owners. Host
        // handles use permissive sharing so they do not add another policy.
        options.access_mode(access).share_mode(7);
    }
    #[cfg(not(windows))]
    let _ = access;
    options.open(path).map(FileStream::from)
}

#[cfg(windows)]
fn set(file: &std::fs::File, class: i32, bytes: &[u8]) -> io::Result<()> {
    use std::os::windows::io::AsRawHandle;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn SetFileInformationByHandle(
            file: *mut std::ffi::c_void,
            class: i32,
            data: *const std::ffi::c_void,
            bytes: u32,
        ) -> i32;
    }
    // SAFETY: File owns the live instance; callers provide the documented,
    // initialized layout for this class and the exact writable storage length.
    if unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle(),
            class,
            bytes.as_ptr().cast(),
            bytes.len() as u32,
        )
    } == 0
    {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

impl FileStream {
    pub fn open_for_mutation(path: &Path, access: u32) -> io::Result<Self> {
        open(path, access)
    }

    /// Keep enumeration and mutation aliases on the same real directory FD.
    pub fn open_directory_instance(path: &Path, access: u32) -> io::Result<Self> {
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .access_mode(access)
                .share_mode(7)
                .custom_flags(0x0200_0000) // FILE_FLAG_BACKUP_SEMANTICS
                .open(path)
                .map(Self::from)
        }
        #[cfg(not(windows))]
        {
            let _ = access;
            std::fs::File::open(path).map(Self::from)
        }
    }

    /// Native delete-pending belongs to the opened FD, including its aliases.
    pub fn set_delete_disposition(&self, delete: bool) -> io::Result<()> {
        let file = self.host().ok_or(io::ErrorKind::PermissionDenied)?;
        #[cfg(windows)]
        {
            // FILE_DISPOSITION_INFO contains one BOOL, unlike NT's BOOLEAN.
            set(file, 4, &i32::from(delete).to_le_bytes())
        }
        #[cfg(not(windows))]
        {
            let _ = (file, delete);
            // POSIX unlink cannot stand in for NT's cancellable pending delete.
            Err(io::ErrorKind::Unsupported.into())
        }
    }

    /// FILE_DISPOSITION_INFO_EX uses one DWORD Flags, not classic BOOL.
    pub fn set_delete_disposition_ex(&self, flags: u32) -> io::Result<()> {
        let file = self.host().ok_or(io::ErrorKind::PermissionDenied)?;
        #[cfg(windows)]
        {
            set(file, 21, &flags.to_le_bytes())
        }
        #[cfg(not(windows))]
        {
            let _ = (file, flags);
            Err(io::ErrorKind::Unsupported.into())
        }
    }

    /// Rename this instance using host FILE_RENAME_INFO, never a source lookup.
    pub fn rename_instance(&self, target: &Path) -> io::Result<()> {
        let file = self.host().ok_or(io::ErrorKind::PermissionDenied)?;
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            #[repr(C)]
            struct Header {
                replace: u32,
                root: usize,
                length: u32,
                name: [u16; 1],
            }
            let name: Vec<_> = std::path::absolute(target)?
                .as_os_str()
                .encode_wide()
                .collect();
            let offset = std::mem::offset_of!(Header, name);
            let length = offset
                .checked_add(name.len() * 2)
                .ok_or(io::ErrorKind::OutOfMemory)?;
            // u64 storage provides host structure alignment on x86/x64/ARM64.
            let mut storage = vec![0u64; length.div_ceil(8)];
            // SAFETY: the byte view remains within initialized owned storage.
            let bytes = unsafe {
                std::slice::from_raw_parts_mut(storage.as_mut_ptr().cast::<u8>(), length)
            };
            let length_offset = std::mem::offset_of!(Header, length);
            bytes[length_offset..length_offset + 4]
                .copy_from_slice(&(name.len() as u32 * 2).to_le_bytes());
            for (index, unit) in name.iter().enumerate() {
                bytes[offset + 2 * index..offset + 2 * index + 2]
                    .copy_from_slice(&unit.to_le_bytes());
            }
            set(file, 3, bytes)
        }
        #[cfg(not(windows))]
        {
            let _ = (file, target);
            // A path rename cannot preserve this FD's identity/no-replace
            // contract against concurrent replacement on every POSIX target.
            Err(io::ErrorKind::Unsupported.into())
        }
    }
}
