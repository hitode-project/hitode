//! The observed UCRT binary reader uses the existing FILE, heap and VFS owner.
use super::*;
use adv_abi::{
    GuestPtr, WideStr,
    guest::{check_range, memory_failure},
    win32,
};
use adv_cpu::backend::{MemFault, Prot};
use std::io::Seek;

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

fn io_failure(env: &mut Env<'_>, error: std::io::Error) -> Outcome {
    let dos = shared_file::io_code(&error);
    let value = shared_file::dos_errno(dos);
    let errno = floating::errno_address(env);
    if let Err(error) = check_range(env.cpu, errno.addr(), 8, Prot::WRITE).and_then(|_| {
        env.cpu.write(
            errno.addr(),
            &[value.to_le_bytes(), dos.to_le_bytes()].concat(),
        )
    }) {
        return memory_failure(error);
    }
    crate::util::set_last_error(env, dos);
    Outcome::Return(value as u32)
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
    let options = file_open_options(&mode).expect("validated rb mode");
    let stream = match crt_open_file_options(env, &name, &mode, options) {
        Ok(stream) => stream,
        Err(error) => return io_failure(env, error),
    };
    if let Err(error) = output.write(env.cpu, stream) {
        // Roll back the same FILE/heap owner if a backend output write fails.
        env.ext.get_mut::<CrtState>().files.remove(&stream);
        let heaps = env.ext.get_mut::<Heaps>();
        heaps.free(env.cpu, &mut env.sys.vm, heaps.process_heap, stream);
        return memory_failure(error);
    }
    Outcome::Return(0)
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
    if env
        .ext
        .get_mut::<CrtState>()
        .files
        .get_mut(&stream.addr())
        .expect("validated live FILE")
        .stream_position()
        .is_ok_and(|position| position > i32::MAX as u64)
    {
        return unavailable(env, "FILE byte position exceeds the signed32 ftell profile");
    }
    call(env)
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
