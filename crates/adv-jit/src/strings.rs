//! Guarded bulk execution of REP copies, fills, comparisons and scans.
use std::time::Instant;

use iced_x86::{Code, Instruction, OpKind, Register};

use crate::{CpuState, EXEC, Memory, READ, WRITE};

// One host operation stays bounded even for very large guest counts. Further chunks
// re-enter the REP basic block, consuming fuel and checking the wall-clock deadline.
const CHUNK_BYTES: u32 = 1 << 20;
pub(crate) const FALLBACK: u32 = 1;
pub(crate) const MORE: u32 = 2;
pub(crate) const BUDGET: u32 = 3;

pub(crate) fn operation(i: &Instruction) -> Option<u32> {
    if !(i.has_rep_prefix() || i.has_repne_prefix())
        || i.has_lock_prefix()
        || !matches!(i.segment_prefix(), Register::None | Register::DS)
    {
        return None;
    }
    let compare = match i.code() {
        Code::Cmpsb_m8_m8 => Some(6),
        Code::Cmpsw_m16_m16 => Some(7),
        Code::Cmpsd_m32_m32 => Some(8),
        Code::Scasb_AL_m8 => Some(12),
        Code::Scasw_AX_m16 => Some(13),
        Code::Scasd_EAX_m32 => Some(14),
        _ => None,
    };
    if let Some(operation) = compare {
        if i.op1_kind() != OpKind::MemoryESEDI
            || (operation < 12 && i.op0_kind() != OpKind::MemorySegESI)
        {
            return None;
        }
        return Some(operation + if i.has_repne_prefix() { 3 } else { 0 });
    }
    if i.has_repne_prefix() || i.op0_kind() != OpKind::MemoryESEDI {
        return None;
    }
    Some(match i.code() {
        Code::Movsb_m8_m8 if i.op1_kind() == OpKind::MemorySegESI => 0,
        Code::Movsw_m16_m16 if i.op1_kind() == OpKind::MemorySegESI => 1,
        Code::Movsd_m32_m32 if i.op1_kind() == OpKind::MemorySegESI => 2,
        Code::Stosb_m8_AL => 3,
        Code::Stosw_m16_AX => 4,
        Code::Stosd_m32_EAX => 5,
        _ => return None,
    })
}

fn expired(deadline: Option<Instant>) -> bool {
    deadline.is_some_and(|at| Instant::now() >= at)
}

fn checked_range(
    memory: &Memory,
    addr: u32,
    len: u32,
    write: bool,
    deadline: Option<Instant>,
) -> Result<usize, u32> {
    let range = memory.range(addr, len as usize).map_err(|_| FALLBACK)?;
    let first = range.start / 4096;
    let last = (range.end - 1) / 4096;
    for page in first..=last {
        // Permission validation itself must remain interruptible for huge ranges.
        if (page - first).is_multiple_of(256) && expired(deadline) {
            return Err(BUDGET);
        }
        let perms = memory.perms[page];
        if !memory.mapped[page]
            || perms & if write { WRITE } else { READ } == 0
            || (write && perms & EXEC != 0)
        {
            return Err(FALLBACK);
        }
    }
    Ok(range.start)
}

