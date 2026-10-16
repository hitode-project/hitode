//! Executable storage for translated code (A64-CTL-1, A64-CTL-2).
//!
//! Each owner leases a contiguous range from the host process code pool,
//! so a direct `B`/`BL` reaches every unit within that lease. Code is
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

use adv_cpu_core::code_pool::{CodeLease, CodePool};
mod backend;
use std::ptr::NonNull;
use std::sync::OnceLock;

/// Direct branches reach ±128 MiB.
pub const MAX_LEN: usize = 128 << 20;

pub struct CodeCache {
    /// Where the translator writes.
    write: NonNull<u8>,
    /// Where the CPU executes; equal to `write` on macOS.
    exec: NonNull<u8>,
    len: usize,
    used: usize,
    _storage: Storage,
    /// Writes not yet synchronized, as offset and length.
    dirty: Vec<(usize, usize)>,
}

impl CodeCache {
    pub fn new(len: usize) -> Result<Self, String> {
        if len == 0 || len > MAX_LEN || !len.is_multiple_of(page_size()?) {
            return Err(format!("bad code cache length {len:#x}"));
        }
        let pool = prepared_pool()?;
        let (write, exec, storage) = match lease(&pool.ranges, len, strict_pool()) {
            Some(lease) => {
                let offset = lease.range().start;
                // SAFETY: this disjoint lease stays inside the process-lifetime pool.
                unsafe {
                    (
                        NonNull::new_unchecked(pool.mapping.write.as_ptr().add(offset)),
                        NonNull::new_unchecked(pool.mapping.exec.as_ptr().add(offset)),
                        Storage::Lease(lease),
                    )
                }
            }
            None if !strict_pool() => {
                let mapping = backend::Mapping::new(len)?;
                (mapping.write, mapping.exec, Storage::Own(mapping))
            }
            None => return Err("prepared A64 code pool exhausted".into()),
        };
        let len = match &storage {
            Storage::Lease(lease) => lease.range().len(),
            Storage::Own(_) => len,
        };
        Ok(Self {
            write,
            exec,
            len,
            used: 0,
            _storage: storage,
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
        unsafe { backend::synchronize(self.write.as_ptr(), self.exec.as_ptr(), &self.dirty) };
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
        backend::writable(true);
        // SAFETY: `offset + len` is inside the mapping, which this cache owns,
        // and the write view is writable while `writable(true)` holds.
        unsafe {
            std::ptr::copy_nonoverlapping(
                words.as_ptr().cast::<u8>(),
                self.write.as_ptr().add(offset),
                len,
            );
        }
        backend::writable(false);
    }
}

/// Owned by one cache; an own mapping unmaps when the cache drops.
enum Storage {
    Lease(#[allow(dead_code)] CodeLease),
    Own(#[allow(dead_code)] backend::Mapping),
}

/// Strict hosts halve the request down to `MIN_LEASE` before giving up:
/// a smaller cache only resets more often, while Tier 0 is far slower.
const MIN_LEASE: usize = 16 << 20;
fn lease(pool: &CodePool, len: usize, strict: bool) -> Option<CodeLease> {
    let mut want = len;
    loop {
        if let Some(lease) = pool.lease(want) {
            return Some(lease);
        }
        if !strict || want <= MIN_LEASE || !(want / 2).is_multiple_of(pool.alignment()) {
            return None;
        }
        want /= 2;
    }
}

/// Only iOS cannot map executable memory after preparation. Other hosts map
/// an exhausted pool's overflow separately, as before the pool existed.
fn strict_pool() -> bool {
    cfg!(target_os = "ios") || cfg!(test) && std::env::var_os("ADV_POOL_EXHAUSTION_CHILD").is_some()
}

/// Call before creating guest machines; on iOS, while the JIT debugger is
/// attached, which this preparation then detaches.
/// The pool is fixed after this first preparation; mapping failure is retained.
pub fn prepare_code_pool() -> Result<(), String> {
    prepared_pool().map(|_| ())
}

struct PreparedPool {
    ranges: CodePool,
    mapping: backend::Mapping,
}

// SAFETY: the mapping lives for the host process. Leases exclude overlapping
// writes; MAP_JIT permission toggles affect only the writing thread.
unsafe impl Send for PreparedPool {}
unsafe impl Sync for PreparedPool {}

fn prepared_pool() -> Result<&'static PreparedPool, String> {
    static POOL: OnceLock<Result<PreparedPool, String>> = OnceLock::new();
    POOL.get_or_init(|| {
        let mib = match std::env::var("ADV_A64_CODE_POOL_MIB") {
            Ok(value) => value
                .parse::<usize>()
                .map_err(|_| "invalid ADV_A64_CODE_POOL_MIB")?,
            // iOS: the debugger marks every 16 KiB page, so launch time follows the size.
            Err(std::env::VarError::NotPresent) if cfg!(target_os = "ios") => 128,
            Err(std::env::VarError::NotPresent) => 512,
            Err(_) => return Err("invalid ADV_A64_CODE_POOL_MIB".into()),
        };
        let len = mib
            .checked_mul(1 << 20)
            .ok_or("A64 code pool size overflow")?;
        let ranges = CodePool::new(len, page_size()?)?;
        let mapping = backend::Mapping::new(len);
        #[cfg(target_os = "ios")]
        backend::finish_preparation();
        Ok(PreparedPool {
            ranges,
            mapping: mapping?,
        })
    })
    .as_ref()
    .map_err(Clone::clone)
}

fn page_size() -> Result<usize, String> {
    // SAFETY: sysconf only queries the host.
    let size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    usize::try_from(size)
        .ok()
        .filter(|&size| size != 0)
        .ok_or_else(|| "cannot determine host page size".into())
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

    // spec: A64-CTL-2
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

    // spec: A64-CTL-13
    #[test]
    fn strict_pools_shrink_leases_before_giving_up() {
        let pool = CodePool::new(64 << 20, page_size().unwrap()).unwrap();
        let _held = pool.lease(40 << 20).unwrap();
        assert!(lease(&pool, 64 << 20, false).is_none());
        let small = lease(&pool, 64 << 20, true).unwrap();
        assert_eq!(small.range().len(), MIN_LEASE);
        assert!(lease(&pool, 64 << 20, true).is_none());
    }

    // spec: A64-CTL-1, A64-CTL-13, A64-ISA-3
    #[test]
    fn pool_exhaustion_runs_tier_zero_without_new_mappings() {
        if std::env::var_os("ADV_POOL_EXHAUSTION_CHILD").is_none() {
            let exe = std::env::current_exe().unwrap();
            let mut command =
                match std::env::var_os("CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUNNER") {
                    Some(runner) => {
                        let mut c = std::process::Command::new(runner);
                        c.arg(exe);
                        c
                    }
                    None => std::process::Command::new(exe),
                };
            let output = command
                .args([
                    "--exact",
                    "cache::tests::pool_exhaustion_runs_tier_zero_without_new_mappings",
                ])
                .env("ADV_POOL_EXHAUSTION_CHILD", "1")
                .env("ADV_A64_CODE_POOL_MIB", "64")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let pool = prepared_pool().unwrap();
        let mut leases = Vec::new();
        while let Some(lease) = pool.ranges.lease(64 << 20) {
            leases.push(lease);
        }
        let mut executor = crate::Executor::new(0x10000, 0x10000).unwrap();
        assert!(executor.diagnostics().contains("code pool exhausted"));
        executor.protect(0x10000, 0x10000, 7).unwrap();
        // Authored integer instruction, not sample code.
        executor.write(0x10000, &[0x40]).unwrap();
        executor.state.eip = 0x10000;
        assert_eq!(
            executor.run(0x10001, 8).unwrap(),
            adv_cpu_core::RunOutcome::ReachedStop
        );
        assert_eq!(executor.state.regs[0], 1);
        assert_eq!(executor.stats.native_entries, 0);
        assert!(executor.stats.interpreter_instructions > 0);
        drop(executor);
        drop(leases);
        assert!(CodeCache::new(64 << 20).is_ok());
    }

    // spec: A64-CTL-1, A64-CTL-13
    #[test]
    fn a_full_cache_refuses_more_code() {
        let page = page_size().unwrap();
        let mut cache = CodeCache::new(page).unwrap();
        assert!(cache.append(&vec![NOP; page / 4]).is_some());
        assert!(cache.append(&[NOP]).is_none());
        assert!(CodeCache::new(MAX_LEN + page).is_err());
        let start = cache.next() - page;
        cache.truncate(0);
        assert_eq!(cache.append(&[NOP]), Some(start));
        cache.sync();
    }
}
