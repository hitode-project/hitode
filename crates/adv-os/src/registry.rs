//! The private virtual registry: keys, values and open-key handles, persisted as a bounded
//! versioned file inside the runtime data folder. Never opens the host registry. Error
//! values are Win32 error codes.
use adv_machine::{Namespace, Objects};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

mod store;

pub use store::{flush, load};

pub const NOT_FOUND: u32 = 2;
pub const ACCESS_DENIED: u32 = 5;
pub const INVALID_HANDLE: u32 = 6;
pub const NO_MEMORY: u32 = 8;
pub const INVALID_DATA: u32 = 13;
pub const NOT_SUPPORTED: u32 = 50;
pub const INVALID_PARAMETER: u32 = 87;
pub const MORE_DATA: u32 = 234;
pub const NOACCESS: u32 = 998;
pub const QUERY: u32 = 1;
pub const SET: u32 = 2;
pub const ALL_ACCESS: u32 = 0xf003f;
pub const MAX_VALUE_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_TOTAL_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_KEYS: usize = 16_384;
pub const MAX_HANDLES: usize = 16_384;
pub const MAX_VALUES: usize = 16_384;
pub const MAX_METADATA_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Key {
    pub root: u32,
    pub path: Vec<u16>,
}
impl Key {
    pub fn parent(&self) -> Self {
        let path = self
            .path
            .iter()
            .rposition(|&unit| unit == 92)
            .map_or(Vec::new(), |at| self.path[..at].to_vec());
        Self {
            root: self.root,
            path,
        }
    }
}
pub struct OpenKey {
    pub key: Key,
    pub access: u32,
    pub deleted: bool,
}
pub struct Value {
    pub kind: u32,
    pub display_name: Vec<u16>,
    // String types are stored as UTF-16 bytes, including exactly the supplied
    // terminators. Other types retain their byte representation and length.
    pub bytes: Vec<u8>,
}
#[derive(Clone, Default)]
pub struct KeyMetadata {
    pub display_name: Vec<u16>,
    pub class: Vec<u16>,
    pub modified: u64,
}
impl KeyMetadata {
    pub fn size(&self) -> usize {
        16 + (self.display_name.len() + self.class.len()) * 2
    }
}
pub struct VirtualRegistry {
    pub keys: BTreeMap<Key, BTreeMap<Vec<u16>, Value>>,
    pub metadata: BTreeMap<Key, KeyMetadata>,
    namespace: Option<Namespace>,
    pub total_bytes: usize,
    pub total_values: usize,
    pub metadata_bytes: usize,
    pub volatile_keys: BTreeSet<Key>,
    pub storage: Option<PathBuf>,
    pub dirty: bool,
    pub load_error: Option<u32>,
    pub storage_lock: Option<std::fs::File>,
}
impl Default for VirtualRegistry {
    fn default() -> Self {
        let keys: BTreeMap<_, _> = [
            0x8000_0000,
            0x8000_0001,
            0x8000_0002,
            0x8000_0003,
            0x8000_0005,
        ]
        .into_iter()
        .map(|root| {
            (
                Key {
                    root,
                    path: Vec::new(),
                },
                BTreeMap::new(),
            )
        })
        .collect();
        let metadata = keys
            .keys()
            .cloned()
            .map(|key| (key, KeyMetadata::default()))
            .collect();
        Self {
            keys,
            metadata,
            namespace: None,
            total_bytes: 0,
            total_values: 0,
            metadata_bytes: 5 * 28,
            volatile_keys: BTreeSet::new(),
            storage: None,
            dirty: false,
            load_error: None,
            storage_lock: None,
        }
    }
}
impl VirtualRegistry {
    pub fn key(&self, objects: &Objects, handle: u32) -> Result<OpenKey, u32> {
        if let Some(error) = self.load_error {
            return Err(error);
        }
        let predefined = Key {
            root: handle,
            path: Vec::new(),
        };
        if self.keys.contains_key(&predefined) {
            return Ok(OpenKey {
                key: predefined,
                access: ALL_ACCESS,
                deleted: false,
            });
        }
        let id = objects
            .lookup_alias::<OpenKey>(self.namespace.ok_or(INVALID_HANDLE)?, handle)
            .ok_or(INVALID_HANDLE)?;
        let open = objects.owned(id).ok_or(INVALID_HANDLE)?;
        if open.deleted {
            return Err(1018);
        }
        Ok(OpenKey {
            key: open.key.clone(),
            access: open.access,
            deleted: false,
        })
    }
    pub fn handle_count(&self, objects: &Objects) -> usize {
        self.namespace.map_or(0, |ns| objects.alias_count(ns))
    }
    pub fn allocate(&mut self, objects: &mut Objects, key: Key, access: u32) -> Result<u32, u32> {
        if self.handle_count(objects) >= MAX_HANDLES {
            return Err(NO_MEMORY);
        }
        let namespace = match self.namespace {
            Some(ns) => ns,
            None => {
                // Keep registry handles outside the usual kernel-handle range;
                // misusing CloseHandle must not accidentally close a thread.
                let ns = objects
                    .new_namespace_in(0x6b00_0000, 0x6fff_fffc)
                    .ok_or(NO_MEMORY)?;
                self.namespace = Some(ns);
                ns
            }
        };
        objects
            .insert_host_in(
                namespace,
                OpenKey {
                    key,
                    access,
                    deleted: false,
                },
            )
            .ok_or(NO_MEMORY)
    }
    pub fn close(&self, objects: &mut Objects, handle: u32) -> Result<(), u32> {
        if self.keys.contains_key(&Key {
            root: handle,
            path: Vec::new(),
        }) {
            return Ok(());
        }
        let ns = self.namespace.ok_or(INVALID_HANDLE)?;
        objects
            .lookup_alias::<OpenKey>(ns, handle)
            .ok_or(INVALID_HANDLE)?;
        objects.close_alias(ns, handle).ok_or(INVALID_HANDLE)?;
        Ok(())
    }
    /// Deletion tombstones existing opens even if the same path is recreated.
    pub fn mark_deleted(&self, objects: &mut Objects, key: &Key) {
        if let Some(ns) = self.namespace {
            for (_, open) in objects.hosts_in_mut::<OpenKey>(ns) {
                if &open.key == key {
                    open.deleted = true;
                }
            }
        }
    }
}
/// Canonical (case-folded) spelling of a key or value name.
pub fn fold(name: &[u16]) -> Vec<u16> {
    name.iter()
        .map(|&unit| crate::nls::unit_case(unit, true))
        .collect()
}
/// The key `subkey` names under `parent`, and every key on the way (including it).
pub fn child_key(parent: &Key, subkey: &[u16]) -> Result<(Key, Vec<Key>), u32> {
    if subkey.first() == Some(&92) {
        return Err(INVALID_PARAMETER);
    }
    let mut key = parent.clone();
    let mut parents = Vec::new();
    let parts: Vec<_> = subkey
        .split(|&unit| unit == 92)
        .filter(|p| !p.is_empty())
        .collect();
    if parts.len() > 32 {
        return Err(INVALID_PARAMETER);
    }
    for part in parts {
        if part.len() > 255 {
            return Err(INVALID_PARAMETER);
        }
        if !key.path.is_empty() {
            key.path.push(92)
        }
        key.path.extend(fold(part));
        parents.push(key.clone());
    }
    if key.path.len() > 32_767 || key.path.iter().filter(|&&u| u == 92).count() >= 512 {
        return Err(INVALID_PARAMETER);
    }
    Ok((key, parents))
}
