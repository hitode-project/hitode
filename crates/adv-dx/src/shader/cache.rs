use super::*;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{OnceLock, mpsc};
const MAX_CACHE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_QUEUED_WRITES: usize = 64;
static TMP_NONCE: AtomicU64 = AtomicU64::new(1);

enum WriterMsg {
    Write {
        directory: PathBuf,
        path: PathBuf,
        bytes: Vec<u8>,
    },
    #[cfg(test)]
    Flush(mpsc::SyncSender<()>),
}

struct DirIndex {
    entries: VecDeque<(PathBuf, u64)>,
    total: u64,
}

impl DirIndex {
    fn scan(directory: &Path) -> Self {
        let mut found: Vec<_> = std::fs::read_dir(directory)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let path = entry.path();
                if path.extension()?.to_str()? != "wgsl-cache" {
                    return None;
                }
                let stem = path.file_stem()?.to_str()?;
                if stem.len() != 64 || !stem.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return None;
                }
                let metadata = entry.metadata().ok()?;
                if !metadata.is_file() {
                    return None;
                }
                Some((metadata.modified().ok()?, metadata.len(), path))
            })
            .collect();
        found.sort_by_key(|(time, _, _)| *time);
        let total = found.iter().map(|(_, size, _)| *size).sum();
        let entries = found
            .into_iter()
            .map(|(_, size, path)| (path, size))
            .collect();
        Self { entries, total }
    }

    fn record_and_prune(&mut self, path: &Path, size: u64) {
        if let Some(pos) = self.entries.iter().position(|(p, _)| p == path) {
            let (_, old_size) = self.entries.remove(pos).unwrap();
            self.total = self.total.saturating_sub(old_size);
        }
        self.entries.push_back((path.to_path_buf(), size));
        self.total = self.total.saturating_add(size);
        while self.total > MAX_CACHE_BYTES {
            let Some((old, old_size)) = self.entries.pop_front() else {
                break;
            };
            if old == path {
                self.entries.push_back((old, old_size));
                break;
            }
            match std::fs::remove_file(&old) {
                Ok(()) => self.total = self.total.saturating_sub(old_size),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    self.total = self.total.saturating_sub(old_size);
                }
                Err(_) => {}
            }
        }
    }
}

fn writer() -> Option<&'static mpsc::SyncSender<WriterMsg>> {
    static WRITER: OnceLock<Option<mpsc::SyncSender<WriterMsg>>> = OnceLock::new();
    WRITER
        .get_or_init(|| {
            let (tx, rx) = mpsc::sync_channel::<WriterMsg>(MAX_QUEUED_WRITES);
            std::thread::Builder::new()
                .name("adv-wgsl-cache".into())
                .spawn(move || {
                    let mut dirs: HashMap<PathBuf, DirIndex> = HashMap::new();
                    while let Ok(msg) = rx.recv() {
                        match msg {
                            WriterMsg::Write {
                                directory,
                                path,
                                bytes,
                            } => {
                                let _ = write_entry(&mut dirs, &directory, &path, &bytes);
                            }
                            #[cfg(test)]
                            WriterMsg::Flush(done) => {
                                dirs.clear();
                                let _ = done.send(());
                            }
                        }
                    }
                })
                .ok()
                .map(|_| tx)
        })
        .as_ref()
}

fn write_entry(
    dirs: &mut HashMap<PathBuf, DirIndex>,
    directory: &Path,
    path: &Path,
    bytes: &[u8],
) -> std::io::Result<()> {
    if !directory.is_dir() {
        dirs.remove(directory);
    }
    std::fs::create_dir_all(directory)?;
    let index = dirs
        .entry(directory.to_path_buf())
        .or_insert_with(|| DirIndex::scan(directory));
    let nonce = TMP_NONCE.fetch_add(1, Ordering::Relaxed);
    let temporary = path.with_extension(format!("{}.{}.tmp", std::process::id(), nonce));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        drop(file);
        std::fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result?;
    index.record_and_prune(path, bytes.len() as u64);
    Ok(())
}

