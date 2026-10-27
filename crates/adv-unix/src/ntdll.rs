//! The exports of ntdll.so that ntdll.dll does not share, after
//! `include/wine/unixlib.h` and Wine's ntdll.so. All take the C calling
//! convention.

use std::collections::HashMap;
use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;

use adv_cpu::backend::GuestExt;
use adv_machine::{Conv, CtxId, Env, Outcome};
use adv_nt::file::Files;
use adv_nt::memory::{self, Memory};
use adv_nt::path::{self, Lookup};
use adv_nt::status;
use adv_nt::win32k;

use crate::Function;
use crate::libc::allocate_string;
use crate::loader::Libraries;

const FILE_OPEN: u32 = 1;
/// The longest NT name, in UTF-16 units.
const MAX_NAME: u32 = 0x7fff;

pub fn function(name: &str) -> Option<Function> {
    let cdecl = |argc| Conv::Cdecl { argc };
    Some(match name {
        "KeAddSystemServiceTable" => (cdecl(5), add_system_service_table),
        "KeUserModeCallback" => (cdecl(5), user_mode_callback),
        "ntdll_add_syscall_debug_info" => (cdecl(3), add_syscall_debug_info),
        "ntdll_get_build_dir" => (cdecl(0), build_dir),
        "ntdll_get_data_dir" => (cdecl(0), data_dir),
        "ntdll_get_dos_file_name" => (cdecl(3), dos_file_name),
        "ntdll_get_unix_file_name" => (cdecl(3), unix_file_name),
        "ntdll_set_exception_jmp_buf" => (cdecl(1), set_exception_jmp_buf),
        "ntdll_wcsicmp" => (cdecl(2), wcsicmp),
        "ntdll_wcsnicmp" => (cdecl(3), wcsnicmp),
        "ntdll_wcstoumbs" => (cdecl(5), wcstoumbs),
        _ => return None,
    })
}

/// KeAddSystemServiceTable(funcs, counters, limit, arguments, index)
fn add_system_service_table(env: &mut Env<'_>) -> Outcome {
    let (functions, limit, arguments, index) = (env.arg(0), env.arg(2), env.arg(3), env.arg(4));
    Outcome::Return(win32k::add_table(env, functions, limit, arguments, index).into())
}

/// KeUserModeCallback(id, args, len, ret_ptr, ret_len)
fn user_mode_callback(env: &mut Env<'_>) -> Outcome {
    let (id, args, length, ret_ptr, ret_length) =
        (env.arg(0), env.arg(1), env.arg(2), env.arg(3), env.arg(4));
    win32k::user_mode_callback(env, id, args, length, ret_ptr, ret_length)
}

/// The names behind system call numbers, for Wine's relay traces.
fn add_syscall_debug_info(_: &mut Env<'_>) -> Outcome {
    Outcome::Return(0)
}

/// Wine runs from its installation, not its build tree.
fn build_dir(_: &mut Env<'_>) -> Outcome {
    Outcome::Return(0)
}

/// The NT name of Wine's data directory, `share/wine` of the installation
/// whose Unix libraries the process loads, once made.
struct DataDir(u32);

fn data_dir(env: &mut Env<'_>) -> Outcome {
    if let Some(&DataDir(name)) = env.ext.try_get::<DataDir>() {
        return Outcome::Return(name);
    }
    // <install>/lib/wine/i386-unix/<library>.so
    let libraries = &env.ext.get_or_default::<Libraries>().0;
    let Some(install) = libraries
        .first()
        .and_then(|first| first.path.ancestors().nth(4))
    else {
        return Outcome::Fail("ntdll_get_data_dir: no Unix library is loaded".into());
    };
    let host = install.join("share/wine");
    let name = path::nt_name(&env.ext.get::<Files>().dosdevices, &host);
    let bytes = wide(&name);
    let memory = env.ext.get_or_default::<Memory>();
    let Some(address) =
        memory::allocate_committed(env.cpu, &mut env.sys.vm, memory, bytes.len() as u32)
    else {
        return Outcome::Fail("ntdll_get_data_dir: no memory".into());
    };
    if env.cpu.write(address, &bytes).is_err() {
        return Outcome::Fail("ntdll_get_data_dir: unwritable name".into());
    }
    env.ext.insert(DataDir(address));
    Outcome::Return(address)
}

