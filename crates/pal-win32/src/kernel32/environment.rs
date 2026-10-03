//! Process-local environment. A/W calls share UTF-16 state; enumeration returns
//! owned snapshots whose lifetime is independent of later environment changes.
use std::collections::{BTreeMap, BTreeSet};

use pal_cpu::{GuestExt, Prot};
use pal_machine::{Env, Outcome, Registry};

use crate::{Win32, stdcall, util::set_last_error};

const MAX_UNITS: usize = 32_767;
const MAX_ENV_UNITS: usize = 2 * 1024 * 1024;

struct Variable {
    name: Vec<u16>,
    value: Vec<u16>,
}
#[derive(Default)]
struct Environment {
    variables: BTreeMap<Vec<u16>, Variable>,
    snapshots: BTreeSet<u32>,
    units: usize,
}

fn fold(name: &[u16]) -> Vec<u16> {
    name.iter()
        .map(|&unit| crate::user32::strings::unit_case(unit, true))
        .collect()
}
fn initialize(env: &mut Env<'_>) {
    if env.ext.try_get::<Environment>().is_some() {
        return;
    }
    let mut state = Environment::default();
    for entry in super::process::environment(env.ext.get::<Win32>()) {
        // The leading '=' of the per-drive current-directory entry is part of
        // its name, not the delimiter between its name and value.
        let at = entry[usize::from(entry.starts_with('='))..]
            .find('=')
            .unwrap()
            + usize::from(entry.starts_with('='));
        let name: Vec<_> = entry[..at].encode_utf16().collect();
        let value: Vec<_> = entry[at + 1..].encode_utf16().collect();
        state.units += name.len() + value.len() + 2;
        state
            .variables
            .insert(fold(&name), Variable { name, value });
    }
    env.ext.insert(state);
}

fn read(env: &Env<'_>, pointer: u32, wide: bool) -> Result<Vec<u16>, u32> {
    if pointer == 0 {
        return Err(87);
    }
    if wide {
        let mut result = Vec::new();
        for index in 0..=MAX_UNITS {
            let at = pointer.checked_add((index * 2) as u32).ok_or(998u32)?;
            let unit = env.cpu.read_u16(at).map_err(|_| 998u32)?;
            if unit == 0 {
                return Ok(result);
            }
            result.push(unit);
        }
    } else {
        // A guest may explicitly choose UTF-8 ACP; its BMP characters need up
        // to three bytes per UTF-16 unit. Bound bytes first, units after decode.
        let limit = MAX_UNITS * 4 + 1;
        let bytes = env.cpu.read_cstr(pointer, limit).map_err(|_| 998u32)?;
        if bytes.len() == limit {
            return Err(206);
        }
        pointer.checked_add(bytes.len() as u32).ok_or(998u32)?;
        let result = env.ext.get::<Win32>().codec.to_utf16(0, &bytes);
        if result.len() <= MAX_UNITS {
            return Ok(result);
        }
    }
    Err(206)
}
fn encoded(env: &Env<'_>, value: &[u16], wide: bool) -> Vec<u8> {
    if wide {
        value.iter().flat_map(|unit| unit.to_le_bytes()).collect()
    } else {
        env.ext.get::<Win32>().codec.from_utf16(0, value).0
    }
}
fn error(env: &mut Env<'_>, error: u32) -> Outcome {
    set_last_error(env, error);
    Outcome::Return(0)
}
fn write(env: &mut Env<'_>, pointer: u32, mut bytes: Vec<u8>, wide: bool) -> Result<(), u32> {
    bytes.resize(bytes.len() + if wide { 2 } else { 1 }, 0);
    if pointer == 0 || !env.cpu.accessible(pointer, bytes.len() as u32, Prot::WRITE) {
        return Err(998);
    }
    env.cpu.write(pointer, &bytes).map_err(|_| 998)
}

fn get(env: &mut Env<'_>, wide: bool) -> Outcome {
    let (name, output, capacity) = (env.arg(0), env.arg(1), env.arg(2));
    let name = match read(env, name, wide) {
        Ok(name) => name,
        Err(code) => return error(env, code),
    };
    initialize(env);
    let Some(variable) = env.ext.get::<Environment>().variables.get(&fold(&name)) else {
        return error(env, 203);
    };
    let bytes = encoded(env, &variable.value, wide);
    let len = bytes.len() as u32 / if wide { 2 } else { 1 };
    if capacity <= len {
        return Outcome::Return(len + 1);
    }
    if let Err(code) = write(env, output, bytes, wide) {
        return error(env, code);
    }
    Outcome::Return(len)
}

