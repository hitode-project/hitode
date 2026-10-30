//! Guarded bulk execution of REP copies, fills, comparisons and scans.
use std::time::Instant;

use iced_x86::{Code, Instruction, OpKind, Register};

use crate::{CpuState, EXEC, Memory, READ, WRITE};

// One host operation stays bounded even for very large guest counts. Further chunks
// re-enter the REP basic block, consuming fuel and checking the wall-clock deadline.
const CHUNK_BYTES: u32 = 1 << 20;
// Byte atomics cost more than ordinary memcpy. Return to the generated safepoint
// frequently so a callback producer cannot delay stop/budget/interrupt handling.
const SHARED_CHUNK_BYTES: u32 = 4096;
pub const FALLBACK: u32 = 1;
pub const MORE: u32 = 2;
pub const BUDGET: u32 = 3;
pub const READ_FAULT: u32 = 4;
pub const WRITE_FAULT: u32 = 5;

pub fn operation(i: &Instruction) -> Option<u32> {
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
        if page != first && (page - first).is_multiple_of(256) && expired(deadline) {
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

fn execute(bridge: &mut crate::trap::Bridge<'_>, state: &mut CpuState, operation: u32) -> u32 {
    if bridge.expired() {
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
    let overlap = if copy {
        let Some(src_end) = src.checked_add(len) else {
            return FALLBACK;
        };
        src < dst_end && dst < src_end
    } else {
        false
    };
    // SAFETY: Executor supplies its live, exclusively borrowed shared allocation;
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
    if bridge.expired() {
        return BUDGET;
    }
    let mut bytes = len.min(CHUNK_BYTES);
    let shared = memory.shared_range(dst, bytes as usize)
        || (copy && memory.shared_range(src, bytes as usize));
    if overlap {
        return FALLBACK;
    }
    if shared {
        bytes = bytes.min(SHARED_CHUNK_BYTES);
        // A file first-touch failure must belong to the next guest element,
        // rather than a later page we happened to prefetch. Each prepared chunk
        // stays within the pages touched by its first element.
        bytes = bytes.min(((4096 - dst % 4096) / width).max(1) * width);
        if copy {
            bytes = bytes.min(((4096 - src % 4096) / width).max(1) * width);
        }
    }
    if copy && memory.rep_alias_hazard(src, dst, bytes as usize) {
        return FALLBACK;
    }
    if shared {
        // Dedicated device storage must never reach the raw-pointer bulk paths.
        // Preserve full-range permission validation and bounded REP progress;
        // Audio helpers use word atomics; worker-owned file views use page copies.
        if copy
            && memory
                .prepare_native_access(src, bytes as usize, false)
                .is_err()
        {
            state.fault_addr = src;
            return READ_FAULT;
        }
        if memory
            .prepare_native_access(dst, bytes as usize, true)
            .is_err()
        {
            state.fault_addr = dst;
            return WRITE_FAULT;
        }
        let result = if copy {
            memory.copy(src, dst, bytes as usize)
        } else {
            memory.fill(
                dst,
                &state.regs[0].to_le_bytes()[..width as usize],
                bytes as usize,
            )
        };
        // Successful preparation is retained under this exclusive borrow.
        // copy/fill cannot encounter a new lazy IO/COW failure after mutation.
        if result.is_err() {
            std::process::abort();
        }
    } else {
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
    }
    let elements = bytes / width;
    state.regs[1] -= elements;
    state.regs[7] += bytes;
    if copy {
        state.regs[6] += bytes;
    }
    bridge.bulk_calls += 1;
    bridge.bulk_bytes += u64::from(bytes);
    bridge.charge_bulk(u64::from(bytes));
    if state.regs[1] == 0 { 0 } else { MORE }
}

/// Full-range validation permits progress to be committed at a chunk boundary.
/// Otherwise a faulting REP comparison must start in the interpreter with its original
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
fn readable_elements(
    memory: &Memory,
    address: u32,
    width: u32,
    backwards: bool,
) -> Option<(u32, u8)> {
    let range = memory.range(address, width as usize).ok()?;
    let first = range.start / 4096;
    let last = (range.end - 1) / 4096;
    if (first..=last).any(|page| !memory.mapped[page] || memory.perms[page] & READ == 0) {
        return None;
    }
    let within_page = (range.start % 4096) as u32;
    let file = (first..=last).any(|page| {
        memory.perms[page] & (crate::memory::SHARED | crate::memory::AUDIO) == crate::memory::SHARED
    });
    let kind = if file {
        2
    } else if (first..=last).any(|page| memory.perms[page] & crate::memory::AUDIO != 0) {
        1
    } else {
        0
    };
    let span = if backwards {
        within_page / width + 1
    } else {
        // An unaligned word/dword can straddle a page; both pages above were
        // checked, but process just that one element before choosing a new span.
        ((4096 - within_page) / width).max(1)
    };
    Some((span, kind))
}

/// The complete element and its pages were checked by readable_elements. A wide
/// guest element on PCM uses the same aligned word atomics as other accessors;
/// concurrent readers may observe separate words, as the PCM protocol allows.
#[inline(always)]
unsafe fn read_element<const KIND: u8>(
    memory: &Memory,
    address: u32,
    width: u32,
    file: FileSpan<'_>,
) -> u32 {
    let pointer = if KIND == 2 {
        unsafe { file.bytes.as_ptr().add((address - file.start) as usize) }
    } else {
        unsafe { memory.ptr.as_ptr().add((address - memory.base) as usize) }
    };
    if KIND == 1 {
        let mut bytes = [0; 4];
        // SAFETY: readable_elements checked the live PCM pages, including padding.
        unsafe { crate::shared_memory::atomic_copy(pointer, &mut bytes[..width as usize]) };
        u32::from_le_bytes(bytes)
    } else {
        unsafe {
            match width {
                1 => u32::from(pointer.read()),
                2 => u32::from(u16::from_le(pointer.cast::<u16>().read_unaligned())),
                _ => u32::from_le(pointer.cast::<u32>().read_unaligned()),
            }
        }
    }
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

struct ScanProgress {
    source: u32,
    destination: u32,
    completed: u32,
    left: u32,
    right: u32,
}

#[derive(Clone, Copy)]
struct FileSpan<'a> {
    start: u32,
    bytes: &'a [u8],
}

// Keep the word-atomic alternatives out of the ordinary loop's optimization
// unit. Width and access modes are fixed for a checked span; dispatch costs one
// call per page, while no classification or width test runs for each element.
#[inline(never)]
#[allow(clippy::too_many_arguments)]
fn scan_span<const WIDTH: u32, const SOURCE_KIND: u8, const DESTINATION_KIND: u8>(
    memory: &Memory,
    accumulator: u32,
    cmps: bool,
    equal_repeat: bool,
    delta: u32,
    count: u32,
    span: u32,
    progress: &mut ScanProgress,
    source_file: FileSpan<'_>,
    destination_file: FileSpan<'_>,
) -> bool {
    let mut source = progress.source;
    let mut destination = progress.destination;
    let mut completed = progress.completed;
    let mut left = progress.left;
    let mut right = progress.right;
    let mask = match WIDTH {
        1 => 0xff,
        2 => 0xffff,
        _ => u32::MAX,
    };
    let mut stopped = false;
    for _ in 0..span {
        // SAFETY: readable_elements checked all complete elements in this span.
        unsafe {
            left = if cmps {
                read_element::<SOURCE_KIND>(memory, source, WIDTH, source_file)
            } else {
                accumulator
            };
            right = read_element::<DESTINATION_KIND>(memory, destination, WIDTH, destination_file);
        }
        completed += 1;
        destination = destination.wrapping_add(delta);
        if cmps {
            source = source.wrapping_add(delta);
        }
        if ((left ^ right) & mask == 0) != equal_repeat || completed == count {
            stopped = true;
            break;
        }
    }
    *progress = ScanProgress {
        source,
        destination,
        completed,
        left,
        right,
    };
    stopped
}

#[derive(Clone, Copy)]
struct ScanConfig {
    width: u32,
    accumulator: u32,
    cmps: bool,
    equal_repeat: bool,
    delta: u32,
    count: u32,
    span: u32,
    source_kind: u8,
    destination_kind: u8,
}

#[inline(always)]
fn dispatch_span(
    memory: &Memory,
    config: ScanConfig,
    progress: &mut ScanProgress,
    source_file: FileSpan<'_>,
    destination_file: FileSpan<'_>,
) -> bool {
    macro_rules! scan {
        ($src:literal, $dst:literal) => {
            match config.width {
                1 => scan_span::<1, $src, $dst>(
                    memory,
                    config.accumulator,
                    config.cmps,
                    config.equal_repeat,
                    config.delta,
                    config.count,
                    config.span,
                    progress,
                    source_file,
                    destination_file,
                ),
                2 => scan_span::<2, $src, $dst>(
                    memory,
                    config.accumulator,
                    config.cmps,
                    config.equal_repeat,
                    config.delta,
                    config.count,
                    config.span,
                    progress,
                    source_file,
                    destination_file,
                ),
                _ => scan_span::<4, $src, $dst>(
                    memory,
                    config.accumulator,
                    config.cmps,
                    config.equal_repeat,
                    config.delta,
                    config.count,
                    config.span,
                    progress,
                    source_file,
                    destination_file,
                ),
            }
        };
    }
    match (config.source_kind, config.destination_kind) {
        (0, 0) => scan!(0, 0),
        (0, 1) => scan!(0, 1),
        (1, 0) => scan!(1, 0),
        (1, 1) => scan!(1, 1),
        (0, 2) => scan!(0, 2),
        (1, 2) => scan!(1, 2),
        (2, 0) => scan!(2, 0),
        (2, 1) => scan!(2, 1),
        (2, 2) => scan!(2, 2),
        _ => unreachable!(),
    }
}

// Only file scans allocate these private stack buffers. A span stays in the
// pages touched by its first element, so lazy IO does not touch later pages.
#[inline(never)]
fn scan_files(
    memory: &mut Memory,
    config: ScanConfig,
    progress: &mut ScanProgress,
) -> Result<bool, u32> {
    let mut source_bytes = [0; 4096];
    let mut destination_bytes = [0; 4096];
    let mut source_file = FileSpan {
        start: 0,
        bytes: &[],
    };
    let mut destination_file = FileSpan {
        start: 0,
        bytes: &[],
    };
    let bytes = (config.span * config.width) as usize;
    let start = |address: u32| {
        if (config.delta as i32) < 0 {
            address - (config.span - 1) * config.width
        } else {
            address
        }
    };
    if config.source_kind == 2 && config.cmps {
        let address = start(progress.source);
        memory
            .prepare_native_access(address, bytes, false)
            .map_err(|_| progress.source)?;
        memory
            .read_into(address, &mut source_bytes[..bytes])
            .map_err(|_| progress.source)?;
        source_file = FileSpan {
            start: address,
            bytes: &source_bytes[..bytes],
        };
    }
    if config.destination_kind == 2 {
        let address = start(progress.destination);
        memory
            .prepare_native_access(address, bytes, false)
            .map_err(|_| progress.destination)?;
        memory
            .read_into(address, &mut destination_bytes[..bytes])
            .map_err(|_| progress.destination)?;
        destination_file = FileSpan {
            start: address,
            bytes: &destination_bytes[..bytes],
        };
    }
    Ok(dispatch_span(
        memory,
        config,
        progress,
        source_file,
        destination_file,
    ))
}

fn compare(bridge: &mut crate::trap::Bridge<'_>, state: &mut CpuState, operation: u32) -> u32 {
    let count = state.regs[1];
    if count == 0 {
        state.rep = None;
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
    // the Executor's live memory. Comparisons do not write or dirty guest pages.
    let memory = unsafe { &mut *bridge.memory };
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
    let mut limit = count.min(CHUNK_BYTES / width);
    let (mut source, mut destination) = (state.regs[6], state.regs[7]);
    let mut completed = 0;
    let (mut left, mut right) = (0, 0);
    let mut charged = 0;
    let outcome = 'scan: loop {
        bridge.charge_bulk(u64::from((completed - charged) * width));
        charged = completed;
        if bridge.expired() {
            if !fully_checked || completed == 0 {
                return BUDGET;
            }
            break BUDGET;
        }
        let Some((mut span, destination_kind)) =
            readable_elements(memory, destination, width, backwards)
        else {
            // A non-validated scan has not changed any architectural state.
            return FALLBACK;
        };
        let mut source_kind = 0;
        if cmps {
            let Some((source_span, kind)) = readable_elements(memory, source, width, backwards)
            else {
                return FALLBACK;
            };
            span = span.min(source_span);
            source_kind = kind;
        }
        if source_kind != 0 || destination_kind != 0 {
            limit = limit.min(completed + SHARED_CHUNK_BYTES / width);
        }
        span = span.min(limit - completed);
        let mut progress = ScanProgress {
            source,
            destination,
            completed,
            left,
            right,
        };
        let config = ScanConfig {
            width,
            accumulator: state.regs[0],
            cmps,
            equal_repeat,
            delta,
            count,
            span,
            source_kind,
            destination_kind,
        };
        let stopped = if source_kind == 2 || destination_kind == 2 {
            match scan_files(memory, config, &mut progress) {
                Ok(stopped) => stopped,
                Err(address) => {
                    state.regs[1] -= completed;
                    state.regs[7] = destination;
                    if cmps {
                        state.regs[6] = source;
                    }
                    state.fault_addr = address;
                    crate::interpreter::RepContinuation::fault_native_compare(state, operation);
                    if completed != 0 {
                        bridge.bulk_calls += 1;
                        bridge.bulk_bytes += u64::from(completed * width);
                    }
                    return READ_FAULT;
                }
            }
        } else {
            let empty = FileSpan {
                start: 0,
                bytes: &[],
            };
            dispatch_span(memory, config, &mut progress, empty, empty)
        };
        ScanProgress {
            source,
            destination,
            completed,
            left,
            right,
        } = progress;
        if stopped {
            break 'scan 0;
        }
        if completed == limit {
            if !fully_checked {
                // A very long unbounded CRT scan is conservative after this
                // bounded attempt; replay it in the interpreter with its original flags.
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
    crate::interpreter::RepContinuation::commit_native_compare(state, operation, outcome != 0);
    state.eflags = comparison_flags(state.eflags, left, right, width);
    bridge.bulk_calls += 1;
    bridge.bulk_bytes += u64::from(completed * width);
    outcome
}

/// Called only by native code with the Executor's live bridge and saved CpuState.
pub unsafe extern "C" fn repeat(
    bridge: *mut std::ffi::c_void,
    state: *mut CpuState,
    operation: u32,
) -> u32 {
    // SAFETY: same ABI/lifetime contract as host_call; validation precedes all writes.
    let (bridge, state) = unsafe { (&mut *bridge.cast::<crate::trap::Bridge<'_>>(), &mut *state) };
    state.settle_flags();
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        execute(bridge, state, operation)
    }))
    .unwrap_or(FALLBACK)
}
