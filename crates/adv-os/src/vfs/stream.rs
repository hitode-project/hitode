//! File-handle storage shared by ordinary files and read-only virtual images.
use super::{FileIdentity, file_identity};
use std::io::{self, Cursor, Read, Seek, SeekFrom, Write};
use std::sync::Arc;
use std::time::SystemTime;

/// One file owner caches identity at open/reopen, never in the ordinary I/O
/// loop. A replacement FileStream gets a new identity automatically.
pub struct HostFile {
    file: std::fs::File,
    identity: Option<FileIdentity>,
}

pub enum FileStream {
    Host(HostFile),
    ReadOnly(Cursor<Arc<[u8]>>),
    Optical(super::OpticalFile),
    /// The null device: reads end immediately and writes are discarded. Its
    /// file position is never retained, so every position query reports zero.
    Null,
}

/// Standard metadata for the opened file instance, independent of its path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StandardInformation {
    pub allocation_size: u64,
    pub end_of_file: u64,
    pub number_of_links: u32,
    pub delete_pending: bool,
    pub directory: bool,
}

#[cfg(windows)]
fn host_standard_information(file: &std::fs::File) -> io::Result<StandardInformation> {
    use std::os::windows::io::AsRawHandle;
    #[repr(C)]
    struct NativeStandardInformation {
        allocation_size: i64,
        end_of_file: i64,
        number_of_links: u32,
        delete_pending: u8,
        directory: u8,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetFileInformationByHandleEx(
            file: *mut std::ffi::c_void,
            class: i32,
            information: *mut std::ffi::c_void,
            size: u32,
        ) -> i32;
    }
    let mut information = NativeStandardInformation {
        allocation_size: 0,
        end_of_file: 0,
        number_of_links: 0,
        delete_pending: 0,
        directory: 0,
    };
    // SAFETY: the File owns a live handle and FileStandardInfo (1) writes the
    // documented FILE_STANDARD_INFO layout into this writable aligned value.
    if unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            1,
            (&mut information as *mut NativeStandardInformation).cast(),
            std::mem::size_of::<NativeStandardInformation>() as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(StandardInformation {
        allocation_size: u64::try_from(information.allocation_size)
            .map_err(|_| io::ErrorKind::InvalidData)?,
        end_of_file: u64::try_from(information.end_of_file)
            .map_err(|_| io::ErrorKind::InvalidData)?,
        number_of_links: information.number_of_links,
        delete_pending: information.delete_pending != 0,
        directory: information.directory != 0,
    })
}

#[cfg(unix)]
fn host_standard_information(file: &std::fs::File) -> io::Result<StandardInformation> {
    use std::os::unix::fs::MetadataExt;
    let metadata = file.metadata()?;
    Ok(StandardInformation {
        // st_blocks counts allocated 512-byte units, including sparse files.
        allocation_size: metadata
            .blocks()
            .checked_mul(512)
            .ok_or(io::ErrorKind::Unsupported)?,
        end_of_file: metadata.len(),
        number_of_links: u32::try_from(metadata.nlink()).map_err(|_| io::ErrorKind::Unsupported)?,
        // POSIX unlinked objects have no remaining names and are reclaimed
        // after their final owner closes; there is no separate pending queue.
        delete_pending: metadata.nlink() == 0,
        directory: metadata.is_dir(),
    })
}

#[cfg(not(any(windows, unix)))]
fn host_standard_information(_: &std::fs::File) -> io::Result<StandardInformation> {
    Err(io::ErrorKind::Unsupported.into())
}

