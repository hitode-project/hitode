//! Guest heaps (HeapAlloc & co., also behind malloc, LocalAlloc, GlobalAlloc).
//!
//! Metadata lives on the host: blocks never carry headers in guest memory, so guest
//! overruns can't corrupt the allocator. Each heap owns arenas from the VM; free space is
//! indexed by address (for coalescing) and by size (best fit). Blocks are 8-byte aligned
//! like the x86 Windows heap. Large blocks get their own VM region; one recently
//! freed region per heap is retained for equal-capacity reuse, bounded to 16 MiB.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use adv_cpu::backend::{Guest, MemFault, Prot};
use adv_machine::{Vm, layout::GRANULE};

const ALIGN: u32 = 8;
const ARENA: u32 = 0x10_0000;
const LARGE: u32 = 0x8_0000;
const LARGE_REUSE_LIMIT: u32 = 16 * ARENA;

fn round(size: u32) -> Option<u32> {
    size.max(1)
        .checked_add(ALIGN - 1)
        .map(|size| size & !(ALIGN - 1))
}

fn zero_memory(cpu: &mut dyn Guest, addr: u32, len: u32) -> Result<(), MemFault> {
    let zeros = [0; 4096];
    let mut offset = 0;
    while offset < len {
        let n = (len - offset).min(zeros.len() as u32);
        cpu.write(addr + offset, &zeros[..n as usize])?;
        offset += n;
    }
    Ok(())
}

#[derive(Default)]
pub struct Heap {
    synchronization: HeapSynchronization,
    executable: bool,
    generate_exceptions: bool,
    free_by_addr: BTreeMap<u32, u32>,
    free_by_size: BTreeSet<(u32, u32)>,
    /// Block address -> (rounded size, requested size).
    used: HashMap<u32, (u32, u32)>,
    /// Blocks with their own VM region.
    large: HashMap<u32, u32>,
    /// One free large region (address, VM capacity), outside the live block index.
    spare_large: Option<(u32, u32)>,
    arenas: Vec<u32>,
}

/// Process-owned serialization identity; the personality initializes and waits
/// on the lock stored in this heap's reserved infrastructure header.
#[derive(Clone, Copy)]
pub struct HeapSynchronization {
    pub serialized: bool,
    pub lock: Option<u32>,
}

impl Default for HeapSynchronization {
    fn default() -> Self {
        Self {
            serialized: true,
            lock: None,
        }
    }
}

impl Heap {
    fn protection(&self) -> Prot {
        if self.executable { Prot::RWX } else { Prot::RW }
    }

    fn insert_free(&mut self, addr: u32, size: u32) {
        let (mut addr, mut size) = (addr, size);
        // Merge with the following and preceding free ranges.
        if let Some(&next) = self.free_by_addr.get(&(addr + size)) {
            self.free_by_addr.remove(&(addr + size));
            self.free_by_size.remove(&(next, addr + size));
            size += next;
        }
        if let Some((&pa, &ps)) = self.free_by_addr.range(..addr).next_back()
            && pa + ps == addr
        {
            self.free_by_addr.remove(&pa);
            self.free_by_size.remove(&(ps, pa));
            addr = pa;
            size += ps;
        }
        self.free_by_addr.insert(addr, size);
        self.free_by_size.insert((size, addr));
    }

    fn take_free(&mut self, size: u32) -> Option<u32> {
        let &(fsize, faddr) = self.free_by_size.range((size, 0)..).next()?;
        self.free_by_size.remove(&(fsize, faddr));
        self.free_by_addr.remove(&faddr);
        if fsize > size {
            self.insert_free(faddr + size, fsize - size);
        }
        Some(faddr)
    }

    fn grow(&mut self, cpu: &mut dyn Guest, vm: &mut Vm, need: u32) -> Result<(), MemFault> {
        let size = need.max(ARENA);
        let a = self.map_region(cpu, vm, size)?;
        self.arenas.push(a);
        self.insert_free(a, vm.region(a).unwrap_or(size));
        Ok(())
    }

    fn map_region(&mut self, cpu: &mut dyn Guest, vm: &mut Vm, size: u32) -> Result<u32, MemFault> {
        match vm.alloc(cpu, size, self.protection()) {
            Ok(addr) => Ok(addr),
            Err(error) => {
                // Reclaim the spare before reporting memory pressure to the guest.
                let Some((spare, _)) = self.spare_large else {
                    return Err(error);
                };
                vm.free(cpu, spare)?;
                self.spare_large = None;
                vm.alloc(cpu, size, self.protection())
            }
        }
    }
}

