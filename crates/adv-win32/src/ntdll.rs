//! Minimal ntdll RTL functions for registry paths and unicode strings.
mod path;
mod file;

use adv_abi::Registry;
use adv_cpu::backend::GuestExt;
use adv_machine::{Env, Outcome};
use adv_os::Heaps;

use crate::stdcall;

const NTDLL: &str = "ntdll.dll";
const STATUS_SUCCESS: u32 = 0;

/// RtlFormatCurrentUserKeyPath(PUNICODE_STRING CurrentUserKeyPath) -> NTSTATUS
fn rtl_format_current_user_key_path(env: &mut Env<'_>) -> Outcome {
    let out = env.arg(0);
    if out == 0 {
        return Outcome::Return(0xc000_000d); // STATUS_INVALID_PARAMETER
    }
    // Path: \REGISTRY\USER\S-1-5-21-0-0-0-1000
    const PATH: &str = r"\REGISTRY\USER\S-1-5-21-0-0-0-1000";
    let units: Vec<u16> = PATH.encode_utf16().chain([0]).collect();
    let byte_len = ((units.len() - 1) * 2) as u16;
    let max_byte_len = (units.len() * 2) as u16;

    let heap = env.ext.get::<Heaps>().process_heap;
    let alloc = match env.ext.get_mut::<Heaps>().alloc(
        env.cpu,
        &mut env.sys.vm,
        heap,
        max_byte_len as u32,
        false,
    ) {
        Some(addr) => addr,
        None => return Outcome::Return(0xc000_0017), // STATUS_NO_MEMORY
    };

    crate::util::write_units(env.cpu, alloc, &units);
    let _ = env.cpu.write(out, &byte_len.to_le_bytes());
    let _ = env.cpu.write(out + 2, &max_byte_len.to_le_bytes());
    let _ = env.cpu.write_u32(out + 4, alloc);
    Outcome::Return(STATUS_SUCCESS)
}

/// RtlFreeUnicodeString(PUNICODE_STRING UnicodeString) -> VOID
fn rtl_free_unicode_string(env: &mut Env<'_>) -> Outcome {
    let p = env.arg(0);
    if p != 0 {
        if let Ok(buf) = env.cpu.read_u32(p + 4) {
            if buf != 0 {
                let heap = env.ext.get::<Heaps>().process_heap;
                let _ = env.ext.get_mut::<Heaps>().free(env.cpu, &mut env.sys.vm, heap, buf);
                let _ = env.cpu.write_u32(p + 4, 0);
            }
        }
        let _ = env.cpu.write(p, &0u16.to_le_bytes());
        let _ = env.cpu.write(p + 2, &0u16.to_le_bytes());
    }
    Outcome::Return(0)
}

/// RtlInitUnicodeString(PUNICODE_STRING DestinationString, PCWSTR SourceString) -> VOID
fn rtl_init_unicode_string(env: &mut Env<'_>) -> Outcome {
    let (dst, src) = (env.arg(0), env.arg(1));
    if dst != 0 {
        if src == 0 {
            let _ = env.cpu.write(dst, &0u16.to_le_bytes());
            let _ = env.cpu.write(dst + 2, &0u16.to_le_bytes());
            let _ = env.cpu.write_u32(dst + 4, 0);
        } else {
            let s = crate::util::wstr(env.cpu, src);
            let byte_len = (s.len() * 2) as u16;
            let max_byte_len = ((s.len() + 1) * 2) as u16;
            let _ = env.cpu.write(dst, &byte_len.to_le_bytes());
            let _ = env.cpu.write(dst + 2, &max_byte_len.to_le_bytes());
            let _ = env.cpu.write_u32(dst + 4, src);
        }
    }
    Outcome::Return(0)
}

pub fn register(reg: &mut Registry) {
    path::register(reg);
    file::register(reg);
    stdcall(reg, NTDLL, "RtlFormatCurrentUserKeyPath", 1, rtl_format_current_user_key_path);
    stdcall(reg, NTDLL, "RtlFreeUnicodeString", 1, rtl_free_unicode_string);
    stdcall(reg, NTDLL, "RtlInitUnicodeString", 2, rtl_init_unicode_string);
}
