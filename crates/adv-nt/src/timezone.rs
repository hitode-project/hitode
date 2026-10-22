//! The host's time zone, as Wine's `get_timezone_info` derives it: the bias
//! and daylight saving transitions come from the host C library, and the
//! names from the entry of the registry's time zone database that matches
//! them.

use adv_machine::Env;

use crate::registry;
use crate::status::Error;

/// RTL_DYNAMIC_TIME_ZONE_INFORMATION, and the RTL_TIME_ZONE_INFORMATION that
/// begins it.
pub const DYNAMIC_SIZE: usize = 0x1b0;
pub const STATIC_SIZE: usize = 0xac;

const TIME_ZONES: &str =
    r"\Registry\Machine\Software\Microsoft\Windows NT\CurrentVersion\Time Zones";
const REG_SZ: u32 = 1;
const REG_BINARY: u32 = 3;
/// The registry's TZI value: three biases and two dates.
const TZI_SIZE: usize = 44;
/// The largest value `reg_query_value` reads.
const VALUE_LIMIT: usize = 244;

/// SYSTEMTIME.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Date {
    year: u16,
    month: u16,
    day_of_week: u16,
    day: u16,
    hour: u16,
    minute: u16,
    second: u16,
    milliseconds: u16,
}

impl Date {
    fn parse(bytes: &[u8]) -> Date {
        let at = |index: usize| u16::from_le_bytes([bytes[2 * index], bytes[2 * index + 1]]);
        Date {
            year: at(0),
            month: at(1),
            day_of_week: at(2),
            day: at(3),
            hour: at(4),
            minute: at(5),
            second: at(6),
            milliseconds: at(7),
        }
    }

    fn encode(&self) -> [u8; 16] {
        let mut bytes = [0u8; 16];
        let fields = [
            self.year,
            self.month,
            self.day_of_week,
            self.day,
            self.hour,
            self.minute,
            self.second,
            self.milliseconds,
        ];
        for (index, field) in fields.into_iter().enumerate() {
            bytes[2 * index..2 * index + 2].copy_from_slice(&field.to_le_bytes());
        }
        bytes
    }

