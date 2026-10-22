//! Cryptographic binary/text conversion shares the ABI catalog and checked guest
//! memory boundaries. Text syntax is ASCII, independent of the process ANSI code page.
mod encoding;
use adv_abi::{AnsiStr, GuestPtr, Registry, WideStr, guest::check_range, signature};
use adv_cpu::backend::{GuestExt, Prot};
use adv_machine::{Env, Outcome};
const DLL: &str = "crypt32.dll";
fn fail(env: &mut Env<'_>, error: u32) -> Outcome {
    crate::util::set_last_error(env, error);
    Outcome::Return(0)
}
fn read(env: &Env<'_>, pointer: u32, length: u32) -> Result<Vec<u8>, u32> {
    check_range(env.cpu, pointer, length, Prot::READ).map_err(|_| 87u32)?;
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(length as usize).map_err(|_| 8u32)?;
    bytes.resize(length as usize, 0);
    env.cpu.read(pointer, &mut bytes).map_err(|_| 87u32)?;
    Ok(bytes)
}
fn binary_to_string(env: &mut Env<'_>, wide: bool) -> Outcome {
    let (input, length, flags, output, size) =
        (env.arg(0), env.arg(1), env.arg(2), env.arg(3), env.arg(4));
    if input == 0 || check_range(env.cpu, size, 4, Prot::READ | Prot::WRITE).is_err() {
        return fail(env, 87);
    }
    let input = match read(env, input, length) {
        Ok(value) => value,
        Err(error) => return fail(env, error),
    };
    let encoded = match encoding::encode(&input, flags) {
        Ok(value) => value,
        Err(error) => return fail(env, error),
    };
    let binary = flags & 0xff == 2;
    let characters = encoded.len() as u32;
    let needed = match characters.checked_add(u32::from(!binary)) {
        Some(value) => value,
        None => return fail(env, 8),
    };
    if output == 0 {
        let _ = env.cpu.write_u32(size, needed);
        return Outcome::Return(1);
    }
    if env.cpu.read_u32(size).unwrap() < needed {
        let _ = env.cpu.write_u32(size, needed);
        return fail(env, 234);
    }
    let bytes = if binary {
        encoded
    } else if wide {
        encoded
            .into_iter()
            .chain([0])
            .flat_map(|byte| u16::from(byte).to_le_bytes())
            .collect::<Vec<_>>()
    } else {
        encoded.into_iter().chain([0]).collect()
    };
    if u32::try_from(bytes.len())
        .ok()
        .is_none_or(|length| check_range(env.cpu, output, length, Prot::WRITE).is_err())
    {
        return fail(env, 87);
    }
    let _ = env.cpu.write(output, &bytes);
    let _ = env.cpu.write_u32(size, characters);
    Outcome::Return(1)
}
fn string_to_binary(env: &mut Env<'_>, wide: bool) -> Outcome {
    let (input, length, flags, output, size, skip, detected) = (
        env.arg(0),
        env.arg(1),
        env.arg(2),
        env.arg(3),
        env.arg(4),
        env.arg(5),
        env.arg(6),
    );
    if input == 0
        || check_range(env.cpu, size, 4, Prot::READ | Prot::WRITE).is_err()
        || [skip, detected]
            .into_iter()
            .any(|pointer| pointer != 0 && check_range(env.cpu, pointer, 4, Prot::WRITE).is_err())
    {
        return fail(env, 87);
    }
    let binary = if length == 0 {
        if wide {
            match WideStr(GuestPtr::new(input)).read(env.cpu, (u32::MAX / 2) as usize) {
                Ok(value) => value.into_iter().flat_map(u16::to_le_bytes).collect(),
                Err(_) => return fail(env, 87),
            }
        } else {
            match AnsiStr(GuestPtr::new(input)).read(env.cpu, u32::MAX as usize) {
                Ok(value) => value,
                Err(_) => return fail(env, 87),
            }
        }
    } else {
        let Some(bytes) = length.checked_mul(if wide { 2 } else { 1 }) else {
            return fail(env, 87);
        };
        match read(env, input, bytes) {
            Ok(value) => value,
            Err(error) => return fail(env, error),
        }
    };
    let text = if wide {
        binary
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect::<Vec<_>>()
    } else {
        binary.iter().copied().map(u16::from).collect()
    };
    let (bytes, skipped, format) = match encoding::decode(&text, &binary, flags) {
        Ok(value) => value,
        Err(error) => return fail(env, error),
    };
    let needed = bytes.len() as u32;
    if output != 0 && env.cpu.read_u32(size).unwrap() < needed {
        let _ = env.cpu.write_u32(size, needed);
        return fail(env, 234);
    }
    if output != 0 && check_range(env.cpu, output, needed, Prot::WRITE).is_err() {
        return fail(env, 87);
    }
    if output != 0 {
        let _ = env.cpu.write(output, &bytes);
    }
    let _ = env.cpu.write_u32(size, needed);
    if skip != 0 {
        let _ = env.cpu.write_u32(skip, skipped);
    }
    if detected != 0 {
        let _ = env.cpu.write_u32(detected, format);
    }
    Outcome::Return(1)
}
pub(super) fn register(registry: &mut Registry) {
    for (name, handler) in [
        (
            "CryptBinaryToStringA",
            (|env: &mut Env<'_>| binary_to_string(env, false)) as adv_machine::Handler,
        ),
        ("CryptBinaryToStringW", |env| binary_to_string(env, true)),
        ("CryptStringToBinaryA", |env| string_to_binary(env, false)),
        ("CryptStringToBinaryW", |env| string_to_binary(env, true)),
    ] {
        registry.add(
            DLL,
            name,
            signature::lookup(DLL, name).unwrap().conv.unwrap(),
            handler,
        );
    }
}
#[cfg(test)]
mod tests;
