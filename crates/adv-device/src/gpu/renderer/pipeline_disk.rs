//! Optional native Vulkan cache. The portable WGSL cache belongs to the
//! frontend; driver blobs never enter guest memory or repository artifacts.
use super::*;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const MAGIC: &[u8; 8] = b"ADVPIPC1";
const LIMIT: u64 = 32 * 1024 * 1024;
static NEXT_FILE: AtomicU64 = AtomicU64::new(1);
pub(super) struct DiskCache {
    pub(super) cache: wgpu::PipelineCache,
    path: PathBuf,
    dirty: bool,
    updates: u32,
}
impl DiskCache {
    pub(super) fn changed(&mut self) {
        self.dirty = true;
        self.updates += 1;
        if self.updates.is_multiple_of(16) {
            self.save();
        }
    }

    fn save(&mut self) {
        if !self.dirty {
            return;
        }
        let Some(data) = self.cache.get_data().filter(|v| v.len() as u64 <= LIMIT) else {
            return;
        };
        if save_blob(&self.path, &data).is_ok() {
            self.dirty = false;
        }
    }
}
impl Drop for DiskCache {
    fn drop(&mut self) {
        self.save();
    }
}

pub(super) fn load_blob(path: &Path) -> Option<Vec<u8>> {
    let file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    if !(40..=LIMIT + 40).contains(&len) {
        return None;
    }
    let mut bytes = Vec::with_capacity(len as usize);
    file.take(LIMIT + 41).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 != len || &bytes[..8] != MAGIC {
        return None;
    }
    let data = &bytes[40..];
    if Sha256::digest(data).as_slice() != &bytes[8..40] {
        return None;
    }
    Some(data.to_vec())
}
pub(super) fn save_blob(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let Some(directory) = path.parent() else {
        return Err(std::io::ErrorKind::InvalidInput.into());
    };
    std::fs::create_dir_all(directory)?;
    let temporary = directory.join(format!(
        ".pipeline-{}-{}.tmp",
        std::process::id(),
        NEXT_FILE.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(MAGIC)?;
        file.write_all(&Sha256::digest(data))?;
        file.write_all(data)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}
impl Gpu {
    /// Opt in using an application-owned local cache directory. Drivers/backends
    /// without native cache support continue using the portable shader cache.
    pub fn set_pipeline_cache_directory(&mut self, directory: Option<&Path>) {
        self.programmable.warm = warmup::Warmup::default();
        self.programmable.disk = None;
        let Some(directory) = directory else {
            return;
        };
        let key = recipes::partition(&self.context.adapter.get_info());
        let store = recipes::Store::open(directory, key.clone());
        if self
            .context
            .device
            .features()
            .contains(wgpu::Features::PIPELINE_CACHE)
        {
            let path = directory.join(format!("{key}.bin"));
            let data = load_blob(&path);
            // SAFETY: only get_data() results are written with our envelope, checked
            // length and SHA-256. The explicit application-owned cache is trusted
            // like the application's other compiled caches; it is never a guest
            // path. wgpu additionally checks its backend/adapter/driver header and
            // discards obsolete data because fallback is enabled.
            let cache = unsafe {
                self.context
                    .device
                    .create_pipeline_cache(&wgpu::PipelineCacheDescriptor {
                        label: Some("programmable driver cache"),
                        data: data.as_deref(),
                        fallback: true,
                    })
            };
            self.programmable.disk = Some(DiskCache {
                cache,
                path,
                dirty: false,
                updates: 0,
            });
        }
        self.programmable.warm = warmup::Warmup::open(
            store,
            &self.context,
            &self.uniform,
            &self.shader,
            &self.layout,
            self.programmable.disk.as_ref().map(|d| &d.cache),
        );
    }
    pub fn flush_pipeline_cache(&mut self) {
        self.programmable.warm.flush();
        if let Some(disk) = &mut self.programmable.disk {
            disk.save();
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn disk_envelope_rejects_corruption_and_replaces_atomically() {
        let directory = std::env::temp_dir().join(format!(
            "adv-pipeline-cache-{}-{}",
            std::process::id(),
            NEXT_FILE.fetch_add(1, Ordering::Relaxed)
        ));
        let path = directory.join("cache.bin");
        save_blob(&path, b"test payload").unwrap();
        assert_eq!(load_blob(&path).unwrap(), b"test payload");
        save_blob(&path, b"updated payload").unwrap();
        assert_eq!(load_blob(&path).unwrap(), b"updated payload");
        let mut bytes = std::fs::read(&path).unwrap();
        bytes[40] ^= 1;
        std::fs::write(&path, bytes).unwrap();
        assert!(load_blob(&path).is_none());
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_dir(&directory).unwrap();
    }
}
