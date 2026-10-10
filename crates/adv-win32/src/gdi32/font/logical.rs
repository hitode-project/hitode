//! Unicode logical fonts retain the original face for the common font matcher.
//! A/W LOGFONTs share all 28 scalar bytes and one GDI object lifetime.
use super::super::add_logfont_with_face;
use crate::{Win32, util::set_last_error};
use adv_abi::{Env, GuestPtr, Handle, Registry, WideStr, guest::check_range, win32};
use adv_cpu::backend::Prot;

#[win32(gdi32, name = "CreateFontIndirectW", level = implement)]
fn create_indirect_w(env: &mut Env<'_>, pointer: GuestPtr<[u8; 92]>) -> Handle {
    // Native NULL returns NULL without changing LastError. Other unreadable
    // guest spans follow the existing A front's checked failure contract.
    if check_range(env.cpu, pointer.addr(), 92, Prot::READ).is_err() {
        return Handle(0);
    }
    let Ok(wide) = pointer.read(env.cpu) else {
        return Handle(0);
    };
    from_wide(env, wide, "gdi32.dll!CreateFontIndirectW")
}

fn from_wide(env: &mut Env<'_>, wide: [u8; 92], api: &str) -> Handle {
    let units: Vec<_> = wide[28..]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|bytes| u16::from_le_bytes(*bytes))
        .collect();
    // Native preserves all 32 units in a nonterminated LOGFONT field. The fixed
    // field never permits a search beyond these 92 input bytes.
    let end = units.iter().position(|&unit| unit == 0).unwrap_or(32);
    let Ok(face) = String::from_utf16(&units[..end]) else {
        adv_abi::warn_unavailable(
            env,
            api,
            "logical face with unpaired UTF-16 surrogates has no supported font request representation",
        );
        set_last_error(env, 1113);
        return Handle(0);
    };
    // GetObjectA describes the same font in the guest ACP. Keep that legacy
    // serialization separate from the Unicode matcher name and charset byte;
    // never split a DBCS/UTF8 scalar at the fixed ANSI field boundary.
    let mut raw = vec![0; 60];
    raw[..28].copy_from_slice(&wide[..28]);
    let limit = if end == 32 { 32 } else { 31 };
    let mut at = 28;
    for character in face.chars() {
        let bytes = env.ext.get::<Win32>().codec.encode(&character.to_string());
        if at + bytes.len() > 28 + limit {
            break;
        }
        raw[at..at + bytes.len()].copy_from_slice(&bytes);
        at += bytes.len();
    }
    Handle(add_logfont_with_face(env, raw, face, Some(wide)))
}

#[win32(gdi32, name = "CreateFontW", level = implement)]
#[expect(clippy::too_many_arguments, reason = "SDK CreateFontW fields")]
fn create_w(
    env: &mut Env<'_>,
    height: i32,
    width: i32,
    escapement: i32,
    orientation: i32,
    weight: i32,
    italic: u32,
    underline: u32,
    strikeout: u32,
    charset: u32,
    precision: u32,
    clip: u32,
    quality: u32,
    pitch: u32,
    face: WideStr,
) -> Handle {
    let mut wide = [0; 92];
    for (slot, value) in wide[..20].as_chunks_mut::<4>().0.iter_mut().zip([
        height,
        width,
        escapement,
        orientation,
        weight,
    ]) {
        *slot = value.to_le_bytes();
    }
    for (slot, value) in wide[20..28].iter_mut().zip([
        italic, underline, strikeout, charset, precision, clip, quality, pitch,
    ]) {
        *slot = value as u8;
    }
    if !face.0.is_null() {
        let Ok(units) = face.read(env.cpu, 4096) else {
            return Handle(0);
        };
        for (slot, &unit) in wide[28..]
            .as_chunks_mut::<2>()
            .0
            .iter_mut()
            .take(31)
            .zip(&units)
        {
            *slot = unit.to_le_bytes();
        }
    }
    from_wide(env, wide, "gdi32.dll!CreateFontW")
}

pub(super) fn register(registry: &mut Registry) {
    registry.register(create_indirect_w_ABI);
    registry.register(create_w_ABI);
}
