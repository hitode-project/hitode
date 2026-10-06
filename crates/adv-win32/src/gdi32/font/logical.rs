//! Unicode logical fonts retain the original face for the common font matcher.
//! A/W LOGFONTs share all 28 scalar bytes and one GDI object lifetime.
use super::super::add_logfont_with_face;
use crate::{Win32, util::set_last_error};
use adv_abi::{Env, GuestPtr, Handle, Registry, guest::check_range, win32};
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
            "gdi32.dll!CreateFontIndirectW",
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
    Handle(add_logfont_with_face(env, raw, face))
}

pub(super) fn register(registry: &mut Registry) {
    registry.register(create_indirect_w_ABI);
}
