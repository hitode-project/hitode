//! Executable storage for translated code (workbook §7.1).
//!
//! One contiguous range, so a direct `B`/`BL` reaches every unit. Code is
//! never writable and executable through the same mapping at once:
//!
//! - Linux and Android map one memfd twice, a read-write view the
//!   translator writes and a read-execute view the CPU runs.
//! - macOS uses one `MAP_JIT` range and flips the calling thread between
//!   writing and executing with `pthread_jit_write_protect_np`.
//!
//! When the cache fills, the owner clears it and translates again.

use std::ptr::NonNull;

/// Direct branches reach ±128 MiB.
pub const MAX_LEN: usize = 128 << 20;

pub struct CodeCache {
    /// Where the translator writes.
    write: NonNull<u8>,
    /// Where the CPU executes; equal to `write` on macOS.
    exec: NonNull<u8>,
    len: usize,
    used: usize,
}

impl CodeCache {
    pub fn new(len: usize) -> Result<Self, String> {
        if len == 0 || len > MAX_LEN || !len.is_multiple_of(page_size()?) {
            return Err(format!("bad code cache length {len:#x}"));
        }
        let (write, exec) = os::map(len)?;
        Ok(Self {
            write,
            exec,
            len,
            used: 0,
        })
    }

    /// Executable address of the next appended word.
    pub fn next(&self) -> usize {
        self.exec.as_ptr() as usize + self.used
    }

    pub fn used(&self) -> usize {
        self.used
    }

    /// Appends `words` and returns their executable address, or `None` when
    /// they do not fit.
    pub fn append(&mut self, words: &[u32]) -> Option<usize> {
        let bytes = words.len() * 4;
        if bytes > self.len - self.used {
            return None;
        }
        let address = self.next();
        self.store(self.used, words);
        self.used += bytes;
        Some(address)
    }

    /// Rewrites one word that is already in the cache.
    pub fn patch(&mut self, address: usize, word: u32) {
        let offset = address
            .checked_sub(self.exec.as_ptr() as usize)
            .filter(|&offset| offset + 4 <= self.used && offset.is_multiple_of(4))
            .expect("patch inside the cache");
        self.store(offset, &[word]);
    }

    /// Forgets everything after the first `keep` bytes.
    pub fn truncate(&mut self, keep: usize) {
        assert!(keep <= self.used);
        self.used = keep;
    }

    fn store(&mut self, offset: usize, words: &[u32]) {
        let len = words.len() * 4;
        os::writable(true);
        // SAFETY: `offset + len` is inside the mapping, which this cache owns,
        // and the write view is writable while `writable(true)` holds.
        unsafe {
            std::ptr::copy_nonoverlapping(
                words.as_ptr().cast::<u8>(),
                self.write.as_ptr().add(offset),
                len,
            );
        }
        os::writable(false);
        // SAFETY: both ranges are inside the live mapping.
        unsafe {
            os::synchronize(
                self.write.as_ptr().add(offset),
                self.exec.as_ptr().add(offset),
                len,
            );
        }
    }
}

impl Drop for CodeCache {
    fn drop(&mut self) {
        // SAFETY: the views were mapped by `os::map` with this length and no
        // translated code runs once the owner drops its cache.
        unsafe { os::unmap(self.write, self.exec, self.len) };
    }
}

fn page_size() -> Result<usize, String> {
    // SAFETY: sysconf only queries the host.
    let size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    usize::try_from(size)
        .ok()
        .filter(|&size| size != 0)
        .ok_or_else(|| "cannot determine host page size".into())
}

/// Cleans data lines through the write view to the point of unification,
/// then invalidates instruction lines through the execute view.
///
/// # Safety
/// Both ranges must be mapped.
#[cfg(any(target_os = "linux", target_os = "android"))]
unsafe fn clean_and_invalidate(write: *const u8, exec: *const u8, len: usize) {
    use std::arch::asm;
    let ctr: u64;
    // SAFETY: CTR_EL0 is readable at EL0 on Linux.
    unsafe { asm!("mrs {}, ctr_el0", out(reg) ctr, options(nomem, nostack)) };
    let data_line = 4usize << (ctr >> 16 & 0xf);
    let code_line = 4usize << (ctr & 0xf);
    let mut line = write as usize & !(data_line - 1);
    while line < write as usize + len {
        // SAFETY: cache maintenance on mapped addresses has no other effect.
        unsafe { asm!("dc cvau, {}", in(reg) line, options(nostack)) };
        line += data_line;
    }
    // SAFETY: barriers only.
    unsafe { asm!("dsb ish", options(nostack)) };
    let mut line = exec as usize & !(code_line - 1);
    while line < exec as usize + len {
        // SAFETY: as above.
        unsafe { asm!("ic ivau, {}", in(reg) line, options(nostack)) };
        line += code_line;
    }
    // SAFETY: barriers only.
    unsafe { asm!("dsb ish", "isb", options(nostack)) };
}

#[cfg(any(target_os = "linux", target_os = "android"))]
mod os {
    use std::os::fd::AsRawFd;
    use std::ptr::NonNull;

