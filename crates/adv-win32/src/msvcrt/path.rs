//! Secure CRT path splitting is lexical: no host path, filesystem, or locale
//! service is involved. Input is bounded and snapshotted before any output.
use super::*;
use adv_abi::{
    AnsiStr, GuestPtr, WideStr,
    guest::{check_range, memory_failure},
    win32,
};
use adv_cpu::backend::{MemFault, Prot};
use std::ops::Range;

const MODULES: [&str; 3] = ["msvcrt.dll", "msvcr80.dll", "msvcr90.dll"];
const LIMIT: usize = 65536;

pub(super) fn register(registry: &mut Registry) {
    for module in MODULES {
        for api in [split_a_ABI, split_w_ABI] {
            registry.register(adv_abi::Api { module, ..api });
        }
    }
}

#[derive(Clone, Copy)]
struct Output {
    address: u32,
    capacity: u32,
}
impl Output {
    fn present(self) -> bool {
        self.address != 0 && self.capacity != 0
    }
}

fn errno(env: &mut Env<'_>, value: i32) -> Outcome {
    match floating::errno_address(env).write(env.cpu, value) {
        Ok(()) => Outcome::Return(value as u32),
        Err(error) => memory_failure(error),
    }
}
fn unsupported(env: &mut Env<'_>, reason: &str) -> Outcome {
    let thunk = env.sys.thunks.get(env.thunk);
    let api = format!("{}!{}", thunk.module, thunk.name);
    adv_abi::warn_unavailable(env, &api, reason);
    errno(env, 40) // ENOSYS, without an invented invalid-parameter callback.
}
fn clear(env: &mut Env<'_>, outputs: &[Output; 4], width: u32) -> Result<(), MemFault> {
    for output in outputs.iter().filter(|output| output.present()) {
        env.cpu.write(output.address, &[0; 2][..width as usize])?;
    }
    Ok(())
}
fn write(env: &mut Env<'_>, output: Output, units: &[u16], width: u32) -> Result<(), MemFault> {
    if !output.present() {
        return Ok(());
    }
    let bytes: Vec<_> = if width == 2 {
        units
            .iter()
            .copied()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect()
    } else {
        units.iter().map(|&unit| unit as u8).chain([0]).collect()
    };
    env.cpu.write(output.address, &bytes)
}

pub(super) fn split_ranges(
    path: &[u16],
    lead: impl Fn(u16) -> bool,
) -> Result<[Range<usize>; 4], &'static str> {
    let drive = usize::from(path.get(1) == Some(&u16::from(b':'))) * 2;
    let mut directory = drive;
    let mut extension = None;
    let mut at = drive;
    while at < path.len() {
        if lead(path[at]) {
            if at + 1 == path.len() {
                return Err("incomplete multibyte path character is unavailable");
            }
            at += 2;
            continue;
        }
        match path[at] {
            47 | 92 => {
                directory = at + 1;
                extension = None;
            }
            46 => extension = Some(at),
            _ => {}
        }
        at += 1;
    }
    let extension = extension.unwrap_or(path.len());
    Ok([
        0..drive,
        drive..directory,
        directory..extension,
        extension..path.len(),
    ])
}

