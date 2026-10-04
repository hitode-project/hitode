//! Time: all readings come from the machine clock (virtual or real).

use adv_abi::{GuestPtr, Registry, types::FILETIME, win32};
use adv_cpu::backend::{Guest, GuestExt};
use adv_machine::{Env, Outcome, Wait};

use super::K32;
use crate::{Win32, stdcall, util::write_units};

/// FILETIME of 1970-01-01.
const UNIX_EPOCH_FT: u64 = 116_444_736_000_000_000;
/// Wall-clock start for virtual-time runs: 2020-01-01T00:00:00Z (deterministic).
const VIRTUAL_START_UNIX: u64 = 1_577_836_800;

/// Current UTC time as FILETIME (100 ns units since 1601).
pub fn filetime_now(env: &mut Env<'_>) -> u64 {
    let clock = &mut env.sys.clock;
    let unix_us = if clock.is_virtual() {
        VIRTUAL_START_UNIX * 1_000_000 + clock.guest_now_us() - clock.epoch_us
    } else {
        let _ = clock.guest_now_us();
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_micros() as u64)
            .unwrap_or(0)
    };
    UNIX_EPOCH_FT + unix_us * 10
}

fn local_offset_ft(env: &Env<'_>) -> i64 {
    i64::from(env.ext.get::<Win32>().cfg.utc_offset_min) * 60 * 10_000_000
}

/// Days since 1970-01-01 to (year, month, day). (H. Hinnant's civil_from_days.)
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = if m > 2 { m - 3 } else { m + 9 } as i64;
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// FILETIME to SYSTEMTIME fields.
pub fn filetime_to_systemtime(ft: u64) -> [u16; 8] {
    let unix_100ns = ft as i64 - UNIX_EPOCH_FT as i64;
    let secs = unix_100ns.div_euclid(10_000_000);
    let ms = (unix_100ns.rem_euclid(10_000_000) / 10_000) as u16;
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    let dow = (days + 4).rem_euclid(7) as u16; // 1970-01-01 was a Thursday
    [
        y as u16,
        m as u16,
        dow,
        d as u16,
        (sod / 3600) as u16,
        (sod / 60 % 60) as u16,
        (sod % 60) as u16,
        ms,
    ]
}

fn systemtime_to_filetime(st: [u16; 8]) -> u64 {
    let days = days_from_civil(i64::from(st[0]), u32::from(st[1]), u32::from(st[3]));
    let secs = days * 86_400 + i64::from(st[4]) * 3600 + i64::from(st[5]) * 60 + i64::from(st[6]);
    ((secs * 10_000_000 + i64::from(st[7]) * 10_000) + UNIX_EPOCH_FT as i64) as u64
}

fn filetime_to_dos(ft: u64) -> Option<(u16, u16)> {
    // Native rounds upward to the next two-second FAT tick before checking
    // its representable year. This can carry the final second of 1979 into
    // 1980, or reject the final second of 2107 after it carries into 2108.
    let tick = 20_000_000;
    let rounded = ft.checked_add(tick - 1)? / tick * tick;
    let first = systemtime_to_filetime([1980, 1, 0, 1, 0, 0, 0, 0]);
    let limit = systemtime_to_filetime([2108, 1, 0, 1, 0, 0, 0, 0]);
    if !(first..limit).contains(&rounded) {
        return None;
    }
    let [year, month, _, day, hour, minute, second, _] = filetime_to_systemtime(rounded);
    Some((
        ((year - 1980) << 9) | (month << 5) | day,
        (hour << 11) | (minute << 5) | (second / 2),
    ))
}

fn dos_to_filetime(date: u16, time: u16) -> Option<u64> {
    let year = 1980 + (date >> 9);
    let month = (date >> 5) & 15;
    let day = date & 31;
    let hour = time >> 11;
    let minute = (time >> 5) & 63;
    let second = (time & 31) * 2;
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }
    let days = days_from_civil(i64::from(year), u32::from(month), u32::from(day));
    if civil_from_days(days) != (i64::from(year), u32::from(month), u32::from(day)) {
        return None;
    }
    Some(systemtime_to_filetime([
        year, month, 0, day, hour, minute, second, 0,
    ]))
}

