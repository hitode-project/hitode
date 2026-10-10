//! Executable image metadata the loader hands to the execution engine.

/// Loader metadata uses module-relative offsets. An empty snapshot still
/// registers the module for persistent-cache identity.
pub struct AotImage {
    /// Digest of the original PE bytes, before relocation or import binding.
    pub identity: [u8; 32],
    pub base: u32,
    pub size: u32,
    pub primary: bool,
    pub image: Vec<u8>,
    /// Half-open executable RVA ranges, clipped to the mapped image.
    pub executable: Vec<(u32, u32)>,
    /// Entry point and TLS first, followed by exports and relocated pointers.
    pub seeds: Vec<u32>,
}

impl AotImage {
    pub fn identity(bytes: &[u8]) -> [u8; 32] {
        use sha2::{Digest, Sha256};
        Sha256::digest(bytes).into()
    }
}
