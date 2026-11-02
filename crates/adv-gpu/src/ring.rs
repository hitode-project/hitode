//! The upload ring: one persistent mapped buffer per device, written in order
//! by producers and read in place by the GPU.
//!
//! It replaces per-write staging. Vertex, index and constant data, guest
//! texture uploads and small copies all live in spans; the owner thread binds
//! or copies from them without touching the bytes again. Today's Metal path
//! already works this way with a 64 MiB ring (`gpu/metal/ring.rs`).

use crate::{Buffer, Result};

/// One ring per producer thread. `Send`, not `Sync`.
pub struct Ring {
    pub(crate) inner: crate::imp::Ring,
}

impl Ring {
    /// Reserve `size` bytes aligned to `align`. Returns [`crate::Error::Busy`]
    /// when the space is still in flight; nothing is reserved then, so the
    /// caller may flush and retry without having written anything.
    pub fn reserve(&mut self, size: u64, align: u64) -> Result<Span> {
        self.inner.reserve(size, align)
    }
}

/// Reserved ring bytes. Write them, then hand the span to the owner thread with
/// the commands that use it. Spans retire in reservation order when the last
/// submission that used them completes ([`crate::Encoder::retain`]). A span
/// dropped without use retires with the next submission.
pub struct Span {
    pub(crate) inner: crate::imp::Span,
}

impl Span {
    pub fn bytes(&mut self) -> &mut [u8] {
        self.inner.bytes()
    }
    pub fn buffer(&self) -> &Buffer {
        self.inner.buffer()
    }
    pub fn offset(&self) -> u64 {
        self.inner.offset()
    }
    pub fn len(&self) -> u64 {
        self.inner.len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