#[cfg(test)]
fn flush_writer() {
    if let Some(tx) = writer() {
        let (done_tx, done_rx) = mpsc::sync_channel(1);
        if tx.send(WriterMsg::Flush(done_tx)).is_ok() {
            let _ = done_rx.recv();
        }
    }
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn key(shader: &Shader, ieee_f32: bool) -> String {
    let mut hash = Sha256::new();
    hash.update(TRANSLATOR_ABI.as_bytes());
    hash.update([u8::from(ieee_f32)]);
    for word in shader.bytecode() {
        hash.update(word.to_le_bytes());
    }
    format!("{:x}", hash.finalize())
}
pub(super) fn translate_cached(
    shader: &Shader,
    directory: &Path,
    ieee_f32: bool,
) -> Result<Translation, Error> {
    let key = key(shader, ieee_f32);
    let path = directory.join(format!("{key}.wgsl-cache"));
    if let Ok(file) = std::fs::File::open(&path) {
        let mut bytes = Vec::new();
        if file
            .take(MAX_TRANSLATION_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .is_ok()
            && bytes.len() <= MAX_TRANSLATION_BYTES
            && let Some(translation) = decode(&bytes, &key, shader.stage)
        {
            return Ok(translation);
        }
    }
    let translation = shader.translate(ieee_f32)?;
    enqueue_save(directory, &path, &key, &translation);
    Ok(translation)
}
fn decode(bytes: &[u8], key: &str, stage: Stage) -> Option<Translation> {
    let text = std::str::from_utf8(bytes).ok()?;
    let mut parts = text.splitn(4, '\n');
    if parts.next()? != TRANSLATOR_ABI || parts.next()? != key {
        return None;
    }
    let expected = parts.next()?;
    let payload = parts.next()?;
    if expected != digest(payload.as_bytes()) {
        return None;
    }
    let mut parts = payload.splitn(4, '\n');
    let flags = parts.next()?.parse::<u8>().ok()?;
    if flags > 3 {
        return None;
    }
    let samplers_text = parts.next()?;
    let usage = parts
        .next()?
        .split(',')
        .map(str::parse::<u64>)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    let constants = adv_device::constants::ConstantUsage {
        registers: usage.get(..5)?.try_into().ok()?,
        relative: *usage.get(5)? != 0,
    };
    if usage.len() != 6 || usage[5] > 1 || constants.registers[4] >> 48 != 0 {
        return None;
    }
    let source = parts.next()?.to_string();
    let mut samplers = Vec::new();
    if !samplers_text.is_empty() {
        for field in samplers_text.split(',') {
            let (index, dimension) = field.split_once(':')?;
            let index = index.parse::<u8>().ok()?;
            if index >= if stage == Stage::Vertex { 4 } else { 16 }
                || samplers.iter().any(|s: &SamplerBinding| s.index == index)
            {
                return None;
            }
            let dimension = match dimension {
                "2" => SamplerDimension::D2,
                "3" => SamplerDimension::D3,
                "c" => SamplerDimension::Cube,
                _ => return None,
            };
            samplers.push(SamplerBinding { index, dimension });
        }
    }
    let translation = Translation {
        constants,
        source,
        samplers,
        writes_depth: flags & 1 != 0,
        writes_point_size: flags & 2 != 0,
    };
    translation.validate(stage).ok()?;
    Some(translation)
}
fn enqueue_save(directory: &Path, path: &Path, key: &str, tr: &Translation) {
    let flags = u8::from(tr.writes_depth) | u8::from(tr.writes_point_size) << 1;
    let samplers = tr
        .samplers
        .iter()
        .map(|s| {
            format!(
                "{}:{}",
                s.index,
                match s.dimension {
                    SamplerDimension::D2 => "2",
                    SamplerDimension::D3 => "3",
                    SamplerDimension::Cube => "c",
                }
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let usage = tr
        .constants
        .registers
        .into_iter()
        .chain([u64::from(tr.constants.relative)])
        .map(|v| v.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let payload = format!("{flags}\n{samplers}\n{usage}\n{}", tr.source);
    let bytes = format!(
        "{TRANSLATOR_ABI}\n{key}\n{}\n{payload}",
        digest(payload.as_bytes())
    );
    if bytes.len() > MAX_TRANSLATION_BYTES {
        return;
    }
    if let Some(tx) = writer() {
        let _ = tx.try_send(WriterMsg::Write {
            directory: directory.to_path_buf(),
            path: path.to_path_buf(),
            bytes: bytes.into_bytes(),
        });
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // spec: GFX-CACHE-2
    fn corrupted_and_version_cache_regenerates() {
        let shader = Shader::parse(&[
            0xfffe0200, 0x0200001f, 0x80000000, 0x900f0000, 0x02000001, 0xc00f0000, 0xa0e40003,
            0xffff,
        ])
        .unwrap();
        let dir = std::env::temp_dir().join(format!(
            "adv-shader-cache-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let original = shader.translate_cached(&dir, false).unwrap();
        flush_writer();
        let path = dir.join(format!("{}.wgsl-cache", key(&shader, false)));
        assert!(path.is_file());
        let cached = shader.translate_cached(&dir, false).unwrap();
        assert_eq!(original.source, cached.source);
        assert_eq!(original.constants, cached.constants);
        assert_eq!(cached.constants.registers[0], 1 << 3);
        let payload = format!("0\n\n0,0,0,0,{},0\n{}", u64::MAX, original.source);
        let malformed = format!(
            "{TRANSLATOR_ABI}\n{}\n{}\n{payload}",
            key(&shader, false),
            digest(payload.as_bytes())
        );
        assert!(decode(malformed.as_bytes(), &key(&shader, false), shader.stage).is_none());
        std::fs::write(&path, "corrupt").unwrap();
        assert_eq!(
            original.source,
            shader.translate_cached(&dir, false).unwrap().source
        );
        flush_writer();
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, text.replacen(TRANSLATOR_ABI, "old-translator", 1)).unwrap();
        assert_eq!(
            original.source,
            shader.translate_cached(&dir, false).unwrap().source
        );
        flush_writer();
        for item in std::fs::read_dir(&dir).unwrap() {
            std::fs::remove_file(item.unwrap().path()).unwrap();
        }
        std::fs::remove_dir(dir).unwrap();
    }
    #[test]
    // spec: GFX-CACHE-3
    fn aggregate_wgsl_eviction_leaves_unrelated_files() {
        let shader =
            Shader::parse(&[0xffff0200, 0x02000001, 0x800f0800, 0xa0e40000, 0xffff]).unwrap();
        let dir = std::env::temp_dir().join(format!(
            "adv-wgsl-evict-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        for n in 0..2 {
            std::fs::File::create(dir.join(format!("{n:064x}.wgsl-cache")))
                .unwrap()
                .set_len(MAX_CACHE_BYTES)
                .unwrap();
        }
        let unrelated = dir.join("keep.bin");
        std::fs::File::create(&unrelated)
            .unwrap()
            .set_len(MAX_CACHE_BYTES * 2)
            .unwrap();
        shader.translate_cached(&dir, false).unwrap();
        flush_writer();
        let total: u64 = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.path().extension().is_some_and(|v| v == "wgsl-cache"))
            .map(|e| e.metadata().unwrap().len())
            .sum();
        assert!(total <= MAX_CACHE_BYTES);
        assert!(unrelated.is_file());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
