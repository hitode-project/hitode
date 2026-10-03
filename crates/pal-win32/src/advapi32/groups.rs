//! Enumeration is stable in canonical UTF-16 order. Names retain their original
//! spelling; deleted handles remain closable but cannot observe a recreated key.
use pal_cpu::{GuestExt, Prot};
use pal_machine::{Env, Outcome, Registry};

use super::{
    ACCESS_DENIED, INVALID_DATA, INVALID_PARAMETER, Key, KeyMetadata, MORE_DATA, NOACCESS, NOT_FOUND, QUERY, SET,
    Value, VirtualRegistry, child_key, fold, name, output, state, string_type,
};
use crate::{Win32, stdcall};

const ENUMERATE: u32 = 8;
const NO_MORE_ITEMS: u32 = 259;
fn range(env: &Env<'_>, ptr: u32, len: usize, optional: bool) -> Result<(), u32> {
    if ptr == 0 {
        return if optional { Ok(()) } else { Err(INVALID_PARAMETER) };
    }
    if ptr.checked_add(len as u32).is_none() || !env.cpu.accessible(ptr, len as u32, Prot::WRITE) {
        return Err(NOACCESS);
    }
    Ok(())
}
fn capacity(env: &Env<'_>, ptr: u32, optional: bool) -> Result<u32, u32> {
    output(env, ptr, optional)?;
    if ptr == 0 {
        return Ok(0);
    }
    if !env.cpu.accessible(ptr, 4, Prot::READ) {
        return Err(NOACCESS);
    }
    env.cpu.read_u32(ptr).map_err(|_| NOACCESS)
}
fn put_word(env: &mut Env<'_>, ptr: u32, value: u32) -> Result<(), u32> {
    if ptr != 0 {
        env.cpu.write_u32(ptr, value).map_err(|_| NOACCESS)?;
    }
    Ok(())
}
fn text(env: &Env<'_>, units: &[u16], wide: bool) -> (Vec<u8>, usize) {
    let mut bytes = if wide {
        units.iter().flat_map(|unit| unit.to_le_bytes()).collect()
    } else {
        env.ext.get::<Win32>().codec.from_utf16(0, units).0
    };
    let count = bytes.len() / if wide { 2 } else { 1 };
    bytes.extend(if wide { vec![0, 0] } else { vec![0] });
    (bytes, count)
}
fn value_data(env: &Env<'_>, value: Value, wide: bool) -> Result<Vec<u8>, u32> {
    if !wide && string_type(value.kind) {
        if value.bytes.len() % 2 != 0 {
            return Err(INVALID_DATA);
        }
        let units: Vec<_> = value.bytes.chunks_exact(2).map(|p| u16::from_le_bytes([p[0], p[1]])).collect();
        Ok(env.ext.get::<Win32>().codec.from_utf16(0, &units).0)
    } else {
        Ok(value.bytes)
    }
}
fn child(parent: &Key, candidate: &Key) -> bool {
    if parent.root != candidate.root {
        return false;
    }
    let suffix = if parent.path.is_empty() {
        candidate.path.as_slice()
    } else {
        if !candidate.path.starts_with(&parent.path) || candidate.path.get(parent.path.len()) != Some(&92) {
            return false;
        }
        &candidate.path[parent.path.len() + 1..]
    };
    !suffix.is_empty() && !suffix.contains(&92)
}
fn children<'a>(registry: &'a VirtualRegistry, parent: &'a Key) -> impl Iterator<Item = (&'a Key, &'a KeyMetadata)> {
    let root = parent.root;
    let mut prefix = parent.path.clone();
    if !prefix.is_empty() {
        prefix.push(92);
    }
    let start = Key { root, path: prefix.clone() };
    registry
        .metadata
        .range(start..)
        .take_while(move |(key, _)| key.root == root && key.path.starts_with(&prefix))
        .filter(move |(key, _)| child(parent, key))
}
fn write_time(env: &mut Env<'_>, ptr: u32, value: u64) -> Result<(), u32> {
    if ptr != 0 {
        env.cpu.write(ptr, &value.to_le_bytes()).map_err(|_| NOACCESS)?;
    }
    Ok(())
}
fn enum_key(env: &mut Env<'_>, wide: bool) -> Outcome {
    let result = (|| -> Result<u32, u32> {
        if env.arg(4) != 0 {
            return Err(INVALID_PARAMETER);
        }
        let (hkey, index, buffer, name_size, class_out, class_size, time_out) =
            (env.arg(0), env.arg(1) as usize, env.arg(2), env.arg(3), env.arg(5), env.arg(6), env.arg(7));
        let opened = state(env).key(hkey)?;
        if opened.access & ENUMERATE == 0 {
            return Err(ACCESS_DENIED);
        }
        let name_capacity = capacity(env, name_size, false)? as usize;
        let class_capacity = capacity(env, class_size, class_out == 0)? as usize;
        if buffer == 0 {
            return Err(INVALID_PARAMETER);
        }
        range(env, time_out, 8, true)?;
        let metadata =
            children(state(env), &opened.key).nth(index).map(|(_, metadata)| metadata.clone()).ok_or(NO_MORE_ITEMS)?;
        let (name, name_len) = text(env, &metadata.display_name, wide);
        let (class, class_len) = text(env, &metadata.class, wide);
        write_time(env, time_out, metadata.modified)?;
        if name_capacity <= name_len || (class_out != 0 && class_capacity <= class_len) {
            // Native EnumKeyEx leaves the name capacity untouched on short
            // buffers and clears the optional class count. Neither string is copied.
            put_word(env, class_size, 0)?;
            return Err(MORE_DATA);
        }
        range(env, buffer, name.len(), false)?;
        if class_out != 0 {
            range(env, class_out, class.len(), false)?;
        }
        env.cpu.write(buffer, &name).map_err(|_| NOACCESS)?;
        if class_out != 0 {
            env.cpu.write(class_out, &class).map_err(|_| NOACCESS)?;
        }
        put_word(env, name_size, name_len as u32)?;
        put_word(env, class_size, class_len as u32)?;
        Ok(0)
    })();
    Outcome::Return(result.unwrap_or_else(|error| error))
}
fn enum_value(env: &mut Env<'_>, wide: bool) -> Outcome {
    let result = (|| -> Result<u32, u32> {
        if env.arg(4) != 0 {
            return Err(INVALID_PARAMETER);
        }
        let (hkey, index, buffer, name_size, type_out, data_out, data_size) =
            (env.arg(0), env.arg(1) as usize, env.arg(2), env.arg(3), env.arg(5), env.arg(6), env.arg(7));
        let opened = state(env).key(hkey)?;
        if opened.access & QUERY == 0 {
            return Err(ACCESS_DENIED);
        }
        let name_capacity = capacity(env, name_size, false)? as usize;
        let data_capacity = capacity(env, data_size, data_out == 0)? as usize;
        output(env, type_out, true)?;
        if buffer == 0 {
            return Err(INVALID_PARAMETER);
        }
        let value =
            state(env).keys.get(&opened.key).and_then(|values| values.values().nth(index)).ok_or(NO_MORE_ITEMS)?;
        let raw_size = value.bytes.len();
        let convert = !wide && string_type(value.kind);
        let (kind, display_name, value) = (
            value.kind,
            value.display_name.clone(),
            if data_out != 0 || convert {
                Some(Value { kind: value.kind, bytes: value.bytes.clone(), display_name: Vec::new() })
            } else {
                None
            },
        );
        // Size-only binary/W calls never copy the payload. ANSI string sizes
        // require conversion; a real data copy owns at most one raw buffer.
        let bytes = value.map(|value| value_data(env, value, wide)).transpose()?;
        let data_len = bytes.as_ref().map_or(raw_size, Vec::len);
        let (name, name_len) = text(env, &display_name, wide);
        put_word(env, type_out, kind)?;
        put_word(env, data_size, data_len as u32)?;
        if name_capacity <= name_len || (!wide && name_capacity > 32767) {
            return Err(MORE_DATA);
        }
        range(env, buffer, name.len(), false)?;
        env.cpu.write(buffer, &name).map_err(|_| NOACCESS)?;
        put_word(env, name_size, name_len as u32)?;
        if data_out == 0 {
            return Ok(0);
        }
        if data_capacity < data_len {
            return Err(MORE_DATA);
        }
        let bytes = bytes.unwrap(); // A nonnull data_out always owns the payload.
        if !bytes.is_empty() {
            range(env, data_out, bytes.len(), false)?;
            env.cpu.write(data_out, &bytes).map_err(|_| NOACCESS)?;
        }
        Ok(0)
    })();
    Outcome::Return(result.unwrap_or_else(|error| error))
}
fn query_info(env: &mut Env<'_>, wide: bool) -> Outcome {
    let result = (|| -> Result<u32, u32> {
        if env.arg(3) != 0 {
            return Err(INVALID_PARAMETER);
        }
        let (hkey, class_out, class_size, time_out) = (env.arg(0), env.arg(1), env.arg(2), env.arg(11));
        let opened = state(env).key(hkey)?;
        if opened.access & QUERY == 0 {
            return Err(ACCESS_DENIED);
        }
        let class_capacity = capacity(env, class_size, class_out == 0)? as usize;
        let outputs: Vec<_> = (4..11).map(|i| env.arg(i)).collect();
        for &out in &outputs {
            output(env, out, true)?;
        }
        range(env, time_out, 8, true)?;
        let registry = state(env);
        let metadata = registry.metadata.get(&opened.key).ok_or(NOT_FOUND)?.clone();
        let child_metadata: Vec<_> = children(registry, &opened.key).map(|(_, metadata)| metadata).collect();
        let values = registry.keys.get(&opened.key).ok_or(NOT_FOUND)?;
        // Native A/W QueryInfo both report the stored Unicode maxima, while
        // actual Enum A output lengths and value bytes are ACP-converted.
        let counts = [
            child_metadata.len(),
            child_metadata.iter().map(|meta| meta.display_name.len()).max().unwrap_or(0),
            child_metadata.iter().map(|meta| meta.class.len()).max().unwrap_or(0),
            values.len(),
            values.values().map(|value| value.display_name.len()).max().unwrap_or(0),
            values.values().map(|value| value.bytes.len()).max().unwrap_or(0),
            20,
        ]; // The private namespace's self-relative default security descriptor.
        let (class, class_len) = text(env, &metadata.class, wide);
        if wide && class_out != 0 && class_capacity != 0 && class_capacity <= class_len {
            // Native W clears statistics/time when its class query needs a
            // larger buffer; the ANSI wrapper collects metadata before conversion.
            for &out in &outputs {
                put_word(env, out, 0)?;
            }
            write_time(env, time_out, 0)?;
            put_word(env, class_size, class_len as u32)?;
            return Err(MORE_DATA);
        }
        for (&out, count) in outputs.iter().zip(counts) {
            put_word(env, out, count as u32)?;
        }
        write_time(env, time_out, metadata.modified)?;
        if class_out == 0 || class_capacity == 0 {
            put_word(env, class_size, 0)?;
            return Ok(0);
        }
        if class_capacity <= class_len {
            // ANSI QueryInfo converts after collecting metadata. Preserve DBCS
            // character boundaries, terminates a short class buffer, and reports 234.
            let codec = env.ext.get::<Win32>().codec;
            let limit = class_capacity - 1;
            let mut at = 0;
            while at < class_len {
                let width = if codec.is_lead_byte(0, class[at]) { 2 } else { 1 };
                if at + width > limit {
                    break;
                }
                at += width;
            }
            range(env, class_out, at + 1, false)?;
            env.cpu.write(class_out, &class[..at]).map_err(|_| NOACCESS)?;
            env.cpu.write(class_out + at as u32, &[0]).map_err(|_| NOACCESS)?;
            put_word(env, class_size, at as u32)?;
            return Err(MORE_DATA);
        }
        range(env, class_out, class.len(), false)?;
        env.cpu.write(class_out, &class).map_err(|_| NOACCESS)?;
        put_word(env, class_size, class_len as u32)?;
        Ok(0)
    })();
    Outcome::Return(result.unwrap_or_else(|error| error))
}
fn delete_value(env: &mut Env<'_>, wide: bool) -> Outcome {
    let result = (|| -> Result<u32, u32> {
        let hkey = env.arg(0);
        let value_name = fold(&name(env, env.arg(1), wide, true)?);
        let modified = crate::kernel32::filetime_now(env);
        let opened = state(env).key(hkey)?;
        if opened.access & SET == 0 {
            return Err(ACCESS_DENIED);
        }
        let registry = state(env);
        let value =
            registry.keys.get_mut(&opened.key).and_then(|values| values.remove(&value_name)).ok_or(NOT_FOUND)?;
        registry.total_bytes -= value.bytes.len();
        registry.total_values -= 1;
        registry.metadata_bytes -= 16 + (value_name.len() + value.display_name.len()) * 2;
        registry.metadata.get_mut(&opened.key).ok_or(NOT_FOUND)?.modified = modified;
        if !registry.volatile_keys.contains(&opened.key) {
            registry.dirty = true;
        }
        Ok(0)
    })();
    Outcome::Return(result.unwrap_or_else(|error| error))
}
fn delete_key(env: &mut Env<'_>, wide: bool) -> Outcome {
    let result = (|| -> Result<u32, u32> {
        let hkey = env.arg(0);
        let subkey = name(env, env.arg(1), wide, false)?;
        let modified = crate::kernel32::filetime_now(env);
        let opened = state(env).key(hkey)?;
        let (key, _) = child_key(&opened.key, &subkey)?;
        if key.path.is_empty() {
            return Err(ACCESS_DENIED);
        }
        let registry = state(env);
        if !registry.keys.contains_key(&key) {
            return Err(NOT_FOUND);
        }
        if children(registry, &key).next().is_some() {
            return Err(ACCESS_DENIED);
        }
        let values = registry.keys.remove(&key).ok_or(NOT_FOUND)?;
        let metadata = registry.metadata.remove(&key).ok_or(NOT_FOUND)?;
        registry.metadata_bytes -= 12 + key.path.len() * 2 + metadata.size();
        for (name, value) in values {
            registry.total_bytes -= value.bytes.len();
            registry.total_values -= 1;
            registry.metadata_bytes -= 16 + (name.len() + value.display_name.len()) * 2;
        }
        if !registry.volatile_keys.remove(&key) {
            registry.dirty = true;
        }
        for opened in registry.handles.values_mut() {
            if opened.key == key {
                opened.deleted = true;
            }
        }
        let parent = key.parent();
        if let Some(metadata) = registry.metadata.get_mut(&parent) {
            metadata.modified = modified;
        }
        if !registry.volatile_keys.contains(&parent) {
            registry.dirty = true;
        }
        Ok(0)
    })();
    Outcome::Return(result.unwrap_or_else(|error| error))
}
pub(super) fn register(registry: &mut Registry) {
    for (name, argc, handler) in [
        ("RegEnumKeyExA", 8, (|e: &mut Env<'_>| enum_key(e, false)) as pal_machine::Handler),
        ("RegEnumKeyExW", 8, |e| enum_key(e, true)),
        ("RegEnumValueA", 8, |e| enum_value(e, false)),
        ("RegEnumValueW", 8, |e| enum_value(e, true)),
        ("RegQueryInfoKeyA", 12, |e| query_info(e, false)),
        ("RegQueryInfoKeyW", 12, |e| query_info(e, true)),
        ("RegDeleteValueA", 2, |e| delete_value(e, false)),
        ("RegDeleteValueW", 2, |e| delete_value(e, true)),
        ("RegDeleteKeyA", 2, |e| delete_key(e, false)),
        ("RegDeleteKeyW", 2, |e| delete_key(e, true)),
    ] {
        stdcall(registry, "advapi32.dll", name, argc, handler);
    }
}
