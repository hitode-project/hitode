//! Calls from Unix code into ntdll.dll's stdcall exports.
//!
//! Wine builds its Unix libraries with `__stdcall` defined away, so they
//! call every ntdll function as cdecl and pop the arguments themselves.
//! ntdll.dll's system call stubs and `Rtl` functions pop them too. Such an
//! import binds to a trampoline that pushes a copy of the arguments, calls
//! the export and returns to the caller with its own arguments in place.

use std::collections::HashMap;

use adv_machine::Env;
use adv_nt::memory::{self, Memory, PAGE_EXECUTE_READWRITE};
use adv_nt::syscalls::SYSCALLS;

/// ntdll.dll's stdcall exports that are not system calls, with their
/// argument counts, after `ntdll.spec`.
const STDCALL: &[(&str, u8)] = &[
    ("NtCurrentTeb", 0),
    ("NtGetTickCount", 0),
    ("RtlCustomCPToUnicodeN", 6),
    ("RtlDowncaseUnicodeChar", 1),
    ("RtlGetLastWin32Error", 0),
    ("RtlInitCodePageTable", 2),
    ("RtlInitUnicodeString", 2),
    ("RtlNtStatusToDosError", 1),
    ("RtlSetLastWin32Error", 1),
    ("RtlUnicodeToCustomCPN", 6),
    ("RtlUnicodeToUTF8N", 5),
    ("RtlUpcaseUnicodeChar", 1),
    ("RtlUTF8ToUnicodeN", 5),
];

/// ntdll.dll's cdecl exports that Unix code calls as ntdll.so's.
const CDECL: &[&str] = &[
    "__wine_dbg_get_channel_flags",
    "__wine_dbg_header",
    "__wine_dbg_output",
    "__wine_dbg_strdup",
    "wine_server_call",
    "wine_server_handle_to_fd",
];

const PAGE: u32 = 0x1000;

/// The page trampolines are written to and how much of it is used.
#[derive(Default)]
struct Trampolines {
    page: u32,
    used: u32,
    by_target: HashMap<u32, u32>,
}

/// What a cdecl call to ntdll.dll's export `name` at `address` binds to.
pub fn adapt(env: &mut Env<'_>, name: &str, address: u32) -> Result<u32, String> {
    let count = SYSCALLS
        .iter()
        .find(|&&(syscall, _)| syscall == name)
        .map(|&(_, bytes)| bytes / 4)
        .or_else(|| {
            STDCALL
                .iter()
                .find(|&&(stdcall, _)| stdcall == name)
                .map(|&(_, count)| count.into())
        });
    match count {
        Some(0) => Ok(address),
        Some(count) => trampoline(env, address, count),
        None if CDECL.contains(&name) => Ok(address),
        None => Err(format!("ntdll.dll!{name} has no known calling convention")),
    }
}

/// `push dword [esp + 4 * count]`, `count` times, then `call target` and
/// `ret`.
fn trampoline(env: &mut Env<'_>, target: u32, count: u32) -> Result<u32, String> {
    if let Some(&address) = env
        .ext
        .get_or_default::<Trampolines>()
        .by_target
        .get(&target)
    {
        return Ok(address);
    }
    let displacement = u8::try_from(4 * count).map_err(|_| "too many arguments")?;
    let mut code = Vec::new();
    for _ in 0..count {
        code.extend_from_slice(&[0xff, 0x74, 0x24, displacement]);
    }
    let length = code.len() as u32 + 6;

    let trampolines = env.ext.get_or_default::<Trampolines>();
    if trampolines.page == 0 || trampolines.used + length > PAGE {
        let memory = env.ext.get_or_default::<Memory>();
        let page = memory::allocate_committed(env.cpu, &mut env.sys.vm, memory, PAGE)
            .ok_or("no memory for trampolines")?;
        let prot = memory::guest_protection(memory, PAGE_EXECUTE_READWRITE);
        env.cpu
            .protect(page, PAGE, prot)
            .map_err(|e| e.to_string())?;
        memory::set_protections(memory, page, &[PAGE_EXECUTE_READWRITE]);
        let trampolines = env.ext.get_or_default::<Trampolines>();
        trampolines.page = page;
        trampolines.used = 0;
    }
    let trampolines = env.ext.get_or_default::<Trampolines>();
    let address = trampolines.page + trampolines.used;
    let call = address + code.len() as u32;
    code.push(0xe8);
    code.extend_from_slice(&target.wrapping_sub(call + 5).to_le_bytes());
    code.push(0xc3);
    trampolines.used = (trampolines.used + length).next_multiple_of(16);
    trampolines.by_target.insert(target, address);
    env.cpu.write(address, &code).map_err(|e| e.to_string())?;
    Ok(address)
}