#[derive(Default)]
pub struct Heaps {
    heaps: HashMap<u32, Heap>,
    pub process_heap: u32,
}

impl Heaps {
    /// Snapshot only handles owned by this guest process, in stable order.
    pub fn handles(&self) -> Vec<u32> {
        let mut handles: Vec<_> = self.heaps.keys().copied().collect();
        handles.sort_unstable();
        handles
    }
    /// Create a heap; its handle is the address of a small reserved region (like Windows,
    /// where a heap handle points at the heap's header).
    pub fn create(&mut self, cpu: &mut dyn Guest, vm: &mut Vm) -> Result<u32, MemFault> {
        self.create_with_options(cpu, vm, true, false)
    }

    /// Create an independently owned heap with its caller-selected serialization.
    pub fn create_with_serialization(
        &mut self,
        cpu: &mut dyn Guest,
        vm: &mut Vm,
        serialized: bool,
    ) -> Result<u32, MemFault> {
        self.create_with_options(cpu, vm, serialized, false)
    }

    /// Executable heaps retain their permission for initial, grown and large arenas.
    pub fn create_with_execute(
        &mut self,
        cpu: &mut dyn Guest,
        vm: &mut Vm,
        executable: bool,
    ) -> Result<u32, MemFault> {
        self.create_with_options(cpu, vm, true, executable)
    }

    /// Heap serialization and execute permission are independent creation options.
    pub fn create_with_options(
        &mut self,
        cpu: &mut dyn Guest,
        vm: &mut Vm,
        serialized: bool,
        executable: bool,
    ) -> Result<u32, MemFault> {
        self.create_with_policy(cpu, vm, serialized, executable, false)
    }

    /// Retain the allocation failure policy in the same owner as heap storage.
    pub fn create_with_policy(
        &mut self,
        cpu: &mut dyn Guest,
        vm: &mut Vm,
        serialized: bool,
        executable: bool,
        generate_exceptions: bool,
    ) -> Result<u32, MemFault> {
        let mut h = Heap {
            executable,
            generate_exceptions,
            synchronization: HeapSynchronization {
                serialized,
                lock: None,
            },
            ..Heap::default()
        };
        let base = vm.alloc(cpu, ARENA, h.protection())?;
        h.arenas.push(base);
        // Keep the first granule-page as the "header" so no block address equals the handle.
        h.insert_free(base + 0x1000, vm.region(base).unwrap_or(ARENA) - 0x1000);
        self.heaps.insert(base, h);
        Ok(base)
    }

    /// Normalize the allocator's existing zero-handle process-heap alias.
    pub fn normalize(&self, handle: u32) -> Option<u32> {
        let handle = if handle == 0 {
            self.process_heap
        } else {
            handle
        };
        self.heaps.contains_key(&handle).then_some(handle)
    }

    pub fn synchronization(&self, handle: u32) -> Option<HeapSynchronization> {
        self.heaps
            .get(&self.normalize(handle)?)
            .map(|heap| heap.synchronization)
    }

    /// Invalid or destroyed handles cannot borrow another heap's creation policy.
    pub fn generates_exceptions(&self, handle: u32) -> Option<bool> {
        self.heaps
            .get(&self.normalize(handle)?)
            .map(|heap| heap.generate_exceptions)
    }

    /// Publish a personality-owned lock only after its guest storage is initialized.
    pub fn set_lock(&mut self, handle: u32, lock: u32) -> bool {
        let Some(handle) = self.normalize(handle) else {
            return false;
        };
        self.heaps.get_mut(&handle).unwrap().synchronization.lock = Some(lock);
        true
    }

    pub fn destroy(&mut self, cpu: &mut dyn Guest, vm: &mut Vm, handle: u32) -> bool {
        let Some(h) = self.heaps.remove(&handle) else {
            return false;
        };
        for a in h
            .arenas
            .into_iter()
            .chain(h.large.into_keys())
            .chain(h.spare_large.map(|(addr, _)| addr))
        {
            let _ = vm.free(cpu, a);
        }
        true
    }

    fn heap(&mut self, handle: u32) -> Option<&mut Heap> {
        let handle = if handle == 0 {
            self.process_heap
        } else {
            handle
        };
        self.heaps.get_mut(&handle)
    }

