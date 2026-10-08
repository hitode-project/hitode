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
    }
}
fn api(env: &Env<'_>, wide: bool) -> String {
    format!(
        "{}!{}",
        env.sys.thunks.get(env.thunk).module,
        if wide { "_wmkdir" } else { "_mkdir" }
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
fn dos_errno(code: u32) -> i32 {
    match code {
        2 | 3 | 15 | 18 | 123 | 206 => 2, // ENOENT
        5 | 16 | 32 | 33 | 65 => 13,      // EACCES
        8 | 14 => 12,                     // ENOMEM
        80 | 183 => 17,                   // EEXIST
        112 => 28,                        // ENOSPC
        267 => 20,                        // ENOTDIR
        _ => 22,                          // EINVAL
    }
}
fn mkdir(env: &mut Env<'_>, pointer: u32, wide: bool) -> Outcome {
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
                    &api(env, wide),
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
                &api(env, wide),
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
    match crate::kernel32::create_directory_path(env, &path) {
        Ok(()) => {
            crate::native::diagnose_bypass(env, &api(env, wide), &["NtCreateFile"]);
            Outcome::Return(0)
        } // Preserve existing errno/DOS errno.
        Err(dos) => fail(env, errno, dos_errno(dos), dos),
    }
}
#[win32(msvcrt,name="_mkdir",level=implement)]
fn mkdir_a(env: &mut Env<'_>, path: AnsiStr) -> Outcome {
    mkdir(env, path.0.addr(), false)
}
#[win32(msvcrt,name="_wmkdir",level=implement)]
fn mkdir_w(env: &mut Env<'_>, path: WideStr) -> Outcome {
    mkdir(env, path.0.addr(), true)
}

#[cfg(test)]
mod tests;
