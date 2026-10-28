//! System discovery only needs name, OS/2 and post tables for fontdb metadata.
//! Keep original file sources and collection indices for full glyph loading.
use super::MAX_SYSTEM_FONT_BYTES;
#[cfg(any(target_os = "macos", test))]
use super::{Database, Source};
#[cfg(any(target_os = "macos", test))]
use std::{collections::BTreeSet, path::PathBuf, sync::Arc};
use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    path::Path,
};

fn read(file: &mut File, offset: u64, length: usize) -> io::Result<Vec<u8>> {
    if length > MAX_SYSTEM_FONT_BYTES {
        return Err(io::ErrorKind::InvalidData.into());
    }
    file.seek(SeekFrom::Start(offset))?;
    let mut data = vec![0; length];
    file.read_exact(&mut data)?;
    Ok(data)
}
fn u32_at(data: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes(data[offset..offset + 4].try_into().unwrap())
}
fn metadata(file: &mut File, offset: u64, charmap: bool) -> io::Result<Vec<u8>> {
    let header = read(file, offset, 12)?;
    if &header[..4] == b"ttcf" {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let count = u16::from_be_bytes([header[4], header[5]]) as usize;
    let directory = read(file, offset + 12, count * 16)?;
    let tables: Vec<_> = directory
        .as_chunks::<16>()
        .0
        .iter()
        .filter(|entry| {
            matches!(&entry[..4], b"name" | b"OS/2" | b"post") || charmap && &entry[..4] == b"cmap"
        })
        .collect();
    if tables.len() > 4 {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let count = tables.len() as u16;
    let power: u16 = if count == 0 { 0 } else { 1 << count.ilog2() };
    let mut data = header;
    data[4..6].copy_from_slice(&count.to_be_bytes());
    data[6..8].copy_from_slice(&(power * 16).to_be_bytes());
    data[8..10].copy_from_slice(&(if power == 0 { 0 } else { power.ilog2() as u16 }).to_be_bytes());
    data[10..12].copy_from_slice(&(count * 16 - power * 16).to_be_bytes());
    data.resize(12 + tables.len() * 16, 0);
    for (index, table) in tables.into_iter().enumerate() {
        let length = u32_at(table, 12) as usize;
        if data.len() + length > MAX_SYSTEM_FONT_BYTES {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let start = data.len() as u32;
        let content = read(file, u64::from(u32_at(table, 8)), length)?;
        let entry = &mut data[12 + index * 16..12 + (index + 1) * 16];
        entry.copy_from_slice(table);
        entry[8..12].copy_from_slice(&start.to_be_bytes());
        data.extend_from_slice(&content);
        data.resize(data.len().next_multiple_of(4), 0);
    }
    Ok(data)
}
/// The naming tables of one face, with its character map when `charmap` is
/// set, and whether it has TrueType outlines.
pub(super) fn face_metadata(path: &Path, index: u32, charmap: bool) -> io::Result<(Vec<u8>, bool)> {
    let mut file = File::open(path)?;
    let header = read(&mut file, 0, 12)?;
    let offset = if &header[..4] == b"ttcf" {
        if index >= u32_at(&header, 8) {
            return Err(io::ErrorKind::InvalidData.into());
        }
        u64::from(u32_at(&read(&mut file, 12 + u64::from(index) * 4, 4)?, 0))
    } else {
        0
    };
    let header = read(&mut file, offset, 12)?;
    let count = u16::from_be_bytes([header[4], header[5]]) as usize;
    let directory = read(&mut file, offset + 12, count * 16)?;
    let truetype = directory
        .as_chunks::<16>()
        .0
        .iter()
        .any(|entry| &entry[..4] == b"glyf");
    Ok((metadata(&mut file, offset, charmap)?, truetype))
}
#[cfg(any(target_os = "macos", test))]
fn load_file(db: &mut Database, path: &Path) -> io::Result<()> {
    let mut file = File::open(path)?;
    let header = read(&mut file, 0, 12)?;
    let offsets = if &header[..4] == b"ttcf" {
        let count = u32_at(&header, 8) as usize;
        read(
            &mut file,
            12,
            count.checked_mul(4).ok_or(io::ErrorKind::InvalidData)?,
        )?
        .as_chunks::<4>()
        .0
        .iter()
        .map(|bytes| u32_at(bytes, 0))
        .collect()
    } else {
        vec![0]
    };
    for (index, offset) in offsets.into_iter().enumerate() {
        let Ok(data) = metadata(&mut file, u64::from(offset), false) else {
            continue;
        };
        let mut probe = Database::new();
        probe.load_font_source(Source::Binary(Arc::new(data)));
        for mut face in probe.faces().cloned() {
            face.source = Source::File(path.to_owned());
            face.index = index as u32;
            db.push_face_info(face);
        }
    }
    Ok(())
}
#[cfg(any(target_os = "macos", test))]
fn load_directory(db: &mut Database, directory: &Path, seen: &mut BTreeSet<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let path = if kind.is_symlink() {
            let Ok(path) = entry.path().canonicalize() else {
                continue;
            };
            path
        } else {
            entry.path()
        };
        if !seen.insert(path.clone()) {
            continue;
        }
        if path.is_dir() {
            load_directory(db, &path, seen);
        } else if matches!(
            path.extension().and_then(|extension| extension.to_str()),
            Some("ttf" | "ttc" | "otf" | "otc" | "TTF" | "TTC" | "OTF" | "OTC")
        ) {
            let _ = load_file(db, &path);
        }
    }
}
#[cfg(target_os = "macos")]
pub(super) fn load_system_fonts(db: &mut Database) {
    // Match fontdb's macOS directories and discovery order.
    let mut seen = BTreeSet::new();
    for directory in ["/Library/Fonts", "/System/Library/Fonts"] {
        load_directory(db, Path::new(directory), &mut seen);
    }
    if let Ok(entries) = std::fs::read_dir("/System/Library/AssetsV2") {
        for entry in entries.flatten() {
            if entry
                .file_name()
                .to_string_lossy()
                .starts_with("com_apple_MobileAsset_Font")
            {
                load_directory(db, &entry.path(), &mut seen);
            }
        }
    }
    load_directory(db, Path::new("/Network/Library/Fonts"), &mut seen);
    if let Some(home) = std::env::var_os("HOME") {
        load_directory(db, &PathBuf::from(home).join("Library/Fonts"), &mut seen);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn compare(expected: &Database, actual: &Database) {
        assert_eq!(expected.faces().count(), actual.faces().count());
        for (a, b) in expected.faces().zip(actual.faces()) {
            assert_eq!(a.index, b.index);
            assert_eq!(a.families, b.families);
            assert_eq!(a.post_script_name, b.post_script_name);
            assert_eq!(
                (a.style, a.weight, a.stretch, a.monospaced),
                (b.style, b.weight, b.stretch, b.monospaced)
            );
            let (Source::File(a), Source::File(b)) = (&a.source, &b.source) else {
                panic!("original file source lost")
            };
            assert_eq!(a, b);
        }
    }
    // spec: DEV-FONT-1
    #[test]
    fn metadata_matches_full_font_and_collection_discovery() {
        for name in ["font-resource.ttf", "font-resource.ttc"] {
            let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures")
                .join(name);
            let mut expected = Database::new();
            expected.load_font_file(&path).unwrap();
            let mut actual = Database::new();
            load_file(&mut actual, &path).unwrap();
            compare(&expected, &actual);
            for info in actual.faces() {
                let (data, truetype) = face_metadata(&path, info.index, false).unwrap();
                assert!(truetype);
                let mut metadata = Database::new();
                metadata.load_font_data(data);
                assert_eq!(metadata.faces().next().unwrap().families, info.families);
            }
        }
    }
    // spec: DEV-FONT-1
    #[test]
    #[cfg(target_os = "macos")]
    #[ignore = "compares metadata for all installed macOS fonts"]
    fn installed_metadata_matches_full_discovery() {
        let mut expected = Database::new();
        expected.load_system_fonts();
        let mut actual = Database::new();
        load_system_fonts(&mut actual);
        compare(&expected, &actual);
    }
}
