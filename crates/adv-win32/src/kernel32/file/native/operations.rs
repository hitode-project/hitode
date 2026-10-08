//! Common synchronous file operations. Host NT semantic cores are a fast path
//! only while their public native entries have not been changed by the guest.
use super::*;
use adv_abi::{GuestPtr, Handle};

pub(crate) type Done<T> = Box<dyn FnOnce(&mut Env<'_>, Result<T, u32>) -> Outcome>;

fn call(
    env: &mut Env<'_>,
    name: &str,
    args: Vec<u32>,
    fast: impl FnOnce(&mut Env<'_>) -> u32,
    then: impl FnOnce(&mut Env<'_>, u32) -> Outcome + 'static,
) -> Outcome {
    match overridden(env, name) {
        Ok(Some(target)) => Outcome::call(target, args, move |env, value| then(env, value.value())),
        Ok(None) => {
            let status = fast(env);
            then(env, status)
        }
        Err(_) => crate::native::unavailable(env, name),
    }
}

pub(crate) fn flush(env: &mut Env<'_>, handle: u32, then: Done<()>) -> Outcome {
    let scratch = match attributes(env, r"C:\", 8) {
        Ok(scratch) => scratch,
        Err(status) => return then(env, Err(status)),
    };
    let iosb = scratch.extra;
    call(
        env,
        "NtFlushBuffersFile",
        vec![handle, iosb],
        move |env| {
            crate::ntdll::file::metadata::flush(env, Handle(handle), GuestPtr::new(iosb)) as u32
        },
        move |env, status| {
            if status == 0x103 {
                return pending();
            }
            scratch.free(env);
            then(
                env,
                if (status as i32) < 0 {
                    Err(status)
                } else {
                    Ok(())
                },
            )
        },
    )
}

fn pending() -> Outcome {
    Outcome::Fail("native file operation: asynchronous completion is unavailable".into())
}

pub(crate) fn open(
    env: &mut Env<'_>,
    path: &str,
    access: u32,
    share: u32,
    disposition: u32,
    options: u32,
    then: Done<u32>,
) -> Outcome {
    let path = env.ext.get::<Win32>().vfs.normalize(path);
    let attrs = match attributes(env, &path, 16) {
        Ok(attrs) => attrs,
        Err(status) => return then(env, Err(status)),
    };
    let (out, object, iosb) = (attrs.extra, attrs.object, attrs.extra + 8);
    let use_open = disposition == 1
        && matches!(overridden(env, "NtCreateFile"), Ok(None))
        && matches!(overridden(env, "NtOpenFile"), Ok(Some(_)));
    let name = if use_open {
        "NtOpenFile"
    } else {
        "NtCreateFile"
    };
    let args = if use_open {
        vec![out, access, object, iosb, share, options]
    } else {
        vec![
            out,
            access,
            object,
            iosb,
            0,
            0x80,
            share,
            disposition,
            options,
            0,
            0,
        ]
    };
    call(
        env,
        name,
        args,
        move |env| {
            crate::ntdll::file::create(
                env,
                GuestPtr::new(out),
                access,
                GuestPtr::new(object),
                GuestPtr::new(iosb),
                GuestPtr::new(0),
                0x80,
                share,
                disposition,
                options,
                GuestPtr::new(0),
                0,
            ) as u32
        },
        move |env, status| {
            if status == 0x103 {
                return pending();
            }
            let handle = env.cpu.read_u32(out);
            attrs.free(env);
            let result = if (status as i32) < 0 {
                Err(status)
            } else {
                handle.map_err(|_| ACCESS_VIOLATION).and_then(|handle| {
                    if matches!(handle, 0 | u32::MAX) {
                        Err(INVALID_PARAMETER)
                    } else {
                        Ok(handle)
                    }
                })
            };
            then(env, result)
        },
    )
}

pub(crate) fn transfer(
    env: &mut Env<'_>,
    handle: u32,
    buffer: u32,
    length: u32,
    writing: bool,
    then: Done<u32>,
) -> Outcome {
    let permission = if writing { Prot::READ } else { Prot::WRITE };
    if check_range(env.cpu, buffer, length, permission).is_err() {
        return then(env, Err(ACCESS_VIOLATION));
    }
    let scratch = match attributes(env, r"C:\", 8) {
        Ok(scratch) => scratch,
        Err(status) => return then(env, Err(status)),
    };
    let iosb = scratch.extra;
    let name = if writing { "NtWriteFile" } else { "NtReadFile" };
    call(
        env,
        name,
        vec![handle, 0, 0, 0, iosb, buffer, length, 0, 0],
        move |env| {
            let implementation = if writing {
                crate::ntdll::file::write::write
            } else {
                crate::ntdll::file::read
            };
            implementation(
                env,
                Handle(handle),
                Handle(0),
                GuestPtr::new(0),
                GuestPtr::new(0),
                GuestPtr::new(iosb),
                GuestPtr::new(buffer),
                length,
                GuestPtr::new(0),
                GuestPtr::new(0),
            ) as u32
        },
        move |env, status| {
            if status == 0x103 {
                return pending();
            }
            let count = env.cpu.read_u32(iosb + 4);
            scratch.free(env);
            let result = if !writing && status == END_OF_FILE {
                Ok(0)
            } else if (status as i32) < 0 {
                Err(status)
            } else {
                count.map_err(|_| ACCESS_VIOLATION).and_then(|count| {
                    if count <= length {
                        Ok(count)
                    } else {
                        Err(INVALID_PARAMETER)
                    }
                })
            };
            then(env, result)
        },
    )
}

pub(crate) fn query(env: &mut Env<'_>, handle: u32, class: u32, then: Done<u64>) -> Outcome {
    let scratch = match attributes(env, r"C:\", 32) {
        Ok(scratch) => scratch,
        Err(status) => return then(env, Err(status)),
    };
    let iosb = scratch.extra;
    let out = iosb + 8;
    let length = if class == 5 { 24 } else { 8 };
    call(
        env,
        "NtQueryInformationFile",
        vec![handle, iosb, out, length, class],
        move |env| {
            crate::ntdll::file::query(
                env,
                Handle(handle),
                GuestPtr::new(iosb),
                GuestPtr::new(out),
                length,
                class as i32,
            ) as u32
        },
        move |env, status| {
            if status == 0x103 {
                return pending();
            }
            let mut bytes = [0; 8];
            let result = if (status as i32) < 0 {
                Err(status)
            } else {
                env.cpu
                    .read(out + if class == 5 { 8 } else { 0 }, &mut bytes)
                    .map(|()| u64::from_le_bytes(bytes))
                    .map_err(|_| ACCESS_VIOLATION)
            };
            scratch.free(env);
            then(env, result)
        },
    )
}

pub(crate) fn seek(
    env: &mut Env<'_>,
    handle: u32,
    distance: i64,
    method: u32,
    then: Done<u64>,
) -> Outcome {
    if method > 2 {
        return then(env, Err(INVALID_PARAMETER));
    }
    let apply: Done<u64> = Box::new(move |env, base| {
        let base = match base {
            Ok(base) => base,
            Err(status) => return then(env, Err(status)),
        };
        let position = i128::from(base) + i128::from(distance);
        if !(0..=i128::from(i64::MAX)).contains(&position) {
            return then(env, Err(INVALID_PARAMETER));
        }
        let scratch = match attributes(env, r"C:\", 16) {
            Ok(scratch) => scratch,
            Err(status) => return then(env, Err(status)),
        };
        let (iosb, input) = (scratch.extra, scratch.extra + 8);
        let position = position as u64;
        if env.cpu.write(input, &position.to_le_bytes()).is_err() {
            scratch.free(env);
            return then(env, Err(ACCESS_VIOLATION));
        }
        call(
            env,
            "NtSetInformationFile",
            vec![handle, iosb, input, 8, 14],
            move |env| {
                crate::ntdll::file::set(
                    env,
                    Handle(handle),
                    GuestPtr::new(iosb),
                    GuestPtr::new(input),
                    8,
                    14,
                ) as u32
            },
            move |env, status| {
                if status == 0x103 {
                    return pending();
                }
                scratch.free(env);
                then(
                    env,
                    if (status as i32) < 0 {
                        Err(status)
                    } else {
                        Ok(position)
                    },
                )
            },
        )
    });
    if method == 0 {
        apply(env, Ok(0))
    } else {
        query(env, handle, if method == 1 { 14 } else { 5 }, apply)
    }
}

pub(crate) fn close(env: &mut Env<'_>, handle: u32, then: Done<()>) -> Outcome {
    call(
        env,
        "NtClose",
        vec![handle],
        move |env| crate::ntdll::file::close(env, Handle(handle)) as u32,
        move |env, status| {
            then(
                env,
                if (status as i32) < 0 {
                    Err(status)
                } else {
                    Ok(())
                },
            )
        },
    )
}

pub(crate) fn information(
    env: &mut Env<'_>,
    handle: u32,
    class: u32,
    length: u32,
    volume: bool,
    then: Done<Vec<u8>>,
) -> Outcome {
    let scratch = match attributes(env, r"C:\", 8 + length) {
        Ok(scratch) => scratch,
        Err(status) => return then(env, Err(status)),
    };
    let (iosb, output) = (scratch.extra, scratch.extra + 8);
    call(
        env,
        if volume {
            "NtQueryVolumeInformationFile"
        } else {
            "NtQueryInformationFile"
        },
        vec![handle, iosb, output, length, class],
        move |env| {
            let implementation = if volume {
                crate::ntdll::file::metadata::volume
            } else {
                crate::ntdll::file::query
            };
            implementation(
                env,
                Handle(handle),
                GuestPtr::new(iosb),
                GuestPtr::new(output),
                length,
                class as i32,
            ) as u32
        },
        move |env, status| {
            if status == 0x103 {
                return pending();
            }
            let mut bytes = vec![0; length as usize];
            let result = if (status as i32) < 0 {
                Err(status)
            } else {
                env.cpu
                    .read(output, &mut bytes)
                    .map(|()| bytes)
                    .map_err(|_| ACCESS_VIOLATION)
            };
            scratch.free(env);
            then(env, result)
        },
    )
}

pub(crate) fn set_information(
    env: &mut Env<'_>,
    handle: u32,
    class: u32,
    bytes: Vec<u8>,
    then: Done<()>,
) -> Outcome {
    let length = bytes.len() as u32;
    let scratch = match attributes(env, r"C:\", 8 + length) {
        Ok(scratch) => scratch,
        Err(status) => return then(env, Err(status)),
    };
    let (iosb, input) = (scratch.extra, scratch.extra + 8);
    if env.cpu.write(input, &bytes).is_err() {
        scratch.free(env);
        return then(env, Err(ACCESS_VIOLATION));
    }
    call(
        env,
        "NtSetInformationFile",
        vec![handle, iosb, input, length, class],
        move |env| {
            crate::ntdll::file::set(
                env,
                Handle(handle),
                GuestPtr::new(iosb),
                GuestPtr::new(input),
                length,
                class as i32,
            ) as u32
        },
        move |env, status| {
            if status == 0x103 {
                return pending();
            }
            scratch.free(env);
            then(
                env,
                if (status as i32) < 0 {
                    Err(status)
                } else {
                    Ok(())
                },
            )
        },
    )
}