pub enum Metadata {
    Host(std::fs::Metadata),
    ReadOnly(u64),
    Optical(u64, Option<SystemTime>),
}
impl Metadata {
    pub fn len(&self) -> u64 {
        match self {
            Self::Host(info) => info.len(),
            Self::ReadOnly(len) => *len,
            Self::Optical(len, _) => *len,
        }
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn created(&self) -> io::Result<SystemTime> {
        match self {
            // Hosts without birth times (Android f2fs) report the last write.
            Self::Host(info) => info.created().or_else(|_| info.modified()),
            Self::ReadOnly(_) => Ok(SystemTime::UNIX_EPOCH),
            Self::Optical(_, time) => Ok(time.unwrap_or(SystemTime::UNIX_EPOCH)),
        }
    }
    pub fn accessed(&self) -> io::Result<SystemTime> {
        match self {
            Self::Host(info) => info.accessed(),
            Self::ReadOnly(_) => Ok(SystemTime::UNIX_EPOCH),
            Self::Optical(_, time) => Ok(time.unwrap_or(SystemTime::UNIX_EPOCH)),
        }
    }
    pub fn modified(&self) -> io::Result<SystemTime> {
        match self {
            Self::Host(info) => info.modified(),
            Self::ReadOnly(_) => Ok(SystemTime::UNIX_EPOCH),
            Self::Optical(_, time) => Ok(time.unwrap_or(SystemTime::UNIX_EPOCH)),
        }
    }
}
impl From<std::fs::File> for FileStream {
    fn from(file: std::fs::File) -> Self {
        let identity = file_identity(&file).ok();
        Self::Host(HostFile { file, identity })
    }
}
impl FileStream {
    pub fn read_only(bytes: Arc<[u8]>) -> Self {
        Self::ReadOnly(Cursor::new(bytes))
    }
    pub fn is_null_device(&self) -> bool {
        matches!(self, Self::Null)
    }
    pub fn host(&self) -> Option<&std::fs::File> {
        match self {
            Self::Host(file) => Some(&file.file),
            Self::ReadOnly(_) | Self::Optical(_) | Self::Null => None,
        }
    }
    /// No syscall. Unknown/non-file identities simply have no shared content.
    pub fn cached_identity(&self) -> Option<FileIdentity> {
        match self {
            Self::Host(file) => file.identity,
            Self::ReadOnly(cursor) => Some((u64::MAX, cursor.get_ref().as_ptr() as usize as u64)),
            Self::Optical(file) => Some(file.identity()),
            Self::Null => None,
        }
    }
    pub fn memory_bytes(&self) -> Option<&Arc<[u8]>> {
        match self {
            Self::Host(_) | Self::Optical(_) | Self::Null => None,
            Self::ReadOnly(cursor) => Some(cursor.get_ref()),
        }
    }
    pub fn optical_file(&self) -> Option<&super::OpticalFile> {
        match self {
            Self::Optical(file) => Some(file),
            _ => None,
        }
    }
    pub fn stream_position(&self) -> io::Result<u64> {
        match self {
            Self::Host(file) => {
                let mut file = &file.file;
                file.stream_position()
            }
            Self::ReadOnly(cursor) => Ok(cursor.position()),
            Self::Optical(file) => Ok(file.position()),
            Self::Null => Ok(0),
        }
    }
    pub fn metadata(&self) -> io::Result<Metadata> {
        match self {
            Self::Host(file) => file.file.metadata().map(Metadata::Host),
            Self::ReadOnly(cursor) => Ok(Metadata::ReadOnly(cursor.get_ref().len() as u64)),
            Self::Optical(file) => Ok(Metadata::Optical(file.len(), file.modified())),
            Self::Null => Ok(Metadata::ReadOnly(0)),
        }
    }
    pub fn standard_information(&self) -> io::Result<StandardInformation> {
        if let Self::Host(file) = self {
            return host_standard_information(&file.file);
        }
        // System32 byte images, ISO extents and the null device are immutable VFS file objects.
        // Their model owns exactly their backing bytes and one logical link,
        // with no deletion queue or directory handles. This is not a host
        // allocation/identity claim and does not round EOF into cluster sizes.
        let length = self.metadata()?.len();
        Ok(StandardInformation {
            allocation_size: length,
            end_of_file: length,
            number_of_links: 1,
            delete_pending: false,
            directory: false,
        })
    }
    pub fn set_len(&self, len: u64) -> io::Result<()> {
        match self {
            Self::Host(file) => file.file.set_len(len),
            Self::ReadOnly(_) | Self::Optical(_) => Err(io::ErrorKind::PermissionDenied.into()),
            Self::Null => Err(io::ErrorKind::Unsupported.into()),
        }
    }
}
impl Read for FileStream {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Host(file) => file.file.read(output),
            Self::ReadOnly(cursor) => cursor.read(output),
            Self::Optical(file) => file.read(output),
            Self::Null => Ok(0),
        }
    }
}
impl Write for FileStream {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        match self {
            Self::Host(file) => file.file.write(input),
            Self::ReadOnly(_) | Self::Optical(_) => Err(io::ErrorKind::PermissionDenied.into()),
            Self::Null => Ok(input.len()),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Host(file) => file.file.flush(),
            Self::ReadOnly(_) | Self::Optical(_) | Self::Null => Ok(()),
        }
    }
}
impl Seek for FileStream {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        match self {
            Self::Host(file) => file.file.seek(position),
            Self::ReadOnly(cursor) => cursor.seek(position),
            Self::Optical(file) => file.seek(position),
            // Current and end positions are both zero on the null device.
            Self::Null => match position {
                SeekFrom::Start(offset) => Ok(offset),
                SeekFrom::Current(offset) | SeekFrom::End(offset) => {
                    u64::try_from(offset).map_err(|_| io::ErrorKind::InvalidInput.into())
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::section::identity_queries;
    use super::*;

    #[cfg(windows)]
    fn mark_sparse(file: &std::fs::File) {
        use std::os::windows::io::AsRawHandle;
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn DeviceIoControl(
                file: *mut std::ffi::c_void,
                code: u32,
                input: *const std::ffi::c_void,
                input_size: u32,
                output: *mut std::ffi::c_void,
                output_size: u32,
                returned: *mut u32,
                overlapped: *mut std::ffi::c_void,
            ) -> i32;
        }
        let mut returned = 0;
        // SAFETY: the owned handle and returned DWORD are live; FSCTL_SET_SPARSE
        // requires no input or output data and is performed synchronously.
        assert_ne!(
            unsafe {
                DeviceIoControl(
                    file.as_raw_handle(),
                    0x900c4,
                    std::ptr::null(),
                    0,
                    std::ptr::null_mut(),
                    0,
                    &mut returned,
                    std::ptr::null_mut(),
                )
            },
            0,
            "{}",
            io::Error::last_os_error()
        );
    }

    #[cfg(any(windows, unix))]
    #[test]
    fn standard_information_uses_sparse_allocation_and_the_opened_linked_instance() {
        let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/stream-standard-tests")
            .join(std::process::id().to_string());
        std::fs::create_dir_all(&directory).unwrap();
        let source = directory.join("source.bin");
        let alias = directory.join("alias.bin");
        let renamed = directory.join("renamed.bin");
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&source)
            .unwrap();
        #[cfg(windows)]
        mark_sparse(&file);
        file.set_len(0x1_0000_0003).unwrap();
        let mut stream = FileStream::from(file);
        stream.seek(SeekFrom::Start(7)).unwrap();
        // The adb shell SELinux domain on Android denies links; the product never links.
        if let Err(error) = std::fs::hard_link(&source, &alias) {
            assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
            return;
        }
        std::fs::rename(&source, &renamed).unwrap();
        std::fs::write(&source, b"new instance").unwrap();
        let standard = stream.standard_information().unwrap();
        assert_eq!(standard.end_of_file, 0x1_0000_0003);
        assert!(standard.allocation_size < standard.end_of_file);
        #[cfg(windows)]
        assert_eq!(standard.allocation_size, 0);
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let metadata = stream.host().unwrap().metadata().unwrap();
            assert_eq!(standard.allocation_size, metadata.blocks() * 512);
        }
        assert_eq!(standard.number_of_links, 2);
        assert!(!standard.delete_pending);
        assert!(!standard.directory);
        assert_eq!(stream.stream_position().unwrap(), 7);
        std::fs::remove_file(&alias).unwrap();
        assert_eq!(stream.standard_information().unwrap().number_of_links, 1);

        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.custom_flags(0x02000000); // FILE_FLAG_BACKUP_SEMANTICS
        }
        let opened_directory = FileStream::from(options.open(&directory).unwrap());
        assert!(opened_directory.standard_information().unwrap().directory);
        drop(opened_directory);
        drop(stream);
        std::fs::remove_file(&renamed).unwrap();
        std::fs::remove_file(&source).unwrap();
        std::fs::remove_dir(&directory).unwrap();
    }

    #[test]
    fn host_identity_is_cached_across_io_and_refreshed_on_retarget() {
        let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/shared-file-section-tests");
        std::fs::create_dir_all(&directory).unwrap();
        let source = directory.join(format!("cached-source-{}.bin", std::process::id()));
        let target = directory.join(format!("cached-target-{}.bin", std::process::id()));
        std::fs::write(&source, [0x11; 32]).unwrap();
        std::fs::write(&target, [0x22; 32]).unwrap();
        let open = |path: &std::path::Path| {
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(path)
                .unwrap()
        };
        let before = identity_queries();
        let mut stream = FileStream::from(open(&source));
        let old_id = stream.cached_identity().unwrap();
        assert_eq!(identity_queries(), before + 1);
        let mut bytes = [0; 1];
        for _ in 0..128 {
            assert_eq!(stream.cached_identity(), Some(old_id));
            stream.seek(SeekFrom::Start(7)).unwrap();
            stream.read_exact(&mut bytes).unwrap();
            assert_eq!(bytes, [0x11]);
            stream.write_all(&[0x33]).unwrap();
        }
        // Each iteration exercises the ordinary read/write path and the
        // identity used for a weak section lookup, without another OS query.
        assert_eq!(identity_queries(), before + 1);
        stream.seek(SeekFrom::Start(17)).unwrap();
        let mut replacement = open(&target);
        replacement
            .seek(SeekFrom::Start(stream.stream_position().unwrap()))
            .unwrap();
        let replacement = FileStream::from(replacement);
        let new_id = replacement.cached_identity().unwrap();
        assert_ne!(new_id, old_id);
        assert_eq!(identity_queries(), before + 2);
        stream = replacement; // The prepared overlay owner transfer performs no I/O.
        assert_eq!(stream.cached_identity(), Some(new_id));
        assert_eq!(stream.stream_position().unwrap(), 17);
        stream.read_exact(&mut bytes).unwrap();
        assert_eq!(bytes, [0x22]);
        assert_eq!(identity_queries(), before + 2);
        drop(stream);
        assert_eq!(std::fs::read(&source).unwrap()[8], 0x33);
        assert_eq!(std::fs::read(&target).unwrap()[8], 0x22);
        std::fs::remove_file(source).unwrap();
        std::fs::remove_file(target).unwrap();
    }

    #[test]
    fn virtual_streams_share_content_identity_but_keep_independent_cursors() {
        let data: Arc<[u8]> = Arc::from([1u8, 2, 3, 4]);
        let before = identity_queries();
        let mut first = FileStream::read_only(data.clone());
        let second = FileStream::read_only(data.clone());
        let other = FileStream::read_only(Arc::from([1u8, 2, 3, 4]));
        assert_eq!(first.cached_identity(), second.cached_identity());
        assert_ne!(first.cached_identity(), other.cached_identity());
        first.seek(SeekFrom::Start(3)).unwrap();
        assert_eq!(first.stream_position().unwrap(), 3);
        assert_eq!(second.stream_position().unwrap(), 0);
        assert_eq!(identity_queries(), before);
    }
}
