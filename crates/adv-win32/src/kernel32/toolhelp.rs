//! Toolhelp snapshots copy the guest Loader/context owners, never host modules.
use adv_abi::{GuestPtr, Handle, Registry, guest::check_range, win32};
use adv_cpu::backend::Prot;
use adv_machine::{Env, Outcome};

use super::K32;
use crate::util::{err, set_last_error};
use crate::{SnapshotModule, ThreadSnapshot, Win32, stdcall};

const SNAPTHREAD: u32 = 4;
const SNAPMODULE: u32 = 8;
const SNAPMODULE32: u32 = 16;
const MODULE_A_BYTES: usize = 548;
const MODULE_W_BYTES: usize = 1064;
const NAME_UNITS: usize = 256;
const PATH_UNITS: usize = 260;

#[win32(kernel32, name = "CreateToolhelp32Snapshot", level = state)]
fn create_snapshot(env: &mut Env<'_>, flags: u32, pid: u32) -> Outcome {
    if flags & !(SNAPTHREAD | SNAPMODULE | SNAPMODULE32) != 0 {
        adv_abi::warn_unavailable(
            env,
            "kernel32.dll!CreateToolhelp32Snapshot",
            "process, heap, inherited or unknown Toolhelp snapshot categories are unavailable (supported: thread/module/module32 or empty snapshot)",
        );
        set_last_error(env, 50);
        return Outcome::Return(u32::MAX);
    }
    if flags & (SNAPMODULE | SNAPMODULE32) != 0 && pid != 0 && pid != env.sys.pid {
        adv_abi::warn_unavailable(
            env,
            "kernel32.dll!CreateToolhelp32Snapshot",
            "cross-process Toolhelp module snapshots are unavailable; only the current guest process is represented",
        );
        set_last_error(env, 50);
        return Outcome::Return(u32::MAX);
    }
    let rows = if flags & SNAPTHREAD != 0 {
        env.sys
            .contexts
            .iter()
            .filter(|c| !c.is_done())
            .map(|c| (c.tid, env.sys.pid))
            .collect()
    } else {
        Vec::new()
    };
    // This guest is32-bit. Native x86 SNAPMODULE32 alone creates an empty module
    // list; with SNAPMODULE it includes the same32-bit modules exactly once.
    let modules = if flags & SNAPMODULE != 0 {
        let captured = adv_os::modules(env.ext)
            .iter()
            .filter(|(_, m)| m.executable_visible() && m.ldr_entry != 0 && m.lifetime.mapped)
            .map(|(id, m)| {
                let path = super::module::module_path(env, id)?;
                let name = path.rsplit(['\\', '/']).next().unwrap_or(&path).to_owned();
                Ok(SnapshotModule {
                    base: m.base,
                    size: m.size,
                    pid: env.sys.pid,
                    name,
                    path,
                })
            })
            .collect::<Result<Vec<_>, adv_cpu::backend::MemFault>>();
        match captured {
            Ok(rows) => rows,
            Err(_) => {
                adv_abi::warn_unavailable(
                    env,
                    "kernel32.dll!CreateToolhelp32Snapshot",
                    "guest Loader module list contains an unreadable or malformed owned LDR FullDllName; snapshot cannot be frozen",
                );
                set_last_error(env, 24);
                return Outcome::Return(u32::MAX);
            }
        }
    } else {
        Vec::new()
    };
    let handle = env.sys.objects.insert_host(ThreadSnapshot {
        rows,
        cursor: 0,
        modules,
        module_cursor: 0,
    });
    if handle == 0 {
        set_last_error(env, err::NOT_ENOUGH_MEMORY);
        return Outcome::Return(u32::MAX);
    }
    Outcome::Return(handle)
}

