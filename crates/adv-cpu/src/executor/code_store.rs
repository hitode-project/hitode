//! Process-owned translated code and lookup metadata. No guest register state lives here.
//! Publication stays on the controller; retirement drains native leases first.
use crate::{Region, compile::Compiled};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

pub(super) struct CodeStore {
    pub(super) compiled: BTreeMap<u32, Compiled>,
    pub(super) regions: BTreeMap<u32, Region>,
    pub(super) page_regions: BTreeMap<u32, BTreeSet<u32>>,
    pub(super) region_pages: BTreeMap<u32, BTreeSet<u32>>,
    pub(super) entry_owner: BTreeMap<u32, u32>,
    /// Published targets mirror entry_owner, including overlapping owners.
    pub(super) link_targets: BTreeMap<u32, usize>,
    /// Targets changed since `links` last mirrored link_targets. None rebuilds.
    link_updates: Option<BTreeSet<u32>>,
    /// Only resident Fast code can acquire promotion heat.
    pub(super) fast_regions: BTreeSet<u32>,
    pub(super) links: Arc<crate::linking::Table>,
    pub(super) links_revision: u64,
    pub(super) cache_revision: u64,
    // Queuing a region changes the range index before native publication changes
    // cache_revision. Stop validation therefore needs its own generation.
    regions_revision: u64,
    stop_validation: Option<(u32, u64, bool)>,
    #[cfg(test)]
    stop_scans: usize,
}

impl Default for CodeStore {
    fn default() -> Self {
        Self {
            compiled: BTreeMap::new(),
            regions: BTreeMap::new(),
            page_regions: BTreeMap::new(),
            region_pages: BTreeMap::new(),
            entry_owner: BTreeMap::new(),
            link_targets: BTreeMap::new(),
            link_updates: Some(BTreeSet::new()),
            fast_regions: BTreeSet::new(),
            links: Arc::default(),
            links_revision: 0,
            cache_revision: 1,
            regions_revision: 1,
            stop_validation: None,
            #[cfg(test)]
            stop_scans: 0,
        }
    }
}

impl CodeStore {
    fn ranges_changed(&mut self) {
        self.regions_revision = self.regions_revision.wrapping_add(1);
        // Explicit invalidation also prevents an old cache hit after wraparound.
        self.stop_validation = None;
    }

    pub(super) fn insert_region(&mut self, region: Region) {
        self.regions.insert(region.start, region);
        self.ranges_changed();
    }

    pub(super) fn remove_region(&mut self, start: u32) -> Option<Region> {
        let region = self.regions.remove(&start);
        if region.is_some() {
            self.ranges_changed();
        }
        region
    }

    pub(super) fn clear_regions(&mut self) {
        self.regions.clear();
        self.ranges_changed();
    }

    pub(super) fn stop_is_outside(&mut self, stop: u32) -> bool {
        if let Some((cached_stop, revision, outside)) = self.stop_validation
            && cached_stop == stop
            && revision == self.regions_revision
        {
            return outside;
        }
        #[cfg(test)]
        {
            self.stop_scans += 1;
        }
        // The closest preceding region may be shorter than an earlier,
        // overlapping region. Preserve the full range predicate on a miss.
        let outside = !self.regions.values().any(|region| {
            u64::from(stop) >= u64::from(region.start)
                && u64::from(stop) < u64::from(region.start) + region.len as u64
        });
        self.stop_validation = Some((stop, self.regions_revision, outside));
        outside
    }

    pub(super) fn link(&mut self, pc: u32, target: usize) {
        self.link_targets.insert(pc, target);
        if let Some(updates) = &mut self.link_updates {
            updates.insert(pc);
        }
    }

    /// Callers prepare with `retire_links` before code named by `pc` is dropped.
    pub(super) fn unlink(&mut self, pc: u32) {
        self.link_targets.remove(&pc);
        if let Some(updates) = &mut self.link_updates {
            updates.remove(&pc);
        }
        match Arc::get_mut(&mut self.links) {
            Some(links) => links.remove(pc),
            None => self.drop_links(),
        }
    }

    pub(super) fn clear_links(&mut self) {
        self.link_targets.clear();
        self.links = Arc::default();
        self.link_updates = Some(BTreeSet::new());
    }

    /// Before retirement the table may name only live code: pending updates can
    /// still point an overlapped entry at its previous owner.
    pub(super) fn retire_links(&mut self) {
        if !self.apply_link_updates() {
            self.drop_links();
        }
    }

    pub(super) fn refresh_links(&mut self) {
        if self.links_revision == self.cache_revision {
            return;
        }
        if !self.apply_link_updates() {
            let mut links = crate::linking::Table::default();
            links.rebuild(self.link_targets.iter().map(|(&pc, &target)| (pc, target)));
            // A worker may still hold the previous immutable Arc generation.
            self.links = Arc::new(links);
            self.link_updates = Some(BTreeSet::new());
        }
        self.links_revision = self.cache_revision;
    }

    fn drop_links(&mut self) {
        self.links = Arc::default();
        self.link_updates = None;
    }

    /// Updates an unshared table in place. False leaves a rebuild to the caller.
    fn apply_link_updates(&mut self) -> bool {
        let Some(updates) = &mut self.link_updates else {
            return false;
        };
        if updates.is_empty() {
            return true;
        }
        let Some(links) = Arc::get_mut(&mut self.links) else {
            return false;
        };
        let targets = &self.link_targets;
        let applied = updates.iter().all(|&pc| match targets.get(&pc) {
            Some(&target) => links.insert(pc, target),
            None => {
                links.remove(pc);
                true
            }
        });
        updates.clear();
        if !applied {
            self.link_updates = None;
        }
        applied
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_cache_tracks_queued_replaced_removed_and_cleared_overlapping_ranges() {
        let mut code = CodeStore::default();
        assert!(code.stop_is_outside(0x2000));
        assert!(code.stop_is_outside(0x2000));
        assert_eq!(code.stop_scans, 1);
        let native_revision = code.cache_revision;
        code.insert_region(Region::new(0x1000, 0x2000));
        code.insert_region(Region::new(0x1800, 0x100));
        assert_eq!(code.cache_revision, native_revision);
        assert!(!code.stop_is_outside(0x2000));
        assert!(!code.stop_is_outside(0x2000));
        assert_eq!(code.stop_scans, 2);
        code.insert_region(Region::new(0x1000, 0x100));
        assert!(code.stop_is_outside(0x2000));
        code.insert_region(Region::new(0x1000, 0x2000));
        assert!(!code.stop_is_outside(0x2000));
        code.remove_region(0x1000);
        assert!(code.stop_is_outside(0x2000));
        code.insert_region(Region::new(0x2000, 1));
        assert!(!code.stop_is_outside(0x2000));
        code.clear_regions();
        assert!(code.stop_is_outside(0x2000));
        assert_eq!(code.stop_scans, 7);
    }

    #[test]
    fn stop_ranges_keep_half_open_bounds_at_the_end_of_guest_space() {
        let mut code = CodeStore::default();
        code.insert_region(Region::new(u32::MAX - 15, 16));
        assert!(!code.stop_is_outside(u32::MAX));
        assert!(code.stop_is_outside(u32::MAX - 16));
        code.insert_region(Region::new(u32::MAX - 15, 15));
        assert!(code.stop_is_outside(u32::MAX));
        assert!(!code.stop_is_outside(u32::MAX - 1));
    }
}
