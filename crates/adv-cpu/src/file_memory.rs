//! Worker-owned PCM and lazy/COW operands after native software guards.
use std::panic::{AssertUnwindSafe, catch_unwind};

unsafe fn memory<'a>(bridge: *mut u8) -> &'a mut crate::Memory {
    unsafe { &mut *(*bridge.cast::<crate::trap::Bridge<'_>>()).memory }
}

// Buffers are the bridge's bounded scratch, never guest slices.
pub(crate) unsafe extern "C" fn read(
    bridge: *mut u8,
    address: u32,
    output: *mut u8,
    width: u32,
) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        let memory = unsafe { memory(bridge) };
        memory.prepare_native_access(address, width as usize, false)?;
        let output = unsafe { std::slice::from_raw_parts_mut(output, width as usize) };
        memory.read_into(address, output)
    }))
    .map_or(1, |result| u32::from(result.is_err()))
}

pub(crate) unsafe extern "C" fn write(
    bridge: *mut u8,
    address: u32,
    input: *const u8,
    width: u32,
) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        let memory = unsafe { memory(bridge) };
        memory.prepare_native_access(address, width as usize, true)?;
        let input = unsafe { std::slice::from_raw_parts(input, width as usize) };
        memory.write(address, input)
    }))
    .map_or(1, |result| u32::from(result.is_err()))
}

/// Whether a store would change observed code. Only such stores leave native
/// code; other data on a code page is stored in place.
pub(crate) unsafe extern "C" fn code_store(bridge: *mut u8, address: u32, width: u32) -> u32 {
    u32::from(unsafe { memory(bridge) }.stores_code(address, width as usize))
}

/// Prepare an RMW destination before generated SSA flags/registers change.
pub(crate) unsafe extern "C" fn prepare(bridge: *mut u8, address: u32, width: u32) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        unsafe { memory(bridge) }.prepare_native_access(address, width as usize, true)
    }))
    .map_or(1, |result| u32::from(result.is_err()))
}

/// The full range was prepared by prepare(), under the same exclusive worker
/// borrow. No guest/trap/mapping mutation can intervene, so this is infallible.
/// An unexpected failure is an internal contract violation, not a guest fault
/// that could be reported after the RMW's SSA flags have already changed.
pub(crate) unsafe extern "C" fn write_prepared(
    bridge: *mut u8,
    address: u32,
    input: *const u8,
    width: u32,
) {
    let memory = unsafe { memory(bridge) };
    let input = unsafe { std::slice::from_raw_parts(input, width as usize) };
    if memory.write(address, input).is_err() {
        std::process::abort();
    }
}