    /// A transition at `time`, in UTC.
    fn at(time: libc::time_t) -> Date {
        let tm = gmtime(time);
        Date {
            year: (tm.tm_year + 1900) as u16,
            month: (tm.tm_mon + 1) as u16,
            day_of_week: tm.tm_wday as u16,
            day: tm.tm_mday as u16,
            hour: tm.tm_hour as u16,
            minute: tm.tm_min as u16,
            second: tm.tm_sec as u16,
            milliseconds: 0,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct TimeZone {
    bias: i32,
    standard_name: Vec<u16>,
    standard_date: Date,
    standard_bias: i32,
    daylight_name: Vec<u16>,
    daylight_date: Date,
    daylight_bias: i32,
    key_name: Vec<u16>,
}

impl TimeZone {
    /// RTL_DYNAMIC_TIME_ZONE_INFORMATION.
    pub fn encode(&self) -> [u8; DYNAMIC_SIZE] {
        let mut bytes = [0u8; DYNAMIC_SIZE];
        let mut text = |offset: usize, units: usize, text: &[u16]| {
            for (index, unit) in text.iter().take(units - 1).enumerate() {
                bytes[offset + 2 * index..offset + 2 * index + 2]
                    .copy_from_slice(&unit.to_le_bytes());
            }
        };
        text(0x04, 32, &self.standard_name);
        text(0x58, 32, &self.daylight_name);
        text(0xac, 128, &self.key_name);
        bytes[0x00..0x04].copy_from_slice(&self.bias.to_le_bytes());
        bytes[0x44..0x54].copy_from_slice(&self.standard_date.encode());
        bytes[0x54..0x58].copy_from_slice(&self.standard_bias.to_le_bytes());
        bytes[0x98..0xa8].copy_from_slice(&self.daylight_date.encode());
        bytes[0xa8..0xac].copy_from_slice(&self.daylight_bias.to_le_bytes());
        bytes
    }
}

/// The time zone last derived, valid while the year and bias hold.
struct Cached {
    year: i32,
    bias: i32,
    zone: TimeZone,
}

/// The host's current time zone.
pub fn current(env: &mut Env<'_>) -> Result<TimeZone, Error> {
    // SAFETY: time accepts a null pointer.
    let now = unsafe { libc::time(std::ptr::null_mut()) };
    let bias = ((mktime(&mut gmtime(now)) - now) / 60) as i32;
    let mut tm = localtime(now);
    if let Some(cached) = env.ext.try_get::<Cached>()
        && cached.year == tm.tm_year
        && cached.bias == bias
    {
        return Ok(cached.zone.clone());
    }
    let name = short_name(&tm);
    let year = tm.tm_year;
    let mut zone = TimeZone {
        bias,
        ..Default::default()
    };

    tm.tm_isdst = 0;
    tm.tm_mday = 1;
    (
        tm.tm_mon, tm.tm_hour, tm.tm_min, tm.tm_sec, tm.tm_wday, tm.tm_yday,
    ) = (0, 0, 0, 0, 0, 0);
    let year_start = mktime(&mut tm);
    (tm.tm_mday, tm.tm_wday, tm.tm_yday) = (0, 0, 0);
    (tm.tm_mon, tm.tm_hour, tm.tm_min, tm.tm_sec) = (12, 23, 59, 59);
    let year_end = mktime(&mut tm);

    let (mut daylight, mut standard) = (0, 0);
    let (first, is_daylight) = find_dst_change(year_start, year_end);
    *if is_daylight {
        &mut daylight
    } else {
        &mut standard
    } = first;
    let (second, is_daylight) = find_dst_change(first, year_end);
    *if is_daylight {
        &mut daylight
    } else {
        &mut standard
    } = second;
    if daylight != standard && daylight != 0 && standard != 0 {
        let bias = libc::time_t::from(zone.bias) * 60;
        zone.daylight_bias = -60;
        zone.daylight_date = Date::at(daylight - bias);
        zone.standard_date = Date::at(standard - bias + 60 * 60);
    }

    find_registry_entry(env, &mut zone, &name, year + 1900)?;
    env.ext.insert(Cached {
        year,
        bias,
        zone: zone.clone(),
    });
    Ok(zone)
}

/// The host's current bias from UTC in seconds, daylight saving included,
/// as the server's `set_user_shared_data_time` computes it.
pub fn bias_now() -> libc::time_t {
    // SAFETY: time accepts a null pointer.
    let now = unsafe { libc::time(std::ptr::null_mut()) };
    let mut bias = mktime(&mut gmtime(now)) - now;
    if localtime(now).tm_isdst > 0 {
        bias -= 3600;
    }
    bias
}

/// Names `zone` after the registry entry that matches it, if one does.
fn find_registry_entry(
    env: &mut Env<'_>,
    zone: &mut TimeZone,
    name: &str,
    year: i32,
) -> Result<(), Error> {
    let zones = match registry::open_for_reading(env, 0, TIME_ZONES) {
        Ok(key) => key,
        Err(Error::Status(_)) => return Ok(()),
        Err(error) => return Err(error),
    };
    let found = search(env, zones, zone, name, year);
    crate::server::close(env, zones)?;
    if let Some(entry) = found? {
        *zone = entry;
    }
    Ok(())
}

fn search(
    env: &mut Env<'_>,
    zones: u32,
    zone: &TimeZone,
    name: &str,
    year: i32,
) -> Result<Option<TimeZone>, Error> {
    let mut index = 0;
    while let Some(key_name) = registry::subkey(env, zones, index)? {
        index += 1;
        let subkey =
            match registry::open_for_reading(env, zones, &String::from_utf16_lossy(&key_name)) {
                Ok(key) => key,
                Err(Error::Status(_)) => continue,
                Err(error) => return Err(error),
            };
        let entry = read_entry(env, subkey, key_name, year);
        crate::server::close(env, subkey)?;
        if let Some(entry) = entry?
            && matches(zone, &entry)
            && matches_name(name, &entry)
        {
            return Ok(Some(entry));
        }
    }
    Ok(None)
}

/// A registry entry, as Wine's `find_reg_tz_info` reads it.
fn read_entry(
    env: &mut Env<'_>,
    key: u32,
    key_name: Vec<u16>,
    year: i32,
) -> Result<Option<TimeZone>, Error> {
    let text = |env: &mut Env<'_>, names: [&str; 2]| -> Result<Option<Vec<u16>>, Error> {
        for name in names {
            if let Some((REG_SZ, data)) = registry::value(env, key, name, VALUE_LIMIT)? {
                let units = data
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|&pair| u16::from_le_bytes(pair))
                    .take_while(|&unit| unit != 0)
                    .collect();
                return Ok(Some(units));
            }
        }
        Ok(None)
    };
    let Some(standard_name) = text(env, ["MUI_Std", "Std"])? else {
        return Ok(None);
    };
    let Some(daylight_name) = text(env, ["MUI_Dlt", "Dlt"])? else {
        return Ok(None);
    };
    // This year's entry under Dynamic DST comes first.
    let mut data = None;
    match registry::open_for_reading(env, key, "Dynamic DST") {
        Ok(dynamic) => {
            let value = registry::value(env, dynamic, &year.to_string(), VALUE_LIMIT);
            crate::server::close(env, dynamic)?;
            data = value?;
        }
        Err(Error::Status(_)) => {}
        Err(error) => return Err(error),
    }
    if !matches!(data, Some((REG_BINARY, _))) {
        data = registry::value(env, key, "TZI", VALUE_LIMIT)?;
    }
    let data = match data {
        Some((REG_BINARY, data)) => Some(data),
        _ => None,
    };
    let Some(mut data) = data else {
        return Ok(None);
    };
    data.resize(data.len().max(TZI_SIZE), 0);
    let long = |offset: usize| i32::from_le_bytes(data[offset..offset + 4].try_into().unwrap());
    Ok(Some(TimeZone {
        bias: long(0),
        standard_name,
        standard_date: Date::parse(&data[12..28]),
        standard_bias: long(4),
        daylight_name,
        daylight_date: Date::parse(&data[28..44]),
        daylight_bias: long(8),
        key_name,
    }))
}

fn matches(zone: &TimeZone, entry: &TimeZone) -> bool {
    zone.bias == entry.bias
        && matches_date(&zone.standard_date, &entry.standard_date)
        && matches_date(&zone.daylight_date, &entry.daylight_date)
}

fn matches_date(date: &Date, entry: &Date) -> bool {
    if date.month != entry.month {
        return false;
    }
    if date.month == 0 {
        // No transitions.
        return true;
    }
    let day = if entry.year == 0 {
        // The entry names the nth weekday of the month.
        weekday_to_day(
            i32::from(date.year) - 1900,
            i32::from(entry.day),
            i32::from(entry.month) - 1,
            i32::from(entry.day_of_week),
        ) as u16
    } else {
        entry.day
    };
    // 23:59:59.999 matches midnight of the following day.
    if entry.year == 0
        && (entry.hour, entry.minute, entry.second, entry.milliseconds) == (23, 59, 59, 999)
    {
        return date.day == day + 1
            && (date.hour, date.minute, date.second, date.milliseconds) == (0, 0, 0, 0);
    }
    date.day == day
        && (date.hour, date.minute, date.second, date.milliseconds)
            == (entry.hour, entry.minute, entry.second, entry.milliseconds)
}

/// Zones without daylight saving that share a bias, told apart by the
/// host's abbreviation and their bias in the past.
fn matches_name(name: &str, entry: &TimeZone) -> bool {
    const ZONES: &[(&str, &str, libc::time_t, i32)] = &[
        // 2016-01-01 00:00:00 UTC.
        ("North Korea Standard Time", "KST", 1_451_606_400, -510),
        ("Korea Standard Time", "KST", 1_451_606_400, -540),
        ("Tokyo Standard Time", "JST", 0, 0),
        // YAKST until tzdata 2016f.
        ("Yakutsk Standard Time", "+09", 0, 0),
    ];
    if entry.daylight_date.month != 0 {
        return true;
    }
    let key = String::from_utf16_lossy(&entry.key_name);
    match ZONES.iter().find(|zone| zone.0 == key) {
        Some(&(_, short, past, past_bias)) => {
            short == name
                && (past == 0 || (mktime(&mut gmtime(past)) - past) / 60 == past_bias.into())
        }
        None => true,
    }
}

/// The day of the month of the `day`th (5 = last) `day_of_week` of
/// `month`, in C library terms.
fn weekday_to_day(year: i32, day: i32, month: i32, day_of_week: i32) -> i32 {
    // SAFETY: an all-zero tm is a valid value.
    let mut date: libc::tm = unsafe { std::mem::zeroed() };
    date.tm_year = year;
    date.tm_mon = month;
    date.tm_mday = -1;
    date.tm_wday = -1;
    loop {
        date.tm_mday += 1;
        mktime(&mut date);
        if date.tm_wday == day_of_week && date.tm_mon == month {
            break;
        }
    }
    let mut result = date.tm_mday;
    let mut nth = 1;
    while nth < day {
        date.tm_mday += 7;
        let tm = localtime(mktime(&mut date));
        if tm.tm_mon != month {
            break;
        }
        result = tm.tm_mday;
        nth += 1;
    }
    result
}

/// The first time in `start..=end` whose daylight saving differs from that
/// of `start`, and whether it is daylight saving time.
fn find_dst_change(start: libc::time_t, end: libc::time_t) -> (libc::time_t, bool) {
    let target = i32::from(localtime(start).tm_isdst == 0);
    let (mut low, mut high) = (start, end);
    while low <= high {
        let middle = low + (high - low) / 2;
        if localtime(middle).tm_isdst != target {
            low = middle + 1;
        } else {
            high = middle - 1;
        }
    }
    (low, target != 0)
}

fn short_name(tm: &libc::tm) -> String {
    let mut buffer = [0u8; 16];
    // SAFETY: the format is NUL-terminated and the buffer's size is given.
    let length =
        unsafe { libc::strftime(buffer.as_mut_ptr().cast(), buffer.len(), c"%Z".as_ptr(), tm) };
    String::from_utf8_lossy(&buffer[..length]).into_owned()
}

fn localtime(time: libc::time_t) -> libc::tm {
    // SAFETY: localtime_r fills the zeroed tm it is given.
    unsafe {
        let mut tm = std::mem::zeroed();
        libc::localtime_r(&time, &mut tm);
        tm
    }
}

fn gmtime(time: libc::time_t) -> libc::tm {
    // SAFETY: gmtime_r fills the zeroed tm it is given.
    unsafe {
        let mut tm = std::mem::zeroed();
        libc::gmtime_r(&time, &mut tm);
        tm
    }
}

fn mktime(tm: &mut libc::tm) -> libc::time_t {
    // SAFETY: a valid tm, which mktime normalizes in place.
    unsafe { libc::mktime(tm) }
}
