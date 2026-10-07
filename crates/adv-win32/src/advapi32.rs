//! Registry API over the private virtual registry in [`adv_os::registry`].
use std::collections::BTreeMap;

mod groups;
mod identity;
mod scm;
pub(crate) mod token;

mod legacy;

use adv_abi::Registry;
use adv_cpu::backend::{GuestExt, Prot};
use adv_machine::{Env, Outcome};
use adv_os::registry::{self as store, *};

use crate::{Win32, stdcall};

pub(crate) fn state<'a>(env: &'a mut Env<'_>) -> &'a mut VirtualRegistry {
    if env.ext.try_get::<VirtualRegistry>().is_none() {
        let path = env
            .ext
            .get::<Win32>()
            .cfg
            .os
            .data_dir
            .join("registry/registry-v1.bin");
        env.ext.insert(store::load(path));
    }
    env.ext.get_mut::<VirtualRegistry>()
}
pub(crate) fn opened(env: &mut Env<'_>, handle: u32) -> Result<OpenKey, u32> {
    state(env);
    env.ext
        .get::<VirtualRegistry>()
        .key(&env.sys.objects, handle)
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
pub(crate) fn desired_access(mut access: u32) -> Result<u32, u32> {
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
        let parent = opened(env, hkey)?;
        let (key, parents) = child_key(&parent.key, &subkey)?;
        let registry = env.ext.get_mut::<VirtualRegistry>();
        let existed = registry.keys.contains_key(&key);
        if !create && !existed {
            return Err(NOT_FOUND);
        }
        if registry.handle_count(&env.sys.objects) >= MAX_HANDLES {
            return Err(NO_MEMORY);
        }
        let mut additions = BTreeMap::new();
        let mut metadata = 0;
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
            additions = parents
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
            metadata = missing_keys
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
        }
        // Reserve ownership after every semantic check, before mutating the
        // persistent key tree. Table exhaustion must not leave a created key.
        let handle = if !create && subkey.is_empty() && (hkey & 0x8000_0000 != 0) {
            hkey
        } else {
            registry.allocate(&mut env.sys.objects, key.clone(), access)?
        };
        if create && !existed {
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
        let opened = opened(env, hkey)?;
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
        let opened = opened(env, hkey)?;
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
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&p| u16::from_le_bytes(p))
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
    state(env);
    let code = env
        .ext
        .get::<VirtualRegistry>()
        .close(&mut env.sys.objects, key)
        .map_or_else(|error| error, |()| 0);
    Outcome::Return(code)
}

fn flush_key(env: &mut Env<'_>) -> Outcome {
    let hkey = env.arg(0);
    let result = (|| {
        let opened = opened(env, hkey)?;
        if opened.access & QUERY == 0 {
            return Err(ACCESS_DENIED);
        }
        store::flush(state(env))?;
        Ok(0)
    })();
    Outcome::Return(result.unwrap_or_else(|error| error))
}

fn notify_change_key_value(_env: &mut Env<'_>) -> Outcome {
    Outcome::Return(0)
}

pub fn register(reg: &mut Registry) {
    legacy::register(reg);
    groups::register(reg);
    identity::register(reg);
    scm::register(reg);
    token::register(reg);
    for (name, argc, handler) in [
        (
            "RegOpenKeyExA",
            5,
            (|e: &mut Env<'_>| open_key(e, false, false, false)) as adv_machine::Handler,
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
        ("RegNotifyChangeKeyValue", 5, notify_change_key_value),
    ] {
        stdcall(reg, "advapi32.dll", name, argc, handler);
    }
}
