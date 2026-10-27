//! Host entropy adapter shared by guest cryptographic APIs. This is host-only
//! I/O: it never opens guest paths, exposes a device handle, or changes VFS state.
//! A failed source is propagated; deterministic generators are not a fallback.
use std::io;

#[cfg(unix)]
pub fn fill(bytes: &mut [u8]) -> io::Result<()> {
    use std::io::Read;
    if bytes.is_empty() {
        return Ok(());
    }
    std::fs::File::open("/dev/urandom")?.read_exact(bytes)
}
