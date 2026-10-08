//! Event adapters and native entries share the existing named object owner.
use super::*;
use crate::kernel32::{file_mapping, process::native_thread};
use adv_abi::guest::check_range;
use adv_cpu::backend::Prot;

const FULL_ACCESS: u32 = 0x001f0003;
const ACCESS_VIOLATION: u32 = 0xc0000005;
const NAME_EXISTS: u32 = 0x40000000;
const NAME_COLLISION: u32 = 0xc0000035;
const NOT_SUPPORTED: u32 = 0xc00000bb;

struct Attributes {
    name: Option<Vec<u16>>,
    inherit: bool,
    open_if: bool,
}
fn unsupported(env: &mut Env<'_>, api: &str, reason: &str) -> u32 {
    adv_abi::warn_unavailable(env, &format!("ntdll.dll!{api}"), reason);
    NOT_SUPPORTED
}
fn error(code: u32) -> u32 {
    match code {
        2 => 0xc0000034,
        3 => 0xc000003a,
        6 => 0xc0000024,
        8 => 0xc0000017,
        50 => NOT_SUPPORTED,
        123 | 206 => 0xc0000033,
        183 => NAME_COLLISION,
        998 => ACCESS_VIOLATION,
        _ => 0xc000000d,
    }
}
fn writable(env: &Env<'_>, pointer: u32) -> Result<(), u32> {
    if pointer & 3 != 0 {
        return Err(0x80000002);
    }
    check_range(env.cpu, pointer, 4, Prot::WRITE).map_err(|_| ACCESS_VIOLATION)
}
fn attributes(env: &mut Env<'_>, pointer: u32, api: &str) -> Result<Attributes, u32> {
    let mut result = Attributes {
        name: None,
        inherit: false,
        open_if: false,
    };
    if pointer == 0 {
        return Ok(result);
    }
    if pointer & 3 != 0 {
        return Err(0x80000002);
    }
    check_range(env.cpu, pointer, 24, Prot::READ).map_err(|_| ACCESS_VIOLATION)?;
    let fields: [u32; 6] =
        std::array::from_fn(|i| env.cpu.read_u32(pointer + i as u32 * 4).unwrap_or(0));
    if fields[0] != 24 || fields[3] & !0x1ff2 != 0 {
        return Err(0xc000000d);
    }
    if fields[1] != 0 || fields[4] != 0 || fields[5] != 0 || fields[3] & !(2 | 0x80) != 0 {
        return Err(unsupported(
            env,
            api,
            "event directory handles, security descriptors and extended object attributes are unavailable",
        ));
    }
    result.inherit = fields[3] & 2 != 0;
    result.open_if = fields[3] & 0x80 != 0;
    if fields[2] == 0 {
        return Ok(result);
    }
    let string = fields[2];
    check_range(env.cpu, string, 8, Prot::READ).map_err(|_| ACCESS_VIOLATION)?;
    let length = env.cpu.read_u16(string).map_err(|_| ACCESS_VIOLATION)? as u32;
    let maximum = env.cpu.read_u16(string + 2).map_err(|_| ACCESS_VIOLATION)? as u32;
    let buffer = env.cpu.read_u32(string + 4).map_err(|_| ACCESS_VIOLATION)?;
    if length == 0 || length & 1 != 0 || maximum < length {
        return Err(0xc0000033);
    }
    check_range(env.cpu, buffer, length, Prot::READ).map_err(|_| ACCESS_VIOLATION)?;
    let mut bytes = vec![0; length as usize];
    env.cpu
        .read(buffer, &mut bytes)
        .map_err(|_| ACCESS_VIOLATION)?;
    let units: Vec<_> = bytes
        .chunks_exact(2)
        .map(|word| u16::from_le_bytes([word[0], word[1]]))
        .collect();
    // These absolute directories are the supported virtual session's existing
    // Local/Global profile. Other directory ownership requires an object manager.
    let local: Vec<_> = r"\Sessions\0\BaseNamedObjects\".encode_utf16().collect();
    let global: Vec<_> = r"\BaseNamedObjects\".encode_utf16().collect();
    let (scope, leaf) = if let Some(leaf) = units.strip_prefix(local.as_slice()) {
        (0, leaf)
    } else if let Some(leaf) = units.strip_prefix(global.as_slice()) {
        (1, leaf)
    } else {
        return Err(unsupported(
            env,
            api,
            "event names outside the virtual Local/Global directories are unavailable",
        ));
    };
    if leaf.is_empty() || leaf.len() > 260 || leaf.contains(&0) || leaf.contains(&(b'\\' as u16)) {
        return Err(0xc0000033);
    }
    let mut key = vec![scope];
    key.extend_from_slice(leaf);
    result.name = Some(key);
    Ok(result)
}
fn access(env: &mut Env<'_>, desired: u32, api: &str) -> Result<(), u32> {
    if desired != FULL_ACCESS && desired != 0x10000000 {
        return Err(unsupported(
            env,
            api,
            "restricted event access is not modeled; use EVENT_ALL_ACCESS or GENERIC_ALL",
        ));
    }
    Ok(())
}
pub(crate) fn create_native(env: &mut Env<'_>, args: [u32; 5]) -> u32 {
    let [output, desired, object, kind, initial] = args;
    if let Err(status) = writable(env, output) {
        return status;
    }
    if kind > 1 {
        return 0xc000000d;
    }
    let attrs = match attributes(env, object, "NtCreateEvent") {
        Ok(attrs) => attrs,
        Err(status) => return status,
    };
    if let Err(status) = access(env, desired, "NtCreateEvent") {
        return status;
    }
    let object = Object::Event {
        manual_reset: kind == 0,
        signaled: initial as u8 != 0,
    };
    match file_mapping::sync_create_named(env, object, attrs.name, attrs.open_if) {
        Ok((handle, existed)) => {
            if env.cpu.write_u32(output, handle).is_err() {
                env.sys.objects.close(handle);
                file_mapping::prune(env);
                return ACCESS_VIOLATION;
            }
            super::super::handle::set_inherit(env, handle, attrs.inherit);
            if existed { NAME_EXISTS } else { 0 }
        }
        Err(code) => error(code),
    }
}
pub(crate) fn open_native(env: &mut Env<'_>, args: [u32; 3]) -> u32 {
    let [output, desired, object] = args;
    if let Err(status) = writable(env, output) {
        return status;
    }
    let attrs = match attributes(env, object, "NtOpenEvent") {
        Ok(attrs) => attrs,
        Err(status) => return status,
    };
    let Some(name) = attrs.name else {
        return 0xc0000033;
    };
    let identity = match file_mapping::sync_lookup_named(env, &name, 1) {
        Ok(identity) => identity,
        Err(code) => return error(code),
    };
    if let Err(status) = access(env, desired, "NtOpenEvent") {
        return status;
    }
    let Some(handle) = env.sys.objects.alias_identity(identity) else {
        return 0xc0000017;
    };
    if env.cpu.write_u32(output, handle).is_err() {
        env.sys.objects.close(handle);
        return ACCESS_VIOLATION;
    }
    super::super::handle::set_inherit(env, handle, attrs.inherit);
    0
}
pub(crate) fn state_native(env: &mut Env<'_>, handle: u32, previous: u32, signaled: bool) -> u32 {
    if previous != 0
        && let Err(status) = writable(env, previous)
    {
        return status;
    }
    let old = match env.sys.objects.get(handle) {
        Some(Object::Event { signaled, .. }) => *signaled,
        Some(_) => return 0xc0000024,
        None => return 0xc0000008,
    };
    if previous != 0 && env.cpu.write_u32(previous, u32::from(old)).is_err() {
        return ACCESS_VIOLATION;
    }
    if signaled {
        env.sys.set_event(handle);
    } else {
        env.sys.reset_event(handle);
    }
    0
}
fn marshal(
    env: &mut Env<'_>,
    key: Option<&[u16]>,
    inherit: bool,
    open_if: bool,
) -> Result<u32, Outcome> {
    let mut units = match key {
        Some(key) => {
            let prefix = if key[0] == 1 {
                r"\BaseNamedObjects\"
            } else {
                r"\Sessions\0\BaseNamedObjects\"
            };
            let mut units: Vec<_> = prefix.encode_utf16().collect();
            units.extend_from_slice(&key[1..]);
            units
        }
        None => vec![],
    };
    let length = units.len() as u16 * 2;
    units.push(0);
    let size = (40 + units.len() as u32 * 2 + 3) & !3;
    let buffer = native_thread::scratch(env, size)?;
    let fields = [
        24,
        0,
        if key.is_some() { buffer + 32 } else { 0 },
        u32::from(inherit) * 2 | u32::from(open_if) * 0x80,
        0,
        0,
    ];
    for (i, field) in fields.into_iter().enumerate() {
        env.cpu
            .write_u32(buffer + 8 + i as u32 * 4, field)
            .map_err(|fault| Outcome::Fail(fault.to_string()))?;
    }
    env.cpu
        .write(buffer + 32, &length.to_le_bytes())
        .map_err(|fault| Outcome::Fail(fault.to_string()))?;
    env.cpu
        .write(buffer + 34, &(length + 2).to_le_bytes())
        .map_err(|fault| Outcome::Fail(fault.to_string()))?;
    env.cpu
        .write_u32(buffer + 36, buffer + 40)
        .map_err(|fault| Outcome::Fail(fault.to_string()))?;
    let bytes: Vec<_> = units.iter().flat_map(|unit| unit.to_le_bytes()).collect();
    env.cpu
        .write(buffer + 40, &bytes)
        .map_err(|fault| Outcome::Fail(fault.to_string()))?;
    Ok(buffer)
}
fn front_status(env: &mut Env<'_>, status: u32, success: u32) -> Outcome {
    if (status as i32) >= 0 {
        return Outcome::Return(success);
    }
    let code = match status {
        0xc0000024 => 6,
        0x80000002 => 998,
        NAME_COLLISION => 183,
        _ => super::super::file::native::win32_error(status),
    };
    set_last_error(env, code);
    Outcome::Return(0)
}
fn published(env: &mut Env<'_>, output: u32, status: u32, create: bool) -> Outcome {
    if (status as i32) < 0 {
        return front_status(env, status, 0);
    }
    match env.cpu.read_u32(output) {
        Ok(handle) => {
            set_last_error(
                env,
                if create && status == NAME_EXISTS {
                    183
                } else {
                    0
                },
            );
            Outcome::Return(handle)
        }
        Err(_) => front_status(env, ACCESS_VIOLATION, 0),
    }
}
pub(super) fn create_front(env: &mut Env<'_>) -> Outcome {
    let (security, manual, initial, name) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3));
    let wide = env.sys.thunks.get(env.thunk).name.ends_with('W');
    let inherit = match super::super::handle::security_inherit(env, security) {
        Ok(inherit) => inherit,
        Err(code) => {
            set_last_error(env, code);
            return Outcome::Return(0);
        }
    };
    let key = match file_mapping::sync_name(env, name, wide) {
        Ok(key) => key,
        Err(code) => {
            set_last_error(env, code);
            return Outcome::Return(0);
        }
    };
    match crate::native::overridden(env, "NtCreateEvent") {
        Ok(Some(target)) => {
            let buffer = match marshal(env, key.as_deref(), inherit, true) {
                Ok(buffer) => buffer,
                Err(outcome) => return outcome,
            };
            Outcome::call_on_stack(
                target,
                [
                    buffer,
                    FULL_ACCESS,
                    buffer + 8,
                    u32::from(manual == 0),
                    u32::from(initial != 0),
                ],
                buffer,
                move |env, value| published(env, buffer, value.value(), true),
            )
        }
        Ok(None) => match file_mapping::sync_create_named(
            env,
            Object::Event {
                manual_reset: manual != 0,
                signaled: initial != 0,
            },
            key,
            true,
        ) {
            Ok((handle, existed)) => {
                super::super::handle::set_inherit(env, handle, inherit);
                set_last_error(env, if existed { 183 } else { 0 });
                Outcome::Return(handle)
            }
            Err(code) => {
                set_last_error(env, code);
                Outcome::Return(0)
            }
        },
        Err(_) => crate::native::unavailable(env, "NtCreateEvent"),
    }
}
pub(super) fn open_front(env: &mut Env<'_>, pointer: u32, wide: bool) -> Outcome {
    match crate::native::overridden(env, "NtOpenEvent") {
        Ok(Some(target)) => {
            if pointer == 0 {
                set_last_error(env, 87);
                return Outcome::Return(0);
            }
            let key = match file_mapping::sync_name(env, pointer, wide) {
                Ok(Some(key)) => key,
                Ok(None) => {
                    set_last_error(env, 6);
                    return Outcome::Return(0);
                }
                Err(code) => {
                    set_last_error(env, if code == 123 { 3 } else { code });
                    return Outcome::Return(0);
                }
            };
            let (desired, inherit) = (env.arg(0), env.arg(1) != 0);
            let buffer = match marshal(env, Some(&key), inherit, false) {
                Ok(buffer) => buffer,
                Err(outcome) => return outcome,
            };
            Outcome::call_on_stack(
                target,
                [buffer, desired, buffer + 8],
                buffer,
                move |env, value| published(env, buffer, value.value(), false),
            )
        }
        Ok(None) => file_mapping::open_sync(env, pointer, wide, 1, FULL_ACCESS),
        Err(_) => crate::native::unavailable(env, "NtOpenEvent"),
    }
}
pub(super) fn state_front(env: &mut Env<'_>, signaled: bool) -> Outcome {
    let handle = env.arg(0);
    let api = if signaled {
        "NtSetEvent"
    } else {
        "NtResetEvent"
    };
    match crate::native::overridden(env, api) {
        Ok(Some(target)) => Outcome::call(target, [handle, 0], |env, value| {
            front_status(env, value.value(), 1)
        }),
        Ok(None) => {
            let status = state_native(env, handle, 0, signaled);
            front_status(env, status, 1)
        }
        Err(_) => crate::native::unavailable(env, api),
    }
}
pub(super) fn register(registry: &mut Registry) {
    stdcall(registry, "ntdll.dll", "NtCreateEvent", 5, |env| {
        let args = std::array::from_fn(|i| env.arg(i as u32));
        Outcome::Return(create_native(env, args))
    });
    stdcall(registry, "ntdll.dll", "NtOpenEvent", 3, |env| {
        let args = std::array::from_fn(|i| env.arg(i as u32));
        Outcome::Return(open_native(env, args))
    });
    stdcall(registry, "ntdll.dll", "NtSetEvent", 2, |env| {
        let (handle, previous) = (env.arg(0), env.arg(1));
        Outcome::Return(state_native(env, handle, previous, true))
    });
    stdcall(registry, "ntdll.dll", "NtResetEvent", 2, |env| {
        let (handle, previous) = (env.arg(0), env.arg(1));
        Outcome::Return(state_native(env, handle, previous, false))
    });
}
