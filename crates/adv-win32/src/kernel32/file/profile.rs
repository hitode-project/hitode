//! File-backed INI queries over the existing VFS/FileObj/coherent reader.
//! No host Windows profile/registry or parallel file/cache owner is consulted.
use super::*;
use adv_abi::{AnsiStr, GuestPtr, WideStr, guest::check_range, win32};
use adv_cpu::backend::{MemFault, Prot};

#[cfg(test)]
mod complete_tests;
mod query;
#[cfg(test)]
mod tests;
mod write;
pub(super) fn write_string(env: &mut Env<'_>, wide: bool) -> Outcome {
    write::string(env, wide, true)
}

const MAX_FILE_BYTES: usize = 16 * 1024 * 1024;
const MAX_INPUT_UNITS: usize = 65536;

fn supported_cp(env: &Env<'_>) -> bool {
    matches!(env.ext.get::<Win32>().codec.acp, 932 | 936 | 1252 | 65001)
}
fn unavailable(env: &mut Env<'_>, api: &'static str, reason: &str, error: u32) -> Outcome {
    adv_abi::warn_unavailable(env, api, reason);
    set_last_error(env, error);
    Outcome::Return(0)
}
fn text(env: &Env<'_>, pointer: u32, wide: bool) -> Result<Option<Vec<u16>>, MemFault> {
    if pointer == 0 {
        return Ok(None);
    }
    if wide {
        WideStr(GuestPtr::new(pointer))
            .read(env.cpu, MAX_INPUT_UNITS)
            .map(Some)
    } else {
        AnsiStr(GuestPtr::new(pointer))
            .read(env.cpu, MAX_INPUT_UNITS)
            .map(|bytes| Some(env.ext.get::<Win32>().codec.to_utf16(0, &bytes)))
    }
}
fn path(name: Option<Vec<u16>>) -> Result<String, ()> {
    let name = match name {
        Some(name) => String::from_utf16(&name).map_err(|_| ())?,
        None => "win.ini".to_owned(),
    };
    Ok(
        if name.contains(['\\', '/']) || name.as_bytes().get(1) == Some(&b':') {
            name
        } else {
            format!("C:\\Windows\\{name}")
        },
    )
}

