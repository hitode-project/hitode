//! Guest-owned COM task memory and Automation strings. No host COM pointers.
use std::collections::BTreeSet;

use pal_cpu::{GuestExt, Prot};
use pal_machine::{Env, Outcome, Registry};

use crate::{Win32, stdcall};

const MAX_BSTR_BYTES: u32 = 16 * 1024 * 1024;
#[derive(Default)]
struct Memory {
    task: BTreeSet<u32>,
    strings: BTreeSet<u32>,
}
fn state<'a>(env: &'a mut Env<'_>) -> &'a mut Memory {
    if env.ext.try_get::<Memory>().is_none() {
        env.ext.insert(Memory::default());
    }
    env.ext.get_mut::<Memory>()
}
fn allocate(env: &mut Env<'_>, size: u32) -> u32 {
    if size.checked_add(0xffff).is_none() {
        return 0;
    }
    env.ext
        .get_mut::<Win32>()
        .heaps
        .alloc(env.cpu, &mut env.sys.vm, 0, size, false)
        .unwrap_or(0)
}
fn free(env: &mut Env<'_>, pointer: u32) {
    env.ext
        .get_mut::<Win32>()
        .heaps
        .free(env.cpu, &mut env.sys.vm, 0, pointer);
}
fn task_alloc(env: &mut Env<'_>) -> Outcome {
    let pointer = allocate(env, env.arg(0));
    if pointer != 0 {
        state(env).task.insert(pointer);
    }
    Outcome::Return(pointer)
}
fn task_free(env: &mut Env<'_>) -> Outcome {
    let pointer = env.arg(0);
    if pointer != 0 {
        if !state(env).task.remove(&pointer) {
            return Outcome::Fail(format!(
                "CoTaskMemFree: invalid task allocation {pointer:#x}"
            ));
        }
        free(env, pointer);
    }
    Outcome::Return(0)
}
fn task_realloc(env: &mut Env<'_>) -> Outcome {
    let (old, size) = (env.arg(0), env.arg(1));
    if old == 0 {
        let pointer = allocate(env, size);
        if pointer != 0 {
            state(env).task.insert(pointer);
        }
        return Outcome::Return(pointer);
    }
    if !state(env).task.contains(&old) {
        return Outcome::Fail(format!(
            "CoTaskMemRealloc: invalid task allocation {old:#x}"
        ));
    }
    if size == 0 {
        state(env).task.remove(&old);
        free(env, old);
        return Outcome::Return(0);
    }
    if size.checked_add(0xffff).is_none() {
        return Outcome::Return(0);
    }
    let pointer = env
        .ext
        .get_mut::<Win32>()
        .heaps
        .realloc(env.cpu, &mut env.sys.vm, 0, old, size, false, false)
        .unwrap_or(0);
    if pointer != 0 {
        let memory = state(env);
        memory.task.remove(&old);
        memory.task.insert(pointer);
    }
    Outcome::Return(pointer)
}

