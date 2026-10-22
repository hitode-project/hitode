//! Local account names and provider-owned extended errors require no network I/O.
use super::*;
use crate::{Win32, util::set_last_error};
use std::collections::BTreeMap;
#[derive(Default, Clone)]
struct Error {
    code: u32,
    description: Vec<u16>,
    provider: Vec<u16>,
}
#[derive(Default)]
struct Errors(BTreeMap<adv_machine::CtxId, Error>);
fn encoded(env: &Env<'_>, text: &[u16], wide: bool) -> Vec<u8> {
    if wide {
        text.iter().flat_map(|unit| unit.to_le_bytes()).collect()
    } else {
        env.ext.get::<Win32>().codec.from_utf16(0, text).0
    }
}
fn sized(env: &mut Env<'_>, text: &[u16], buffer: u32, length: u32, wide: bool) -> u32 {
    if check_range(env.cpu, length, 4, Prot::READ | Prot::WRITE).is_err() {
        return INVALID_ADDRESS;
    }
    let width = if wide { 2 } else { 1 };
    let mut bytes = encoded(env, text, wide);
    bytes.resize(bytes.len() + width, 0);
    let needed = (bytes.len() / width) as u32;
    let capacity = env.cpu.read_u32(length).unwrap();
    if capacity < needed {
        let _ = env.cpu.write_u32(length, needed);
        return 234;
    }
    if check_range(env.cpu, buffer, bytes.len() as u32, Prot::WRITE).is_err() {
        return INVALID_ADDRESS;
    }
    if env.cpu.write(buffer, &bytes).is_err() {
        return INVALID_ADDRESS;
    }
    let _ = env.cpu.write_u32(length, needed);
    0
}
fn user(env: &mut Env<'_>, wide: bool) -> Outcome {
    let (name, output, length) = (env.arg(0), env.arg(1), env.arg(2));
    match text(env, name, wide) {
        Ok(name) if !name.is_empty() => return Outcome::Return(NOT_CONNECTED),
        Err(error) => return Outcome::Return(error),
        _ => {}
    }
    let name = env
        .ext
        .get::<Win32>()
        .cfg
        .os
        .user_name
        .encode_utf16()
        .collect::<Vec<_>>();
    Outcome::Return(sized(env, &name, output, length, wide))
}
fn provider_name(env: &mut Env<'_>, wide: bool) -> Outcome {
    let (kind, output, length) = (env.arg(0) & 0xffff0000, env.arg(1), env.arg(2));
    let Some((_, name)) = PROVIDERS.iter().find(|(network, _)| *network == kind) else {
        return Outcome::Return(NO_NETWORK);
    };
    Outcome::Return(sized(
        env,
        &name.encode_utf16().collect::<Vec<_>>(),
        output,
        length,
        wide,
    ))
}
fn provider_info(env: &mut Env<'_>, wide: bool) -> Outcome {
    let name = match text(env, env.arg(0), wide) {
        Ok(value) => value,
        Err(error) => return Outcome::Return(error),
    };
    if !provider(&name) {
        return Outcome::Return(1204);
    }
    let output = env.arg(1);
    if check_range(env.cpu, output, 4, Prot::READ).is_err() {
        return Outcome::Return(INVALID_ADDRESS);
    }
    if env.cpu.read_u32(output).ok() != Some(32) {
        return Outcome::Return(INVALID_PARAMETER);
    }
    adv_abi::warn_unavailable(
        env,
        if wide {
            "mpr.dll!WNetGetNetworkInformationW"
        } else {
            "mpr.dll!WNetGetNetworkInformationA"
        },
        "network-provider DLL capability/version queries are unavailable",
    );
    Outcome::Return(50)
}
fn set_error(env: &mut Env<'_>, wide: bool) -> Outcome {
    let (code, description, provider) = (env.arg(0), env.arg(1), env.arg(2));
    let description = match text(env, description, wide) {
        Ok(value) => value,
        Err(error) => {
            set_last_error(env, error);
            return Outcome::Return(0);
        }
    };
    let provider = match text(env, provider, wide) {
        Ok(value) => value,
        Err(error) => {
            set_last_error(env, error);
            return Outcome::Return(0);
        }
    };
    let errors = env.ext.get_or_default::<Errors>();
    errors.0.retain(|&context, _| {
        context == env.context
            || env
                .sys
                .contexts
                .get(context as usize)
                .is_some_and(|context| !context.is_done())
    });
    errors.0.insert(
        env.context,
        Error {
            code,
            description,
            provider,
        },
    );
    Outcome::Return(0)
}
fn truncated(env: &Env<'_>, text: &[u16], capacity: u32, wide: bool) -> Result<Vec<u8>, u32> {
    if capacity == 0 {
        return Err(INVALID_ADDRESS);
    }
    let width = if wide { 2 } else { 1 };
    let mut bytes = encoded(env, text, wide);
    bytes.truncate((capacity as usize - 1) * width);
    bytes.resize(bytes.len() + width, 0);
    Ok(bytes)
}
fn get_error(env: &mut Env<'_>, wide: bool) -> Outcome {
    let (code, description, description_length, provider, provider_length) =
        (env.arg(0), env.arg(1), env.arg(2), env.arg(3), env.arg(4));
    let error = env
        .ext
        .try_get::<Errors>()
        .and_then(|state| state.0.get(&env.context))
        .cloned()
        .unwrap_or_default();
    let description_bytes = match truncated(env, &error.description, description_length, wide) {
        Ok(value) => value,
        Err(error) => return Outcome::Return(error),
    };
    let provider_bytes = match truncated(env, &error.provider, provider_length, wide) {
        Ok(value) => value,
        Err(error) => return Outcome::Return(error),
    };
    if check_range(env.cpu, code, 4, Prot::WRITE).is_err()
        || check_range(
            env.cpu,
            description,
            description_bytes.len() as u32,
            Prot::WRITE,
        )
        .is_err()
        || check_range(env.cpu, provider, provider_bytes.len() as u32, Prot::WRITE).is_err()
    {
        return Outcome::Return(INVALID_ADDRESS);
    }
    let _ = env.cpu.write_u32(code, error.code);
    let _ = env.cpu.write(description, &description_bytes);
    let _ = env.cpu.write(provider, &provider_bytes);
    Outcome::Return(0)
}
pub(super) fn register(registry: &mut Registry) {
    for (name, handler) in [
        (
            "WNetGetUserA",
            (|env: &mut Env<'_>| user(env, false)) as adv_machine::Handler,
        ),
        ("WNetGetUserW", |env| user(env, true)),
        ("WNetGetProviderNameA", |env| provider_name(env, false)),
        ("WNetGetProviderNameW", |env| provider_name(env, true)),
        ("WNetGetNetworkInformationA", |env| {
            provider_info(env, false)
        }),
        ("WNetGetNetworkInformationW", |env| provider_info(env, true)),
        ("WNetSetLastErrorA", |env| set_error(env, false)),
        ("WNetSetLastErrorW", |env| set_error(env, true)),
        ("WNetGetLastErrorA", |env| get_error(env, false)),
        ("WNetGetLastErrorW", |env| get_error(env, true)),
    ] {
        let declaration = signature::lookup(MPR, name).unwrap();
        registry.add(MPR, name, declaration.conv.unwrap(), handler);
    }
}