enum ReadError {
    Io(u32),
    Unsupported(&'static str, u32),
}
fn contents(env: &mut Env<'_>, filename: &str) -> Result<Vec<u8>, ReadError> {
    let handle = open_with_share(env, filename, 0x80000000, OPEN_EXISTING, 7);
    if handle == INVALID_HANDLE_VALUE {
        return Err(ReadError::Io(crate::util::last_error(env)));
    }
    let result = (|| {
        let sections = &env.ext.get::<Win32>().vfs.sections;
        let file = env
            .sys
            .objects
            .host_mut::<FileObj>(handle)
            .expect("temporary profile FileObj");
        let mut bytes = Vec::new();
        let mut buffer = [0; 32768];
        loop {
            let length = buffer.len().min(MAX_FILE_BYTES + 1 - bytes.len());
            let count = read_coherent(file, sections, &mut buffer[..length])
                .map_err(|error| ReadError::Io(io_code(&error)))?;
            bytes.extend_from_slice(&buffer[..count]);
            if bytes.len() > MAX_FILE_BYTES {
                return Err(ReadError::Unsupported(
                    "profile file exceeds the supported 16MiB bounded-reader budget",
                    50,
                ));
            }
            if count == 0 {
                return Ok(bytes);
            }
        }
    })();
    // No callback or guest handle escapes this synchronous read. Closing the
    // temporary alias releases the same FileObj owner used by ordinary I/O.
    env.sys.objects.close(handle);
    result
}

type ReadProfile = Box<dyn FnOnce(&mut Env<'_>, Result<Vec<u8>, ReadError>) -> Outcome>;

fn with_contents(env: &mut Env<'_>, filename: String, then: ReadProfile) -> Outcome {
    match super::native::active(env) {
        Ok(false) => {
            let result = contents(env, &filename);
            then(env, result)
        }
        Ok(true) => {
            let filename = env.ext.get::<Win32>().vfs.normalize(&filename);
            super::native::read(
                env,
                filename,
                MAX_FILE_BYTES,
                Box::new(move |env, result| {
                    let result = result.map_err(|status| {
                        if status == super::native::FILE_TOO_LARGE {
                            ReadError::Unsupported(
                                "profile file exceeds the supported 16MiB bounded-reader budget",
                                50,
                            )
                        } else {
                            ReadError::Io(super::native::win32_error(status))
                        }
                    });
                    then(env, result)
                }),
            )
        }
        Err(_) => then(
            env,
            Err(ReadError::Unsupported(
                "native file hook export is invalid or requires unsupported forwarded initialization",
                50,
            )),
        ),
    }
}
fn decode(env: &Env<'_>, bytes: &[u8]) -> Result<Vec<u16>, ReadError> {
    if bytes.starts_with(&[0xff, 0xfe, 0, 0])
        || bytes.starts_with(&[0xfe, 0xff])
        || bytes.starts_with(&[0xef, 0xbb, 0xbf])
    {
        return Err(ReadError::Unsupported(
            "UTF32, UTF16BE and UTF8-BOM profile file formats are unavailable (supported: guest ACP bytes or UTF16LE BOM)",
            50,
        ));
    }
    if let Some(bytes) = bytes.strip_prefix(&[0xff, 0xfe]) {
        if bytes.len() & 1 != 0 {
            return Err(ReadError::Unsupported(
                "UTF16LE profile contains an incomplete code unit",
                1113,
            ));
        }
        return Ok(bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .collect());
    }
    if !supported_cp(env) {
        return Err(ReadError::Unsupported(
            "profile ANSI file decoding requires a supported guest ACP (932/936/1252/65001)",
            50,
        ));
    }
    Ok(env.ext.get::<Win32>().codec.to_utf16(0, bytes))
}

struct Ini {
    lines: Vec<Vec<u16>>,
}
fn space(unit: u16) -> bool {
    matches!(unit, 9 | 32)
}
fn trim(units: &[u16]) -> &[u16] {
    let start = units.iter().position(|&u| !space(u)).unwrap_or(units.len());
    let end = units
        .iter()
        .rposition(|&u| !space(u))
        .map_or(start, |i| i + 1);
    &units[start..end]
}
fn section(line: &[u16]) -> Option<&[u16]> {
    let line = trim(line);
    if line.first() != Some(&91) {
        return None;
    }
    let end = line.iter().position(|&u| u == 93)?;
    Some(trim(&line[1..end]))
}
fn same(a: &[u16], b: &[u16]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|(&a, &b)| adv_os::nls::unit_case(a, true) == adv_os::nls::unit_case(b, true))
}
impl Ini {
    fn new(units: &[u16]) -> Self {
        let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
        Self {
            lines: units[..end]
                .split(|&u| u == 10)
                .flat_map(|line| line.strip_suffix(&[13]).unwrap_or(line).split(|&u| u == 13))
                .map(<[u16]>::to_vec)
                .collect(),
        }
    }
    fn query(&self, app: Option<&[u16]>, key: Option<&[u16]>) -> Option<Answer> {
        let Some(app) = app else {
            let sections: Vec<_> = self
                .lines
                .iter()
                .filter_map(|line| section(line).map(<[u16]>::to_vec))
                .collect();
            return (!sections.is_empty()).then_some(Answer::List(sections));
        };
        let start = self
            .lines
            .iter()
            .position(|line| section(line).is_some_and(|s| same(s, app)))?
            + 1;
        let end = self.lines[start..]
            .iter()
            .position(|line| section(line).is_some())
            .map_or(self.lines.len(), |i| start + i);
        let entries = self.lines[start..end].iter().filter_map(|line| {
            let line = trim(line);
            if line.first() == Some(&59) {
                return None;
            }
            let split = line.iter().position(|&u| u == 61)?;
            Some((trim(&line[..split]), trim(&line[split + 1..])))
        });
        if let Some(key) = key {
            let (_, value) = entries.into_iter().find(|(name, _)| same(name, key))?;
            let value = if value.len() >= 2
                && matches!(value[0], 34 | 39)
                && value.last() == Some(&value[0])
            {
                &value[1..value.len() - 1]
            } else {
                value
            };
            Some(Answer::Scalar(value.to_vec()))
        } else {
            Some(Answer::List(entries.map(|(key, _)| key.to_vec()).collect()))
        }
    }
}
enum Answer {
    Scalar(Vec<u16>),
    List(Vec<Vec<u16>>),
}
fn render(
    env: &Env<'_>,
    answer: Answer,
    capacity: u32,
    wide: bool,
    fallback: bool,
) -> (Vec<u8>, u32, u32) {
    let encode = |units: &[u16]| -> Vec<u32> {
        if wide {
            units.iter().map(|&u| u32::from(u)).collect()
        } else {
            env.ext
                .get::<Win32>()
                .codec
                .from_utf16(0, units)
                .0
                .into_iter()
                .map(u32::from)
                .collect()
        }
    };
    let (mut units, list) = match answer {
        Answer::Scalar(units) => (encode(&units), false),
        Answer::List(items) => {
            let mut units = Vec::new();
            for item in items {
                units.extend(encode(&item));
                units.push(0);
            }
            (units, true)
        }
    };
    let required = units.len().max(usize::from(list)) + 1;
    let truncated = required > capacity as usize;
    let copied = if truncated {
        capacity.saturating_sub(if list { 2 } else { 1 }) as usize
    } else {
        units.len()
    };
    units.truncate(copied);
    if capacity != 0 {
        units.push(0);
        if list && truncated && capacity > 1 {
            units.push(0);
        }
    }
    let bytes = if wide {
        units
            .into_iter()
            .flat_map(|u| (u as u16).to_le_bytes())
            .collect()
    } else {
        units.into_iter().map(|u| u as u8).collect()
    };
    // Missing-item defaults retain their lookup error, including an empty
    // default and nSize0. Present values/enumerations report MORE_DATA on trim.
    (
        bytes,
        copied as u32,
        if fallback {
            2
        } else if truncated {
            234
        } else {
            0
        },
    )
}

fn get(
    env: &mut Env<'_>,
    pointers: [u32; 6],
    wide: bool,
    sections_only: bool,
    api: &'static str,
) -> Outcome {
    let [app, key, default, out, capacity, filename] = pointers;
    let bytes = match capacity.checked_mul(if wide { 2 } else { 1 }) {
        Some(bytes) => bytes,
        None => {
            set_last_error(env, 998);
            return Outcome::Return(0);
        }
    };
    if check_range(env.cpu, out, bytes, Prot::WRITE).is_err() {
        set_last_error(env, 998);
        return Outcome::Return(0);
    }
    if !wide && !supported_cp(env) {
        return unavailable(
            env,
            api,
            "profile ANSI argument/output conversion requires a supported guest ACP (932/936/1252/65001)",
            50,
        );
    }
    let app = match text(env, app, wide) {
        Ok(app) => app,
        Err(_) => {
            set_last_error(env, 998);
            return Outcome::Return(0);
        }
    };
    let key = if app.is_none() {
        None
    } else {
        match text(env, key, wide) {
            Ok(key) => key,
            Err(_) => {
                set_last_error(env, 998);
                return Outcome::Return(0);
            }
        }
    };
    let filename = match text(env, filename, wide) {
        Ok(name) => name,
        Err(_) => {
            set_last_error(env, 998);
            return Outcome::Return(0);
        }
    };
    let filename = match path(filename) {
        Ok(name) => name,
        Err(()) => {
            return unavailable(
                env,
                api,
                "profile filename contains an invalid UTF16 path",
                1113,
            );
        }
    };
    with_contents(
        env,
        filename,
        Box::new(move |env, result| {
            let data = match result {
                Ok(bytes) => match decode(env, &bytes) {
                    Ok(data) => Some(data),
                    Err(ReadError::Unsupported(reason, error)) => {
                        return unavailable(env, api, reason, error);
                    }
                    Err(ReadError::Io(_)) => unreachable!(),
                },
                Err(ReadError::Io(error)) => {
                    if sections_only {
                        return output(
                            env,
                            Answer::List(Vec::new()),
                            capacity,
                            out,
                            wide,
                            Some(error),
                        );
                    }
                    let default = match text(env, default, wide) {
                        Ok(default) => default.unwrap_or_default(),
                        Err(_) => {
                            set_last_error(env, 998);
                            return Outcome::Return(0);
                        }
                    };
                    return output(
                        env,
                        Answer::Scalar(strip_default(default)),
                        capacity,
                        out,
                        wide,
                        Some(error),
                    );
                }
                Err(ReadError::Unsupported(reason, error)) => {
                    return unavailable(env, api, reason, error);
                }
            };
            let answer = Ini::new(&data.unwrap()).query(app.as_deref(), key.as_deref());
            match answer {
                Some(answer) => output(env, answer, capacity, out, wide, None),
                None if sections_only => {
                    output(env, Answer::List(Vec::new()), capacity, out, wide, None)
                }
                None => {
                    let default = match text(env, default, wide) {
                        Ok(default) => default.unwrap_or_default(),
                        Err(_) => {
                            set_last_error(env, 998);
                            return Outcome::Return(0);
                        }
                    };
                    output(
                        env,
                        Answer::Scalar(strip_default(default)),
                        capacity,
                        out,
                        wide,
                        Some(2),
                    )
                }
            }
        }),
    )
}
fn strip_default(mut default: Vec<u16>) -> Vec<u16> {
    while default.last() == Some(&32) {
        default.pop();
    }
    default
}
fn output(
    env: &mut Env<'_>,
    answer: Answer,
    capacity: u32,
    out: u32,
    wide: bool,
    lookup_error: Option<u32>,
) -> Outcome {
    let (bytes, length, error) = render(env, answer, capacity, wide, lookup_error.is_some());
    // Guest file callbacks may change the caller's output mapping while the
    // profile operation is suspended. Revalidate before publishing any bytes.
    if !bytes.is_empty()
        && (check_range(env.cpu, out, bytes.len() as u32, Prot::WRITE).is_err()
            || env.cpu.write(out, &bytes).is_err())
    {
        set_last_error(env, 998);
        return Outcome::Return(0);
    }
    set_last_error(env, lookup_error.unwrap_or(error));
    Outcome::Return(length)
}

#[win32(kernel32,name="GetPrivateProfileStringA",level=implement)]
fn get_a(
    env: &mut Env<'_>,
    app: AnsiStr,
    key: AnsiStr,
    default: AnsiStr,
    out: AnsiStr,
    capacity: u32,
    filename: AnsiStr,
) -> Outcome {
    get(
        env,
        [
            app.0.addr(),
            key.0.addr(),
            default.0.addr(),
            out.0.addr(),
            capacity,
            filename.0.addr(),
        ],
        false,
        false,
        "kernel32.dll!GetPrivateProfileStringA",
    )
}
#[win32(kernel32,name="GetPrivateProfileStringW",level=implement)]
fn get_w(
    env: &mut Env<'_>,
    app: WideStr,
    key: WideStr,
    default: WideStr,
    out: WideStr,
    capacity: u32,
    filename: WideStr,
) -> Outcome {
    get(
        env,
        [
            app.0.addr(),
            key.0.addr(),
            default.0.addr(),
            out.0.addr(),
            capacity,
            filename.0.addr(),
        ],
        true,
        false,
        "kernel32.dll!GetPrivateProfileStringW",
    )
}

fn integer(units: &[u16]) -> u32 {
    let start = units
        .iter()
        .position(|&unit| !matches!(unit, 9..=13 | 32))
        .unwrap_or(units.len());
    let units = &units[start..];
    let (negative, units) = match units.first() {
        Some(45) => (true, &units[1..]),
        Some(43) => (false, &units[1..]),
        _ => (false, units),
    };
    let (base, units) = if matches!(units, [48, 120 | 88, ..]) {
        (16u32, &units[2..])
    } else {
        (10u32, units)
    };
    let mut value = 0u32;
    for &unit in units {
        let digit = match unit {
            48..=57 => u32::from(unit - 48),
            65..=70 => u32::from(unit - 65) + 10,
            97..=102 => u32::from(unit - 97) + 10,
            _ => break,
        };
        if digit >= base {
            break;
        }
        value = value.wrapping_mul(base).wrapping_add(digit);
    }
    if negative {
        value.wrapping_neg()
    } else {
        value
    }
}

fn get_int(env: &mut Env<'_>, pointers: [u32; 4], wide: bool, api: &'static str) -> Outcome {
    let [app, key, default, filename] = pointers;
    if !wide && !supported_cp(env) {
        unavailable(
            env,
            api,
            "profile ANSI argument conversion requires a supported guest ACP (932/936/1252/65001)",
            50,
        );
        return Outcome::Return(default);
    }
    let names = [app, key, filename].map(|pointer| text(env, pointer, wide));
    let [Ok(app), Ok(key), Ok(filename)] = names else {
        set_last_error(env, 998);
        return Outcome::Return(default);
    };
    let filename = match path(filename) {
        Ok(filename) => filename,
        Err(()) => {
            unavailable(
                env,
                api,
                "profile filename contains an invalid UTF16 path",
                1113,
            );
            return Outcome::Return(default);
        }
    };
    with_contents(
        env,
        filename,
        Box::new(move |env, result| {
            let data = match result.and_then(|bytes| decode(env, &bytes)) {
                Ok(data) => data,
                Err(ReadError::Io(error)) => {
                    set_last_error(env, error);
                    return Outcome::Return(default);
                }
                Err(ReadError::Unsupported(reason, error)) => {
                    unavailable(env, api, reason, error);
                    return Outcome::Return(default);
                }
            };
            let (Some(app), Some(key)) = (app, key) else {
                set_last_error(env, 2);
                return Outcome::Return(default);
            };
            let Some(Answer::Scalar(value)) = Ini::new(&data).query(Some(&app), Some(&key)) else {
                set_last_error(env, 2);
                return Outcome::Return(default);
            };
            set_last_error(env, 0);
            // A present empty value uses the caller's default but retains lookup success.
            Outcome::Return(if value.is_empty() {
                default
            } else {
                integer(&value)
            })
        }),
    )
}

// https://learn.microsoft.com/windows/win32/api/winbase/nf-winbase-getprivateprofileintw
#[win32(kernel32, name = "GetPrivateProfileIntA", level = implement)]
fn int_a(
    env: &mut Env<'_>,
    app: AnsiStr,
    key: AnsiStr,
    default: i32,
    filename: AnsiStr,
) -> Outcome {
    get_int(
        env,
        [
            app.0.addr(),
            key.0.addr(),
            default as u32,
            filename.0.addr(),
        ],
        false,
        "kernel32.dll!GetPrivateProfileIntA",
    )
}
#[win32(kernel32, name = "GetPrivateProfileIntW", level = implement)]
fn int_w(
    env: &mut Env<'_>,
    app: WideStr,
    key: WideStr,
    default: i32,
    filename: WideStr,
) -> Outcome {
    get_int(
        env,
        [
            app.0.addr(),
            key.0.addr(),
            default as u32,
            filename.0.addr(),
        ],
        true,
        "kernel32.dll!GetPrivateProfileIntW",
    )
}
// https://learn.microsoft.com/windows/win32/api/winbase/nf-winbase-getprofileintw
#[win32(kernel32, name = "GetProfileIntA", level = implement)]
fn profile_int_a(env: &mut Env<'_>, app: AnsiStr, key: AnsiStr, default: i32) -> Outcome {
    get_int(
        env,
        [app.0.addr(), key.0.addr(), default as u32, 0],
        false,
        "kernel32.dll!GetProfileIntA",
    )
}
#[win32(kernel32, name = "GetProfileIntW", level = implement)]
fn profile_int_w(env: &mut Env<'_>, app: WideStr, key: WideStr, default: i32) -> Outcome {
    get_int(
        env,
        [app.0.addr(), key.0.addr(), default as u32, 0],
        true,
        "kernel32.dll!GetProfileIntW",
    )
}
#[win32(kernel32, name = "GetProfileStringA", level = implement)]
fn profile_string_a(
    env: &mut Env<'_>,
    app: AnsiStr,
    key: AnsiStr,
    default: AnsiStr,
    out: AnsiStr,
    capacity: u32,
) -> Outcome {
    get(
        env,
        [
            app.0.addr(),
            key.0.addr(),
            default.0.addr(),
            out.0.addr(),
            capacity,
            0,
        ],
        false,
        false,
        "kernel32.dll!GetProfileStringA",
    )
}
#[win32(kernel32, name = "GetProfileStringW", level = implement)]
fn profile_string_w(
    env: &mut Env<'_>,
    app: WideStr,
    key: WideStr,
    default: WideStr,
    out: WideStr,
    capacity: u32,
) -> Outcome {
    get(
        env,
        [
            app.0.addr(),
            key.0.addr(),
            default.0.addr(),
            out.0.addr(),
            capacity,
            0,
        ],
        true,
        false,
        "kernel32.dll!GetProfileStringW",
    )
}

#[win32(kernel32, name = "GetPrivateProfileSectionNamesA", level = implement)]
fn sections_a(env: &mut Env<'_>, out: AnsiStr, capacity: u32, filename: AnsiStr) -> Outcome {
    get(
        env,
        [0, 0, 0, out.0.addr(), capacity, filename.0.addr()],
        false,
        true,
        "kernel32.dll!GetPrivateProfileSectionNamesA",
    )
}

#[win32(kernel32, name = "GetPrivateProfileSectionNamesW", level = implement)]
fn sections_w(env: &mut Env<'_>, out: WideStr, capacity: u32, filename: WideStr) -> Outcome {
    get(
        env,
        [0, 0, 0, out.0.addr(), capacity, filename.0.addr()],
        true,
        true,
        "kernel32.dll!GetPrivateProfileSectionNamesW",
    )
}

pub(super) fn register(registry: &mut Registry) {
    query::register(registry);
    registry.register(get_a_ABI);
    registry.register(get_w_ABI);
    registry.register(sections_a_ABI);
    registry.register(sections_w_ABI);
    registry.register(int_a_ABI);
    registry.register(int_w_ABI);
    registry.register(profile_int_a_ABI);
    registry.register(profile_int_w_ABI);
    registry.register(profile_string_a_ABI);
    registry.register(profile_string_w_ABI);
}
