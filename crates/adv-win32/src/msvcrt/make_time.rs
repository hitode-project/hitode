//! Normalize guest calendars through the existing Windows civil-time owner.
//! Version limits retain native intermediate writes and x86 overflow checks.
use super::*;
use adv_abi::{GuestPtr, guest::memory_failure};

const NAMES: &[&str] = &[
    "mktime",
    "_mktime32",
    "_mktime64",
    "_mkgmtime",
    "_mkgmtime32",
    "_mkgmtime64",
    "_o__mktime32",
    "_o__mktime64",
    "_o__mkgmtime32",
    "_o__mkgmtime64",
];
pub(super) fn register(registry: &mut Registry) {
    let exports: Vec<_> = NAMES
        .iter()
        .map(|&n| (n, call as adv_machine::Handler))
        .collect();
    versions::register(registry, &exports);
}
fn result(value: i64, wide: bool) -> Outcome {
    if wide {
        Outcome::Return64(value as u64)
    } else {
        Outcome::Return(value as u32)
    }
}
fn failure(env: &mut Env<'_>, wide: bool) -> Outcome {
    match floating::errno_address(env).write(env.cpu, 22) {
        Ok(()) => result(-1, wide),
        Err(fault) => memory_failure(fault),
    }
}
// The pinned native time64 helpers accept a small calendar margin around the
// public range. UTC conversion accepts twelve hours before the epoch; local
// conversion rejects negative timestamps before applying the timezone.
fn valid(seconds: i64, wide: bool, local: bool, limits: versions::CalendarLimits) -> bool {
    let maximum = if wide {
        if local {
            limits.local_max
        } else {
            limits.utc_max
        }
    } else {
        i64::from(i32::MAX)
    };
    (if local { 0 } else { -43_200 }..=maximum).contains(&seconds)
}
fn call(env: &mut Env<'_>) -> Outcome {
    let thunk = env.sys.thunks.get(env.thunk);
    let profile = versions::profile(&thunk.module);
    let name = thunk.name.strip_prefix("_o_").unwrap_or(&thunk.name);
    let wide = name.ends_with("64");
    let local = !name.contains("mkgmtime");
    let pointer = GuestPtr::<i32>::new(env.arg(0));
    if pointer.is_null() && profile.invalid_parameters {
        return errors::invalid_outcome(env, result(-1, wide));
    }
    match normalize(env, pointer, wide, local, profile.calendar_limits) {
        Ok(Some(seconds)) => result(seconds, wide),
        Ok(None) => failure(env, wide),
        Err(fault) => memory_failure(fault),
    }
}
fn normalize(
    env: &mut Env<'_>,
    pointer: GuestPtr<i32>,
    wide: bool,
    local: bool,
    limits: versions::CalendarLimits,
) -> Result<Option<i64>, adv_cpu::backend::MemFault> {
    // Year is checked before any other field is read. Month normalization is
    // observable even when a later range check fails; tm_year stays untouched.
    let mut year = i64::from(pointer.offset(5)?.read(env.cpu)?);
    let year_max = if wide { limits.input_year_max } else { 139 };
    if !(69..=year_max).contains(&year) {
        return Ok(None);
    }
    let mut month = pointer.offset(4)?.read(env.cpu)?;
    if !(0..=11).contains(&month) {
        year += i64::from(month / 12);
        month %= 12;
        pointer.offset(4)?.write(env.cpu, month)?;
        if month < 0 {
            month += 12;
            year -= 1;
            pointer.offset(4)?.write(env.cpu, month)?;
        }
        if !(69..=year_max).contains(&year) {
            return Ok(None);
        }
    }
    let days = crate::kernel32::days_from_civil(year + 1900, month as u32 + 1, 1) - 1;
    let mut seconds = days;
    for (index, scale) in [(3, 1), (2, 24), (1, 60), (0, 60)] {
        let value = i64::from(pointer.offset(index)?.read(env.cpu)?);
        seconds = seconds * scale + value;
        // Native time32 rejects each overflowing operation, even if a later
        // field could mathematically cancel it back into range.
        if !wide && i32::try_from(seconds).is_err() {
            return Ok(None);
        }
        if index != 0 && !wide && i32::try_from(seconds * if index == 3 { 24 } else { 60 }).is_err()
        {
            return Ok(None);
        }
    }
    let offset = if local {
        i64::from(env.ext.get::<crate::Win32>().cfg.utc_offset_min) * 60
    } else {
        0
    };
    seconds -= offset;
    if !wide {
        seconds = i64::from(seconds as i32);
    }
    if !valid(seconds, wide, local, limits) {
        return Ok(None);
    }
    if local && pointer.offset(8)?.read(env.cpu)? > 0 {
        // Explicit tm_isdst uses the CRT's default one-hour daylight bias,
        // even in the fixed-offset profile whose automatic DST is disabled.
        seconds -= 3600;
        if !wide {
            seconds = i64::from(seconds as i32);
        }
        if !valid(seconds, wide, true, limits) {
            return Ok(None);
        }
    }
    let normalized = time64::calendar(seconds + offset);
    // Native rep movsd publishes fields in ascending order. Checked individual
    // writes retain earlier fields if a later field faults on another page.
    for (index, value) in normalized.into_iter().enumerate() {
        pointer.offset(index as u32)?.write(env.cpu, value)?;
    }
    Ok(Some(seconds))
}
