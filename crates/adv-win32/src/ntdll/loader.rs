//! Native counted DLL loading over the common module and initializer owner.
use adv_abi::{GuestPtr, Registry, WideStr, guest::check_range, warn_unavailable, win32};
use adv_cpu::backend::{GuestExt, Prot};
use adv_machine::{Env, Outcome};

pub(super) fn register(registry: &mut Registry) {
    registry.register(load_ABI);
}

#[win32(ntdll, name = "LdrLoadDll", level = implement)]
fn load(
    env: &mut Env<'_>,
    search: WideStr,
    characteristics: GuestPtr<u32>,
    name: GuestPtr<()>,
    output: GuestPtr<u32>,
) -> Outcome {
    if check_range(env.cpu, output.addr(), 4, Prot::WRITE).is_err()
        || check_range(env.cpu, name.addr(), 8, Prot::READ).is_err()
    {
        return Outcome::Return(0xc0000005);
    }
    if !search.0.is_null() {
        warn_unavailable(
            env,
            "ntdll.dll!LdrLoadDll",
            "explicit native DLL search paths and search modifiers are unavailable",
        );
        return Outcome::Return(0xc00000bb);
    }
    if !characteristics.is_null() {
        if check_range(env.cpu, characteristics.addr(), 4, Prot::READ).is_err() {
            return Outcome::Return(0xc0000005);
        }
        let Ok(flags) = characteristics.read(env.cpu) else {
            return Outcome::Return(0xc0000005);
        };
        if flags != 0 {
            warn_unavailable(
                env,
                "ntdll.dll!LdrLoadDll",
                "native DLL characteristics require an unimplemented loader profile",
            );
            return Outcome::Return(0xc00000bb);
        }
    }
    let Ok(length) = env.cpu.read_u16(name.addr()) else {
        return Outcome::Return(0xc0000005);
    };
    let Ok(buffer) = env.cpu.read_u32(name.addr() + 4) else {
        return Outcome::Return(0xc0000005);
    };
    if length & 1 != 0 {
        warn_unavailable(
            env,
            "ntdll.dll!LdrLoadDll",
            "odd-byte Unicode DLL names are unavailable",
        );
        return Outcome::Return(0xc00000bb);
    }
    let mut bytes = vec![0; usize::from(length)];
    if length != 0
        && (check_range(env.cpu, buffer, u32::from(length), Prot::READ).is_err()
            || env.cpu.read(buffer, &mut bytes).is_err())
    {
        return Outcome::Return(0xc0000005);
    }
    let units: Vec<_> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .collect();
    let Ok(name) = String::from_utf16(&units) else {
        warn_unavailable(
            env,
            "ntdll.dll!LdrLoadDll",
            "unpaired UTF-16 DLL names are unavailable",
        );
        return Outcome::Return(0xc00000bb);
    };
    if units.contains(&0) {
        warn_unavailable(
            env,
            "ntdll.dll!LdrLoadDll",
            "embedded-NUL DLL names are unavailable",
        );
        return Outcome::Return(0xc00000bb);
    }
    // Native failures preserve the output, and DllMain owns its LastError effects.
    crate::kernel32::native_load_library(env, name, output.addr())
}