/// `text` in UTF-16 with a terminating zero.
fn wide(text: &str) -> Vec<u8> {
    text.encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect()
}

/// The NUL-terminated UTF-16 string at `address`.
fn read_wide(env: &Env<'_>, address: u32) -> Option<Vec<u16>> {
    let mut units = Vec::new();
    for index in 0..MAX_NAME {
        match env.cpu.read_u16(address + 2 * index).ok()? {
            0 => return Some(units),
            unit => units.push(unit),
        }
    }
    None
}

/// ntdll_get_unix_file_name(dos, *unix_name, disposition): the host path
/// of a DOS or NT name, in a new heap block. A missing file that
/// `disposition` may create is named with STATUS_NO_SUCH_FILE.
fn unix_file_name(env: &mut Env<'_>) -> Outcome {
    let (dos, result, disposition) = (env.arg(0), env.arg(1), env.arg(2));
    let Some(units) = read_wide(env, dos) else {
        return Outcome::Fail(format!(
            "ntdll_get_unix_file_name: unreadable name at {dos:#x}"
        ));
    };
    let name = String::from_utf16_lossy(&units);
    let nt = if name.starts_with(r"\??\") {
        name
    } else if let Some(rest) = name.strip_prefix(r"\\?\") {
        format!(r"\??\{rest}")
    } else if name.as_bytes().get(1) == Some(&b':') {
        format!(r"\??\{name}")
    } else {
        return Outcome::Return(OBJECT_PATH_SYNTAX_BAD);
    };
    let units: Vec<u16> = nt.encode_utf16().collect();
    let (host, found) = match path::lookup(&env.ext.get::<Files>().dosdevices, &units) {
        Ok(Lookup::Found(host)) => (host, status::SUCCESS),
        Ok(Lookup::Missing(host)) if disposition != FILE_OPEN => (host, status::NO_SUCH_FILE),
        Ok(Lookup::Missing(_)) => return Outcome::Return(status::OBJECT_NAME_NOT_FOUND),
        Ok(Lookup::Device) => return Outcome::Return(status::BAD_DEVICE_TYPE),
        Err(error) => return Outcome::Return(error),
    };
    let bytes = host.into_os_string().into_vec();
    allocate_string(env, bytes, move |env, block| {
        if block == 0 {
            return Outcome::Return(status::NO_MEMORY);
        }
        match env.cpu.write_u32(result, block) {
            Ok(()) => Outcome::Return(found),
            Err(_) => Outcome::Fail("ntdll_get_unix_file_name: unwritable result".into()),
        }
    })
}

const OBJECT_PATH_SYNTAX_BAD: u32 = 0xc000_003b;

/// ntdll_get_dos_file_name(unix_name, *dos, disposition): `C:\…` for a
/// file on a drive, `\\?\unix\…` for one elsewhere, in a new heap block.
fn dos_file_name(env: &mut Env<'_>) -> Outcome {
    let (unix, result, disposition) = (env.arg(0), env.arg(1), env.arg(2));
    let Ok(bytes) = env.cpu.read_cstr(unix, ::libc::PATH_MAX as usize) else {
        return Outcome::Fail(format!(
            "ntdll_get_dos_file_name: unreadable name at {unix:#x}"
        ));
    };
    let host = PathBuf::from(OsString::from_vec(bytes));
    let found = match host.exists() {
        true => status::SUCCESS,
        false if disposition == FILE_OPEN => return Outcome::Return(status::OBJECT_NAME_NOT_FOUND),
        false => status::NO_SUCH_FILE,
    };
    let nt = path::nt_name(&env.ext.get::<Files>().dosdevices, &host);
    let dos = match nt.strip_prefix(r"\??\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => rest.to_owned(),
        _ => format!(r"\\{}", &nt[2..]),
    };
    // Two zero bytes end the UTF-16 string; the block gets the second.
    let mut bytes = wide(&dos);
    bytes.pop();
    allocate_string(env, bytes, move |env, block| {
        if block == 0 {
            return Outcome::Return(status::NO_MEMORY);
        }
        match env.cpu.write_u32(result, block) {
            Ok(()) => Outcome::Return(found),
            Err(_) => Outcome::Fail("ntdll_get_dos_file_name: unwritable result".into()),
        }
    })
}

