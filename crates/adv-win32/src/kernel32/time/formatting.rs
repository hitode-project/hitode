//! Date and time pictures use the same locale data as GetLocaleInfo and enumeration.
use super::*;
use crate::kernel32::nlsapi::locales::{self, Locale};
use adv_abi::{AnsiStr, WideStr, guest::check_range};
use adv_cpu::backend::Prot;
/// Expand a GetDateFormat/GetTimeFormat picture over SYSTEMTIME `st` (year, month, weekday,
/// day, hour, minute, second, ms), using localized Gregorian names.
pub(crate) fn format_picture(pic: &[u16], st: [u16; 8], locale: Locale, time: bool) -> Vec<u16> {
    let [year, month, wday, day, hour, minute, second, _] = st.map(u32::from);
    let mut out = String::new();
    let mut i = 0;
    while i < pic.len() {
        let c = pic[i];
        let run = pic[i..].iter().take_while(|&&u| u == c).count();
        i += run;
        let num = |v: u32, pad: bool| {
            if pad {
                format!("{v:02}")
            } else {
                v.to_string()
            }
        };
        let h12 = if hour % 12 == 0 { 12 } else { hour % 12 };
        let character = char::from_u32(u32::from(c)).unwrap_or('\u{fffd}');
        if time && matches!(character, 'd' | 'M' | 'y' | 'g')
            || !time && matches!(character, 'h' | 'H' | 'm' | 's' | 't')
        {
            out.extend(std::iter::repeat_n(character, run));
            continue;
        }
        match character {
            '\'' => {
                // Quoted text; a doubled quote inside (or as the whole run) is one quote.
                i -= run - 1;
                if run > 1 {
                    out.push('\'');
                    i += 1;
                    continue;
                }
                while i < pic.len() {
                    if pic[i] == u16::from(b'\'') {
                        if pic.get(i + 1) == Some(&u16::from(b'\'')) {
                            out.push('\'');
                            i += 2;
                            continue;
                        }
                        i += 1;
                        break;
                    }
                    out.push(char::from_u32(u32::from(pic[i])).unwrap_or('\u{fffd}'));
                    i += 1;
                }
            }
            'd' => {
                out += &match run {
                    1 | 2 => num(day, run == 2),
                    3 => locales::day_name(locale, wday as usize, true),
                    _ => locales::day_name(locale, wday as usize, false),
                }
            }
            'M' => {
                out += &match run {
                    1 | 2 => num(month, run == 2),
                    3 => locales::month_name(locale, month, true),
                    _ => locales::month_name(locale, month, false),
                }
            }
            'y' => {
                out += &match run {
                    1 => (year % 100).to_string(),
                    2 => format!("{:02}", year % 100),
                    _ => format!("{year:04}"),
                }
            }
            'g' => {
                out += if locale.id == 0x411 {
                    "西暦"
                } else if locale.id == 0x804 {
                    "公元"
                } else {
                    "A.D."
                }
            }
            'h' => out += &num(h12, run >= 2),
            'H' => out += &num(hour, run >= 2),
            'm' => out += &num(minute, run >= 2),
            's' => out += &num(second, run >= 2),
            't' => {
                let marker =
                    locales::info(locale, if hour < 12 { 0x28 } else { 0x29 }).expect("AM/PM data");
                out.extend(marker.chars().take(if run == 1 { 1 } else { usize::MAX }));
            }
            other => out.extend(std::iter::repeat_n(other, run)),
        }
    }
    out.encode_utf16().collect()
}

