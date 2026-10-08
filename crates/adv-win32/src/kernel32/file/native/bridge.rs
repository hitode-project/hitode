//! Synchronous Win32 file fronts over live guest NT entries. The provider keeps
//! the original handle, bytes and cursor; no shadow file or handle table exists.
use super::*;
use crate::util::{INVALID_HANDLE_VALUE, set_last_error};

type Value = Box<dyn FnOnce(&mut Env<'_>, Result<u64, u32>) -> Outcome>;

fn hook(env: &mut Env<'_>, name: &str) -> Result<Option<u32>, u32> {
    overridden(env, name).map_err(|_| 998)
}
fn failed(env: &mut Env<'_>, error: u32, value: u32) -> Outcome {
    set_last_error(env, error);
    Outcome::Return(value)
}
fn unsupported(env: &mut Env<'_>, api: &str, reason: &str, value: u32) -> Outcome {
    warn_unavailable(env, api, reason);
    failed(env, 50, value)
}
fn pending() -> Outcome {
    // A pending provider may still retain the scratch outputs. Do not free or
    // reuse them before a completion protocol exists.
    Outcome::Fail("native file bridge: asynchronous completion is unavailable".into())
}
struct Scratch(u32);
impl Scratch {
    fn new(env: &mut Env<'_>) -> Result<Self, u32> {
        env.ext
            .get_mut::<Heaps>()
            .alloc(env.cpu, &mut env.sys.vm, 0, 32, true)
            .map(Self)
            .ok_or(8)
    }
    fn free(self, env: &mut Env<'_>) {
        env.ext
            .get_mut::<Heaps>()
            .free(env.cpu, &mut env.sys.vm, 0, self.0);
    }
}

pub(in crate::kernel32) fn create(
    env: &mut Env<'_>,
    path: &str,
    access: u32,
    share: u32,
    inherit: bool,
) -> Option<Outcome> {
    let disposition = env.arg(4);
    let (target, open) = match (hook(env, "NtCreateFile"), hook(env, "NtOpenFile")) {
        (Ok(Some(target)), _) => (target, false),
        (Ok(None), Ok(Some(target))) if disposition == 3 => (target, true),
        (Ok(None), Ok(Some(_))) => match entry(env, "NtCreateFile") {
            Ok(target) => (target, false),
            Err(status) => return Some(failed(env, win32_error(status), INVALID_HANDLE_VALUE)),
        },
        (Ok(None), Ok(None)) => return None,
        _ => return Some(failed(env, 998, INVALID_HANDLE_VALUE)),
    };
    // Keep unsupported native operations explicit instead of silently opening
    // a different physical file behind a virtual provider.
    let flags = env.arg(5);
    let native_disposition = match disposition {
        1 => 2, // CREATE_NEW -> FILE_CREATE
        2 => 5, // CREATE_ALWAYS -> FILE_OVERWRITE_IF
        3 => 1, // OPEN_EXISTING -> FILE_OPEN
        4 => 3, // OPEN_ALWAYS -> FILE_OPEN_IF
        5 => 4, // TRUNCATE_EXISTING -> FILE_OVERWRITE
        _ => return Some(failed(env, 87, INVALID_HANDLE_VALUE)),
    };
    // DELETE is forwarded unchanged; NtCreateFile owns its existing-file rule.
    if inherit
        || access & !0xc013_019f != 0
        || access & 0xc001_0007 == 0
        || flags & 0xe7ff_0000 != 0
        || env.arg(6) != 0
    {
        return Some(unsupported(
            env,
            "kernel32.dll!CreateFile",
            "live NT file providers require non-inheritable synchronous ordinary file access",
            INVALID_HANDLE_VALUE,
        ));
    }
    if disposition == 5 && access & 0x4000_0002 == 0 {
        return Some(failed(env, 87, INVALID_HANDLE_VALUE));
    }
    if share & !7 != 0 {
        return Some(failed(env, 87, INVALID_HANDLE_VALUE));
    }
    let path = env.ext.get::<Win32>().vfs.normalize(path);
    let attrs = match attributes(env, &path, 16) {
        Ok(attrs) => attrs,
        Err(status) => return Some(failed(env, win32_error(status), INVALID_HANDLE_VALUE)),
    };
    let options = 0x60
        | if flags & 0x1000_0000 != 0 { 0x800 } else { 0 }
        | if flags & 0x0800_0000 != 0 { 4 } else { 0 };
    let args = if open {
        vec![
            attrs.extra,
            access | 0x100080,
            attrs.object,
            attrs.extra + 8,
            share,
            options,
        ]
    } else {
        vec![
            attrs.extra,
            access | 0x100080,
            attrs.object,
            attrs.extra + 8,
            0,
            flags & 0xffff,
            share,
            native_disposition,
            options,
            0,
            0,
        ]
    };
    Some(Outcome::call(target, args, move |env, resumed| {
        let status = resumed.value();
        if status == 0x103 {
            return pending();
        }
        let handle = env.cpu.read_u32(attrs.extra);
        let action = env.cpu.read_u32(attrs.extra + 12);
        attrs.free(env);
        if (status as i32) < 0 {
            return failed(env, win32_error(status), INVALID_HANDLE_VALUE);
        }
        match handle {
            Ok(handle) if handle != 0 && handle != INVALID_HANDLE_VALUE => {
                crate::kernel32::handle::set_inherit(env, handle, inherit);
                let already_existed = matches!(disposition, 2 | 4) && matches!(action, Ok(1 | 3)); // FILE_OPENED / FILE_OVERWRITTEN
                set_last_error(env, if already_existed { 183 } else { 0 });
                Outcome::Return(handle)
            }
            _ => failed(env, 998, INVALID_HANDLE_VALUE),
        }
    }))
}

pub(in crate::kernel32) fn read(env: &mut Env<'_>) -> Option<Outcome> {
    transfer(env, false)
}

pub(in crate::kernel32) fn write(env: &mut Env<'_>) -> Option<Outcome> {
    transfer(env, true)
}

fn transfer(env: &mut Env<'_>, writing: bool) -> Option<Outcome> {
    let target = match hook(env, if writing { "NtWriteFile" } else { "NtReadFile" }) {
        Ok(Some(target)) => target,
        Ok(None) => return None,
        Err(error) => return Some(failed(env, error, 0)),
    };
    let (handle, buffer, length, output, overlapped) =
        (env.arg(0), env.arg(1), env.arg(2), env.arg(3), env.arg(4));
    if overlapped != 0 {
        return Some(unsupported(
            env,
            if writing {
                "kernel32.dll!WriteFile"
            } else {
                "kernel32.dll!ReadFile"
            },
            "live NT file providers require synchronous transfers",
            0,
        ));
    }
    let permission = if writing { Prot::READ } else { Prot::WRITE };
    if check_range(env.cpu, buffer, length, permission).is_err()
        || output != 0
            && (check_range(env.cpu, output, 4, Prot::WRITE).is_err()
                || env.cpu.write_u32(output, 0).is_err())
    {
        return Some(failed(env, 998, 0));
    }
    let scratch = match Scratch::new(env) {
        Ok(scratch) => scratch,
        Err(error) => return Some(failed(env, error, 0)),
    };
    let previous = crate::util::last_error(env);
    Some(Outcome::call(
        target,
        [handle, 0, 0, 0, scratch.0, buffer, length, 0, 0],
        move |env, resumed| {
            let status = resumed.value();
            if status == 0x103 {
                return pending();
            }
            let eof = !writing && status == END_OF_FILE;
            let count = if eof {
                Ok(0)
            } else {
                env.cpu.read_u32(scratch.0 + 4).map_err(|_| 998)
            };
            scratch.free(env);
            if (status as i32) < 0 && !eof {
                return failed(env, win32_error(status), 0);
            }
            let count = match count {
                Ok(count) if count <= length => count,
                Ok(_) => return failed(env, 87, 0),
                Err(error) => return failed(env, error, 0),
            };
            if output != 0
                && (check_range(env.cpu, output, 4, Prot::WRITE).is_err()
                    || env.cpu.write_u32(output, count).is_err())
            {
                return failed(env, 998, 0);
            }
            set_last_error(env, previous);
            Outcome::Return(1)
        },
    ))
}

fn query(env: &mut Env<'_>, handle: u32, class: u32, then: Value) -> Outcome {
    let target = match entry(env, "NtQueryInformationFile") {
        Ok(target) => target,
        Err(status) => return then(env, Err(win32_error(status))),
    };
    let scratch = match Scratch::new(env) {
        Ok(scratch) => scratch,
        Err(error) => return then(env, Err(error)),
    };
    let length = if class == 5 { 24 } else { 8 };
    let offset = if class == 5 { 16 } else { 8 };
    Outcome::call(
        target,
        [handle, scratch.0, scratch.0 + 8, length, class],
        move |env, resumed| {
            let status = resumed.value();
            if status == 0x103 {
                return pending();
            }
            let result = if (status as i32) < 0 {
                Err(win32_error(status))
            } else {
                let mut bytes = [0; 8];
                // Native Win32 fronts consume the fixed structure on NT_SUCCESS;
                // they do not require IO_STATUS_BLOCK.Information to be filled
                // by an interposed provider for a size/position query.
                env.cpu
                    .read(scratch.0 + offset, &mut bytes)
                    .map(|()| u64::from_le_bytes(bytes))
                    .map_err(|_| 998)
            };
            scratch.free(env);
            then(env, result)
        },
    )
}

pub(in crate::kernel32) fn size(env: &mut Env<'_>, handle: u32, then: Value) -> Option<Outcome> {
    match hook(env, "NtQueryInformationFile") {
        Ok(Some(_)) => Some(query(env, handle, 5, then)),
        Ok(None) => None,
        Err(error) => Some(then(env, Err(error))),
    }
}

pub(in crate::kernel32) fn seek(
    env: &mut Env<'_>,
    handle: u32,
    distance: i64,
    method: u32,
    then: Value,
) -> Option<Outcome> {
    match (
        hook(env, "NtSetInformationFile"),
        hook(env, "NtQueryInformationFile"),
    ) {
        (Ok(None), Ok(None)) => return None,
        (Err(error), _) | (_, Err(error)) => return Some(then(env, Err(error))),
        _ => {}
    }
    if method > 2 {
        return Some(then(env, Err(87)));
    }
    let apply: Value = Box::new(move |env, base| {
        let base = match base {
            Ok(base) => base,
            Err(error) => return then(env, Err(error)),
        };
        let position = i128::from(base) + i128::from(distance);
        if position < 0 {
            return then(env, Err(131));
        }
        if position > i128::from(i64::MAX) {
            return then(env, Err(87));
        }
        let position = position as u64;
        let target = match entry(env, "NtSetInformationFile") {
            Ok(target) => target,
            Err(status) => return then(env, Err(win32_error(status))),
        };
        let scratch = match Scratch::new(env) {
            Ok(scratch) => scratch,
            Err(error) => return then(env, Err(error)),
        };
        if env
            .cpu
            .write(scratch.0 + 8, &position.to_le_bytes())
            .is_err()
        {
            scratch.free(env);
            return then(env, Err(998));
        }
        Outcome::call(
            target,
            [handle, scratch.0, scratch.0 + 8, 8, 14],
            move |env, resumed| {
                let status = resumed.value();
                if status == 0x103 {
                    return pending();
                }
                scratch.free(env);
                then(
                    env,
                    if (status as i32) < 0 {
                        Err(win32_error(status))
                    } else {
                        Ok(position)
                    },
                )
            },
        )
    });
    Some(if method == 0 {
        apply(env, Ok(0))
    } else {
        query(env, handle, if method == 1 { 14 } else { 5 }, apply)
    })
}

pub(in crate::kernel32) fn close(env: &mut Env<'_>, handle: u32) -> Option<Outcome> {
    let target = match hook(env, "NtClose") {
        Ok(Some(target)) => target,
        Ok(None) => return None,
        Err(error) => return Some(failed(env, error, 0)),
    };
    let previous = crate::util::last_error(env);
    Some(Outcome::call(target, [handle], move |env, resumed| {
        let status = resumed.value();
        if (status as i32) < 0 {
            return failed(env, win32_error(status), 0);
        }
        set_last_error(env, previous);
        Outcome::Return(1)
    }))
}

#[cfg(test)]
mod tests;
