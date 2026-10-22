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

#[cfg(windows)]
pub fn fill(bytes: &mut [u8]) -> io::Result<()> {
    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn SystemFunction036(buffer: *mut u8, length: u32) -> u8;
    }
    for chunk in bytes.chunks_mut(u32::MAX as usize) {
        // SAFETY: the host function writes exactly the supplied, live slice.
        if unsafe { SystemFunction036(chunk.as_mut_ptr(), chunk.len() as u32) } == 0 {
            return Err(io::Error::other("host entropy source failed"));
        }
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
pub fn fill(_bytes: &mut [u8]) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "host entropy source unavailable",
    ))
}
