//! The primary DirectDraw device is the same virtual display used by GDI.
use adv_abi::{Env, GuestPtr, Outcome, Registry, win32};
use adv_cpu::backend::{Prot, Reg};

const DESCRIPTION: &str = "hitode virtual display adapter";
const DRIVER: &str = "display";
const INVALID: u32 = 0x80070057;

pub(super) fn register(registry: &mut Registry) {
    registry.register(enumerate_a_ABI);
    registry.register(enumerate_w_ABI);
    registry.register(enumerate_ex_a_ABI);
    registry.register(enumerate_ex_w_ABI);
}

fn enumerate(
    env: &mut Env<'_>,
    callback: u32,
    context: u32,
    wide: bool,
    extended: bool,
) -> Outcome {
    if callback == 0 || !env.cpu.accessible(callback, 1, Prot::EXEC) {
        return Outcome::Return(INVALID);
    }
    let encode = |text: &str| -> Vec<u8> {
        if wide {
            text.encode_utf16()
                .chain([0])
                .flat_map(u16::to_le_bytes)
                .collect()
        } else {
            [text.as_bytes(), &[0]].concat()
        }
    };
    let description = encode(DESCRIPTION);
    let driver = encode(DRIVER);
    let length = (description.len() + driver.len()) as u32;
    let Some(scratch) = env.cpu.reg(Reg::Esp).checked_sub(length).map(|p| p & !3) else {
        return Outcome::Return(INVALID);
    };
    if adv_abi::guest::check_range(env.cpu, scratch, length, Prot::WRITE).is_err()
        || env.cpu.write(scratch, &description).is_err()
        || env
            .cpu
            .write(scratch + description.len() as u32, &driver)
            .is_err()
    {
        return Outcome::Return(INVALID);
    }
    // NULL GUID denotes the primary display accepted by DirectDrawCreate.
    // Scratch remains above the callback stack, including nested enumeration
    // and waits. There is one device, so both callback stop/continue return DD_OK.
    let mut args = vec![0, scratch, scratch + description.len() as u32, context];
    if extended {
        // Primary virtual adapter passes NULL HMONITOR, matching Win11 ddraw.dll.
        args.push(0);
    }
    Outcome::call_on_stack(callback, args, scratch, |_, _| Outcome::Return(0))
}

#[win32(ddraw, name = "DirectDrawEnumerateA", level = state)]
fn enumerate_a(env: &mut Env<'_>, callback: GuestPtr<()>, context: GuestPtr<()>) -> Outcome {
    enumerate(env, callback.addr(), context.addr(), false, false)
}

#[win32(ddraw, name = "DirectDrawEnumerateW", level = state)]
fn enumerate_w(env: &mut Env<'_>, callback: GuestPtr<()>, context: GuestPtr<()>) -> Outcome {
    enumerate(env, callback.addr(), context.addr(), true, false)
}

#[win32(ddraw, name = "DirectDrawEnumerateExA", level = implement)]
fn enumerate_ex_a(
    env: &mut Env<'_>,
    callback: GuestPtr<()>,
    context: GuestPtr<()>,
    flags: u32,
) -> Outcome {
    if flags & !0x7 != 0 {
        return Outcome::Return(INVALID);
    }
    enumerate(env, callback.addr(), context.addr(), false, true)
}

#[win32(ddraw, name = "DirectDrawEnumerateExW", level = implement)]
fn enumerate_ex_w(
    env: &mut Env<'_>,
    callback: GuestPtr<()>,
    context: GuestPtr<()>,
    flags: u32,
) -> Outcome {
    if flags & !0x7 != 0 {
        return Outcome::Return(INVALID);
    }
    enumerate(env, callback.addr(), context.addr(), true, true)
}

#[cfg(test)]
mod tests;
