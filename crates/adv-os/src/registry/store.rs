//! Bounded binary snapshot, fsync and atomic replacement. Corrupt original
//! files are left intact; the virtual registry returns their load error.
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::{
    INVALID_DATA, Key, KeyMetadata, MAX_KEYS, MAX_METADATA_BYTES, MAX_TOTAL_BYTES, MAX_VALUE_BYTES,
    MAX_VALUES, NO_MEMORY, Value, VirtualRegistry,
};

const MAGIC: &[u8; 8] = b"PALREG02";
const MAX_FILE: usize = MAX_METADATA_BYTES + MAX_TOTAL_BYTES + 20;
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

fn io_error(error: std::io::Error) -> u32 {
    #[cfg(windows)]
    if matches!(error.raw_os_error(), Some(32 | 33)) {
        return error.raw_os_error().unwrap() as u32;
    }
    if error.kind() == std::io::ErrorKind::WouldBlock {
        return 33;
    }
    if error.kind() == std::io::ErrorKind::PermissionDenied {
        5
    } else {
        29
    } // ERROR_WRITE_FAULT
}

fn lock(path: &Path) -> Result<File, u32> {
    let parent = path.parent().ok_or(29u32)?;
    std::fs::create_dir_all(parent).map_err(io_error)?;
    let path = parent.join("registry-v1.lock");
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(0);
    }
    let file = options.open(path).map_err(io_error)?;
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        unsafe extern "C" {
            fn flock(fd: i32, operation: i32) -> i32;
        }
        // SAFETY: fd belongs to this live File; LOCK_EX|LOCK_NB never blocks.
        if unsafe { flock(file.as_raw_fd(), 2 | 4) } != 0 {
            return Err(io_error(std::io::Error::last_os_error()));
        }
    }
    Ok(file)
}

/// Open the registry stored at `path`, locking it against other processes. A file that
/// fails to load is left intact and every access reports the load error.
pub fn load(path: PathBuf) -> VirtualRegistry {
    let mut storage_lock = None;
    let result = (|| -> Result<VirtualRegistry, u32> {
        storage_lock = Some(lock(&path)?);
        let file = match File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(VirtualRegistry::default());
            }
            Err(error) => return Err(io_error(error)),
        };
        let mut bytes = Vec::new();
        file.take(MAX_FILE as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(io_error)?;
        if bytes.len() > MAX_FILE {
            return Err(INVALID_DATA);
        }
        decode(&bytes)
    })();
    let mut registry = match result {
        Ok(registry) => registry,
        Err(error) => {
            log::warn!(
                "virtual registry {}: load failed with {error}; preserving the original file",
                path.display()
            );
            let mut registry = VirtualRegistry::default();
            registry.load_error = Some(error);
            registry
        }
    };
    registry.storage = Some(path);
    registry.storage_lock = storage_lock;
    registry
}

struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl<'a> Cursor<'a> {
    fn take(&mut self, len: usize) -> Result<&'a [u8], u32> {
        let end = self.at.checked_add(len).ok_or(INVALID_DATA)?;
        let bytes = self.bytes.get(self.at..end).ok_or(INVALID_DATA)?;
        self.at = end;
        Ok(bytes)
    }
    fn word(&mut self) -> Result<u32, u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn time(&mut self) -> Result<u64, u32> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn units(&mut self, maximum: usize) -> Result<Vec<u16>, u32> {
        let len = self.word()? as usize;
        if len > maximum {
            return Err(INVALID_DATA);
        }
        let bytes = self.take(len * 2)?;
        let units: Vec<_> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&p| u16::from_le_bytes(p))
            .collect();
        if units.contains(&0) {
            return Err(INVALID_DATA);
        }
        Ok(units)
    }
}
fn decode(bytes: &[u8]) -> Result<VirtualRegistry, u32> {
    if bytes.len() < 20 {
        return Err(INVALID_DATA);
    }
    let (bytes, stored_checksum) = bytes.split_at(bytes.len() - 8);
    if checksum(bytes) != u64::from_le_bytes(stored_checksum.try_into().unwrap()) {
        return Err(INVALID_DATA);
    }
    let mut cursor = Cursor { bytes, at: 0 };
    let marker = cursor.take(8)?;
    let current = if marker == MAGIC {
        true
    } else if marker == b"PALREG01" {
        false
    } else {
        return Err(INVALID_DATA);
    };
    let count = cursor.word()? as usize;
    if count > MAX_KEYS {
        return Err(INVALID_DATA);
    }
    let mut registry = VirtualRegistry::default();
    let mut seen = BTreeSet::new();
    for _ in 0..count {
        let root = cursor.word()?;
        if !registry.keys.contains_key(&Key {
            root,
            path: Vec::new(),
        }) {
            return Err(INVALID_DATA);
        }
        let path = cursor.units(32_767)?;
        if path.first() == Some(&92)
            || path.last() == Some(&92)
            || path.iter().filter(|&&unit| unit == 92).count() >= 512
        {
            return Err(INVALID_DATA);
        }
        if path
            .split(|&unit| unit == 92)
            .any(|part| part.len() > 255 || (!path.is_empty() && part.is_empty()))
        {
            return Err(INVALID_DATA);
        }
        let key = Key { root, path };
        if !seen.insert(key.clone()) || super::fold(&key.path) != key.path {
            return Err(INVALID_DATA);
        }
        if let Some(at) = key.path.iter().rposition(|&unit| unit == 92)
            && !registry.keys.contains_key(&Key {
                root,
                path: key.path[..at].to_vec(),
            })
        {
            return Err(INVALID_DATA);
        }
        let canonical_name = key
            .path
            .rsplit(|&unit| unit == 92)
            .next()
            .unwrap_or_default();
        let metadata = if current {
            let display_name = cursor.units(255)?;
            if super::fold(&display_name) != canonical_name {
                return Err(INVALID_DATA);
            }
            KeyMetadata {
                display_name,
                class: cursor.units(16_383)?,
                modified: cursor.time()?,
            }
        } else {
            KeyMetadata {
                display_name: canonical_name.to_vec(),
                ..KeyMetadata::default()
            }
        };
        let nvalues = cursor.word()? as usize;
        if registry.total_values + nvalues > MAX_VALUES {
            return Err(INVALID_DATA);
        }
        let mut values = BTreeMap::new();
        for _ in 0..nvalues {
            let name = cursor.units(16_383)?;
            if super::fold(&name) != name || values.contains_key(&name) {
                return Err(INVALID_DATA);
            }
            let display_name = if current {
                cursor.units(16_383)?
            } else {
                name.clone()
            };
            if super::fold(&display_name) != name {
                return Err(INVALID_DATA);
            }
            let kind = cursor.word()?;
            let len = cursor.word()? as usize;
            if len > MAX_VALUE_BYTES || registry.total_bytes + len > MAX_TOTAL_BYTES {
                return Err(INVALID_DATA);
            }
            let value = Value {
                kind,
                bytes: cursor.take(len)?.to_vec(),
                display_name,
            };
            registry.total_bytes += len;
            registry.total_values += 1;
            registry.metadata_bytes += 16 + (name.len() + value.display_name.len()) * 2;
            values.insert(name, value);
        }
        if !registry.keys.contains_key(&key) {
            registry.metadata_bytes += 12 + key.path.len() * 2 + metadata.size();
        } else {
            registry.metadata_bytes += metadata.size() - 16;
        }
        if registry.metadata_bytes > MAX_METADATA_BYTES {
            return Err(INVALID_DATA);
        }
        registry.metadata.insert(key.clone(), metadata);
        registry.keys.insert(key, values);
    }
    if cursor.at != bytes.len() || registry.keys.len() > MAX_KEYS {
        return Err(INVALID_DATA);
    }
    Ok(registry)
}
fn units(bytes: &mut Vec<u8>, units: &[u16]) {
    bytes.extend_from_slice(&(units.len() as u32).to_le_bytes());
    bytes.extend(units.iter().flat_map(|unit| unit.to_le_bytes()));
}
fn encode(registry: &VirtualRegistry) -> Result<Vec<u8>, u32> {
    let keys: Vec<_> = registry
        .keys
        .iter()
        .filter(|(key, _)| !registry.volatile_keys.contains(*key))
        .collect();
    let mut bytes = MAGIC.to_vec();
    bytes.extend_from_slice(&(keys.len() as u32).to_le_bytes());
    for (key, values) in keys {
        bytes.extend_from_slice(&key.root.to_le_bytes());
        units(&mut bytes, &key.path);
        let metadata = registry.metadata.get(key).ok_or(INVALID_DATA)?;
        units(&mut bytes, &metadata.display_name);
        units(&mut bytes, &metadata.class);
        bytes.extend_from_slice(&metadata.modified.to_le_bytes());
        bytes.extend_from_slice(&(values.len() as u32).to_le_bytes());
        for (name, value) in values {
            units(&mut bytes, name);
            units(&mut bytes, &value.display_name);
            bytes.extend_from_slice(&value.kind.to_le_bytes());
            bytes.extend_from_slice(&(value.bytes.len() as u32).to_le_bytes());
            bytes.extend_from_slice(&value.bytes);
        }
        if bytes.len() > MAX_FILE {
            return Err(NO_MEMORY);
        }
    }
    bytes.extend_from_slice(&checksum(&bytes).to_le_bytes());
    Ok(bytes)
}
fn checksum(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |value, byte| {
        (value ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
    })
}

