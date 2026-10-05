//! Code page and locale functions.

use adv_abi::Registry;
use adv_cpu::backend::GuestExt;
use adv_machine::{Env, Outcome};

use super::K32;
use crate::util::{
    bytes_or_cstr, err, out_u32, put_cstr, put_wstr, set_last_error, units_or_wstr, write_units,
    zero,
};
use crate::{Win32, stdcall};
use adv_os::nls::{ctype1, is_valid_codepage, lead_byte_ranges};

/// Japanese (0x411) or Chinese PRC (0x804), following the code page.
fn lcid(env: &Env<'_>) -> u32 {
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

fn is_dbcs_lead_byte(env: &mut Env<'_>) -> Outcome {
    let b = env.arg(0) as u8;
    Outcome::Return(u32::from(env.ext.get::<Win32>().codec.is_lead_byte(0, b)))
}

fn is_dbcs_lead_byte_ex(env: &mut Env<'_>) -> Outcome {
    let (cp, b) = (env.arg(0), env.arg(1) as u8);
    Outcome::Return(u32::from(env.ext.get::<Win32>().codec.is_lead_byte(cp, b)))
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

fn get_string_type_w(env: &mut Env<'_>) -> Outcome {
    let (ty, src, len, out) = (env.arg(0), env.arg(1), env.arg(2) as i32, env.arg(3));
    let units = units_or_wstr(env.cpu, src, len);
    let types: Vec<u16> = match ty {
        1 => units.iter().map(|&u| ctype1(u)).collect(),
        _ => vec![0; units.len()],
    };
    write_units(env.cpu, out, &types);
    Outcome::Return(1)
}

fn get_string_type_a(env: &mut Env<'_>) -> Outcome {
    let (_lcid, ty, src, len, out) = (
        env.arg(0),
        env.arg(1),
        env.arg(2),
        env.arg(3) as i32,
        env.arg(4),
    );
    let bytes = bytes_or_cstr(env.cpu, src, len);
    let codec = env.ext.get::<Win32>().codec;
    // One entry per byte; a DBCS character's type goes to both its bytes.
    let mut types = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let n = if codec.is_lead_byte(0, bytes[i]) && i + 1 < bytes.len() {
            2
        } else {
            1
        };
        let u = codec
            .to_utf16(0, &bytes[i..i + n])
            .first()
            .copied()
            .unwrap_or(0);
        let t = if ty == 1 { ctype1(u) } else { 0 };
        types.extend(std::iter::repeat_n(t, n));
        i += n;
    }
    write_units(env.cpu, out, &types);
    Outcome::Return(1)
}

/// A few LOCALE_* values the CRT and games ask for.
fn locale_info(lcid: u32, ty: u32) -> Option<String> {
    let ja = lcid != 0x804;
    Some(match ty & 0xffff {
        0x1 => format!("{lcid:04x}"),                        // LOCALE_ILANGUAGE
        0x3 | 0x59 => if ja { "JPN" } else { "CHS" }.into(), // LOCALE_SABBREVLANGNAME
        0x5 => if ja { "81" } else { "86" }.into(),          // LOCALE_ICOUNTRY
        0x7 | 0x5a => if ja { "JPN" } else { "CHN" }.into(), // LOCALE_SABBREVCTRYNAME
        0xb => if ja { "932" } else { "936" }.into(), // LOCALE_IDEFAULTANSICODEPAGE (0x1004 masked)
        0xe => ".".into(),                            // LOCALE_SDECIMAL
        0xf => ",".into(),                            // LOCALE_STHOUSAND
        0x1d => "/".into(),                           // LOCALE_SDATE
        0x1e => ":".into(),                           // LOCALE_STIME
        0x1004 => if ja { "932" } else { "936" }.into(),
        _ => return None,
    })
}

fn get_locale_info(env: &mut Env<'_>, wide: bool) -> Outcome {
    let (lcid_arg, ty, buf, cap) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3));
    let l = if lcid_arg == 0x400 || lcid_arg == 0x800 {
        lcid(env)
    } else {
        lcid_arg
    };
    let Some(v) = locale_info(l, ty) else {
        log::info!("GetLocaleInfo({l:#x}, {ty:#x}): unsupported");
        set_last_error(env, err::INVALID_PARAMETER);
        return Outcome::Return(0);
    };
    let n = if wide {
        v.encode_utf16().count()
    } else {
        v.len()
    } as u32
        + 1;
    if cap == 0 {
        return Outcome::Return(n);
    }
    let ok = if wide {
        put_wstr(env.cpu, buf, cap, &v.encode_utf16().collect::<Vec<_>>())
    } else {
        put_cstr(env.cpu, buf, cap, v.as_bytes())
    };
    if ok.is_none() {
        set_last_error(env, err::INSUFFICIENT_BUFFER);
        return Outcome::Return(0);
    }
    Outcome::Return(n)
}

