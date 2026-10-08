//! The existing CRT FILE owner stores byte positions as legacy signed fpos_t.
//! No second stream table, text conversion state, or host CRT is introduced.
use super::*;
use adv_abi::{
    GuestPtr,
    guest::{check_range, memory_failure},
    win32,
};
use adv_cpu::backend::Prot;

const MODULES: [&str; 3] = ["msvcrt.dll", "msvcr80.dll", "msvcr90.dll"];

pub(super) fn register(registry: &mut Registry) {
    for module in MODULES {
        for api in [get_ABI, set_ABI] {
            registry.register(adv_abi::Api { module, ..api });
        }
    }
}

fn error(env: &mut Env<'_>, errno: i32, dos: Option<u32>) -> Outcome {
    let address = floating::errno_address(env);
    let result = if let Some(dos) = dos {
        let bytes = [errno.to_le_bytes(), dos.to_le_bytes()].concat();
        env.cpu.write(address.addr(), &bytes)
    } else {
        address.write(env.cpu, errno)
    };
    match result {
        Ok(()) => {
            if let Some(dos) = dos {
                crate::util::set_last_error(env, dos);
            }
            Outcome::Return(u32::MAX)
        }
        Err(error) => memory_failure(error),
    }
}

fn invalid(env: &mut Env<'_>) -> Outcome {
    // VC80 preserves DOS errno, publishes errno22 before the returning callback,
    // and leaves a non-NULL output unchanged on these NULL-parameter failures.
    crate::util::set_last_error(env, 0);
    let thunk = env.sys.thunks.get(env.thunk);
    let name = format!("{}!{}", thunk.module, thunk.name);
    invalid_parameter_result(env, &name, 22, u32::MAX)
}

#[win32(msvcrt, name = "fgetpos", level = implement)]
fn get(env: &mut Env<'_>, stream: GuestPtr<u8>, position: GuestPtr<i64>) -> Outcome {
    if stream.is_null() || position.is_null() {
        return invalid(env);
    }
    // Validate the complete output before querying the stream; it can be
    // write-only. A failed guard neither touches the output nor moves the FILE.
    if let Err(error) = check_range(env.cpu, position.addr(), 8, Prot::WRITE) {
        return memory_failure(error);
    }
    let file = env
        .ext
        .try_get::<CrtState>()
        .and_then(|state| state.files.get(&stream.addr()));
    let Some(_) = file else {
        let errno = floating::errno_address(env);
        if let Err(error) = GuestPtr::<u32>::new(errno.addr() + 4).write(env.cpu, 0) {
            return memory_failure(error);
        }
        crate::util::set_last_error(env, 0);
        let thunk = env.sys.thunks.get(env.thunk);
        let name = format!("{}!{}", thunk.module, thunk.name);
        // Measured closed-FILE failure: the callback sees the previous output;
        // fgetpos publishes -1 only after that callback returns.
        return invalid_parameter_then(env, &name, 9, move |env| {
            match position.write(env.cpu, -1) {
                Ok(()) => Outcome::Return(u32::MAX),
                Err(error) => memory_failure(error),
            }
        });
    };
    file::position(
        env,
        stream.addr(),
        Box::new(move |env, result| {
            let offset = match result {
                Ok(offset) => offset,
                Err(status) => {
                    if let Err(fault) = position.write(env.cpu, -1) {
                        return memory_failure(fault);
                    }
                    return file::error(env, status, u32::MAX);
                }
            };
            let Ok(offset) = i64::try_from(offset) else {
                let thunk = env.sys.thunks.get(env.thunk);
                let name = format!("{}!{}", thunk.module, thunk.name);
                adv_abi::warn_unavailable(env, &name, "FILE byte position exceeds signed64 fpos_t");
                if let Err(fault) = position.write(env.cpu, -1) {
                    return memory_failure(fault);
                }
                return error(env, 40, None);
            };
            match position.write(env.cpu, offset) {
                Ok(()) => Outcome::Return(0),
                Err(fault) => memory_failure(fault),
            }
        }),
    )
}

#[win32(msvcrt, name = "fsetpos", level = implement)]
fn set(env: &mut Env<'_>, stream: GuestPtr<u8>, position: GuestPtr<i64>) -> Outcome {
    if stream.is_null() || position.is_null() {
        return invalid(env);
    }
    // Read the entire signed64 value before seeking, including page boundaries.
    let offset = match position.read(env.cpu) {
        Ok(offset) => offset,
        Err(error) => return memory_failure(error),
    };
    let file = env
        .ext
        .try_get::<CrtState>()
        .and_then(|state| state.files.get(&stream.addr()));
    let Some(_) = file else {
        // VC80 fsetpos(closed) is an ordinary EINVAL failure, no callback, and
        // preserves both the DOS error slot and Win32 LastError.
        return error(env, 22, None);
    };
    if offset < 0 {
        return error(env, 22, Some(131)); // ERROR_NEGATIVE_SEEK, cursor unchanged.
    }
    file::seek(
        env,
        stream.addr(),
        offset,
        0,
        Box::new(|env, result| match result {
            Ok(_) => Outcome::Return(0),
            Err(status) => file::error(env, status, u32::MAX),
        }),
    )
}

#[cfg(test)]
mod tests;
