//! Bounded, process-owned guest metadata over existing VFS backing instances.
//! No host attribute writes, copy-up, guest handles, or persistent sidecar files.
use super::*;
use std::{cell::RefCell, fs::Metadata, io};

const DIRECTORY: u32 = 0x10;
const ARCHIVE: u32 = 0x20;
const NORMAL: u32 = 0x80;
const MAX_NORMAL_INSTANCES: usize = 4096;

#[derive(PartialEq, Eq)]
struct Stamp {
    created: Option<SystemTime>,
    modified: Option<SystemTime>,
    size: u64,
}
impl Stamp {
    fn new(metadata: &Metadata) -> Self {
        Self {
            created: metadata.created().ok(),
            modified: metadata.modified().ok(),
            size: metadata.len(),
        }
    }
}
#[derive(Default)]
pub(super) struct Attributes {
    normal: RefCell<BTreeMap<FileIdentity, Stamp>>,
}
fn identity(path: &Path, metadata: &Metadata) -> io::Result<FileIdentity> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        let _ = metadata;
        // Zero data access requests only a metadata identity. Share all access
        // so an existing guest FileObj remains the sole I/O/share owner.
        let file = std::fs::OpenOptions::new()
            .access_mode(0)
            .share_mode(7)
            .open(path)?;
        file_identity(&file)
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let _ = path;
        Ok((metadata.dev(), metadata.ino()))
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = (path, metadata);
        Err(io::ErrorKind::Unsupported.into())
    }
}
impl Attributes {
    pub(super) fn read(&self, path: &Path, metadata: &Metadata) -> io::Result<u32> {
        if metadata.is_dir() {
            return Ok(DIRECTORY);
        }
        if self.normal.borrow().is_empty() {
            return Ok(ARCHIVE);
        }
        self.read_instance(identity(path, metadata)?, metadata)
    }
    fn read_instance(&self, identity: FileIdentity, metadata: &Metadata) -> io::Result<u32> {
        if metadata.is_dir() {
            return Ok(DIRECTORY);
        }
        let mut normal = self.normal.borrow_mut();
        match normal.get(&identity) {
            Some(stamp) if stamp == &Stamp::new(metadata) => Ok(NORMAL),
            Some(_) => {
                // A recreated instance or a subsequent backing-file write has
                // fresh default/archive metadata. Names alone cannot revive it.
                normal.remove(&identity);
                Ok(ARCHIVE)
            }
            None => Ok(ARCHIVE),
        }
    }
    fn set_normal(&self, path: &Path, metadata: &Metadata) -> io::Result<()> {
        // DIRECTORY is structural, never converted into a regular file. The
        // currently supported directory metadata has no other mutable bits.
        if metadata.is_dir() {
            return Ok(());
        }
        if !metadata.is_file() {
            return Err(io::ErrorKind::Unsupported.into());
        }
        let identity = identity(path, metadata)?;
        let mut normal = self.normal.borrow_mut();
        if !normal.contains_key(&identity) && normal.len() >= MAX_NORMAL_INSTANCES {
            return Err(io::ErrorKind::OutOfMemory.into());
        }
        normal.insert(identity, Stamp::new(metadata));
        Ok(())
    }
}
impl Vfs {
    /// Query the same guest attribute view for the opened backing instance,
    /// even after its path was renamed, removed, or replaced.
    pub fn file_handle_attributes(&self, file: &std::fs::File) -> io::Result<u32> {
        let metadata = file.metadata()?;
        if metadata.is_dir() {
            return Ok(DIRECTORY);
        }
        if self.attributes.normal.borrow().is_empty() {
            return Ok(ARCHIVE);
        }
        self.attributes
            .read_instance(file_identity(file)?, &metadata)
    }
    /// Baseline guest file metadata is ARCHIVE/DIRECTORY. An explicitly cleared
    /// regular-file instance reads NORMAL through both lookup and enumeration.
    pub fn file_attributes(&self, guest: &str) -> io::Result<u32> {
        if self.is_optical_path(guest) {
            return Ok(self.optical_entry(guest)?.attributes);
        }
        let path = self.lookup(guest).ok_or(io::ErrorKind::NotFound)?;
        self.attributes.read(&path, &std::fs::metadata(&path)?)
    }
    /// Publish NORMAL only after verifying a live ordinary VFS instance. Host
    /// permissions/attributes and every read-only source layer remain untouched.
    pub fn set_file_normal(&self, guest: &str) -> io::Result<()> {
        if self.is_optical_path(guest) {
            self.optical_entry(guest)?;
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let path = self.lookup(guest).ok_or(io::ErrorKind::NotFound)?;
        self.attributes
            .set_normal(&path, &std::fs::metadata(&path)?)
    }
}