#[win32(kernel32, name = "FileTimeToDosDateTime", level = implement)]
fn file_time_to_dos_date_time(
    env: &mut Env<'_>,
    input: GuestPtr<FILETIME>,
    date: GuestPtr<u16>,
    time: GuestPtr<u16>,
) -> Result<i32, adv_cpu::backend::MemFault> {
    let ft = input.read(env.cpu)?;
    let value = u64::from(ft.dwLowDateTime) | u64::from(ft.dwHighDateTime) << 32;
    let Some((date_value, time_value)) = filetime_to_dos(value) else {
        crate::util::set_last_error(env, crate::util::err::INVALID_PARAMETER);
        return Ok(0);
    };
    // Verify both outputs before publishing either one. Invalid input leaves
    // both buffers intact and reports ERROR_INVALID_PARAMETER, as native does.
    adv_abi::guest::check_range(env.cpu, date.addr(), 2, adv_cpu::backend::Prot::WRITE)?;
    adv_abi::guest::check_range(env.cpu, time.addr(), 2, adv_cpu::backend::Prot::WRITE)?;
    date.write(env.cpu, date_value)?;
    time.write(env.cpu, time_value)?;
    Ok(1)
}

#[win32(kernel32, name = "DosDateTimeToFileTime", level = implement)]
fn dos_date_time_to_file_time(
    env: &mut Env<'_>,
    date: u32,
    time: u32,
    output: GuestPtr<FILETIME>,
) -> Result<i32, adv_cpu::backend::MemFault> {
    let Some(value) = dos_to_filetime(date as u16, time as u16) else {
        crate::util::set_last_error(env, crate::util::err::INVALID_PARAMETER);
        return Ok(0);
    };
    output.write(
        env.cpu,
        FILETIME {
            dwLowDateTime: value as u32,
            dwHighDateTime: (value >> 32) as u32,
        },
    )?;
    Ok(1)
}

fn read_ft(cpu: &dyn Guest, p: u32) -> u64 {
    let lo = cpu.read_u32(p).unwrap_or(0);
    let hi = cpu.read_u32(p + 4).unwrap_or(0);
    (u64::from(hi) << 32) | u64::from(lo)
}

fn write_ft(cpu: &mut dyn Guest, p: u32, ft: u64) {
    let _ = cpu.write_u32(p, ft as u32);
    let _ = cpu.write_u32(p + 4, (ft >> 32) as u32);
}

fn read_st(cpu: &dyn Guest, p: u32) -> [u16; 8] {
    let mut st = [0u16; 8];
    for (i, f) in st.iter_mut().enumerate() {
        *f = cpu.read_u16(p + 2 * i as u32).unwrap_or(0);
    }
    st
}

#[win32(kernel32, name = "GetSystemTimeAsFileTime", level = implement)]
fn get_system_time_as_file_time(
    env: &mut Env<'_>,
    out: GuestPtr<FILETIME>,
) -> Result<(), adv_cpu::backend::MemFault> {
    let ft = filetime_now(env);
    out.write(
        env.cpu,
        FILETIME {
            dwLowDateTime: ft as u32,
            dwHighDateTime: (ft >> 32) as u32,
        },
    )
}

fn get_system_time(env: &mut Env<'_>) -> Outcome {
    let ft = filetime_now(env);
    let out = env.arg(0);
    write_units(env.cpu, out, &filetime_to_systemtime(ft));
    Outcome::Return(0)
}

fn get_local_time(env: &mut Env<'_>) -> Outcome {
    let st = local_systemtime(env);
    let out = env.arg(0);
    write_units(env.cpu, out, &st);
    Outcome::Return(0)
}

/// Expand a GetDateFormat/GetTimeFormat picture over SYSTEMTIME `st` (year, month, weekday,
/// day, hour, minute, second, ms). `'...'` is literal text (`''` a quote); names come out in
/// English, as under an English user locale.
fn format_picture(pic: &[u16], st: [u16; 8]) -> Vec<u16> {
    const DAYS: [&str; 7] = [
        "Sunday",
        "Monday",
        "Tuesday",
        "Wednesday",
        "Thursday",
        "Friday",
        "Saturday",
    ];
    const MONTHS: [&str; 12] = [
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
    ];
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
        match char::from_u32(u32::from(c)).unwrap_or('\u{fffd}') {
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
                    3 => DAYS[wday as usize % 7][..3].to_owned(),
                    _ => DAYS[wday as usize % 7].to_owned(),
                }
            }
            'M' => {
                out += &match run {
                    1 | 2 => num(month, run == 2),
                    3 => MONTHS[(month as usize + 11) % 12][..3].to_owned(),
                    _ => MONTHS[(month as usize + 11) % 12].to_owned(),
                }
            }
            'y' => {
                out += &match run {
                    1 => (year % 100).to_string(),
                    2 => format!("{:02}", year % 100),
                    _ => year.to_string(),
                }
            }
            'g' => {}
            'h' => out += &num(h12, run >= 2),
            'H' => out += &num(hour, run >= 2),
            'm' => out += &num(minute, run >= 2),
            's' => out += &num(second, run >= 2),
            't' => out += &(if hour < 12 { "AM" } else { "PM" })[..run.min(2)],
            other => out.extend(std::iter::repeat_n(other, run)),
        }
    }
    out.encode_utf16().collect()
}