fn execute(bridge: &mut crate::host::Bridge<'_>, state: &mut CpuState, operation: u32) -> u32 {
    if expired(bridge.deadline) {
        return BUDGET;
    }
    if (6..18).contains(&operation) {
        return compare(bridge, state, operation);
    }
    let (copy, width) = match operation {
        0 => (true, 1),
        1 => (true, 2),
        2 => (true, 4),
        3 => (false, 1),
        4 => (false, 2),
        5 => (false, 4),
        _ => return FALLBACK,
    };
    let count = state.regs[1];
    if count == 0 {
        return 0;
    }
    if state.eflags & (1 << 10) != 0 {
        return FALLBACK;
    }
    let Some(len) = count.checked_mul(width) else {
        return FALLBACK;
    };
    let dst = state.regs[7];
    let Some(dst_end) = dst.checked_add(len) else {
        return FALLBACK;
    };
    let src = state.regs[6];
    if copy {
        let Some(src_end) = src.checked_add(len) else {
            return FALLBACK;
        };
        // REP is a sequence of element reads/writes, not memmove. Conservatively
        // hand every overlap back before its first element can change memory.
        if src < dst_end && dst < src_end {
            return FALLBACK;
        }
    }
    // SAFETY: Machine supplies its live, exclusively borrowed shared allocation;
    // generated code has stopped and no trap or scheduler runs during this call.
    let memory = unsafe { &mut *bridge.memory };
    let dst_offset = match checked_range(memory, dst, len, true, bridge.deadline) {
        Ok(at) => at,
        Err(exit) => return exit,
    };
    let src_offset = if copy {
        match checked_range(memory, src, len, false, bridge.deadline) {
            Ok(at) => at,
            Err(exit) => return exit,
        }
    } else {
        0
    };
    if expired(bridge.deadline) {
        return BUDGET;
    }
    let bytes = len.min(CHUNK_BYTES);
    // SAFETY: all pages, permissions, bounds, non-overlap and address arithmetic
    // were validated for the complete remaining instruction before any mutation.
    unsafe {
        let destination = memory.ptr.as_ptr().add(dst_offset);
        if copy {
            std::ptr::copy_nonoverlapping(
                memory.ptr.as_ptr().add(src_offset),
                destination,
                bytes as usize,
            );
        } else if width == 1 {
            std::ptr::write_bytes(destination, state.regs[0] as u8, bytes as usize);
        } else if width == 4 && (destination as usize).is_multiple_of(4) {
            std::slice::from_raw_parts_mut(destination.cast::<u32>(), (bytes / 4) as usize)
                .fill(state.regs[0].to_le());
        } else if width == 2 && (destination as usize).is_multiple_of(2) {
            std::slice::from_raw_parts_mut(destination.cast::<u16>(), (bytes / 2) as usize)
                .fill((state.regs[0] as u16).to_le());
        } else {
            // Unaligned fills use non-overlapping byte copies of a repeated pattern.
            let pattern = state.regs[0].to_le_bytes();
            std::ptr::copy_nonoverlapping(pattern.as_ptr(), destination, width as usize);
            let mut filled = width as usize;
            while filled < bytes as usize {
                let n = filled.min(bytes as usize - filled);
                std::ptr::copy_nonoverlapping(destination, destination.add(filled), n);
                filled += n;
            }
        }
    }
    let elements = bytes / width;
    state.regs[1] -= elements;
    state.regs[7] += bytes;
    if copy {
        state.regs[6] += bytes;
    }
    bridge.bulk_calls += 1;
    bridge.bulk_bytes += u64::from(bytes);
    if state.regs[1] == 0 { 0 } else { MORE }
}

/// Full-range validation permits progress to be committed at a chunk boundary.
/// Otherwise a faulting REP comparison must start in Unicorn with its original
/// flags: Intel restores the instruction's input EFLAGS on a CMPS/SCAS fault.
fn compare_range(
    memory: &Memory,
    address: u32,
    count: u32,
    width: u32,
    backwards: bool,
    deadline: Option<Instant>,
) -> Result<(), u32> {
    let bytes = count.checked_mul(width).ok_or(FALLBACK)?;
    let start = if backwards {
        address.checked_sub(bytes - width).ok_or(FALLBACK)?
    } else {
        address
    };
    checked_range(memory, start, bytes, false, deadline).map(|_| ())
}

/// Validate only the pages containing the next element and return how many
/// subsequent elements stay within those pages. No later guest page is read.
fn readable_elements(memory: &Memory, address: u32, width: u32, backwards: bool) -> Option<u32> {
    let range = memory.range(address, width as usize).ok()?;
    let first = range.start / 4096;
    let last = (range.end - 1) / 4096;
    if (first..=last).any(|page| !memory.mapped[page] || memory.perms[page] & READ == 0) {
        return None;
    }
    let within_page = (range.start % 4096) as u32;
    Some(if backwards {
        within_page / width + 1
    } else {
        // An unaligned word/dword can straddle a page; both pages above were
        // checked, but process just that one element before choosing a new span.
        ((4096 - within_page) / width).max(1)
    })
}

fn comparison_flags(old: u32, left: u32, right: u32, width: u32) -> u32 {
    let mask = match width {
        1 => 0xff,
        2 => 0xffff,
        _ => u32::MAX,
    };
    let sign = 1 << (width * 8 - 1);
    let (left, right) = (left & mask, right & mask);
    let result = left.wrapping_sub(right) & mask;
    (old & !0x8d5)
        | u32::from(left < right)
        | (u32::from((result as u8).count_ones().is_multiple_of(2)) << 2)
        | ((left ^ right ^ result) & 0x10)
        | (u32::from(result == 0) << 6)
        | (u32::from(result & sign != 0) << 7)
        | (u32::from((left ^ right) & (left ^ result) & sign != 0) << 11)
}

