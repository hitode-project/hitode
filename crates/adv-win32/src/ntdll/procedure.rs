//! Native counted export lookup shares Win32's loader, forwarders and diagnostics.
use adv_abi::{GuestPtr, Registry, guest::check_range, warn_unavailable, win32};
use adv_cpu::backend::{GuestExt, Prot};
use adv_machine::{Env, Outcome};
use adv_pe::ImportName;

pub(super) fn register(registry: &mut Registry) {
    registry.register(lookup_ABI);
}

#[win32(ntdll, name = "LdrGetProcedureAddress", level = implement)]
fn lookup(
    env: &mut Env<'_>,
    module: GuestPtr<()>,
    name: GuestPtr<()>,
    ordinal: u32,
    output: GuestPtr<u32>,
) -> Outcome {
    if check_range(env.cpu, output.addr(), 4, Prot::WRITE).is_err()
        || output.write(env.cpu, 0).is_err()
    {
        return Outcome::Return(0xc0000005);
    }
    let symbol = if name.is_null() {
        if ordinal == 0 {
            return Outcome::Return(0xc000000d);
        }
        if ordinal > u32::from(u16::MAX) {
            warn_unavailable(env, "ntdll.dll!LdrGetProcedureAddress", "export ordinals beyond the shared 16-bit import-name profile are unavailable");
            return Outcome::Return(0xc00000bb);
        }
        ImportName::Ordinal(ordinal as u16)
    } else {
        if check_range(env.cpu, name.addr(), 8, Prot::READ).is_err() {
            return Outcome::Return(0xc0000005);
        }
        let Ok(length) = env.cpu.read_u16(name.addr()) else {
            return Outcome::Return(0xc0000005);
        };
        let Ok(buffer) = env.cpu.read_u32(name.addr() + 4) else {
            return Outcome::Return(0xc0000005);
        };
        let mut bytes = vec![0; usize::from(length)];
        if length != 0
            && (check_range(env.cpu, buffer, u32::from(length), Prot::READ).is_err()
                || env.cpu.read(buffer, &mut bytes).is_err())
        {
            return Outcome::Return(0xc0000005);
        }
        ImportName::Name {
            hint: 0,
            name: String::from_utf8_lossy(&bytes).into_owned(),
        }
    };
    crate::kernel32::native_get_procedure(env, module.addr(), symbol, output.addr())
}
