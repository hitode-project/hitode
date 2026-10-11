//! An isolated process UI profile. No host system setting is read or written.
use super::{user, user_mut};
use crate::{Win32, util::set_last_error};
use adv_abi::{AnsiStr, Env, GuestPtr, Registry, WideStr, win32};
use adv_cpu::backend::Prot;

pub struct IconFont {
    header: [u8; 28],
    face: [u16; 32],
}
impl Default for IconFont {
    fn default() -> Self {
        let mut header = [0; 28];
        header[..4].copy_from_slice(&(-12i32).to_le_bytes());
        header[16..20].copy_from_slice(&400i32.to_le_bytes());
        header[23] = 1; // DEFAULT_CHARSET: matching uses the existing font path.
        let mut face = [0; 32];
        for (slot, unit) in face.iter_mut().zip("MS UI Gothic".encode_utf16()) {
            *slot = unit;
        }
        Self { header, face }
    }
}
fn fail(env: &mut Env<'_>, error: u32) -> i32 {
    set_last_error(env, error);
    0
}
fn parameters(env: &mut Env<'_>, action: u32, parameter: u32, output: u32, wide: bool) -> i32 {
    match action {
        0x1d => user_mut(env).double_click.width = parameter,
        0x1e => user_mut(env).double_click.height = parameter,
        0x20 => user_mut(env).double_click.set_time(parameter),
        0x21 => user_mut(env).swap_buttons = parameter != 0,
        _ => {}
    }
    if matches!(action, 0x1d | 0x1e | 0x20 | 0x21) {
        return 1;
    }
    if action != 0x1f {
        return fail(env, 120);
    }
    let length = if wide { 92 } else { 60 };
    if adv_abi::guest::check_range(env.cpu, output, length, Prot::WRITE).is_err() {
        return fail(env, 87);
    }
    let font = &user(env).icon_title_font;
    let mut bytes = [0; 92];
    bytes[..28].copy_from_slice(&font.header);
    if wide {
        for (slot, unit) in bytes[28..].as_chunks_mut::<2>().0.iter_mut().zip(font.face) {
            *slot = unit.to_le_bytes();
        }
    } else {
        let end = font.face.iter().position(|&unit| unit == 0).unwrap_or(31);
        let name = env
            .ext
            .get::<Win32>()
            .codec
            .from_utf16(0, &font.face[..end])
            .0;
        let count = name.len().min(31);
        bytes[28..28 + count].copy_from_slice(&name[..count]);
    }
    if env.cpu.write(output, &bytes[..length as usize]).is_err() {
        return fail(env, 87);
    }
    // Native accepts every uiParam size for this fixed-layout GET. Its zero
    // form clears LastError; normal forms preserve the previous value.
    if parameter == 0 {
        set_last_error(env, 0);
    }
    1
}
#[win32(user32,name="SystemParametersInfoA",level=state)]
fn system_a(
    env: &mut Env<'_>,
    action: u32,
    parameter: u32,
    output: GuestPtr<()>,
    _flags: u32,
) -> i32 {
    parameters(env, action, parameter, output.addr(), false)
}
#[win32(user32,name="SystemParametersInfoW",level=state)]
fn system_w(
    env: &mut Env<'_>,
    action: u32,
    parameter: u32,
    output: GuestPtr<()>,
    _flags: u32,
) -> i32 {
    parameters(env, action, parameter, output.addr(), true)
}
fn registered(env: &mut Env<'_>, name: Vec<u16>) -> u32 {
    // RegisterWindowMessage and RegisterClipboardFormat share the USER atom
    // namespace natively. Names identify messages/formats, not resource owners.
    user_mut(env).clipboard.register(name).unwrap_or_else(|| {
        set_last_error(env, 87);
        0
    })
}
#[win32(user32,name="RegisterWindowMessageA",level=state)]
fn message_a(env: &mut Env<'_>, name: AnsiStr) -> u32 {
    let Ok(bytes) = name.read(env.cpu, 256) else {
        return fail(env, 87) as u32;
    };
    registered(env, env.ext.get::<Win32>().codec.to_utf16(0, &bytes))
}
#[win32(user32,name="RegisterWindowMessageW",level=state)]
fn message_w(env: &mut Env<'_>, name: WideStr) -> u32 {
    let Ok(units) = name.read(env.cpu, 256) else {
        return fail(env, 87) as u32;
    };
    registered(env, units)
}
pub(super) fn register(registry: &mut Registry) {
    for api in [system_a_ABI, system_w_ABI, message_a_ABI, message_w_ABI] {
        registry.register(api);
    }
}

#[cfg(test)]
mod tests;
