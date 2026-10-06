//! Byte scans share one checked guest-memory implementation across CRT fronts.
use super::*;
use adv_abi::{GuestPtr, guest::check_range, win32};
use adv_cpu::backend::{Guest, MemFault, Prot};

const PAGE_BYTES: u32 = 4096;
const MODULES: [&str; 5] = [
    "msvcrt.dll",
    "msvcr80.dll",
    "msvcr90.dll",
    "ucrtbase.dll",
    "api-ms-win-crt-string-l1-1-0.dll",
];

pub(super) fn register(registry: &mut Registry) {
    for module in MODULES {
        registry.register(adv_abi::Api {
            module,
            ..memchr_ABI
        });
    }
}

fn find(cpu: &dyn Guest, mut address: u32, value: u8, mut count: u32) -> Result<u32, MemFault> {
    // Do not validate the declared whole range: a match in its readable prefix
    // is valid even when later pages are unmapped. Zero count never touches it.
    let mut scratch = [0u8; PAGE_BYTES as usize];
    while count != 0 {
        let mut length = count.min(PAGE_BYTES - (address & (PAGE_BYTES - 1)));
        // Each bulk read stays inside one readable guest page. If a backend
        // exposes a smaller readable prefix, fall back to one byte so an early
        // match still wins over a fault in a later byte of that same page.
        if !cpu.accessible(address, length, Prot::READ) {
            length = 1;
        }
        check_range(cpu, address, length, Prot::READ)?;
        cpu.read(address, &mut scratch[..length as usize])?;
        if let Some(offset) = scratch[..length as usize]
            .iter()
            .position(|&byte| byte == value)
        {
            return Ok(address + offset as u32); // checked readable range above
        }
        count -= length;
        if count != 0 {
            address = address.checked_add(length).ok_or(MemFault {
                addr: address,
                len: length.saturating_add(count),
            })?;
        }
    }
    Ok(0)
}

#[win32(ucrtbase, name = "memchr", level = implement)]
fn memchr(
    env: &mut Env<'_>,
    buffer: GuestPtr<u8>,
    value: i32,
    count: u32,
) -> Result<GuestPtr<u8>, MemFault> {
    // The C int is converted to unsigned char, including negative/high values.
    find(env.cpu, buffer.addr(), value as u8, count).map(GuestPtr::new)
}

#[cfg(test)]
mod tests;
