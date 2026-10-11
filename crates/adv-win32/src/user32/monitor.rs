//! A/W layout views of the same virtual primary display profile.
use super::{user, window};
use adv_abi::{Env, GuestPtr, Handle, Registry, win32};
use adv_cpu::backend::Prot;

mod enumeration;
mod settings;

const PRIMARY_DEVICE_NAME: &str = r"\\.\DISPLAY1";

pub(super) fn register(registry: &mut Registry) {
    registry.register(info_a_ABI);
    registry.register(info_w_ABI);
    enumeration::register(registry);
    settings::register(registry);
}
fn fail(env: &mut Env<'_>, error: u32) -> i32 {
    crate::util::set_last_error(env, error);
    0
}
fn info(env: &mut Env<'_>, monitor: Handle, output: GuestPtr<()>, wide: bool) -> i32 {
    if monitor.0 != window::HMONITOR_PRIMARY {
        return fail(env, 1461); // ERROR_INVALID_MONITOR_HANDLE.
    }
    let Ok(size) = GuestPtr::<u32>::new(output.addr()).read(env.cpu) else {
        return fail(env, 87);
    };
    let extended = if wide { 104 } else { 72 };
    if size != 40 && size != extended {
        return fail(env, 87);
    }
    // One complete writable span is checked before any member is published.
    if adv_abi::guest::check_range(env.cpu, output.addr(), size, Prot::WRITE).is_err() {
        return fail(env, 87);
    }
    let (width, height) = user(env).screen;
    let mut bytes = [0u8; 104];
    let work_height = height.saturating_sub(40).max(0);
    for (index, value) in [
        size,
        0,
        0,
        width as u32,
        height as u32,
        0,
        0,
        width as u32,
        work_height as u32,
        1,
    ]
    .into_iter()
    .enumerate()
    {
        bytes[index * 4..index * 4 + 4].copy_from_slice(&value.to_le_bytes());
    }
    if size == extended {
        let name = PRIMARY_DEVICE_NAME.as_bytes();
        if wide {
            for (index, &character) in name.iter().enumerate() {
                bytes[40 + index * 2..42 + index * 2]
                    .copy_from_slice(&u16::from(character).to_le_bytes());
            }
        } else {
            bytes[40..40 + name.len()].copy_from_slice(name);
        }
    }
    if env
        .cpu
        .write(output.addr(), &bytes[..size as usize])
        .is_err()
    {
        return fail(env, 87);
    }
    1
}
#[win32(user32, name = "GetMonitorInfoA", level = state)]
fn info_a(env: &mut Env<'_>, monitor: Handle, output: GuestPtr<()>) -> i32 {
    info(env, monitor, output, false)
}
#[win32(user32, name = "GetMonitorInfoW", level = state)]
fn info_w(env: &mut Env<'_>, monitor: Handle, output: GuestPtr<()>) -> i32 {
    info(env, monitor, output, true)
}

#[cfg(test)]
mod tests;
