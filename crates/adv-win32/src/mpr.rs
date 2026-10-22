//! The offline Multiple Provider Router owns no redirected connections. Local
//! account/provider discovery and per-thread extended errors still work.
mod enumeration;
mod query;
use adv_abi::{Registry, guest::check_range, signature};
use adv_cpu::backend::{GuestExt, Prot};
use adv_machine::{Env, Outcome};
const MPR: &str = "mpr.dll";
const INVALID_PARAMETER: u32 = 87;
const INVALID_ADDRESS: u32 = 487;
const NO_NETWORK: u32 = 1222;
const NOT_CONNECTED: u32 = 2250;
const PROVIDERS: [(u32, &str); 3] = [
    (0x00020000, "Microsoft Windows Network"),
    (0x002e0000, "Web Client Network"),
    (0x00360000, "Microsoft Terminal Services"),
];
fn text(env: &Env<'_>, pointer: u32, wide: bool) -> Result<Vec<u16>, u32> {
    crate::network::text(env, pointer, wide).map_err(|_| INVALID_ADDRESS)
}
fn provider(value: &[u16]) -> bool {
    let value = String::from_utf16_lossy(value);
    PROVIDERS
        .iter()
        .any(|(_, name)| name.eq_ignore_ascii_case(&value))
}
fn resource(env: &Env<'_>, pointer: u32, wide: bool) -> Result<(), u32> {
    if check_range(env.cpu, pointer, 32, Prot::READ).is_err() {
        return Err(INVALID_ADDRESS);
    }
    let kind = env.cpu.read_u32(pointer + 4).map_err(|_| INVALID_ADDRESS)?;
    if kind > 2 {
        return Err(INVALID_PARAMETER);
    }
    let remote = env
        .cpu
        .read_u32(pointer + 20)
        .map_err(|_| INVALID_ADDRESS)?;
    if text(env, remote, wide)?.is_empty() {
        return Err(67);
    }
    let name = env
        .cpu
        .read_u32(pointer + 28)
        .map_err(|_| INVALID_ADDRESS)?;
    let name = text(env, name, wide)?;
    if !name.is_empty() && !provider(&name) {
        return Err(1204);
    }
    Ok(())
}
fn connect(env: &mut Env<'_>, wide: bool, index: u32) -> Outcome {
    let pointer = env.arg(index);
    Outcome::Return(resource(env, pointer, wide).err().unwrap_or(NO_NETWORK))
}
fn connect_simple(env: &mut Env<'_>, wide: bool) -> Outcome {
    Outcome::Return(match text(env, env.arg(0), wide) {
        Ok(remote) if !remote.is_empty() => NO_NETWORK,
        Ok(_) => 67,
        Err(error) => error,
    })
}
fn cancel(env: &mut Env<'_>, wide: bool) -> Outcome {
    Outcome::Return(match text(env, env.arg(0), wide) {
        Ok(name) if !name.is_empty() => NOT_CONNECTED,
        Ok(_) => INVALID_PARAMETER,
        Err(error) => error,
    })
}
fn get_connection(env: &mut Env<'_>, wide: bool) -> Outcome {
    Outcome::Return(match text(env, env.arg(0), wide) {
        Ok(name)
            if name.len() == 2
                && name[0] <= 127
                && (name[0] as u8).is_ascii_alphabetic()
                && name[1] == u16::from(b':') =>
        {
            NOT_CONNECTED
        }
        Ok(_) => 1200,
        Err(error) => error,
    })
}
fn universal(env: &mut Env<'_>, wide: bool) -> Outcome {
    if !matches!(env.arg(1), 1 | 2) {
        return Outcome::Return(124);
    }
    Outcome::Return(match text(env, env.arg(0), wide) {
        Ok(name) if !name.is_empty() => NOT_CONNECTED,
        Ok(_) => INVALID_PARAMETER,
        Err(error) => error,
    })
}
fn open_enum(env: &mut Env<'_>, wide: bool) -> Outcome {
    let (scope, kind, usage, input, output) =
        (env.arg(0), env.arg(1), env.arg(2), env.arg(3), env.arg(4));
    if !matches!(scope, 1 | 2 | 3 | 5)
        || kind > 2
        || scope == 2 && usage & !0x13 != 0
        || matches!(scope, 1 | 3) && input != 0
    {
        return Outcome::Return(INVALID_PARAMETER);
    }
    if check_range(env.cpu, output, 4, Prot::WRITE).is_err() {
        return Outcome::Return(INVALID_ADDRESS);
    }
    if scope == 2 && input != 0 {
        if check_range(env.cpu, input, 32, Prot::READ).is_err() {
            return Outcome::Return(INVALID_ADDRESS);
        }
        if env.cpu.read_u32(input + 12).unwrap_or(0) & 2 == 0 {
            return Outcome::Return(1207);
        }
        let name = env.cpu.read_u32(input + 28).unwrap_or(0);
        match text(env, name, wide) {
            Ok(name) if !name.is_empty() && !provider(&name) => return Outcome::Return(1204),
            Err(error) => return Outcome::Return(error),
            _ => {}
        }
    }
    if matches!(scope, 1 | 3) {
        return enumeration::open(env, scope, kind, output);
    }
    Outcome::Return(NO_NETWORK)
}
fn dialog(env: &mut Env<'_>) -> Outcome {
    Outcome::Return(if env.arg(1) == 1 {
        NO_NETWORK
    } else {
        INVALID_PARAMETER
    })
}
fn dialog_struct(env: &mut Env<'_>, wide: bool, connect_dialog: bool) -> Outcome {
    let pointer = env.arg(0);
    if check_range(env.cpu, pointer, 20, Prot::READ).is_err() {
        return Outcome::Return(INVALID_ADDRESS);
    }
    if env.cpu.read_u32(pointer).ok() != Some(20) {
        return Outcome::Return(INVALID_PARAMETER);
    }
    if connect_dialog {
        let resource = env.cpu.read_u32(pointer + 8).unwrap_or(0);
        return Outcome::Return(
            self::resource(env, resource, wide)
                .err()
                .unwrap_or(NO_NETWORK),
        );
    }
    Outcome::Return(NO_NETWORK)
}
fn performance(env: &mut Env<'_>, wide: bool) -> Outcome {
    Outcome::Return(resource(env, env.arg(0), wide).err().unwrap_or(NO_NETWORK))
}
pub(super) fn register(registry: &mut Registry) {
    for (name, handler) in [
        (
            "MultinetGetConnectionPerformanceA",
            (|env: &mut Env<'_>| performance(env, false)) as adv_machine::Handler,
        ),
        ("MultinetGetConnectionPerformanceW", |env| {
            performance(env, true)
        }),
        ("WNetAddConnectionA", |env| connect_simple(env, false)),
        ("WNetAddConnectionW", |env| connect_simple(env, true)),
        ("WNetAddConnection2A", |env| connect(env, false, 0)),
        ("WNetAddConnection2W", |env| connect(env, true, 0)),
        ("WNetAddConnection3A", |env| connect(env, false, 1)),
        ("WNetAddConnection3W", |env| connect(env, true, 1)),
        ("WNetAddConnection4A", |env| connect(env, false, 1)),
        ("WNetAddConnection4W", |env| connect(env, true, 1)),
        ("WNetUseConnectionA", |env| connect(env, false, 1)),
        ("WNetUseConnectionW", |env| connect(env, true, 1)),
        ("WNetUseConnection4A", |env| connect(env, false, 1)),
        ("WNetUseConnection4W", |env| connect(env, true, 1)),
        ("WNetCancelConnectionA", |env| cancel(env, false)),
        ("WNetCancelConnectionW", |env| cancel(env, true)),
        ("WNetCancelConnection2A", |env| cancel(env, false)),
        ("WNetCancelConnection2W", |env| cancel(env, true)),
        ("WNetGetConnectionA", |env| get_connection(env, false)),
        ("WNetGetConnectionW", |env| get_connection(env, true)),
        ("WNetGetUniversalNameA", |env| universal(env, false)),
        ("WNetGetUniversalNameW", |env| universal(env, true)),
        ("WNetOpenEnumA", |env| open_enum(env, false)),
        ("WNetOpenEnumW", |env| open_enum(env, true)),
        ("WNetEnumResourceA", enumeration::next),
        ("WNetEnumResourceW", enumeration::next),
        ("WNetCloseEnum", enumeration::close),
        ("WNetGetResourceInformationA", |env| connect(env, false, 0)),
        ("WNetGetResourceInformationW", |env| connect(env, true, 0)),
        ("WNetGetResourceParentA", |env| connect(env, false, 0)),
        ("WNetGetResourceParentW", |env| connect(env, true, 0)),
        ("WNetConnectionDialog", dialog),
        ("WNetDisconnectDialog", dialog),
        ("WNetConnectionDialog1A", |env| {
            dialog_struct(env, false, true)
        }),
        ("WNetConnectionDialog1W", |env| {
            dialog_struct(env, true, true)
        }),
        ("WNetDisconnectDialog1A", |env| {
            dialog_struct(env, false, false)
        }),
        ("WNetDisconnectDialog1W", |env| {
            dialog_struct(env, true, false)
        }),
    ] {
        let declaration = signature::lookup(MPR, name).unwrap();
        registry.add(MPR, name, declaration.conv.unwrap(), handler);
    }
    query::register(registry);
}
#[cfg(test)]
mod tests;
