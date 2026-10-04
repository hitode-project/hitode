//! ANSI and UTF-16 path fronts over the one process-owned VFS current directory.
use super::*;
use adv_abi::{AnsiStr, GuestPtr, WideStr, guest::check_range, win32};
use adv_cpu::backend::{MemFault, Prot};

fn ansi(env: &Env<'_>, path: AnsiStr) -> Result<String, MemFault> {
    Ok(env
        .ext
        .get::<Win32>()
        .codec
        .decode(&path.read(env.cpu, 32768)?))
}
fn wide(env: &Env<'_>, path: WideStr) -> Result<String, MemFault> {
    Ok(String::from_utf16_lossy(&path.read(env.cpu, 32768)?))
}
fn path_string(
    env: &mut Env<'_>,
    path: &str,
    cap: u32,
    output: u32,
    part: Option<GuestPtr<u32>>,
    wide: bool,
) -> Result<u32, MemFault> {
    let bytes = if wide {
        path.encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>()
    } else {
        let mut b = env.ext.get::<Win32>().codec.encode(path);
        b.push(0);
        b
    };
    let unit = if wide { 2 } else { 1 };
    let required = (bytes.len() / unit) as u32;
    if cap < required {
        return Ok(required);
    }
    check_range(env.cpu, output, bytes.len() as u32, Prot::WRITE)?;
    if let Some(part) = part.filter(|p| !p.is_null()) {
        check_range(env.cpu, part.addr(), 4, Prot::WRITE)?;
    }
    env.cpu.write(output, &bytes)?;
    if let Some(part) = part.filter(|p| !p.is_null()) {
        let pointer = if path.ends_with('\\') {
            0
        } else {
            // CP932's trail byte can be 0x5c. Find the separator in Unicode,
            // then encode the prefix; never search the final ANSI byte string.
            let prefix = path.rfind('\\').map_or("", |index| &path[..index + 1]);
            let length = if wide {
                prefix.encode_utf16().count() * 2
            } else {
                env.ext.get::<Win32>().codec.encode(prefix).len()
            };
            output + length as u32
        };
        part.write(env.cpu, pointer)?;
    }
    Ok(required - 1)
}
fn full(
    env: &mut Env<'_>,
    name: String,
    cap: u32,
    output: u32,
    part: GuestPtr<u32>,
    wide: bool,
) -> Result<u32, MemFault> {
    if name.is_empty() {
        set_last_error(env, 123);
        return Ok(0);
    }
    // Native GetFullPathName's preflight can overestimate before removing
    // dot components. Retain that input-dependent requirement when short.
    let expanded = env.ext.get::<Win32>().vfs.expand(&name);
    let estimate = if wide {
        expanded.encode_utf16().count()
    } else {
        env.ext.get::<Win32>().codec.encode(&expanded).len()
    } as u32
        + 1;
    if wide && cap < estimate {
        return Ok(estimate);
    }
    let full = env.ext.get::<Win32>().vfs.normalize(&name);
    path_string(env, &full, cap, output, Some(part), wide)
}
fn current(env: &mut Env<'_>, cap: u32, output: u32, wide: bool) -> Result<u32, MemFault> {
    let path = env.ext.get::<Win32>().vfs.cwd.clone();
    path_string(env, &path, cap, output, None, wide)
}
fn set_current(env: &mut Env<'_>, path: String) -> i32 {
    let w = env.ext.get_mut::<Win32>();
    let mut path = w.vfs.normalize(&path);
    if !w.vfs.is_dir(&path) {
        set_last_error(env, err::PATH_NOT_FOUND);
        return 0;
    }
    if path.len() > 3 {
        path = path.trim_end_matches('\\').to_owned();
    }
    w.vfs.cwd = path;
    1
}
#[win32(kernel32, name = "GetFullPathNameA", level = implement)]
fn full_a(
    env: &mut Env<'_>,
    name: AnsiStr,
    cap: u32,
    output: AnsiStr,
    part: GuestPtr<u32>,
) -> Result<u32, MemFault> {
    let name = ansi(env, name)?;
    full(env, name, cap, output.0.addr(), part, false)
}
#[win32(kernel32, name = "GetFullPathNameW", level = implement)]
fn full_w(
    env: &mut Env<'_>,
    name: WideStr,
    cap: u32,
    output: WideStr,
    part: GuestPtr<u32>,
) -> Result<u32, MemFault> {
    let name = wide(env, name)?;
    full(env, name, cap, output.0.addr(), part, true)
}
#[win32(kernel32, name = "GetCurrentDirectoryA", level = implement)]
fn current_a(env: &mut Env<'_>, cap: u32, output: AnsiStr) -> Result<u32, MemFault> {
    current(env, cap, output.0.addr(), false)
}
#[win32(kernel32, name = "GetCurrentDirectoryW", level = implement)]
fn current_w(env: &mut Env<'_>, cap: u32, output: WideStr) -> Result<u32, MemFault> {
    current(env, cap, output.0.addr(), true)
}
#[win32(kernel32, name = "SetCurrentDirectoryA", level = implement)]
fn set_a(env: &mut Env<'_>, path: AnsiStr) -> Result<i32, MemFault> {
    let path = ansi(env, path)?;
    Ok(set_current(env, path))
}
#[win32(kernel32, name = "SetCurrentDirectoryW", level = implement)]
fn set_w(env: &mut Env<'_>, path: WideStr) -> Result<i32, MemFault> {
    let path = wide(env, path)?;
    Ok(set_current(env, path))
}
#[win32(kernel32, name = "GetWindowsDirectoryA", level = implement)]
fn windows_a(env: &mut Env<'_>, output: AnsiStr, cap: u32) -> Result<u32, MemFault> {
    path_string(env, "C:\\Windows", cap, output.0.addr(), None, false)
}
#[win32(kernel32, name = "GetWindowsDirectoryW", level = implement)]
fn windows_w(env: &mut Env<'_>, output: WideStr, cap: u32) -> Result<u32, MemFault> {
    path_string(env, "C:\\Windows", cap, output.0.addr(), None, true)
}
#[win32(kernel32, name = "GetSystemDirectoryA", level = implement)]
fn system_a(env: &mut Env<'_>, output: AnsiStr, cap: u32) -> Result<u32, MemFault> {
    path_string(
        env,
        "C:\\Windows\\system32",
        cap,
        output.0.addr(),
        None,
        false,
    )
}
#[win32(kernel32, name = "GetSystemDirectoryW", level = implement)]
fn system_w(env: &mut Env<'_>, output: WideStr, cap: u32) -> Result<u32, MemFault> {
    path_string(
        env,
        "C:\\Windows\\system32",
        cap,
        output.0.addr(),
        None,
        true,
    )
}
fn temp(env: &mut Env<'_>, cap: u32, output: u32, wide: bool) -> Result<u32, MemFault> {
    let value = ["TMP", "TEMP", "USERPROFILE"]
        .into_iter()
        .find_map(|name| super::super::environment::value(env, name));
    let value = value.map_or_else(
        || "C:\\Windows".to_owned(),
        |v| String::from_utf16_lossy(&v),
    );
    // Native W returns its one-unit requirement without touching the caller
    // buffer for an existing empty variable. Native A then converts an
    // uninitialized temporary; reject that unsupported, nondeterministic case.
    if value.is_empty() {
        if wide {
            return Ok(1);
        }
        set_last_error(env, 123);
        return Ok(0);
    }
    let mut path = env.ext.get::<Win32>().vfs.normalize(&value);
    if !path.ends_with('\\') {
        path.push('\\');
    }
    path_string(env, &path, cap, output, None, wide)
}
#[win32(kernel32, name = "GetTempPathA", level = implement)]
fn temp_a(env: &mut Env<'_>, cap: u32, output: AnsiStr) -> Result<u32, MemFault> {
    temp(env, cap, output.0.addr(), false)
}
#[win32(kernel32, name = "GetTempPathW", level = implement)]
fn temp_w(env: &mut Env<'_>, cap: u32, output: WideStr) -> Result<u32, MemFault> {
    temp(env, cap, output.0.addr(), true)
}
#[win32(kernel32, name = "CreateDirectoryA", level = implement)]
fn create_a(
    env: &mut Env<'_>,
    path: AnsiStr,
    _attributes: GuestPtr<u8>,
) -> Result<Outcome, MemFault> {
    let path = ansi(env, path)?;
    Ok(create_directory(env, path))
}
#[win32(kernel32, name = "CreateDirectoryW", level = implement)]
fn create_w(
    env: &mut Env<'_>,
    path: WideStr,
    _attributes: GuestPtr<u8>,
) -> Result<Outcome, MemFault> {
    let path = wide(env, path)?;
    Ok(create_directory(env, path))
}
fn remove(env: &mut Env<'_>, path: String) -> i32 {
    let w = env.ext.get::<Win32>();
    let full = w.vfs.normalize(&path);
    if full
        .trim_end_matches('\\')
        .eq_ignore_ascii_case(w.vfs.cwd.trim_end_matches('\\'))
    {
        set_last_error(env, 32);
        return 0;
    }
    if w.vfs.is_base_only(&full) {
        set_last_error(env, err::ACCESS_DENIED);
        return 0;
    }
    let Some(host) = w.vfs.lookup(&full) else {
        set_last_error(env, err::PATH_NOT_FOUND);
        return 0;
    };
    match std::fs::remove_dir(host) {
        Ok(()) => 1,
        Err(e) => {
            if e.kind() == std::io::ErrorKind::DirectoryNotEmpty {
                set_last_error(env, 145);
            } else {
                io_error(env, &e);
            }
            0
        }
    }
}
#[win32(kernel32, name = "RemoveDirectoryA", level = implement)]
fn remove_a(env: &mut Env<'_>, path: AnsiStr) -> Result<i32, MemFault> {
    let path = ansi(env, path)?;
    Ok(remove(env, path))
}
#[win32(kernel32, name = "RemoveDirectoryW", level = implement)]
fn remove_w(env: &mut Env<'_>, path: WideStr) -> Result<i32, MemFault> {
    let path = wide(env, path)?;
    Ok(remove(env, path))
}
#[win32(kernel32, name = "DeleteFileA", level = implement)]
fn delete_a(env: &mut Env<'_>, path: AnsiStr) -> Result<Outcome, MemFault> {
    let path = ansi(env, path)?;
    Ok(delete_file(env, path))
}
#[win32(kernel32, name = "DeleteFileW", level = implement)]
fn delete_w(env: &mut Env<'_>, path: WideStr) -> Result<Outcome, MemFault> {
    let path = wide(env, path)?;
    Ok(delete_file(env, path))
}
#[win32(kernel32, name = "CopyFileA", level = implement)]
fn copy_a(
    env: &mut Env<'_>,
    source: AnsiStr,
    target: AnsiStr,
    fail: i32,
) -> Result<Outcome, MemFault> {
    let s = ansi(env, source)?;
    let t = ansi(env, target)?;
    Ok(copy_file(env, s, t, fail != 0, false))
}
#[win32(kernel32, name = "CopyFileW", level = implement)]
fn copy_w(
    env: &mut Env<'_>,
    source: WideStr,
    target: WideStr,
    fail: i32,
) -> Result<Outcome, MemFault> {
    let s = wide(env, source)?;
    let t = wide(env, target)?;
    Ok(copy_file(env, s, t, fail != 0, false))
}
#[win32(kernel32, name = "MoveFileA", level = implement)]
fn move_a(env: &mut Env<'_>, source: AnsiStr, target: AnsiStr) -> Result<Outcome, MemFault> {
    let s = ansi(env, source)?;
    let t = ansi(env, target)?;
    Ok(copy_file(env, s, t, true, true))
}
#[win32(kernel32, name = "MoveFileW", level = implement)]
fn move_w(env: &mut Env<'_>, source: WideStr, target: WideStr) -> Result<Outcome, MemFault> {
    let s = wide(env, source)?;
    let t = wide(env, target)?;
    Ok(copy_file(env, s, t, true, true))
}
pub(super) fn register(reg: &mut Registry) {
    for api in [
        full_a_ABI,
        full_w_ABI,
        current_a_ABI,
        current_w_ABI,
        set_a_ABI,
        set_w_ABI,
        windows_a_ABI,
        windows_w_ABI,
        system_a_ABI,
        system_w_ABI,
        temp_a_ABI,
        temp_w_ABI,
        create_a_ABI,
        create_w_ABI,
        remove_a_ABI,
        remove_w_ABI,
        delete_a_ABI,
        delete_w_ABI,
        copy_a_ABI,
        copy_w_ABI,
        move_a_ABI,
        move_w_ABI,
    ] {
        reg.register(api);
    }
}
