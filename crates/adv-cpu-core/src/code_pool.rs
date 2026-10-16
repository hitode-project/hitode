//! Platform-neutral leases in a fixed, already prepared executable pool.
//! Allocation never moves a live lease; dropping it coalesces adjacent holes.
use std::{
    ops::Range,
    sync::{Arc, Mutex},
};

#[derive(Clone)]
pub struct CodePool {
    free: Arc<Mutex<Vec<Range<usize>>>>,
    alignment: usize,
}

pub struct CodeLease {
    pool: CodePool,
    range: Range<usize>,
}

impl CodePool {
    pub fn new(len: usize, alignment: usize) -> Result<Self, String> {
        if !alignment.is_power_of_two() || len == 0 || !len.is_multiple_of(alignment) {
            return Err("code pool size must be nonzero and aligned".into());
        }
        Ok(Self {
            free: Arc::new(Mutex::new(vec![0..len])),
            alignment,
        })
    }

    /// None is a capability limit: the caller must use Tier 0 for this process.
    pub fn lease(&self, len: usize) -> Option<CodeLease> {
        if len == 0 || !len.is_multiple_of(self.alignment) {
            return None;
        }
        let mut free = self.free.lock().unwrap();
        let index = free.iter().position(|range| range.len() >= len)?;
        let start = free[index].start;
        free[index].start += len;
        if free[index].is_empty() {
            free.remove(index);
        }
        Some(CodeLease {
            pool: self.clone(),
            range: start..start + len,
        })
    }
}

impl CodePool {
    pub fn alignment(&self) -> usize {
        self.alignment
    }
}

impl CodeLease {
    pub fn range(&self) -> Range<usize> {
        self.range.clone()
    }
}

impl Drop for CodeLease {
    fn drop(&mut self) {
        let mut free = self.pool.free.lock().unwrap();
        let mut range = self.range.clone();
        let index = free.partition_point(|hole| hole.end < range.start);
        while index < free.len() && free[index].start <= range.end {
            let hole = free.remove(index);
            range.start = range.start.min(hole.start);
            range.end = range.end.max(hole.end);
        }
        free.insert(index, range);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // spec: A64-CTL-1, A64-CTL-13
    #[test]
    fn processes_keep_disjoint_leases_and_reuse_returned_space() {
        let pool = CodePool::new(4 * 4096, 4096).unwrap();
        let first = pool.lease(4096).unwrap();
        let second = pool.lease(8192).unwrap();
        let third = pool.lease(4096).unwrap();
        assert_eq!(first.range(), 0..4096);
        assert_eq!(second.range(), 4096..12288);
        assert_eq!(third.range(), 12288..16384);
        assert!(pool.lease(4096).is_none());
        drop(second);
        assert!(pool.lease(12288).is_none());
        drop(first);
        let joined = pool.lease(12288).unwrap();
        assert_eq!(joined.range(), 0..12288);
        drop(third);
        drop(joined);
        assert_eq!(pool.lease(16384).unwrap().range(), 0..16384);
    }

    // spec: A64-CTL-1, A64-CTL-13
    #[test]
    fn release_survives_pool_handle_and_rejects_invalid_requests() {
        assert!(CodePool::new(0, 4096).is_err());
        assert!(CodePool::new(4096, 3).is_err());
        assert!(CodePool::new(4097, 4096).is_err());
        let pool = CodePool::new(8192, 4096).unwrap();
        assert!(pool.lease(0).is_none());
        assert!(pool.lease(1).is_none());
        assert!(pool.lease(usize::MAX).is_none());
        let lease = pool.lease(4096).unwrap();
        let other = pool.clone();
        drop(pool);
        std::thread::spawn(move || drop(lease)).join().unwrap();
        assert_eq!(other.lease(8192).unwrap().range(), 0..8192);
    }
}
