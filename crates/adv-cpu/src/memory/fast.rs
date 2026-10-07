//! Exact permissions and two-page grants in one immutable native page table.
//!
//! A helper may promote a file page while native code is still running. Keep
//! that invocation's table immutable: stale grants take the precise slow path,
//! and the next invocation publishes the newly ordinary page. Revocations go
//! through the existing epoch/trap exit and worker quiescence protocol.
use super::{EXEC, READ, SHARED, WRITE};
use std::ops::Range;
use std::sync::Arc;

pub(crate) struct FastPermissions {
    bytes: Arc<Vec<u8>>,
    dirty: Option<Range<usize>>,
}

impl FastPermissions {
    pub(super) fn new(pages: usize) -> Self {
        Self::from_snapshot(Arc::new(vec![0; pages]))
    }

    pub(super) fn from_snapshot(bytes: Arc<Vec<u8>>) -> Self {
        Self { bytes, dirty: None }
    }

    pub(super) fn invalidate(&mut self, pages: Range<usize>) {
        if pages.is_empty() {
            return;
        }
        let affected = pages.start.saturating_sub(1)..pages.end;
        self.dirty = Some(match self.dirty.take() {
            Some(old) => old.start.min(affected.start)..old.end.max(affected.end),
            None => affected,
        });
    }

    /// The returned allocation must remain pinned for the entire native chain,
    /// including row workers. No mapping/helper update mutates these bytes.
    pub(crate) fn snapshot(&mut self, permissions: &[u8]) -> Arc<Vec<u8>> {
        debug_assert_eq!(permissions.len(), self.bytes.len());
        if let Some(pages) = self.dirty.take() {
            let bytes = Arc::make_mut(&mut self.bytes);
            for page in pages {
                let current = permissions[page];
                // Exact checks use the same base as the derived grants.
                bytes[page] = current
                    | (permissions.get(page + 1).map_or(0, |&next| {
                        let both = permissions[page] & next;
                        let either = permissions[page] | next;
                        if either & SHARED != 0 {
                            0
                        } else {
                            (both & READ) | if either & EXEC == 0 { both & WRITE } else { 0 }
                        }
                    }) << 5);
            }
        }
        self.bytes.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::AUDIO;

    #[test]
    fn grants_are_sufficient_for_every_pair_of_page_kinds() {
        for first in 0..32 {
            for second in 0..32 {
                let mut table = FastPermissions::new(2);
                table.invalidate(0..2);
                let snapshot = table.snapshot(&[first, second]);
                let readable = [first, second].iter().all(|p| p & (READ | SHARED) == READ);
                let writable = [first, second]
                    .iter()
                    .all(|p| p & (WRITE | SHARED | EXEC) == WRITE);
                assert_eq!(snapshot[0] & (READ << 5) != 0, readable);
                assert_eq!(snapshot[0] & (WRITE << 5) != 0, writable);
                assert_eq!(
                    snapshot[1] & ((READ | WRITE) << 5),
                    0,
                    "the end of the window never wraps"
                );
                assert_eq!(snapshot[0] & 31, first);
                assert_eq!(snapshot[1] & 31, second);
            }
        }
    }

    #[test]
    fn revocation_updates_predecessor_and_keeps_old_snapshot_immutable() {
        let mut permissions = [READ | WRITE; 4];
        let mut table = FastPermissions::new(4);
        table.invalidate(0..4);
        let old = table.snapshot(&permissions);
        permissions[2] = READ | SHARED | AUDIO;
        table.invalidate(2..3);
        assert_eq!(&**old, &[99, 99, 99, 3]);
        let new = table.snapshot(&permissions);
        assert_eq!(&**new, &[99, 3, READ | SHARED | AUDIO, 3]);
        permissions[2] = READ;
        table.invalidate(2..3);
        let promoted = table.snapshot(&permissions);
        assert_eq!(&**promoted, &[99, 35, 33, 3]);
        assert_eq!(&**new, &[99, 3, READ | SHARED | AUDIO, 3]);
    }

    #[test]
    fn file_promotion_keeps_the_running_snapshot_conservative_until_reentry() {
        use crate::{Memory, backend::FilePage};
        let mut memory = Memory::reserve(0x10000, 3 * 4096).unwrap();
        memory.map(0x10000, 4096, READ | WRITE).unwrap();
        memory.map(0x12000, 4096, READ | WRITE).unwrap();
        let page = FilePage::new([0x71; 4096]);
        memory
            .map_file_pages(0x11000, &[page], READ | WRITE, false)
            .unwrap();
        let running = memory.fast.snapshot(&memory.perms);
        assert_eq!(running[0], READ | WRITE);
        assert_eq!(running[1], READ | WRITE | SHARED);
        memory.prepare_native_access(0x11000, 4, false).unwrap();
        assert_eq!(memory.read(0x11000, 4).unwrap().as_ref(), &[0x71; 4]);
        assert_eq!(running[1], READ | WRITE | SHARED);
        let next = memory.fast.snapshot(&memory.perms);
        assert_eq!(next[0] & super::super::FAST_READ, super::super::FAST_READ);
        assert_eq!(next[1] & SHARED, 0);
        assert_eq!(running[0], READ | WRITE);
    }
}
