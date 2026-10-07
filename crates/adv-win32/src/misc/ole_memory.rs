//! Guest-owned COM task memory and Automation strings. No host COM pointers.
use std::collections::BTreeSet;

use adv_abi::Registry;
use adv_cpu::backend::{GuestExt, Prot};
use adv_machine::{Env, Outcome};
use adv_os::Heaps;
use adv_os::com::{MAX_BSTR_BYTES, TaskMemory, copy_bstr, free_bstr, owns_bstr};

use crate::stdcall;

/// Task allocations are shared with other COM fronts (DirectShow media types).
fn task<'a>(env: &'a mut Env<'_>) -> &'a mut BTreeSet<u32> {
    &mut env.ext.get_or_default::<TaskMemory>().allocations
}
fn allocate(env: &mut Env<'_>, size: u32) -> u32 {
    if size.checked_add(0xffff).is_none() {
        return 0;
    }
    env.ext
        .get_mut::<Heaps>()
        .alloc(env.cpu, &mut env.sys.vm, 0, size, false)
        .unwrap_or(0)
}
fn free(env: &mut Env<'_>, pointer: u32) {
    env.ext
        .get_mut::<Heaps>()
        .free(env.cpu, &mut env.sys.vm, 0, pointer);
}
fn task_alloc(env: &mut Env<'_>) -> Outcome {
    let pointer = allocate(env, env.arg(0));
    if pointer != 0 {
        task(env).insert(pointer);
    }
    Outcome::Return(pointer)
}
fn task_free(env: &mut Env<'_>) -> Outcome {
    let pointer = env.arg(0);
    if pointer != 0 {
        if !task(env).remove(&pointer) {
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
            task(env).insert(pointer);
        }
        return Outcome::Return(pointer);
    }
    if !task(env).contains(&old) {
        return Outcome::Fail(format!(
            "CoTaskMemRealloc: invalid task allocation {old:#x}"
        ));
    }
    if size == 0 {
        task(env).remove(&old);
        free(env, old);
        return Outcome::Return(0);
    }
    if size.checked_add(0xffff).is_none() {
        return Outcome::Return(0);
    }
    let pointer = env
        .ext
        .get_mut::<Heaps>()
        .realloc(env.cpu, &mut env.sys.vm, 0, old, size, false, false)
        .unwrap_or(0);
    if pointer != 0 {
        let memory = task(env);
        memory.remove(&old);
        memory.insert(pointer);
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
    Outcome::Return(bytes.map_or(0, |bytes| copy_bstr(env, source, bytes)))
}
fn string_free(env: &mut Env<'_>) -> Outcome {
    let pointer = env.arg(0);
    if !free_bstr(env, pointer) {
        return Outcome::Fail(format!("SysFreeString: invalid BSTR {pointer:#x}"));
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
    if old != 0 && !owns_bstr(env, old) {
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
        let pointer = copy_bstr(env, source, bytes);
        if pointer == 0 {
            return Outcome::Return(0);
        }
        pointer
    };
    if let Err(error) = env.cpu.write_u32(output, pointer) {
        if pointer != 0 {
            free_bstr(env, pointer);
        }
        return Outcome::Fail(error.to_string());
    }
    if old != 0 {
        free_bstr(env, old);
    }
    Outcome::Return(1)
}

fn format_guid(bytes: &[u8; 16]) -> String {
    let d1 = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    let d2 = u16::from_le_bytes([bytes[4], bytes[5]]);
    let d3 = u16::from_le_bytes([bytes[6], bytes[7]]);
    format!(
        "{{{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
        d1,
        d2,
        d3,
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15]
    )
}

fn parse_guid(s: &str) -> Option<[u8; 16]> {
    let s = s.trim();
    let s = s.strip_prefix('{')?.strip_suffix('}')?;
    let mut parts = s.split('-');
    let p1 = u32::from_str_radix(parts.next()?, 16).ok()?;
    let p2 = u16::from_str_radix(parts.next()?, 16).ok()?;
    let p3 = u16::from_str_radix(parts.next()?, 16).ok()?;
    let p4 = parts.next()?;
    if p4.len() != 4 {
        return None;
    }
    let p4_0 = u8::from_str_radix(&p4[0..2], 16).ok()?;
    let p4_1 = u8::from_str_radix(&p4[2..4], 16).ok()?;
    let p5 = parts.next()?;
    if p5.len() != 12 || parts.next().is_some() {
        return None;
    }
    let mut p4_tail = [0u8; 6];
    for i in 0..6 {
        p4_tail[i] = u8::from_str_radix(&p5[i * 2..i * 2 + 2], 16).ok()?;
    }
    let mut bytes = [0u8; 16];
    bytes[0..4].copy_from_slice(&p1.to_le_bytes());
    bytes[4..6].copy_from_slice(&p2.to_le_bytes());
    bytes[6..8].copy_from_slice(&p3.to_le_bytes());
    bytes[8] = p4_0;
    bytes[9] = p4_1;
    bytes[10..16].copy_from_slice(&p4_tail);
    Some(bytes)
}

fn string_from_clsid(env: &mut Env<'_>) -> Outcome {
    let rclsid = env.arg(0);
    let lplpsz = env.arg(1);
    if lplpsz == 0 {
        return Outcome::Return(0x8000_4003); // E_POINTER
    }
    if rclsid == 0 {
        return Outcome::Return(0x8007_0057); // E_INVALIDARG
    }
    let mut guid_bytes = [0u8; 16];
    if env.cpu.read(rclsid, &mut guid_bytes).is_err() {
        return Outcome::Return(0x8007_0057);
    }
    let guid_str = format_guid(&guid_bytes);
    let utf16: Vec<u8> = guid_str
        .encode_utf16()
        .chain(std::iter::once(0))
        .flat_map(|c| c.to_le_bytes())
        .collect();
    let pointer = allocate(env, utf16.len() as u32);
    if pointer == 0 {
        return Outcome::Return(0x8007_000E); // E_OUTOFMEMORY
    }
    task(env).insert(pointer);
    if env.cpu.write(pointer, &utf16).is_err() {
        task(env).remove(&pointer);
        free(env, pointer);
        return Outcome::Return(0x8007_000E);
    }
    if env.cpu.write_u32(lplpsz, pointer).is_err() {
        task(env).remove(&pointer);
        free(env, pointer);
        return Outcome::Return(0x8000_4003);
    }
    Outcome::Return(0)
}

fn string_from_guid2(env: &mut Env<'_>) -> Outcome {
    let rguid = env.arg(0);
    let lpsz = env.arg(1);
    let cch_max = env.arg(2) as i32;
    if rguid == 0 || lpsz == 0 || cch_max < 39 {
        return Outcome::Return(0);
    }
    let mut guid_bytes = [0u8; 16];
    if env.cpu.read(rguid, &mut guid_bytes).is_err() {
        return Outcome::Return(0);
    }
    let guid_str = format_guid(&guid_bytes);
    let utf16: Vec<u8> = guid_str
        .encode_utf16()
        .chain(std::iter::once(0))
        .flat_map(|c| c.to_le_bytes())
        .collect();
    if env.cpu.write(lpsz, &utf16).is_err() {
        return Outcome::Return(0);
    }
    Outcome::Return(39)
}

pub(super) fn clsid_from_string(env: &mut Env<'_>) -> Outcome {
    let lpsz = env.arg(0);
    let pclsid = env.arg(1);
    if pclsid == 0 {
        return Outcome::Return(0x8000_4003); // E_POINTER
    }
    if lpsz == 0 {
        let _ = env.cpu.write(pclsid, &[0u8; 16]);
        return Outcome::Return(0);
    }
    let mut u16_chars = Vec::new();
    let mut ptr = lpsz;
    loop {
        match env.cpu.read_u16(ptr) {
            Ok(0) => break,
            Ok(c) => {
                u16_chars.push(c);
                ptr += 2;
                if u16_chars.len() > 128 {
                    return Outcome::Return(0x8004_01F3); // CO_E_CLASSSTRING
                }
            }
            Err(_) => return Outcome::Return(0x8000_4003),
        }
    }
    let s = match String::from_utf16(&u16_chars) {
        Ok(s) => s,
        Err(_) => return Outcome::Return(0x8004_01F3),
    };
    if s.is_empty() {
        let _ = env.cpu.write(pclsid, &[0u8; 16]);
        return Outcome::Return(0);
    }
    match parse_guid(&s) {
        Some(guid) => {
            if env.cpu.write(pclsid, &guid).is_err() {
                Outcome::Return(0x8000_4003)
            } else {
                Outcome::Return(0)
            }
        }
        None => Outcome::Return(0x8004_01F3), // CO_E_CLASSSTRING
    }
}

fn co_create_guid(env: &mut Env<'_>) -> Outcome {
    let pguid = env.arg(0);
    if pguid == 0 {
        return Outcome::Return(0x8000_4003);
    }
    use std::time::SystemTime;
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut bytes = [0u8; 16];
    bytes[0..8].copy_from_slice(&(nanos as u64).to_le_bytes());
    bytes[8..16].copy_from_slice(&((nanos >> 64) as u64 ^ 0x0123_4567_89ab_cdef).to_le_bytes());
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    if env.cpu.write(pguid, &bytes).is_err() {
        return Outcome::Return(0x8000_4003);
    }
    Outcome::Return(0)
}

pub(super) fn register(registry: &mut Registry) {
    stdcall(registry, "ole32.dll", "CoTaskMemAlloc", 1, task_alloc);
    stdcall(registry, "ole32.dll", "CoTaskMemRealloc", 2, task_realloc);
    stdcall(registry, "ole32.dll", "CoTaskMemFree", 1, task_free);
    stdcall(
        registry,
        "ole32.dll",
        "StringFromCLSID",
        2,
        string_from_clsid,
    );
    stdcall(
        registry,
        "ole32.dll",
        "StringFromGUID2",
        3,
        string_from_guid2,
    );
    stdcall(
        registry,
        "ole32.dll",
        "CLSIDFromString",
        2,
        clsid_from_string,
    );
    stdcall(registry, "ole32.dll", "IIDFromString", 2, clsid_from_string);
    stdcall(registry, "ole32.dll", "CoCreateGuid", 1, co_create_guid);
    stdcall(
        registry,
        "shell32.dll",
        "SHCLSIDFromString",
        2,
        clsid_from_string,
    );
    for (name, ordinal, argc, handler) in [
        (
            "SysAllocString",
            "#2",
            1,
            (|e: &mut Env<'_>| string_alloc(e, 0)) as adv_machine::Handler,
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
