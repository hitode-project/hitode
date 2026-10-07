//! Legacy default-value queries share the private registry's keys and handles.
//! REG_EXPAND_SZ expansion is unavailable here; it never consults host state.
//! API contract: https://learn.microsoft.com/windows/win32/api/winreg/nf-winreg-regqueryvaluea
use super::{
    ACCESS_DENIED, INVALID_DATA, MORE_DATA, NOACCESS, NOT_FOUND, NOT_SUPPORTED, QUERY, Value,
    child_key, name, opened, output, state, string_type,
};
use crate::Win32;
use adv_abi::{AnsiStr, GuestPtr, Handle, Registry, WideStr, guest::check_range, win32};
use adv_cpu::backend::{GuestExt, Prot};
use adv_machine::Env;

#[cfg(test)]
mod tests;

pub(super) fn register(registry: &mut Registry) {
    registry.register(query_a_ABI);
    registry.register(query_w_ABI);
}

#[win32(advapi32, name = "RegQueryValueA", level = implement)]
fn query_a(
    env: &mut Env<'_>,
    key: Handle,
    subkey: AnsiStr,
    data: AnsiStr,
    size: GuestPtr<i32>,
) -> u32 {
    query(
        env,
        key.0,
        subkey.0.addr(),
        data.0.addr(),
        size.addr(),
        false,
    )
}
#[win32(advapi32, name = "RegQueryValueW", level = implement)]
fn query_w(
    env: &mut Env<'_>,
    key: Handle,
    subkey: WideStr,
    data: WideStr,
    size: GuestPtr<i32>,
) -> u32 {
    query(
        env,
        key.0,
        subkey.0.addr(),
        data.0.addr(),
        size.addr(),
        true,
    )
}

fn unavailable(env: &mut Env<'_>, wide: bool, reason: &str, error: u32) -> u32 {
    adv_abi::warn_unavailable(
        env,
        if wide {
            "advapi32.dll!RegQueryValueW"
        } else {
            "advapi32.dll!RegQueryValueA"
        },
        reason,
    );
    error
}

fn encoded(env: &Env<'_>, value: Value, wide: bool) -> Result<Vec<u8>, u32> {
    let mut bytes = value.bytes;
    if string_type(value.kind) {
        if !bytes.len().is_multiple_of(2) {
            return Err(INVALID_DATA);
        }
        // The legacy API adds a missing terminator to nonempty REG_SZ data.
        // Explicit zero-byte values and all stored extra terminators survive.
        if value.kind == 1 && !bytes.is_empty() && !bytes.ends_with(&[0, 0]) {
            bytes.extend_from_slice(&[0, 0]);
        }
        if !wide {
            let units: Vec<_> = bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&p| u16::from_le_bytes(p))
                .collect();
            bytes = env.ext.get::<Win32>().codec.from_utf16(0, &units).0;
        }
    }
    Ok(bytes)
}

fn query(env: &mut Env<'_>, hkey: u32, subkey: u32, data: u32, size: u32, wide: bool) -> u32 {
    let result = (|| -> Result<u32, u32> {
        if !wide && !matches!(env.ext.get::<Win32>().codec.acp, 932 | 936 | 1252 | 65001) {
            return Err(unavailable(
                env,
                wide,
                "legacy ANSI registry queries require guest ACP 932/936/1252/65001",
                NOT_SUPPORTED,
            ));
        }
        let subkey = name(env, subkey, wide, true)?;
        let parent = opened(env, hkey)?;
        if parent.access & QUERY == 0 {
            return Err(ACCESS_DENIED);
        }
        let (key, _) = child_key(&parent.key, &subkey)?;
        output(env, size, data == 0)?;
        let capacity = if data == 0 {
            0
        } else {
            // LONG is passed to the native value query as a DWORD capacity.
            // A negative bit pattern is not a request to allocate that amount.
            GuestPtr::<u32>::new(size)
                .read(env.cpu)
                .map_err(|_| NOACCESS)?
        };
        let value = state(env)
            .keys
            .get(&key)
            .ok_or(NOT_FOUND)?
            .get(&Vec::new())
            .map(|value| Value {
                kind: value.kind,
                display_name: Vec::new(),
                bytes: value.bytes.clone(),
            });
        let missing_default = value.is_none();
        if value.as_ref().is_some_and(|value| value.kind == 2) {
            return Err(unavailable(
                env,
                wide,
                "legacy REG_EXPAND_SZ environment expansion is unavailable; no host environment is consulted",
                NOT_SUPPORTED,
            ));
        }
        let (kind, bytes) = match value {
            None => (1, vec![0; if wide { 2 } else { 1 }]),
            Some(value) => (value.kind, encoded(env, value, wide)?),
        };
        let required = bytes.len() as u32;
        if size != 0 {
            env.cpu.write_u32(size, required).map_err(|_| NOACCESS)?;
        }
        if data == 0 {
            return Ok(if kind == 1 { 0 } else { INVALID_DATA });
        }
        if capacity < required {
            // Native reports a missing default as an empty successful value,
            // even at zero capacity. Preserve that status without an overrun.
            return Ok(if missing_default { 0 } else { MORE_DATA });
        }
        if !bytes.is_empty() {
            check_range(env.cpu, data, required, Prot::WRITE).map_err(|_| NOACCESS)?;
            env.cpu.write(data, &bytes).map_err(|_| NOACCESS)?;
        }
        Ok(if kind == 1 { 0 } else { INVALID_DATA })
    })();
    result.unwrap_or_else(|error| error)
}
