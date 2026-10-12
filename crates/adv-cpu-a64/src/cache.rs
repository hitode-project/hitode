//! Executable storage for translated code (execution ABI §A5.1).
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
//!
//! Writes only record what changed; `sync` makes it visible to instruction
//! fetch, once per batch, before the owner runs translated code again. An
//! `ic ivau` sequence costs about the same for one line as for a unit, so
//! one sync per batch instead of one per unit or patch.

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
    /// Writes not yet synchronized, as offset and length.
    dirty: Vec<(usize, usize)>,
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
            dirty: Vec::new(),
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
        self.mark(self.used, bytes);
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
        self.mark(offset, 4);
    }

    fn mark(&mut self, offset: usize, len: usize) {
        match self.dirty.last_mut() {
            Some((start, dirty)) if *start + *dirty == offset => *dirty += len,
            _ => self.dirty.push((offset, len)),
        }
    }

    /// The word at `address`, which must be inside the cache.
    pub fn word(&self, address: usize) -> u32 {
        let offset = address
            .checked_sub(self.exec.as_ptr() as usize)
            .filter(|&offset| offset + 4 <= self.used && offset.is_multiple_of(4))
            .expect("word inside the cache");
        // SAFETY: inside the live, readable write view.
        unsafe { self.write.as_ptr().add(offset).cast::<u32>().read() }
    }

    /// Makes every write since the last sync visible to instruction fetch.
    pub fn sync(&mut self) {
        if self.dirty.is_empty() {
            return;
        }
        // SAFETY: every range is inside the live mapping.
        unsafe { os::synchronize(self.write.as_ptr(), self.exec.as_ptr(), &self.dirty) };
        self.dirty.clear();
    }

    /// Forgets everything after the first `keep` bytes.
    pub fn truncate(&mut self, keep: usize) {
        assert!(keep <= self.used);
        self.used = keep;
        // Later appends reuse the space and are synchronized themselves.
        self.dirty.retain(|&(start, _)| start < keep);
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

#[cfg(any(target_os = "linux", target_os = "android"))]
fn each_line(base: *const u8, line: usize, ranges: &[(usize, usize)], mut each: impl FnMut(usize)) {
    for &(offset, len) in ranges {
        let start = base as usize + offset;
        let mut at = start & !(line - 1);
        while at < start + len {
            each(at);
            at += line;
        }
    }
}

/// Cleans data lines through the write view to the point of unification,
/// then invalidates instruction lines through the execute view, with one
/// set of barriers for all `ranges` (offsets and lengths). Cores with
/// CTR_EL0.IDC need no data clean.
///
/// # Safety
/// Every range must be mapped in both views.
#[cfg(any(target_os = "linux", target_os = "android"))]
unsafe fn clean_and_invalidate(write: *const u8, exec: *const u8, ranges: &[(usize, usize)]) {
    use std::arch::asm;
    let ctr: u64;
    // SAFETY: CTR_EL0 is readable at EL0 on Linux.
    unsafe { asm!("mrs {}, ctr_el0", out(reg) ctr, options(nomem, nostack)) };
    if ctr >> 28 & 1 == 0 {
        let data_line = 4usize << (ctr >> 16 & 0xf);
        // SAFETY: cache maintenance on mapped addresses has no other effect.
        each_line(write, data_line, ranges, |at| unsafe {
            asm!("dc cvau, {}", in(reg) at, options(nostack))
        });
    }
    // SAFETY: barriers only.
    unsafe { asm!("dsb ish", options(nostack)) };
    let code_line = 4usize << (ctr & 0xf);
    // SAFETY: as above.
    each_line(exec, code_line, ranges, |at| unsafe {
        asm!("ic ivau, {}", in(reg) at, options(nostack))
    });
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
    /// Every range must be mapped in both views.
    pub unsafe fn synchronize(write: *const u8, exec: *const u8, ranges: &[(usize, usize)]) {
        // SAFETY: forwarded.
        unsafe { super::clean_and_invalidate(write, exec, ranges) };
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
    /// Every range must be mapped.
    pub unsafe fn synchronize(_: *const u8, exec: *const u8, ranges: &[(usize, usize)]) {
        for &(offset, len) in ranges {
            // SAFETY: forwarded.
            unsafe { sys_icache_invalidate(exec.add(offset).cast_mut().cast(), len) };
        }
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
        cache.sync();
        assert_eq!(call(first), 42);
        cache.patch(first, movz(W, Reg::new(0), 7, 0));
        assert_eq!(cache.word(first), movz(W, Reg::new(0), 7, 0));
        cache.sync();
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