    pub fn map(len: usize) -> Result<(NonNull<u8>, NonNull<u8>), String> {
        let fd = adv_cpu_core::shared_memory::create_backing(c"adv-a64-code")?;
        let size = libc::off_t::try_from(len).map_err(|_| "code cache is too large")?;
        // SAFETY: the descriptor is fresh and owned here.
        if unsafe { libc::ftruncate(fd.as_raw_fd(), size) } != 0 {
            return Err(format!(
                "size code cache: {}",
                std::io::Error::last_os_error()
            ));
        }
        let view = |protection| {
            // SAFETY: a new shared mapping of the sized object at an OS-chosen
            // address; it overlaps nothing.
            let address = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    len,
                    protection,
                    libc::MAP_SHARED,
                    fd.as_raw_fd(),
                    0,
                )
            };
            if address == libc::MAP_FAILED {
                Err(format!(
                    "map code cache view: {}",
                    std::io::Error::last_os_error()
                ))
            } else {
                NonNull::new(address.cast::<u8>()).ok_or_else(|| "code cache at null".into())
            }
        };
        let write = view(libc::PROT_READ | libc::PROT_WRITE)?;
        let exec = match view(libc::PROT_READ | libc::PROT_EXEC) {
            Ok(exec) => exec,
            Err(error) => {
                // SAFETY: `write` was mapped above with this length.
                unsafe { libc::munmap(write.as_ptr().cast(), len) };
                return Err(error);
            }
        };
        // The descriptor closes here; the two mappings keep the object.
        Ok((write, exec))
    }

    pub fn writable(_: bool) {}

    /// # Safety
    /// Both ranges must be mapped.
    pub unsafe fn synchronize(write: *const u8, exec: *const u8, len: usize) {
        // SAFETY: forwarded.
        unsafe { super::clean_and_invalidate(write, exec, len) };
    }

    /// # Safety
    /// The views must come from `map(len)` and be unused.
    pub unsafe fn unmap(write: NonNull<u8>, exec: NonNull<u8>, len: usize) {
        // SAFETY: forwarded.
        unsafe {
            libc::munmap(write.as_ptr().cast(), len);
            libc::munmap(exec.as_ptr().cast(), len);
        }
    }
}

#[cfg(target_os = "macos")]
mod os {
    use std::ptr::NonNull;

    unsafe extern "C" {
        fn pthread_jit_write_protect_np(enabled: libc::c_int);
        fn sys_icache_invalidate(start: *mut libc::c_void, len: libc::size_t);
    }

    pub fn map(len: usize) -> Result<(NonNull<u8>, NonNull<u8>), String> {
        // SAFETY: a new private JIT mapping at an OS-chosen address.
        let address = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
                libc::MAP_PRIVATE | libc::MAP_ANON | libc::MAP_JIT,
                -1,
                0,
            )
        };
        if address == libc::MAP_FAILED {
            return Err(format!(
                "map JIT code cache: {}",
                std::io::Error::last_os_error()
            ));
        }
        let view = NonNull::new(address.cast::<u8>()).ok_or("code cache at null")?;
        Ok((view, view))
    }

    /// Flips this thread's view of every MAP_JIT range.
    pub fn writable(enabled: bool) {
        // SAFETY: affects only the calling thread's JIT permission.
        unsafe { pthread_jit_write_protect_np(libc::c_int::from(!enabled)) };
    }

    /// # Safety
    /// The range must be mapped.
    pub unsafe fn synchronize(_: *const u8, exec: *const u8, len: usize) {
        // SAFETY: forwarded.
        unsafe { sys_icache_invalidate(exec.cast_mut().cast(), len) };
    }

    /// # Safety
    /// The view must come from `map(len)` and be unused.
    pub unsafe fn unmap(view: NonNull<u8>, _: NonNull<u8>, len: usize) {
        // SAFETY: forwarded.
        unsafe { libc::munmap(view.as_ptr().cast(), len) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encode::Size::W;
    use crate::encode::*;

    fn call(address: usize) -> u32 {
        // SAFETY: the code at `address` is a complete leaf function.
        let function: extern "C" fn() -> u32 = unsafe { std::mem::transmute(address) };
        function()
    }

    #[test]
    fn appended_code_runs_and_patches_take_effect() {
        let mut cache = CodeCache::new(page_size().unwrap()).unwrap();
        let first = cache
            .append(&[movz(W, Reg::new(0), 42, 0), ret(Reg::LR)])
            .unwrap();
        assert_eq!(call(first), 42);
        cache.patch(first, movz(W, Reg::new(0), 7, 0));
        assert_eq!(call(first), 7);
        let second = cache
            .append(&[movz(W, Reg::new(0), 9, 0), ret(Reg::LR)])
            .unwrap();
        assert_eq!(second, first + 8);
        cache.truncate(8);
        assert_eq!(cache.next(), second);
    }

    #[test]
    fn a_full_cache_refuses_more_code() {
        let page = page_size().unwrap();
        let mut cache = CodeCache::new(page).unwrap();
        assert!(cache.append(&vec![NOP; page / 4]).is_some());
        assert!(cache.append(&[NOP]).is_none());
        assert!(CodeCache::new(MAX_LEN + page).is_err());
    }
}
