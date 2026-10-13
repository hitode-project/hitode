//! Userland ISO9660/Joliet optical media. The image is one read-only host file;
//! guest streams retain only bounded extent metadata and independent cursors.
//! hadris-iso is the only ISO parser, built with just the read features.
//! CDFS volume serials follow Wine's mountmgr
//! (https://github.com/wine-mirror/wine/blob/master/dlls/mountmgr.sys/device.c),
//! and volume flags and name limits follow the MS-FSA CDFS definitions instead
//! of the ISO authoring limits.
use super::{DirEntry, FileIdentity, FileStream, Vfs};
use hadris_iso::file::EntryType;
use hadris_iso::sync::directory::{DirectoryRecord, DirectoryRef};
use hadris_iso::sync::read::IsoImage;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

const SECTOR: u64 = 2048;
const MAX_DESCRIPTORS: u64 = 64;
const MAX_METADATA_BYTES: u64 = 16 * 1024 * 1024;
const MAX_ENTRIES: usize = 65_536;
const MAX_EXTENTS: usize = 4096;

/// Shared volume facts used by all guest personalities, without native mounts.
#[derive(Clone, Debug)]
pub struct OpticalDriveInfo {
    pub label: String,
    pub serial: u32,
    pub bytes_per_sector: u32,
    pub total_bytes: u64,
    pub filesystem: &'static str,
    pub filesystem_flags: u32,
    pub max_component_length: u32,
}

pub(super) struct OpticalMedia {
    image: IsoImage<File>,
    root: DirectoryRef,
    joliet: bool,
    pub(super) info: OpticalDriveInfo,
}

#[derive(Clone)]
struct Extent {
    image_offset: u64,
    length: u64,
}

struct Entry {
    metadata: DirEntry,
    record: DirectoryRecord,
    extents: Vec<Extent>,
    identity: u64,
}

