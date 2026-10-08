//! FILE metadata refers to the original NT/Win32 file object and cursor.
use super::*;
use crate::kernel32::file::native::{operations, win32_error};
use adv_abi::{GuestPtr, WideStr, guest::memory_failure};

pub(super) type OpenDone = operations::Done<u32>;

pub(super) fn error(env: &mut Env<'_>, status: u32, result: u32) -> Outcome {
    let dos = win32_error(status);
    let errno = floating::errno_address(env);
    let value = if dos == 6 {
        9
    } else {
        shared_file::dos_errno(dos)
    };
    if let Err(fault) = env.cpu.write(
        errno.addr(),
        &[value.to_le_bytes(), dos.to_le_bytes()].concat(),
    ) {
        return memory_failure(fault);
    }
    crate::util::set_last_error(env, dos);
    Outcome::Return(result)
}

pub(super) fn open(
    env: &mut Env<'_>,
    name: &str,
    mode: &str,
    share: u32,
    then: OpenDone,
) -> Outcome {
    let first = mode.as_bytes().first().copied();
    let update = mode.contains('+');
    let (access, disposition) = match first {
        Some(b'r') => (0x8010_0080 | if update { 0x4000_0000 } else { 0 }, 1),
        Some(b'w') => (0x4010_0080 | if update { 0x8000_0000 } else { 0 }, 5),
        Some(b'a') => (0x4010_0080 | if update { 0x8000_0000 } else { 0 }, 3),
        _ => return then(env, Err(0xc000_000d)),
    };
    let binary = mode.contains('b') && !mode.contains("ccs=");
    let append = first == Some(b'a');
    if !binary
        && ["NtCreateFile", "NtOpenFile", "NtReadFile", "NtWriteFile"]
            .into_iter()
            .any(|name| !matches!(crate::native::overridden(env, name), Ok(None)))
    {
        adv_abi::warn_unavailable(
            env,
            "CRT stdio text conversion",
            "text/Unicode translation is unavailable over the live native file provider",
        );
    }
    operations::open(
        env,
        name,
        access,
        share,
        disposition,
        0x60,
        Box::new(move |env, result| {
            let handle = match result {
                Ok(handle) => handle,
                Err(status) => return then(env, Err(status)),
            };
            crate::native_heap::allocate_then(
                env,
                32,
                true,
                Box::new(move |env, pointer| {
                    if pointer == 0 {
                        return operations::close(
                            env,
                            handle,
                            Box::new(move |env, _| then(env, Err(0xc000_0017))),
                        );
                    }
                    env.ext.get_or_default::<CrtState>().files.insert(
                        pointer,
                        CrtFile {
                            handle,
                            binary,
                            append,
                            eof: false,
                        },
                    );
                    then(env, Ok(pointer))
                }),
            )
        }),
    )
}

pub(super) fn handle(env: &Env<'_>, stream: u32) -> Option<u32> {
    env.ext
        .try_get::<CrtState>()?
        .files
        .get(&stream)
        .map(|file| file.handle)
}

pub(super) fn close(env: &mut Env<'_>, stream: u32, then: operations::Done<()>) -> Outcome {
    let Some(handle) = handle(env, stream) else {
        return then(env, Err(0xc000_0008));
    };
    operations::close(
        env,
        handle,
        Box::new(move |env, result| {
            env.ext.get_mut::<CrtState>().files.remove(&stream);
            crate::native_heap::free_then(env, stream, Box::new(move |env, _| then(env, result)))
        }),
    )
}

struct Transfer {
    stream: u32,
    handle: u32,
    buffer: u32,
    length: u32,
    writing: bool,
    completed: u32,
    then: operations::Done<u32>,
}

impl Transfer {
    fn next(self, env: &mut Env<'_>) -> Outcome {
        let remaining = self.length - self.completed;
        if remaining == 0 {
            return (self.then)(env, Ok(self.completed));
        }
        let Some(buffer) = self.buffer.checked_add(self.completed) else {
            return (self.then)(env, Err(0xc000_0005));
        };
        operations::transfer(
            env,
            self.handle,
            buffer,
            remaining,
            self.writing,
            Box::new(move |env, result| {
                let mut transfer = self;
                match result {
                    Ok(0) => {
                        if !transfer.writing
                            && let Some(file) = env
                                .ext
                                .get_mut::<CrtState>()
                                .files
                                .get_mut(&transfer.stream)
                        {
                            file.eof = true;
                        }
                        (transfer.then)(env, Ok(transfer.completed))
                    }
                    Ok(count) => {
                        transfer.completed += count;
                        transfer.next(env)
                    }
                    Err(status) if transfer.completed != 0 => match error(env, status, 0) {
                        Outcome::Return(_) => (transfer.then)(env, Ok(transfer.completed)),
                        failure => failure,
                    },
                    Err(status) => (transfer.then)(env, Err(status)),
                }
            }),
        )
    }
}

pub(super) fn transfer(
    env: &mut Env<'_>,
    stream: u32,
    buffer: u32,
    length: u32,
    writing: bool,
    then: operations::Done<u32>,
) -> Outcome {
    let Some(file) = env
        .ext
        .try_get::<CrtState>()
        .and_then(|state| state.files.get(&stream))
    else {
        return then(env, Err(0xc000_0008));
    };
    let (handle, append) = (file.handle, file.append && writing);
    let transfer = Transfer {
        stream,
        handle,
        buffer,
        length,
        writing,
        completed: 0,
        then,
    };
    if append {
        operations::seek(
            env,
            handle,
            0,
            2,
            Box::new(move |env, result| match result {
                Ok(_) => transfer.next(env),
                Err(status) => (transfer.then)(env, Err(status)),
            }),
        )
    } else {
        transfer.next(env)
    }
}

