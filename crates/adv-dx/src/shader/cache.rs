use super::*;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::Path;
const MAX_CACHE_BYTES: u64 = 32 * 1024 * 1024;
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn key(shader: &Shader) -> String {
    let mut hash = Sha256::new();
    hash.update(TRANSLATOR_ABI.as_bytes());
    for word in shader.bytecode() {
        hash.update(word.to_le_bytes());
    }
    format!("{:x}", hash.finalize())
}
pub(super) fn translate_cached(shader: &Shader, directory: &Path) -> Result<Translation, Error> {
    let key = key(shader);
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
    let translation = shader.translate()?;
    let _ = save(directory, &path, &key, &translation);
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
    let mut parts = payload.splitn(3, '\n');
    let flags = parts.next()?.parse::<u8>().ok()?;
    if flags > 3 {
        return None;
    }
    let samplers_text = parts.next()?;
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
        source,
        samplers,
        writes_depth: flags & 1 != 0,
        writes_point_size: flags & 2 != 0,
    };
    translation.validate(stage).ok()?;
    Some(translation)
}
fn save(directory: &Path, path: &Path, key: &str, tr: &Translation) -> std::io::Result<()> {
    std::fs::create_dir_all(directory)?;
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
    let payload = format!("{flags}\n{samplers}\n{}", tr.source);
    let bytes = format!(
        "{TRANSLATOR_ABI}\n{key}\n{}\n{payload}",
        digest(payload.as_bytes())
    );
    if bytes.len() > MAX_TRANSLATION_BYTES {
        return Ok(());
    }
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temporary = path.with_extension(format!("{}.{}.tmp", std::process::id(), nonce));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    let result = (|| {
        file.write_all(bytes.as_bytes())?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result?;
    // Only evict this cache's files, never other contents of a supplied directory.
    let mut entries: Vec<_> = std::fs::read_dir(directory)?
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
    let mut total: u64 = entries.iter().map(|(_, size, _)| *size).sum();
    entries.sort_by_key(|(time, _, _)| *time);
    for (_, size, old) in entries {
        if total <= MAX_CACHE_BYTES {
            break;
        }
        if old != path && std::fs::remove_file(old).is_ok() {
            total = total.saturating_sub(size);
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn corrupted_and_version_cache_regenerates() {
        let shader = Shader::parse(&[
            0xfffe0200, 0x0200001f, 0x80000000, 0x900f0000, 0x02000001, 0xc00f0000, 0x90e40000,
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
        let original = shader.translate_cached(&dir).unwrap();
        let path = dir.join(format!("{}.wgsl-cache", key(&shader)));
        assert!(path.is_file());
        let cached = shader.translate_cached(&dir).unwrap();
        assert_eq!(original.source, cached.source);
        std::fs::write(&path, "corrupt").unwrap();
        assert_eq!(
            original.source,
            shader.translate_cached(&dir).unwrap().source
        );
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, text.replacen(TRANSLATOR_ABI, "old-translator", 1)).unwrap();
        assert_eq!(
            original.source,
            shader.translate_cached(&dir).unwrap().source
        );
        for item in std::fs::read_dir(&dir).unwrap() {
            std::fs::remove_file(item.unwrap().path()).unwrap();
        }
        std::fs::remove_dir(dir).unwrap();
    }
}
