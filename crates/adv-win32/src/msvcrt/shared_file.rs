//! Shared-open fronts use the existing CRT FILE/process-heap owner and VFS.
//! Binary read/write plus SH_DENYNO is the measured profile; other valid modes fail
//! explicitly until the shared stdio owner implements their required semantics.
use super::*;
use adv_abi::{
    AnsiStr, GuestPtr, WideStr,
    guest::{check_range, memory_failure},
    win32,
};
use adv_cpu::backend::Prot;

const MODULES: [&str; 3] = ["msvcrt.dll", "msvcr80.dll", "msvcr90.dll"];
const SH_DENYNO: i32 = 0x40;

pub(super) fn register(registry: &mut Registry) {
    for module in MODULES {
        for api in [open_a_ABI, open_w_ABI] {
            registry.register(adv_abi::Api { module, ..api });
        }
    }
}

fn fail(env: &mut Env<'_>, errno: GuestPtr<i32>, value: i32, dos: u32) -> Outcome {
    let bytes = [value.to_le_bytes(), dos.to_le_bytes()].concat();
    if let Err(error) = env.cpu.write(errno.addr(), &bytes) {
        return memory_failure(error);
    }
    crate::util::set_last_error(env, dos);
    Outcome::Return(0)
}

fn unsupported(env: &mut Env<'_>, errno: GuestPtr<i32>, reason: &str) -> Outcome {
    let thunk = env.sys.thunks.get(env.thunk);
    let api = format!("{}!{}", thunk.module, thunk.name);
    adv_abi::warn_unavailable(env, &api, reason);
    fail(env, errno, 40, 50) // VC ENOSYS / ERROR_NOT_SUPPORTED, no invalid handler.
}

fn invalid(env: &mut Env<'_>, clear_dos: bool) -> Outcome {
    if clear_dos {
        let errno = floating::errno_address(env);
        if let Err(error) = GuestPtr::<u32>::new(errno.addr() + 4).write(env.cpu, 0) {
            return memory_failure(error);
        }
    }
    crate::util::set_last_error(env, 0);
    let name = env.sys.thunks.get(env.thunk).name.clone();
    invalid_parameter_result(env, &name, 22, 0)
}

pub(super) fn io_code(error: &std::io::Error) -> u32 {
    #[cfg(windows)]
    if let Some(code) = error.raw_os_error() {
        return code as u32;
    }
    match error.kind() {
        std::io::ErrorKind::NotFound => 2,
        std::io::ErrorKind::PermissionDenied => 5,
        std::io::ErrorKind::AlreadyExists => 80,
        std::io::ErrorKind::OutOfMemory => 8,
        std::io::ErrorKind::InvalidInput => 87,
        _ => 5,
    }
}

pub(super) fn dos_errno(dos: u32) -> i32 {
    match dos {
        2 | 3 | 15 | 18 | 123 | 206 => 2,
        5 | 16 | 32 | 33 | 65 => 13,
        8 | 14 => 12,
        80 | 183 => 17,
        112 => 28,
        _ => 22,
    }
}

fn open(env: &mut Env<'_>, filename: u32, mode: u32, share: i32, wide: bool) -> Outcome {
    let errno = floating::errno_address(env);
    if errno.is_null() {
        return Outcome::Fail("shared CRT file errno allocation failed".into());
    }
    if let Err(error) = check_range(env.cpu, errno.addr(), 8, Prot::WRITE) {
        return memory_failure(error);
    }
    if filename == 0 || mode == 0 {
        return invalid(env, false);
    }
    let (name, mode) = if wide {
        let read = |pointer, limit| WideStr(GuestPtr::new(pointer)).read(env.cpu, limit);
        let name = match read(filename, 32768) {
            Ok(value) => value,
            Err(error) => return memory_failure(error),
        };
        let mode = match read(mode, 32) {
            Ok(value) => value,
            Err(error) => return memory_failure(error),
        };
        match (String::from_utf16(&name), String::from_utf16(&mode)) {
            (Ok(name), Ok(mode)) => (name, mode),
            _ => {
                return unsupported(
                    env,
                    errno,
                    "shared CRT path/mode contains unsupported invalid UTF16 units",
                );
            }
        }
    } else {
        let cp = env.ext.get::<crate::Win32>().codec.acp;
        if !matches!(cp, 932 | 936 | 1252 | 65001) {
            return unsupported(
                env,
                errno,
                "shared CRT ANSI paths require guest ACP 932/936/1252/65001",
            );
        }
        let name = match AnsiStr(GuestPtr::new(filename)).read(env.cpu, 32768) {
            Ok(value) => value,
            Err(error) => return memory_failure(error),
        };
        let mode = match AnsiStr(GuestPtr::new(mode)).read(env.cpu, 32) {
            Ok(value) => value,
            Err(error) => return memory_failure(error),
        };
        (
            env.ext.get::<crate::Win32>().codec.decode(&name),
            String::from_utf8_lossy(&mode).into_owned(),
        )
    };
    if !mode.starts_with(['r', 'w', 'a']) {
        return invalid(env, false);
    }
    if !matches!(share, 0x10 | 0x20 | 0x30 | 0x40 | 0x80) {
        return invalid(env, true);
    }
    if share != SH_DENYNO {
        return unsupported(
            env,
            errno,
            "shared CRT deny/secure sharing modes are unavailable; only SH_DENYNO is supported",
        );
    }
    if !matches!(mode.as_str(), "rb" | "wb") {
        return unsupported(
            env,
            errno,
            "shared CRT text/update/append modes are unavailable; only binary rb/wb are supported",
        );
    }
    file::open(
        env,
        &name,
        &mode,
        3,
        Box::new(move |env, result| match result {
            Ok(stream) => {
                crate::util::set_last_error(env, 0);
                Outcome::Return(stream)
            }
            Err(status) => {
                let dos = crate::kernel32::file::native::win32_error(status);
                fail(env, errno, dos_errno(dos), dos)
            }
        }),
    )
}

#[win32(msvcrt, name = "_fsopen", level = implement)]
fn open_a(env: &mut Env<'_>, filename: AnsiStr, mode: AnsiStr, share: i32) -> Outcome {
    open(env, filename.0.addr(), mode.0.addr(), share, false)
}

#[win32(msvcrt, name = "_wfsopen", level = implement)]
fn open_w(env: &mut Env<'_>, filename: WideStr, mode: WideStr, share: i32) -> Outcome {
    open(env, filename.0.addr(), mode.0.addr(), share, true)
}

#[cfg(test)]
mod tests;
