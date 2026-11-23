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
    // One free range that spans the whole pool.
    #[allow(clippy::single_range_in_vec_init)]
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

    /// Extends `lease` in place into any immediately following free range by at
    /// least `min_extra` and at most `want_extra` bytes, rounded down to the
    /// pool alignment. Returns the granted byte count, or 0 without modifying
    /// `lease` if the adjacent space is smaller than `min_extra`.
    pub fn extend(&self, lease: &mut CodeLease, want_extra: usize, min_extra: usize) -> usize {
        let want = want_extra & !(self.alignment - 1);
        let min = min_extra.next_multiple_of(self.alignment);
        if want == 0 || want < min {
            return 0;
        }
        let mut free = self.free.lock().unwrap();
        let end = lease.range.end;
        let Some(index) = free.iter().position(|hole| hole.start == end) else {
            return 0;
        };
        let grant = free[index].len().min(want) & !(self.alignment - 1);
        if grant < min {
            return 0;
        }
        free[index].start += grant;
        if free[index].is_empty() {
            free.remove(index);
        }
        lease.range.end += grant;
        grant
    }

    /// Atomically returns `lease` and acquires a strictly larger contiguous
    /// range (up to `want_len`, aligned down) from the largest resulting free
    /// hole. Leaves `lease` untouched and returns `false` if no strictly larger
    /// range is available.
    pub fn relocate(&self, lease: &mut CodeLease, want_len: usize) -> bool {
        let old = lease.range.clone();
        let want = want_len & !(self.alignment - 1);
        if want <= old.len() {
            return false;
        }
        let mut free = self.free.lock().unwrap();
        // Temporarily return and coalesce `old` under the pool lock so no other
        // caller can claim our space before we decide whether to move.
        let mut merged = old.clone();
        let insert_at = free.partition_point(|hole| hole.end < merged.start);
        let mut saved = Vec::new();
        while insert_at < free.len() && free[insert_at].start <= merged.end {
            let hole = free.remove(insert_at);
            merged.start = merged.start.min(hole.start);
            merged.end = merged.end.max(hole.end);
            saved.push(hole);
        }
        free.insert(insert_at, merged);

        let best = free
            .iter()
            .enumerate()
            .max_by_key(|(_, hole)| hole.len())
            .filter(|(_, hole)| (hole.len() & !(self.alignment - 1)) > old.len())
            .map(|(index, _)| index);

        let Some(best_index) = best else {
            // Restore original free list so `lease` stays valid in place.
            free.remove(insert_at);
            for (offset, hole) in saved.into_iter().enumerate() {
                free.insert(insert_at + offset, hole);
            }
            return false;
        };

        let grant = free[best_index].len().min(want) & !(self.alignment - 1);
        let start = free[best_index].start;
        free[best_index].start += grant;
        if free[best_index].is_empty() {
            free.remove(best_index);
        }
        lease.range = start..start + grant;
        true
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

    pub fn extend(&mut self, want_extra: usize, min_extra: usize) -> usize {
        let pool = self.pool.clone();
        pool.extend(self, want_extra, min_extra)
    }

    pub fn relocate(&mut self, want_len: usize) -> bool {
        let pool = self.pool.clone();
        pool.relocate(self, want_len)
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

    // spec: A64-CTL-1, A64-CTL-13
    #[test]
    fn leases_extend_in_place_into_adjacent_free_space() {
        let pool = CodePool::new(64 * 4096, 4096).unwrap();
        let mut first = pool.lease(8 * 4096).unwrap();
        assert_eq!(first.extend(8 * 4096, 4096), 8 * 4096);
        assert_eq!(first.range(), 0..16 * 4096);

        let blocker = pool.lease(4 * 4096).unwrap();
        assert_eq!(blocker.range(), 16 * 4096..20 * 4096);
        assert_eq!(first.extend(8 * 4096, 4096), 0);
        assert_eq!(first.range(), 0..16 * 4096);

        let tail = pool.lease(40 * 4096).unwrap();
        assert_eq!(tail.range(), 20 * 4096..60 * 4096);
        drop(blocker);
        assert_eq!(first.extend(8 * 4096, 4096), 4 * 4096);
        assert_eq!(first.range(), 0..20 * 4096);

        drop(first);
        drop(tail);
        assert_eq!(pool.lease(64 * 4096).unwrap().range(), 0..64 * 4096);
    }

    // spec: A64-CTL-1, A64-CTL-13
    #[test]
    fn relocate_moves_blocked_lease_atomically_and_rolls_back_when_no_larger_hole_exists() {
        let pool = CodePool::new(64 * 4096, 4096).unwrap();
        let left = pool.lease(8 * 4096).unwrap();
        let mut pinned = pool.lease(8 * 4096).unwrap();
        let right = pool.lease(8 * 4096).unwrap();
        assert_eq!(pinned.range(), 8 * 4096..16 * 4096);
        assert_eq!(pinned.extend(8 * 4096, 4096), 0);
        assert!(pinned.relocate(16 * 4096));
        assert_eq!(pinned.range(), 24 * 4096..40 * 4096);
        let mut reused = pool.lease(8 * 4096).unwrap();
        assert_eq!(reused.range(), 8 * 4096..16 * 4096);

        let mut fill = pool.lease(24 * 4096).unwrap();
        assert_eq!(fill.range(), 40 * 4096..64 * 4096);
        assert!(!fill.relocate(32 * 4096));
        assert_eq!(fill.range(), 40 * 4096..64 * 4096);

        // Freeing both neighbors coalesces `reused` with left + right into 0..24.
        drop(left);
        drop(right);
        assert!(reused.relocate(24 * 4096));
        assert_eq!(reused.range(), 0..24 * 4096);

        drop(reused);
        drop(pinned);
        drop(fill);
        assert_eq!(pool.lease(64 * 4096).unwrap().range(), 0..64 * 4096);
    }
}
