//! Winsock initialization lifetime only. Socket/network APIs remain unresolved
//! until a faithful persona exists; this module never creates a host socket.
use crate::util::{last_error, set_last_error};
use adv_abi::Registry;
use adv_cpu::backend::Prot;
use adv_machine::{Env, Outcome};

const VERSION_NOT_SUPPORTED: u32 = 10092;
const NOT_INITIALIZED: u32 = 10093;
const FAULT: u32 = 10014;
const DATA_BYTES: u32 = 400;
#[derive(Default)]
struct Library {
    references: u32,
}

pub fn register(registry: &mut Registry) {
    for (name, argc, handler) in [
        ("WSAStartup", 2, startup as adv_machine::Handler),
        ("#115", 2, startup),
        ("WSACleanup", 0, cleanup),
        ("#116", 0, cleanup),
        ("WSAGetLastError", 0, get_error),
        ("#111", 0, get_error),
        ("WSASetLastError", 1, set_error),
        ("#112", 1, set_error),
    ] {
        crate::stdcall(registry, "ws2_32.dll", name, argc, handler);
    }
}
fn negotiated(requested: u16) -> Option<u16> {
    let major = requested as u8;
    let minor = (requested >> 8) as u8;
    match major {
        0 => None,
        1 => Some(1 | u16::from(minor.min(1)) << 8),
        2 => Some(2 | u16::from(minor.min(2)) << 8),
        _ => Some(0x0202),
    }
}
fn startup(env: &mut Env<'_>) -> Outcome {
    let version = negotiated(env.arg(0) as u16);
    let data = env.arg(1);
    if data == 0 {
        return Outcome::Return(if version.is_none() {
            VERSION_NOT_SUPPORTED
        } else {
            FAULT
        });
    }
    if !env.cpu.accessible(data, DATA_BYTES, Prot::WRITE) {
        return Outcome::Return(FAULT);
    }
    let returned = version.unwrap_or(0x0202);
    let legacy = returned as u8 == 1;
    // WSADATA x86: WORD versions at 0/2, strings at 4/261, WORD limits at
    // 390/392, padding at 394, vendor pointer at 396. Native leaves padding,
    // unused string suffixes and the deprecated vendor pointer untouched.
    let fields = [returned, 0x0202];
    for (index, value) in fields.into_iter().enumerate() {
        if env
            .cpu
            .write(data + index as u32 * 2, &value.to_le_bytes())
            .is_err()
        {
            return Outcome::Return(FAULT);
        }
    }
    for (offset, bytes) in [(4, &b"WinSock 2.0\0"[..]), (261, &b"Running\0"[..])] {
        if env.cpu.write(data + offset, bytes).is_err() {
            return Outcome::Return(FAULT);
        }
    }
    if env
        .cpu
        .write(
            data + 390,
            &(if legacy { 32767u16 } else { 0 }).to_le_bytes(),
        )
        .is_err()
        || env
            .cpu
            .write(
                data + 392,
                &(if legacy { 65467u16 } else { 0 }).to_le_bytes(),
            )
            .is_err()
    {
        return Outcome::Return(FAULT);
    }
    if version.is_none() {
        return Outcome::Return(VERSION_NOT_SUPPORTED);
    }
    if env.ext.try_get::<Library>().is_none() {
        env.ext.insert(Library::default());
    }
    let library = env.ext.get_mut::<Library>();
    library.references = library.references.saturating_add(1);
    set_last_error(env, 0);
    Outcome::Return(0)
}
fn cleanup(env: &mut Env<'_>) -> Outcome {
    let Some(library) = env
        .ext
        .try_get_mut::<Library>()
        .filter(|library| library.references != 0)
    else {
        set_last_error(env, NOT_INITIALIZED);
        return Outcome::Return(u32::MAX);
    };
    library.references -= 1;
    set_last_error(env, 0);
    Outcome::Return(0)
}
fn get_error(env: &mut Env<'_>) -> Outcome {
    Outcome::Return(last_error(env))
}
fn set_error(env: &mut Env<'_>) -> Outcome {
    set_last_error(env, env.arg(0));
    Outcome::Return(0)
}
