//! Access permissions and two-page grants in one immutable native page table.
//! EXEC is a store barrier here: serial unchecked-host memory may clear it for
//! pages never observed as code. The guest permission table is unchanged.
//!
//! A helper may promote a file page while native code is still running. Keep
//! that invocation's table immutable: stale grants take the precise slow path,
//! and the next invocation publishes the newly ordinary page. Revocations go
//! through the existing epoch/trap exit and worker quiescence protocol.
//! The serial software-checked owner exposes linear COPY reads while retaining
//! EXEC as a first-store barrier. This never grants guest execute permission.
use super::{EXEC, READ, SHARED, WRITE};
use std::ops::Range;
use std::sync::Arc;

/// Dirty ranges kept apart before they collapse into one union.
const MAX_DIRTY_RANGES: usize = 64;

pub struct FastPermissions {
    bytes: Arc<Vec<u8>>,
    /// Disjoint, sorted page ranges to recompute; distant changes stay apart.
    dirty: Vec<Range<usize>>,
    selective: bool,
    /// Counts the snapshots whose bytes differ from the one before.
    version: u64,
}

impl FastPermissions {
    pub(super) fn new(pages: usize) -> Self {
        Self::from_snapshot(Arc::new(vec![0; pages]))
    }

    pub(super) fn from_snapshot(bytes: Arc<Vec<u8>>) -> Self {
        Self {
            bytes,
            dirty: Vec::new(),
            selective: false,
            version: 0,
        }
    }

    /// Changes whenever a snapshot's bytes may differ from the last one's,
    /// including when they were updated in place because no other holder
    /// remained.
    pub fn version(&self) -> u64 {
        self.version
    }

    pub(super) fn invalidate(&mut self, pages: Range<usize>) {
        if pages.is_empty() {
            return;
        }
        let mut affected = pages.start.saturating_sub(1)..pages.end;
        // Absorb every range this one touches, keeping the rest in order.
        let first = self
            .dirty
            .partition_point(|range| range.end < affected.start);
        let last = self
            .dirty
            .partition_point(|range| range.start <= affected.end);
        for range in self.dirty.drain(first..last) {
            affected = range.start.min(affected.start)..range.end.max(affected.end);
        }
        self.dirty.insert(first, affected);
        // Bound the bookkeeping: past this many, one union is cheaper to keep.
        if self.dirty.len() > MAX_DIRTY_RANGES {
            let union = self.dirty[0].start..self.dirty[self.dirty.len() - 1].end;
            self.dirty = vec![union];
        }
    }

