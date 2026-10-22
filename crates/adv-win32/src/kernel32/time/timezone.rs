//! UTC conversion uses the guest offset or the caller's actual transition rules.
use super::*;
use adv_abi::guest::check_range;
use adv_cpu::backend::Prot;
fn valid(st: [u16; 8]) -> bool {
    (1601..=30827).contains(&st[0])
        && (1..=12).contains(&st[1])
        && st[3] != 0
        && st[4] < 24
        && st[5] < 60
        && st[6] < 60
        && st[7] < 1000
        && civil_from_days(days_from_civil(
            i64::from(st[0]),
            u32::from(st[1]),
            u32::from(st[3]),
        )) == (i64::from(st[0]), u32::from(st[1]), u32::from(st[3]))
}
fn transition(mut st: [u16; 8], year: u16) -> Option<[u16; 8]> {
    if !(1..=12).contains(&st[1]) {
        return None;
    }
    if st[0] == 0 {
        if st[2] > 6 || !(1..=5).contains(&st[3]) {
            return None;
        }
        let first =
            (days_from_civil(i64::from(year), u32::from(st[1]), 1) + 4).rem_euclid(7) as u16;
        let mut day = 1 + (st[2] + 7 - first) % 7 + 7 * (st[3] - 1);
        if st[3] == 5
            && civil_from_days(days_from_civil(
                i64::from(year),
                u32::from(st[1]),
                u32::from(day),
            ))
            .1 != u32::from(st[1])
        {
            day -= 7;
        }
        st[0] = year;
        st[3] = day;
    }
    valid(st).then_some(st)
}
fn fail(env: &mut Env<'_>, error: u32) -> Outcome {
    crate::util::set_last_error(env, error);
    Outcome::Return(0)
}
fn convert(env: &mut Env<'_>) -> Outcome {
    let zone = env.arg(0);
    let input = env.arg(1);
    let output = env.arg(2);
    if check_range(env.cpu, input, 16, Prot::READ).is_err()
        || check_range(env.cpu, output, 16, Prot::WRITE).is_err()
    {
        return fail(env, 87);
    }
    let st = read_st(env.cpu, input);
    if !valid(st) {
        return fail(env, 87);
    }
    let utc = i128::from(systemtime_to_filetime(st));
    let bias = if zone == 0 {
        -i64::from(env.ext.get::<Win32>().cfg.utc_offset_min)
    } else {
        if check_range(env.cpu, zone, 172, Prot::READ).is_err() {
            return fail(env, 87);
        }
        let base = i64::from(env.cpu.read_u32(zone).unwrap() as i32);
        let standard = read_st(env.cpu, zone + 68);
        let daylight = read_st(env.cpu, zone + 152);
        if standard[1] == 0 {
            base
        } else {
            let Some(standard) = transition(standard, st[0]) else {
                return fail(env, 87);
            };
            let Some(daylight) = transition(daylight, st[0]) else {
                return fail(env, 87);
            };
            let standard_bias = base + i64::from(env.cpu.read_u32(zone + 84).unwrap() as i32);
            let daylight_bias = base + i64::from(env.cpu.read_u32(zone + 168).unwrap() as i32);
            let start = i128::from(systemtime_to_filetime(daylight))
                + i128::from(standard_bias) * 600_000_000;
            let end = i128::from(systemtime_to_filetime(standard))
                + i128::from(daylight_bias) * 600_000_000;
            let summer = if start < end {
                utc >= start && utc < end
            } else {
                utc >= start || utc < end
            };
            if summer { daylight_bias } else { standard_bias }
        }
    };
    let local = utc - i128::from(bias) * 600_000_000;
    if !(0..=i128::from(i64::MAX)).contains(&local) {
        return fail(env, 87);
    }
    let local = filetime_to_systemtime(local as u64);
    if !valid(local) {
        return fail(env, 87);
    }
    write_units(env.cpu, output, &local);
    Outcome::Return(1)
}
pub(super) fn register(registry: &mut Registry) {
    crate::stdcall(registry, K32, "SystemTimeToTzSpecificLocalTime", 3, convert);
}
#[cfg(test)]
mod tests;