#[cfg(windows)]
fn replace(from: &Path, to: &Path) -> Result<(), u32> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(from: *const u16, to: *const u16, flags: u32) -> i32;
    }
    let source: Vec<_> = from.as_os_str().encode_wide().chain([0]).collect();
    let target: Vec<_> = to.as_os_str().encode_wide().chain([0]).collect();
    // SAFETY: both vectors contain owned, NUL-terminated file paths; they stay
    // alive through the synchronous filesystem call. No guest pointer is used.
    if unsafe { MoveFileExW(source.as_ptr(), target.as_ptr(), 1 | 8) } == 0 {
        Err(io_error(std::io::Error::last_os_error()))
    } else {
        Ok(())
    }
}
#[cfg(not(windows))]
fn replace(from: &Path, to: &Path) -> Result<(), u32> {
    std::fs::rename(from, to).map_err(io_error)?;
    if let Some(parent) = to.parent() {
        File::open(parent)
            .and_then(|file| file.sync_all())
            .map_err(io_error)?;
    }
    Ok(())
}
/// Write nonvolatile keys back, atomically replacing the file.
pub fn flush(registry: &mut VirtualRegistry) -> Result<(), u32> {
    if let Some(error) = registry.load_error {
        return Err(error);
    }
    if !registry.dirty {
        return Ok(());
    }
    let path = registry.storage.as_ref().ok_or(29u32)?;
    let bytes = encode(registry)?;
    let parent = path.parent().ok_or(29u32)?;
    std::fs::create_dir_all(parent).map_err(io_error)?;
    let temp = parent.join(format!(
        "registry-v1.tmp-{}-{}",
        std::process::id(),
        NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .map_err(io_error)?;
    let result = (|| {
        file.write_all(&bytes).map_err(io_error)?;
        file.sync_all().map_err(io_error)?;
        drop(file);
        replace(&temp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result?;
    registry.dirty = false;
    Ok(())
}
impl Drop for VirtualRegistry {
    fn drop(&mut self) {
        if self.dirty
            && let Err(error) = flush(self)
        {
            log::warn!("virtual registry: final flush failed with {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_snapshot_migrates_names_and_retains_data_in_version_two() {
        let mut legacy = b"PALREG01".to_vec();
        legacy.extend_from_slice(&6u32.to_le_bytes());
        for root in [
            0x8000_0000u32,
            0x8000_0001,
            0x8000_0002,
            0x8000_0003,
            0x8000_0005,
        ] {
            legacy.extend_from_slice(&root.to_le_bytes());
            units(&mut legacy, &[]);
            legacy.extend_from_slice(&0u32.to_le_bytes());
        }
        legacy.extend_from_slice(&0x8000_0001u32.to_le_bytes());
        units(&mut legacy, &"CONFIG".encode_utf16().collect::<Vec<_>>());
        legacy.extend_from_slice(&1u32.to_le_bytes());
        units(&mut legacy, &"VALUE".encode_utf16().collect::<Vec<_>>());
        legacy.extend_from_slice(&4u32.to_le_bytes());
        legacy.extend_from_slice(&4u32.to_le_bytes());
        legacy.extend_from_slice(&0x12345678u32.to_le_bytes());
        legacy.extend_from_slice(&checksum(&legacy).to_le_bytes());
        let old = decode(&legacy).unwrap();
        let current = encode(&old).unwrap();
        assert_eq!(&current[..8], MAGIC);
        let new = decode(&current).unwrap();
        let key = Key {
            root: 0x8000_0001,
            path: "CONFIG".encode_utf16().collect(),
        };
        let metadata = new.metadata.get(&key).unwrap();
        assert_eq!(metadata.display_name, key.path);
        assert!(metadata.class.is_empty());
        assert_eq!(metadata.modified, 0);
        let value = new.keys.get(&key).unwrap().values().next().unwrap();
        assert_eq!(
            value.display_name,
            "VALUE".encode_utf16().collect::<Vec<_>>()
        );
        assert_eq!(
            (value.kind, value.bytes.as_slice()),
            (4, 0x12345678u32.to_le_bytes().as_slice())
        );
        assert_eq!(
            (new.total_values, new.total_bytes, new.metadata_bytes),
            (old.total_values, old.total_bytes, old.metadata_bytes)
        );
    }

    #[test]
    fn filesystem_write_failure_is_reported_and_preserves_existing_content() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!("../../target/registry-io-{unique}"));
        std::fs::create_dir_all(&directory).unwrap();
        let blocking = directory.join("registry");
        std::fs::write(&blocking, b"unrelated existing file").unwrap();
        let mut registry = VirtualRegistry::default();
        registry.storage = Some(blocking.join("registry-v1.bin"));
        registry.dirty = true;
        assert!(flush(&mut registry).is_err());
        assert!(registry.dirty, "failure must remain retryable");
        assert_eq!(
            std::fs::read(&blocking).unwrap(),
            b"unrelated existing file"
        );
        registry.dirty = false; // Do not deliberately repeat the same failing Drop IO.
    }

    #[test]
    fn snapshot_rejects_truncation_duplicate_or_oversized_counts_even_with_valid_checksum() {
        let registry = VirtualRegistry::default();
        let good = encode(&registry).unwrap();
        assert!(decode(&good).is_ok());
        for cut in 0..good.len() {
            assert!(decode(&good[..cut]).is_err());
        }
        for offset in [8, 12 + 4] {
            let mut malformed = good[..good.len() - 8].to_vec();
            malformed[offset..offset + 4].copy_from_slice(&u32::MAX.to_le_bytes());
            malformed.extend_from_slice(&checksum(&malformed).to_le_bytes());
            assert!(decode(&malformed).is_err());
        }
        let mut duplicate = good[..good.len() - 8].to_vec();
        // V2 root records are 28 bytes each. Make the second root repeat the first.
        let first_root = duplicate[12..16].to_vec();
        duplicate[40..44].copy_from_slice(&first_root);
        duplicate.extend_from_slice(&checksum(&duplicate).to_le_bytes());
        assert!(decode(&duplicate).is_err());
    }
}
