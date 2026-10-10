//! Versioned native entry slots. Only the owning execution thread publishes or
//! retires code, between native calls. Generated tail calls never own code pages.

pub(crate) fn hash(pc: u32) -> u32 {
    let mixed = (pc ^ (pc >> 16)).wrapping_mul(0x9e37_79b1);
    mixed ^ (mixed >> 16)
}

/// A key no zero-extended guest PC can equal.
pub(crate) const VACANT: u64 = u64::MAX;

#[repr(C)]
pub(crate) struct Slot {
    pub key: u64,
    pub entry: usize,
}

const EMPTY_SLOT: Slot = Slot {
    key: VACANT,
    entry: 0,
};

/// One vacant slot: generated probes need no null or empty-table check.
static EMPTY: [Slot; 1] = [EMPTY_SLOT];

/// Probe operands for a chain that links nothing.
pub(crate) fn unlinked() -> (*const Slot, u32) {
    (EMPTY.as_ptr(), 0)
}

/// Linear probing at most half full: every probe run ends at a vacant slot.
#[derive(Default)]
pub(crate) struct Table {
    slots: Vec<Slot>,
    len: usize,
}

impl Table {
    pub fn rebuild(&mut self, entries: impl Iterator<Item = (u32, usize)>) {
        let entries: Vec<_> = entries.collect();
        self.slots.clear();
        self.len = 0;
        if entries.is_empty() {
            return;
        }
        let capacity = (entries.len() * 4).next_power_of_two().max(256);
        self.slots.resize_with(capacity, || EMPTY_SLOT);
        for (pc, entry) in entries {
            assert!(self.insert(pc, entry));
        }
    }

    /// The slot holding `pc`, or the vacant slot ending its probe run.
    fn find(&self, pc: u32) -> usize {
        let mask = self.slots.len() - 1;
        let mut index = hash(pc) as usize & mask;
        while self.slots[index].key != VACANT && self.slots[index].key != u64::from(pc) {
            index = (index + 1) & mask;
        }
        index
    }

    /// Updates one key in an exclusively owned table. False asks the caller to
    /// rebuild: the table is empty or would pass half load.
    pub fn insert(&mut self, pc: u32, entry: usize) -> bool {
        debug_assert_ne!(entry, 0);
        if self.slots.is_empty() {
            return false;
        }
        let index = self.find(pc);
        if self.slots[index].key == VACANT {
            if (self.len + 1) * 2 > self.slots.len() {
                return false;
            }
            self.len += 1;
        }
        self.slots[index] = Slot {
            key: u64::from(pc),
            entry,
        };
        true
    }

    /// Removes one key by shifting later members of its probe run backward.
    /// Lookups stop at the first vacant slot, so no tombstone may remain.
    pub fn remove(&mut self, pc: u32) {
        if !self.contains(pc) {
            return;
        }
        let mask = self.slots.len() - 1;
        let mut hole = self.find(pc);
        self.len -= 1;
        let mut next = hole;
        loop {
            next = (next + 1) & mask;
            let key = self.slots[next].key;
            if key == VACANT {
                break;
            }
            let home = hash(key as u32) as usize & mask;
            // Move a key only when the hole lies on its own probe path.
            if next.wrapping_sub(home) & mask >= next.wrapping_sub(hole) & mask {
                self.slots.swap(hole, next);
                hole = next;
            }
        }
        self.slots[hole] = EMPTY_SLOT;
    }

    /// The caller keeps this table borrowed throughout native execution.
    /// Parallel workers borrow an immutable image; all updates require &mut.
    pub fn parts(&self) -> (*const Slot, u32) {
        if self.slots.is_empty() {
            unlinked()
        } else {
            (self.slots.as_ptr(), (self.slots.len() - 1) as u32)
        }
    }

    /// Whether the generated probe would find `pc`.
    pub fn contains(&self, pc: u32) -> bool {
        !self.slots.is_empty() && self.slots[self.find(pc)].key != VACANT
    }
}

pub(crate) use crate::compile::contract::NativeContext;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn polling_word_pointer_has_a_fixed_native_offset() {
        assert_eq!(std::mem::size_of::<std::sync::atomic::AtomicBool>(), 1);
        assert_eq!(std::mem::offset_of!(NativeContext, interrupt), 40);
    }

    /// The probe generated code performs.
    fn lookup(table: &Table, pc: u32) -> Option<usize> {
        let (slots, mask) = table.parts();
        // SAFETY: parts belongs to this borrowed table.
        let slots = unsafe { std::slice::from_raw_parts(slots, mask as usize + 1) };
        let mut index = hash(pc) & mask;
        loop {
            let slot = &slots[index as usize];
            if slot.key == u64::from(pc) {
                return Some(slot.entry);
            }
            if slot.key == VACANT {
                return None;
            }
            index = (index + 1) & mask;
        }
    }

    #[test]
    fn in_place_updates_match_a_rebuilt_table() {
        let mut expected = BTreeMap::new();
        let mut table = Table::default();
        let mut seed = 0x2545_f491_u32;
        let mut rebuilds = 0;
        for step in 0..20_000usize {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            // Few distinct, clustered keys keep probe runs long and collisions frequent.
            let pc = 0x40_0000 + (seed % 1500) * 4;
            if seed & 0x3000 == 0 {
                expected.remove(&pc);
                table.remove(pc);
            } else {
                expected.insert(pc, step + 1);
                if !table.insert(pc, step + 1) {
                    rebuilds += 1;
                    table.rebuild(expected.iter().map(|(&pc, &entry)| (pc, entry)));
                }
            }
            if step % 997 == 0 {
                for key in (0..1500).map(|n| 0x40_0000 + n * 4) {
                    assert_eq!(lookup(&table, key), expected.get(&key).copied(), "{key:#x}");
                }
            }
        }
        assert_eq!(table.len, expected.len());
        assert!(rebuilds < 20, "{rebuilds}");
    }
}
