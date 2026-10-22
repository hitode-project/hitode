//! Time: all readings come from the machine clock (virtual or real).

use adv_abi::{GuestPtr, Registry, types::FILETIME, win32};
use adv_cpu::backend::{Guest, GuestExt};
use adv_machine::{CtxId, Env, Outcome, Wait};

use super::K32;
mod formatting;
mod timezone;
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
pub(crate) fn civil_from_days(z: i64) -> (i64, u32, u32) {
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

pub(crate) fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
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

fn local_systemtime(env: &mut Env<'_>) -> [u16; 8] {
    let ft = filetime_now(env) as i64 + local_offset_ft(env);
    filetime_to_systemtime(ft as u64)
}

/// Back-to-back reads which prove a context is only polling the clock. Bounded
/// per-item loops read the clock back to back too: Siglus stamps each of 256
/// keys, twice a frame, and a 64-read streak stretched its frames to 200 ms.
const POLL_STREAK: u32 = 2048;
/// Longest gap between two reads of one poll: a loop doing real work between
/// reads is not a busy-wait, however often it asks for the time.
const POLL_GAP_US: u64 = 50;

/// The last clock read and how many reads on that context came back to back,
/// with no other host call and almost no guest time between them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ClockPoll {
    context: Option<CtxId>,
    call: u64,
    read_us: u64,
    streak: u32,
}

impl ClockPoll {
    fn next_streak(&self, context: CtxId, call: u64, now: u64) -> u32 {
        if self.context == Some(context)
            && self.call + 1 == call
            && now.saturating_sub(self.read_us) <= POLL_GAP_US
        {
            self.streak.saturating_add(1)
        } else {
            1
        }
    }

    fn record(&mut self, context: CtxId, call: u64, now: u64) {
        self.streak = self.next_streak(context, call, now);
        (self.context, self.call, self.read_us) = (Some(context), call, now);
    }
}

/// Preview without advancing virtual time or committing real-clock progress.
/// A declined leaf must leave the ordinary path its original clock and streak.
fn leaf_clock(env: &Env<'_>) -> Option<(adv_machine::Clock, u64)> {
    let mut clock = env.sys.clock.clone();
    let now = clock.guest_now_us();
    // Dispatch increments host_calls after a successful leaf, before a trap.
    let (context, call) = (env.sys.current, env.sys.host_calls + 1);
    if env
        .ext
        .try_get::<ClockPoll>()
        .is_some_and(|poll| poll.next_streak(context, call, now) >= POLL_STREAK)
    {
        return None;
    }
    Some((clock, now))
}

fn record_leaf_clock(env: &mut Env<'_>, clock: adv_machine::Clock, now: u64) {
    env.sys.clock = clock;
    env.ext
        .get_or_default::<ClockPoll>()
        .record(env.sys.current, env.sys.host_calls + 1, now);
}

pub(crate) fn tick_leaf(env: &mut Env<'_>) -> Option<u64> {
    let (clock, us) = leaf_clock(env)?;
    record_leaf_clock(env, clock, us);
    Some((u64::from(adv_abi::leaf_edx(env)) << 32) | u64::from((us / 1000) as u32))
}

fn counter_leaf(env: &mut Env<'_>) -> Option<u64> {
    let out = GuestPtr::<i64>::new(env.cpu.arg(0).ok()?);
    adv_abi::guest::check_range(env.cpu, out.addr(), 8, adv_cpu::backend::Prot::WRITE).ok()?;
    let (clock, us) = leaf_clock(env)?;
    out.write(
        env.cpu,
        (us * (adv_machine::clock::QPC_FREQ / 1_000_000)) as i64,
    )
    .ok()?;
    record_leaf_clock(env, clock, us);
    Some((u64::from(adv_abi::leaf_edx(env)) << 32) | 1)
}

