//! Matching byte-atomic accesses to validated linear PCM aliases.
use std::sync::atomic::{AtomicU8, Ordering};

#[inline]
unsafe fn write<const N: usize>(pointer: *mut u8, low: u64, high: u64) {
    let bytes = [low.to_le_bytes(), high.to_le_bytes()];
    for index in 0..N {
        // SAFETY: native guards validated every mapped page and guest write
        // permission, including cross-page elements. The owner retains the
        // stable linear alias. Every concurrent device access is AtomicU8 too.
        unsafe {
            (&*pointer.add(index).cast::<AtomicU8>())
                .store(bytes[index / 8][index % 8], Ordering::Relaxed);
        }
    }
}

/// Only native code calls this with a completely validated 1/2/4/8/16-byte
/// range. Cranelift's atomic_store is SeqCst; this bounded helper preserves
/// the PCM protocol's Relaxed byte stores without emitting wide atomics.
pub unsafe extern "C" fn store(pointer: *mut u8, low: u64, high: u64, width: u32) {
    unsafe {
        match width {
            1 => write::<1>(pointer, low, high),
            2 => write::<2>(pointer, low, high),
            4 => write::<4>(pointer, low, high),
            8 => write::<8>(pointer, low, high),
            16 => write::<16>(pointer, low, high),
            _ => std::process::abort(),
        }
    }
}
