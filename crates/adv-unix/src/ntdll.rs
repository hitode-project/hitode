//! The exports of ntdll.so that ntdll.dll does not share, after
//! `include/wine/unixlib.h` and Wine's ntdll.so. All take the C calling
//! convention.

use std::collections::HashMap;

use adv_cpu::backend::GuestExt;
use adv_machine::{Conv, CtxId, Env, Outcome};
use adv_nt::win32k;

use crate::Function;

pub fn function(name: &str) -> Option<Function> {
    let cdecl = |argc| Conv::Cdecl { argc };
    Some(match name {
        "KeAddSystemServiceTable" => (cdecl(5), add_system_service_table),
        "KeUserModeCallback" => (cdecl(5), user_mode_callback),
        "ntdll_add_syscall_debug_info" => (cdecl(3), add_syscall_debug_info),
        "ntdll_get_build_dir" => (cdecl(0), build_dir),
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
