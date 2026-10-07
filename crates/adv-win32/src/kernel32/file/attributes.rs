//! Checked A/W fronts over the VFS's one guest attribute view.
use super::*;
use adv_abi::{AnsiStr, WideStr, win32};

#[cfg(test)]
mod tests;

fn unavailable(env: &mut Env<'_>, api: &str, reason: &str, error: u32) {
    adv_abi::warn_unavailable(env, api, reason);
    set_last_error(env, error);
}
fn path(env: &mut Env<'_>, pointer: u32, wide: bool, api: &str) -> Result<String, u32> {
    if pointer == 0 {
        return Err(87);
    }
    let path = if wide {
        let units = WideStr(adv_abi::GuestPtr::new(pointer))
            .read(env.cpu, 32768)
            .map_err(|_| 998u32)?;
        String::from_utf16(&units).map_err(|_| {
            unavailable(env, api, "file-attribute path contains invalid UTF16", 1113);
            1113u32
        })?
    } else {
        if !matches!(env.ext.get::<Win32>().codec.acp, 932 | 936 | 1252 | 65001) {
            unavailable(
                env,
                api,
                "ANSI file-attribute paths require guest ACP 932/936/1252/65001",
                50,
            );
            return Err(50);
        }
        let bytes = AnsiStr(adv_abi::GuestPtr::new(pointer))
            .read(env.cpu, 32768)
            .map_err(|_| 998u32)?;
        env.ext.get::<Win32>().codec.decode(&bytes)
    };
    if path.is_empty() { Err(123) } else { Ok(path) }
}
fn failed(env: &mut Env<'_>, api: &str, path: &str, error: &std::io::Error) {
    match error.kind() {
        std::io::ErrorKind::Unsupported => unavailable(
            env,
            api,
            "backing-file identity metadata is unavailable",
            50,
        ),
        std::io::ErrorKind::OutOfMemory => unavailable(
            env,
            api,
            "bounded file-attribute metadata budget exhausted (4096 file instances)",
            8,
        ),
        std::io::ErrorKind::NotFound => {
            let vfs = &env.ext.get::<Win32>().vfs;
            let full = vfs.normalize(path);
            let parent = full.trim_end_matches('\\').rsplit_once('\\').map(|(p, _)| {
                if p.len() == 2 {
                    format!("{p}\\")
                } else {
                    p.to_owned()
                }
            });
            let code = if parent.is_some_and(|p| vfs.is_dir(&p)) {
                2
            } else {
                3
            };
            set_last_error(env, code);
        }
        _ => io_error(env, error),
    }
}
fn get(env: &mut Env<'_>, pointer: u32, wide: bool) -> Outcome {
    let api = if wide {
        "kernel32.dll!GetFileAttributesW"
    } else {
        "kernel32.dll!GetFileAttributesA"
    };
    let path = match path(env, pointer, wide, api) {
        Ok(path) => path,
        Err(error) => {
            set_last_error(env, error);
            return Outcome::Return(u32::MAX);
        }
    };
    match file_attributes_path(env, &path, api) {
        Ok(attributes) => Outcome::Return(attributes),
        Err(_) => Outcome::Return(u32::MAX),
    }
}

/// One guest attribute view, also used by CRT access checks without opening files.
pub(crate) fn file_attributes_path(env: &mut Env<'_>, path: &str, api: &str) -> Result<u32, u32> {
    let guest = env.ext.get::<Win32>().vfs.normalize(path);
    if matches!(system_file(env, &guest), Ok(Some(_))) {
        return Ok(FILE_ATTRIBUTE_ARCHIVE | 1);
    }
    match env.ext.get::<Win32>().vfs.file_attributes(path) {
        Ok(attributes) => Ok(attributes),
        Err(error) => {
            failed(env, api, path, &error);
            Err(crate::util::last_error(env))
        }
    }
}
fn set(env: &mut Env<'_>, pointer: u32, flags: u32, wide: bool) -> Outcome {
    let api = if wide {
        "kernel32.dll!SetFileAttributesW"
    } else {
        "kernel32.dll!SetFileAttributesA"
    };
    let path = match path(env, pointer, wide, api) {
        Ok(path) => path,
        Err(error) => {
            set_last_error(env, error);
            return Outcome::Return(0);
        }
    };
    if flags != 0x80 {
        // Stable mask-based reason: no guest path or pointer expands the warning key.
        unavailable(
            env,
            api,
            &format!(
                "requested flags {flags:#010x} are unavailable; supported profile is FILE_ATTRIBUTE_NORMAL (0x80) alone"
            ),
            50,
        );
        return Outcome::Return(0);
    }
    let guest = env.ext.get::<Win32>().vfs.normalize(&path);
    if matches!(system_file(env, &guest), Ok(Some(_))) {
        unavailable(
            env,
            api,
            "synthetic system-file attribute updates are unavailable",
            50,
        );
        return Outcome::Return(0);
    }
    match env.ext.get::<Win32>().vfs.set_file_normal(&path) {
        Ok(()) => Outcome::Return(1),
        Err(error) => {
            failed(env, api, &path, &error);
            Outcome::Return(0)
        }
    }
}
#[win32(kernel32,name="GetFileAttributesA",level=implement)]
fn get_a(env: &mut Env<'_>, name: AnsiStr) -> Outcome {
    get(env, name.0.addr(), false)
}
#[win32(kernel32,name="GetFileAttributesW",level=implement)]
fn get_w(env: &mut Env<'_>, name: WideStr) -> Outcome {
    get(env, name.0.addr(), true)
}
#[win32(kernel32,name="SetFileAttributesA",level=state)]
fn set_a(env: &mut Env<'_>, name: AnsiStr, flags: u32) -> Outcome {
    set(env, name.0.addr(), flags, false)
}
#[win32(kernel32,name="SetFileAttributesW",level=state)]
fn set_w(env: &mut Env<'_>, name: WideStr, flags: u32) -> Outcome {
    set(env, name.0.addr(), flags, true)
}
pub(super) fn register(registry: &mut Registry) {
    registry.register(get_a_ABI);
    registry.register(get_w_ABI);
    registry.register(set_a_ABI);
    registry.register(set_w_ABI);
}
