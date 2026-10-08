//! Native counted DLL loading over the common module and initializer owner.
use adv_abi::{GuestPtr, Registry, WideStr, guest::check_range, warn_unavailable, win32};
use adv_cpu::backend::{GuestExt, Prot};
use adv_machine::{Env, Outcome};

pub(super) fn register(registry: &mut Registry) {
    registry.register(load_ABI);
    crate::stdcall(registry, "ntdll.dll", "LdrUnloadDll", 1, |env| {
        crate::kernel32::native_unload_library(env, env.arg(0))
    });
    // Verified x86 NTAPI prototypes; these counted-name APIs have no SDK ABI
    // metadata in the generated catalogue.
    crate::stdcall(registry, "ntdll.dll", "LdrGetDllHandle", 4, |env| {
        let args = [1, env.arg(0), env.arg(1), env.arg(2), env.arg(3)];
        crate::native::route(env, "LdrGetDllHandleEx", args, move |env| {
            get_handle(env, args[0], args[1], args[2], args[3], args[4])
        })
    });
    crate::stdcall(registry, "ntdll.dll", "LdrGetDllHandleEx", 5, |env| {
        get_handle(
            env,
            env.arg(0),
            env.arg(1),
            env.arg(2),
            env.arg(3),
            env.arg(4),
        )
    });
    crate::stdcall(registry, "ntdll.dll", "LdrAddRefDll", 2, |env| {
        crate::kernel32::native_add_ref_library(env, env.arg(0), env.arg(1))
    });
    crate::stdcall(registry, "ntdll.dll", "RtlPcToFileHeader", 2, |env| {
        crate::kernel32::native_pc_to_file_header(env, env.arg(0), env.arg(1))
    });
}

fn get_handle(
    env: &mut Env<'_>,
    flags: u32,
    search: u32,
    characteristics: u32,
    name: u32,
    output: u32,
) -> Outcome {
    const API: &str = "ntdll.dll!LdrGetDllHandleEx";
    if flags & !3 != 0 || flags == 3 {
        return Outcome::Return(0xc000000d);
    }
    if check_range(env.cpu, output, 4, Prot::WRITE).is_err() {
        return Outcome::Return(0xc0000005);
    }
    if search != 0 {
        warn_unavailable(
            env,
            API,
            "explicit module lookup search policies are unavailable",
        );
        return Outcome::Return(0xc00000bb);
    }
    if characteristics != 0 {
        if check_range(env.cpu, characteristics, 4, Prot::READ).is_err() {
            return Outcome::Return(0xc0000005);
        }
        match env.cpu.read_u32(characteristics) {
            Ok(0) => {}
            Ok(_) => {
                warn_unavailable(env, API, "module lookup characteristics are unavailable");
                return Outcome::Return(0xc00000bb);
            }
            Err(_) => return Outcome::Return(0xc0000005),
        }
    }
    match counted_name(env, name, API) {
        Ok(name) => crate::kernel32::native_get_dll_handle(env, flags, name, output),
        Err(status) => Outcome::Return(status),
    }
}

#[win32(ntdll, name = "LdrLoadDll", level = implement)]
fn load(
    env: &mut Env<'_>,
    search: WideStr,
    characteristics: GuestPtr<u32>,
    name: GuestPtr<()>,
    output: GuestPtr<u32>,
) -> Outcome {
    if check_range(env.cpu, output.addr(), 4, Prot::WRITE).is_err() {
        return Outcome::Return(0xc0000005);
    }
    // Native Win32 encodes the default and altered policies as tagged flags.
    let altered = match search.0.addr() {
        0 | 1 => false,
        9 => true,
        _ => {
            warn_unavailable(
                env,
                "ntdll.dll!LdrLoadDll",
                "explicit native DLL search paths and search modifiers are unavailable",
            );
            return Outcome::Return(0xc00000bb);
        }
    };
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
    match counted_name(env, name.addr(), "ntdll.dll!LdrLoadDll") {
        // Native failures preserve the output and DllMain's LastError effects.
        Ok(name) => crate::kernel32::native_load_library(env, name, output.addr(), altered),
        Err(status) => Outcome::Return(status),
    }
}

fn counted_name(env: &mut Env<'_>, name: u32, api: &str) -> Result<String, u32> {
    if check_range(env.cpu, name, 8, Prot::READ).is_err() {
        return Err(0xc0000005);
    }
    let Ok(length) = env.cpu.read_u16(name) else {
        return Err(0xc0000005);
    };
    let Ok(buffer) = env.cpu.read_u32(name + 4) else {
        return Err(0xc0000005);
    };
    if length & 1 != 0 {
        warn_unavailable(env, api, "odd-byte Unicode DLL names are unavailable");
        return Err(0xc00000bb);
    }
    let mut bytes = vec![0; usize::from(length)];
    if length != 0
        && (check_range(env.cpu, buffer, u32::from(length), Prot::READ).is_err()
            || env.cpu.read(buffer, &mut bytes).is_err())
    {
        return Err(0xc0000005);
    }
    let units: Vec<_> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .collect();
    let Ok(name) = String::from_utf16(&units) else {
        warn_unavailable(env, api, "unpaired UTF-16 DLL names are unavailable");
        return Err(0xc00000bb);
    };
    if units.contains(&0) {
        warn_unavailable(env, api, "embedded-NUL DLL names are unavailable");
        return Err(0xc00000bb);
    }
    Ok(name)
}
