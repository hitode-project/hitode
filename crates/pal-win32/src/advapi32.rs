//! Private virtual registry. Never opens the host registry. Nonvolatile data
//! lives in a bounded versioned file inside the configured runtime data folder.
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

mod groups;
mod store;

use pal_cpu::{GuestExt, Prot};
use pal_machine::{Env, Outcome, Registry};

use crate::{Win32, stdcall};

const NOT_FOUND: u32 = 2;
const ACCESS_DENIED: u32 = 5;
const INVALID_HANDLE: u32 = 6;
const NO_MEMORY: u32 = 8;
const INVALID_DATA: u32 = 13;
const NOT_SUPPORTED: u32 = 50;
const INVALID_PARAMETER: u32 = 87;
const MORE_DATA: u32 = 234;
const NOACCESS: u32 = 998;
const QUERY: u32 = 1;
const SET: u32 = 2;
const ALL_ACCESS: u32 = 0xf003f;
const MAX_VALUE_BYTES: usize = 16 * 1024 * 1024;
const MAX_TOTAL_BYTES: usize = 32 * 1024 * 1024;
const MAX_KEYS: usize = 16_384;
const MAX_HANDLES: usize = 16_384;
const MAX_VALUES: usize = 16_384;
const MAX_METADATA_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Key {
    root: u32,
    path: Vec<u16>,
}
impl Key {
    fn parent(&self) -> Self {
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
struct OpenKey {
    key: Key,
    access: u32,
    deleted: bool,
}
struct Value {
    kind: u32,
    display_name: Vec<u16>,
    // String types are stored as UTF-16 bytes, including exactly the supplied
    // terminators. Other types retain their byte representation and length.
    bytes: Vec<u8>,
}
#[derive(Clone, Default)]
struct KeyMetadata {
    display_name: Vec<u16>,
    class: Vec<u16>,
    modified: u64,
}
impl KeyMetadata {
    fn size(&self) -> usize {
        16 + (self.display_name.len() + self.class.len()) * 2
    }
}
struct VirtualRegistry {
    keys: BTreeMap<Key, BTreeMap<Vec<u16>, Value>>,
    metadata: BTreeMap<Key, KeyMetadata>,
    handles: BTreeMap<u32, OpenKey>,
    next_handle: u32,
    total_bytes: usize,
    total_values: usize,
    metadata_bytes: usize,
    volatile_keys: BTreeSet<Key>,
    storage: Option<PathBuf>,
    dirty: bool,
    load_error: Option<u32>,
    storage_lock: Option<std::fs::File>,
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
            handles: BTreeMap::new(),
            next_handle: 0x6b00_0000,
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
    fn key(&self, handle: u32) -> Result<OpenKey, u32> {
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
        let open = self.handles.get(&handle).ok_or(INVALID_HANDLE)?;
        if open.deleted {
            return Err(1018);
        }
        Ok(OpenKey {
            key: open.key.clone(),
            access: open.access,
            deleted: false,
        })
    }
    fn allocate(&mut self, key: Key, access: u32) -> Result<u32, u32> {
        if self.handles.len() >= MAX_HANDLES {
            return Err(NO_MEMORY);
        }
        let handle = self.next_handle;
        self.next_handle = handle
            .checked_add(4)
            .filter(|next| *next < 0x7000_0000)
            .ok_or(NO_MEMORY)?;
        self.handles.insert(
            handle,
            OpenKey {
                key,
                access,
                deleted: false,
            },
        );
        Ok(handle)
    }
}
fn state<'a>(env: &'a mut Env<'_>) -> &'a mut VirtualRegistry {
    if env.ext.try_get::<VirtualRegistry>().is_none() {
        let path = env
            .ext
            .get::<Win32>()
            .cfg
            .data_dir
            .join("registry/registry-v1.bin");
        env.ext.insert(store::load(path));
    }
    env.ext.get_mut::<VirtualRegistry>()
}
fn fold(name: &[u16]) -> Vec<u16> {
    name.iter()
        .map(|&unit| crate::user32::strings::unit_case(unit, true))
        .collect()
}
fn name(env: &Env<'_>, ptr: u32, wide: bool, nullable: bool) -> Result<Vec<u16>, u32> {
    if ptr == 0 {
        return if nullable {
            Ok(Vec::new())
        } else {
            Err(INVALID_PARAMETER)
        };
    }
    if wide {
        let mut units = Vec::new();
        for index in 0..=16_383u32 {
            let at = ptr.checked_add(index * 2).ok_or(NOACCESS)?;
            if !env.cpu.accessible(at, 2, Prot::READ) {
                return Err(NOACCESS);
            }
            let unit = env.cpu.read_u16(at).map_err(|_| NOACCESS)?;
            if unit == 0 {
                return Ok(units);
            }
            units.push(unit);
        }
    } else {
        let mut bytes = Vec::new();
        for index in 0..32_768u32 {
            let at = ptr.checked_add(index).ok_or(NOACCESS)?;
            if !env.cpu.accessible(at, 1, Prot::READ) {
                return Err(NOACCESS);
            }
            let byte = env.cpu.read_u8(at).map_err(|_| NOACCESS)?;
            if byte == 0 {
                let units = env.ext.get::<Win32>().codec.to_utf16(0, &bytes);
                return if units.len() <= 16_383 {
                    Ok(units)
                } else {
                    Err(INVALID_PARAMETER)
                };
            }
            bytes.push(byte);
        }
    }
    Err(INVALID_PARAMETER)
}
fn output(env: &Env<'_>, ptr: u32, optional: bool) -> Result<(), u32> {
    if ptr == 0 {
        return if optional {
            Ok(())
        } else {
            Err(INVALID_PARAMETER)
        };
    }
    if ptr.checked_add(4).is_none() || !env.cpu.accessible(ptr, 4, Prot::WRITE) {
        return Err(NOACCESS);
    }
    Ok(())
}
fn desired_access(mut access: u32) -> Result<u32, u32> {
    if access & 0x300 == 0x300 {
        return Err(INVALID_PARAMETER);
    }
    // The provider models a single 32-bit view. Do not pretend an explicit
    // 64-bit view is equivalent to it or fabricate host-machine contents.
    if access & 0x100 != 0 {
        return Err(NOT_SUPPORTED);
    }
    access &= !0x200;
    if access & 0x8000_0000 != 0 {
        access = (access & !0x8000_0000) | 0x20019
    }
    if access & 0x4000_0000 != 0 {
        access = (access & !0x4000_0000) | 0x20006
    }
    if access & 0x2000_0000 != 0 {
        access = (access & !0x2000_0000) | 0x20019
    }
    if access & 0x1000_0000 != 0 {
        access = (access & !0x1000_0000) | ALL_ACCESS
    }
    if access & 0x0200_0000 != 0 {
        access = (access & !0x0200_0000) | ALL_ACCESS
    }
    if access & !ALL_ACCESS != 0 {
        return Err(ACCESS_DENIED);
    }
    Ok(access)
}
fn child_key(parent: &Key, subkey: &[u16]) -> Result<(Key, Vec<Key>), u32> {
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
fn open_key(env: &mut Env<'_>, wide: bool, create: bool, legacy: bool) -> Outcome {
    let result = (|| -> Result<u32, u32> {
        let hkey = env.arg(0);
        let (options, access, out, disposition) = if legacy {
            (0, ALL_ACCESS, env.arg(2), 0)
        } else if create {
            if env.arg(2) != 0 {
                return Err(INVALID_PARAMETER);
            }
            (env.arg(4), env.arg(5), env.arg(7), env.arg(8))
        } else {
            (env.arg(2), env.arg(3), env.arg(4), 0)
        };
        if (create && options & !1 != 0) || (!create && options != 0) {
            return Err(NOT_SUPPORTED);
        }
        let access = desired_access(access)?;
        output(env, out, false)?;
        output(env, disposition, true)?;
        if create && !legacy && env.arg(6) != 0 {
            let sa = env.arg(6);
            if !env.cpu.accessible(sa, 12, Prot::READ) {
                return Err(NOACCESS);
            }
            if env.cpu.read_u32(sa).map_err(|_| NOACCESS)? != 12 {
                return Err(INVALID_PARAMETER);
            }
            if env.cpu.read_u32(sa + 4).map_err(|_| NOACCESS)? != 0 {
                return Err(NOT_SUPPORTED);
            }
        }
        let subkey = name(env, env.arg(1), wide, !create || legacy)?;
        let class = if create && !legacy {
            name(env, env.arg(3), wide, true)?
        } else {
            Vec::new()
        };
        let modified = crate::kernel32::filetime_now(env);
        let parent = state(env).key(hkey)?;
        let (key, parents) = child_key(&parent.key, &subkey)?;
        let registry = state(env);
        let existed = registry.keys.contains_key(&key);
        if !create && !existed {
            return Err(NOT_FOUND);
        }
        if registry.handles.len() >= MAX_HANDLES {
            return Err(NO_MEMORY);
        }
        if create && !existed {
            let missing_keys: Vec<_> = parents
                .iter()
                .filter(|key| !registry.keys.contains_key(*key))
                .collect();
            let missing = missing_keys.len();
            let parts: Vec<_> = subkey
                .split(|&unit| unit == 92)
                .filter(|part| !part.is_empty())
                .collect();
            let additions: BTreeMap<_, _> = parents
                .iter()
                .zip(parts)
                .map(|(candidate, part)| {
                    let class = if candidate == &key {
                        class.clone()
                    } else {
                        Vec::new()
                    };
                    (
                        candidate.clone(),
                        KeyMetadata {
                            display_name: part.to_vec(),
                            class,
                            modified,
                        },
                    )
                })
                .collect();
            let metadata: usize = missing_keys
                .iter()
                .map(|key| 12 + key.path.len() * 2 + additions[*key].size())
                .sum();
            if registry.metadata_bytes + metadata > MAX_METADATA_BYTES {
                return Err(NO_MEMORY);
            }
            let ancestor_volatile = std::iter::once(&parent.key)
                .chain(parents.iter())
                .any(|key| registry.volatile_keys.contains(key));
            if options == 0 && ancestor_volatile {
                return Err(1021);
            } // ERROR_CHILD_MUST_BE_VOLATILE
            if registry.keys.len() + missing > MAX_KEYS {
                return Err(NO_MEMORY);
            }
            // This virtual namespace uses a default writable ACL. Like Win32,
            // creating a child checks that ACL, not the parent's handle mask.
            for parent in parents {
                if !registry.keys.contains_key(&parent) {
                    if options & 1 != 0 {
                        registry.volatile_keys.insert(parent.clone());
                    } else {
                        registry.dirty = true;
                    }
                    registry
                        .metadata
                        .insert(parent.clone(), additions[&parent].clone());
                    let immediate_parent = parent.parent();
                    registry.keys.insert(parent, BTreeMap::new());
                    if let Some(meta) = registry.metadata.get_mut(&immediate_parent) {
                        meta.modified = modified;
                    }
                    if !registry.volatile_keys.contains(&immediate_parent) {
                        registry.dirty = true;
                    }
                }
            }
            registry.metadata_bytes += metadata;
        }
        let handle = if !create && subkey.is_empty() && (hkey & 0x8000_0000 != 0) {
            hkey
        } else {
            state(env).allocate(key, access)?
        };
        env.cpu.write_u32(out, handle).map_err(|_| NOACCESS)?;
        if disposition != 0 {
            env.cpu
                .write_u32(disposition, if existed { 2 } else { 1 })
                .map_err(|_| NOACCESS)?;
        }
        Ok(0)
    })();
    Outcome::Return(result.unwrap_or_else(|error| error))
}
fn string_type(kind: u32) -> bool {
    matches!(kind, 1 | 2 | 7)
}
fn set_value(env: &mut Env<'_>, wide: bool) -> Outcome {
    let result = (|| -> Result<u32, u32> {
        if env.arg(2) != 0 {
            return Err(INVALID_PARAMETER);
        }
        let display_name = name(env, env.arg(1), wide, true)?;
        let name = fold(&display_name);
        let modified = crate::kernel32::filetime_now(env);
        let hkey = env.arg(0);
        let opened = state(env).key(hkey)?;
        if opened.access & SET == 0 {
            return Err(ACCESS_DENIED);
        }
        let (kind, ptr, count) = (env.arg(3), env.arg(4), env.arg(5) as usize);
        if count > MAX_VALUE_BYTES {
            return Err(NO_MEMORY);
        }
        if count != 0
            && (ptr == 0
                || ptr.checked_add(count as u32).is_none()
                || !env.cpu.accessible(ptr, count as u32, Prot::READ))
        {
            return Err(NOACCESS);
        }
        let mut bytes = vec![0; count];
        if count != 0 {
            env.cpu.read(ptr, &mut bytes).map_err(|_| NOACCESS)?
        }
        if !wide && string_type(kind) {
            bytes = env
                .ext
                .get::<Win32>()
                .codec
                .to_utf16(0, &bytes)
                .into_iter()
                .flat_map(u16::to_le_bytes)
                .collect();
        }
        let registry = state(env);
        let values = registry.keys.get_mut(&opened.key).ok_or(INVALID_HANDLE)?;
        let previous = values.get(&name).map_or(0, |v| v.bytes.len());
        let new_value = !values.contains_key(&name);
        if new_value && registry.total_values >= MAX_VALUES {
            return Err(NO_MEMORY);
        }
        let total = registry.total_bytes - previous + bytes.len();
        let metadata = if new_value {
            16 + (name.len() + display_name.len()) * 2
        } else {
            0
        };
        if registry.metadata_bytes + metadata > MAX_METADATA_BYTES {
            return Err(NO_MEMORY);
        }
        if bytes.len() > MAX_VALUE_BYTES || total > MAX_TOTAL_BYTES {
            return Err(NO_MEMORY);
        }
        let display_name = values
            .get(&name)
            .map_or(display_name, |value| value.display_name.clone());
        values.insert(
            name,
            Value {
                kind,
                bytes,
                display_name,
            },
        );
        registry.total_bytes = total;
        registry.total_values += usize::from(new_value);
        registry.metadata_bytes += metadata;
        if !registry.volatile_keys.contains(&opened.key) {
            registry.dirty = true;
        }
        if let Some(meta) = registry.metadata.get_mut(&opened.key) {
            meta.modified = modified;
        }
        Ok(0)
    })();
    Outcome::Return(result.unwrap_or_else(|error| error))
}
fn query_value(env: &mut Env<'_>, wide: bool) -> Outcome {
    let result = (|| -> Result<u32, u32> {
        if env.arg(2) != 0 {
            return Err(INVALID_PARAMETER);
        }
        let name = fold(&name(env, env.arg(1), wide, true)?);
        let hkey = env.arg(0);
        let opened = state(env).key(hkey)?;
        if opened.access & QUERY == 0 {
            return Err(ACCESS_DENIED);
        }
        let (type_out, data_out, size_out) = (env.arg(3), env.arg(4), env.arg(5));
        output(env, type_out, true)?;
        output(env, size_out, data_out == 0)?;
        let capacity = if data_out != 0 {
            env.cpu.read_u32(size_out).map_err(|_| NOACCESS)?
        } else {
            0
        };
        let value = state(env)
            .keys
            .get(&opened.key)
            .and_then(|values| values.get(&name))
            .ok_or(NOT_FOUND)?;
        let (kind, mut bytes) = (value.kind, value.bytes.clone());
        if !wide && string_type(kind) {
            if bytes.len() % 2 != 0 {
                return Err(INVALID_DATA);
            }
            let units: Vec<_> = bytes
                .chunks_exact(2)
                .map(|p| u16::from_le_bytes([p[0], p[1]]))
                .collect();
            bytes = env.ext.get::<Win32>().codec.from_utf16(0, &units).0;
        }
        if type_out != 0 {
            env.cpu.write_u32(type_out, kind).map_err(|_| NOACCESS)?;
        }
        if size_out != 0 {
            env.cpu
                .write_u32(size_out, bytes.len() as u32)
                .map_err(|_| NOACCESS)?;
        }
        if data_out == 0 {
            return Ok(0);
        }
        if capacity < bytes.len() as u32 {
            return Err(MORE_DATA);
        }
        if !bytes.is_empty() {
            if !env
                .cpu
                .accessible(data_out, bytes.len() as u32, Prot::WRITE)
            {
                return Err(NOACCESS);
            }
            env.cpu.write(data_out, &bytes).map_err(|_| NOACCESS)?;
        }
        Ok(0)
    })();
    Outcome::Return(result.unwrap_or_else(|error| error))
}
fn close_key(env: &mut Env<'_>) -> Outcome {
    let key = env.arg(0);
    let registry = state(env);
    let code = if registry.keys.contains_key(&Key {
        root: key,
        path: Vec::new(),
    }) || registry.handles.remove(&key).is_some()
    {
        0
    } else {
        INVALID_HANDLE
    };
    Outcome::Return(code)
}

fn flush_key(env: &mut Env<'_>) -> Outcome {
    let hkey = env.arg(0);
    let result = (|| {
        let opened = state(env).key(hkey)?;
        if opened.access & QUERY == 0 {
            return Err(ACCESS_DENIED);
        }
        store::flush(state(env))?;
        Ok(0)
    })();
    Outcome::Return(result.unwrap_or_else(|error| error))
}

pub fn register(reg: &mut Registry) {
    groups::register(reg);
    for (name, argc, handler) in [
        (
            "RegOpenKeyExA",
            5,
            (|e: &mut Env<'_>| open_key(e, false, false, false)) as pal_machine::Handler,
        ),
        ("RegOpenKeyExW", 5, |e| open_key(e, true, false, false)),
        ("RegOpenKeyA", 3, |e| open_key(e, false, false, true)),
        ("RegOpenKeyW", 3, |e| open_key(e, true, false, true)),
        ("RegCreateKeyExA", 9, |e| open_key(e, false, true, false)),
        ("RegCreateKeyExW", 9, |e| open_key(e, true, true, false)),
        ("RegCreateKeyA", 3, |e| open_key(e, false, true, true)),
        ("RegCreateKeyW", 3, |e| open_key(e, true, true, true)),
        ("RegSetValueExA", 6, |e| set_value(e, false)),
        ("RegSetValueExW", 6, |e| set_value(e, true)),
        ("RegQueryValueExA", 6, |e| query_value(e, false)),
        ("RegQueryValueExW", 6, |e| query_value(e, true)),
        ("RegCloseKey", 1, close_key),
        ("RegFlushKey", 1, flush_key),
    ] {
        stdcall(reg, "advapi32.dll", name, argc, handler);
    }
}
