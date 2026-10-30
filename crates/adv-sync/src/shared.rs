//! Dedicated device-readable guest storage. No borrowed guest slice or VM
//! pointer may cross into a device callback.
use std::sync::Arc;

/// A stable, atomically read view. Implementations retain their backing
/// allocation and support concurrent atomic stores by the guest owner.
/// Loads never lock, allocate, or perform IO. Every concurrent Rust accessor
/// uses one aligned atomic width; byte loads may extract lanes from a word.
/// Overlapping mixed-width accesses involving writes and ordinary Rust byte
/// references are forbidden. Native guest code accesses linear OS aliases under
/// their shared-memory contract. Guest unmapping and protection changes affect
/// only the CPU view; this view retains its independent storage and OS read-only
/// protection until its last owner drops.
pub trait SharedReadView: Send + Sync {
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// Return one byte using a relaxed atomic load; out-of-range reads return zero.
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
