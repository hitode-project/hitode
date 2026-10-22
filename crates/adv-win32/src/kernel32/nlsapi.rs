//! Code page and locale functions.

use adv_abi::{Registry, win32};
use adv_cpu::backend::GuestExt;
use adv_machine::{Env, Outcome};

use super::K32;
use crate::util::{bytes_or_cstr, err, out_u32, set_last_error, units_or_wstr, write_units, zero};
use crate::{Win32, stdcall};
use adv_os::nls::{is_valid_codepage, lead_byte_ranges};

mod character_types;
mod collation;
mod enumeration;
pub(in crate::kernel32) mod locales;
mod preferred_ui;

/// Japanese (0x411) or Chinese PRC (0x804), following the code page.
pub(crate) fn lcid(env: &Env<'_>) -> u32 {
    if env.ext.get::<Win32>().codec.acp == 936 {
        0x804
    } else {
        0x411
    }
}

fn get_cp_info(env: &mut Env<'_>) -> Outcome {
    let (cp, p) = (env.arg(0), env.arg(1));
    let cp = env.ext.get::<Win32>().codec.resolve(cp);
    if !is_valid_codepage(cp) {
        set_last_error(env, err::INVALID_PARAMETER);
        return Outcome::Return(0);
    }
    zero(env.cpu, p, 18);
    let ranges = lead_byte_ranges(cp);
    let max = if ranges.is_empty() && cp != 65001 {
        1
    } else if cp == 65001 {
        4
    } else {
        2
    };
    let _ = env.cpu.write_u32(p, max);
    let _ = env.cpu.write(p + 4, b"?\0");
    let lead: Vec<u8> = ranges.iter().flat_map(|&(a, b)| [a, b]).collect();
    let _ = env.cpu.write(p + 6, &lead);
    Outcome::Return(1)
}

#[win32(kernel32, name = "IsDBCSLeadByte", level = implement, leaf)]
fn is_dbcs_lead_byte(env: &mut Env<'_>, b: u32) -> i32 {
    i32::from(env.ext.get::<Win32>().codec.is_lead_byte(0, b as u8))
}

#[win32(kernel32, name = "IsDBCSLeadByteEx", level = implement, leaf)]
fn is_dbcs_lead_byte_ex(env: &mut Env<'_>, cp: u32, b: u32) -> i32 {
    i32::from(env.ext.get::<Win32>().codec.is_lead_byte(cp, b as u8))
}

