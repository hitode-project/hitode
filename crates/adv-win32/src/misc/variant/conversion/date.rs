//! Gregorian OLE DATE values use the shared calendar and locale picture engine.
use super::*;
use crate::kernel32::{civil_from_days, days_from_civil, locale_info};
const EPOCH: i64 = -25569; // 1899-12-30 relative to 1970-01-01.
pub(super) fn validate(value: f64) -> Result<(), u32> {
    parts(value).map(|_| ())
}
fn parts(value: f64) -> Result<[u16; 8], u32> {
    if !value.is_finite() || value <= -657435.0 || value >= 2958466.0 {
        return Err(OVERFLOW);
    }
    let mut days = value.trunc() as i64;
    let mut seconds = (value.fract().abs() * 86400.0).round() as u32;
    if seconds == 86400 {
        seconds = 0;
        days += 1;
    }
    let (year, month, day) = civil_from_days(days + EPOCH);
    if !(100..=9999).contains(&year) {
        return Err(OVERFLOW);
    }
    Ok([
        year as u16,
        month as u16,
        (days + EPOCH + 4).rem_euclid(7) as u16,
        day as u16,
        (seconds / 3600) as u16,
        (seconds / 60 % 60) as u16,
        (seconds % 60) as u16,
        0,
    ])
}
pub(super) fn format(value: f64, locale: Locale, flags: u32) -> Result<String, u32> {
    if flags & 3 == 3 {
        return Err(INVALID);
    }
    let st = parts(value)?;
    let date = flags & 1 == 0 && (flags & 2 != 0 || value.trunc() != 0.0);
    let time = flags & 2 == 0 && (flags & 1 != 0 || value.fract() != 0.0 || value == 0.0);
    let mut text = Vec::new();
    if date {
        let picture = locale_info(locale, 0x1f).ok_or(INVALID)?;
        text = crate::kernel32::time::format_locale_picture(
            &picture.encode_utf16().collect::<Vec<_>>(),
            st,
            locale,
            false,
        );
    }
    if time {
        if !text.is_empty() {
            text.push(32);
        }
        let picture = locale_info(locale, 0x1003).ok_or(INVALID)?;
        text.extend(crate::kernel32::time::format_locale_picture(
            &picture.encode_utf16().collect::<Vec<_>>(),
            st,
            locale,
            true,
        ));
    }
    String::from_utf16(&text).map_err(|_| INVALID)
}
fn time(text: &str, locale: Locale) -> Result<(u32, &str), u32> {
    let Some(colon) = text.find(':') else {
        return Ok((0, text));
    };
    let start = text[..colon]
        .rfind(|c: char| !c.is_ascii_digit())
        .map_or(0, |p| p + text[p..].chars().next().unwrap().len_utf8());
    let mut before = text[..start].trim();
    let mut token = text[start..].trim();
    let mut pm = None;
    for (marker, is_pm) in [
        ("AM".into(), false),
        ("PM".into(), true),
        (locale_info(locale, 0x28).ok_or(INVALID)?, false),
        (locale_info(locale, 0x29).ok_or(INVALID)?, true),
    ] {
        if let Some(rest) = before.strip_suffix(&marker) {
            before = rest.trim();
            pm = Some(is_pm);
        }
        if token.len() >= marker.len()
            && token
                .get(token.len() - marker.len()..)
                .is_some_and(|s| s.eq_ignore_ascii_case(&marker))
        {
            token = token[..token.len() - marker.len()].trim();
            pm = Some(is_pm);
        }
    }
    let fields = token
        .split(':')
        .map(str::parse::<u32>)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| MISMATCH)?;
    if !(2..=3).contains(&fields.len()) {
        return Err(MISMATCH);
    }
    let mut hour = fields[0];
    let minute = fields[1];
    let second = *fields.get(2).unwrap_or(&0);
    if let Some(pm) = pm {
        if !(1..=12).contains(&hour) {
            return Err(MISMATCH);
        }
        hour = hour % 12 + if pm { 12 } else { 0 };
    }
    if hour > 23 || minute > 59 || second > 59 {
        return Err(MISMATCH);
    }
    Ok((hour * 3600 + minute * 60 + second, before))
}
pub(super) fn parse(env: &mut Env<'_>, text: &str, locale: Locale, flags: u32) -> Result<f64, u32> {
    let ft = crate::kernel32::filetime_now(env);
    let offset = i64::from(env.ext.get::<crate::Win32>().cfg.utc_offset_min) * 600_000_000;
    let current = crate::kernel32::time::filetime_to_systemtime(ft.saturating_add_signed(offset));
    parse_with_year(text, locale, flags, u32::from(current[0]))
}
fn parse_with_year(text: &str, locale: Locale, flags: u32, current: u32) -> Result<f64, u32> {
    if flags & 3 == 3 {
        return Err(INVALID);
    }
    let (seconds, raw) = time(text.trim(), locale)?;
    let raw = raw.trim();
    let mut days = 0;
    if !raw.is_empty() {
        let normalized = raw.replace(['年', '月', '日', '-', '/', '.', ','], " ");
        let fields = normalized.split_whitespace().collect::<Vec<_>>();
        if !(2..=3).contains(&fields.len()) {
            return Err(MISMATCH);
        }
        let mut named = None;
        let mut values = Vec::new();
        for (at, field) in fields.iter().enumerate() {
            if let Ok(value) = field.parse::<u32>() {
                values.push(value);
                continue;
            }
            let month = (1..=12)
                .find(|month| {
                    [0x38 + month - 1, 0x44 + month - 1]
                        .into_iter()
                        .any(|kind| {
                            locale_info(locale, kind)
                                .is_some_and(|name| name.eq_ignore_ascii_case(field))
                        })
                })
                .ok_or(MISMATCH)?;
            if named.replace((at, month)).is_some() {
                return Err(MISMATCH);
            }
        }
        let (mut year, month, day) = if let Some((at, month)) = named {
            if values.len() == 1 {
                (current, month, values[0])
            } else if at == 0 || values[0] <= 31 {
                (values[1], month, values[0])
            } else {
                (values[0], month, values[1])
            }
        } else if fields.len() == 2 {
            (current, values[0], values[1])
        } else if fields[0].len() >= 3 || values[0] > 31 {
            (values[0], values[1], values[2])
        } else if locale.id & 0x3ff == 9 || locale.id == 0x7f {
            (values[2], values[0], values[1])
        } else {
            (values[0], values[1], values[2])
        };
        if year < 100 {
            year += if year < 30 { 2000 } else { 1900 };
        }
        if !(100..=9999).contains(&year) || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
            return Err(MISMATCH);
        }
        days = days_from_civil(i64::from(year), month, day);
        if civil_from_days(days) != (i64::from(year), month, day) {
            return Err(MISMATCH);
        }
        days -= EPOCH;
    } else if !text.contains(':') {
        return Err(MISMATCH);
    }
    if flags & 1 != 0 {
        days = 0;
    }
    let fraction = if flags & 2 != 0 {
        0.0
    } else {
        f64::from(seconds) / 86400.0
    };
    Ok(days as f64 + if days < 0 { -fraction } else { fraction })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gregorian_automation_epoch_negative_fractions_and_fixed_locale_pictures() {
        for (id, value, text) in [
            (0x409, 0.0, "12:00:00 AM"),
            (0x411, 0.5, "12:00:00"),
            (0x409, -1.25, "12/29/1899 6:00:00 AM"),
            (0x411, -1.25, "1899/12/29 6:00:00"),
            (0x409, 45292.0, "1/1/2024"),
            (0x411, 45292.0, "2024/01/01"),
            (0x804, 45292.0, "2024/1/1"),
        ] {
            let locale = Locale {
                id,
                name: "",
                cp: 1252,
            };
            assert_eq!(format(value, locale, 0).unwrap(), text);
            assert_eq!(parse_with_year(text, locale, 0, 2024).unwrap(), value);
        }
        let locale = Locale {
            id: 0x409,
            name: "en-US",
            cp: 1252,
        };
        for (text, value) in [
            ("2024-02-29", 45351.0),
            ("Feb 29, 2024", 45351.0),
            ("12/30/1899 6:00 PM", 0.75),
            ("12/29/1899 6:00 AM", -1.25),
            ("2/29", 45351.0),
        ] {
            assert_eq!(
                parse_with_year(text, locale, 0, 2024).unwrap(),
                value,
                "{text}"
            );
        }
        assert_eq!(parse_with_year("2/29/2023", locale, 0, 2024), Err(MISMATCH));
    }
}
