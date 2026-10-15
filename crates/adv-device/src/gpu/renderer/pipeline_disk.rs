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
    pending: Option<compile::Compile<()>>,
}
impl DiskCache {
    pub(super) fn changed(&mut self) {
        self.dirty = true;
        self.updates += 1;
        if self.updates.is_multiple_of(16)
            && self
                .pending
                .as_ref()
                .is_none_or(|job| job.ready().is_some())
        {
            let (cache, path) = (self.cache.clone(), self.path.clone());
            self.pending = Some(compile::Compile::spawn(
                LIMIT as usize,
                compile::CompileCounters::default(),
                move || {
                    if let Some(data) = cache.get_data().filter(|v| v.len() as u64 <= LIMIT) {
                        save_blob(&path, &data).map_err(|e| e.to_string())?;
                    }
                    Ok(())
                },
            ));
        }
    }
    fn save(&mut self) {
        if let Some(job) = self.pending.take() {
            let _ = job.resolve();
        }
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
    if data.len() as u64 > LIMIT {
        return Err(std::io::ErrorKind::InvalidInput.into());
    }
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
                pending: None,
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

#[cfg(test)]
mod boundary_tests {
    use super::*;
    // spec: GFX-CACHE-3
    #[test]
    fn native_size_boundary_rejects_extended_blobs_before_reading() {
        let dir = std::env::temp_dir().join(format!(
            "adv-driver-bound-{}-{}",
            std::process::id(),
            NEXT_FILE.fetch_add(1, Ordering::Relaxed)
        ));
        let path = dir.join("cache.bin");
        save_blob(&path, b"payload").unwrap();
        let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.set_len(LIMIT + 41).unwrap();
        assert!(load_blob(&path).is_none());
        file.set_len(LIMIT + 40).unwrap();
        assert!(load_blob(&path).is_none()); // exact boundary, invalid checksum
        drop(file);
        std::fs::remove_dir_all(dir).unwrap();
    }
    // spec: GFX-CACHE-3, GFX-CACHE-8
    #[test]
    #[cfg(not(target_vendor = "apple"))]
    fn incompatible_native_driver_header_falls_back_and_rebuilds() {
        let mut gpu = pollster::block_on(Gpu::request_backend(
            !cfg!(target_os = "android"),
            GpuBackend::Vulkan,
        ))
        .unwrap();
        let dir = std::env::temp_dir().join(format!(
            "adv-driver-fallback-{}-{}",
            std::process::id(),
            NEXT_FILE.fetch_add(1, Ordering::Relaxed)
        ));
        gpu.set_pipeline_cache_directory(Some(&dir));
        let disk = gpu.programmable.disk.as_mut().unwrap();
        let mut data = disk.cache.get_data().unwrap();
        // wgpu 30's driver validation key occupies bytes 32..48, before any
        // native payload. A mismatched key must never reach the Vulkan driver.
        assert!(data.len() >= 64);
        data[32] ^= 1;
        let path = disk.path.clone();
        save_blob(&path, &data).unwrap();
        gpu.set_pipeline_cache_directory(Some(&dir));
        let target = gpu.target(1, 1, false).unwrap();
        gpu.draw(
            &target,
            &[Vertex {
                position: [-1., -1., 0., 1.],
                color: [1.; 4],
                uv: [0.; 2],
            }; 3],
            None,
            DrawState::new(1, 1),
        )
        .unwrap();
        gpu.flush_pipeline_cache();
        gpu.check().unwrap();
        assert!(load_blob(&path).is_some());
        gpu.set_pipeline_cache_directory(None);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