/// An immutable optical-file owner. Clones have independent logical cursors and
/// keep the opened image alive across remounts and parent-process termination.
#[derive(Clone)]
pub struct OpticalFile {
    media: Arc<OpticalMedia>,
    extents: Arc<[Extent]>,
    length: u64,
    position: u64,
    modified: Option<SystemTime>,
    identity: u64,
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn iso_error(error: hadris_iso::sync::Error) -> io::Error {
    // Retain meaningful host-independent errors; parser diagnostics stay data.
    let kind = match error.kind() {
        hadris_iso::sync::ErrorKind::Unsupported => io::ErrorKind::Unsupported,
        hadris_iso::sync::ErrorKind::UnexpectedEof => io::ErrorKind::UnexpectedEof,
        hadris_iso::sync::ErrorKind::InvalidInput => io::ErrorKind::InvalidInput,
        _ => io::ErrorKind::InvalidData,
    };
    io::Error::new(kind, error.to_string())
}

fn both_u32(bytes: &[u8]) -> io::Result<u32> {
    let little = u32::from_le_bytes(bytes[..4].try_into().unwrap());
    let big = u32::from_be_bytes(bytes[4..8].try_into().unwrap());
    if little != big {
        return Err(invalid("inconsistent ISO endian fields"));
    }
    Ok(little)
}

fn range(offset: u64, length: u64, image_len: u64) -> io::Result<()> {
    if offset.checked_add(length).is_none_or(|end| end > image_len) {
        return Err(invalid("ISO extent exceeds volume bounds"));
    }
    Ok(())
}

/// Bound metadata before the library caches the path table. This prevents an
/// untrusted size from allocating image-sized metadata, and caps descriptor scans.
fn inspect(file: &mut File) -> io::Result<OpticalDriveInfo> {
    let image_len = file.metadata()?.len();
    let mut primary: Option<[u8; SECTOR as usize]> = None;
    let mut label = None;
    let mut label_level = 0;
    let mut serial_descriptor: Option<[u8; SECTOR as usize]> = None;
    for sector in 16..16 + MAX_DESCRIPTORS {
        let mut descriptor = [0; SECTOR as usize];
        file.seek(SeekFrom::Start(sector * SECTOR))?;
        file.read_exact(&mut descriptor)?;
        if &descriptor[1..6] != b"CD001" || descriptor[6] != 1 {
            return Err(invalid("invalid ISO9660 volume descriptor"));
        }
        if descriptor[0] == 255 {
            let descriptor = primary.ok_or_else(|| invalid("ISO has no primary descriptor"))?;
            let total_bytes = u64::from(both_u32(&descriptor[80..88])?) * SECTOR;
            if total_bytes > image_len || total_bytes < 19 * SECTOR {
                return Err(invalid("ISO volume capacity exceeds image bounds"));
            }
            // NT CDFS descriptor checksum, also used by Wine's
            // dlls/mountmgr.sys/device.c VOLUME_GetSuperblockSerial. Prefer the
            // selected Joliet descriptor, as Windows does for Unicode volumes.
            let selected = serial_descriptor.as_ref().unwrap_or(&descriptor);
            let mut columns = [0u8; 4];
            for (index, byte) in selected.iter().enumerate() {
                columns[index % 4] = columns[index % 4].wrapping_add(*byte);
            }
            let serial = u32::from_be_bytes(columns);
            return Ok(OpticalDriveInfo {
                label: label.unwrap_or_else(|| {
                    String::from_utf8_lossy(&descriptor[40..72])
                        .trim_end_matches([' ', '\0'])
                        .to_owned()
                }),
                serial,
                bytes_per_sector: SECTOR as u32,
                total_bytes,
                filesystem: "CDFS",
                // MS-FSA product behavior: CDFS reports case-sensitive search,
                // read-only media, and Unicode on disk only for Joliet. The
                // Windows reader accepts extended identifiers beyond ISO's
                // nominal 31/64-character authoring limits.
                filesystem_flags: 0x80001 | if label_level != 0 { 4 } else { 0 },
                max_component_length: if label_level != 0 { 110 } else { 221 },
            });
        }
        if descriptor[0] == 1 || descriptor[0] == 2 {
            if descriptor[128..132] != [0, 8, 8, 0] {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "ISO logical block size must be 2048",
                ));
            }
            let path_len = u64::from(both_u32(&descriptor[132..140])?);
            if path_len > MAX_METADATA_BYTES {
                return Err(invalid("ISO path table exceeds metadata limit"));
            }
            let path_lba = u32::from_le_bytes(descriptor[140..144].try_into().unwrap());
            range(u64::from(path_lba) * SECTOR, path_len, image_len)?;
            let root_offset = u64::from(both_u32(&descriptor[158..166])?) * SECTOR;
            let root_length = u64::from(both_u32(&descriptor[166..174])?);
            if root_length > MAX_METADATA_BYTES {
                return Err(invalid("ISO root directory exceeds metadata limit"));
            }
            range(root_offset, root_length, image_len)?;
            if descriptor[0] == 1 {
                primary = Some(descriptor);
            } else {
                let level = match &descriptor[88..91] {
                    b"%/@" => 1,
                    b"%/C" => 2,
                    b"%/E" => 3,
                    _ => 0,
                };
                if level != 0 && level >= label_level {
                    let units = descriptor[40..72]
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|b| u16::from_be_bytes([b[0], b[1]]));
                    let text: Result<String, _> = char::decode_utf16(units).collect();
                    label = Some(
                        text.map_err(|_| invalid("invalid Joliet volume label"))?
                            .trim_end_matches([' ', '\0'])
                            .to_owned(),
                    );
                    label_level = level;
                    serial_descriptor = Some(descriptor);
                }
            }
        }
    }
    Err(invalid("ISO descriptor count exceeds metadata limit"))
}

impl OpticalMedia {
    fn open(path: &Path) -> io::Result<Arc<Self>> {
        let mut file = File::open(path)?;
        let info = inspect(&mut file)?;
        let image = IsoImage::open(file).map_err(iso_error)?;
        // Windows sees Joliet Unicode names ahead of the primary short names.
        let selected = image
            .root_dirs()
            .iter()
            .filter_map(|root| match root.entry_type() {
                EntryType::Joliet { level, .. } => Some((level, *root)),
                _ => None,
            })
            .max_by_key(|(level, _)| *level)
            .map(|(_, root)| root);
        let (root, joliet) = match selected {
            Some(root) => (root.dir_ref(), true),
            None => (
                image
                    .root_dirs()
                    .iter()
                    .next()
                    .ok_or_else(|| invalid("ISO root missing"))?
                    .dir_ref(),
                false,
            ),
        };
        Ok(Arc::new(Self {
            image,
            root,
            joliet,
            info,
        }))
    }

