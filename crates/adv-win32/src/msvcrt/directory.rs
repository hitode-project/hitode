//! Legacy CRT directory fronts over the one checked Kernel32/VFS operation.
use super::*;
use adv_abi::{
    AnsiStr, GuestPtr, WideStr,
    guest::{check_range, memory_failure},
    win32,
};
use adv_cpu::backend::Prot;

const MODULES: [&str; 3] = ["msvcrt.dll", "msvcr80.dll", "msvcr90.dll"];

pub(super) fn register(registry: &mut Registry) {
    for module in MODULES {
        for api in [mkdir_a_ABI, mkdir_w_ABI] {
            registry.register(adv_abi::Api { module, ..api });
        }
        // Microsoft <direct.h>: int __cdecl _[w]rmdir(const [w]char_t *).
        registry.add(module, "_rmdir", Conv::Cdecl { argc: 1 }, |env| {
            directory(env, env.arg(0), false, true)
        });
        registry.add(module, "_wrmdir", Conv::Cdecl { argc: 1 }, |env| {
            directory(env, env.arg(0), true, true)
        });
    }
}
fn api(env: &Env<'_>, wide: bool, removing: bool) -> String {
    format!(
        "{}!{}",
        env.sys.thunks.get(env.thunk).module,
        match (wide, removing) {
            (false, false) => "_mkdir",
            (true, false) => "_wmkdir",
            (false, true) => "_rmdir",
            (true, true) => "_wrmdir",
        }
    )
}
fn errno_pair(env: &mut Env<'_>) -> Result<GuestPtr<i32>, Outcome> {
    let errno = floating::errno_address(env);
    if errno.is_null() {
        return Err(Outcome::Fail(
            "CRT directory errno allocation failed".into(),
        ));
    }
    check_range(env.cpu, errno.addr(), 8, Prot::WRITE).map_err(memory_failure)?;
    Ok(errno)
}
fn fail(env: &mut Env<'_>, errno: GuestPtr<i32>, value: i32, dos: u32) -> Outcome {
    // Both cells are preflighted before any directory creation. Publish the pair
    // in one checked write; failed directory operations cannot report a success.
    let bytes = [value.to_le_bytes(), dos.to_le_bytes()].concat();
    if let Err(error) = env.cpu.write(errno.addr(), &bytes) {
        return memory_failure(error);
    }
    crate::util::set_last_error(env, dos);
    Outcome::Return(u32::MAX)
}
fn directory(env: &mut Env<'_>, pointer: u32, wide: bool, removing: bool) -> Outcome {
    let errno = match errno_pair(env) {
        Ok(errno) => errno,
        Err(outcome) => return outcome,
    };
    if pointer == 0 {
        // The authored VC90 oracle passes NULL through CreateDirectory: -1,
        // ENOENT/DOS PATH_NOT_FOUND, without invoking its invalid handler.
        return fail(env, errno, 2, 3);
    }
    let path = if wide {
        let units = match WideStr(GuestPtr::new(pointer)).read(env.cpu, 32768) {
            Ok(units) => units,
            Err(error) => return memory_failure(error),
        };
        match String::from_utf16(&units) {
            Ok(path) => path,
            Err(_) => {
                adv_abi::warn_unavailable(
                    env,
                    &api(env, wide, removing),
                    "directory path contains unsupported invalid UTF16 units",
                );
                return fail(env, errno, 42, 1113); // EILSEQ / NO_UNICODE_TRANSLATION.
            }
        }
    } else {
        let cp = env.ext.get::<crate::Win32>().codec.acp;
        if !matches!(cp, 932 | 936 | 1252 | 65001) {
            adv_abi::warn_unavailable(
                env,
                &api(env, wide, removing),
                "ANSI directory paths require guest ACP 932/936/1252/65001",
            );
            return fail(env, errno, 40, 50); // ENOSYS / NOT_SUPPORTED.
        }
        let bytes = match AnsiStr(GuestPtr::new(pointer)).read(env.cpu, 32768) {
            Ok(bytes) => bytes,
            Err(error) => return memory_failure(error),
        };
        // These legacy path APIs use the Win32 ANSI file-name code page, not
        // the formatter's LC_CTYPE/C-locale multibyte conversion rules.
        env.ext.get::<crate::Win32>().codec.decode(&bytes)
    };
    let then = Box::new(
        move |env: &mut Env<'_>, result: Result<(), u32>| match result {
            Ok(()) => Outcome::Return(0),
            Err(status) => {
                let dos = crate::kernel32::file::native::directory::error(status);
                let code = if removing && matches!(dos, 145 | 267) {
                    if dos == 145 { 41 } else { 22 }
                } else {
                    errors::dos_errno(dos)
                };
                fail(env, errno, code, dos)
            }
        },
    );
    if removing {
        crate::kernel32::file::native::directory::remove(env, path, then)
    } else {
        crate::kernel32::file::native::directory::create(env, path, then)
    }
}

#[win32(msvcrt,name="_mkdir",level=implement)]
fn mkdir_a(env: &mut Env<'_>, path: AnsiStr) -> Outcome {
    directory(env, path.0.addr(), false, false)
}
#[win32(msvcrt,name="_wmkdir",level=implement)]
fn mkdir_w(env: &mut Env<'_>, path: WideStr) -> Outcome {
    directory(env, path.0.addr(), true, false)
}

#[cfg(test)]
mod tests;