/// CompareStringW(lcid, flags, s1, n1, s2, n2): code-unit order, NORM_IGNORECASE folding
/// through `char::to_lowercase` for single-unit results. CSTR_LESS_THAN/EQUAL/GREATER_THAN.
fn compare_string_w(env: &mut Env<'_>) -> Outcome {
    const NORM_IGNORECASE: u32 = 1;
    let flags = env.arg(1);
    let a = units_or_wstr(env.cpu, env.arg(2), env.arg(3) as i32);
    let b = units_or_wstr(env.cpu, env.arg(4), env.arg(5) as i32);
    let fold = |u: u16| {
        if flags & NORM_IGNORECASE == 0 {
            return u;
        }
        char::from_u32(u32::from(u))
            .and_then(|c| {
                let mut l = c.to_lowercase();
                match (l.next(), l.next()) {
                    (Some(x), None) if (x as u32) < 0x1_0000 => Some(x as u16),
                    _ => None,
                }
            })
            .unwrap_or(u)
    };
    let ord = a.iter().map(|&u| fold(u)).cmp(b.iter().map(|&u| fold(u)));
    Outcome::Return((ord as i32 + 2) as u32)
}

fn compare_string_a(env: &mut Env<'_>) -> Outcome {
    const NORM_IGNORECASE: u32 = 1;
    let flags = env.arg(1);
    let a_bytes = bytes_or_cstr(env.cpu, env.arg(2), env.arg(3) as i32);
    let b_bytes = bytes_or_cstr(env.cpu, env.arg(4), env.arg(5) as i32);
    let codec = env.ext.get::<Win32>().codec;
    let a = codec.to_utf16(0, &a_bytes);
    let b = codec.to_utf16(0, &b_bytes);
    let fold = |u: u16| {
        if flags & NORM_IGNORECASE == 0 {
            return u;
        }
        char::from_u32(u32::from(u))
            .and_then(|c| {
                let mut l = c.to_lowercase();
                match (l.next(), l.next()) {
                    (Some(x), None) if (x as u32) < 0x1_0000 => Some(x as u16),
                    _ => None,
                }
            })
            .unwrap_or(u)
    };
    let ord = a.iter().map(|&u| fold(u)).cmp(b.iter().map(|&u| fold(u)));
    Outcome::Return((ord as i32 + 2) as u32)
}

pub(super) fn register(reg: &mut Registry) {
    stdcall(reg, K32, "CompareStringA", 6, compare_string_a);
    stdcall(reg, K32, "CompareStringW", 6, compare_string_w);
    // Enumerates nothing: callers probing for a locale name find none and use their default.
    stdcall(reg, K32, "EnumSystemLocalesW", 2, |_| Outcome::Return(1));
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
    stdcall(reg, K32, "IsDBCSLeadByte", 1, is_dbcs_lead_byte);
    stdcall(reg, K32, "IsDBCSLeadByteEx", 2, is_dbcs_lead_byte_ex);
    stdcall(reg, K32, "MultiByteToWideChar", 6, multi_byte_to_wide_char);
    stdcall(reg, K32, "WideCharToMultiByte", 8, wide_char_to_multi_byte);
    stdcall(reg, K32, "LCMapStringW", 6, lc_map_string_w);
    stdcall(reg, K32, "LCMapStringA", 6, lc_map_string_a);
    stdcall(reg, K32, "GetStringTypeW", 4, get_string_type_w);
    stdcall(reg, K32, "GetStringTypeA", 5, get_string_type_a);
    stdcall(reg, K32, "GetLocaleInfoA", 4, |e| get_locale_info(e, false));
    stdcall(reg, K32, "GetLocaleInfoW", 4, |e| get_locale_info(e, true));
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
