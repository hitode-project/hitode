//! Win32 find fronts consume a single native directory enumeration cursor.
use super::*;
use crate::util::{INVALID_HANDLE_VALUE, set_last_error};
use adv_abi::{GuestPtr, Handle};
use std::collections::HashMap;

#[derive(Default)]
struct Finds(HashMap<u32, Option<u64>>);

fn owned(env: &mut Env<'_>, handle: u32) -> bool {
    let identity = env
        .ext
        .try_get::<Finds>()
        .and_then(|finds| finds.0.get(&handle))
        .copied();
    if let Some(identity) = identity {
        if env.sys.objects.object_id(handle) == identity {
            return true;
        }
        // A direct native close can invalidate this frontend's marker. Never
        // consume or close a different object reusing the original number.
        env.ext.get_mut::<Finds>().0.remove(&handle);
    }
    false
}

fn failure(env: &mut Env<'_>, status: u32, first: bool) -> Outcome {
    let error = match status {
        0x8000_0006 => {
            if first {
                2
            } else {
                18
            }
        }
        0xc000_000f => 2,
        _ => win32_error(status),
    };
    set_last_error(env, error);
    Outcome::Return(if first { INVALID_HANDLE_VALUE } else { 0 })
}

fn publish(env: &mut Env<'_>, output: u32, wide: bool, bytes: &[u8]) -> Result<(), u32> {
    if bytes.len() < 94 {
        return Err(INVALID_PARAMETER);
    }
    let length = u32::from_le_bytes(bytes[60..64].try_into().unwrap()) as usize;
    if length & 1 != 0 || length > bytes.len() - 94 {
        return Err(INVALID_PARAMETER);
    }
    let units: Vec<_> = bytes[94..94 + length]
        .chunks_exact(2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .collect();
    let mut data = vec![0; if wide { 592 } else { 320 }];
    data[..4].copy_from_slice(&bytes[56..60]);
    data[4..28].copy_from_slice(&bytes[8..32]);
    data[28..32].copy_from_slice(&bytes[44..48]);
    data[32..36].copy_from_slice(&bytes[40..44]);
    let short = usize::from(bytes[68]).min(24) & !1;
    let short_units: Vec<_> = bytes[70..70 + short]
        .chunks_exact(2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .collect();
    if wide {
        for (offset, capacity, name) in [
            (44, 259, units.as_slice()),
            (564, 13, short_units.as_slice()),
        ] {
            for (index, unit) in name.iter().take(capacity).enumerate() {
                data[offset + index * 2..offset + index * 2 + 2]
                    .copy_from_slice(&unit.to_le_bytes());
            }
        }
    } else {
        for (offset, capacity, name) in [
            (44, 259, units.as_slice()),
            (304, 13, short_units.as_slice()),
        ] {
            let name = env
                .ext
                .get::<Win32>()
                .codec
                .encode(&String::from_utf16_lossy(name));
            let count = name.len().min(capacity);
            data[offset..offset + count].copy_from_slice(&name[..count]);
        }
    }
    check_range(env.cpu, output, data.len() as u32, Prot::WRITE).map_err(|_| ACCESS_VIOLATION)?;
    env.cpu.write(output, &data).map_err(|_| ACCESS_VIOLATION)
}

fn query(env: &mut Env<'_>, handle: u32, mask: Option<String>, output: u32, wide: bool) -> Outcome {
    let first = mask.is_some();
    let units: Vec<_> = mask.as_deref().unwrap_or("").encode_utf16().collect();
    if units.len() > 32767 {
        return failure(env, INVALID_PARAMETER, first);
    }
    const BUFFER: u32 = 16;
    const CAPACITY: u32 = 64 * 1024;
    let scratch = match attributes(env, r"C:\", BUFFER + CAPACITY + units.len() as u32 * 2) {
        Ok(scratch) => scratch,
        Err(status) => return failure(env, status, first),
    };
    let iosb = scratch.extra;
    let descriptor = iosb + 8;
    let buffer = iosb + BUFFER;
    let name = buffer + CAPACITY;
    if first {
        let length = (units.len() as u16) * 2;
        let initialized = env
            .cpu
            .write(
                descriptor,
                &((u32::from(length) | (u32::from(length) << 16))
                    .to_le_bytes()
                    .into_iter()
                    .chain(name.to_le_bytes())
                    .collect::<Vec<_>>()),
            )
            .and_then(|()| {
                env.cpu.write(
                    name,
                    &units
                        .iter()
                        .flat_map(|u| u.to_le_bytes())
                        .collect::<Vec<_>>(),
                )
            });
        if initialized.is_err() {
            scratch.free(env);
            return failure(env, ACCESS_VIOLATION, first);
        }
    }
    let filename = if first { descriptor } else { 0 };
    let complete = move |env: &mut Env<'_>, status: u32| {
        if status == 0x103 {
            return Outcome::Fail(
                "native directory: asynchronous completion is unavailable".into(),
            );
        }
        let reported = env.cpu.read_u32(iosb + 4);
        let result = if (status as i32) < 0 {
            Err(status)
        } else {
            reported.map_err(|_| ACCESS_VIOLATION).and_then(|reported| {
                if !(94..=CAPACITY).contains(&reported) {
                    return Err(INVALID_PARAMETER);
                }
                let mut bytes = vec![0; reported as usize];
                env.cpu
                    .read(buffer, &mut bytes)
                    .map_err(|_| ACCESS_VIOLATION)?;
                publish(env, output, wide, &bytes)
            })
        };
        scratch.free(env);
        match result {
            Ok(()) => {
                if first {
                    let identity = env.sys.objects.object_id(handle);
                    env.ext.get_or_default::<Finds>().0.insert(handle, identity);
                }
                Outcome::Return(if first { handle } else { 1 })
            }
            Err(status) if first => operations::close(
                env,
                handle,
                Box::new(move |env, _| failure(env, status, true)),
            ),
            Err(status) => failure(env, status, false),
        }
    };
    match overridden(env, "NtQueryDirectoryFile") {
        Ok(Some(target)) => Outcome::call(
            target,
            [
                handle,
                0,
                0,
                0,
                iosb,
                buffer,
                CAPACITY,
                3,
                1,
                filename,
                u32::from(first),
            ],
            move |env, value| complete(env, value.value()),
        ),
        Ok(None) => {
            let status = crate::ntdll::file::enumeration::query(
                env,
                Handle(handle),
                Handle(0),
                GuestPtr::new(0),
                GuestPtr::new(0),
                GuestPtr::new(iosb),
                GuestPtr::new(buffer),
                CAPACITY,
                3,
                1,
                GuestPtr::new(filename),
                u32::from(first),
            );
            complete(env, status as u32)
        }
        Err(_) => crate::native::unavailable(env, "NtQueryDirectoryFile"),
    }
}

pub(in crate::kernel32) fn first(
    env: &mut Env<'_>,
    pattern: String,
    output: u32,
    wide: bool,
) -> Outcome {
    if check_range(env.cpu, output, if wide { 592 } else { 320 }, Prot::WRITE).is_err() {
        return failure(env, ACCESS_VIOLATION, true);
    }
    let full = env.ext.get::<Win32>().vfs.normalize(&pattern);
    let Some((directory, mask)) = full.rsplit_once('\\') else {
        return failure(env, INVALID_PARAMETER, true);
    };
    let directory = if directory.len() == 2 {
        format!("{directory}\\")
    } else {
        directory.to_owned()
    };
    let mask = mask.to_owned();
    operations::open(
        env,
        &directory,
        0x8010_0081,
        7,
        1,
        0x21,
        Box::new(move |env, result| match result {
            Ok(handle) => query(env, handle, Some(mask), output, wide),
            Err(status) => failure(env, status, true),
        }),
    )
}

pub(in crate::kernel32) fn next(env: &mut Env<'_>, wide: bool) -> Outcome {
    let (handle, output) = (env.arg(0), env.arg(1));
    if !owned(env, handle) {
        return failure(env, 0xc000_0008, false);
    }
    if check_range(env.cpu, output, if wide { 592 } else { 320 }, Prot::WRITE).is_err() {
        return failure(env, ACCESS_VIOLATION, false);
    }
    query(env, handle, None, output, wide)
}

pub(in crate::kernel32) fn close(env: &mut Env<'_>, handle: u32) -> Outcome {
    if !owned(env, handle) {
        return failure(env, 0xc000_0008, false);
    }
    operations::close(
        env,
        handle,
        Box::new(move |env, result| match result {
            Ok(()) => {
                env.ext.get_mut::<Finds>().0.remove(&handle);
                Outcome::Return(1)
            }
            Err(status) => failure(env, status, false),
        }),
    )
}

#[cfg(test)]
mod tests;
