//! Guarded bulk execution of forward REP copies and fills.
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
    if !i.has_rep_prefix()
        || i.has_repne_prefix()
        || i.has_lock_prefix()
        || !matches!(i.segment_prefix(), Register::None | Register::DS)
        || i.op0_kind() != OpKind::MemoryESEDI
    {
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
