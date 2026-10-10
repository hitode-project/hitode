//! Windows JIT allocations with explicit W^X publication and real retirement.
//! Cranelift 0.125's Windows SystemMemoryProvider lacks its allocation Drop;
//! use owned VirtualAlloc regions instead of relying on leaked executable code.
use cranelift_jit::{BranchProtection, JITMemoryProvider};
use cranelift_module::{ModuleError, ModuleResult};
use std::{ffi::c_void, io, ptr::NonNull};

#[link(name = "kernel32")]
unsafe extern "system" {
    fn VirtualAlloc(address: *mut c_void, size: usize, kind: u32, protection: u32) -> *mut c_void;
    fn VirtualFree(address: *mut c_void, size: usize, kind: u32) -> i32;
    fn VirtualProtect(address: *mut c_void, size: usize, protection: u32, old: *mut u32) -> i32;
    fn FlushInstructionCache(process: *mut c_void, address: *const c_void, len: usize) -> i32;
    fn VirtualQuery(address: *const c_void, info: *mut c_void, len: usize) -> usize;
}

const PAGE: usize = 4096;
// Keep every byte of a module in one interval smaller than rel32's reach.
const RELATIVE_WINDOW: usize = 1 << 30;
#[repr(C)]
#[derive(Default)]
struct Info {
    base: usize,
    allocation: usize,
    allocation_protection: u32,
    partition: u16,
    region: usize,
    state: u32,
    protection: u32,
    kind: u32,
}
fn query(pointer: *const u8) -> io::Result<Info> {
    let mut result = Info::default();
    if unsafe {
        VirtualQuery(
            pointer.cast(),
            (&mut result as *mut Info).cast(),
            std::mem::size_of::<Info>(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(result)
}

/// # Safety
/// `code` belongs to this provider, remains finalized, and is not executing.
/// Every code allocation owns exactly one disjoint read-only xdata tail page.
pub(crate) unsafe fn unwind_storage(code: usize, size: usize) -> Result<(*mut u8, usize), String> {
    let code_info = query(code as *const u8).map_err(|error| error.to_string())?;
    let storage = code_info
        .base
        .checked_add(code_info.region)
        .ok_or("unwind storage overflow")?;
    let metadata = query(storage as *const u8).map_err(|error| error.to_string())?;
    if code_info.protection != 0x20
        || code_info.state != 0x1000
        || code_info.kind != 0x20000
        || code.checked_add(size).is_none_or(|end| end > storage)
        || metadata.allocation != code_info.allocation
        || metadata.base != storage
        || metadata.state != 0x1000
        || metadata.region != PAGE
        || metadata.protection != 2
    {
        return Err("code provider has no private read-only xdata tail page".into());
    }
    Ok((storage as *mut u8, PAGE))
}

/// # Safety
/// The two finalized function owners remain alive for the entire query.
pub(crate) unsafe fn code_storage_bytes(functions: [usize; 2]) -> Result<(usize, usize), String> {
    let mut previous = 0;
    let mut committed = 0;
    let mut reserved = 0;
    for function in functions {
        let allocation = query(function as *const u8)
            .map_err(|error| error.to_string())?
            .allocation;
        if allocation == previous {
            continue;
        }
        previous = allocation;
        let mut address = allocation;
        loop {
            let region = query(address as *const u8).map_err(|error| error.to_string())?;
            if region.allocation != allocation || region.state == 0x10000 {
                break;
            }
            reserved += region.region;
            if region.state == 0x1000 {
                committed += region.region;
            }
            address = address
                .checked_add(region.region)
                .ok_or("code allocation range overflow")?;
        }
    }
    Ok((committed, reserved))
}

const GRANULARITY: usize = 0x1_0000;

/// Reserve and commit `len` bytes at a free, granularity-aligned `start`.
/// Another thread may win the race, which returns `None`.
fn reserve_at(start: usize, len: usize) -> Option<*mut u8> {
    // SAFETY: a fresh reservation request at an address observed free.
    let base = unsafe { VirtualAlloc(start as *mut c_void, len, 0x3000, 4) };
    (!base.is_null()).then_some(base.cast())
}

/// Cranelift reaches a module's constants with 32-bit PC-relative
/// relocations, so every piece of a module must stay inside one
/// [`RELATIVE_WINDOW`]. A later piece is placed in the nearest free range
/// above or below the module's current span `[low, high)`; searching only
/// above failed whenever another thread's guest reservation followed the
/// first piece.
fn near(low: usize, high: usize, len: usize) -> Option<*mut u8> {
    let floor = high.saturating_sub(RELATIVE_WINDOW);
    let ceiling = low.checked_add(RELATIVE_WINDOW)?;
    let mut up = Some(high);
    let mut down = Some(low);
    while up.is_some() || down.is_some() {
        if let Some(address) = up {
            up = None;
            // A failed query (past the user address space) ends one direction.
            if address < ceiling
                && let Ok(region) = query(address as *const u8)
                && let Some(end) = region.base.checked_add(region.region)
            {
                if region.state == 0x10000
                    && let Some(start) = region.base.checked_next_multiple_of(GRANULARITY)
                    && start
                        .checked_add(len)
                        .is_some_and(|stop| stop <= end.min(ceiling))
                    && let Some(base) = reserve_at(start, len)
                {
                    return Some(base);
                }
                up = Some(end);
            }
        }
        if let Some(address) = down {
            down = None;
            // VirtualQuery reports a free range from the queried page up, not
            // from its start, so downward steps try the highest aligned spot
            // and on conflict continue below the blocking reservation.
            if let Some(start) = address
                .checked_sub(len)
                .map(|start| start & !(GRANULARITY - 1))
                .filter(|&start| start >= floor && start > 0)
                && let Ok(region) = query(start as *const u8)
            {
                if region.state != 0x10000 {
                    down = Some(region.allocation);
                } else if region.base + region.region < start + len {
                    down = query((region.base + region.region) as *const u8)
                        .ok()
                        .map(|blocker| blocker.allocation);
                } else if let Some(base) = reserve_at(start, len) {
                    return Some(base);
                } else {
                    // Another thread took the range; retry below it.
                    down = Some(start);
                }
            }
        }
    }
    None
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Code,
    ReadOnly,
    Writable,
}
struct Allocation {
    base: NonNull<u8>,
    len: usize,
    used: usize,
    kind: Kind,
    published: bool,
}

#[derive(Default)]
pub(crate) struct WindowsMemory {
    allocations: Vec<Allocation>,
}
// SAFETY: allocation ownership is exclusive and addresses do not move. A
// compiler can transfer the finalized module to its execution owner; metadata
// and generated pages are never mutated concurrently through this provider.
unsafe impl Send for WindowsMemory {}

impl WindowsMemory {
    fn allocate(&mut self, size: usize, align: u64, kind: Kind) -> io::Result<*mut u8> {
        self.allocate_with_near(size, align, kind, near)
    }

    fn allocate_with_near(
        &mut self,
        size: usize,
        align: u64,
        kind: Kind,
        allocate_near: impl FnOnce(usize, usize, usize) -> Option<*mut u8>,
    ) -> io::Result<*mut u8> {
        let align =
            usize::try_from(align).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        if !align.is_power_of_two() {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let size = size.max(1);
        let fit = |allocation: &Allocation| {
            let address = (allocation.base.as_ptr() as usize).checked_add(allocation.used)?;
            let aligned = address.checked_add(align - 1)? & !(align - 1);
            let offset = aligned.checked_sub(allocation.base.as_ptr() as usize)?;
            (offset.checked_add(size)? <= allocation.len).then_some(offset)
        };
        for allocation in self.allocations.iter_mut().rev() {
            if kind != Kind::Code
                && allocation.kind == kind
                && !allocation.published
                && let Some(offset) = fit(allocation)
            {
                allocation.used = offset + size;
                return Ok(unsafe { allocation.base.as_ptr().add(offset) });
            }
        }
        let code_len = size
            .checked_add(align - 1)
            .and_then(|n| n.checked_next_multiple_of(PAGE))
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
        let len = code_len
            .checked_add(if kind == Kind::Code { PAGE } else { 0 })
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
        if len > RELATIVE_WINDOW {
            return Err(io::ErrorKind::OutOfMemory.into());
        }
        let span = self.allocations.iter().fold(None, |span, piece| {
            let start = piece.base.as_ptr() as usize;
            let end = start + piece.len;
            Some(span.map_or((start, end), |(low, high): (usize, usize)| {
                (low.min(start), high.max(end))
            }))
        });
        let pointer = match span {
            Some((low, high)) => allocate_near(low, high, len)
                // A far fallback can make Cranelift's module-local rel32
                // references overflow. Reject this compilation instead; its
                // unmodified guest code remains available to the interpreter.
                .ok_or_else(|| io::Error::from(io::ErrorKind::OutOfMemory))?,
            None => {
                // SAFETY: the first piece owns a fresh reservation; all later
                // pieces must stay in its relative window. Top-down placement
                // keeps modules away from bottom-up guest address spaces, which
                // would otherwise fill the window right after the first piece.
                unsafe { VirtualAlloc(std::ptr::null_mut(), len, 0x3000 | 0x10_0000, 4) }
                    .cast::<u8>()
            }
        };
        let base = NonNull::new(pointer).ok_or_else(io::Error::last_os_error)?;
        let mut allocation = Allocation {
            base,
            len,
            used: 0,
            kind,
            published: false,
        };
        let offset = fit(&allocation).expect("new aligned JIT allocation must fit");
        debug_assert!(kind != Kind::Code || offset + size <= code_len);
        allocation.used = offset + size;
        let pointer = unsafe { base.as_ptr().add(offset) };
        self.allocations.push(allocation);
        Ok(pointer)
    }

    fn release(&mut self) {
        for allocation in self.allocations.drain(..) {
            // MEM_RELEASE requires the original reservation base and size 0.
            if unsafe { VirtualFree(allocation.base.as_ptr().cast(), 0, 0x8000) } == 0 {
                // There is no valid way to continue retirement with an OS
                // mapping that unexpectedly rejects its exclusive owner.
                std::process::abort();
            }
        }
    }
}

impl JITMemoryProvider for WindowsMemory {
    fn allocate_readexec(&mut self, size: usize, align: u64) -> io::Result<*mut u8> {
        self.allocate(size, align, Kind::Code)
    }
    fn allocate_readwrite(&mut self, size: usize, align: u64) -> io::Result<*mut u8> {
        self.allocate(size, align, Kind::Writable)
    }
    fn allocate_readonly(&mut self, size: usize, align: u64) -> io::Result<*mut u8> {
        self.allocate(size, align, Kind::ReadOnly)
    }
    unsafe fn free_memory(&mut self) {
        self.release();
    }
    fn finalize(&mut self, branch_protection: BranchProtection) -> ModuleResult<()> {
        if branch_protection != BranchProtection::None {
            return Err(ModuleError::Backend(
                io::Error::new(
                    io::ErrorKind::Unsupported,
                    "unsupported Windows x64 branch protection",
                )
                .into(),
            ));
        }
        for allocation in &mut self.allocations {
            if allocation.published {
                continue;
            }
            let protection = match allocation.kind {
                Kind::Code => 0x20,
                Kind::ReadOnly => 2,
                Kind::Writable => 4,
            };
            let mut old = 0;
            let span = allocation.len
                - if allocation.kind == Kind::Code {
                    PAGE
                } else {
                    0
                };
            if protection != 4
                && unsafe {
                    VirtualProtect(allocation.base.as_ptr().cast(), span, protection, &mut old)
                } == 0
            {
                return Err(ModuleError::Backend(io::Error::last_os_error().into()));
            }
            if allocation.kind == Kind::Code
                && unsafe {
                    VirtualProtect(allocation.base.as_ptr().add(span).cast(), PAGE, 2, &mut old)
                } == 0
            {
                return Err(ModuleError::Backend(io::Error::last_os_error().into()));
            }
            if allocation.kind == Kind::Code
                && unsafe {
                    FlushInstructionCache(
                        (-1isize) as *mut c_void,
                        allocation.base.as_ptr().cast(),
                        span,
                    )
                } == 0
            {
                return Err(ModuleError::Backend(io::Error::last_os_error().into()));
            }
            allocation.published = true;
        }
        Ok(())
    }
}
impl Drop for WindowsMemory {
    fn drop(&mut self) {
        // Compiler errors never publish an entry and still retire their pages.
        // Successful owners first unregister unwind data, then free the module.
        self.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn finalization_separates_code_constants_data_and_later_publications() {
        let mut provider = WindowsMemory::default();
        let code = provider.allocate_readexec(64, 32).unwrap();
        let constant = provider.allocate_readonly(64, 64).unwrap();
        let writable = provider.allocate_readwrite(64, 16).unwrap();
        assert_eq!(code as usize % 32, 0);
        assert_eq!(constant as usize % 64, 0);
        assert_eq!(query(code).unwrap().protection, 4);
        provider.finalize(BranchProtection::None).unwrap();
        assert_eq!(query(code).unwrap().protection, 0x20);
        let (xdata, capacity) = unsafe { unwind_storage(code as usize, 64) }.unwrap();
        assert_eq!(capacity, PAGE);
        assert_eq!(query(xdata).unwrap().protection, 2);
        assert_eq!(query(constant).unwrap().protection, 2);
        assert_eq!(query(writable).unwrap().protection, 4);
        let next = provider.allocate_readexec(16, 16).unwrap();
        assert_ne!(
            query(next).unwrap().allocation,
            query(code).unwrap().allocation
        );
        assert_eq!(query(next).unwrap().protection, 4);
        assert_eq!(query(code).unwrap().protection, 0x20);
        provider.finalize(BranchProtection::None).unwrap();
        assert_eq!(query(next).unwrap().protection, 0x20);
        unsafe {
            provider.free_memory();
        }
        assert!(provider.allocations.is_empty());
    }
    #[test]
    fn explicit_release_and_compile_error_drop_release_real_os_reservations() {
        for explicit in [false, true] {
            let mut provider = WindowsMemory::default();
            // Query an interior page after release: simultaneous tiny JIT
            // allocations can recycle the beginning of this former span.
            let pointer = provider.allocate_readexec(1024 * 1024, 16).unwrap();
            let interior = unsafe { pointer.add(768 * 1024) };
            assert_eq!(query(interior).unwrap().state, 0x1000);
            if explicit {
                provider.finalize(BranchProtection::None).unwrap();
                unsafe {
                    provider.free_memory();
                }
            }
            drop(provider);
            assert_eq!(query(interior).unwrap().state, 0x10000);
        }
    }
    #[test]
    fn module_pieces_stay_within_relative_reach_of_each_other() {
        let mut provider = WindowsMemory::default();
        let code = provider.allocate_readexec(64, 16).unwrap() as usize;
        // Several guest-sized reservations made between pieces of one module.
        let blockers: Vec<_> = (0..4)
            .map(|_| unsafe { VirtualAlloc(std::ptr::null_mut(), 1 << 32, 0x2000, 1) })
            .collect();
        let constant = provider.allocate_readonly(64, 16).unwrap() as usize;
        let data = provider.allocate_readwrite(64, 16).unwrap() as usize;
        for blocker in blockers.into_iter().filter(|blocker| !blocker.is_null()) {
            assert_ne!(unsafe { VirtualFree(blocker, 0, 0x8000) }, 0);
        }
        for piece in [constant, data] {
            assert!(piece.abs_diff(code) < 1 << 31, "{code:#x} {piece:#x}");
        }
    }
    #[test]
    fn exhausted_near_search_fails_without_far_fallback_or_partial_owner() {
        let mut provider = WindowsMemory::default();
        let code = provider.allocate_readexec(64, 16).unwrap();
        let allocated_bytes: usize = provider.allocations.iter().map(|piece| piece.len).sum();
        for kind in [Kind::Code, Kind::ReadOnly, Kind::Writable] {
            let used = provider.allocations[0].used;
            let error = provider
                .allocate_with_near(64, 16, kind, |low, high, len| {
                    assert_eq!(low, code as usize);
                    assert!(high > low && high - low <= RELATIVE_WINDOW);
                    assert!(len <= RELATIVE_WINDOW);
                    None
                })
                .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::OutOfMemory);
            assert_eq!(provider.allocations.len(), 1);
            assert_eq!(
                provider
                    .allocations
                    .iter()
                    .map(|piece| piece.len)
                    .sum::<usize>(),
                allocated_bytes
            );
            assert_eq!(provider.allocations[0].used, used);
            assert_eq!(query(code).unwrap().protection, 4);
        }
        let constant = provider.allocate_readonly(64, 16).unwrap();
        assert!((constant as usize).abs_diff(code as usize) < RELATIVE_WINDOW);
        provider.finalize(BranchProtection::None).unwrap();
        assert_eq!(query(code).unwrap().protection, 0x20);
        assert_eq!(query(constant).unwrap().protection, 2);
        // SAFETY: the live finalized code owner retains its private xdata page.
        let (storage, capacity) = unsafe { unwind_storage(code as usize, 64) }.unwrap();
        assert_eq!(capacity, PAGE);
        assert_eq!(query(storage).unwrap().protection, 2);
    }
    #[test]
    fn near_search_also_places_pieces_below_the_module() {
        // Nothing above the 47-bit user address space can be reserved, so the
        // search must find room below a span that ends there.
        let top = 0x7fff_ffff_0000;
        let piece = near(top, top, 0x2_0000).unwrap() as usize;
        assert!(piece < top && top - piece <= RELATIVE_WINDOW, "{piece:#x}");
        assert_eq!(piece % GRANULARITY, 0);
        assert_eq!(query(piece as *const u8).unwrap().protection, 4);
        assert_ne!(unsafe { VirtualFree(piece as *mut c_void, 0, 0x8000) }, 0);
    }
    #[test]
    fn first_piece_cannot_exceed_the_modules_relative_window() {
        let mut provider = WindowsMemory::default();
        let error = provider
            .allocate_with_near(RELATIVE_WINDOW, 16, Kind::Code, |_, _, _| {
                panic!("a first allocation must not search relative to another owner")
            })
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::OutOfMemory);
        assert!(provider.allocations.is_empty());
    }
    #[test]
    fn invalid_sizes_and_alignments_leave_no_partial_allocation() {
        let mut provider = WindowsMemory::default();
        for (size, alignment) in [(8, 0), (8, 3), (usize::MAX, 16)] {
            assert!(provider.allocate_readexec(size, alignment).is_err());
            assert!(provider.allocations.is_empty());
        }
    }
}
