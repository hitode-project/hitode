//! Every profile writer uses the query parser and the VFS file/coherence owner.
use super::*;
#[derive(Clone)]
enum Change {
    Key {
        key: Option<Vec<u16>>,
        value: Option<Vec<u16>>,
    },
    Section(Option<Vec<Vec<u16>>>),
}
impl Ini {
    pub(super) fn bounds(&self, app: &[u16]) -> Option<(usize, usize)> {
        let begin = self
            .lines
            .iter()
            .position(|line| section(line).is_some_and(|name| same(name, app)))?;
        let end = self.lines[begin + 1..]
            .iter()
            .position(|line| section(line).is_some())
            .map_or(self.lines.len(), |index| begin + 1 + index);
        Some((begin, end))
    }
    fn change(&mut self, app: &[u16], change: Change) {
        let bounds = self.bounds(app);
        match change {
            Change::Section(None) | Change::Key { key: None, .. } => {
                if let Some((begin, end)) = bounds {
                    self.lines.drain(begin..end);
                }
            }
            Change::Section(Some(lines)) => {
                if let Some((begin, end)) = bounds {
                    self.lines.splice(begin + 1..end, lines);
                } else {
                    self.lines.push([&[91], app, &[93]].concat());
                    self.lines.extend(lines);
                }
            }
            Change::Key {
                key: Some(key),
                value,
            } => {
                let entry = bounds.and_then(|(begin, end)| {
                    (begin + 1..end).find(|&index| {
                        let line = trim(&self.lines[index]);
                        line.first() != Some(&59)
                            && line
                                .iter()
                                .position(|&unit| unit == 61)
                                .is_some_and(|end| same(trim(&line[..end]), &key))
                    })
                });
                match (entry, value) {
                    (Some(index), None) => {
                        self.lines.remove(index);
                    }
                    (Some(index), Some(value)) => {
                        self.lines[index] = [&key[..], &[61], &value[..]].concat();
                    }
                    (None, Some(value)) => {
                        let index = if let Some((_, end)) = bounds {
                            end
                        } else {
                            self.lines.push([&[91], app, &[93]].concat());
                            self.lines.len()
                        };
                        self.lines
                            .insert(index, [&key[..], &[61], &value[..]].concat());
                    }
                    (None, None) => {}
                }
            }
        }
    }
}
fn failed(env: &mut Env<'_>, error: u32) -> Outcome {
    set_last_error(env, error);
    Outcome::Return(0)
}
fn multisz(env: &Env<'_>, pointer: u32, wide: bool) -> Result<Option<Vec<Vec<u16>>>, u32> {
    if pointer == 0 {
        return Ok(None);
    }
    let mut items = Vec::new();
    let mut consumed = 0u32;
    while consumed < MAX_INPUT_UNITS as u32 {
        let offset = consumed
            .checked_mul(if wide { 2 } else { 1 })
            .and_then(|offset| pointer.checked_add(offset))
            .ok_or(998u32)?;
        let item = text(env, offset, wide).map_err(|_| 998u32)?.unwrap();
        if item.is_empty() {
            return Ok(Some(items));
        }
        let length = if wide {
            item.len()
        } else {
            env.ext.get::<Win32>().codec.from_utf16(0, &item).0.len()
        };
        consumed = consumed.checked_add(length as u32 + 1).ok_or(998u32)?;
        items.push(item);
    }
    Err(87)
}
fn apply(
    env: &mut Env<'_>,
    app: Vec<u16>,
    filename: String,
    change: Change,
    api: &'static str,
) -> Outcome {
    let saved = filename.clone();
    with_contents(
        env,
        filename,
        Box::new(move |env, result| {
            let bytes = match result {
                Ok(bytes) => bytes,
                Err(ReadError::Io(2)) => Vec::new(),
                Err(ReadError::Io(error)) => return failed(env, error),
                Err(ReadError::Unsupported(reason, error)) => {
                    return unavailable(env, api, reason, error);
                }
            };
            let unicode = bytes.starts_with(&[0xff, 0xfe]);
            let units = match decode(env, &bytes) {
                Ok(units) => units,
                Err(ReadError::Io(error)) => return failed(env, error),
                Err(ReadError::Unsupported(reason, error)) => {
                    return unavailable(env, api, reason, error);
                }
            };
            let mut ini = Ini::new(&units);
            while ini.lines.last().is_some_and(Vec::is_empty) {
                ini.lines.pop();
            }
            ini.change(&app, change);
            let mut units = Vec::new();
            for line in ini.lines {
                units.extend(line);
                units.extend([13, 10]);
            }
            let bytes = if unicode {
                [0xff, 0xfe]
                    .into_iter()
                    .chain(units.into_iter().flat_map(u16::to_le_bytes))
                    .collect()
            } else {
                env.ext.get::<Win32>().codec.from_utf16(0, &units).0
            };
            replace(env, saved, bytes)
        }),
    )
}
pub(super) fn string(env: &mut Env<'_>, wide: bool, private: bool) -> Outcome {
    let pointers = [
        env.arg(0),
        env.arg(1),
        env.arg(2),
        if private { env.arg(3) } else { 0 },
    ];
    let [app, key, value, _] = pointers;
    if app == 0 && key == 0 && value == 0 {
        return failed(env, 0);
    }
    let [Ok(app), Ok(key), Ok(value), Ok(filename)] =
        pointers.map(|pointer| text(env, pointer, wide))
    else {
        return failed(env, 998);
    };
    let Some(app) = app else {
        return failed(env, 87);
    };
    let filename = match path(filename) {
        Ok(value) => value,
        Err(_) => return failed(env, 1113),
    };
    apply(
        env,
        app,
        filename,
        Change::Key { key, value },
        "kernel32.dll!WritePrivateProfileString",
    )
}
pub(super) fn section_write(env: &mut Env<'_>, wide: bool, private: bool) -> Outcome {
    let (app, value, filename) = (env.arg(0), env.arg(1), if private { env.arg(2) } else { 0 });
    let (Ok(Some(app)), Ok(filename)) = (text(env, app, wide), text(env, filename, wide)) else {
        return failed(env, 87);
    };
    let items = match multisz(env, value, wide) {
        Ok(items) => items,
        Err(error) => return failed(env, error),
    };
    let filename = match path(filename) {
        Ok(value) => value,
        Err(_) => return failed(env, 1113),
    };
    apply(
        env,
        app,
        filename,
        Change::Section(items),
        "kernel32.dll!WritePrivateProfileSection",
    )
}
pub(super) fn structure(env: &mut Env<'_>, wide: bool) -> Outcome {
    let pointers = [env.arg(0), env.arg(1), env.arg(4)];
    let (input, length) = (env.arg(2), env.arg(3));
    let [Ok(app), Ok(key), Ok(filename)] = pointers.map(|pointer| text(env, pointer, wide)) else {
        return failed(env, 998);
    };
    if pointers[0] == 0 && pointers[1] == 0 && input == 0 {
        return failed(env, 0);
    }
    let Some(app) = app else {
        return failed(env, 87);
    };
    let filename = match path(filename) {
        Ok(value) => value,
        Err(_) => return failed(env, 1113),
    };
    let value = if input == 0 || key.is_none() {
        None
    } else {
        if check_range(env.cpu, input, length, Prot::READ).is_err() {
            return failed(env, 998);
        }
        let Some(required) = (length as usize)
            .checked_add(1)
            .and_then(|length| length.checked_mul(2))
        else {
            return failed(env, 8);
        };
        let mut bytes = Vec::new();
        if bytes.try_reserve_exact(length as usize).is_err() {
            return failed(env, 8);
        }
        bytes.resize(length as usize, 0);
        if env.cpu.read(input, &mut bytes).is_err() {
            return failed(env, 998);
        }
        let checksum = bytes.iter().fold(0u8, |sum, &byte| sum.wrapping_add(byte));
        let mut value = Vec::new();
        if value.try_reserve_exact(required).is_err() {
            return failed(env, 8);
        }
        for byte in bytes.into_iter().chain([checksum]) {
            value.push(u16::from(b"0123456789ABCDEF"[(byte >> 4) as usize]));
            value.push(u16::from(b"0123456789ABCDEF"[(byte & 15) as usize]));
        }
        Some(value)
    };
    apply(
        env,
        app,
        filename,
        Change::Key { key, value },
        "kernel32.dll!WritePrivateProfileStruct",
    )
}
fn replace(env: &mut Env<'_>, filename: String, bytes: Vec<u8>) -> Outcome {
    if super::super::native::metadata::active(
        env,
        &[
            "NtCreateFile",
            "NtWriteFile",
            "NtSetInformationFile",
            "NtClose",
        ],
    ) {
        return native_replace(env, filename, bytes);
    }
    match replace_contents(
        env,
        &filename,
        bytes.len() as u64,
        false,
        |file, content| {
            if let Some(content) = content {
                content.write_to(file, 0, &bytes)
            } else {
                file.write_all(&bytes)
            }
        },
    ) {
        Ok(()) => {
            set_last_error(env, 0);
            Outcome::Return(1)
        }
        Err(error) => failed(env, error),
    }
}