fn query(env: &mut Env<'_>, time: bool, wide: bool, named: bool) -> Outcome {
    let locale = if named {
        locales::from_name(env, env.arg(0), 0).and_then(|id| locales::by_id(env, id))
    } else {
        locales::by_id(env, env.arg(0))
    };
    let locale = match locale {
        Ok(locale) => locale,
        Err(error) => return locales::fail(env, error),
    };
    let flags = env.arg(1);
    let input = env.arg(2);
    let picture = env.arg(3);
    let allowed = if time { 0x8000000f } else { 0xc000003f };
    if flags & !allowed != 0
        || (!time && (flags & 0xb).count_ones() > 1)
        || (!time && picture != 0 && flags != 0)
    {
        return locales::fail(env, 1004);
    }
    if !time && (flags & 4 != 0 || named && env.arg(6) != 0) {
        adv_abi::warn_unavailable(
            env,
            "kernel32.dll!GetDateFormatEx",
            "alternate calendar picture data is unavailable",
        );
        return locales::fail(env, 87);
    }
    let mut st = if input == 0 {
        local_systemtime(env)
    } else {
        if check_range(env.cpu, input, 16, Prot::READ).is_err() {
            return locales::fail(env, 87);
        }
        read_st(env.cpu, input)
    };
    if time {
        if st[4] > 23 || st[5] > 59 || st[6] > 59 {
            return locales::fail(env, 87);
        }
    } else {
        if !valid_date(st) {
            return locales::fail(env, 87);
        }
        st[2] = (days_from_civil(i64::from(st[0]), u32::from(st[1]), u32::from(st[3])) + 4)
            .rem_euclid(7) as u16;
    }
    if flags & 0x30 != 0 || time && picture != 0 && flags & 0xf != 0 {
        adv_abi::warn_unavailable(
            env,
            "kernel32.dll!GetDateFormatEx",
            "direction marks and custom time-picture field suppression are unavailable",
        );
        return locales::fail(env, 50);
    }
    let picture = if picture != 0 {
        if wide {
            match WideStr(GuestPtr::new(picture)).read(env.cpu, 32768) {
                Ok(units) => units,
                Err(_) => return locales::fail(env, 87),
            }
        } else {
            match AnsiStr(GuestPtr::new(picture)).read(env.cpu, 32768) {
                Ok(bytes) => env.ext.get::<Win32>().codec.to_utf16(0, &bytes),
                Err(_) => return locales::fail(env, 87),
            }
        }
    } else {
        let mut text = if time {
            locales::info(locale, 0x1003).expect("time format")
        } else if flags & 8 != 0 {
            locales::info(locale, 0x1006).expect("year month format")
        } else {
            locales::info(locale, if flags & 2 != 0 { 0x20 } else { 0x1f }).expect("date format")
        };
        if time {
            if flags & 1 != 0 {
                text = if flags & 8 != 0 || locale.id != 0x409 {
                    "H"
                } else {
                    "h tt"
                }
                .into();
            } else if flags & 2 != 0 {
                text = text.replace(":ss", "");
            }
            if flags & 4 != 0 {
                text = text.replace(" tt", "").replace("tt", "");
            }
            if flags & 8 != 0 {
                text = text.replace('h', "H");
            }
        }
        text.encode_utf16().collect()
    };
    let out = format_picture(&picture, st, locale, time);
    locales::put(
        env,
        &String::from_utf16_lossy(&out),
        env.arg(4),
        env.arg(5),
        wide,
    )
}
fn valid_date(st: [u16; 8]) -> bool {
    if !(1601..=30827).contains(&st[0]) || !(1..=12).contains(&st[1]) || st[3] == 0 {
        return false;
    }
    let days = days_from_civil(i64::from(st[0]), u32::from(st[1]), u32::from(st[3]));
    civil_from_days(days) == (i64::from(st[0]), u32::from(st[1]), u32::from(st[3]))
}
pub(super) fn register(registry: &mut Registry) {
    for (name, time, wide, named, count) in [
        ("GetDateFormatA", false, false, false, 6),
        ("GetDateFormatW", false, true, false, 6),
        ("GetTimeFormatA", true, false, false, 6),
        ("GetTimeFormatW", true, true, false, 6),
        ("GetDateFormatEx", false, true, true, 7),
        ("GetTimeFormatEx", true, true, true, 6),
    ] {
        let handler: adv_machine::Handler = match (time, wide, named) {
            (false, false, false) => |e| query(e, false, false, false),
            (false, true, false) => |e| query(e, false, true, false),
            (true, false, false) => |e| query(e, true, false, false),
            (true, true, false) => |e| query(e, true, true, false),
            (false, true, true) => |e| query(e, false, true, true),
            _ => |e| query(e, true, true, true),
        };
        crate::stdcall(registry, K32, name, count, handler);
    }
}
#[cfg(test)]
mod tests;