fn set(env: &mut Env<'_>, wide: bool) -> Outcome {
    let name = match read(env, env.arg(0), wide) {
        Ok(name) if !name.is_empty() && !name.contains(&u16::from(b'=')) => name,
        Ok(_) => return error(env, 87),
        Err(code) => return error(env, code),
    };
    let value = if env.arg(1) == 0 {
        None
    } else {
        match read(env, env.arg(1), wide) {
            Ok(value) => Some(value),
            Err(code) => return error(env, code),
        }
    };
    initialize(env);
    let key = fold(&name);
    let state = env.ext.get_mut::<Environment>();
    let old = state
        .variables
        .get(&key)
        .map_or(0, |v| v.name.len() + v.value.len() + 2);
    let new = value
        .as_ref()
        .map_or(0, |value| name.len() + value.len() + 2);
    if state.units - old + new > MAX_ENV_UNITS {
        return error(env, 8);
    }
    state.units = state.units - old + new;
    if let Some(value) = value {
        state.variables.insert(key, Variable { name, value });
    } else {
        state.variables.remove(&key);
    }
    Outcome::Return(1)
}

fn expand(env: &mut Env<'_>, wide: bool) -> Outcome {
    let (input, output, capacity) = (env.arg(0), env.arg(1), env.arg(2));
    let input = match read(env, input, wide) {
        Ok(input) => input,
        Err(code) => return error(env, code),
    };
    initialize(env);
    let state = env.ext.get::<Environment>();
    let mut expanded = Vec::new();
    let mut at = 0;
    while at < input.len() {
        let end = if input[at] == u16::from(b'%') {
            input[at + 1..]
                .iter()
                .position(|&unit| unit == u16::from(b'%'))
        } else {
            None
        };
        if let Some(end) = end {
            let end = at + 1 + end;
            if let Some(value) = state.variables.get(&fold(&input[at + 1..end])) {
                expanded.extend_from_slice(&value.value);
            } else {
                expanded.extend_from_slice(&input[at..=end]);
            }
            at = end + 1;
        } else {
            expanded.push(input[at]);
            at += 1;
        }
        if expanded.len() > MAX_ENV_UNITS {
            return error(env, 8);
        }
    }
    let bytes = encoded(env, &expanded, wide);
    let required = bytes.len() as u32 / if wide { 2 } else { 1 } + 1;
    // The ANSI wrapper reserves one extra conversion byte on its sizing path.
    // On success its return is the actual byte length including the NUL.
    let minimum = required + u32::from(!wide);
    if output != 0 && capacity >= minimum {
        if let Err(code) = write(env, output, bytes, wide) {
            return error(env, code);
        }
        return Outcome::Return(required);
    }
    Outcome::Return(minimum)
}

fn strings(env: &mut Env<'_>, wide: bool) -> Outcome {
    initialize(env);
    let mut units = Vec::with_capacity(env.ext.get::<Environment>().units + 1);
    for variable in env.ext.get::<Environment>().variables.values() {
        units.extend_from_slice(&variable.name);
        units.push(u16::from(b'='));
        units.extend_from_slice(&variable.value);
        units.push(0);
    }
    units.push(0);
    if units.len() == 1 {
        units.push(0);
    }
    let bytes = encoded(env, &units, wide);
    let state = env.ext.get_mut::<Win32>();
    let Some(pointer) = state
        .heaps
        .alloc(env.cpu, &mut env.sys.vm, 0, bytes.len() as u32, false)
    else {
        return error(env, 8);
    };
    if env.cpu.write(pointer, &bytes).is_err() {
        state.heaps.free(env.cpu, &mut env.sys.vm, 0, pointer);
        return error(env, 998);
    }
    env.ext.get_mut::<Environment>().snapshots.insert(pointer);
    Outcome::Return(pointer)
}
fn free_strings(env: &mut Env<'_>) -> Outcome {
    let pointer = env.arg(0);
    initialize(env);
    if !env.ext.get_mut::<Environment>().snapshots.remove(&pointer) {
        return error(env, 87);
    }
    let state = env.ext.get_mut::<Win32>();
    Outcome::Return(u32::from(state.heaps.free(
        env.cpu,
        &mut env.sys.vm,
        0,
        pointer,
    )))
}

pub(super) fn register(registry: &mut Registry) {
    stdcall(registry, super::K32, "GetEnvironmentVariableA", 3, |e| {
        get(e, false)
    });
    stdcall(registry, super::K32, "GetEnvironmentVariableW", 3, |e| {
        get(e, true)
    });
    stdcall(registry, super::K32, "SetEnvironmentVariableA", 2, |e| {
        set(e, false)
    });
    stdcall(registry, super::K32, "SetEnvironmentVariableW", 2, |e| {
        set(e, true)
    });
    stdcall(registry, super::K32, "ExpandEnvironmentStringsA", 3, |e| {
        expand(e, false)
    });
    stdcall(registry, super::K32, "ExpandEnvironmentStringsW", 3, |e| {
        expand(e, true)
    });
    stdcall(registry, super::K32, "GetEnvironmentStrings", 0, |e| {
        strings(e, false)
    });
    stdcall(registry, super::K32, "GetEnvironmentStringsA", 0, |e| {
        strings(e, false)
    });
    stdcall(registry, super::K32, "GetEnvironmentStringsW", 0, |e| {
        strings(e, true)
    });
    stdcall(
        registry,
        super::K32,
        "FreeEnvironmentStringsA",
        1,
        free_strings,
    );
    stdcall(
        registry,
        super::K32,
        "FreeEnvironmentStringsW",
        1,
        free_strings,
    );
}
