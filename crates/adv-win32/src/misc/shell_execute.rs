//! Shell activation has no guest child-process or host-launch backend. Keep the
//! exports callable and return a real failure rather than inventing execution.
use adv_abi::{AnsiStr, GuestPtr, Handle, Registry, WideStr, guest::check_range, win32};
use adv_cpu::backend::{GuestExt, Prot};
use adv_machine::Env;

use crate::util::{MAX_STR, err, set_last_error};

// Windows SDK shellapi.h: both x86 SHELLEXECUTEINFO layouts have 15 DWORDs.
// https://learn.microsoft.com/windows/win32/api/shellapi/ns-shellapi-shellexecuteinfoa
const INFO_SIZE: u32 = 60;
const INST_APP: u32 = 32;
const PROCESS: u32 = 56;
const ERROR_NOACCESS: u32 = 998;
const SEE_MASK_CLASSNAME: u32 = 1;
const SEE_MASK_CLASSKEY: u32 = 3;
const SEE_MASK_IDLIST: u32 = 4;
const SEE_MASK_INVOKEIDLIST: u32 = 8; // The additional bit; the complete mask is 0x0c.

#[derive(Default)]
struct Request {
    mask: u32,
    verb: u32,
    file: u32,
    parameters: u32,
    directory: u32,
    id_list: u32,
    class: u32,
    class_key: u32,
}

/// Validate a supplied terminated string without silently converting faults to
/// empty input. No target is opened, resolved through a host association or run.
fn nonempty_text(env: &Env<'_>, address: u32, wide: bool) -> Result<bool, u32> {
    if address == 0 {
        return Ok(false);
    }
    let nonempty = if wide {
        !WideStr(GuestPtr::new(address))
            .read(env.cpu, MAX_STR)
            .map_err(|_| ERROR_NOACCESS)?
            .is_empty()
    } else {
        !AnsiStr(GuestPtr::new(address))
            .read(env.cpu, MAX_STR)
            .map_err(|_| ERROR_NOACCESS)?
            .is_empty()
    };
    Ok(nonempty)
}

fn request_error(env: &mut Env<'_>, request: &Request, wide: bool, api: &str) -> u32 {
    let validation = (|| {
        let by_id_list = request.mask & SEE_MASK_IDLIST != 0 && request.id_list != 0;
        if request.mask & SEE_MASK_IDLIST != 0
            && request.mask & SEE_MASK_INVOKEIDLIST == 0
            && !by_id_list
        {
            return Err(err::INVALID_PARAMETER);
        }
        if !by_id_list && !nonempty_text(env, request.file, wide)? {
            return Err(err::INVALID_PARAMETER);
        }
        for address in [request.verb, request.parameters, request.directory] {
            nonempty_text(env, address, wide)?;
        }
        match request.mask & SEE_MASK_CLASSKEY {
            SEE_MASK_CLASSKEY if request.class_key == 0 => return Err(err::INVALID_PARAMETER),
            SEE_MASK_CLASSNAME if !nonempty_text(env, request.class, wide)? => {
                return Err(err::INVALID_PARAMETER);
            }
            _ => {}
        }
        Ok(())
    })();
    if let Err(error) = validation {
        return error;
    }
    adv_abi::warn_unavailable(
        env,
        api,
        "shell activation is unavailable; guest child processes and host launch are not supported",
    );
    err::ACCESS_DENIED
}

// Legacy ShellExecute has no INVALID_PARAMETER/NOACCESS code. Access denial is
// its failure value; LastError retains the more precise validation error.
fn execute(env: &mut Env<'_>, request: Request, wide: bool, api: &str) -> Handle {
    let error = request_error(env, &request, wide, api);
    set_last_error(env, error);
    Handle(err::ACCESS_DENIED)
}

fn execute_ex(env: &mut Env<'_>, pointer: u32, wide: bool, api: &str) -> i32 {
    if pointer == 0 {
        set_last_error(env, err::INVALID_PARAMETER);
        return 0;
    }
    let size =
        check_range(env.cpu, pointer, 4, Prot::READ).and_then(|()| env.cpu.read_u32(pointer));
    let Ok(size) = size else {
        set_last_error(env, ERROR_NOACCESS);
        return 0;
    };
    if size != INFO_SIZE {
        set_last_error(env, err::INVALID_PARAMETER);
        return 0;
    }
    // Validate both outputs before changing either, including a structure that
    // crosses onto a readonly or unmapped page. Host writes bypass permissions.
    if check_range(env.cpu, pointer, INFO_SIZE, Prot::READ).is_err()
        || check_range(env.cpu, pointer + INST_APP, 4, Prot::WRITE).is_err()
        || check_range(env.cpu, pointer + PROCESS, 4, Prot::WRITE).is_err()
    {
        set_last_error(env, ERROR_NOACCESS);
        return 0;
    }
    let mut raw = [0; INFO_SIZE as usize];
    if env.cpu.read(pointer, &mut raw).is_err() {
        set_last_error(env, ERROR_NOACCESS);
        return 0;
    }
    let word = |offset: usize| u32::from_le_bytes(raw[offset..offset + 4].try_into().unwrap());
    let request = Request {
        mask: word(4),
        verb: word(12),
        file: word(16),
        parameters: word(20),
        directory: word(24),
        id_list: word(36),
        class: word(40),
        class_key: word(44),
    };
    let error = request_error(env, &request, wide, api);
    if env
        .cpu
        .write_u32(pointer + INST_APP, err::ACCESS_DENIED)
        .is_err()
        || env.cpu.write_u32(pointer + PROCESS, 0).is_err()
    {
        set_last_error(env, ERROR_NOACCESS);
        return 0;
    }
    set_last_error(env, error);
    0
}

#[win32(shell32, name = "ShellExecuteA", level = safe_fail)]
fn shell_execute_a(
    env: &mut Env<'_>,
    _window: Handle,
    verb: AnsiStr,
    file: AnsiStr,
    parameters: AnsiStr,
    directory: AnsiStr,
    _show: i32,
) -> Handle {
    execute(
        env,
        Request {
            verb: verb.0.addr(),
            file: file.0.addr(),
            parameters: parameters.0.addr(),
            directory: directory.0.addr(),
            ..Default::default()
        },
        false,
        "ShellExecuteA",
    )
}

#[win32(shell32, name = "ShellExecuteW", level = safe_fail)]
fn shell_execute_w(
    env: &mut Env<'_>,
    _window: Handle,
    verb: WideStr,
    file: WideStr,
    parameters: WideStr,
    directory: WideStr,
    _show: i32,
) -> Handle {
    execute(
        env,
        Request {
            verb: verb.0.addr(),
            file: file.0.addr(),
            parameters: parameters.0.addr(),
            directory: directory.0.addr(),
            ..Default::default()
        },
        true,
        "ShellExecuteW",
    )
}

#[win32(shell32, name = "ShellExecuteExA", level = safe_fail)]
fn shell_execute_ex_a(env: &mut Env<'_>, info: GuestPtr<()>) -> i32 {
    execute_ex(env, info.addr(), false, "ShellExecuteExA")
}

#[win32(shell32, name = "ShellExecuteExW", level = safe_fail)]
fn shell_execute_ex_w(env: &mut Env<'_>, info: GuestPtr<()>) -> i32 {
    execute_ex(env, info.addr(), true, "ShellExecuteExW")
}

pub(super) fn register(registry: &mut Registry) {
    registry.register(shell_execute_a_ABI);
    registry.register(shell_execute_w_ABI);
    registry.register(shell_execute_ex_a_ABI);
    registry.register(shell_execute_ex_w_ABI);
}