fn text_length(env: &Env<'_>, pointer: u32) -> Option<u32> {
    if pointer == 0 {
        return Some(0);
    }
    for index in 0..=MAX_BSTR_BYTES / 2 {
        if env.cpu.read_u16(pointer.checked_add(index * 2)?).ok()? == 0 {
            return Some(index * 2);
        }
    }
    None
}
fn new_string(env: &mut Env<'_>, source: u32, bytes: u32) -> u32 {
    if bytes > MAX_BSTR_BYTES || (source != 0 && !env.cpu.accessible(source, bytes, Prot::READ)) {
        return 0;
    }
    let base = allocate(env, bytes + 6);
    if base == 0 {
        return 0;
    }
    let pointer = base + 4;
    let mut buffer = [0u8; 4096];
    let result = (|| {
        env.cpu.write_u32(base, bytes).ok()?;
        if source != 0 {
            let mut at = 0;
            while at < bytes {
                let n = (bytes - at).min(buffer.len() as u32);
                env.cpu.read(source + at, &mut buffer[..n as usize]).ok()?;
                env.cpu.write(pointer + at, &buffer[..n as usize]).ok()?;
                at += n;
            }
        }
        env.cpu.write(pointer + bytes, &[0, 0]).ok()
    })();
    if result.is_none() {
        free(env, base);
        return 0;
    }
    state(env).strings.insert(pointer);
    pointer
}
fn string_alloc(env: &mut Env<'_>, form: u8) -> Outcome {
    let source = env.arg(0);
    if form == 0 && source == 0 {
        return Outcome::Return(0);
    }
    let bytes = match form {
        0 => text_length(env, source),
        1 => env.arg(1).checked_mul(2),
        _ => Some(env.arg(1)),
    };
    Outcome::Return(bytes.map_or(0, |bytes| new_string(env, source, bytes)))
}
fn string_free(env: &mut Env<'_>) -> Outcome {
    let pointer = env.arg(0);
    if pointer != 0 {
        if !state(env).strings.remove(&pointer) {
            return Outcome::Fail(format!("SysFreeString: invalid BSTR {pointer:#x}"));
        }
        free(env, pointer - 4);
    }
    Outcome::Return(0)
}
fn string_length(env: &mut Env<'_>, bytes: bool) -> Outcome {
    let pointer = env.arg(0);
    if pointer == 0 {
        return Outcome::Return(0);
    }
    let Some(length) = pointer
        .checked_sub(4)
        .and_then(|p| env.cpu.read_u32(p).ok())
    else {
        return Outcome::Fail(format!("SysStringLen: unreadable BSTR {pointer:#x}"));
    };
    Outcome::Return(if bytes { length } else { length / 2 })
}
fn string_realloc(env: &mut Env<'_>, counted: bool) -> Outcome {
    let (output, source) = (env.arg(0), env.arg(1));
    if !env.cpu.accessible(output, 4, Prot::RW) {
        return Outcome::Return(0);
    }
    let old = env.cpu.read_u32(output).unwrap();
    if old != 0 && !state(env).strings.contains(&old) {
        return Outcome::Fail(format!("SysReAllocString: invalid BSTR {old:#x}"));
    }
    let null = !counted && source == 0;
    let pointer = if null {
        0
    } else {
        let bytes = if counted {
            env.arg(2).checked_mul(2)
        } else {
            text_length(env, source)
        };
        let Some(bytes) = bytes else {
            return Outcome::Return(0);
        };
        // Allocate/copy before retiring old, including source aliases of old.
        let pointer = new_string(env, source, bytes);
        if pointer == 0 {
            return Outcome::Return(0);
        }
        pointer
    };
    if let Err(error) = env.cpu.write_u32(output, pointer) {
        if pointer != 0 {
            state(env).strings.remove(&pointer);
            free(env, pointer - 4);
        }
        return Outcome::Fail(error.to_string());
    }
    if old != 0 {
        state(env).strings.remove(&old);
        free(env, old - 4);
    }
    Outcome::Return(1)
}

pub(super) fn register(registry: &mut Registry) {
    stdcall(registry, "ole32.dll", "CoTaskMemAlloc", 1, task_alloc);
    stdcall(registry, "ole32.dll", "CoTaskMemRealloc", 2, task_realloc);
    stdcall(registry, "ole32.dll", "CoTaskMemFree", 1, task_free);
    for (name, ordinal, argc, handler) in [
        (
            "SysAllocString",
            "#2",
            1,
            (|e: &mut Env<'_>| string_alloc(e, 0)) as pal_machine::Handler,
        ),
        ("SysReAllocString", "#3", 2, |e| string_realloc(e, false)),
        ("SysAllocStringLen", "#4", 2, |e| string_alloc(e, 1)),
        ("SysReAllocStringLen", "#5", 3, |e| string_realloc(e, true)),
        ("SysFreeString", "#6", 1, string_free),
        ("SysStringLen", "#7", 1, |e| string_length(e, false)),
        ("SysAllocStringByteLen", "#150", 2, |e| string_alloc(e, 2)),
        ("SysStringByteLen", "#149", 1, |e| string_length(e, true)),
    ] {
        stdcall(registry, "oleaut32.dll", name, argc, handler);
        stdcall(registry, "oleaut32.dll", ordinal, argc, handler);
    }
}
