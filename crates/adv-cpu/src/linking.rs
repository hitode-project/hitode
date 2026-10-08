//! Versioned native entry slots. Only the owning execution thread publishes or
//! retires code, between native calls. Generated tail calls never own code pages.
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::CpuState;

pub(crate) mod shadow;

pub(crate) const PROBES: usize = 8;
pub(crate) const CHAIN_LIMIT: u32 = 1024;

pub(crate) fn hash(pc: u32) -> u32 {
    let mixed = (pc ^ (pc >> 16)).wrapping_mul(0x9e37_79b1);
    mixed ^ (mixed >> 16)
}

#[repr(C)]
pub(crate) struct Slot {
    pub pc: u32,
    pub entry: AtomicUsize,
}

#[derive(Default)]
pub(crate) struct Table {
    slots: Vec<Slot>,
    len: usize,
}

impl Table {
    pub fn rebuild(&mut self, entries: impl Iterator<Item = (u32, usize)>) {
        let entries: Vec<_> = entries.collect();
        self.len = entries.len();
        if entries.is_empty() {
            self.slots.clear();
            return;
        }
        let mut capacity = (entries.len() * 4).next_power_of_two().max(256);
        loop {
            let mut slots: Vec<_> = (0..capacity)
                .map(|_| Slot {
                    pc: 0,
                    entry: AtomicUsize::new(0),
                })
                .collect();
            let mask = capacity - 1;
            let mut complete = true;
            for &(pc, entry) in &entries {
                let first = hash(pc) as usize & mask;
                let index = (0..PROBES)
                    .map(|offset| (first + offset) & mask)
                    .find(|&index| slots[index].entry.load(Ordering::Relaxed) == 0);
                let Some(index) = index else {
                    complete = false;
                    break;
                };
                slots[index].pc = pc;
                slots[index].entry.store(entry, Ordering::Release);
            }
            if complete {
                self.slots = slots;
                return;
            }
            capacity *= 2;
        }
    }

    /// Updates one key in an exclusively owned table. False asks the caller to
    /// rebuild: the table is empty, would pass half load, or the probe window is full.
    pub fn insert(&mut self, pc: u32, entry: usize) -> bool {
        debug_assert_ne!(entry, 0);
        let Some(mask) = self.slots.len().checked_sub(1) else {
            return false;
        };
        let first = hash(pc) as usize & mask;
        for offset in 0..PROBES {
            let slot = &mut self.slots[(first + offset) & mask];
            let current = slot.entry.get_mut();
            if *current == 0 {
                if (self.len + 1) * 2 > mask + 1 {
                    return false;
                }
                self.len += 1;
                slot.pc = pc;
                *current = entry;
                return true;
            }
            if slot.pc == pc {
                *current = entry;
                return true;
            }
        }
        false
    }

    /// Removes one key by shifting later members of its probe run backward.
    /// Lookups stop at the first empty slot, so no tombstone may remain.
    pub fn remove(&mut self, pc: u32) {
        let Some(mask) = self.slots.len().checked_sub(1) else {
            return;
        };
        let first = hash(pc) as usize & mask;
        let Some(mut hole) = (0..PROBES)
            .map(|offset| (first + offset) & mask)
            .take_while(|&index| self.slots[index].entry.load(Ordering::Relaxed) != 0)
            .find(|&index| self.slots[index].pc == pc)
        else {
            return;
        };
        self.len -= 1;
        let mut next = hole;
        loop {
            next = (next + 1) & mask;
            let entry = *self.slots[next].entry.get_mut();
            if entry == 0 {
                break;
            }
            let home = hash(self.slots[next].pc) as usize & mask;
            // Move a key only when the hole lies on its own probe path.
            if next.wrapping_sub(home) & mask >= next.wrapping_sub(hole) & mask {
                self.slots[hole].pc = self.slots[next].pc;
                *self.slots[hole].entry.get_mut() = entry;
                hole = next;
            }
        }
        self.slots[hole].pc = 0;
        *self.slots[hole].entry.get_mut() = 0;
    }

    pub fn parts(&self) -> (*const Slot, u32) {
        if self.slots.is_empty() {
            (std::ptr::null(), 0)
        } else {
            (self.slots.as_ptr(), (self.slots.len() - 1) as u32)
        }
    }
}

/// ABI used only by generated code. The C entry trampoline passes this context
/// and the guest memory base to a Tail-convention region. No Rust stack grows
/// when regions call one another. The table and code allocations outlive a chain.
#[repr(C)]
pub(crate) struct NativeContext {
    pub state: *mut CpuState,
    /// Borrowed row executor; null in guest-parallel and child row contexts.
    pub rows: *const crate::rows::RowPool,
    /// Actual architectural register writes of a dedicated row kernel.
    pub written: u32,
    /// Pinned native page metadata: exact permissions plus two-page grants.
    pub perms: *const u8,
    pub bridge: *mut std::ffi::c_void,
    /// Pinned AtomicBool, read by generated byte-width atomic loads only.
    pub interrupt: *const std::sync::atomic::AtomicBool,
    pub slots: *const Slot,
    pub mask: u32,
    pub remaining: u32,
    pub stop: u32,
    /// Only the initial borrowed owner has a hardware-fault session. Tail
    /// transfers clear this flag before entering another checked region.
    #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
    pub fault_reads: u8,
    pub linked_entries: u64,
    /// Exclusive metadata borrowed for this chain. Cleared before/after native
    /// execution, so it cannot retain a target across publication or retirement.
    pub shadow: *mut shadow::Stack,
    /// Optional direct scalar engine; null for checked/parallel/row execution.
    pub direct: *const std::ffi::c_void,
}

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
        if slots.is_null() {
            return None;
        }
        // SAFETY: parts belongs to this borrowed table.
        let slots = unsafe { std::slice::from_raw_parts(slots, mask as usize + 1) };
        (0..PROBES)
            .map(|offset| &slots[(hash(pc) as usize + offset) & mask as usize])
            .map(|slot| (slot.pc, slot.entry.load(Ordering::Acquire)))
            .take_while(|&(_, entry)| entry != 0)
            .find(|&(key, _)| key == pc)
            .map(|(_, entry)| entry)
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
