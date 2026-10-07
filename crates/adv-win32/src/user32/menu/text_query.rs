//! Legacy menu text queries use the same UTF-16 item owner as MENUITEMINFO.
use super::{get, user};
use crate::{Win32, util::set_last_error};
use adv_abi::{AnsiStr, Env, Handle, Registry, WideStr, guest::check_range, win32};
use adv_cpu::backend::Prot;

pub(super) fn register(registry: &mut Registry) {
    registry.register(get_a_ABI);
    registry.register(get_w_ABI);
}

#[win32(user32, name = "GetMenuStringA", level = state)]
fn get_a(
    env: &mut Env<'_>,
    menu: Handle,
    item: u32,
    output: AnsiStr,
    capacity: i32,
    flags: u32,
) -> i32 {
    query(env, menu.0, item, output.0.addr(), capacity, flags, false)
}

#[win32(user32, name = "GetMenuStringW", level = state)]
fn get_w(
    env: &mut Env<'_>,
    menu: Handle,
    item: u32,
    output: WideStr,
    capacity: i32,
    flags: u32,
) -> i32 {
    query(env, menu.0, item, output.0.addr(), capacity, flags, true)
}

fn failure(env: &mut Env<'_>, error: u32) -> i32 {
    set_last_error(env, error);
    0
}

fn query(
    env: &mut Env<'_>,
    menu: u32,
    item: u32,
    output: u32,
    capacity: i32,
    flags: u32,
    wide: bool,
) -> i32 {
    let width = if wide { 2 } else { 1 };
    if capacity != 0 {
        if output == 0 {
            return failure(env, 998);
        }
        if check_range(env.cpu, output, width, Prot::WRITE).is_err()
            || env.cpu.write(output, &[0; 2][..width as usize]).is_err()
        {
            return failure(env, 998);
        }
    }
    if get(env, menu).is_none() {
        return failure(env, 87);
    }
    let Some((owner, position)) =
        user(env)
            .menus
            .locate(&env.sys.objects, menu, item, flags & 0x400 != 0)
    else {
        return failure(env, 1456);
    };
    let text = &get(env, owner).unwrap()[position].text;
    let mut bytes = if wide {
        text.iter()
            .flat_map(|unit| unit.to_le_bytes())
            .collect::<Vec<_>>()
    } else {
        env.ext.get::<Win32>().codec.from_utf16(0, text).0
    };
    let length = bytes.len() / width as usize;
    if capacity == 0 || (wide && capacity < 0) {
        return length as i32;
    }
    let limit = if capacity < 0 {
        length
    } else {
        length.min(capacity as usize - 1)
    };
    // Native ANSI queries stop before an incomplete multibyte character.
    // Use the same conversion policy for sizing and copying, including fallback.
    let copied = if !wide && limit < length {
        let codec = &env.ext.get::<Win32>().codec;
        let mut copied = 0;
        for character in String::from_utf16_lossy(text).chars() {
            let size = codec.encode(&character.to_string()).len();
            if copied + size > limit {
                break;
            }
            copied += size;
        }
        copied
    } else {
        limit
    };
    bytes.truncate(copied * width as usize);
    bytes.resize(bytes.len() + width as usize, 0);
    if check_range(env.cpu, output, bytes.len() as u32, Prot::WRITE).is_err()
        || env.cpu.write(output, &bytes).is_err()
    {
        return failure(env, 998);
    }
    copied as i32
}

#[cfg(test)]
mod tests;
