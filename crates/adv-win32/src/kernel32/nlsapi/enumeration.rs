//! Guest callbacks over the same installed locales and Gregorian profile data.
use super::locales::{self, LOCALES, NEUTRAL_LOCALES};
use adv_abi::Registry;
use adv_cpu::backend::{Prot, Reg};
use adv_machine::{Env, Outcome};
use std::collections::VecDeque;

struct Enumeration {
    rows: VecDeque<(String, u32)>,
    callback: u32,
    scratch: u32,
    wide: bool,
    extended: bool,
    param: u32,
    codepage: u32,
}
fn next(env: &mut Env<'_>, mut state: Enumeration) -> Outcome {
    let Some((row, flags)) = state.rows.pop_front() else {
        return Outcome::Return(1);
    };
    let bytes = if state.wide {
        row.encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>()
    } else {
        let mut bytes = env
            .ext
            .get::<crate::Win32>()
            .codec
            .from_utf16(state.codepage, &row.encode_utf16().collect::<Vec<_>>())
            .0;
        bytes.push(0);
        bytes
    };
    if env.cpu.write(state.scratch, &bytes).is_err() {
        return locales::fail(env, 998);
    }
    let mut args = vec![state.scratch];
    if state.extended {
        args.extend([flags, state.param]);
    }
    Outcome::call_on_stack(state.callback, args, state.scratch, move |env, result| {
        if result.value() == 0 {
            Outcome::Return(1)
        } else {
            next(env, state)
        }
    })
}
fn start(
    env: &mut Env<'_>,
    rows: Vec<(String, u32)>,
    wide: bool,
    extended: bool,
    codepage: u32,
) -> Outcome {
    let callback = env.arg(0);
    if callback == 0 {
        return locales::fail(env, 87);
    }
    const BYTES: u32 = 512;
    let Some(scratch) = env
        .cpu
        .reg(Reg::Esp)
        .checked_sub(BYTES + 15)
        .map(|p| p & !15)
    else {
        return locales::fail(env, 8);
    };
    if scratch < env.ctx().stack_lo || !env.cpu.accessible(scratch, BYTES, Prot::RW) {
        return locales::fail(env, 8);
    }
    next(
        env,
        Enumeration {
            rows: rows.into(),
            callback,
            scratch,
            wide,
            extended,
            param: if extended { env.arg(2) } else { 0 },
            codepage,
        },
    )
}
fn system(env: &mut Env<'_>, wide: bool, extended: bool) -> Outcome {
    let flags = env.arg(1);
    if extended {
        if env.arg(3) != 0 {
            return locales::fail(env, 87);
        }
        if flags & !0x3f != 0 {
            return locales::fail(env, 1004);
        }
        let category = flags & 0xf;
        if category != 0 && category & 1 == 0 {
            return start(env, Vec::new(), true, true, 0);
        }
        let selection = flags & 0x30;
        let rows = NEUTRAL_LOCALES
            .iter()
            .map(|l| (l, 0x11))
            .chain(LOCALES.iter().filter(|l| l.id != 0x7f).map(|l| (l, 0x21)))
            .filter(|(_, kind)| selection == 0 || selection & kind & 0x30 != 0)
            .map(|(l, kind)| (l.name.to_owned(), kind))
            .collect();
        return start(env, rows, true, true, 0);
    }
    if !matches!(flags, 0..=2) {
        return locales::fail(env, 1004);
    }
    let rows = LOCALES
        .iter()
        .filter(|l| l.id != 0x7f)
        .map(|l| (format!("{:08X}", l.id), 0))
        .collect();
    start(env, rows, wide, false, 0)
}
fn calendar(env: &mut Env<'_>) -> Outcome {
    let locale = match locales::by_id(env, env.arg(1)) {
        Ok(locale) => locale,
        Err(error) => return locales::fail(env, error),
    };
    if env.arg(2) != 1 {
        adv_abi::warn_unavailable(
            env,
            "kernel32.dll!EnumCalendarInfoA",
            "non-Gregorian calendar enumeration has no profile data",
        );
        return locales::fail(env, 87);
    }
    let kind = env.arg(3);
    if kind & !0x800000ff != 0 {
        return locales::fail(env, 1004);
    }
    let text = match kind & 0xff {
        1 => "1".into(),
        2 => if locale.id == 0x411 {
            "西暦 (日本語)"
        } else if locale.id == 0x804 {
            "公历"
        } else {
            "Gregorian Calendar"
        }
        .into(),
        3 => "0".into(),
        4 => if locale.id == 0x411 {
            "西暦"
        } else if locale.id == 0x804 {
            "公元"
        } else {
            "A.D."
        }
        .into(),
        5 => locales::info(locale, 0x1f).expect("short date"),
        6 => locales::info(locale, 0x20).expect("long date"),
        7..=0xd => locales::day_name(locale, ((kind & 0xff) - 6) as usize, false),
        0xe..=0x14 => locales::day_name(locale, ((kind & 0xff) - 0xd) as usize, true),
        0x15..=0x20 => locales::month_name(locale, (kind & 0xff) - 0x14, false),
        0x22..=0x2d => locales::month_name(locale, (kind & 0xff) - 0x21, true),
        0x21 | 0x2e => String::new(),
        0x2f => locales::info(locale, 0x1006).expect("year month"),
        0x30 => "2029".into(),
        _ => {
            adv_abi::warn_unavailable(
                env,
                "kernel32.dll!EnumCalendarInfoA",
                "calendar information field has no profile data",
            );
            return locales::fail(env, 1004);
        }
    };
    start(env, vec![(text, 0)], false, false, locale.cp)
}
pub(super) fn register(registry: &mut Registry) {
    crate::stdcall(registry, super::K32, "EnumSystemLocalesA", 2, |e| {
        system(e, false, false)
    });
    crate::stdcall(registry, super::K32, "EnumSystemLocalesW", 2, |e| {
        system(e, true, false)
    });
    crate::stdcall(registry, super::K32, "EnumSystemLocalesEx", 4, |e| {
        system(e, true, true)
    });
    crate::stdcall(registry, super::K32, "EnumCalendarInfoA", 4, calendar);
}