/// Shared tail of the A<->W converters: output size query, copy, or insufficient buffer.
fn deliver<T: Copy>(
    env: &mut Env<'_>,
    out: &[T],
    dst: u32,
    cap: u32,
    write: fn(&mut Env<'_>, u32, &[T]),
) -> Outcome {
    if cap == 0 {
        return Outcome::Return(out.len() as u32);
    }
    if out.len() > cap as usize {
        set_last_error(env, err::INSUFFICIENT_BUFFER);
        return Outcome::Return(0);
    }
    write(env, dst, out);
    Outcome::Return(out.len() as u32)
}

fn multi_byte_to_wide_char(env: &mut Env<'_>) -> Outcome {
    let (cp, _flags, src, len, dst, cap) = (
        env.arg(0),
        env.arg(1),
        env.arg(2),
        env.arg(3) as i32,
        env.arg(4),
        env.arg(5),
    );
    let mut bytes = bytes_or_cstr(env.cpu, src, len);
    if len < 0 {
        bytes.push(0);
    }
    let units = env.ext.get::<Win32>().codec.to_utf16(cp, &bytes);
    deliver(env, &units, dst, cap, |e, p, v| write_units(e.cpu, p, v))
}

fn wide_char_to_multi_byte(env: &mut Env<'_>) -> Outcome {
    let (cp, _flags, src, len, dst, cap, _defchar, used) = (
        env.arg(0),
        env.arg(1),
        env.arg(2),
        env.arg(3) as i32,
        env.arg(4),
        env.arg(5),
        env.arg(6),
        env.arg(7),
    );
    let mut units = units_or_wstr(env.cpu, src, len);
    if len < 0 {
        units.push(0);
    }
    let (bytes, lossy) = env.ext.get::<Win32>().codec.from_utf16(cp, &units);
    out_u32(env.cpu, used, u32::from(lossy));
    deliver(env, &bytes, dst, cap, |e, p, v| {
        let _ = e.cpu.write(p, v);
    })
}

const LCMAP_LOWERCASE: u32 = 0x100;
const LCMAP_UPPERCASE: u32 = 0x200;
const LCMAP_SORTKEY: u32 = 0x400;

fn lcmap(flags: u32, units: &[u16]) -> Vec<u16> {
    let s = String::from_utf16_lossy(units);
    let mapped = if flags & LCMAP_UPPERCASE != 0 {
        s.to_uppercase()
    } else if flags & LCMAP_LOWERCASE != 0 {
        s.to_lowercase()
    } else {
        s
    };
    let out: Vec<u16> = mapped.encode_utf16().collect();
    // Case mapping must not change lengths the caller computed; fall back per unit if it did.
    if out.len() == units.len() {
        out
    } else {
        units.to_vec()
    }
}

fn lc_map_string_w(env: &mut Env<'_>) -> Outcome {
    let (_lcid, flags, src, len, dst, cap) = (
        env.arg(0),
        env.arg(1),
        env.arg(2),
        env.arg(3) as i32,
        env.arg(4),
        env.arg(5),
    );
    let mut units = units_or_wstr(env.cpu, src, len);
    if flags & LCMAP_SORTKEY != 0 {
        // A byte key that sorts like the (case-folded) text: big-endian units, then 0.
        let key: Vec<u8> = lcmap(LCMAP_UPPERCASE, &units)
            .iter()
            .flat_map(|u| u.to_be_bytes())
            .chain([1, 1, 1, 1, 0])
            .collect();
        return deliver(env, &key, dst, cap, |e, p, v| {
            let _ = e.cpu.write(p, v);
        });
    }
    if len < 0 {
        units.push(0);
    }
    let out = lcmap(flags, &units);
    deliver(env, &out, dst, cap, |e, p, v| write_units(e.cpu, p, v))
}

fn lc_map_string_a(env: &mut Env<'_>) -> Outcome {
    let (_lcid, flags, src, len, dst, cap) = (
        env.arg(0),
        env.arg(1),
        env.arg(2),
        env.arg(3) as i32,
        env.arg(4),
        env.arg(5),
    );
    let mut bytes = bytes_or_cstr(env.cpu, src, len);
    if len < 0 {
        bytes.push(0);
    }
    let codec = env.ext.get::<Win32>().codec;
    let out = if flags & (LCMAP_UPPERCASE | LCMAP_LOWERCASE) != 0 {
        // Map only ASCII letters; DBCS trail bytes must not be touched.
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            let b = bytes[i];
            if codec.is_lead_byte(0, b) && i + 1 < bytes.len() {
                out.extend_from_slice(&bytes[i..i + 2]);
                i += 2;
                continue;
            }
            out.push(if flags & LCMAP_UPPERCASE != 0 {
                b.to_ascii_uppercase()
            } else {
                b.to_ascii_lowercase()
            });
            i += 1;
        }
        out
    } else {
        bytes
    };
    deliver(env, &out, dst, cap, |e, p, v| {
        let _ = e.cpu.write(p, v);
    })
}

pub(super) fn compare(env: &mut Env<'_>, locale: u32, flags: u32, a: &[u16], b: &[u16]) -> u32 {
    let locale = if matches!(locale, 0 | 0x400 | 0x800 | 0xc00 | 0x1000 | 0x1400) {
        lcid(env)
    } else {
        locale
    };
    match collation::compare(locale, flags, a, b) {
        Ok(value) => value,
        Err(error) => {
            set_last_error(env, error);
            0
        }
    }
}

pub(super) fn decode_comparison(env: &Env<'_>, bytes: &[u8]) -> Result<Vec<u16>, u32> {
    let codec = env.ext.get::<Win32>().codec;
    collation::decode(codec, codec.acp, bytes)
}

fn compare_string_w(env: &mut Env<'_>) -> Outcome {
    let locale = env.arg(0);
    let flags = env.arg(1);
    let a = units_or_wstr(env.cpu, env.arg(2), env.arg(3) as i32);
    let b = units_or_wstr(env.cpu, env.arg(4), env.arg(5) as i32);
    Outcome::Return(compare(env, locale, flags, &a, &b))
}