fn thread_entry(env: &mut Env<'_>, first: bool) -> Outcome {
    let (handle, out) = (env.arg(0), env.arg(1));
    let size = match GuestPtr::<u32>::new(out).read(env.cpu) {
        Ok(size) => size,
        Err(_) => {
            set_last_error(env, 998);
            return Outcome::Return(0);
        }
    };
    if size < 28 {
        set_last_error(env, 24);
        return Outcome::Return(0);
    }
    if check_range(env.cpu, out, 28, Prot::WRITE).is_err() {
        set_last_error(env, 998);
        return Outcome::Return(0);
    }
    let Some(snapshot) = env.sys.objects.host::<ThreadSnapshot>(handle) else {
        set_last_error(env, err::INVALID_HANDLE);
        return Outcome::Return(0);
    };
    let index = if first { 0 } else { snapshot.cursor };
    let Some(&(tid, pid)) = snapshot.rows.get(index) else {
        set_last_error(env, err::NO_MORE_FILES);
        return Outcome::Return(0);
    };
    let bytes: Vec<_> = [28u32, 0, tid, pid, 8, 0, 0]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect();
    if env.cpu.write(out, &bytes).is_err() {
        set_last_error(env, 998);
        return Outcome::Return(0);
    }
    env.sys
        .objects
        .host_mut::<ThreadSnapshot>(handle)
        .unwrap()
        .cursor = index + 1;
    Outcome::Return(1)
}

fn ansi(env: &Env<'_>, text: &str, capacity: usize) -> Vec<u8> {
    let codec = &env.ext.get::<Win32>().codec;
    let bytes = codec.encode(text);
    let mut end = if codec.acp == 65001 {
        let mut end = bytes.len().min(capacity - 1);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        end
    } else {
        0
    };
    if codec.acp != 65001 {
        while end < bytes.len() {
            let width = if codec.is_lead_byte(0, bytes[end]) && end + 1 < bytes.len() {
                2
            } else {
                1
            };
            if end + width >= capacity {
                break;
            }
            end += width;
        }
    }
    let mut result = bytes[..end].to_vec();
    result.push(0);
    result
}
fn wide(text: &str, capacity: usize) -> Vec<u8> {
    let mut words: Vec<_> = text.encode_utf16().take(capacity - 1).collect();
    if words
        .last()
        .is_some_and(|word| (0xd800..=0xdbff).contains(word))
    {
        words.pop();
    }
    words.push(0);
    words.into_iter().flat_map(u16::to_le_bytes).collect()
}