fn compare(bridge: &mut crate::host::Bridge<'_>, state: &mut CpuState, operation: u32) -> u32 {
    let count = state.regs[1];
    if count == 0 {
        return 0;
    }
    let width: u32 = match (operation - 6) % 3 {
        0 => 1,
        1 => 2,
        _ => 4,
    };
    let cmps = operation < 12;
    let equal_repeat = (operation - 6) % 6 < 3;
    let backwards = state.eflags & (1 << 10) != 0;
    let delta = if backwards {
        width.wrapping_neg()
    } else {
        width
    };
    // SAFETY: the native executor is suspended and the bridge exclusively borrows
    // the Machine's live memory. Comparisons do not write or dirty guest pages.
    let memory = unsafe { &*bridge.memory };
    let validated = compare_range(
        memory,
        state.regs[7],
        count,
        width,
        backwards,
        bridge.deadline,
    )
    .and_then(|()| {
        if cmps {
            compare_range(
                memory,
                state.regs[6],
                count,
                width,
                backwards,
                bridge.deadline,
            )
        } else {
            Ok(())
        }
    });
    let fully_checked = match validated {
        Ok(()) => true,
        Err(BUDGET) => return BUDGET,
        Err(_) => false,
    };
    let limit = count.min(CHUNK_BYTES / width);
    let (mut source, mut destination) = (state.regs[6], state.regs[7]);
    let mut completed = 0;
    let (mut left, mut right) = (0, 0);
    let outcome = 'scan: loop {
        if expired(bridge.deadline) {
            if !fully_checked || completed == 0 {
                return BUDGET;
            }
            break BUDGET;
        }
        let Some(mut span) = readable_elements(memory, destination, width, backwards) else {
            // A non-validated scan has not changed any architectural state.
            return FALLBACK;
        };
        if cmps {
            let Some(source_span) = readable_elements(memory, source, width, backwards) else {
                return FALLBACK;
            };
            span = span.min(source_span);
        }
        span = span.min(limit - completed);
        for _ in 0..span {
            // SAFETY: the page span above validates each entire element, including
            // unaligned elements straddling two pages. Reads occur in guest order;
            // a stopping comparison never reads the next element.
            unsafe {
                let read = |address: u32| {
                    let pointer = memory.ptr.as_ptr().add((address - memory.base) as usize);
                    match width {
                        1 => u32::from(pointer.read()),
                        2 => u32::from(u16::from_le(pointer.cast::<u16>().read_unaligned())),
                        _ => u32::from_le(pointer.cast::<u32>().read_unaligned()),
                    }
                };
                left = if cmps { read(source) } else { state.regs[0] };
                right = read(destination);
            }
            completed += 1;
            destination = destination.wrapping_add(delta);
            if cmps {
                source = source.wrapping_add(delta);
            }
            let equal = match width {
                1 => left as u8 == right as u8,
                2 => left as u16 == right as u16,
                _ => left == right,
            };
            if equal != equal_repeat || completed == count {
                break 'scan 0;
            }
        }
        if completed == limit {
            if !fully_checked {
                // A very long unbounded CRT scan is conservative after this
                // bounded attempt; replay it in Unicorn with its original flags.
                return FALLBACK;
            }
            break MORE;
        }
    };
    state.regs[1] -= completed;
    state.regs[7] = destination;
    if cmps {
        state.regs[6] = source;
    }
    state.eflags = comparison_flags(state.eflags, left, right, width);
    bridge.bulk_calls += 1;
    bridge.bulk_bytes += u64::from(completed * width);
    outcome
}

/// Called only by native code with the Machine's live bridge and saved CpuState.
pub(crate) unsafe extern "C" fn repeat(
    bridge: *mut std::ffi::c_void,
    state: *mut CpuState,
    operation: u32,
) -> u32 {
    // SAFETY: same ABI/lifetime contract as host_call; validation precedes all writes.
    let (bridge, state) = unsafe { (&mut *bridge.cast::<crate::host::Bridge<'_>>(), &mut *state) };
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        execute(bridge, state, operation)
    }))
    .unwrap_or(FALLBACK)
}