fn compare_string_a(env: &mut Env<'_>) -> Outcome {
    let locale = env.arg(0);
    let flags = env.arg(1);
    let a_bytes = bytes_or_cstr(env.cpu, env.arg(2), env.arg(3) as i32);
    let b_bytes = bytes_or_cstr(env.cpu, env.arg(4), env.arg(5) as i32);
    let codec = env.ext.get::<Win32>().codec;
    let resolved = if matches!(locale, 0 | 0x400 | 0x800 | 0xc00 | 0x1000 | 0x1400) {
        lcid(env)
    } else {
        locale
    };
    let result = collation::compare_ansi(resolved, flags, codec, &a_bytes, &b_bytes);
    Outcome::Return(match result {
        Ok(value) => value,
        Err(error) => {
            set_last_error(env, error);
            0
        }
    })
}

fn compare_string_ex(env: &mut Env<'_>) -> Outcome {
    if env.arg(6) != 0 || env.arg(7) != 0 || env.arg(8) != 0 {
        set_last_error(env, err::INVALID_PARAMETER);
        return Outcome::Return(0);
    }
    let locale = if env.arg(0) == 0 {
        lcid(env)
    } else {
        let name = units_or_wstr(env.cpu, env.arg(0), -1);
        match collation::locale(&name, lcid(env)) {
            Ok(locale) => locale,
            Err(error) => {
                set_last_error(env, error);
                return Outcome::Return(0);
            }
        }
    };
    let a = units_or_wstr(env.cpu, env.arg(2), env.arg(3) as i32);
    let b = units_or_wstr(env.cpu, env.arg(4), env.arg(5) as i32);
    Outcome::Return(compare(env, locale, env.arg(1), &a, &b))
}

pub(super) fn register(reg: &mut Registry) {
    preferred_ui::register(reg);
    locales::register(reg);
    character_types::register(reg);
    enumeration::register(reg);
    stdcall(reg, K32, "CompareStringA", 6, compare_string_a);
    stdcall(reg, K32, "CompareStringW", 6, compare_string_w);
    // Same leading layout with a locale name; the version, reserved and
    // lParam arguments must be null and do not affect the comparison.
    stdcall(reg, K32, "CompareStringEx", 9, compare_string_ex);
    stdcall(reg, K32, "GetACP", 0, |e| {
        Outcome::Return(e.ext.get::<Win32>().codec.acp)
    });
    stdcall(reg, K32, "GetOEMCP", 0, |e| {
        Outcome::Return(e.ext.get::<Win32>().codec.acp)
    });
    stdcall(reg, K32, "IsValidCodePage", 1, |e| {
        Outcome::Return(u32::from(is_valid_codepage(e.arg(0))))
    });
    stdcall(reg, K32, "GetCPInfo", 2, get_cp_info);
    reg.register(is_dbcs_lead_byte_ABI);
    reg.register(is_dbcs_lead_byte_ex_ABI);
    stdcall(reg, K32, "MultiByteToWideChar", 6, multi_byte_to_wide_char);
    stdcall(reg, K32, "WideCharToMultiByte", 8, wide_char_to_multi_byte);
    stdcall(reg, K32, "LCMapStringW", 6, lc_map_string_w);
    stdcall(reg, K32, "LCMapStringA", 6, lc_map_string_a);
    // The locale name replaces the LCID; the mapping itself ignores both, and
    // the trailing version, reserved and sort-handle arguments are unused.
    stdcall(reg, K32, "LCMapStringEx", 9, lc_map_string_w);
    stdcall(reg, K32, "GetLocaleInfoA", 4, |e| {
        locales::query(e, false, false)
    });
    stdcall(reg, K32, "GetLocaleInfoW", 4, |e| {
        locales::query(e, true, false)
    });
    for n in [
        "GetUserDefaultLCID",
        "GetSystemDefaultLCID",
        "GetThreadLocale",
        "GetUserDefaultLangID",
        "GetSystemDefaultLangID",
        "GetUserDefaultUILanguage",
        "GetSystemDefaultUILanguage",
    ] {
        stdcall(reg, K32, n, 0, |e| Outcome::Return(lcid(e)));
    }
    stdcall(reg, K32, "IsValidLocale", 2, |_| Outcome::Return(1));
    stdcall(reg, K32, "SetThreadLocale", 1, |_| Outcome::Return(1));
}
