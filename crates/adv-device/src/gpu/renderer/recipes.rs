//! Portable recipes contain no object identities or native backend handles.
//! Native binary archives can share this driver/backend partition and envelope.
use super::pipeline_disk::{load_blob, save_blob};
use super::*;
use compile::{Compile, CompileCounters};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
const VERSION: u32 = 1;
const LIMIT: usize = 8 * 1024 * 1024;

#[derive(Clone, Serialize, Deserialize)]
pub(super) enum Recipe {
    Fixed(PipelineKey),
    Program {
        source: Arc<str>,
        textures: Vec<bool>,
        streams: Vec<(u32, Vec<Attribute>)>,
        raster: PipelineKey,
    },
}
impl Recipe {
    pub fn program(key: &program::Key, program: &Program) -> Self {
        Self::Program {
            source: program.source.clone(),
            textures: key.textures.clone(),
            streams: key.streams.clone(),
            raster: key.raster,
        }
    }
    pub fn encoded(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap()
    }
    pub fn key(&self) -> [u8; 32] {
        Sha256::digest(self.encoded()).into()
    }
}
#[derive(Serialize, Deserialize)]
struct Archive {
    version: u32,
    identity: String,
    entries: VecDeque<Recipe>,
}
struct Journal {
    path: PathBuf,
    archive: Archive,
    bytes: usize,
    dirty: bool,
    persisted: u64,
    revision: u64,
    keys: std::collections::HashSet<[u8; 32]>,
    sizes: VecDeque<usize>,
}
pub(super) struct Store {
    journal: Arc<Mutex<Journal>>,
    save: Option<Compile<()>>,
    pub discarded: u64,
}
/// Include the backend, complete driver version and target format. The portable
/// recipes remain useful on backends without native binary archive support.
pub(super) fn partition(info: &wgpu::AdapterInfo, driver_header: Option<&[u8]>) -> String {
    let identity = format!(
        "wgpu-30/{:?}/{}/{}/{}/{}/{:?}/{}",
        info.backend,
        info.vendor,
        info.device,
        info.driver,
        info.driver_info,
        FORMAT,
        std::env::consts::ARCH
    );
    let mut hash = Sha256::new();
    hash.update(identity.as_bytes());
    hash.update(driver_header.unwrap_or_default());
    format!("{:x}", hash.finalize())
}
impl Store {
    pub fn open(directory: &Path, identity: String) -> Self {
        let path = directory.join(format!("{identity}.recipes"));
        let archive = load_blob(&path)
            .filter(|bytes| bytes.len() <= LIMIT)
            .and_then(|bytes| serde_json::from_slice::<Archive>(&bytes).ok())
            .filter(|a| {
                a.version == VERSION && a.identity == identity && a.entries.len() <= MAX_PIPELINES
            });
        let discarded = u64::from(path.exists() && archive.is_none());
        let archive = archive.unwrap_or(Archive {
            version: VERSION,
            identity,
            entries: VecDeque::new(),
        });
        let sizes: VecDeque<_> = archive.entries.iter().map(|r| r.encoded().len()).collect();
        let bytes = sizes.iter().sum();
        let keys = archive.entries.iter().map(Recipe::key).collect();
        let persisted = archive.entries.len() as u64;
        Self {
            journal: Arc::new(Mutex::new(Journal {
                path,
                archive,
                bytes,
                dirty: false,
                persisted,
                revision: 0,
                keys,
                sizes,
            })),
            save: None,
            discarded,
        }
    }
    pub fn entries(&self) -> Vec<Recipe> {
        self.journal
            .lock()
            .unwrap()
            .archive
            .entries
            .iter()
            .cloned()
            .collect()
    }
    pub fn persisted(&self) -> u64 {
        self.journal.lock().unwrap().persisted
    }
    #[cfg(test)]
    pub fn len(&self) -> u64 {
        self.journal.lock().unwrap().archive.entries.len() as u64
    }
    pub fn remember(&mut self, recipe: Recipe) {
        let encoded = recipe.encoded();
        let size = encoded.len();
        let key: [u8; 32] = Sha256::digest(&encoded).into();
        if size > LIMIT / 2 {
            return;
        }
        {
            let mut journal = self.journal.lock().unwrap();
            if journal.keys.contains(&key) {
                return;
            }
            while journal.archive.entries.len() >= MAX_PIPELINES || journal.bytes + size > LIMIT / 2
            {
                let Some(old) = journal.archive.entries.pop_front() else {
                    break;
                };
                journal.keys.remove(&old.key());
                journal.bytes -= journal.sizes.pop_front().unwrap();
            }
            journal.bytes += size;
            journal.keys.insert(key);
            journal.sizes.push_back(size);
            journal.revision += 1;
            journal.archive.entries.push_back(recipe);
            journal.dirty = true;
        }
        if self.save.as_ref().is_none_or(|job| job.ready().is_some()) {
            let journal = self.journal.clone();
            self.save = Some(Compile::spawn(
                LIMIT,
                CompileCounters::default(),
                move || save(&journal),
            ));
        }
    }
    pub fn flush(&mut self) {
        if let Some(job) = self.save.take() {
            let _ = job.resolve();
        }
        let _ = save(&self.journal);
    }
}
fn save(journal: &Mutex<Journal>) -> Result<(), String> {
    let (bytes, path, revision, count) = {
        let journal = journal.lock().unwrap();
        if !journal.dirty {
            return Ok(());
        }
        (
            serde_json::to_vec(&journal.archive).map_err(|e| e.to_string())?,
            journal.path.clone(),
            journal.revision,
            journal.archive.entries.len() as u64,
        )
    };
    if bytes.len() > LIMIT {
        return Err("recipe archive exceeds budget".into());
    }
    save_blob(&path, &bytes).map_err(|e| e.to_string())?;
    {
        let mut journal = journal.lock().unwrap();
        journal.persisted = count;
        if journal.revision == revision {
            journal.dirty = false;
        }
    }
    // Retain four backend/driver partitions; portable WGSL has its own budget.
    if let Some(directory) = path.parent() {
        let mut partitions: Vec<_> = std::fs::read_dir(directory)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .filter_map(|e| {
                let path = e.path();
                (path.extension()?.to_str()? == "recipes")
                    .then_some((e.metadata().ok()?.modified().ok()?, path))
            })
            .collect();
        partitions.sort_by_key(|p| p.0);
        let remove = partitions.len().saturating_sub(4);
        let path_current = &path;
        for (_, path) in partitions.into_iter().take(remove) {
            if &path != path_current {
                let _ = std::fs::remove_file(path.with_extension("bin"));
                let _ = std::fs::remove_file(path);
            }
        }
    }
    Ok(())
}
impl Drop for Store {
    fn drop(&mut self) {
        self.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn recipe(n: u8) -> Recipe {
        Recipe::Fixed(PipelineKey {
            topology: Topology::Triangles,
            cull: Cull::None,
            color_blend: None,
            alpha_blend: None,
            depth: None,
            depth_write: false,
            depth_format: None,
            stencil: None,
            color_write: n,
        })
    }
    fn directory() -> PathBuf {
        std::env::temp_dir().join(format!(
            "adv-recipes-{}-{}",
            std::process::id(),
            NEXT_RESOURCE.fetch_add(1, Ordering::Relaxed)
        ))
    }
    // spec: GFX-CACHE-8, GFX-CACHE-3
    #[test]
    fn truncated_random_version_and_driver_changes_rebuild_safely() {
        let dir = directory();
        let mut store = Store::open(&dir, "driver".into());
        store.remember(recipe(15));
        store.flush();
        let path = dir.join("driver.recipes");
        let original = std::fs::read(&path).unwrap();
        for bytes in [original[..20].to_vec(), vec![0x53; original.len()]] {
            std::fs::write(&path, bytes).unwrap();
            let mut reopened = Store::open(&dir, "driver".into());
            assert_eq!(reopened.len(), 0);
            assert_eq!(reopened.discarded, 1);
            reopened.remember(recipe(15));
            reopened.flush();
            assert_eq!(Store::open(&dir, "driver".into()).len(), 1);
        }
        let bad = Archive {
            version: VERSION + 1,
            identity: "driver".into(),
            entries: [recipe(15)].into(),
        };
        save_blob(&path, &serde_json::to_vec(&bad).unwrap()).unwrap();
        assert_eq!(Store::open(&dir, "driver".into()).discarded, 1);
        let bad = Archive {
            version: VERSION,
            identity: "other-driver".into(),
            entries: [recipe(15)].into(),
        };
        save_blob(&path, &serde_json::to_vec(&bad).unwrap()).unwrap();
        assert_eq!(Store::open(&dir, "driver".into()).len(), 0);
        assert_eq!(Store::open(&dir, "new-driver".into()).len(), 0);
        drop(store);
        std::fs::remove_dir_all(dir).unwrap();
    }
    // spec: GFX-CACHE-8
    #[test]
    fn key_covers_source_layout_texture_visibility_raster_and_formats() {
        let base = Recipe::Program {
            source: "source".into(),
            textures: vec![false],
            streams: vec![(
                16,
                vec![Attribute {
                    location: 0,
                    offset: 0,
                    format: AttributeFormat::Float4,
                }],
            )],
            raster: match recipe(15) {
                Recipe::Fixed(k) => k,
                _ => unreachable!(),
            },
        };
        let variants = (0..6).map(|i| {
            let mut r = base.clone();
            if let Recipe::Program {
                source,
                textures,
                streams,
                raster,
            } = &mut r
            {
                match i {
                    0 => *source = "other".into(),
                    1 => textures[0] = true,
                    2 => streams[0].0 = 32,
                    3 => streams[0].1[0].format = AttributeFormat::Uint8x4,
                    4 => raster.cull = Cull::Clockwise,
                    _ => raster.depth_format = Some(DepthFormat::Float32),
                }
            }
            r
        });
        for r in variants {
            assert_ne!(r.key(), base.key());
        }
    }
    // spec: GFX-CACHE-9, GFX-CACHE-3
    #[test]
    fn byte_entry_and_partition_budgets_evict_oldest() {
        let dir = directory();
        let mut store = Store::open(&dir, "one".into());
        for n in 0..MAX_PIPELINES + 1 {
            let mut r = recipe(15);
            if let Recipe::Fixed(k) = &mut r {
                k.stencil = Some(Stencil {
                    front: StencilFace {
                        compare: Compare::Always,
                        fail: StencilOp::Keep,
                        depth_fail: StencilOp::Keep,
                        pass: StencilOp::Keep,
                    },
                    back: StencilFace {
                        compare: Compare::Always,
                        fail: StencilOp::Keep,
                        depth_fail: StencilOp::Keep,
                        pass: StencilOp::Keep,
                    },
                    read_mask: n as u32,
                    write_mask: 0,
                });
            }
            store.remember(r);
        }
        assert_eq!(store.len(), MAX_PIPELINES as u64);
        let mut large = match recipe(15) {
            Recipe::Fixed(raster) => Recipe::Program {
                source: "a".repeat(LIMIT / 3).into(),
                textures: vec![],
                streams: vec![],
                raster,
            },
            _ => unreachable!(),
        };
        store.remember(large.clone());
        if let Recipe::Program { source, .. } = &mut large {
            *source = "b".repeat(LIMIT / 3).into();
        }
        store.remember(large);
        store.flush();
        assert_eq!(store.len(), 1);
        for n in 0..5 {
            let mut partition = Store::open(&dir, format!("partition-{n}"));
            partition.remember(recipe(15));
            partition.flush();
        }
        assert_eq!(
            std::fs::read_dir(&dir)
                .unwrap()
                .filter_map(Result::ok)
                .filter(|e| e.path().extension().is_some_and(|s| s == "recipes"))
                .count(),
            4
        );
        drop(store);
        std::fs::remove_dir_all(dir).unwrap();
    }
    // spec: GFX-CACHE-10, GFX-CACHE-8
    #[test]
    fn failed_cache_writes_do_not_claim_persisted_entries() {
        let path = directory();
        std::fs::write(&path, b"file").unwrap();
        let mut store = Store::open(&path, "driver".into());
        store.remember(recipe(15));
        store.flush();
        assert_eq!(store.len(), 1);
        assert_eq!(store.persisted(), 0);
        drop(store);
        std::fs::remove_file(path).unwrap();
    }
}
