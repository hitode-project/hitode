//! The observed UCRT binary reader uses the existing FILE, heap and VFS owner.
use super::*;
use adv_abi::{
    GuestPtr, WideStr,
    guest::{check_range, memory_failure},
    win32,
};
use adv_cpu::backend::{MemFault, Prot};

pub(super) fn register(registry: &mut Registry) {
    for module in ["ucrtbase.dll", "api-ms-win-crt-stdio-l1-1-0.dll"] {
        for api in [open_ABI, seek_ABI, tell_ABI, read_ABI, close_ABI] {
            registry.register(adv_abi::Api { module, ..api });
        }
    }
}

fn unavailable(env: &mut Env<'_>, reason: &str) -> Outcome {
    let thunk = env.sys.thunks.get(env.thunk);
    let api = format!("{}!{}", thunk.module, thunk.name);
    adv_abi::warn_unavailable(env, &api, reason);
    Outcome::Fail(format!("{api}: {reason}"))
}

fn owned(env: &mut Env<'_>, stream: u32, binary: bool) -> Result<(), Outcome> {
    let file = env.ext.get::<CrtState>().files.get(&stream);
    match file {
        None => Err(unavailable(
            env,
            "stream is not owned by the existing CRT FILE profile",
        )),
        Some(file) if binary && !file.binary => Err(unavailable(
            env,
            "text/Unicode stdio conversion is unavailable; binary FILE required",
        )),
        Some(_) => Ok(()),
    }
}

#[win32(ucrtbase, name = "_wfopen_s", level = implement)]
fn open(env: &mut Env<'_>, output: GuestPtr<u32>, filename: WideStr, mode: WideStr) -> Outcome {
    if output.is_null() {
        return invalid_parameter(env, "_wfopen_s", 22);
    }
    // Validate and clear the complete FILE* output before any file/heap creation.
    if let Err(error) = output.write(env.cpu, 0) {
        return memory_failure(error);
    }
    if filename.0.is_null() || mode.0.is_null() {
        return invalid_parameter(env, "_wfopen_s", 22);
    }
    let (name, mode) = match filename
        .read(env.cpu, 32768)
        .and_then(|name| mode.read(env.cpu, 32).map(|mode| (name, mode)))
    {
        Ok((name, mode)) => match (String::from_utf16(&name), String::from_utf16(&mode)) {
            (Ok(name), Ok(mode)) => (name, mode),
            _ => {
                return unavailable(
                    env,
                    "invalid UTF16 path/mode is outside the binary stdio profile",
                );
            }
        },
        Err(error) => return memory_failure(error),
    };
    if mode != "rb" {
        return unavailable(
            env,
            "only ordinary rb opening is supported by this binary reader profile",
        );
    }
    file::open(
        env,
        &name,
        &mode,
        3,
        Box::new(move |env, result| match result {
            Ok(stream) => match output.write(env.cpu, stream) {
                Ok(()) => Outcome::Return(0),
                Err(error) => file::close(env, stream, Box::new(move |_, _| memory_failure(error))),
            },
            Err(status) => file::error(
                env,
                status,
                shared_file::dos_errno(crate::kernel32::file::native::win32_error(status)) as u32,
            ),
        }),
    )
}

#[win32(ucrtbase, name = "fseek", level = implement)]
fn seek(env: &mut Env<'_>, stream: GuestPtr<u8>, _offset: i32, _origin: i32) -> Outcome {
    if let Err(outcome) = owned(env, stream.addr(), true) {
        return outcome;
    }
    call(env)
}

#[win32(ucrtbase, name = "ftell", level = implement)]
fn tell(env: &mut Env<'_>, stream: GuestPtr<u8>) -> Outcome {
    if let Err(outcome) = owned(env, stream.addr(), true) {
        return outcome;
    }
    file::position(
        env,
        stream.addr(),
        Box::new(|env, result| match result {
            Ok(position) if position <= i32::MAX as u64 => Outcome::Return(position as u32),
            Ok(_) => unavailable(env, "FILE byte position exceeds the signed32 ftell profile"),
            Err(status) => file::error(env, status, u32::MAX),
        }),
    )
}

#[win32(ucrtbase, name = "fread", level = implement)]
fn read(
    env: &mut Env<'_>,
    buffer: GuestPtr<u8>,
    size: u32,
    count: u32,
    stream: GuestPtr<u8>,
) -> Outcome {
    if size == 0 || count == 0 {
        return Outcome::Return(0);
    }
    let Some(total) = size.checked_mul(count) else {
        return memory_failure(MemFault {
            addr: buffer.addr(),
            len: u32::MAX,
        });
    };
    if total > 16 * 1024 * 1024 {
        return unavailable(
            env,
            "read exceeds the existing 16 MiB binary FILE operation profile",
        );
    }
    if let Err(outcome) = owned(env, stream.addr(), true) {
        return outcome;
    }
    if let Err(error) = check_range(env.cpu, buffer.addr(), total, Prot::WRITE) {
        return memory_failure(error);
    }
    call(env)
}

#[win32(ucrtbase, name = "fclose", level = implement)]
fn close(env: &mut Env<'_>, stream: GuestPtr<u8>) -> Outcome {
    if let Err(outcome) = owned(env, stream.addr(), false) {
        return outcome;
    }
    call(env)
}