/// Each thread's buffer that a fault in guarded code jumps back to.
#[derive(Default)]
pub struct JumpBuffers(pub HashMap<CtxId, u32>);

fn set_exception_jmp_buf(env: &mut Env<'_>) -> Outcome {
    let (context, buffer) = (env.context, env.arg(0));
    let buffers = &mut env.ext.get_or_default::<JumpBuffers>().0;
    if buffer == 0 {
        buffers.remove(&context);
    } else {
        buffers.insert(context, buffer);
    }
    Outcome::Return(0)
}

fn upcase(c: u16) -> i32 {
    char::from_u32(c.into())
        .and_then(|c| {
            let mut upper = c.to_uppercase();
            match (upper.next(), upper.next()) {
                (Some(upper), None) if (upper as u32) < 0x10000 => Some(upper as i32),
                _ => None,
            }
        })
        .unwrap_or(c.into())
}

/// Compares up to `n` UTF-16 units without regard to case, as
/// `ntdll_wcsnicmp` does.
fn compare(env: &Env<'_>, mut a: u32, mut b: u32, n: u32) -> Outcome {
    for _ in 0..n {
        let (Ok(x), Ok(y)) = (env.cpu.read_u16(a), env.cpu.read_u16(b)) else {
            return Outcome::Fail(format!("wcsicmp: unreadable string at {a:#x} or {b:#x}"));
        };
        let difference = upcase(x) - upcase(y);
        if difference != 0 || x == 0 {
            return Outcome::Return(difference as u32);
        }
        (a, b) = (a + 2, b + 2);
    }
    Outcome::Return(0)
}

fn wcsicmp(env: &mut Env<'_>) -> Outcome {
    compare(env, env.arg(0), env.arg(1), u32::MAX)
}

fn wcsnicmp(env: &mut Env<'_>) -> Outcome {
    let n = env.arg(2) as i32;
    compare(env, env.arg(0), env.arg(1), n.max(0) as u32)
}

/// ntdll_wcstoumbs(src, srclen, dst, dstlen, strict): UTF-8, the Unix code
/// page here. Returns the bytes written; a string that does not fit is
/// cut at a character. With `strict`, an unpaired surrogate fails with -1.
fn wcstoumbs(env: &mut Env<'_>) -> Outcome {
    let (source, count, destination, capacity, strict) = (
        env.arg(0),
        env.arg(1),
        env.arg(2),
        env.arg(3),
        env.arg(4) != 0,
    );
    let mut units = vec![0; count as usize * 2];
    if env.cpu.read(source, &mut units).is_err() {
        return Outcome::Fail(format!("wcstoumbs: unreadable string at {source:#x}"));
    }
    let units = units
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&unit| u16::from_le_bytes(unit));
    let mut bytes = Vec::new();
    for c in char::decode_utf16(units) {
        let c = match c {
            Ok(c) => c,
            Err(_) if strict => return Outcome::Return(u32::MAX),
            Err(_) => char::REPLACEMENT_CHARACTER,
        };
        let mut buffer = [0; 4];
        let encoded = c.encode_utf8(&mut buffer).as_bytes();
        if bytes.len() + encoded.len() > capacity as usize {
            break;
        }
        bytes.extend_from_slice(encoded);
    }
    match env.cpu.write(destination, &bytes) {
        Ok(()) => Outcome::Return(bytes.len() as u32),
        Err(_) => Outcome::Fail(format!("wcstoumbs: unwritable buffer at {destination:#x}")),
    }
}
