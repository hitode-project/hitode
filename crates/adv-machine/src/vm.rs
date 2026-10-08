//! Guest address-space bookkeeping.
//!
//! Every region the host puts in the guest (images, stacks, heaps, host pages) is recorded
//! here, so image loading can tell whether a preferred base is free. Dynamic allocations
//! are first-fit over 64 KiB granules (the Windows allocation granularity) inside
//! [`DYN_START`, `DYN_END`). Mapping is done through the caller's [`Guest`], so this also
//! works from inside trap handlers.

use std::collections::BTreeMap;

use adv_cpu::backend::{Guest, MemFault, Prot};

use crate::layout::{DYN_END, DYN_START, GRANULE, PAGE};

pub struct Vm {
    /// Reserved regions: start -> size.
    used: BTreeMap<u32, u32>,
}

impl Default for Vm {
    fn default() -> Self {
        Self::new()
    }
}

pub const fn align_up(v: u32, a: u32) -> u32 {
    (v + a - 1) & !(a - 1)
}

impl Vm {
    pub fn new() -> Self {
        Self {
            used: BTreeMap::new(),
        }
    }

    fn overlaps(&self, addr: u32, size: u32) -> bool {
        let end = u64::from(addr) + u64::from(size);
        // The region starting at or before `addr`, and any starting inside [addr, end).
        let before = self
            .used
            .range(..=addr)
            .next_back()
            .is_some_and(|(&s, &l)| u64::from(s) + u64::from(l) > u64::from(addr));
        before
            || self
                .used
                .range(addr..)
                .next()
                .is_some_and(|(&s, _)| u64::from(s) < end)
    }

    fn find_free(&self, size: u32) -> Option<u32> {
        let mut cursor = DYN_START;
        for (&start, &len) in self.used.range(..DYN_END) {
            if start >= cursor && start - cursor >= size {
                return Some(cursor);
            }
            // A fixed region may end at 4 GiB or cross DYN_END. There can be
            // no later dynamic allocation; never round that end back to zero.
            let end = start.checked_add(len)?.checked_add(GRANULE - 1)? & !(GRANULE - 1);
            cursor = cursor.max(end);
        }
        (DYN_END.checked_sub(cursor)? >= size).then_some(cursor)
    }

    /// Reserve `[addr, addr + size)` without mapping. Fails if any part is taken.
    pub fn reserve_at(&mut self, addr: u32, size: u32) -> bool {
        if size == 0 || u64::from(addr) + u64::from(size) > 1 << 32 || self.overlaps(addr, size) {
            return false;
        }
        self.used.insert(addr, size);
        true
    }

    /// Reserve a free granule-aligned region of at least `size` bytes, without mapping.
    pub fn reserve(&mut self, size: u32) -> Option<u32> {
        let size = size.max(1).checked_add(GRANULE - 1)? & !(GRANULE - 1);
        let addr = self.find_free(size)?;
        self.used.insert(addr, size);
        Some(addr)
    }

    /// Reserve exactly `size` bytes at a free granule-aligned address. Like a
    /// Windows section view, the rest of the last granule is neither part of the
    /// region nor available to later reservations.
    pub fn reserve_exact(&mut self, size: u32) -> Option<u32> {
        let granules = size.max(1).checked_add(GRANULE - 1)? & !(GRANULE - 1);
        let addr = self.find_free(granules)?;
        self.used.insert(addr, size.max(1));
        Some(addr)
    }

    /// Reserve and map `size` bytes (rounded up to a granule).
    pub fn alloc(&mut self, cpu: &mut dyn Guest, size: u32, prot: Prot) -> Result<u32, MemFault> {
        let addr = self.reserve(size).ok_or(MemFault { addr: 0, len: size })?;
        let size = self.used[&addr];
        if let Err(error) = cpu.map(addr, size, prot) {
            self.used.remove(&addr);
            return Err(error);
        }
        Ok(addr)
    }

