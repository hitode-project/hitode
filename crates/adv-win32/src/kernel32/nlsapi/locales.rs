//! Locale names and information share the portable profile's locale data.
use crate::{Win32, util::set_last_error};
use adv_abi::{GuestPtr, Registry, WideStr, guest::check_range};
use adv_cpu::backend::{GuestExt, Prot};
use adv_machine::{Env, Outcome};

#[derive(Clone, Copy)]
pub(super) struct Locale {
    pub id: u32,
    pub name: &'static str,
    pub cp: u32,
}
pub(super) const LOCALES: [Locale; 4] = [
    Locale {
        id: 0x409,
        name: "en-US",
        cp: 1252,
    },
    Locale {
        id: 0x411,
        name: "ja-JP",
        cp: 932,
    },
    Locale {
        id: 0x804,
        name: "zh-CN",
        cp: 936,
    },
    Locale {
        id: 0x7f,
        name: "",
        cp: 1252,
    },
];
pub(super) fn fail(env: &mut Env<'_>, error: u32) -> Outcome {
    set_last_error(env, error);
    Outcome::Return(0)
}
pub(super) fn by_id(env: &Env<'_>, id: u32) -> Result<Locale, u32> {
    let id = if matches!(id, 0 | 0x400 | 0x800 | 0xc00 | 0x1400) {
        super::lcid(env)
    } else {
        id
    };
    LOCALES.iter().find(|l| l.id == id).copied().ok_or(87)
}
pub(super) fn from_name(env: &Env<'_>, pointer: u32, flags: u32) -> Result<u32, u32> {
    if flags & !0x08000000 != 0 {
        return Err(1004);
    }
    if pointer == 0 {
        return Ok(super::lcid(env));
    }
    let units = WideStr(GuestPtr::new(pointer))
        .read(env.cpu, 85)
        .map_err(|_| 87u32)?;
    let name = String::from_utf16(&units).map_err(|_| 87u32)?;
    if name == "!x-sys-default-locale" {
        return Ok(super::lcid(env));
    }
    if let Some(locale) = LOCALES
        .iter()
        .find(|locale| name.eq_ignore_ascii_case(locale.name))
    {
        return Ok(locale.id);
    }
    for (neutral, specific, id) in [("en", 0x409, 9), ("ja", 0x411, 0x11), ("zh", 0x804, 4)] {
        if name.eq_ignore_ascii_case(neutral) {
            return Ok(if flags == 0 { specific } else { id });
        }
    }
    Err(87)
}
pub(super) fn put(
    env: &mut Env<'_>,
    text: &str,
    output: u32,
    capacity: u32,
    wide: bool,
) -> Outcome {
    if (capacity as i32) < 0 {
        return fail(env, 87);
    }
    let bytes = if wide {
        text.encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>()
    } else {
        let mut bytes = env.ext.get::<Win32>().codec.encode(text);
        bytes.push(0);
        bytes
    };
    let needed = bytes.len() as u32 / if wide { 2 } else { 1 };
    if capacity == 0 {
        return Outcome::Return(needed);
    }
    if capacity < needed {
        return fail(env, 122);
    }
    if check_range(env.cpu, output, bytes.len() as u32, Prot::WRITE).is_err() {
        return fail(env, 87);
    }
    let _ = env.cpu.write(output, &bytes);
    Outcome::Return(needed)
}
pub(super) fn info(locale: Locale, kind: u32) -> Option<String> {
    let ja = locale.id == 0x411;
    let zh = locale.id == 0x804;
    Some(match kind {
        1 => format!("{:04x}", locale.id),
        2 | 0x6f => if ja {
            "Japanese (Japan)"
        } else if zh {
            "Chinese (Simplified, PRC)"
        } else {
            "English (United States)"
        }
        .into(),
        3 => if ja {
            "JPN"
        } else if zh {
            "CHS"
        } else {
            "ENU"
        }
        .into(),
        4 | 0x1002 => if ja {
            "日本語"
        } else if zh {
            "中文(简体)"
        } else {
            "English"
        }
        .into(),
        5 => if ja {
            "81"
        } else if zh {
            "86"
        } else {
            "1"
        }
        .into(),
        6 | 0x1003 => if ja {
            "Japan"
        } else if zh {
            "China"
        } else {
            "United States"
        }
        .into(),
        7 => if ja {
            "JPN"
        } else if zh {
            "CHN"
        } else {
            "USA"
        }
        .into(),
        8 => if ja {
            "日本"
        } else if zh {
            "中国"
        } else {
            "United States"
        }
        .into(),
        0xb | 0x1004 => locale.cp.to_string(),
        0xc => ",".into(),
        0xd => "0".into(),
        0xe | 0x16 => ".".into(),
        0xf | 0x17 => ",".into(),
        0x10 | 0x18 => "3;0".into(),
        0x11 => "2".into(),
        0x12 => "1".into(),
        0x13 => "0123456789".into(),
        0x14 => if ja {
            "¥"
        } else if zh {
            "￥"
        } else {
            "$"
        }
        .into(),
        0x15 => if ja {
            "JPY"
        } else if zh {
            "CNY"
        } else {
            "USD"
        }
        .into(),
        0x19 | 0x1a => if ja { "0" } else { "2" }.into(),
        0x1d => "/".into(),
        0x1e => ":".into(),
        0x1f => if ja {
            "yyyy/MM/dd"
        } else if zh {
            "yyyy/M/d"
        } else {
            "M/d/yyyy"
        }
        .into(),
        0x20 => if ja || zh {
            "yyyy'年'M'月'd'日'"
        } else {
            "dddd, MMMM dd, yyyy"
        }
        .into(),
        0x21 => if ja || zh { "2" } else { "0" }.into(),
        0x23 => if ja || zh { "1" } else { "0" }.into(),
        0x28 => if ja {
            "午前"
        } else if zh {
            "上午"
        } else {
            "AM"
        }
        .into(),
        0x29 => if ja {
            "午後"
        } else if zh {
            "下午"
        } else {
            "PM"
        }
        .into(),
        0x1005 => if ja || zh { "H:mm:ss" } else { "h:mm:ss tt" }.into(),
        0x1009 => "1".into(),
        0x100c => "1".into(),
        0x59 => if ja {
            "ja"
        } else if zh {
            "zh"
        } else {
            "en"
        }
        .into(),
        0x5a => if ja {
            "JP"
        } else if zh {
            "CN"
        } else {
            "US"
        }
        .into(),
        0x5c => locale.name.into(),
        _ => return None,
    })
}
pub(super) fn query(env: &mut Env<'_>, wide: bool, named: bool) -> Outcome {
    let locale = if named {
        from_name(env, env.arg(0), 0).and_then(|id| by_id(env, id))
    } else {
        by_id(env, env.arg(0))
    };
    let locale = match locale {
        Ok(locale) => locale,
        Err(error) => return fail(env, error),
    };
    let kind = env.arg(1);
    if kind & !(0xffff | 0xe8000000) != 0 {
        return fail(env, 1004);
    }
    let Some(text) = info(locale, kind & 0xffff) else {
        adv_abi::warn_unavailable(
            env,
            "kernel32.dll!GetLocaleInfo",
            "locale information field has no portable profile data",
        );
        return fail(env, 1004);
    };
    if kind & 0x20000000 == 0 {
        return put(env, &text, env.arg(2), env.arg(3), wide);
    }
    let Ok(number) = u32::from_str_radix(&text, if kind & 0xffff == 1 { 16 } else { 10 }) else {
        return fail(env, 1004);
    };
    let needed = if wide { 2 } else { 4 };
    if env.arg(3) == 0 {
        return Outcome::Return(needed);
    }
    if env.arg(3) < needed {
        return fail(env, 122);
    }
    if check_range(env.cpu, env.arg(2), 4, Prot::WRITE).is_err() {
        return fail(env, 87);
    }
    let _ = env.cpu.write_u32(env.arg(2), number);
    Outcome::Return(needed)
}
pub(super) fn register(reg: &mut Registry) {
    for (name, count, handler) in [
        (
            "GetLocaleInfoEx",
            4,
            (|e: &mut Env<'_>| query(e, true, true)) as adv_machine::Handler,
        ),
        ("LocaleNameToLCID", 2, |e| {
            match from_name(e, e.arg(0), e.arg(1)) {
                Ok(id) => Outcome::Return(id),
                Err(error) => fail(e, error),
            }
        }),
        ("LCIDToLocaleName", 4, |e| {
            if e.arg(3) & !0x08000000 != 0 {
                return fail(e, 1004);
            }
            match by_id(e, e.arg(0)) {
                Ok(locale) => put(e, locale.name, e.arg(1), e.arg(2), true),
                Err(error) => fail(e, error),
            }
        }),
        ("GetUserDefaultLocaleName", 2, |e| {
            let locale = by_id(e, super::lcid(e)).expect("profile locale");
            put(e, locale.name, e.arg(0), e.arg(1), true)
        }),
        ("IsValidLocaleName", 1, |e| {
            Outcome::Return(u32::from(from_name(e, e.arg(0), 0x08000000).is_ok()))
        }),
    ] {
        crate::stdcall(reg, super::K32, name, count, handler);
    }
}
#[cfg(test)]
mod tests;
