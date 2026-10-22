//! Locale names and information share the portable profile's locale data.
use crate::{Win32, util::set_last_error};
use adv_abi::{GuestPtr, Registry, WideStr, guest::check_range};
use adv_cpu::backend::{GuestExt, Prot};
use adv_machine::{Env, Outcome};

#[derive(Clone, Copy)]
pub(crate) struct Locale {
    pub id: u32,
    pub name: &'static str,
    pub cp: u32,
}
pub(in crate::kernel32) const LOCALES: [Locale; 4] = [
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
pub(super) const NEUTRAL_LOCALES: [Locale; 4] = [
    Locale {
        id: 9,
        name: "en",
        cp: 1252,
    },
    Locale {
        id: 0x11,
        name: "ja",
        cp: 932,
    },
    Locale {
        id: 0x7804,
        name: "zh",
        cp: 936,
    },
    Locale {
        id: 4,
        name: "zh-Hans",
        cp: 936,
    },
];
pub(in crate::kernel32) fn fail(env: &mut Env<'_>, error: u32) -> Outcome {
    set_last_error(env, error);
    Outcome::Return(0)
}
pub(crate) fn by_id(env: &Env<'_>, id: u32) -> Result<Locale, u32> {
    let id = if matches!(id, 0 | 0x400 | 0x800 | 0xc00 | 0x1400) {
        super::lcid(env)
    } else {
        id
    };
    LOCALES
        .iter()
        .chain(NEUTRAL_LOCALES.iter())
        .find(|l| l.id == id)
        .copied()
        .ok_or(87)
}
pub(in crate::kernel32) fn from_name(env: &Env<'_>, pointer: u32, flags: u32) -> Result<u32, u32> {
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
    for (neutral, specific, id) in [
        ("en", 0x409, 9),
        ("ja", 0x411, 0x11),
        ("zh", 0x804, 0x7804),
        ("zh-Hans", 0x804, 4),
    ] {
        if name.eq_ignore_ascii_case(neutral) {
            return Ok(if flags == 0 { specific } else { id });
        }
    }
    Err(87)
}
pub(in crate::kernel32) fn put(
    env: &mut Env<'_>,
    text: &str,
    output: u32,
    capacity: u32,
    wide: bool,
) -> Outcome {
    put_with_cp(env, text, output, capacity, wide, 0)
}
fn put_with_cp(
    env: &mut Env<'_>,
    text: &str,
    output: u32,
    capacity: u32,
    wide: bool,
    codepage: u32,
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
        let mut bytes = env
            .ext
            .get::<Win32>()
            .codec
            .from_utf16(codepage, &text.encode_utf16().collect::<Vec<_>>())
            .0;
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
pub(in crate::kernel32) fn day_name(locale: Locale, day: usize, short: bool) -> String {
    let day = day % 7;
    match locale.id & 0x3ff {
        0x11 => {
            let text = [
                "日曜日",
                "月曜日",
                "火曜日",
                "水曜日",
                "木曜日",
                "金曜日",
                "土曜日",
            ][day];
            if short {
                text.chars().take(1).collect()
            } else {
                text.into()
            }
        }
        4 => {
            let text = [
                "星期日",
                "星期一",
                "星期二",
                "星期三",
                "星期四",
                "星期五",
                "星期六",
            ][day];
            if short {
                text.replace("星期", "周")
            } else {
                text.into()
            }
        }
        _ => {
            let text = [
                "Sunday",
                "Monday",
                "Tuesday",
                "Wednesday",
                "Thursday",
                "Friday",
                "Saturday",
            ][day];
            if short { text[..3].into() } else { text.into() }
        }
    }
}
pub(in crate::kernel32) fn month_name(locale: Locale, month: u32, short: bool) -> String {
    if !(1..=12).contains(&month) {
        return String::new();
    }
    match (locale.id & 0x3ff, short) {
        (0x11, true) => month.to_string(),
        (0x11, false) | (4, true) => format!("{month}月"),
        (4, false) => format!(
            "{}月",
            [
                "一", "二", "三", "四", "五", "六", "七", "八", "九", "十", "十一", "十二"
            ][month as usize - 1]
        ),
        _ => {
            let text = [
                "January",
                "February",
                "March",
                "April",
                "May",
                "June",
                "July",
                "August",
                "September",
                "October",
                "November",
                "December",
            ][month as usize - 1];
            if short { text[..3].into() } else { text.into() }
        }
    }
}
pub(crate) fn info(locale: Locale, kind: u32) -> Option<String> {
    let index = match locale.id {
        0x409 | 9 => 0,
        0x411 | 0x11 => 1,
        0x804 | 4 | 0x7804 => 2,
        0x7f => 3,
        _ => return None,
    };
    match kind {
        0x2a..=0x30 => return Some(day_name(locale, ((kind - 0x2a + 1) % 7) as usize, false)),
        0x31..=0x37 => return Some(day_name(locale, ((kind - 0x31 + 1) % 7) as usize, true)),
        0x38..=0x43 => return Some(month_name(locale, kind - 0x38 + 1, false)),
        0x44..=0x4f => return Some(month_name(locale, kind - 0x44 + 1, true)),
        _ => (),
    }
    let values = match kind {
        1 => return Some(format!("{:04x}", locale.id)),
        3 => ["ENU", "JPN", "CHS", "IVL"],
        4 => ["English", "日本語", "中文(简体)", "Invariant Language"],
        5 => ["1", "81", "86", "1"],
        7 => ["USA", "JPN", "CHN", ""],
        8 => ["United States", "日本", "中国", "Invariant Country"],
        0xb => ["437", "932", "936", "437"],
        0x1004 => return Some(locale.cp.to_string()),
        0xc => [","; 4],
        0xd => ["1", "0", "0", "0"],
        0xe | 0x16 => ["."; 4],
        0xf | 0x17 => [","; 4],
        0x10 | 0x18 => ["3;0"; 4],
        0x11 => ["2"; 4],
        0x12 => ["1"; 4],
        0x13 => ["0123456789"; 4],
        0x14 => ["$", "¥", "¥", "¤"],
        0x15 => ["USD", "JPY", "CNY", "XDR"],
        0x19 | 0x1a => ["2", "0", "2", "2"],
        0x1d => ["/"; 4],
        0x1e => [":"; 4],
        0x1f => ["M/d/yyyy", "yyyy/MM/dd", "yyyy/M/d", "MM/dd/yyyy"],
        0x20 => [
            "dddd, MMMM dd, yyyy",
            "yyyy'年'M'月'd'日'",
            "yyyy'年'M'月'd'日'",
            "dddd, dd MMMM yyyy",
        ],
        0x21 => ["0", "2", "2", "0"],
        0x23 => ["0", "1", "1", "1"],
        0x28 => ["AM", "午前", "上午", "AM"],
        0x29 => ["PM", "午後", "下午", "PM"],
        0x1001 => [
            "English",
            "Japanese",
            "Chinese (Simplified)",
            "Invariant Language",
        ],
        0x1002 => ["United States", "Japan", "China", "Invariant Country"],
        0x1005 => ["0"; 4],
        0x100c => ["6", "6", "0", "6"],
        0x1003 => ["h:mm:ss tt", "H:mm:ss", "H:mm:ss", "HH:mm:ss"],
        0x1006 => ["MMMM yyyy", "yyyy'年'M'月'", "yyyy'年'M'月'", "yyyy MMMM"],
        0x1009 => ["1"; 4],
        0x59 => ["en", "ja", "zh", "iv"],
        0x5a => ["US", "JP", "CN", "IV"],
        0x5c => return Some(locale.name.into()),
        _ => return None,
    };
    Some(values[index].into())
}
pub(in crate::kernel32) fn query(env: &mut Env<'_>, wide: bool, named: bool) -> Outcome {
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
        return put_with_cp(
            env,
            &text,
            env.arg(2),
            env.arg(3),
            wide,
            if kind & 0x40000000 != 0 { 0 } else { locale.cp },
        );
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
pub(in crate::kernel32) fn register(reg: &mut Registry) {
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
            match by_id(
                e,
                if e.arg(3) == 0 {
                    match e.arg(0) {
                        9 => 0x409,
                        0x11 => 0x411,
                        4 | 0x7804 => 0x804,
                        id => id,
                    }
                } else {
                    e.arg(0)
                },
            ) {
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