    fn check_directory(&self, directory: DirectoryRef) -> io::Result<()> {
        if directory.size as u64 > MAX_METADATA_BYTES {
            return Err(invalid("ISO directory exceeds metadata limit"));
        }
        range(
            directory.extent.0 as u64 * SECTOR,
            directory.size as u64,
            self.info.total_bytes,
        )
    }

    fn name(&self, record: &DirectoryRecord) -> io::Result<String> {
        let mut name = if self.joliet {
            if !record.name().len().is_multiple_of(2) {
                return Err(invalid("invalid Joliet file identifier"));
            }
            let units = record
                .name()
                .as_chunks::<2>()
                .0
                .iter()
                .map(|b| u16::from_be_bytes([b[0], b[1]]));
            char::decode_utf16(units)
                .collect::<Result<String, _>>()
                .map_err(|_| invalid("invalid Joliet file identifier"))?
        } else {
            String::from_utf8(record.name().to_vec())
                .map_err(|_| invalid("invalid ISO file identifier"))?
        };
        if let Some((base, version)) = name.rsplit_once(';')
            && !version.is_empty()
            && version.bytes().all(|b| b.is_ascii_digit())
        {
            name = base.to_owned();
        }
        name = name.trim_end_matches('.').to_owned();
        if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\', '\0']) {
            return Err(invalid("invalid ISO path component"));
        }
        Ok(name)
    }

    fn extent(&self, record: &DirectoryRecord) -> io::Result<Extent> {
        let header = record.header();
        if header.file_unit_size != 0 || header.interleave_gap_size != 0 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "interleaved ISO files are unsupported",
            ));
        }
        let image_offset =
            (u64::from(header.extent.read()) + u64::from(header.extended_attr_record)) * SECTOR;
        let length = u64::from(header.data_len.read());
        range(image_offset, length, self.info.total_bytes)?;
        Ok(Extent {
            image_offset,
            length,
        })
    }

    fn scan(
        &self,
        directory: DirectoryRef,
        mut visit: impl FnMut(Entry) -> bool,
    ) -> io::Result<()> {
        self.check_directory(directory)?;
        let dir = self.image.open_dir(directory);
        let mut records = dir.raw_entries();
        let mut count = 0;
        loop {
            let record_offset = records.offset();
            let Some(record) = records.next() else {
                break;
            };
            let record = record.map_err(iso_error)?;
            let record_end = records.offset();
            let record_start = record_end - record.size();
            if record_end > directory.size
                || record_start % SECTOR as usize + record.size() > SECTOR as usize
            {
                return Err(invalid(
                    "ISO directory record exceeds directory or sector bounds",
                ));
            }
            count += 1;
            if count > MAX_ENTRIES {
                return Err(invalid("ISO directory entry limit exceeded"));
            }
            if record.is_special() || record.header().flags & 4 != 0 {
                continue;
            }
            let name = self.name(&record)?;
            let mut extents = vec![self.extent(&record)?];
            let mut flags = record.header().flags;
            while flags & 0x80 != 0 {
                if record.is_directory() || extents.len() >= MAX_EXTENTS {
                    return Err(invalid("ISO extent chain limit exceeded"));
                }
                let continuation = records
                    .next()
                    .ok_or_else(|| invalid("ISO extent chain is truncated"))?
                    .map_err(iso_error)?;
                let continuation_end = records.offset();
                let continuation_start = continuation_end - continuation.size();
                if continuation_end > directory.size
                    || continuation_start % SECTOR as usize + continuation.size() > SECTOR as usize
                {
                    return Err(invalid(
                        "ISO continuation exceeds directory or sector bounds",
                    ));
                }
                if continuation.name() != record.name()
                    || (continuation.header().flags ^ record.header().flags) & !0x80 != 0
                {
                    return Err(invalid("ISO extent continuation mismatch"));
                }
                flags = continuation.header().flags;
                extents.push(self.extent(&continuation)?);
            }
            let size = extents.iter().try_fold(0u64, |sum, extent| {
                sum.checked_add(extent.length)
                    .ok_or_else(|| invalid("ISO file size overflow"))
            })?;
            let timestamp = record_time(&record);
            let is_dir = record.is_directory();
            let metadata = DirEntry {
                name,
                is_dir,
                size: if is_dir { 0 } else { size },
                created: timestamp,
                accessed: timestamp,
                modified: timestamp,
                attributes: 1
                    | if is_dir { 0x10 } else { 0x20 }
                    | if record.header().flags & 1 != 0 { 2 } else { 0 },
            };
            let identity = directory.extent.0 as u64 * SECTOR + record_offset as u64;
            if !visit(Entry {
                metadata,
                record,
                extents,
                identity,
            }) {
                break;
            }
        }
        Ok(())
    }

    fn directory_ref(&self, entry: &Entry) -> io::Result<DirectoryRef> {
        let mut reference = entry
            .record
            .as_dir_ref()
            .map_err(|_| io::ErrorKind::NotADirectory)?;
        reference.extent.0 += entry.record.header().extended_attr_record as usize;
        self.check_directory(reference)?;
        Ok(reference)
    }

    fn find(&self, path: &str) -> io::Result<Option<Entry>> {
        let mut directory = self.root;
        let mut components = path.split('\\').filter(|part| !part.is_empty()).peekable();
        while let Some(component) = components.next() {
            let mut found = None;
            self.scan(directory, |entry| {
                if entry.metadata.name.eq_ignore_ascii_case(component) {
                    found = Some(entry);
                    false
                } else {
                    true
                }
            })?;
            let entry = found.ok_or(io::ErrorKind::NotFound)?;
            if components.peek().is_none() {
                return Ok(Some(entry));
            }
            directory = self.directory_ref(&entry)?;
        }
        Ok(None)
    }

    fn list(&self, path: &str) -> io::Result<Vec<DirEntry>> {
        let directory = match self.find(path)? {
            Some(entry) => self.directory_ref(&entry)?,
            None => self.root,
        };
        let mut entries = Vec::new();
        self.scan(directory, |entry| {
            entries.push(entry.metadata);
            true
        })?;
        entries.sort_by_cached_key(|entry| entry.name.to_lowercase());
        Ok(entries)
    }
}

