//! File-handle storage shared by ordinary files and read-only virtual images.
use std::io::{self, Cursor, Read, Seek, SeekFrom, Write};
use std::sync::Arc;
use std::time::SystemTime;

pub enum FileStream {
    Host(std::fs::File),
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
        Self::Host(file)
    }
}
impl FileStream {
    pub fn read_only(bytes: Arc<[u8]>) -> Self {
        Self::ReadOnly(Cursor::new(bytes))
    }
    pub fn host(&self) -> Option<&std::fs::File> {
        match self {
            Self::Host(file) => Some(file),
            Self::ReadOnly(_) => None,
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
                let mut file = file;
                file.stream_position()
            }
            Self::ReadOnly(cursor) => Ok(cursor.position()),
        }
    }
    pub fn metadata(&self) -> io::Result<Metadata> {
        match self {
            Self::Host(file) => file.metadata().map(Metadata::Host),
            Self::ReadOnly(cursor) => Ok(Metadata::ReadOnly(cursor.get_ref().len() as u64)),
        }
    }
    pub fn set_len(&self, len: u64) -> io::Result<()> {
        match self {
            Self::Host(file) => file.set_len(len),
            Self::ReadOnly(_) => Err(io::ErrorKind::PermissionDenied.into()),
        }
    }
}
impl Read for FileStream {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Host(file) => file.read(output),
            Self::ReadOnly(cursor) => cursor.read(output),
        }
    }
}
impl Write for FileStream {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        match self {
            Self::Host(file) => file.write(input),
            Self::ReadOnly(_) => Err(io::ErrorKind::PermissionDenied.into()),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Host(file) => file.flush(),
            Self::ReadOnly(_) => Ok(()),
        }
    }
}
impl Seek for FileStream {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        match self {
            Self::Host(file) => file.seek(position),
            Self::ReadOnly(cursor) => cursor.seek(position),
        }
    }
}
