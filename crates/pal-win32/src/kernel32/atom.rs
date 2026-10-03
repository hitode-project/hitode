//! Process-environment global atoms. No atom is installed in the host OS table.
use std::collections::{BTreeMap, BTreeSet};

use pal_cpu::{GuestExt, Prot};
use pal_machine::{Env, Outcome, Registry};

use crate::{Win32, stdcall, util::set_last_error};

struct Entry {
    spelling: Vec<u16>,
    key: Vec<u16>,
    references: u32,
}

struct Atoms {
    names: BTreeMap<Vec<u16>, u16>,
    entries: BTreeMap<u16, Entry>,
    free: BTreeSet<u16>,
    next: u32,
}

impl Default for Atoms {
    fn default() -> Self {
        Self {
            names: BTreeMap::new(),
            entries: BTreeMap::new(),
            free: BTreeSet::new(),
            next: 0xc000,
        }
    }
}

enum Name {
    Integer(u16),
    String(Vec<u16>),
}

fn name(env: &Env<'_>, wide: bool) -> Result<Name, u32> {
    let pointer = env.arg(0);
    if pointer < 0x10000 {
        return if (1..0xc000).contains(&pointer) {
            Ok(Name::Integer(pointer as u16))
        } else {
            Err(87)
        };
    }
    let units = if wide {
        let mut units = Vec::new();
        for i in 0..=255 {
            let address = pointer.checked_add(i * 2).ok_or(87u32)?;
            let unit = env.cpu.read_u16(address).map_err(|_| 87u32)?;
            if unit == 0 {
                break;
            }
            units.push(unit);
        }
        units
    } else {
        let bytes = env.cpu.read_cstr(pointer, 256).map_err(|_| 87u32)?;
        env.ext.get::<Win32>().codec.to_utf16(0, &bytes)
    };
    if units.is_empty() {
        return Err(123);
    }
    if units.len() > 255 {
        return Err(87);
    }
    if units[0] == b'#' as u16
        && units.len() > 1
        && units[1..].iter().all(|c| (0x30..=0x39).contains(c))
    {
        let value = units[1..]
            .iter()
            .try_fold(0u32, |v, &c| {
                v.checked_mul(10)?.checked_add(u32::from(c - 0x30))
            })
            .ok_or(87u32)?;
        return if (1..0xc000).contains(&value) {
            Ok(Name::Integer(value as u16))
        } else {
            Err(87)
        };
    }
    Ok(Name::String(units))
}

fn fail(env: &mut Env<'_>, error: u32) -> Outcome {
    set_last_error(env, error);
    Outcome::Return(0)
}

fn lookup(env: &mut Env<'_>, wide: bool, add: bool) -> Outcome {
    let spelling = match name(env, wide) {
        Ok(Name::Integer(atom)) => return Outcome::Return(u32::from(atom)),
        Ok(Name::String(units)) => units,
        Err(error) => return fail(env, error),
    };
    let key: Vec<_> = spelling
        .iter()
        .map(|&u| crate::user32::strings::unit_case(u, true))
        .collect();
    if env.ext.try_get_mut::<Atoms>().is_none() {
        env.ext.insert(Atoms::default());
    }
    let table = env.ext.get_mut::<Atoms>();
    if let Some(&id) = table.names.get(&key) {
        if add {
            let entry = table.entries.get_mut(&id).unwrap();
            let Some(references) = entry.references.checked_add(1) else {
                return fail(env, 8);
            };
            entry.references = references;
        }
        return Outcome::Return(u32::from(id));
    }
    if !add {
        return fail(env, 2);
    }
    let id = if let Some(id) = table.free.pop_first() {
        id
    } else if table.next <= u32::from(u16::MAX) {
        let id = table.next as u16;
        table.next += 1;
        id
    } else {
        return fail(env, 8);
    };
    table.names.insert(key.clone(), id);
    table.entries.insert(
        id,
        Entry {
            spelling,
            key,
            references: 1,
        },
    );
    Outcome::Return(u32::from(id))
}

fn delete(env: &mut Env<'_>) -> Outcome {
    let atom = env.arg(0) as u16;
    if atom < 0xc000 {
        return Outcome::Return(0);
    }
    let Some(table) = env.ext.try_get_mut::<Atoms>() else {
        return fail(env, 6);
    };
    let Some(entry) = table.entries.get_mut(&atom) else {
        return fail(env, 6);
    };
    entry.references -= 1;
    if entry.references == 0 {
        let entry = table.entries.remove(&atom).unwrap();
        table.names.remove(&entry.key);
        table.free.insert(atom);
    }
    Outcome::Return(0)
}

fn get_name(env: &mut Env<'_>, wide: bool) -> Outcome {
    let (atom, out, capacity) = (env.arg(0) as u16, env.arg(1), env.arg(2) as i32);
    if capacity <= 0 {
        return fail(env, 234);
    }
    let units = if (1..0xc000).contains(&atom) {
        format!("#{atom}").encode_utf16().collect::<Vec<_>>()
    } else if let Some(table) = env.ext.try_get_mut::<Atoms>() {
        match table.entries.get(&atom) {
            Some(entry) => entry.spelling.clone(),
            None => return fail(env, 6),
        }
    } else {
        return fail(env, 6);
    };
    let mut bytes = if wide {
        units.iter().flat_map(|c| c.to_le_bytes()).collect()
    } else {
        env.ext.get::<Win32>().codec.from_utf16(0, &units).0
    };
    let width = if wide { 2 } else { 1 };
    let length = bytes.len() / width;
    let n = length.min(capacity as usize - usize::from(!wide));
    bytes.truncate(n * width);
    // W truncation consumes the whole buffer without a terminator. A writes a
    // NUL-terminated prefix but returns ERROR_MORE_DATA, as native probes show.
    if !wide || length < capacity as usize {
        bytes.resize(bytes.len() + width, 0);
    }
    if !env.cpu.accessible(out, bytes.len() as u32, Prot::WRITE) {
        return fail(env, 998);
    }
    if let Err(error) = env.cpu.write(out, &bytes) {
        return Outcome::Fail(error.to_string());
    }
    if !wide && n < length {
        fail(env, 234)
    } else {
        Outcome::Return(n as u32)
    }
}

pub(super) fn register(reg: &mut Registry) {
    stdcall(reg, super::K32, "GlobalAddAtomA", 1, |e| {
        lookup(e, false, true)
    });
    stdcall(reg, super::K32, "GlobalAddAtomW", 1, |e| {
        lookup(e, true, true)
    });
    stdcall(reg, super::K32, "GlobalFindAtomA", 1, |e| {
        lookup(e, false, false)
    });
    stdcall(reg, super::K32, "GlobalFindAtomW", 1, |e| {
        lookup(e, true, false)
    });
    stdcall(reg, super::K32, "GlobalDeleteAtom", 1, delete);
    stdcall(reg, super::K32, "GlobalGetAtomNameA", 3, |e| {
        get_name(e, false)
    });
    stdcall(reg, super::K32, "GlobalGetAtomNameW", 3, |e| {
        get_name(e, true)
    });
}