    pub fn alloc(
        &mut self,
        cpu: &mut dyn Guest,
        vm: &mut Vm,
        handle: u32,
        size: u32,
        zero: bool,
    ) -> Option<u32> {
        let h = self.heap(handle)?;
        let rsize = round(size)?;
        let mut freshly_mapped = false;
        let addr = if rsize >= LARGE {
            let capacity = rsize.checked_add(GRANULE - 1)? & !(GRANULE - 1);
            let a = if h.spare_large.is_some_and(|(_, len)| len == capacity) {
                let (a, _) = h.spare_large.take().unwrap();
                // VirtualProtect may have changed a previous caller's pages.
                // Restore this heap's policy before publishing reused storage.
                if cpu.protect(a, capacity, h.protection()).is_err() {
                    let _ = vm.free(cpu, a);
                    return None;
                }
                a
            } else {
                let a = h.map_region(cpu, vm, rsize).ok()?;
                freshly_mapped = true; // Guest::map guarantees zeroed memory.
                a
            };
            h.large.insert(a, rsize);
            a
        } else {
            match h.take_free(rsize) {
                Some(a) => a,
                None => {
                    h.grow(cpu, vm, rsize).ok()?;
                    h.take_free(rsize)?
                }
            }
        };
        h.used.insert(addr, (rsize, size));
        if zero && !freshly_mapped && zero_memory(cpu, addr, rsize).is_err() {
            self.release(cpu, vm, handle, addr, false);
            return None;
        }
        Some(addr)
    }

    pub fn free(&mut self, cpu: &mut dyn Guest, vm: &mut Vm, handle: u32, addr: u32) -> bool {
        self.release(cpu, vm, handle, addr, true)
    }

    fn release(
        &mut self,
        cpu: &mut dyn Guest,
        vm: &mut Vm,
        handle: u32,
        addr: u32,
        retain_large: bool,
    ) -> bool {
        let Some(h) = self.heap(handle) else {
            return false;
        };
        let Some((rsize, _)) = h.used.remove(&addr) else {
            return false;
        };
        if h.large.remove(&addr).is_some() {
            let capacity = vm.region(addr).unwrap_or(rsize);
            if retain_large
                && capacity <= LARGE_REUSE_LIMIT
                && !cpu.has_locked_pages(addr, capacity)
            {
                if h.spare_large
                    .is_none_or(|(previous, _)| vm.free(cpu, previous).is_ok())
                {
                    h.spare_large = Some((addr, capacity));
                } else {
                    // Keep ownership of a spare whose pins could not retire.
                    let _ = vm.free(cpu, addr);
                }
            } else {
                let _ = vm.free(cpu, addr);
            }
        } else {
            h.insert_free(addr, rsize);
        }
        true
    }

    /// Requested size of a live block.
    pub fn size(&mut self, handle: u32, addr: u32) -> Option<u32> {
        self.heap(handle)?.used.get(&addr).map(|&(_, s)| s)
    }

