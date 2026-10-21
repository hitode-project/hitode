//! errno and _doserrno are adjacent cells owned by the existing CRT thread
//! allocation. Getters and setters never consult the host process's errno.
use super::*;
use adv_abi::{GuestPtr, guest::memory_failure};

pub(super) fn register(registry: &mut Registry) {
    versions::register(
        registry,
        &[
            ("_errno", address),
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