struct Writer {
    file: u32,
    scratch: super::super::native::Attributes,
    bytes: Vec<u8>,
    offset: usize,
}
impl Writer {
    fn finish(self, env: &mut Env<'_>, result: Result<(), u32>) -> Outcome {
        super::super::native::operations::close(
            env,
            self.file,
            Box::new(move |env, closed| {
                self.scratch.free(env);
                match result.and(closed) {
                    Ok(()) => {
                        set_last_error(env, 0);
                        Outcome::Return(1)
                    }
                    Err(status) => failed(env, super::super::native::win32_error(status)),
                }
            }),
        )
    }
    fn next(mut self, env: &mut Env<'_>) -> Outcome {
        use super::super::native::operations;
        if self.offset == self.bytes.len() {
            let length = (self.bytes.len() as u64).to_le_bytes().to_vec();
            return operations::set_information(
                env,
                self.file,
                20,
                length,
                Box::new(move |env, result| self.finish(env, result)),
            );
        }
        let length = (self.bytes.len() - self.offset).min(65536);
        if env
            .cpu
            .write(
                self.scratch.extra,
                &self.bytes[self.offset..self.offset + length],
            )
            .is_err()
        {
            return self.finish(env, Err(0xc0000005));
        }
        operations::transfer(
            env,
            self.file,
            self.scratch.extra,
            length as u32,
            true,
            Box::new(move |env, result| match result {
                Ok(0) => self.finish(env, Err(0xc0000001)),
                Ok(count) => {
                    self.offset += count as usize;
                    self.next(env)
                }
                Err(status) => self.finish(env, Err(status)),
            }),
        )
    }
}
fn native_replace(env: &mut Env<'_>, filename: String, bytes: Vec<u8>) -> Outcome {
    use super::super::native::{attributes, operations};
    let filename = env.ext.get::<Win32>().vfs.normalize(&filename);
    if !super::super::native::metadata::active(env, &["NtCreateFile", "NtOpenFile"])
        && let Err(error) = replacement_allowed(env, &filename, bytes.len() as u64, false)
    {
        return failed(env, error);
    }
    let path = filename.clone();
    operations::open(
        env,
        &path,
        0x40100080,
        7,
        3,
        0x60,
        Box::new(move |env, result| {
            let file = match result {
                Ok(file) => file,
                Err(status) => return failed(env, super::super::native::win32_error(status)),
            };
            match attributes(env, &filename, 65536) {
                Ok(scratch) => Writer {
                    file,
                    scratch,
                    bytes,
                    offset: 0,
                }
                .next(env),
                Err(status) => operations::close(
                    env,
                    file,
                    Box::new(move |env, _| failed(env, super::super::native::win32_error(status))),
                ),
            }
        }),
    )
}