/// Get{Date,Time}FormatW(lcid, flags, SYSTEMTIME* (NULL: now, local), picture*, buf, cap).
/// Without a picture the user locale's defaults apply (short/long date, H:mm:ss).
fn get_date_time_format_w(env: &mut Env<'_>, time: bool) -> Outcome {
    const DATE_LONGDATE: u32 = 2;
    const TIME_NOMINUTESORSECONDS: u32 = 1;
    const TIME_NOSECONDS: u32 = 2;
    let (flags, st_p, pic_p, buf, cap) =
        (env.arg(1), env.arg(2), env.arg(3), env.arg(4), env.arg(5));
    let st = if st_p != 0 {
        read_st(env.cpu, st_p)
    } else {
        local_systemtime(env)
    };
    let pic: Vec<u16> = if pic_p != 0 {
        crate::util::wstr(env.cpu, pic_p)
    } else {
        let d = match (time, flags) {
            (true, f) if f & TIME_NOMINUTESORSECONDS != 0 => "H",
            (true, f) if f & TIME_NOSECONDS != 0 => "H:mm",
            (true, _) => "H:mm:ss",
            (false, f) if f & DATE_LONGDATE != 0 => "dddd, MMMM d, yyyy",
            (false, _) => "yyyy/MM/dd",
        };
        d.encode_utf16().collect()
    };
    let out = format_picture(&pic, st);
    let n = out.len() as u32 + 1;
    if cap == 0 {
        return Outcome::Return(n);
    }
    if crate::util::put_wstr(env.cpu, buf, cap, &out).is_none() {
        crate::util::set_last_error(env, crate::util::err::INSUFFICIENT_BUFFER);
        return Outcome::Return(0);
    }
    Outcome::Return(n)
}

fn local_systemtime(env: &mut Env<'_>) -> [u16; 8] {
    let ft = filetime_now(env) as i64 + local_offset_ft(env);
    filetime_to_systemtime(ft as u64)
}

#[win32(kernel32, name = "GetTickCount", level = implement)]
fn get_tick_count(env: &mut Env<'_>) -> u32 {
    (env.sys.clock.guest_now_us() / 1000) as u32
}

#[win32(kernel32, name = "QueryPerformanceCounter", level = implement)]
fn query_performance_counter(
    env: &mut Env<'_>,
    out: GuestPtr<i64>,
) -> Result<i32, adv_cpu::backend::MemFault> {
    let t = env.sys.clock.guest_now_us() * (adv_machine::clock::QPC_FREQ / 1_000_000);
    out.write(env.cpu, t as i64)?;
    Ok(1)
}

#[win32(kernel32, name = "QueryPerformanceFrequency", level = implement)]
fn query_performance_frequency(
    env: &mut Env<'_>,
    out: GuestPtr<i64>,
) -> Result<i32, adv_cpu::backend::MemFault> {
    out.write(env.cpu, adv_machine::clock::QPC_FREQ as i64)?;
    Ok(1)
}

/// Sleep(ms): 0 yields, anything else blocks the context until the deadline.
#[win32(kernel32, name = "Sleep", level = implement)]
pub(crate) fn sleep(env: &mut Env<'_>, ms: u32) -> Outcome {
    if ms == 0 {
        return Outcome::block_then_return(Wait::Yield, 0);
    }
    if ms == u32::MAX {
        return Outcome::Fail("Sleep(INFINITE)".into());
    }
    let until = env.sys.clock.now_us() + u64::from(ms) * 1000;
    Outcome::block_then_return(Wait::Until(until), 0)
}

fn file_time_to_system_time(env: &mut Env<'_>) -> Outcome {
    let ft = read_ft(env.cpu, env.arg(0));
    let out = env.arg(1);
    write_units(env.cpu, out, &filetime_to_systemtime(ft));
    Outcome::Return(1)
}

