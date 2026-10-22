//! RtlGenRandom (SystemFunction036), which the CRT's rand_s loads by name.
//! Uses the shared OS entropy adapter, also used by CryptoAPI providers.
//! API contract: https://learn.microsoft.com/windows/win32/api/ntsecapi/nf-ntsecapi-rtlgenrandom
use adv_abi::{GuestPtr, Registry, guest::check_range, win32};
use adv_cpu::backend::Prot;
use adv_machine::Env;

#[cfg(test)]
mod tests;

pub(super) fn register(registry: &mut Registry) {
    registry.register(generate_ABI);
}

#[win32(advapi32, name = "SystemFunction036", level = implement)]
fn generate(env: &mut Env<'_>, buffer: GuestPtr<u8>, length: u32) -> u32 {
    if check_range(env.cpu, buffer.addr(), length, Prot::WRITE).is_err() {
        return 0;
    }
    let mut bytes = Vec::new();
    if bytes.try_reserve_exact(length as usize).is_err() {
        return 0;
    }
    bytes.resize(length as usize, 0);
    if adv_os::entropy::fill(&mut bytes).is_err() {
        return 0;
    }
    u32::from(length == 0 || env.cpu.write(buffer.addr(), &bytes).is_ok())
}
