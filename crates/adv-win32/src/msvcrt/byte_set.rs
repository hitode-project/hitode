//! Byte and UTF-16 set scans share code-unit membership and checked page access.
use adv_abi::{
    AnsiStr, Registry, WideStr,
    guest::{check_range, memory_failure},
    win32,
};
use adv_cpu::backend::{Guest, MemFault, Prot};
use adv_machine::{Env, Outcome};

#[cfg(test)]
const MODULES: [&str; 3] = ["msvcrt.dll", "msvcr80.dll", "msvcr90.dll"];
const PAGE_BYTES: u32 = 4096;

pub(super) fn register(registry: &mut Registry) {
    let exports: Vec<_> = [
        strpbrk_ABI,
        strcspn_ABI,
        strspn_ABI,
        wcspbrk_ABI,
        wcscspn_ABI,
        wcsspn_ABI,
    ]
    .iter()
    .map(|api| (api.name, api.handler))
    .collect();
    super::versions::register(registry, &exports);
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

/// Read page-local chunks, falling back to one code unit at a permission
/// boundary. A stopping unit is observed before any later inaccessible unit.
fn scan_units(
    cpu: &dyn Guest,
    mut address: u32,
    limit: u32,
    width: u32,
    mut visit: impl FnMut(u16, u32, u32) -> Option<u32>,
) -> Result<u32, ScanError> {
    let mut scratch = [0u8; PAGE_BYTES as usize];
    let mut offset = 0;
    while offset < limit {
        let boundary = ((PAGE_BYTES - (address & (PAGE_BYTES - 1))) / width).max(1);
        let mut length = (limit - offset).min(boundary).min(PAGE_BYTES / width);
        if !cpu.accessible(address, length * width, Prot::READ) {
            length = 1;
        }
        check_range(cpu, address, length * width, Prot::READ)?;
        cpu.read(address, &mut scratch[..(length * width) as usize])?;
        for (index, bytes) in scratch[..(length * width) as usize]
            .chunks_exact(width as usize)
            .enumerate()
        {
            let index = index as u32;
            let unit = if width == 1 {
                u16::from(bytes[0])
            } else {
                u16::from_le_bytes([bytes[0], bytes[1]])
            };
            if let Some(value) = visit(unit, address + index * width, offset + index) {
                return Ok(value);
            }
        }
        offset += length;
        if offset < limit {
            address = address.checked_add(length * width).ok_or(MemFault {
                addr: address,
                len: (length * width).saturating_add(1),
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

fn search_units(
    cpu: &dyn Guest,
    source: u32,
    control: u32,
    width: u32,
    mode: Search,
) -> Result<u32, ScanError> {
    // NUL terminates the control string and is never a member. The wide set
    // indexes raw UTF-16 units, including unpaired surrogates, without decoding.
    let mut members = vec![0u64; if width == 1 { 4 } else { 1024 }];
    let limit = u32::MAX / width;
    scan_units(cpu, control, limit, width, |byte, _, _| {
        if byte == 0 {
            return Some(0);
        }
        members[usize::from(byte >> 6)] |= 1u64 << (byte & 63);
        None
    })?;
    scan_units(cpu, source, limit, width, |byte, address, offset| {
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

fn run(env: &mut Env<'_>, source: u32, control: u32, width: u32, mode: Search) -> Outcome {
    match search_units(env.cpu, source, control, width, mode) {
        Ok(value) => Outcome::Return(value),
        Err(ScanError::Memory(error)) => memory_failure(error),
        // No artificial length bound: exhausting the x86 address space without
        // a terminator violates the string precondition and cannot wrap to zero.
        Err(ScanError::Limit) => memory_failure(MemFault {
            addr: source,
            len: u32::MAX,
        }),
    }
}

#[cfg(test)]
fn search(cpu: &dyn Guest, source: u32, control: u32, mode: Search) -> Result<u32, ScanError> {
    search_units(cpu, source, control, 1, mode)
}
#[cfg(test)]
fn scan_bytes(
    cpu: &dyn Guest,
    address: u32,
    limit: u32,
    mut visit: impl FnMut(u8, u32, u32) -> Option<u32>,
) -> Result<u32, ScanError> {
    scan_units(cpu, address, limit, 1, |unit, address, offset| {
        visit(unit as u8, address, offset)
    })
}

#[win32(msvcrt, name = "strpbrk", level = implement)]
fn strpbrk(env: &mut Env<'_>, source: AnsiStr, control: AnsiStr) -> Outcome {
    run(env, source.0.addr(), control.0.addr(), 1, Search::Pointer)
}

#[win32(msvcrt, name = "strcspn", level = implement)]
fn strcspn(env: &mut Env<'_>, source: AnsiStr, control: AnsiStr) -> Outcome {
    run(
        env,
        source.0.addr(),
        control.0.addr(),
        1,
        Search::RejectSpan,
    )
}

#[win32(msvcrt, name = "strspn", level = implement)]
fn strspn(env: &mut Env<'_>, source: AnsiStr, control: AnsiStr) -> Outcome {
    run(
        env,
        source.0.addr(),
        control.0.addr(),
        1,
        Search::AcceptSpan,
    )
}

#[win32(msvcrt, name = "wcspbrk", level = implement)]
fn wcspbrk(env: &mut Env<'_>, source: WideStr, control: WideStr) -> Outcome {
    run(env, source.0.addr(), control.0.addr(), 2, Search::Pointer)
}
#[win32(msvcrt, name = "wcscspn", level = implement)]
fn wcscspn(env: &mut Env<'_>, source: WideStr, control: WideStr) -> Outcome {
    run(
        env,
        source.0.addr(),
        control.0.addr(),
        2,
        Search::RejectSpan,
    )
}
#[win32(msvcrt, name = "wcsspn", level = implement)]
fn wcsspn(env: &mut Env<'_>, source: WideStr, control: WideStr) -> Outcome {
    run(
        env,
        source.0.addr(),
        control.0.addr(),
        2,
        Search::AcceptSpan,
    )
}

#[cfg(test)]
mod tests;