fn system_time_to_file_time(env: &mut Env<'_>) -> Outcome {
    let st = read_st(env.cpu, env.arg(0));
    let out = env.arg(1);
    write_ft(env.cpu, out, systemtime_to_filetime(st));
    Outcome::Return(1)
}

fn file_time_to_local_file_time(env: &mut Env<'_>) -> Outcome {
    let ft = read_ft(env.cpu, env.arg(0)) as i64 + local_offset_ft(env);
    let out = env.arg(1);
    write_ft(env.cpu, out, ft as u64);
    Outcome::Return(1)
}

fn local_file_time_to_file_time(env: &mut Env<'_>) -> Outcome {
    let ft = read_ft(env.cpu, env.arg(0)) as i64 - local_offset_ft(env);
    let out = env.arg(1);
    write_ft(env.cpu, out, ft as u64);
    Outcome::Return(1)
}

fn compare_file_time(env: &mut Env<'_>) -> Outcome {
    let (a, b) = (read_ft(env.cpu, env.arg(0)), read_ft(env.cpu, env.arg(1)));
    Outcome::Return(a.cmp(&b) as i32 as u32)
}

/// TIME_ZONE_INFORMATION for a fixed offset without DST (JST by default).
fn get_time_zone_information(env: &mut Env<'_>) -> Outcome {
    let p = env.arg(0);
    let bias = -env.ext.get::<Win32>().cfg.utc_offset_min;
    crate::util::zero(env.cpu, p, 172);
    let _ = env.cpu.write_u32(p, bias as u32);
    Outcome::Return(0) // TIME_ZONE_ID_UNKNOWN
}