    /// Reserve a region and map only `[start + guard, end)` (the guard stays unmapped).
    pub fn alloc_with_guard(
        &mut self,
        cpu: &mut dyn Guest,
        size: u32,
        guard: u32,
        prot: Prot,
    ) -> Result<u32, MemFault> {
        let requested = size
            .checked_add(guard)
            .ok_or(MemFault { addr: 0, len: size })?;
        let addr = self.reserve(requested).ok_or(MemFault {
            addr: 0,
            len: requested,
        })?;
        let total = self.used[&addr];
        if let Err(error) = cpu.map(addr + guard, total - guard, prot) {
            self.used.remove(&addr);
            return Err(error);
        }
        Ok(addr)
    }

    /// Size of the region starting at `addr`, if it is one.
    pub fn region(&self, addr: u32) -> Option<u32> {
        self.used.get(&addr).copied()
    }

    /// `(start, size)` of the region containing `addr`.
    pub fn containing(&self, addr: u32) -> Option<(u32, u32)> {
        self.used
            .range(..=addr)
            .next_back()
            .map(|(&b, &s)| (b, s))
            .filter(|&(b, s)| addr - b < s)
    }

    /// Next reserved boundary; callers can describe a free interval without
    /// scanning hundreds of thousands of unmapped pages.
    pub fn next_region(&self, address: u32) -> Option<u32> {
        self.used.range(address..).next().map(|(&base, _)| base)
    }

