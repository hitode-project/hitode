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
}

pub enum Metadata {
    Host(std::fs::Metadata),
    ReadOnly(u64),
}
impl Metadata {
    pub fn len(&self) -> u64 {
        match self {
            Self::Host(info) => info.len(),
            Self::ReadOnly(len) => *len,
        }
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn created(&self) -> io::Result<SystemTime> {
        match self {
            Self::Host(info) => info.created(),
            Self::ReadOnly(_) => Ok(SystemTime::UNIX_EPOCH),
        }
    }
    pub fn accessed(&self) -> io::Result<SystemTime> {
        match self {
            Self::Host(info) => info.accessed(),
            Self::ReadOnly(_) => Ok(SystemTime::UNIX_EPOCH),
        }
    }
    pub fn modified(&self) -> io::Result<SystemTime> {
        match self {
            Self::Host(info) => info.modified(),
            Self::ReadOnly(_) => Ok(SystemTime::UNIX_EPOCH),
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
    pub fn host(&self) -> Option<&std::fs::File> {
        match self {
            Self::Host(file) => Some(&file.file),
            Self::ReadOnly(_) => None,
        }
    }
    /// No syscall. Unknown/non-file identities simply have no shared content.
    pub fn cached_identity(&self) -> Option<FileIdentity> {
        match self {
            Self::Host(file) => file.identity,
            Self::ReadOnly(cursor) => Some((u64::MAX, cursor.get_ref().as_ptr() as usize as u64)),
        }
    }
    pub fn memory_bytes(&self) -> Option<&Arc<[u8]>> {
        match self {
            Self::Host(_) => None,
            Self::ReadOnly(cursor) => Some(cursor.get_ref()),
        }
    }
    pub fn stream_position(&self) -> io::Result<u64> {
        match self {
            Self::Host(file) => {
                let mut file = &file.file;
                file.stream_position()
            }
            Self::ReadOnly(cursor) => Ok(cursor.position()),
        }
    }
    pub fn metadata(&self) -> io::Result<Metadata> {
        match self {
            Self::Host(file) => file.file.metadata().map(Metadata::Host),
            Self::ReadOnly(cursor) => Ok(Metadata::ReadOnly(cursor.get_ref().len() as u64)),
        }
    }
    pub fn set_len(&self, len: u64) -> io::Result<()> {
        match self {
            Self::Host(file) => file.file.set_len(len),
            Self::ReadOnly(_) => Err(io::ErrorKind::PermissionDenied.into()),
        }
    }
}
impl Read for FileStream {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Host(file) => file.file.read(output),
            Self::ReadOnly(cursor) => cursor.read(output),
        }
    }
}
impl Write for FileStream {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        match self {
            Self::Host(file) => file.file.write(input),
            Self::ReadOnly(_) => Err(io::ErrorKind::PermissionDenied.into()),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Host(file) => file.file.flush(),
            Self::ReadOnly(_) => Ok(()),
        }
    }
}
impl Seek for FileStream {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        match self {
            Self::Host(file) => file.file.seek(position),
            Self::ReadOnly(cursor) => cursor.seek(position),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::section::identity_queries;
    use super::*;

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