    /// The returned allocation must remain pinned for the entire native chain,
    /// including row workers. No mapping/helper update mutates these bytes.
    ///
    /// `window`, where host pages hold `span` guest pages, gives the READ and
    /// WRITE bits the host page of a guest page takes in the guest window. A
    /// page the guest may read but its host page may not is SHARED, and one
    /// the guest may write but its host page may not has the EXEC store
    /// barrier, so native code takes them through `Memory` instead of
    /// faulting on every access.
    pub fn snapshot(
        &mut self,
        permissions: &[u8],
        watched: Option<&[bool]>,
        private_copies: &[Option<bool>],
        window: Option<(usize, &dyn Fn(usize) -> u8)>,
    ) -> Arc<Vec<u8>> {
        debug_assert_eq!(permissions.len(), self.bytes.len());
        debug_assert!(watched.is_none_or(|pages| pages.len() == permissions.len()));
        if self.selective != watched.is_some() {
            self.selective = watched.is_some();
            self.invalidate(0..permissions.len());
        }
        // Guest pages of one host page share its protection: ask once each.
        let mut host = None;
        let mut permission = |page: usize| {
            let mut permissions = permissions[page];
            if watched.is_some_and(|pages| !pages[page]) {
                permissions &= !EXEC;
            }
            // Owned COPY snapshots already have initialized linear bytes and
            // no external alias. Reads can use ordinary native loads. EXEC is
            // a store barrier in this derived table, so the first store still
            // falls back to the owner to retire COPY before committing bytes.
            // Keep guest READ/WRITE bits intact: missing bits signal faults,
            // whereas the store barrier requests a restartable fallback.
            // Hardware fault classification and foreign owners retain their
            // original shared-page contract, just like conservative code writes.
            if watched.is_some()
                && permissions & SHARED != 0
                && private_copies.get(page) == Some(&Some(true))
            {
                permissions = (permissions & !SHARED) | EXEC;
            }
            if let Some((span, allows)) = window {
                let host = match host {
                    Some((index, allows)) if index == page / span => allows,
                    _ => host.insert((page / span, allows(page))).1,
                };
                if permissions & READ != 0 && host & READ == 0 {
                    permissions |= SHARED;
                }
                if permissions & WRITE != 0 && host & WRITE == 0 {
                    permissions |= EXEC;
                }
            }
            permissions
        };
        if !self.dirty.is_empty() {
            self.version += 1;
            let bytes = Arc::make_mut(&mut self.bytes);
            for mut pages in std::mem::take(&mut self.dirty) {
                // A change to one guest page may change its whole host page.
                if let Some((span, _)) = window {
                    pages = (pages.start / span * span).saturating_sub(1)
                        ..pages.end.div_ceil(span) * span;
                    pages.end = pages.end.min(permissions.len());
                }
                let mut next = None;
                for page in pages {
                    let current = next.take().unwrap_or_else(|| permission(page));
                    // Exact checks use the same base as the derived grants.
                    bytes[page] = current
                        | (permissions.get(page + 1).map_or(0, |_| {
                            let next = *next.insert(permission(page + 1));
                            let both = current & next;
                            let either = current | next;
                            if either & SHARED != 0 {
                                0
                            } else {
                                (both & READ) | if either & EXEC == 0 { both & WRITE } else { 0 }
                            }
                        }) << 5);
                }
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
    fn distant_changes_recompute_only_their_own_pages() {
        let mut permissions = vec![READ; 4096];
        let mut table = FastPermissions::new(permissions.len());
        table.invalidate(0..permissions.len());
        table.snapshot(&permissions, None, &[], None);
        // A stale middle page shows whether the two changes became one range.
        for page in [8, 2048, 4000] {
            permissions[page] = READ | WRITE;
        }
        table.invalidate(8..9);
        table.invalidate(4000..4001);
        let snapshot = table.snapshot(&permissions, None, &[], None);
        assert_eq!(snapshot[8] & 7, READ | WRITE);
        assert_eq!(snapshot[4000] & 7, READ | WRITE);
        assert_eq!(snapshot[2048] & 7, READ);
        // The page before each change refreshes its two-page grant.
        assert_eq!(snapshot[7] >> 5, READ);
        assert_eq!(snapshot[3999] >> 5, READ);
    }

    #[test]
    fn owned_copy_grants_preserve_access_bits_and_never_grant_stores() {
        for protection in 0..8 {
            for watched in [None, Some([false, false].as_slice())] {
                let permissions = [protection | SHARED, READ | WRITE];
                let copies = [Some(true), None];
                let mut table = FastPermissions::new(2);
                table.invalidate(0..2);
                let snapshot = table.snapshot(&permissions, watched, &copies, None);
                assert_eq!(snapshot[0] & (READ | WRITE), protection & (READ | WRITE));
                if watched.is_none() {
                    assert_eq!(snapshot[0], permissions[0]);
                    continue;
                }
                assert_eq!(snapshot[0] & SHARED, 0);
                assert_ne!(snapshot[0] & EXEC, 0, "COPY retains its store barrier");
                assert_eq!(snapshot[0] & (READ << 5), (protection & READ) << 5);
                assert_eq!(snapshot[0] & (WRITE << 5), 0);
                assert_eq!(permissions[0], protection | SHARED);
            }
        }
    }

    #[test]
    fn owned_copy_reads_revoke_when_the_owner_leaves_software_checked_mode() {
        use crate::Memory;
        let mut memory = Memory::reserve(0x10000, 4096).unwrap();
        memory
            .map_private_copy_pages(0x10000, &[0x37; 4096], READ | WRITE)
            .unwrap();
        let serial = memory.native_permissions();
        assert_eq!(serial[0], READ | WRITE | EXEC);
        #[cfg(windows)]
        {
            memory.protect(0x10000, 4096, 0).unwrap();
            memory.enable_native_protection().unwrap();
            assert_eq!(memory.native_permissions()[0], SHARED);
            assert!(memory.prepare_native_access(0x10000, 1, false).is_err());
        }
    }

    #[test]
    fn owned_copy_retirement_and_protection_publish_immutable_snapshots() {
        use crate::{Memory, memory::FAST_READ};
        let mut memory = Memory::reserve(0x10000, 2 * 4096).unwrap();
        memory
            .map_private_copy_pages(0x10000, &[0x37; 8192], READ | WRITE)
            .unwrap();
        let old = memory.native_permissions();
        assert_eq!(
            &**old,
            &[READ | WRITE | EXEC | FAST_READ, READ | WRITE | EXEC]
        );
        assert_eq!(&**memory.perms, &[READ | WRITE | SHARED; 2]);
        memory.prepare_native_access(0x10ffe, 4, false).unwrap();
        assert_eq!(memory.file_page_copy_on_write(0x10000), Some(true));
        memory.write(0x10000, &[0x19]).unwrap();
        let retired = memory.native_permissions();
        assert_eq!(retired[0], READ | WRITE | FAST_READ);
        assert_eq!(retired[1], READ | WRITE | EXEC);
        assert_ne!(old[0] & EXEC, 0);
        memory.protect(0x11000, 4096, 0).unwrap();
        let protected = memory.native_permissions();
        assert_eq!(&**protected, &[READ | WRITE, EXEC]);
        assert_eq!(retired[0] & FAST_READ, FAST_READ);
        assert!(memory.prepare_native_access(0x10ffe, 4, false).is_err());
        assert_eq!(memory.file_page_copy_on_write(0x11000), Some(true));
        memory.unmap(0x11000, 4096).unwrap();
        assert_eq!(memory.native_permissions()[1], 0);
        assert_eq!(protected[1], EXEC);
    }

    #[test]
    fn rwx_data_grants_revoke_monotonically_without_changing_guest_permissions() {
        use crate::Memory;
        let mut memory = Memory::new(0x10000, 3 * 4096).unwrap();
        memory
            .protect(0x10000, 3 * 4096, READ | WRITE | EXEC)
            .unwrap();
        let old = memory.native_permissions();
        assert_eq!(&**old, &[99, 99, 3]);
        let versions = memory.versions.clone();
        let epoch = memory.epoch;
        memory.watch_code(0x11010, 1);
        assert_eq!(memory.epoch, epoch + 1);
        assert_eq!(
            memory.versions, versions,
            "observing bytes does not change them"
        );
        assert_eq!(&*memory.perms, &[READ | WRITE | EXEC; 3]);
        let watched = memory.native_permissions();
        assert_eq!(&**watched, &[35, 39, 3]);
        assert_eq!(&**old, &[99, 99, 3], "running snapshots stay immutable");
        memory.watch_code(0x11020, 1);
        assert_eq!(memory.epoch, epoch + 1);
        memory.write(0x12010, &[7]).unwrap();
        assert_eq!(
            memory.epoch,
            epoch + 1,
            "unobserved RWX data does not end a block"
        );
        memory.write(0x11010, &[8]).unwrap();
        assert_eq!(memory.epoch, epoch + 2, "code writes keep the SMC barrier");
    }

    #[test]
    fn grants_are_sufficient_for_every_pair_of_page_kinds() {
        for first in 0..32 {
            for second in 0..32 {
                let mut table = FastPermissions::new(2);
                table.invalidate(0..2);
                let snapshot = table.snapshot(&[first, second], None, &[], None);
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
        let old = table.snapshot(&permissions, None, &[], None);
        permissions[2] = READ | SHARED | AUDIO;
        table.invalidate(2..3);
        assert_eq!(&**old, &[99, 99, 99, 3]);
        let new = table.snapshot(&permissions, None, &[], None);
        assert_eq!(&**new, &[99, 3, READ | SHARED | AUDIO, 3]);
        permissions[2] = READ;
        table.invalidate(2..3);
        let promoted = table.snapshot(&permissions, None, &[], None);
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
        let running = memory.native_permissions();
        assert_eq!(running[0], READ | WRITE);
        assert_eq!(running[1], READ | WRITE | SHARED);
        memory.prepare_native_access(0x11000, 4, false).unwrap();
        assert_eq!(memory.read(0x11000, 4).unwrap().as_ref(), &[0x71; 4]);
        assert_eq!(running[1], READ | WRITE | SHARED);
        let next = memory.native_permissions();
        assert_eq!(next[0] & super::super::FAST_READ, super::super::FAST_READ);
        assert_eq!(next[1] & SHARED, 0);
        assert_eq!(running[0], READ | WRITE);
    }
}
