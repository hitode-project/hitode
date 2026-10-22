//! The virtual CRT environment has no command processor. No host shell or
//! process is started, and ordinary launch failures are not capability gaps.
//! All versions and narrow/wide forms use the existing module/thread errno.
use super::*;
use adv_abi::guest::memory_failure;

const EXPORTS: &[(&str, adv_machine::Handler)] = &[
    ("system", system),
    ("_wsystem", system),
    ("_popen", popen),
    ("_wpopen", popen),
    ("_pclose", pclose),
    ("_execl", launch),
    ("_execle", launch),
    ("_execlp", launch),
    ("_execlpe", launch),
    ("_execv", launch),
    ("_execve", launch),
    ("_execvp", launch),
    ("_execvpe", launch),
    ("_wexecl", launch),
    ("_wexecle", launch),
    ("_wexeclp", launch),
    ("_wexeclpe", launch),
    ("_wexecv", launch),
    ("_wexecve", launch),
    ("_wexecvp", launch),
    ("_wexecvpe", launch),
    ("_spawnl", launch),
    ("_spawnle", launch),
    ("_spawnlp", launch),
    ("_spawnlpe", launch),
    ("_spawnv", launch),
    ("_spawnve", launch),
    ("_spawnvp", launch),
    ("_spawnvpe", launch),
    ("_wspawnl", launch),
    ("_wspawnle", launch),
    ("_wspawnlp", launch),
    ("_wspawnlpe", launch),
    ("_wspawnv", launch),
    ("_wspawnve", launch),
    ("_wspawnvp", launch),
    ("_wspawnvpe", launch),
];

pub(super) fn register(registry: &mut Registry) {
    versions::register(registry, EXPORTS);
}

fn failure(env: &mut Env<'_>, errno: i32, dos: Option<u32>, result: u32) -> Outcome {
    let address = floating::errno_address(env).addr();
    if address == 0 {
        return Outcome::Fail("CRT error-cell allocation failed".into());
    }
    if let Err(fault) = env.cpu.write_u32(address, errno as u32) {
        return memory_failure(fault);
    }
    if let Some(dos) = dos
        && let Err(fault) = env.cpu.write_u32(address + 4, dos)
    {
        return memory_failure(fault);
    }
    Outcome::Return(result)
}

fn invalid(env: &mut Env<'_>, result: u32) -> Outcome {
    errors::invalid(env, result)
}

fn system(env: &mut Env<'_>) -> Outcome {
    if env.arg(0) == 0 {
        // A NULL query neither needs a command buffer nor changes errno.
        return Outcome::Return(0);
    }
    failure(env, 2, Some(2), u32::MAX) // ENOENT / ERROR_FILE_NOT_FOUND
}

fn first(env: &Env<'_>, address: u32, wide: bool) -> Result<u32, adv_cpu::backend::MemFault> {
    if address == 0 {
        return Ok(0);
    }
    if wide {
        env.cpu.read_u16(address).map(u32::from)
    } else {
        env.cpu.read_u8(address).map(u32::from)
    }
}

fn launch(env: &mut Env<'_>) -> Outcome {
    let name = env.sys.thunks.get(env.thunk).name.as_str();
    let wide = name.starts_with("_w");
    let spawn = name.contains("spawn");
    let vector = name.contains("execv") || name.contains("spawnv");
    let path = env.arg(u32::from(spawn));
    let arguments = env.arg(u32::from(spawn) + 1);
    if spawn && env.arg(0) > 4 {
        return invalid(env, u32::MAX);
    }
    match first(env, path, wide) {
        Err(fault) => return memory_failure(fault),
        Ok(0) => return invalid(env, u32::MAX),
        Ok(_) => {}
    }
    let arg0 = if vector && arguments != 0 {
        match env.cpu.read_u32(arguments) {
            Ok(pointer) => pointer,
            Err(fault) => return memory_failure(fault),
        }
    } else {
        arguments
    };
    match first(env, arg0, wide) {
        Err(fault) => memory_failure(fault),
        Ok(0) => invalid(env, u32::MAX),
        Ok(_) => failure(env, 2, Some(2), u32::MAX),
    }
}

fn popen(env: &mut Env<'_>) -> Outcome {
    let wide = env.sys.thunks.get(env.thunk).name == "_wpopen";
    if env.arg(0) == 0 || env.arg(1) == 0 {
        return invalid(env, 0);
    }
    let address = env.arg(1);
    let step = if wide { 2 } else { 1 };
    let valid = (|| {
        if !matches!(first(env, address, wide)?, 0x72 | 0x77) {
            return Ok(false);
        }
        match first(env, address.wrapping_add(step), wide)? {
            0 => Ok(true),
            0x62 | 0x74 => first(env, address.wrapping_add(step * 2), wide).map(|unit| unit == 0),
            _ => Ok(false),
        }
    })();
    match valid {
        Err(fault) => memory_failure(fault),
        Ok(true) => failure(env, 2, Some(2), 0),
        Ok(false) => invalid(env, 0),
    }
}

fn pclose(env: &mut Env<'_>) -> Outcome {
    if env.arg(0) == 0 {
        return invalid(env, u32::MAX);
    }
    // No successful popen can publish a pipe stream. A normal FILE is not a
    // pipe and retains its handle and ownership after this failure.
    failure(env, 9, None, u32::MAX) // EBADF
}