pub(super) fn seek(
    env: &mut Env<'_>,
    stream: u32,
    distance: i64,
    method: u32,
    then: operations::Done<u64>,
) -> Outcome {
    let Some(handle) = handle(env, stream) else {
        return then(env, Err(0xc000_0008));
    };
    operations::seek(
        env,
        handle,
        distance,
        method,
        Box::new(move |env, result| {
            if result.is_ok()
                && let Some(file) = env.ext.get_mut::<CrtState>().files.get_mut(&stream)
            {
                file.eof = false;
            }
            then(env, result)
        }),
    )
}

pub(super) fn position(env: &mut Env<'_>, stream: u32, then: operations::Done<u64>) -> Outcome {
    let Some(handle) = handle(env, stream) else {
        return then(env, Err(0xc000_0008));
    };
    operations::query(env, handle, 14, then)
}

fn open_front(env: &mut Env<'_>, wide: bool, secure: bool) -> Outcome {
    let output = if secure { env.arg(0) } else { 0 };
    let (filename, mode) = (env.arg(u32::from(secure)), env.arg(u32::from(secure) + 1));
    if secure {
        if output == 0 {
            return invalid_parameter(env, "fopen_s", 22);
        }
        if let Err(fault) = GuestPtr::new(output).write(env.cpu, 0u32) {
            return memory_failure(fault);
        }
    }
    if filename == 0 || mode == 0 {
        return if secure {
            invalid_parameter(env, "fopen_s", 22)
        } else {
            Outcome::Return(0)
        };
    }
    let strings = if wide {
        WideStr(GuestPtr::new(filename))
            .read(env.cpu, 32768)
            .and_then(|filename| {
                WideStr(GuestPtr::new(mode)).read(env.cpu, 32).map(|mode| {
                    (
                        String::from_utf16_lossy(&filename),
                        String::from_utf16_lossy(&mode),
                    )
                })
            })
    } else {
        env.cpu.read_cstr(filename, 32768).and_then(|filename| {
            env.cpu.read_cstr(mode, 32).map(|mode| {
                (
                    env.ext.get::<crate::Win32>().codec.decode(&filename),
                    String::from_utf8_lossy(&mode).into_owned(),
                )
            })
        })
    };
    let (filename, mode) = match strings {
        Ok(strings) => strings,
        Err(fault) => return memory_failure(fault),
    };
    open(
        env,
        &filename,
        &mode,
        3,
        Box::new(move |env, result| match result {
            Ok(stream) => {
                if secure {
                    if let Err(fault) = GuestPtr::new(output).write(env.cpu, stream) {
                        return close(env, stream, Box::new(move |_, _| memory_failure(fault)));
                    }
                    Outcome::Return(0)
                } else {
                    Outcome::Return(stream)
                }
            }
            Err(status) => {
                let value = if secure {
                    shared_file::dos_errno(win32_error(status)) as u32
                } else {
                    0
                };
                error(env, status, value)
            }
        }),
    )
}

pub(super) fn front(env: &mut Env<'_>, name: &str) -> Outcome {
    match name {
        "fopen" | "_wfopen" | "fopen_s" | "_wfopen_s" => {
            open_front(env, name.starts_with('_'), name.ends_with("_s"))
        }
        "fread" | "fwrite" => {
            let (buffer, size, count, stream) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3));
            if size == 0 || count == 0 {
                return Outcome::Return(0);
            }
            let Some(total) = size.checked_mul(count) else {
                return error(env, 0xc000_000d, 0);
            };
            transfer(
                env,
                stream,
                buffer,
                total,
                name == "fwrite",
                Box::new(move |env, result| match result {
                    Ok(count) => Outcome::Return(count / size),
                    Err(status) => error(env, status, 0),
                }),
            )
        }
        "fseek" => {
            let (stream, distance, method) = (env.arg(0), i64::from(env.arg(1) as i32), env.arg(2));
            seek(
                env,
                stream,
                distance,
                method,
                Box::new(|env, result| match result {
                    Ok(_) => Outcome::Return(0),
                    Err(status) => error(env, status, u32::MAX),
                }),
            )
        }
        "ftell" => position(
            env,
            env.arg(0),
            Box::new(|env, result| match result {
                Ok(position) => Outcome::Return(position as u32),
                Err(status) => error(env, status, u32::MAX),
            }),
        ),
        "fclose" => close(
            env,
            env.arg(0),
            Box::new(|env, result| match result {
                Ok(()) => Outcome::Return(0),
                Err(status) => error(env, status, u32::MAX),
            }),
        ),
        "feof" => Outcome::Return(
            env.ext
                .try_get::<CrtState>()
                .and_then(|state| state.files.get(&env.arg(0)))
                .is_none_or(|file| file.eof) as u32,
        ),
        "fflush" => {
            // Streams are unbuffered; completed writes already reached their provider.
            Outcome::Return(0)
        }
        _ => unreachable!(),
    }
}

#[cfg(test)]
mod tests;
