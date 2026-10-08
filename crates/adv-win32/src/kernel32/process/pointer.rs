//! Process-owned reversible pointer obfuscation shared by Win32, RTL and CRT.
use adv_abi::Registry;
use adv_machine::{Env, Outcome};
use std::{collections::hash_map::RandomState, hash::BuildHasher};

struct Cookie(u32);
impl Default for Cookie {
    fn default() -> Self {
        // Independent randomized keys avoid encoding from predictable guest PID
        // or virtual-clock values. Keep the cookie for this guest process's life.
        Self((RandomState::new().hash_one("process pointer cookie") as u32).max(1))
    }
}
fn encode(env: &mut Env<'_>, pointer: u32) -> u32 {
    let cookie = env.ext.get_or_default::<Cookie>().0;
    (pointer ^ cookie).rotate_right(cookie & 31)
}
fn decode(env: &mut Env<'_>, pointer: u32) -> u32 {
    let cookie = env.ext.get_or_default::<Cookie>().0;
    pointer.rotate_left(cookie & 31) ^ cookie
}
pub(super) fn register(registry: &mut Registry) {
    crate::stdcall(registry, "ntdll.dll", "RtlEncodePointer", 1, |env| {
        let pointer = env.arg(0);
        Outcome::Return(encode(env, pointer))
    });
    crate::stdcall(registry, "ntdll.dll", "RtlDecodePointer", 1, |env| {
        let pointer = env.arg(0);
        Outcome::Return(decode(env, pointer))
    });
    crate::stdcall(registry, "kernel32.dll", "EncodePointer", 1, |env| {
        let pointer = env.arg(0);
        crate::native::route(env, "RtlEncodePointer", [pointer], move |env| {
            Outcome::Return(encode(env, pointer))
        })
    });
    crate::stdcall(registry, "kernel32.dll", "DecodePointer", 1, |env| {
        let pointer = env.arg(0);
        crate::native::route(env, "RtlDecodePointer", [pointer], move |env| {
            Outcome::Return(decode(env, pointer))
        })
    });
}

#[cfg(test)]
mod tests;
