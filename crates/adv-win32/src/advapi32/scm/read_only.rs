use crate::{Win32, util::set_last_error};
use adv_abi::{AnsiStr, GuestPtr, Registry, WideStr, guest::check_range, warn_unavailable, win32};
use adv_cpu::backend::{GuestExt, Prot};
use adv_machine::Env;
use adv_os::scm::{self, Scm};

const INVALID_ADDRESS: u32 = 487;
const NOACCESS: u32 = 998;
const INVALID_LEVEL: u32 = 124;
const MORE_DATA: u32 = 234;
const INSUFFICIENT_BUFFER: u32 = 122;

fn fail(env: &mut Env<'_>, error: u32) -> u32 {
    set_last_error(env, error);
    0
}

/// Keep NULL distinct from an empty string. RPC rejects NULL required pointers
/// with ERROR_INVALID_ADDRESS; unreadable storage uses the checked NOACCESS rule.
fn name(env: &Env<'_>, pointer: u32, wide: bool, nullable: bool) -> Result<Option<Vec<u16>>, u32> {
    if pointer == 0 {
        return if nullable {
            Ok(None)
        } else {
            Err(INVALID_ADDRESS)
        };
    }
    if wide {
        let mut units = Vec::new();
        for index in 0..32_768u32 {
            let at = pointer.checked_add(index * 2).ok_or(NOACCESS)?;
            check_range(env.cpu, at, 2, Prot::READ).map_err(|_| NOACCESS)?;
            let unit = env.cpu.read_u16(at).map_err(|_| NOACCESS)?;
            if unit == 0 {
                return Ok(Some(units));
            }
            units.push(unit);
        }
    } else {
        let mut bytes = Vec::new();
        for index in 0..32_768u32 {
            let at = pointer.checked_add(index).ok_or(NOACCESS)?;
            check_range(env.cpu, at, 1, Prot::READ).map_err(|_| NOACCESS)?;
            let byte = env.cpu.read_u8(at).map_err(|_| NOACCESS)?;
            if byte == 0 {
                return Ok(Some(env.ext.get::<Win32>().codec.to_utf16(0, &bytes)));
            }
            bytes.push(byte);
        }
    }
    Err(scm::INVALID_NAME)
}
fn writable_word(env: &Env<'_>, address: u32) -> Result<(), u32> {
    if address == 0 {
        return Err(INVALID_ADDRESS);
    }
    check_range(env.cpu, address, 4, Prot::WRITE).map_err(|_| NOACCESS)
}
fn put_word(env: &mut Env<'_>, address: u32, value: u32) -> Result<(), u32> {
    env.cpu.write_u32(address, value).map_err(|_| NOACCESS)
}
fn spelling(units: &[u16], text: &str) -> bool {
    units.len() == text.len()
        && units
            .iter()
            .zip(text.bytes())
            .all(|(&unit, byte)| unit <= 127 && (unit as u8).eq_ignore_ascii_case(&byte))
}
fn open_manager(env: &mut Env<'_>, machine: u32, database: u32, access: u32, wide: bool) -> u32 {
    let result = (|| {
        let machine = name(env, machine, wide, true)?;
        let database = name(env, database, wide, true)?;
        let local = machine.as_deref().is_none_or(|machine| {
            // PAL-HLE is the existing virtual GetComputerName/COMPUTERNAME owner.
            machine.is_empty()
                || spelling(machine, ".")
                || spelling(machine, "PAL-HLE")
                || spelling(machine, "\\\\PAL-HLE")
        });
        if !local {
            warn_unavailable(
                env,
                if wide {
                    "advapi32.dll!OpenSCManagerW"
                } else {
                    "advapi32.dll!OpenSCManagerA"
                },
                "remote virtual service control manager connections are unavailable",
            );
            return Err(120);
        }
        if database
            .as_deref()
            .is_some_and(|database| !spelling(database, "ServicesActive"))
        {
            return Err(scm::INVALID_NAME);
        }
        env.ext
            .get_or_default::<Scm>()
            .open_manager(&mut env.sys.objects, access)
    })();
    match result {
        Ok(handle) => {
            set_last_error(env, 0);
            handle
        }
        Err(error) => fail(env, error),
    }
}
#[win32(advapi32, name = "OpenSCManagerA", level = implement)]
fn open_a(env: &mut Env<'_>, machine: AnsiStr, database: AnsiStr, access: u32) -> GuestPtr<()> {
    GuestPtr::new(open_manager(
        env,
        machine.0.addr(),
        database.0.addr(),
        access,
        false,
    ))
}
#[win32(advapi32, name = "OpenSCManagerW", level = implement)]
fn open_w(env: &mut Env<'_>, machine: WideStr, database: WideStr, access: u32) -> GuestPtr<()> {
    GuestPtr::new(open_manager(
        env,
        machine.0.addr(),
        database.0.addr(),
        access,
        true,
    ))
}
fn service_open(
    env: &mut Env<'_>,
    manager: u32,
    service_name: u32,
    access: u32,
    wide: bool,
) -> u32 {
    let result = (|| {
        env.ext
            .try_get::<Scm>()
            .ok_or(scm::INVALID_HANDLE)?
            .manager(&env.sys.objects, manager, scm::CONNECT)?;
        let service_name = name(env, service_name, wide, false)?.ok_or(INVALID_ADDRESS)?;
        env.ext
            .get_mut::<Scm>()
            .open_service(&mut env.sys.objects, manager, &service_name, access)
    })();
    match result {
        Ok(handle) => {
            set_last_error(env, 0);
            handle
        }
        Err(error) => fail(env, error),
    }
}
#[win32(advapi32, name = "OpenServiceA", level = implement)]
fn service_a(
    env: &mut Env<'_>,
    manager: GuestPtr<()>,
    service: AnsiStr,
    access: u32,
) -> GuestPtr<()> {
    GuestPtr::new(service_open(
        env,
        manager.addr(),
        service.0.addr(),
        access,
        false,
    ))
}
#[win32(advapi32, name = "OpenServiceW", level = implement)]
fn service_w(
    env: &mut Env<'_>,
    manager: GuestPtr<()>,
    service: WideStr,
    access: u32,
) -> GuestPtr<()> {
    GuestPtr::new(service_open(
        env,
        manager.addr(),
        service.0.addr(),
        access,
        true,
    ))
}
#[win32(advapi32, name = "CloseServiceHandle", level = implement)]
fn close(env: &mut Env<'_>, handle: GuestPtr<()>) -> i32 {
    let result = env
        .ext
        .try_get::<Scm>()
        .ok_or(scm::INVALID_HANDLE)
        .and_then(|state| state.close(&mut env.sys.objects, handle.addr()));
    match result {
        Ok(()) => 1,
        Err(error) => fail(env, error) as i32,
    }
}
fn string_bytes(env: &Env<'_>, text: &str, wide: bool) -> Vec<u8> {
    if wide {
        text.encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect()
    } else {
        let mut bytes = env.ext.get::<Win32>().codec.encode(text);
        bytes.push(0);
        bytes
    }
}
fn dword(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}
fn append_pointer(bytes: &mut Vec<u8>, offset: usize, base: u32, text: &[u8]) -> Result<(), u32> {
    let pointer = base
        .checked_add(u32::try_from(bytes.len()).map_err(|_| scm::NO_MEMORY)?)
        .ok_or(NOACCESS)?;
    dword(bytes, offset, pointer);
    bytes.extend_from_slice(text);
    Ok(())
}
#[derive(Clone, Copy)]
struct Enumeration {
    manager: u32,
    level: i32,
    types: u32,
    state: u32,
    buffer: u32,
    capacity: u32,
    needed: u32,
    returned: u32,
    resume: u32,
    group: u32,
}
fn enumerate(env: &mut Env<'_>, input: Enumeration, wide: bool) -> i32 {
    let result = (|| {
        if input.level != 0 {
            return Err(INVALID_LEVEL);
        }
        writable_word(env, input.needed)?;
        writable_word(env, input.returned)?;
        env.ext
            .try_get::<Scm>()
            .ok_or(scm::INVALID_HANDLE)?
            .manager(&env.sys.objects, input.manager, 0)?;
        let start = if input.resume == 0 {
            0
        } else {
            writable_word(env, input.resume)?;
            check_range(env.cpu, input.resume, 4, Prot::READ).map_err(|_| NOACCESS)?;
            env.cpu.read_u32(input.resume).map_err(|_| NOACCESS)? as usize
        };
        let group = name(env, input.group, wide, true)?;
        // Native RPC initializes these on server-side parameter/rights errors,
        // while invalid level, context handle or required pointers leave them.
        put_word(env, input.needed, 0)?;
        put_word(env, input.returned, 0)?;
        let catalog = env.ext.get::<Scm>().manager(
            &env.sys.objects,
            input.manager,
            scm::ENUMERATE_SERVICE,
        )?;
        let rows = catalog.enumerate(input.types, input.state, group.as_deref())?;
        if start > rows.len() {
            return Err(scm::INVALID_PARAMETER);
        }
        let remaining = &rows[start..];
        let encoded: Vec<_> = remaining
            .iter()
            .map(|service| {
                (
                    string_bytes(env, &service.name, wide),
                    string_bytes(env, &service.configuration.display_name, wide),
                )
            })
            .collect();
        let sizes: Vec<_> = encoded
            .iter()
            .map(|(name, display)| 44 + name.len() + display.len())
            .collect();
        let capacity = (input.capacity as usize).min(scm::MAX_ENUM_BYTES);
        let mut count = 0;
        let mut used = 0;
        for &size in &sizes {
            if used + size > capacity {
                break;
            }
            used += size;
            count += 1;
        }
        let needed = sizes[count..].iter().sum::<usize>();
        let next = if count == remaining.len() {
            0
        } else {
            start + count
        };
        if count != 0 {
            if input.buffer == 0 {
                return Err(INVALID_ADDRESS);
            }
            check_range(env.cpu, input.buffer, used as u32, Prot::WRITE).map_err(|_| NOACCESS)?;
            let mut bytes = vec![0; count * 44];
            for (index, service) in remaining[..count].iter().enumerate() {
                append_pointer(&mut bytes, index * 44, input.buffer, &encoded[index].0)?;
                append_pointer(&mut bytes, index * 44 + 4, input.buffer, &encoded[index].1)?;
                for (word, value) in service.status.words().into_iter().enumerate() {
                    dword(&mut bytes, index * 44 + 8 + word * 4, value);
                }
            }
            env.cpu.write(input.buffer, &bytes).map_err(|_| NOACCESS)?;
        }
        put_word(env, input.needed, needed as u32)?;
        put_word(env, input.returned, count as u32)?;
        if input.resume != 0 {
            put_word(env, input.resume, next as u32)?;
        }
        if count == remaining.len() {
            Ok(1)
        } else {
            Err(MORE_DATA)
        }
    })();
    match result {
        Ok(value) => value,
        Err(error) => fail(env, error) as i32,
    }
}
#[win32(advapi32, name = "EnumServicesStatusExA", level = implement)]
fn enum_a(
    env: &mut Env<'_>,
    manager: GuestPtr<()>,
    level: i32,
    types: u32,
    state: u32,
    buffer: GuestPtr<u8>,
    capacity: u32,
    needed: GuestPtr<u32>,
    returned: GuestPtr<u32>,
    resume: GuestPtr<u32>,
    group: AnsiStr,
) -> i32 {
    enumerate(
        env,
        Enumeration {
            manager: manager.addr(),
            level,
            types,
            state,
            buffer: buffer.addr(),
            capacity,
            needed: needed.addr(),
            returned: returned.addr(),
            resume: resume.addr(),
            group: group.0.addr(),
        },
        false,
    )
}
#[win32(advapi32, name = "EnumServicesStatusExW", level = implement)]
fn enum_w(
    env: &mut Env<'_>,
    manager: GuestPtr<()>,
    level: i32,
    types: u32,
    state: u32,
    buffer: GuestPtr<u8>,
    capacity: u32,
    needed: GuestPtr<u32>,
    returned: GuestPtr<u32>,
    resume: GuestPtr<u32>,
    group: WideStr,
) -> i32 {
    enumerate(
        env,
        Enumeration {
            manager: manager.addr(),
            level,
            types,
            state,
            buffer: buffer.addr(),
            capacity,
            needed: needed.addr(),
            returned: returned.addr(),
            resume: resume.addr(),
            group: group.0.addr(),
        },
        true,
    )
}
fn configuration(
    env: &mut Env<'_>,
    handle: u32,
    buffer: u32,
    capacity: u32,
    needed: u32,
    wide: bool,
) -> i32 {
    let result = (|| {
        let service = env
            .ext
            .try_get::<Scm>()
            .ok_or(scm::INVALID_HANDLE)?
            .service(&env.sys.objects, handle, scm::QUERY_CONFIG)?;
        writable_word(env, needed)?;
        let config = &service.configuration;
        let mut bytes = vec![0; 36];
        dword(&mut bytes, 0, config.service_type);
        dword(&mut bytes, 4, config.start_type);
        dword(&mut bytes, 8, config.error_control);
        dword(&mut bytes, 20, config.tag_id);
        for (offset, text) in [
            (12, &config.binary_path),
            (16, &config.load_order_group),
            (28, &config.account_name),
            (32, &config.display_name),
        ] {
            append_pointer(&mut bytes, offset, 0, &string_bytes(env, text, wide))?;
        }
        let mut dependencies = Vec::new();
        for dependency in &config.dependencies {
            dependencies.extend_from_slice(&string_bytes(env, dependency, wide));
        }
        if dependencies.is_empty() {
            dependencies.resize(if wide { 2 } else { 1 }, 0);
        }
        dependencies.resize(dependencies.len() + if wide { 2 } else { 1 }, 0);
        append_pointer(&mut bytes, 24, 0, &dependencies)?;
        if bytes.len() > scm::MAX_CONFIG_BYTES {
            return Err(scm::INVALID_PARAMETER);
        }
        if (capacity as usize) < bytes.len() {
            put_word(env, needed, bytes.len() as u32)?;
            return Err(INSUFFICIENT_BUFFER);
        }
        if buffer == 0 {
            return Err(INVALID_ADDRESS);
        }
        check_range(env.cpu, buffer, bytes.len() as u32, Prot::WRITE).map_err(|_| NOACCESS)?;
        for offset in [12, 16, 24, 28, 32] {
            let relative = u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
            dword(
                &mut bytes,
                offset,
                buffer.checked_add(relative).ok_or(NOACCESS)?,
            );
        }
        env.cpu.write(buffer, &bytes).map_err(|_| NOACCESS)?;
        put_word(env, needed, bytes.len() as u32)?;
        Ok(1)
    })();
    match result {
        Ok(value) => value,
        Err(error) => fail(env, error) as i32,
    }
}
#[win32(advapi32, name = "QueryServiceConfigA", level = implement)]
fn config_a(
    env: &mut Env<'_>,
    service: GuestPtr<()>,
    buffer: GuestPtr<()>,
    capacity: u32,
    needed: GuestPtr<u32>,
) -> i32 {
    configuration(
        env,
        service.addr(),
        buffer.addr(),
        capacity,
        needed.addr(),
        false,
    )
}
#[win32(advapi32, name = "QueryServiceConfigW", level = implement)]
fn config_w(
    env: &mut Env<'_>,
    service: GuestPtr<()>,
    buffer: GuestPtr<()>,
    capacity: u32,
    needed: GuestPtr<u32>,
) -> i32 {
    configuration(
        env,
        service.addr(),
        buffer.addr(),
        capacity,
        needed.addr(),
        true,
    )
}
pub(super) fn register(registry: &mut Registry) {
    for api in [
        open_a_ABI,
        open_w_ABI,
        service_a_ABI,
        service_w_ABI,
        close_ABI,
        enum_a_ABI,
        enum_w_ABI,
        config_a_ABI,
        config_w_ABI,
    ] {
        registry.register(api);
    }
}
#[cfg(test)]
#[path = "tests.rs"]
mod tests;