fn record_time(record: &DirectoryRecord) -> Option<SystemTime> {
    let date = &record.to_bytes()[18..25];
    let (year, month, day) = (
        i64::from(date[0]) + 1900,
        i64::from(date[1]),
        i64::from(date[2]),
    );
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || date[3] > 23
        || date[4] > 59
        || date[5] > 59
    {
        return None;
    }
    let year = year - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let doy = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let days = era * 146097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719468;
    let seconds =
        days * 86400 + i64::from(date[3]) * 3600 + i64::from(date[4]) * 60 + i64::from(date[5])
            - i64::from(date[6] as i8) * 900;
    if seconds < 0 {
        SystemTime::UNIX_EPOCH.checked_sub(Duration::from_secs(seconds.unsigned_abs()))
    } else {
        SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(seconds as u64))
    }
}

impl OpticalFile {
    pub fn len(&self) -> u64 {
        self.length
    }
    pub fn is_empty(&self) -> bool {
        self.length == 0
    }
    pub fn modified(&self) -> Option<SystemTime> {
        self.modified
    }
    pub fn position(&self) -> u64 {
        self.position
    }
    pub fn identity(&self) -> FileIdentity {
        (
            u64::MAX - 1 - Arc::as_ptr(&self.media) as usize as u64,
            self.identity,
        )
    }
    /// Read a bounded file range without changing any guest cursor.
    pub fn read_at(&self, offset: u64, output: &mut [u8]) -> io::Result<usize> {
        let wanted = (output.len() as u64).min(self.length.saturating_sub(offset)) as usize;
        let mut written = 0;
        let mut logical = 0;
        for extent in self.extents.iter() {
            if written == wanted {
                break;
            }
            if offset + written as u64 >= logical + extent.length {
                logical += extent.length;
                continue;
            }
            let within = (offset + written as u64).saturating_sub(logical);
            let count = (extent.length - within).min((wanted - written) as u64) as usize;
            self.media
                .image
                .read_bytes_at(
                    extent.image_offset + within,
                    &mut output[written..written + count],
                )
                .map_err(iso_error)?;
            written += count;
            logical += extent.length;
        }
        Ok(written)
    }
}
impl Read for OpticalFile {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let count = self.read_at(self.position, output)?;
        self.position += count as u64;
        Ok(count)
    }
}
impl Seek for OpticalFile {
    fn seek(&mut self, seek: SeekFrom) -> io::Result<u64> {
        let position = match seek {
            SeekFrom::Start(position) => position,
            SeekFrom::Current(offset) => self
                .position
                .checked_add_signed(offset)
                .ok_or(io::ErrorKind::InvalidInput)?,
            SeekFrom::End(offset) => self
                .length
                .checked_add_signed(offset)
                .ok_or(io::ErrorKind::InvalidInput)?,
        };
        self.position = position;
        Ok(position)
    }
}

