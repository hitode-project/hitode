//! Dedicated device-readable guest storage. No borrowed guest slice or VM
//! pointer may cross into a device callback.
use std::sync::Arc;

/// A stable, byte-atomic, read-only view. Implementations must retain their
/// backing allocation for this object's lifetime and support concurrent byte
/// atomic stores by the guest owner. Loads never lock, allocate, or perform IO.
/// Wide mixed-size atomics and ordinary concurrent Rust writes are forbidden.
pub trait SharedReadView: Send + Sync {
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// Return one relaxed atomic byte; out-of-range reads return zero.
    fn load_u8(&self, index: usize) -> u8;
    fn copy_into(&self, offset: usize, destination: &mut [u8]) {
        for (index, byte) in destination.iter_mut().enumerate() {
            *byte = offset.checked_add(index).map_or(0, |at| self.load_u8(at));
        }
    }
}

#[derive(Clone)]
pub struct SharedRegion {
    pub guest_base: u32,
    pub len: u32,
    pub read_alias: Arc<dyn SharedReadView>,
}

impl std::fmt::Debug for SharedRegion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SharedRegion")
            .field("guest_base", &self.guest_base)
            .field("len", &self.len)
            .finish_non_exhaustive()
    }
}
