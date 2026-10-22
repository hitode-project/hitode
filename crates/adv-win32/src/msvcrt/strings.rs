//! Byte and UTF-16 string operations share checked guest access and version
//! export selection. Character counts are code units, independent of locale.
use super::*;
use adv_abi::{GuestPtr, guest::memory_failure};
use adv_cpu::backend::MemFault;

const NAMES: &[&str] = &[
    "strlen", "strnlen", "strcmp", "strncmp", "strchr", "strrchr", "strstr", "strcpy", "strncpy",
    "strcat", "strncat", "wcslen", "wcsnlen", "wcscmp", "wcsncmp", "wcschr", "wcsrchr", "wcsstr",
    "wcscpy", "wcsncpy", "wcscat", "wcsncat",
];
pub(super) fn register(registry: &mut Registry) {
    let exports: Vec<_> = NAMES
        .iter()
        .map(|&name| (name, call as adv_machine::Handler))
        .collect();
    versions::register(registry, &exports);
}
pub(super) fn read(env: &Env<'_>, address: u32, width: u32) -> Result<u32, MemFault> {
    if width == 1 {
        GuestPtr::<u8>::new(address).read(env.cpu).map(u32::from)
    } else {
        GuestPtr::<u16>::new(address).read(env.cpu).map(u32::from)
    }
}
pub(super) fn write(
    env: &mut Env<'_>,
    address: u32,
    value: u32,
    width: u32,
) -> Result<(), MemFault> {
    if width == 1 {
        GuestPtr::new(address).write(env.cpu, value as u8)
    } else {
        GuestPtr::new(address).write(env.cpu, value as u16)
    }
}
pub(super) fn advance(address: u32, width: u32) -> Result<u32, MemFault> {
    address.checked_add(width).ok_or(MemFault {
        addr: address,
        len: width.saturating_add(1),
    })
}
pub(super) fn length(
    env: &Env<'_>,
    mut address: u32,
    width: u32,
    limit: Option<u32>,
) -> Result<u32, MemFault> {
    let mut count = 0;
    loop {
        if limit == Some(count) || read(env, address, width)? == 0 {
            return Ok(count);
        }
        count += 1;
        if limit == Some(count) {
            return Ok(count);
        }
        address = advance(address, width)?;
    }
}
fn compare(
    env: &Env<'_>,
    mut left: u32,
    mut right: u32,
    width: u32,
    limit: u32,
) -> Result<u32, MemFault> {
    for index in 0..limit {
        let (a, b) = (read(env, left, width)?, read(env, right, width)?);
        if a != b || a == 0 {
            return Ok((a as i32 - b as i32) as u32);
        }
        if index + 1 != limit {
            left = advance(left, width)?;
            right = advance(right, width)?;
        }
    }
    Ok(0)
}
fn find(
    env: &Env<'_>,
    mut address: u32,
    value: u32,
    width: u32,
    reverse: bool,
) -> Result<u32, MemFault> {
    let value = if width == 1 {
        value & 255
    } else {
        value & 65535
    };
    let mut result = 0;
    loop {
        let unit = read(env, address, width)?;
        if unit == value {
            if !reverse {
                return Ok(address);
            }
            result = address;
        }
        if unit == 0 {
            return Ok(result);
        }
        address = advance(address, width)?;
    }
}
fn substring(
    env: &Env<'_>,
    mut haystack: u32,
    mut needle: u32,
    width: u32,
) -> Result<u32, MemFault> {
    // KMP keeps repeated-prefix searches linear and never reads past a match
    // or terminating code unit in the haystack.
    let mut units = Vec::new();
    loop {
        let unit = read(env, needle, width)?;
        if unit == 0 {
            break;
        }
        units.push(unit);
        needle = advance(needle, width)?;
    }
    if units.is_empty() {
        return Ok(haystack);
    }
    let mut prefix = vec![0; units.len()];
    for i in 1..units.len() {
        let mut j = prefix[i - 1];
        while j != 0 && units[i] != units[j] {
            j = prefix[j - 1];
        }
        if units[i] == units[j] {
            j += 1;
        }
        prefix[i] = j;
    }
    let mut matched = 0;
    loop {
        let unit = read(env, haystack, width)?;
        if unit == 0 {
            return Ok(0);
        }
        while matched != 0 && unit != units[matched] {
            matched = prefix[matched - 1];
        }
        if unit == units[matched] {
            matched += 1;
        }
        if matched == units.len() {
            return Ok(haystack - (matched as u32 - 1) * width);
        }
        haystack = advance(haystack, width)?;
    }
}
fn copy(
    env: &mut Env<'_>,
    destination: u32,
    mut source: u32,
    width: u32,
    limit: u32,
    bounded: bool,
    append: bool,
) -> Result<u32, MemFault> {
    let mut output = if append {
        let count = length(env, destination, width, None)?;
        destination
            .checked_add(count.checked_mul(width).ok_or(MemFault {
                addr: destination,
                len: u32::MAX,
            })?)
            .ok_or(MemFault {
                addr: destination,
                len: u32::MAX,
            })?
    } else {
        destination
    };
    let mut remaining = limit;
    while remaining != 0 {
        let unit = read(env, source, width)?;
        write(env, output, unit, width)?;
        remaining -= 1;
        if unit == 0 {
            if bounded && !append {
                while remaining != 0 {
                    output = advance(output, width)?;
                    write(env, output, 0, width)?;
                    remaining -= 1;
                }
            }
            return Ok(destination);
        }
        if remaining != 0 || append {
            output = advance(output, width)?;
        }
        if remaining != 0 {
            source = advance(source, width)?;
        }
    }
    if append {
        write(env, output, 0, width)?;
    }
    Ok(destination)
}

pub(super) fn call(env: &mut Env<'_>) -> Outcome {
    let name = env.sys.thunks.get(env.thunk).name.clone();
    let width = if name.starts_with("wcs") { 2 } else { 1 };
    let operation = &name[3..];
    let (a, b, c) = (env.arg(0), env.arg(1), env.arg(2));
    let result = match operation {
        "len" => length(env, a, width, None),
        "nlen" => length(env, a, width, Some(b)),
        "cmp" => compare(env, a, b, width, u32::MAX),
        "ncmp" => compare(env, a, b, width, c),
        "chr" => find(env, a, b, width, false),
        "rchr" => find(env, a, b, width, true),
        "str" => substring(env, a, b, width),
        "cpy" | "ncpy" | "cat" | "ncat" => copy(
            env,
            a,
            b,
            width,
            if operation.starts_with('n') {
                c
            } else {
                u32::MAX
            },
            operation.starts_with('n'),
            operation.ends_with("cat"),
        ),
        _ => unreachable!("registered CRT string operation"),
    };
    match result {
        Ok(value) => Outcome::Return(value),
        Err(fault) => memory_failure(fault),
    }
}