pub(super) fn register(reg: &mut Registry) {
    reg.register(get_system_time_as_file_time_ABI);
    reg.register(file_time_to_dos_date_time_ABI);
    reg.register(dos_date_time_to_file_time_ABI);
    stdcall(reg, K32, "GetSystemTime", 1, get_system_time);
    stdcall(reg, K32, "GetLocalTime", 1, get_local_time);
    reg.register(get_tick_count_ABI);
    stdcall(reg, K32, "GetDateFormatW", 6, |e| {
        get_date_time_format_w(e, false)
    });
    stdcall(reg, K32, "GetTimeFormatW", 6, |e| {
        get_date_time_format_w(e, true)
    });
    reg.register(query_performance_counter_ABI);
    reg.register(query_performance_frequency_ABI);
    reg.register(sleep_ABI);
    stdcall(
        reg,
        K32,
        "FileTimeToSystemTime",
        2,
        file_time_to_system_time,
    );
    stdcall(
        reg,
        K32,
        "SystemTimeToFileTime",
        2,
        system_time_to_file_time,
    );
    stdcall(
        reg,
        K32,
        "FileTimeToLocalFileTime",
        2,
        file_time_to_local_file_time,
    );
    stdcall(
        reg,
        K32,
        "LocalFileTimeToFileTime",
        2,
        local_file_time_to_file_time,
    );
    stdcall(reg, K32, "CompareFileTime", 2, compare_file_time);
    stdcall(
        reg,
        K32,
        "GetTimeZoneInformation",
        1,
        get_time_zone_information,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calendar_round_trip() {
        // 2012-07-30 12:34:56.789 UTC, a Monday.
        let st = [2012, 7, 1, 30, 12, 34, 56, 789];
        let ft = systemtime_to_filetime(st);
        assert_eq!(filetime_to_systemtime(ft), st);
        assert_eq!(
            filetime_to_systemtime(UNIX_EPOCH_FT),
            [1970, 1, 4, 1, 0, 0, 0, 0]
        );
    }

    #[test]
    fn dos_conversion_rounds_up_before_range_check_and_carries_calendar_boundaries() {
        let first = systemtime_to_filetime([1980, 1, 0, 1, 0, 0, 0, 0]);
        assert_eq!(filetime_to_dos(first - 20_000_000), None);
        assert_eq!(filetime_to_dos(first - 10_000_000), Some((0x21, 0)));
        assert_eq!(filetime_to_dos(first), Some((0x21, 0)));
        assert_eq!(filetime_to_dos(first + 1), Some((0x21, 1)));
        assert_eq!(filetime_to_dos(first + 20_000_000), Some((0x21, 1)));
        assert_eq!(filetime_to_dos(first + 20_000_001), Some((0x21, 2)));
        let leap_end = systemtime_to_filetime([2000, 2, 0, 29, 23, 59, 59, 999]);
        assert_eq!(
            filetime_to_dos(leap_end),
            Some((((2000 - 1980) << 9) | (3 << 5) | 1, 0))
        );
        let last = systemtime_to_filetime([2107, 12, 0, 31, 23, 59, 58, 0]);
        assert_eq!(filetime_to_dos(last), Some((0xff9f, 0xbf7d)));
        assert_eq!(filetime_to_dos(last + 1), None);
        for invalid in [0, i64::MAX as u64, 1 << 63, u64::MAX] {
            assert_eq!(filetime_to_dos(invalid), None);
        }
    }

    #[test]
    fn dos_calendar_rejects_invalid_leap_days_and_packed_time_fields() {
        let date = |year: u16, month: u16, day: u16| ((year - 1980) << 9) | (month << 5) | day;
        for year in [1980, 2000, 2004] {
            let packed = date(year, 2, 29);
            assert_eq!(
                filetime_to_dos(dos_to_filetime(packed, 0).unwrap()),
                Some((packed, 0))
            );
        }
        for invalid in [
            date(2001, 2, 29),
            date(2100, 2, 29),
            date(1980, 4, 31),
            date(1980, 0, 1),
            date(1980, 13, 1),
            date(1980, 1, 0),
            0xffff,
        ] {
            assert_eq!(dos_to_filetime(invalid, 0), None);
        }
        for invalid in [24 << 11, 60 << 5, 30, 31, 0xffff] {
            assert_eq!(dos_to_filetime(0x21, invalid), None);
        }
    }

    #[test]
    fn dos_handlers_preserve_invalid_outputs_and_preflight_both_success_outputs() {
        use adv_cpu::backend::Prot;
        use adv_machine::{Clock, Machine, layout::PARK_ADDR};
        let mut machine = Machine::new(
            adv_cpu::JitCpu::new(PARK_ADDR, false).unwrap(),
            Registry::default(),
            Clock::virtual_time(),
        )
        .unwrap();
        machine.cpu.map(0x400000, 4096, Prot::RW).unwrap();
        machine.cpu.map(0x401000, 4096, Prot::READ).unwrap();
        let context = machine
            .spawn("dos-conversion", PARK_ADDR, &[], 0x10000)
            .unwrap();
        machine.sys.current = context;
        let mut env = Env {
            cpu: &mut machine.cpu,
            sys: &mut machine.sys,
            ext: &mut machine.ext,
            thunk: 0,
        };
        let input = GuestPtr::new(0x400000);
        let date = GuestPtr::new(0x400010);
        let time = GuestPtr::new(0x400012);
        let output = GuestPtr::new(0x400020);
        input.write(env.cpu, FILETIME::default()).unwrap();
        date.write(env.cpu, 0xcafe).unwrap();
        time.write(env.cpu, 0xbeef).unwrap();
        assert_eq!(
            file_time_to_dos_date_time(&mut env, input, date, time),
            Ok(0)
        );
        assert_eq!(
            (date.read(env.cpu).unwrap(), time.read(env.cpu).unwrap()),
            (0xcafe, 0xbeef)
        );
        assert_eq!(crate::util::last_error(&env), 87);
        output
            .write(
                env.cpu,
                FILETIME {
                    dwLowDateTime: 0xdeadbeef,
                    dwHighDateTime: 0xcafebabe,
                },
            )
            .unwrap();
        assert_eq!(dos_date_time_to_file_time(&mut env, 0x5e, 0, output), Ok(0));
        let intact = output.read(env.cpu).unwrap();
        assert_eq!(
            (intact.dwLowDateTime, intact.dwHighDateTime),
            (0xdeadbeef, 0xcafebabe)
        );
        let first = dos_to_filetime(0x21, 0).unwrap();
        input
            .write(
                env.cpu,
                FILETIME {
                    dwLowDateTime: first as u32,
                    dwHighDateTime: (first >> 32) as u32,
                },
            )
            .unwrap();
        let fault = file_time_to_dos_date_time(&mut env, input, date, GuestPtr::new(0x401000)).unwrap_err();
        assert_eq!(fault.addr, 0x401000);
        assert_eq!(date.read(env.cpu).unwrap(), 0xcafe);
        assert_eq!(
            dos_date_time_to_file_time(&mut env, 0xabcd0021, 0xffff0000, output),
            Ok(1),
            "WORD arguments ignore their unused upper ABI bits"
        );
    }
}