fn module_entry(
    env: &mut Env<'_>,
    handle: u32,
    out: GuestPtr<u8>,
    first: bool,
    is_wide: bool,
) -> Outcome {
    let size = match out.cast::<u32>().read(env.cpu) {
        Ok(size) => size,
        Err(_) => {
            set_last_error(env, 998);
            return Outcome::Return(0);
        }
    };
    let length = if is_wide {
        MODULE_W_BYTES
    } else {
        MODULE_A_BYTES
    };
    // Native SDK x86 oracle: A accepts >=548 and retains that input dwSize;
    // W accepts exactly1064. A Next's short-size error differs from First.
    if (is_wide && size != length as u32) || (!is_wide && size < length as u32) {
        set_last_error(
            env,
            if is_wide {
                24
            } else if first {
                err::INVALID_PARAMETER
            } else {
                13
            },
        );
        return Outcome::Return(0);
    }
    if check_range(env.cpu, out.addr(), length as u32, Prot::WRITE).is_err() {
        set_last_error(env, 998);
        return Outcome::Return(0);
    }
    let Some(snapshot) = env.sys.objects.host::<ThreadSnapshot>(handle) else {
        set_last_error(env, err::INVALID_HANDLE);
        return Outcome::Return(0);
    };
    let index = if first { 0 } else { snapshot.module_cursor + 1 };
    let row = snapshot.modules.get(index).cloned();
    let Some(row) = row else {
        if first && snapshot.modules.is_empty() {
            // Only proven empty-First outputs are cleared. Native W zeroes its
            // complete body, A only its DWORD fields and the two first NULs.
            // The native A failure wrapper at EOS/invalid handle exposes stale
            // stack data; that undefined payload must never be synthesized.
            let result = if is_wide {
                env.cpu.write(out.addr() + 4, &[0; MODULE_W_BYTES - 4])
            } else {
                env.cpu
                    .write(out.addr() + 4, &[0; 28])
                    .and_then(|_| env.cpu.write(out.addr() + 32, &[0]))
                    .and_then(|_| env.cpu.write(out.addr() + 32 + NAME_UNITS as u32, &[0]))
            };
            if result.is_err() {
                set_last_error(env, 998);
                return Outcome::Return(0);
            }
        }
        set_last_error(env, err::NO_MORE_FILES);
        return Outcome::Return(0);
    };
    if !is_wide && !matches!(env.ext.get::<Win32>().codec.acp, 932 | 936 | 1252 | 65001) {
        let api = if first {
            "kernel32.dll!Module32First"
        } else {
            "kernel32.dll!Module32Next"
        };
        let cp = env.ext.get::<Win32>().codec.acp;
        adv_abi::warn_unavailable(
            env,
            api,
            &format!(
                "Toolhelp ANSI module conversion for code page {cp} is unavailable (supported: 932/936/1252/65001); Unicode enumeration remains available"
            ),
        );
        set_last_error(env, 50);
        return Outcome::Return(0);
    }
    let prefix: Vec<_> = [
        size, 1, row.pid, 0xffff, 0xffff, row.base, row.size, row.base,
    ]
    .into_iter()
    .flat_map(u32::to_le_bytes)
    .collect();
    let result = if is_wide {
        let mut bytes = [0; MODULE_W_BYTES];
        bytes[..32].copy_from_slice(&prefix);
        let name = wide(&row.name, NAME_UNITS);
        let path = wide(&row.path, PATH_UNITS);
        bytes[32..32 + name.len()].copy_from_slice(&name);
        let path_offset = 32 + NAME_UNITS * 2;
        bytes[path_offset..path_offset + path.len()].copy_from_slice(&path);
        env.cpu.write(out.addr(), &bytes)
    } else {
        let name = ansi(env, &row.name, NAME_UNITS);
        let path = ansi(env, &row.path, PATH_UNITS);
        let fields = if first {
            env.cpu.write(out.addr(), &prefix)
        } else {
            // Native ANSI Next preserves the caller's th32ProcessID field;
            // ordinary First->Next therefore retains the original process ID.
            env.cpu
                .write(out.addr(), &prefix[..8])
                .and_then(|_| env.cpu.write(out.addr() + 12, &prefix[12..]))
        };
        fields
            .and_then(|_| env.cpu.write(out.addr() + 32, &name))
            .and_then(|_| env.cpu.write(out.addr() + 32 + NAME_UNITS as u32, &path))
    };
    if result.is_err() {
        set_last_error(env, 998);
        return Outcome::Return(0);
    }
    env.sys
        .objects
        .host_mut::<ThreadSnapshot>(handle)
        .unwrap()
        .module_cursor = index;
    Outcome::Return(1)
}

#[win32(kernel32, name = "Module32First", level = state)]
fn module_first(env: &mut Env<'_>, snapshot: Handle, out: GuestPtr<u8>) -> Outcome {
    module_entry(env, snapshot.0, out, true, false)
}
#[win32(kernel32, name = "Module32Next", level = state)]
fn module_next(env: &mut Env<'_>, snapshot: Handle, out: GuestPtr<u8>) -> Outcome {
    module_entry(env, snapshot.0, out, false, false)
}
#[win32(kernel32, name = "Module32FirstW", level = state)]
fn module_first_w(env: &mut Env<'_>, snapshot: Handle, out: GuestPtr<u8>) -> Outcome {
    module_entry(env, snapshot.0, out, true, true)
}
#[win32(kernel32, name = "Module32NextW", level = state)]
fn module_next_w(env: &mut Env<'_>, snapshot: Handle, out: GuestPtr<u8>) -> Outcome {
    module_entry(env, snapshot.0, out, false, true)
}

pub(super) fn register(reg: &mut Registry) {
    for api in [
        create_snapshot_ABI,
        module_first_ABI,
        module_next_ABI,
        module_first_w_ABI,
        module_next_w_ABI,
    ] {
        reg.register(api);
    }
    stdcall(reg, K32, "Thread32First", 2, |e| thread_entry(e, true));
    stdcall(reg, K32, "Thread32Next", 2, |e| thread_entry(e, false));
}

#[cfg(test)]
mod tests;
