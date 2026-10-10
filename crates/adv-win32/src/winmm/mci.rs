//! Explicit no-device MCI boundary. No host activation or fabricated notification.
use adv_abi::{AnsiStr, GuestPtr, Registry, WideStr, guest::check_range, win32};
use adv_cpu::backend::{GuestExt, Prot};
use adv_machine::Env;

const MCI_OPEN: u32 = 0x803;
const INVALID_DEVICE_ID: u32 = 257;
const MISSING_STRING_ARGUMENT: u32 = 269;
const MISSING_PARAMETER: u32 = 273;
const NULL_PARAMETER_BLOCK: u32 = 297;
const DEVICE_NOT_INSTALLED: u32 = 306;

#[win32(winmm, name = "mciSendCommandA", level = safe_fail)]
fn send_a(env: &mut Env<'_>, _device: u32, message: u32, flags: u32, parameters: u32) -> u32 {
    send(env, message, flags, parameters, false)
}
#[win32(winmm, name = "mciSendCommandW", level = safe_fail)]
fn send_w(env: &mut Env<'_>, _device: u32, message: u32, flags: u32, parameters: u32) -> u32 {
    send(env, message, flags, parameters, true)
}
fn send(env: &mut Env<'_>, message: u32, flags: u32, parameters: u32, wide: bool) -> u32 {
    let api = if wide {
        "mciSendCommandW"
    } else {
        "mciSendCommandA"
    };
    if message != MCI_OPEN {
        adv_abi::warn_unavailable(env, api, "no MCI device identifiers are open");
        return INVALID_DEVICE_ID;
    }
    if parameters == 0 {
        return NULL_PARAMETER_BLOCK;
    }
    // SDK x86 MCI_OPEN_PARMSA: callback, device ID, type, element and alias.
    if check_range(env.cpu, parameters, 20, Prot::READ).is_err()
        || check_range(env.cpu, parameters + 4, 4, Prot::WRITE).is_err()
    {
        return MISSING_PARAMETER;
    }
    for (present, is_id, offset) in [(0x2000, 0x1000, 8), (0x200, 0x800, 12), (0x400, 0, 16)] {
        if flags & present == 0 || (is_id != 0 && flags & is_id != 0) {
            continue;
        }
        let Ok(pointer) = env.cpu.read_u32(parameters + offset) else {
            return MISSING_PARAMETER;
        };
        let present = if wide {
            WideStr(GuestPtr::new(pointer))
                .read(env.cpu, crate::util::MAX_STR)
                .is_ok_and(|s| !s.is_empty())
        } else {
            AnsiStr(GuestPtr::new(pointer))
                .read(env.cpu, crate::util::MAX_STR)
                .is_ok_and(|s| !s.is_empty())
        };
        if !present {
            return MISSING_STRING_ARGUMENT;
        }
    }
    if env.cpu.write_u32(parameters + 4, 0).is_err() {
        return MISSING_PARAMETER;
    }
    adv_abi::warn_unavailable(
        env,
        api,
        "MCI devices are not installed in the guest runtime",
    );
    DEVICE_NOT_INSTALLED
}

#[win32(winmm, name = "mciGetErrorStringA", level = implement)]
fn error_string_a(env: &mut Env<'_>, error: u32, output: AnsiStr, capacity: u32) -> i32 {
    error_string(env, error, output.0.addr(), capacity, false)
}
#[win32(winmm, name = "mciGetErrorStringW", level = implement)]
fn error_string_w(env: &mut Env<'_>, error: u32, output: WideStr, capacity: u32) -> i32 {
    error_string(env, error, output.0.addr(), capacity, true)
}
fn error_string(env: &mut Env<'_>, error: u32, output: u32, capacity: u32, wide: bool) -> i32 {
    // General errors have no driver identifier in their high word. Do not claim
    // to translate errors from a driver this runtime has never opened.
    let text = match error {
        0 => "No error.",
        INVALID_DEVICE_ID => "The specified MCI device identifier is invalid.",
        MISSING_STRING_ARGUMENT => "A required MCI string argument is missing or invalid.",
        MISSING_PARAMETER => "A required MCI parameter is missing or inaccessible.",
        NULL_PARAMETER_BLOCK => "No MCI parameter block was supplied.",
        DEVICE_NOT_INSTALLED => "The specified MCI device is not installed.",
        _ => return 0,
    };
    if capacity == 0 {
        return 0;
    }
    let length = text.len().min(capacity as usize - 1);
    if check_range(
        env.cpu,
        output,
        (length as u32 + 1) * if wide { 2 } else { 1 },
        Prot::WRITE,
    )
    .is_err()
    {
        return 0;
    }
    let mut bytes = text.as_bytes()[..length].to_vec();
    bytes.push(0);
    if wide {
        bytes = bytes
            .into_iter()
            .flat_map(|b| u16::from(b).to_le_bytes())
            .collect();
    }
    i32::from(env.cpu.write(output, &bytes).is_ok())
}

pub(super) fn register(registry: &mut Registry) {
    registry.register(send_a_ABI);
    registry.register(send_w_ABI);
    registry.register(error_string_a_ABI);
    registry.register(error_string_w_ABI);
}
