//! errno and _doserrno are adjacent cells owned by the existing CRT thread
//! allocation. Getters and setters never consult the host process's errno.
use super::*;
use adv_abi::{GuestPtr, guest::memory_failure};

pub(super) fn register(registry: &mut Registry) {
    versions::register(
        registry,
        &[
            ("_errno", address),
            ("_dosmaperr", dosmaperr),
            ("__doserrno", address),
            ("_get_errno", get),
            ("_get_doserrno", get),
            ("_set_errno", set),
            ("_set_doserrno", set),
            ("_set_invalid_parameter_handler", set_handler),
            ("_get_invalid_parameter_handler", get_handler),
        ],
    );
}

fn cell(env: &mut Env<'_>) -> Result<u32, Outcome> {
    let dos = env.sys.thunks.get(env.thunk).name.contains("doserrno");
    let pointer = floating::errno_address(env).addr();
    if pointer == 0 {
        return Err(Outcome::Fail("CRT error-cell allocation failed".into()));
    }
    Ok(pointer + if dos { 4 } else { 0 })
}

fn address(env: &mut Env<'_>) -> Outcome {
    match cell(env) {
        Ok(pointer) => Outcome::Return(pointer),
        Err(outcome) => outcome,
    }
}

fn get(env: &mut Env<'_>) -> Outcome {
    let output = GuestPtr::<u32>::new(env.arg(0));
    if output.is_null() {
        let name = env.sys.thunks.get(env.thunk).name.clone();
        if invalid_handler(env) == 0 {
            if let Err(fault) = floating::errno_address(env).write(env.cpu, 22) {
                return memory_failure(fault);
            }
            return invalid_parameter_noinfo(env);
        }
        return invalid_parameter(env, &name, 22);
    }
    let pointer = match cell(env) {
        Ok(pointer) => pointer,
        Err(outcome) => return outcome,
    };
    match env
        .cpu
        .read_u32(pointer)
        .and_then(|value| output.write(env.cpu, value))
    {
        Ok(()) => Outcome::Return(0),
        Err(fault) => memory_failure(fault),
    }
}

fn set(env: &mut Env<'_>) -> Outcome {
    let value = env.arg(0);
    let pointer = match cell(env) {
        Ok(pointer) => pointer,
        Err(outcome) => return outcome,
    };
    match env.cpu.write_u32(pointer, value) {
        Ok(()) => Outcome::Return(0),
        Err(fault) => memory_failure(fault),
    }
}

fn set_handler(env: &mut Env<'_>) -> Outcome {
    let value = env.arg(0);
    Outcome::Return(floating::invalid_set(env, GuestPtr::new(value)).addr())
}

fn get_handler(env: &mut Env<'_>) -> Outcome {
    Outcome::Return(invalid_handler(env))
}

/// Parameter checks share legacy errno and modern invalid-handler profiles.
pub(super) fn invalid(env: &mut Env<'_>, result: u32) -> Outcome {
    invalid_outcome(env, Outcome::Return(result))
}

pub(super) fn invalid_outcome(env: &mut Env<'_>, result: Outcome) -> Outcome {
    validation(env, 22, result)
}

pub(super) fn validation(env: &mut Env<'_>, error: i32, result: Outcome) -> Outcome {
    if let Err(fault) = floating::errno_address(env).write(env.cpu, error) {
        return memory_failure(fault);
    }
    if !versions::profile(&env.sys.thunks.get(env.thunk).module).invalid_parameters {
        return result;
    }
    if invalid_handler(env) == 0 {
        return invalid_parameter_noinfo(env);
    }
    let name = env.sys.thunks.get(env.thunk).name.clone();
    invalid_parameter_then(env, &name, error, move |_| result)
}

/// Pinned Microsoft VC90's first-match DOS error table and fallback ranges.
/// The duplicate ERROR_INVALID_HANDLE row resolves to the first EBADF entry.
pub(super) fn dos_errno(code: u32) -> i32 {
    match code {
        2 | 3 | 15 | 18 | 53 | 67 | 161 | 206 => 2,
        4 => 24,
        5 | 16 | 19..=36 | 65 | 82 | 83 | 108 | 132 | 158 | 167 => 13,
        6 | 114 | 130 => 9,
        7..=9 | 1816 => 12,
        10 => 7,
        11 | 188..=202 => 8,
        17 => 18,
        80 | 183 => 17,
        89 | 164 | 215 => 11,
        109 => 32,
        112 => 28,
        128 | 129 => 10,
        145 => 41,
        _ => 22,
    }
}

pub(super) fn map_dos(env: &mut Env<'_>, code: u32, result: u32) -> Outcome {
    let pointer = floating::errno_address(env).addr();
    if pointer == 0 {
        return crate::kernel32::exit_process(env, 255);
    }
    let errno = if code == 1113
        && versions::profile(&env.sys.thunks.get(env.thunk).module).unicode_file_paths
    {
        42 // UCRT adds NO_UNICODE_TRANSLATION -> EILSEQ to the legacy table.
    } else {
        dos_errno(code)
    };
    let pair = [(errno as u32).to_le_bytes(), code.to_le_bytes()].concat();
    match env.cpu.write(pointer, &pair) {
        Ok(()) => Outcome::Return(result),
        Err(fault) => memory_failure(fault),
    }
}
fn dosmaperr(env: &mut Env<'_>) -> Outcome {
    map_dos(env, env.arg(0), 0)
}
