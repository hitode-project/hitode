//! Independent page subscriptions for serial decoded/native code owners.
//! The dirty-page queue belongs to Tier 2; page stamps survive its consumption.
use crate::{Memory, PAGE_SIZE};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) fn enabled() -> bool {
    std::env::var("ADV_CODE_VALIDITY").as_deref() == Ok("event")
}

#[derive(Default)]
struct Page<K> {
    version: Option<u64>,
    owners: BTreeSet<K>,
}

pub(crate) struct CodeValidity<K> {
    identity: u64,
    epoch: u64,
    pages: BTreeMap<u32, Page<K>>,
    owners: BTreeMap<K, Vec<u32>>,
}

impl<K: Copy + Ord> Default for CodeValidity<K> {
    fn default() -> Self {
        Self {
            identity: 0,
            epoch: 0,
            pages: BTreeMap::new(),
            owners: BTreeMap::new(),
        }
    }
}

impl<K: Copy + Ord> CodeValidity<K> {
    fn version(memory: &Memory, page: u32) -> Option<u64> {
        page.checked_sub(memory.base)
            .and_then(|offset| memory.versions.get(offset as usize / PAGE_SIZE).copied())
    }

    pub fn clear(&mut self) {
        self.pages.clear();
        self.owners.clear();
    }

    pub fn remove(&mut self, owner: K) {
        if let Some(pages) = self.owners.remove(&owner) {
            for page in pages {
                let entry = self.pages.get_mut(&page).unwrap();
                entry.owners.remove(&owner);
                if entry.owners.is_empty() {
                    self.pages.remove(&page);
                }
            }
        }
    }

    /// Call after observation/decoding. Existing subscriptions keep their stamps
    /// until refresh so registration cannot hide a pending invalidation.
    pub fn insert(&mut self, owner: K, pages: impl IntoIterator<Item = u32>, memory: &Memory) {
        self.remove(owner);
        let pages: BTreeSet<_> = pages.into_iter().collect();
        for &page in &pages {
            self.pages
                .entry(page)
                .or_insert_with(|| Page {
                    version: Self::version(memory, page),
                    owners: BTreeSet::new(),
                })
                .owners
                .insert(owner);
        }
        self.owners.insert(owner, pages.into_iter().collect());
    }

    /// No guest reads or page walks on a hit. A Memory identity change watches
    /// all dependencies before revalidation, including inaccessible fetch tails.
    pub fn refresh(&mut self, memory: &mut Memory) -> BTreeSet<K> {
        if self.identity == memory.code_identity && self.epoch == memory.epoch {
            return BTreeSet::new();
        }
        let changed_owner = self.identity != memory.code_identity;
        let mut affected = BTreeSet::new();
        for (&address, page) in &mut self.pages {
            if changed_owner {
                memory.watch_code(address, PAGE_SIZE);
            }
            let version = Self::version(memory, address);
            if changed_owner || version != page.version {
                affected.extend(page.owners.iter().copied());
                page.version = version;
            }
        }
        self.identity = memory.code_identity;
        self.epoch = memory.epoch;
        affected
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subscriptions_survive_consumed_dirty_pages_and_ignore_other_pages() {
        let mut memory = Memory::new(0x1000, 3 * PAGE_SIZE).unwrap();
        let mut cache = CodeValidity::default();
        cache.insert(1, [0x1000, 0x2000], &memory);
        cache.insert(2, [0x2000], &memory);
        assert_eq!(cache.refresh(&mut memory), BTreeSet::from([1, 2]));
        memory.touch(0x3000, 1);
        assert!(cache.refresh(&mut memory).is_empty());
        memory.touch(0x1000, 1);
        memory.dirty_pages.clear();
        assert_eq!(cache.refresh(&mut memory), BTreeSet::from([1]));
        assert!(cache.refresh(&mut memory).is_empty());
        cache.remove(1);
        memory.touch(0x1000, 1);
        assert!(cache.refresh(&mut memory).is_empty());
        memory.touch(0x2000, 1);
        assert_eq!(cache.refresh(&mut memory), BTreeSet::from([2]));
        assert_eq!(cache.pages.len(), 1);
    }

    #[test]
    fn memory_moves_keep_identity_and_new_allocations_do_not() {
        let first = Memory::new(0x1000, PAGE_SIZE).unwrap();
        let id = first.code_identity;
        let mut moved = first;
        let mut cache = CodeValidity::default();
        cache.insert(1, [0x1000], &moved);
        cache.refresh(&mut moved);
        assert_eq!(moved.code_identity, id);
        assert!(cache.refresh(&mut moved).is_empty());
        let mut second = Memory::new(0x1000, PAGE_SIZE).unwrap();
        assert_ne!(second.code_identity, id);
        second.epoch = moved.epoch;
        assert_eq!(cache.refresh(&mut second), BTreeSet::from([1]));
    }
}