impl Vfs {
    /// Publish an optical drive only after opening and validating the ISO.
    /// C: and the configured game-root drive cannot be replaced by optical media.
    pub fn mount_iso(&mut self, letter: char, path: impl AsRef<Path>) -> io::Result<()> {
        let letter = letter.to_ascii_uppercase();
        if !letter.is_ascii_alphabetic()
            || letter == 'C'
            || self.game_root.as_bytes().get(1) == Some(&b':')
                && self.game_root.as_bytes()[0].to_ascii_uppercase() == letter as u8
        {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let media = OpticalMedia::open(path.as_ref())?;
        self.optical.insert(letter, media);
        Ok(())
    }
    /// Child processes share immutable media owners while keeping their VFS state.
    pub fn inherit_optical(&mut self, source: &Vfs) {
        self.optical.clone_from(&source.optical);
    }
    pub fn optical_drive(&self, letter: char) -> Option<&OpticalDriveInfo> {
        self.optical
            .get(&letter.to_ascii_uppercase())
            .map(|media| &media.info)
    }
    pub fn optical_drives(&self) -> impl Iterator<Item = (char, &OpticalDriveInfo)> {
        self.optical
            .iter()
            .map(|(letter, media)| (*letter, &media.info))
    }
    fn optical_path(&self, guest: &str) -> Option<(Arc<OpticalMedia>, String)> {
        let path = self.normalize(guest);
        let path = path.strip_prefix("\\\\?\\").unwrap_or(&path);
        let bytes = path.as_bytes();
        if bytes.get(1) != Some(&b':') || !bytes[0].is_ascii_alphabetic() {
            return None;
        }
        let media = self.optical.get(&(bytes[0].to_ascii_uppercase() as char))?;
        Some((media.clone(), path[2..].to_owned()))
    }
    /// Includes missing files on a mounted drive, so callers reject all writes.
    pub fn is_optical_path(&self, guest: &str) -> bool {
        self.optical_path(guest).is_some()
    }
    pub fn optical_entry(&self, guest: &str) -> io::Result<DirEntry> {
        let (media, path) = self.optical_path(guest).ok_or(io::ErrorKind::NotFound)?;
        Ok(match media.find(&path)? {
            Some(entry) => entry.metadata,
            None => DirEntry {
                name: String::new(),
                is_dir: true,
                size: 0,
                created: None,
                accessed: None,
                modified: None,
                attributes: 0x11,
            },
        })
    }
    pub(super) fn optical_list(&self, guest: &str) -> io::Result<Vec<DirEntry>> {
        let (media, path) = self.optical_path(guest).ok_or(io::ErrorKind::NotFound)?;
        media.list(&path)
    }
    pub fn open_optical_file(&self, guest: &str) -> io::Result<FileStream> {
        let (media, path) = self.optical_path(guest).ok_or(io::ErrorKind::NotFound)?;
        let entry = media.find(&path)?.ok_or(io::ErrorKind::IsADirectory)?;
        if entry.metadata.is_dir {
            return Err(io::ErrorKind::IsADirectory.into());
        }
        Ok(FileStream::Optical(OpticalFile {
            media,
            extents: Arc::from(entry.extents),
            length: entry.metadata.size,
            position: 0,
            modified: entry.metadata.modified,
            identity: entry.identity,
        }))
    }
}

#[cfg(test)]
mod tests;
