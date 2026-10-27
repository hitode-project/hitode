//! Instance-based host mutation adapters. Unsupported platforms fail explicitly.
use super::FileStream;
use std::{io, path::Path};

/// Open the existing backing instance with DELETE, never copy it or create it.
pub(super) fn open(path: &Path, access: u32) -> io::Result<FileStream> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);

    let _ = access;
    options.open(path).map(FileStream::from)
}

impl FileStream {
    pub fn open_for_mutation(path: &Path, access: u32) -> io::Result<Self> {
        open(path, access)
    }

    /// Keep enumeration and mutation aliases on the same real directory FD.
    pub fn open_directory_instance(path: &Path, access: u32) -> io::Result<Self> {
        let _ = access;
        std::fs::File::open(path).map(Self::from)
    }

    /// Native delete-pending belongs to the opened FD, including its aliases.
    pub fn set_delete_disposition(&self, delete: bool) -> io::Result<()> {
        let file = self.host().ok_or(io::ErrorKind::PermissionDenied)?;

        {
            let _ = (file, delete);
            // POSIX unlink cannot stand in for NT's cancellable pending delete.
            Err(io::ErrorKind::Unsupported.into())
        }
    }

    /// FILE_DISPOSITION_INFO_EX uses one DWORD Flags, not classic BOOL.
    pub fn set_delete_disposition_ex(&self, flags: u32) -> io::Result<()> {
        let file = self.host().ok_or(io::ErrorKind::PermissionDenied)?;

        {
            let _ = (file, flags);
            Err(io::ErrorKind::Unsupported.into())
        }
    }

    /// Rename this instance using host FILE_RENAME_INFO, never a source lookup.
    pub fn rename_instance(&self, target: &Path, replace: bool) -> io::Result<()> {
        let file = self.host().ok_or(io::ErrorKind::PermissionDenied)?;

        {
            let _ = (file, target, replace);
            // A path rename cannot preserve this FD's identity/no-replace
            // contract against concurrent replacement on every POSIX target.
            Err(io::ErrorKind::Unsupported.into())
        }
    }
}

/// Persist renamed directory entries before asking the device to flush its cache.
pub fn sync_move(source: &Path, target: &Path) -> io::Result<()> {
    let from = source.parent().ok_or(io::ErrorKind::InvalidInput)?;
    let to = target.parent().ok_or(io::ErrorKind::InvalidInput)?;
    std::fs::File::open(from)?.sync_all()?;
    if from != to {
        std::fs::File::open(to)?.sync_all()?;
    }
    super::sync_file(&std::fs::File::open(target)?)
}