fn split(env: &mut Env<'_>, source: u32, outputs: [Output; 4], wide: bool) -> Outcome {
    let width = if wide { 2 } else { 1 };
    // Validate every participating buffer before clearing or copying anything.
    // Declared capacities are units, including the terminator, not byte counts.
    for output in outputs.iter().filter(|output| output.present()) {
        let Some(bytes) = output.capacity.checked_mul(width) else {
            return memory_failure(MemFault {
                addr: output.address,
                len: u32::MAX,
            });
        };
        if output.address.checked_add(bytes - 1).is_none() {
            return memory_failure(MemFault {
                addr: output.address,
                len: bytes,
            });
        }
        if output.capacity as usize > LIMIT {
            return unsupported(env, "path output capacity exceeds 65536 units");
        }
        if let Err(error) = check_range(env.cpu, output.address, bytes, Prot::WRITE) {
            return memory_failure(error);
        }
    }
    if source == 0
        || outputs
            .iter()
            .any(|output| (output.address == 0) != (output.capacity == 0))
    {
        if let Err(error) = clear(env, &outputs, width) {
            return memory_failure(error);
        }
        // VC80: cleared valid buffers and errno22 precede the returning handler.
        // DOS errno remains unchanged; the invalid-parameter path clears LE.
        crate::util::set_last_error(env, 0);
        let thunk = env.sys.thunks.get(env.thunk);
        let api = format!("{}!{}", thunk.module, thunk.name);
        return invalid_parameter_result(env, &api, 22, 22);
    }
    let path: Vec<u16> = if wide {
        match WideStr(GuestPtr::new(source)).read(env.cpu, LIMIT + 1) {
            Ok(path) => path,
            Err(error) => {
                if error.addr == source && error.len == ((LIMIT + 1) * 2) as u32 {
                    return unsupported(env, "path scan exceeds 65536 UTF-16 units");
                }
                return memory_failure(error);
            }
        }
    } else {
        match AnsiStr(GuestPtr::new(source)).read(env.cpu, LIMIT + 1) {
            Ok(path) => path.into_iter().map(u16::from).collect(),
            Err(error) => {
                if error.addr == source && error.len == (LIMIT + 1) as u32 {
                    return unsupported(env, "path scan exceeds 65536 bytes");
                }
                return memory_failure(error);
            }
        }
    };
    let input_end = u64::from(source) + (path.len() as u64 + 1) * u64::from(width);
    if outputs
        .iter()
        .filter(|output| output.present())
        .any(|output| {
            u64::from(output.address) < input_end
                && u64::from(source)
                    < u64::from(output.address) + u64::from(output.capacity) * u64::from(width)
        })
    {
        // Legacy CRT copies components while scanning, so input/output overlap
        // changes its later scan. Do not claim snapshot semantics match that.
        return unsupported(env, "overlapping path input/output buffers are unavailable");
    }
    let ranges = if wide {
        split_ranges(&path, |_| false)
    } else {
        // Secure split uses the independent multibyte profile, not LC_CTYPE:
        // VC80 still treats a DBCS trail slash as data after setlocale(C/1252).
        // Explicit _setmbcp changes use the same module-owned profile as
        // multibyte traversal; later CTYPE edits remain independent.
        let cp = locale::multibyte_codepage(env);
        if cp != 0 && !adv_os::nls::is_valid_codepage(cp) {
            return unsupported(env, "default CRT multibyte path code page is unavailable");
        }
        split_ranges(&path, |unit| mbcs::is_lead(cp, unit as u8))
    };
    let ranges = match ranges {
        Ok(ranges) => ranges,
        Err(reason) => return unsupported(env, reason),
    };
    for (output, range) in outputs.into_iter().zip(ranges) {
        if output.present() && range.len() >= output.capacity as usize {
            // Measured VC80 ERANGE: already copied preceding components keep
            // their tail bytes; all valid outputs receive an empty first unit.
            // This is an ordinary range failure with no invalid callback.
            if let Err(error) = clear(env, &outputs, width) {
                return memory_failure(error);
            }
            return errno(env, 34);
        }
        if let Err(error) = write(env, output, &path[range], width) {
            return memory_failure(error);
        }
    }
    Outcome::Return(0)
}

#[win32(msvcrt, name = "_splitpath_s", level = implement)]
#[allow(clippy::too_many_arguments)] // Exact nine-argument SDK cdecl frontend.
fn split_a(
    env: &mut Env<'_>,
    path: AnsiStr,
    drive: AnsiStr,
    drive_size: u32,
    dir: AnsiStr,
    dir_size: u32,
    name: AnsiStr,
    name_size: u32,
    ext: AnsiStr,
    ext_size: u32,
) -> Outcome {
    split(
        env,
        path.0.addr(),
        [
            Output {
                address: drive.0.addr(),
                capacity: drive_size,
            },
            Output {
                address: dir.0.addr(),
                capacity: dir_size,
            },
            Output {
                address: name.0.addr(),
                capacity: name_size,
            },
            Output {
                address: ext.0.addr(),
                capacity: ext_size,
            },
        ],
        false,
    )
}

#[win32(msvcrt, name = "_wsplitpath_s", level = implement)]
#[allow(clippy::too_many_arguments)] // Exact nine-argument SDK cdecl frontend.
fn split_w(
    env: &mut Env<'_>,
    path: WideStr,
    drive: WideStr,
    drive_size: u32,
    dir: WideStr,
    dir_size: u32,
    name: WideStr,
    name_size: u32,
    ext: WideStr,
    ext_size: u32,
) -> Outcome {
    split(
        env,
        path.0.addr(),
        [
            Output {
                address: drive.0.addr(),
                capacity: drive_size,
            },
            Output {
                address: dir.0.addr(),
                capacity: dir_size,
            },
            Output {
                address: name.0.addr(),
                capacity: name_size,
            },
            Output {
                address: ext.0.addr(),
                capacity: ext_size,
            },
        ],
        true,
    )
}

#[cfg(test)]
mod tests;