    /// Unmap and release the region starting at `addr`. Unmapped pages (guards) are skipped.
    pub fn free(&mut self, cpu: &mut dyn Guest, addr: u32) -> Result<(), MemFault> {
        let size = self
            .used
            .get(&addr)
            .copied()
            .ok_or(MemFault { addr, len: 0 })?;
        if cpu.unmap(addr, size).is_err() {
            for offset in (0..size).step_by(PAGE as usize) {
                let _ = cpu.unmap(addr + offset, PAGE);
            }
            // The guard-page fallback must not hide a failed physical unpin or
            // make an allocation available for reuse while its pins still live.
            if cpu.has_locked_pages(addr, size) {
                return Err(MemFault { addr, len: size });
            }
        }
        self.used.remove(&addr);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adv_cpu::backend::Reg;

    #[derive(Default)]
    struct MappingGuest {
        fail_next: bool,
        locked: bool,
        requests: Vec<(u32, u32)>,
    }
    impl Guest for MappingGuest {
        fn reg(&self, _: Reg) -> u32 {
            0
        }
        fn set_reg(&mut self, _: Reg, _: u32) {}
        fn read(&self, addr: u32, data: &mut [u8]) -> Result<(), MemFault> {
            Err(MemFault {
                addr,
                len: data.len() as u32,
            })
        }
        fn write(&mut self, addr: u32, data: &[u8]) -> Result<(), MemFault> {
            Err(MemFault {
                addr,
                len: data.len() as u32,
            })
        }
        fn map(&mut self, addr: u32, len: u32, _: Prot) -> Result<(), MemFault> {
            self.requests.push((addr, len));
            if std::mem::take(&mut self.fail_next) {
                Err(MemFault { addr, len })
            } else {
                Ok(())
            }
        }
        fn protect(&mut self, _: u32, _: u32, _: Prot) -> Result<(), MemFault> {
            Ok(())
        }
        fn unmap(&mut self, addr: u32, len: u32) -> Result<(), MemFault> {
            if self.locked {
                Err(MemFault { addr, len })
            } else {
                Ok(())
            }
        }
        fn has_locked_pages(&self, _: u32, _: u32) -> bool {
            self.locked
        }
    }

    #[test]
    fn failed_unpin_keeps_the_allocation_owned_until_release_can_finish() {
        let mut vm = Vm::new();
        let mut guest = MappingGuest::default();
        let address = vm.alloc(&mut guest, PAGE, Prot::RW).unwrap();
        guest.locked = true;
        assert!(vm.free(&mut guest, address).is_err());
        assert_eq!(vm.region(address), Some(GRANULE));
        assert!(!vm.reserve_at(address, GRANULE));

        guest.locked = false;
        vm.free(&mut guest, address).unwrap();
        assert_eq!(vm.region(address), None);
        assert_eq!(vm.reserve(GRANULE), Some(address));
    }

    #[test]
    fn exact_reservations_leave_the_granule_tail_unreserved_but_unusable() {
        let mut vm = Vm::new();
        let view = vm.reserve_exact(0x3000).unwrap();
        assert_eq!(vm.region(view), Some(0x3000));
        assert_eq!(vm.containing(view + 0x3000), None);
        assert_eq!(vm.reserve(PAGE), Some(view + GRANULE));
    }

    #[test]
    fn reserve_at_rejects_overlap() {
        let mut vm = Vm::new();
        assert!(vm.reserve_at(0x40_0000, 0xd3000));
        assert!(!vm.reserve_at(0x40_0000, 0x1000));
        assert!(!vm.reserve_at(0x3f_f000, 0x2000));
        assert!(!vm.reserve_at(0x4d_2000, 0x1000));
        assert!(vm.reserve_at(0x4d_3000, 0x1000));
    }

    #[test]
    fn reserve_skips_fixed_regions_in_dynamic_range() {
        let mut vm = Vm::new();
        assert!(vm.reserve_at(DYN_START, 0x2000)); // e.g. RESOURCE.dll at its preferred base
        let a = vm.reserve(0x1000).unwrap();
        assert_eq!(a, DYN_START + GRANULE);
        let b = vm.reserve(0x2_0000).unwrap();
        assert_eq!(b, a + GRANULE);
    }

    #[test]
    fn failed_mapping_rolls_back_only_its_reservation_and_reuses_address() {
        for guard in [0, PAGE] {
            let mut vm = Vm::new();
            assert!(vm.reserve_at(DYN_START, GRANULE));
            let expected = DYN_START + GRANULE;
            let mut guest = MappingGuest {
                fail_next: true,
                ..MappingGuest::default()
            };
            let allocate = |vm: &mut Vm, guest: &mut MappingGuest| {
                if guard == 0 {
                    vm.alloc(guest, PAGE, Prot::RW)
                } else {
                    vm.alloc_with_guard(guest, PAGE, guard, Prot::RW)
                }
            };
            assert!(allocate(&mut vm, &mut guest).is_err());
            assert_eq!(vm.region(expected), None);
            assert_eq!(vm.region(DYN_START), Some(GRANULE));
            assert_eq!(allocate(&mut vm, &mut guest).unwrap(), expected);
            assert_eq!(guest.requests, [(expected + guard, GRANULE - guard); 2]);
        }
    }

    #[test]
    fn overflowing_size_and_guard_never_map_or_reserve_zero_bytes() {
        let mut vm = Vm::new();
        let mut guest = MappingGuest::default();
        for size in [u32::MAX, u32::MAX - 1, u32::MAX - GRANULE + 2] {
            assert!(vm.reserve(size).is_none());
            assert!(vm.alloc(&mut guest, size, Prot::RW).is_err());
        }
        assert!(
            vm.alloc_with_guard(&mut guest, u32::MAX, PAGE, Prot::RW)
                .is_err()
        );
        assert!(
            vm.alloc_with_guard(&mut guest, u32::MAX - PAGE, PAGE, Prot::RW)
                .is_err()
        );
        assert!(guest.requests.is_empty());
        assert_eq!(vm.alloc(&mut guest, PAGE, Prot::RW).unwrap(), DYN_START);
    }

    #[test]
    fn fixed_region_ending_at_four_gib_cannot_wrap_the_dynamic_cursor() {
        let mut vm = Vm::new();
        assert!(vm.reserve_at(DYN_START + GRANULE, 0u32.wrapping_sub(DYN_START + GRANULE)));
        assert_eq!(vm.reserve(PAGE), Some(DYN_START));
        assert_eq!(vm.reserve(PAGE), None);
    }
}