    /// Which heap a block belongs to (for LocalFree/GlobalFree style APIs without a handle).
    pub fn owner(&self, addr: u32) -> Option<u32> {
        self.heaps
            .iter()
            .find(|(_, h)| h.used.contains_key(&addr))
            .map(|(&k, _)| k)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn realloc(
        &mut self,
        cpu: &mut dyn Guest,
        vm: &mut Vm,
        handle: u32,
        addr: u32,
        size: u32,
        zero: bool,
        in_place_only: bool,
    ) -> Option<u32> {
        let h = self.heap(handle)?;
        let &(rold, old) = h.used.get(&addr)?;
        let rnew = round(size)?;
        if rnew <= rold && !h.large.contains_key(&addr) {
            if zero && size > old && zero_memory(cpu, addr + old, size - old).is_err() {
                return None;
            }
            // Shrink in place.
            if rnew < rold {
                h.insert_free(addr + rnew, rold - rnew);
            }
            h.used.insert(addr, (rnew, size));
            return Some(addr);
        }
        // Grow in place if the following range is free.
        if !h.large.contains_key(&addr)
            && let Some(&next) = h.free_by_addr.get(&(addr + rold))
            && rold + next >= rnew
        {
            if zero && size > old && zero_memory(cpu, addr + old, size - old).is_err() {
                return None;
            }
            h.free_by_addr.remove(&(addr + rold));
            h.free_by_size.remove(&(next, addr + rold));
            if rold + next > rnew {
                h.insert_free(addr + rnew, rold + next - rnew);
            }
            h.used.insert(addr, (rnew, size));
            return Some(addr);
        }
        if in_place_only {
            return None;
        }
        let new = self.alloc(cpu, vm, handle, size, zero)?;
        let mut buf = [0; 4096];
        let mut offset = 0;
        let copy = old.min(size);
        while offset < copy {
            let n = (copy - offset).min(buf.len() as u32);
            if cpu.read(addr + offset, &mut buf[..n as usize]).is_err()
                || cpu.write(new + offset, &buf[..n as usize]).is_err()
            {
                // Failed reallocations must release speculative VM storage.
                self.release(cpu, vm, handle, new, false);
                return None;
            }
            offset += n;
        }
        self.free(cpu, vm, handle, addr);
        Some(new)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adv_cpu::backend::Reg;
    use std::cell::Cell;

    #[derive(Default)]
    struct MemoryGuest {
        memory: BTreeMap<u32, Vec<u8>>,
        read_calls: Cell<usize>,
        write_calls: usize,
        max_read: Cell<usize>,
        max_write: usize,
        fail_read_call: Option<usize>,
        fail_write_call: Option<usize>,
        last_map: u32,
        map_calls: usize,
        protections: Vec<(u32, u32, Prot)>,
        fail_protect: bool,
        fail_next_map: bool,
        locked: bool,
        pinned_region: Option<u32>,
    }
    impl MemoryGuest {
        fn bytes(&self, addr: u32, len: usize) -> Option<&[u8]> {
            let (&base, bytes) = self.memory.range(..=addr).next_back()?;
            bytes.get((addr - base) as usize..(addr - base) as usize + len)
        }
        fn reset_counts(&mut self) {
            self.read_calls.set(0);
            self.write_calls = 0;
            self.max_read.set(0);
            self.max_write = 0;
        }
    }
    impl Guest for MemoryGuest {
        fn reg(&self, _: Reg) -> u32 {
            0
        }
        fn set_reg(&mut self, _: Reg, _: u32) {}
        fn read(&self, addr: u32, bytes: &mut [u8]) -> Result<(), MemFault> {
            self.read_calls.set(self.read_calls.get() + 1);
            self.max_read.set(self.max_read.get().max(bytes.len()));
            let fault = MemFault {
                addr,
                len: bytes.len() as u32,
            };
            if self.fail_read_call == Some(self.read_calls.get()) {
                return Err(fault);
            }
            bytes.copy_from_slice(self.bytes(addr, bytes.len()).ok_or(fault)?);
            Ok(())
        }
        fn write(&mut self, addr: u32, bytes: &[u8]) -> Result<(), MemFault> {
            self.write_calls += 1;
            self.max_write = self.max_write.max(bytes.len());
            let fault = MemFault {
                addr,
                len: bytes.len() as u32,
            };
            if self.fail_write_call == Some(self.write_calls) {
                return Err(fault);
            }
            let (&base, mapped) = self.memory.range_mut(..=addr).next_back().ok_or(fault)?;
            mapped
                .get_mut((addr - base) as usize..(addr - base) as usize + bytes.len())
                .ok_or(fault)?
                .copy_from_slice(bytes);
            Ok(())
        }
        fn map(&mut self, addr: u32, len: u32, _: Prot) -> Result<(), MemFault> {
            self.map_calls += 1;
            if std::mem::take(&mut self.fail_next_map) {
                return Err(MemFault { addr, len });
            }
            self.last_map = addr;
            self.memory.insert(addr, vec![0; len as usize]);
            Ok(())
        }
        fn protect(&mut self, addr: u32, len: u32, prot: Prot) -> Result<(), MemFault> {
            self.protections.push((addr, len, prot));
            if self.fail_protect {
                Err(MemFault { addr, len })
            } else {
                Ok(())
            }
        }
        fn unmap(&mut self, addr: u32, _: u32) -> Result<(), MemFault> {
            if self
                .memory
                .range(..=addr)
                .next_back()
                .is_some_and(|(&base, _)| self.pinned_region == Some(base))
            {
                return Err(MemFault { addr, len: 4096 });
            }
            self.memory.remove(&addr);
            Ok(())
        }
        fn has_locked_pages(&self, addr: u32, _: u32) -> bool {
            self.locked || self.pinned_region == Some(addr)
        }
    }
    fn heap() -> (MemoryGuest, Vm, Heaps) {
        let (mut guest, mut vm, mut heaps) = (MemoryGuest::default(), Vm::new(), Heaps::default());
        heaps.process_heap = heaps.create(&mut guest, &mut vm).unwrap();
        (guest, vm, heaps)
    }

    #[test]
    fn large_reuse_matches_vm_capacity_and_clears_only_when_requested() {
        let (mut guest, mut vm, mut heaps) = heap();
        let size = LARGE + 13;
        let block = heaps.alloc(&mut guest, &mut vm, 0, size, true).unwrap();
        let maps = guest.map_calls;
        assert_eq!(guest.write_calls, 0, "a new mapping is already zeroed");
        guest.write(block, &[0xa5; 32]).unwrap();
        assert!(heaps.free(&mut guest, &mut vm, 0, block));
        assert_eq!(heaps.owner(block), None);
        assert_eq!(heaps.size(0, block), None);
        assert!(!heaps.free(&mut guest, &mut vm, 0, block));
        guest.reset_counts();
        let reused = heaps
            .alloc(&mut guest, &mut vm, 0, size + 9, false)
            .unwrap();
        assert_eq!(reused, block);
        assert_eq!(guest.map_calls, maps);
        assert_eq!(guest.write_calls, 0);
        assert_eq!(guest.bytes(reused, 32).unwrap(), [0xa5; 32]);
        assert_eq!(heaps.size(0, reused), Some(size + 9));
        assert!(heaps.free(&mut guest, &mut vm, 0, reused));
        let cleared = heaps.alloc(&mut guest, &mut vm, 0, size, true).unwrap();
        assert_eq!(cleared, block);
        assert!(
            guest
                .bytes(cleared, size as usize)
                .unwrap()
                .iter()
                .all(|&b| b == 0)
        );
        assert_eq!(guest.map_calls, maps);
        assert_eq!(guest.protections, [(block, LARGE + GRANULE, Prot::RW); 2]);
        assert!(guest.max_write <= 4096);
    }

    #[test]
    fn large_reuse_is_bounded_retires_the_previous_spare_and_destroys_all_regions() {
        let (mut guest, mut vm, mut heaps) = heap();
        let other = heaps.create(&mut guest, &mut vm).unwrap();
        let first = heaps.alloc(&mut guest, &mut vm, 0, LARGE, false).unwrap();
        let recent = heaps
            .alloc(&mut guest, &mut vm, 0, LARGE + 8, false)
            .unwrap();
        let foreign = heaps
            .alloc(&mut guest, &mut vm, other, LARGE, false)
            .unwrap();
        assert!(heaps.free(&mut guest, &mut vm, other, foreign));
        assert!(heaps.free(&mut guest, &mut vm, 0, first));
        assert!(heaps.free(&mut guest, &mut vm, 0, recent));
        assert_eq!(
            vm.region(first),
            None,
            "only the most recent spare survives"
        );
        assert_eq!(vm.region(recent), Some(LARGE + GRANULE));
        assert_eq!(vm.region(foreign), Some(LARGE));
        let mismatch = heaps
            .alloc(&mut guest, &mut vm, 0, LARGE + GRANULE + 8, false)
            .unwrap();
        assert_ne!(
            mismatch, recent,
            "different capacities must not consume the spare"
        );
        let huge = heaps
            .alloc(&mut guest, &mut vm, 0, 16 * ARENA + 1, false)
            .unwrap();
        assert!(heaps.free(&mut guest, &mut vm, 0, huge));
        assert_eq!(vm.region(huge), None);
        assert_eq!(vm.region(recent), Some(LARGE + GRANULE));
        assert!(heaps.destroy(&mut guest, &mut vm, heaps.process_heap));
        for block in [recent, mismatch] {
            assert_eq!(vm.region(block), None);
            assert!(!guest.memory.contains_key(&block));
        }
        assert_eq!(vm.region(foreign), Some(LARGE));
        assert!(heaps.destroy(&mut guest, &mut vm, other));
        assert!(guest.memory.is_empty());
    }

    #[test]
    fn large_reuse_releases_failed_protection_and_does_not_retain_pinned_pages() {
        let (mut guest, mut vm, mut heaps) = heap();
        let block = heaps.alloc(&mut guest, &mut vm, 0, LARGE, false).unwrap();
        assert!(heaps.free(&mut guest, &mut vm, 0, block));
        guest.fail_protect = true;
        assert_eq!(heaps.alloc(&mut guest, &mut vm, 0, LARGE, false), None);
        assert_eq!(vm.region(block), None);
        assert_eq!(heaps.owner(block), None);
        guest.fail_protect = false;
        let pinned = heaps.alloc(&mut guest, &mut vm, 0, LARGE, false).unwrap();
        guest.locked = true;
        assert!(heaps.free(&mut guest, &mut vm, 0, pinned));
        assert_eq!(vm.region(pinned), None);
    }

    #[test]
    fn large_reuse_limit_is_inclusive_and_memory_pressure_reclaims_the_spare() {
        let (mut guest, mut vm, mut heaps) = heap();
        let limit = 16 * ARENA;
        let block = heaps.alloc(&mut guest, &mut vm, 0, limit, false).unwrap();
        assert!(heaps.free(&mut guest, &mut vm, 0, block));
        let maps = guest.map_calls;
        assert_eq!(
            heaps.alloc(&mut guest, &mut vm, 0, limit, false),
            Some(block)
        );
        assert_eq!(guest.map_calls, maps);
        assert!(heaps.free(&mut guest, &mut vm, 0, block));
        guest.fail_next_map = true;
        let smaller = heaps.alloc(&mut guest, &mut vm, 0, LARGE, false).unwrap();
        assert_eq!(
            guest.map_calls,
            maps + 2,
            "retry once after retiring the spare"
        );
        assert_eq!(vm.region(block), Some(LARGE));
        assert_eq!(smaller, block);
        assert_eq!(heaps.size(0, smaller), Some(LARGE));
    }

    #[test]
    fn large_reuse_keeps_ownership_when_spare_eviction_cannot_retire_pins() {
        let (mut guest, mut vm, mut heaps) = heap();
        let spare = heaps.alloc(&mut guest, &mut vm, 0, LARGE, false).unwrap();
        assert!(heaps.free(&mut guest, &mut vm, 0, spare));
        guest.pinned_region = Some(spare);
        let next = heaps
            .alloc(&mut guest, &mut vm, 0, LARGE + 8, false)
            .unwrap();
        assert!(heaps.free(&mut guest, &mut vm, 0, next));
        assert_eq!(vm.region(next), None);
        assert_eq!(vm.region(spare), Some(LARGE));
        guest.pinned_region = None;
        assert!(heaps.destroy(&mut guest, &mut vm, heaps.process_heap));
        assert!(guest.memory.is_empty());
        assert_eq!(vm.region(spare), None);
    }

    #[test]
    fn large_reuse_restores_guest_permissions_for_plain_and_executable_heaps() {
        use adv_cpu::JitCpu;
        let mut cpu = JitCpu::new(adv_machine::layout::PARK_ADDR, false).unwrap();
        let (mut vm, mut heaps) = (Vm::new(), Heaps::default());
        for executable in [false, true] {
            let heap = heaps
                .create_with_execute(&mut cpu, &mut vm, executable)
                .unwrap();
            let block = heaps.alloc(&mut cpu, &mut vm, heap, LARGE, false).unwrap();
            cpu.protect(block, LARGE, Prot::READ).unwrap();
            assert!(heaps.free(&mut cpu, &mut vm, heap, block));
            assert_eq!(
                heaps.alloc(&mut cpu, &mut vm, heap, LARGE, true),
                Some(block)
            );
            assert!(cpu.accessible(block, LARGE, Prot::RW));
            assert_eq!(cpu.accessible(block, LARGE, Prot::EXEC), executable);
            assert!(heaps.destroy(&mut cpu, &mut vm, heap));
            assert!(!cpu.accessible(block, 1, Prot::READ));
        }
    }

    #[test]
    fn allocation_failure_policy_belongs_to_one_live_heap() {
        let (mut guest, mut vm, mut heaps) = heap();
        let ordinary = heaps.process_heap;
        let raising = heaps
            .create_with_policy(&mut guest, &mut vm, false, true, true)
            .unwrap();
        assert_eq!(heaps.generates_exceptions(ordinary), Some(false));
        assert_eq!(heaps.generates_exceptions(0), Some(false));
        assert_eq!(heaps.generates_exceptions(raising), Some(true));
        assert_eq!(heaps.generates_exceptions(0x12345678), None);
        assert!(!heaps.synchronization(raising).unwrap().serialized);
        heaps.process_heap = raising;
        assert_eq!(heaps.generates_exceptions(0), Some(true));
        assert!(heaps.destroy(&mut guest, &mut vm, raising));
        assert_eq!(heaps.generates_exceptions(raising), None);
        assert_eq!(heaps.generates_exceptions(0), None);
        assert_eq!(heaps.generates_exceptions(ordinary), Some(false));
    }

    #[test]
    fn executable_heaps_keep_permissions_across_growth_large_blocks_and_reallocation() {
        use adv_cpu::{
            JitCpu,
            backend::{Backend, Exit, TrapAction, TrapHandler},
        };
        struct Stop;
        impl TrapHandler for Stop {
            fn on_trap(&mut self, _cpu: &mut dyn Guest, vector: u8) -> TrapAction {
                assert_eq!(vector, 0x2e);
                TrapAction::Park
            }
        }
        let mut cpu = JitCpu::new(adv_machine::layout::PARK_ADDR, true).unwrap();
        let (mut vm, mut heaps) = (Vm::new(), Heaps::default());
        let plain = heaps.create(&mut cpu, &mut vm).unwrap();
        let code = heaps.create_with_execute(&mut cpu, &mut vm, true).unwrap();
        let ordinary = heaps.alloc(&mut cpu, &mut vm, plain, 32, false).unwrap();
        let mut blocks = Vec::new();
        for size in [0x60000, 0x60000, 0x60000, 0x90000] {
            blocks.push(heaps.alloc(&mut cpu, &mut vm, code, size, false).unwrap());
        }
        cpu.write(blocks[0], &[0xb8, 37, 0, 0, 0, 0xcd, 0x2e])
            .unwrap();
        let moved = heaps
            .realloc(&mut cpu, &mut vm, code, blocks[0], 0x180000, false, false)
            .unwrap();
        blocks[0] = moved;
        for &address in &blocks {
            assert!(cpu.accessible(address, 7, Prot::RWX));
            cpu.write(address, &[0xb8, 37, 0, 0, 0, 0xcd, 0x2e])
                .unwrap();
            cpu.set_reg(Reg::Eip, address);
            assert!(matches!(cpu.run(&mut Stop), Exit::Parked));
            assert_eq!(cpu.reg(Reg::Eax), 37);
        }
        cpu.write(ordinary, &[0xb8, 37, 0, 0, 0, 0xcd, 0x2e])
            .unwrap();
        cpu.set_reg(Reg::Eip, ordinary);
        assert!(!cpu.accessible(ordinary, 7, Prot::EXEC));
        assert!(matches!(cpu.run(&mut Stop), Exit::Fault(_)));
        assert!(heaps.destroy(&mut cpu, &mut vm, code));
        for address in blocks {
            assert!(!cpu.accessible(address, 1, Prot::READ));
        }
    }

    #[test]
    fn serialization_and_execute_options_are_independent() {
        use adv_cpu::JitCpu;
        let mut cpu = JitCpu::new(adv_machine::layout::PARK_ADDR, false).unwrap();
        let (mut vm, mut heaps) = (Vm::new(), Heaps::default());
        for serialized in [false, true] {
            for executable in [false, true] {
                let heap = heaps
                    .create_with_options(&mut cpu, &mut vm, serialized, executable)
                    .unwrap();
                let synchronization = heaps.synchronization(heap).unwrap();
                assert_eq!(synchronization.serialized, serialized);
                assert_eq!(synchronization.lock, None);
                for size in [32, 0x90000] {
                    let block = heaps.alloc(&mut cpu, &mut vm, heap, size, false).unwrap();
                    assert!(cpu.accessible(block, size, Prot::RW));
                    assert_eq!(cpu.accessible(block, size, Prot::EXEC), executable);
                }
                assert!(heaps.destroy(&mut cpu, &mut vm, heap));
            }
        }
    }

    #[test]
    fn moving_realloc_preserves_bytes_and_uses_bounded_copy_and_zero_blocks() {
        let (mut guest, mut vm, mut heaps) = heap();
        let old = heaps.alloc(&mut guest, &mut vm, 0, 10_017, false).unwrap();
        heaps.alloc(&mut guest, &mut vm, 0, 64, false).unwrap(); // prevents in-place growth
        let pattern: Vec<_> = (0..10_017).map(|i| (i % 251) as u8).collect();
        guest.write(old, &pattern).unwrap();
        guest.reset_counts();
        let new = heaps
            .realloc(&mut guest, &mut vm, 0, old, 21_000, true, false)
            .unwrap();
        assert_ne!(new, old);
        assert_eq!(heaps.size(0, old), None);
        assert_eq!(heaps.size(0, new), Some(21_000));
        assert_eq!(guest.bytes(new, pattern.len()).unwrap(), pattern);
        assert!(
            guest
                .bytes(new + pattern.len() as u32, 21_000 - pattern.len())
                .unwrap()
                .iter()
                .all(|&b| b == 0)
        );
        assert!(guest.max_read.get() <= 4096 && guest.max_write <= 4096);
    }

    #[test]
    fn moving_read_or_write_failure_keeps_old_and_releases_new_large_region() {
        for fail_read in [false, true] {
            let (mut guest, mut vm, mut heaps) = heap();
            let size = LARGE + 8193;
            let old = heaps.alloc(&mut guest, &mut vm, 0, size, false).unwrap();
            let pattern = vec![0x9d; size as usize];
            guest.write(old, &pattern).unwrap();
            guest.reset_counts();
            if fail_read {
                guest.fail_read_call = Some(2);
            } else {
                guest.fail_write_call = Some(2);
            }
            assert_eq!(
                heaps.realloc(&mut guest, &mut vm, 0, old, size + 8192, false, false),
                None
            );
            let failed_new = guest.last_map;
            assert_ne!(failed_new, old);
            assert_eq!(heaps.size(0, old), Some(size));
            assert_eq!(guest.bytes(old, size as usize).unwrap(), pattern);
            assert_eq!(heaps.owner(failed_new), None);
            assert_eq!(vm.region(failed_new), None);
            assert!(!guest.memory.contains_key(&failed_new));
            guest.fail_read_call = None;
            guest.fail_write_call = None;
            let new = heaps
                .realloc(&mut guest, &mut vm, 0, old, size + 8192, false, false)
                .unwrap();
            assert_eq!(new, failed_new);
            assert_eq!(guest.bytes(new, size as usize).unwrap(), pattern);
        }
    }

    #[test]
    fn in_place_zeroing_covers_padding_and_failed_growth_preserves_metadata() {
        let (mut guest, mut vm, mut heaps) = heap();
        let old = heaps.alloc(&mut guest, &mut vm, 0, 6, false).unwrap();
        guest.write(old, &[0xaa; 8]).unwrap();
        assert_eq!(
            heaps.realloc(&mut guest, &mut vm, 0, old, 8, true, false),
            Some(old)
        );
        assert_eq!(
            guest.bytes(old, 8).unwrap(),
            [0xaa, 0xaa, 0xaa, 0xaa, 0xaa, 0xaa, 0, 0]
        );
        guest.reset_counts();
        guest.fail_write_call = Some(2);
        assert_eq!(
            heaps.realloc(&mut guest, &mut vm, 0, old, 5000, true, false),
            None
        );
        assert_eq!(heaps.size(0, old), Some(8));
        assert_eq!(guest.bytes(old, 6).unwrap(), [0xaa; 6]);
        guest.fail_write_call = None;
        assert_eq!(
            heaps.realloc(&mut guest, &mut vm, 0, old, 5000, true, true),
            Some(old)
        );
        assert!(guest.bytes(old + 8, 4992).unwrap().iter().all(|&b| b == 0));
    }

    #[test]
    fn overflowing_rounding_and_failed_zero_allocation_retain_ownership() {
        let (mut guest, mut vm, mut heaps) = heap();
        let old = heaps.alloc(&mut guest, &mut vm, 0, 16, false).unwrap();
        assert_eq!(heaps.alloc(&mut guest, &mut vm, 0, u32::MAX, false), None);
        assert_eq!(
            heaps.realloc(&mut guest, &mut vm, 0, old, u32::MAX, false, false),
            None
        );
        assert_eq!(heaps.size(0, old), Some(16));
        // New VM mappings cannot fail a redundant clear; exercise reused bytes.
        let spare = heaps.alloc(&mut guest, &mut vm, 0, LARGE, false).unwrap();
        assert!(heaps.free(&mut guest, &mut vm, 0, spare));
        guest.reset_counts();
        guest.fail_write_call = Some(1);
        assert_eq!(heaps.alloc(&mut guest, &mut vm, 0, LARGE, true), None);
        let failed = guest.last_map;
        assert_eq!(vm.region(failed), None);
        assert_eq!(heaps.owner(failed), None);
        guest.fail_write_call = None;
        assert_eq!(
            heaps.alloc(&mut guest, &mut vm, 0, LARGE, true),
            Some(failed)
        );
        assert!(guest.max_write <= 4096);
    }
}