fn frequency_leaf(env: &mut Env<'_>) -> Option<u64> {
    let out = GuestPtr::<i64>::new(env.cpu.arg(0).ok()?);
    adv_abi::guest::check_range(env.cpu, out.addr(), 8, adv_cpu::backend::Prot::WRITE).ok()?;
    query_performance_frequency(env, out).ok()?;
    Some((u64::from(adv_abi::leaf_edx(env)) << 32) | 1)
}

/// Read the guest clock for a polling API. A context that does nothing but reread
/// the clock is busy-waiting; once a streak proves it, each read waits for the
/// next millisecond, as a preempted thread would observe, so the spin idles the
/// context and time skipping can reach the time it waits for.
pub(crate) fn poll_clock(
    env: &mut Env<'_>,
    finish: impl FnOnce(&mut Env<'_>, u64) -> Outcome + 'static,
) -> Outcome {
    let now = env.sys.clock.guest_now_us();
    let (context, call) = (env.sys.current, env.sys.host_calls);
    let poll = env.ext.get_or_default::<ClockPoll>();
    poll.record(context, call, now);
    if poll.streak < POLL_STREAK {
        return finish(env, now);
    }
    let next_ms = (now / 1000 + 1) * 1000;
    Outcome::block_timed(Wait::Until(next_ms), move |env, _| {
        let now = env.sys.clock.guest_now_us();
        let call = env.sys.host_calls;
        let poll = env.ext.get_or_default::<ClockPoll>();
        (poll.call, poll.read_us) = (call, now);
        finish(env, now)
    })
}

#[win32(kernel32, name = "GetTickCount", level = implement)]
fn get_tick_count(env: &mut Env<'_>) -> Outcome {
    poll_clock(env, |_, us| Outcome::Return((us / 1000) as u32))
}

#[win32(kernel32, name = "GetTickCount64", level = implement)]
fn get_tick_count64(env: &mut Env<'_>) -> Outcome {
    poll_clock(env, |_, us| Outcome::Return64(us / 1000))
}

#[win32(kernel32, name = "QueryPerformanceCounter", level = implement)]
fn query_performance_counter(env: &mut Env<'_>, out: GuestPtr<i64>) -> Outcome {
    poll_clock(env, move |env, us| {
        let t = us * (adv_machine::clock::QPC_FREQ / 1_000_000);
        match out.write(env.cpu, t as i64) {
            Ok(()) => Outcome::Return(1),
            Err(fault) => adv_abi::guest::memory_failure(fault),
        }
    })
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
    super::apc::sleep_front(env, ms, false, false)
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
    formatting::register(reg);
    timezone::register(reg);
    reg.register(get_tick_count64_ABI);
    reg.register(get_system_time_as_file_time_ABI);
    reg.register(file_time_to_dos_date_time_ABI);
    reg.register(dos_date_time_to_file_time_ABI);
    stdcall(reg, K32, "GetSystemTime", 1, get_system_time);
    stdcall(reg, K32, "GetLocalTime", 1, get_local_time);
    reg.register(adv_abi::Api {
        env_leaf: Some(tick_leaf),
        ..get_tick_count_ABI
    });
    reg.register(adv_abi::Api {
        env_leaf: Some(counter_leaf),
        ..query_performance_counter_ABI
    });
    reg.register(adv_abi::Api {
        env_leaf: Some(frequency_leaf),
        ..query_performance_frequency_ABI
    });
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
            context: machine.sys.current,
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
        let fault =
            file_time_to_dos_date_time(&mut env, input, date, GuestPtr::new(0x401000)).unwrap_err();
        assert_eq!(fault.addr, 0x401000);
        assert_eq!(date.read(env.cpu).unwrap(), 0xcafe);
        assert_eq!(
            dos_date_time_to_file_time(&mut env, 0xabcd0021, 0xffff0000, output),
            Ok(1),
            "WORD arguments ignore their unused upper ABI bits"
        );
    }
}

#[cfg(test)]
mod leaf_tests;
