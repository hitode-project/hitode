//! C byte-set searches: no locale decoding, guest string copies or new owner.
use adv_abi::{
    AnsiStr, Api, Registry,
    guest::{check_range, memory_failure},
    win32,
};
use adv_cpu::backend::{Guest, MemFault, Prot};
use adv_machine::{Env, Outcome};

const MODULES: [&str; 3] = ["msvcrt.dll", "msvcr80.dll", "msvcr90.dll"];
const PAGE_BYTES: u32 = 4096;
const SCAN_LIMIT: u32 = 16 * 1024 * 1024;

pub(super) fn register(registry: &mut Registry) {
    for module in MODULES {
        for api in [strpbrk_ABI, strcspn_ABI, strspn_ABI] {
            registry.register(Api { module, ..api });
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum ScanError {
    Memory(MemFault),
    Limit,
}
impl From<MemFault> for ScanError {
    fn from(error: MemFault) -> Self {
        Self::Memory(error)
    }
}

/// Read only page-local chunks. A smaller readable prefix falls back to one
/// byte, allowing a terminating/matching byte before a later memory fault.
fn scan_bytes(
    cpu: &dyn Guest,
    mut address: u32,
    limit: u32,
    mut visit: impl FnMut(u8, u32, u32) -> Option<u32>,
) -> Result<u32, ScanError> {
    let mut scratch = [0u8; PAGE_BYTES as usize];
    let mut offset = 0;
    while offset < limit {
        let mut length = (limit - offset).min(PAGE_BYTES - (address & (PAGE_BYTES - 1)));
        if !cpu.accessible(address, length, Prot::READ) {
            length = 1;
        }
        check_range(cpu, address, length, Prot::READ)?;
        cpu.read(address, &mut scratch[..length as usize])?;
        for (index, &byte) in scratch[..length as usize].iter().enumerate() {
            // The range above proves every byte address, including 0xffffffff.
            let index = index as u32;
            if let Some(value) = visit(byte, address + index, offset + index) {
                return Ok(value);
            }
        }
        offset += length;
        if offset < limit {
            address = address.checked_add(length).ok_or(MemFault {
                addr: address,
                len: length.saturating_add(1),
            })?;
        }
    }
    Err(ScanError::Limit)
}

#[derive(Clone, Copy)]
enum Search {
    Pointer,
    RejectSpan,
    AcceptSpan,
}

fn search(cpu: &dyn Guest, source: u32, control: u32, mode: Search) -> Result<u32, ScanError> {
    // A fixed 256-bit set treats high bytes as unsigned char. NUL terminates
    // the control string and is never a member, even with trailing bytes.
    let mut members = [0u64; 4];
    scan_bytes(cpu, control, SCAN_LIMIT, |byte, _, _| {
        if byte == 0 {
            return Some(0);
        }
        members[usize::from(byte >> 6)] |= 1u64 << (byte & 63);
        None
    })?;
    scan_bytes(cpu, source, SCAN_LIMIT, |byte, address, offset| {
        if byte == 0 {
            return Some(match mode {
                Search::Pointer => 0,
                Search::RejectSpan | Search::AcceptSpan => offset,
            });
        }
        let member = members[usize::from(byte >> 6)] & (1u64 << (byte & 63)) != 0;
        match mode {
            Search::Pointer if member => Some(address),
            Search::RejectSpan if member => Some(offset),
            Search::AcceptSpan if !member => Some(offset),
            _ => None,
        }
    })
}

fn run(env: &mut Env<'_>, source: AnsiStr, control: AnsiStr, mode: Search) -> Outcome {
    match search(env.cpu, source.0.addr(), control.0.addr(), mode) {
        Ok(value) => Outcome::Return(value),
        Err(ScanError::Memory(error)) => memory_failure(error),
        Err(ScanError::Limit) => {
            let thunk = env.sys.thunks.get(env.thunk);
            let api = format!("{}!{}", thunk.module, thunk.name);
            let reason = "byte-string scan exceeds the 16 MiB bounded profile";
            adv_abi::warn_unavailable(env, &api, reason);
            Outcome::Fail(format!("{api}: {reason}"))
        }
    }
}

#[win32(msvcrt, name = "strpbrk", level = implement)]
fn strpbrk(env: &mut Env<'_>, source: AnsiStr, control: AnsiStr) -> Outcome {
    run(env, source, control, Search::Pointer)
}

#[win32(msvcrt, name = "strcspn", level = implement)]
fn strcspn(env: &mut Env<'_>, source: AnsiStr, control: AnsiStr) -> Outcome {
    run(env, source, control, Search::RejectSpan)
}

#[win32(msvcrt, name = "strspn", level = implement)]
fn strspn(env: &mut Env<'_>, source: AnsiStr, control: AnsiStr) -> Outcome {
    run(env, source, control, Search::AcceptSpan)
}

#[cfg(test)]
mod tests;
