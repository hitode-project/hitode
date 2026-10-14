//! Guest heaps (HeapAlloc & co., also behind malloc, LocalAlloc, GlobalAlloc).
//!
//! Small process-heap blocks carry guest headers and shared free lists. Host reads
//! validate headers against registered slabs; corruption is an invalid pointer, never
//! a panic. Other allocations use host metadata and coalescing arena free ranges.
//! Blocks are 8-byte aligned; large blocks own freshly zeroed VM regions.

use std::collections::{BTreeMap, BTreeSet};

use rustc_hash::FxHashMap;

use adv_cpu::backend::{Guest, GuestExt, MemFault, Prot};
use adv_machine::Vm;

mod inspect;
mod small;

const ALIGN: u32 = 8;
const ARENA: u32 = 0x10_0000;
const LARGE: u32 = 0x8_0000;

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
    used: FxHashMap<u32, (u32, u32)>,
    /// Blocks with their own VM region.
    large: FxHashMap<u32, u32>,
    arenas: Vec<u32>,
    slabs: Option<Vec<small::Slab>>,
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
        let a = vm.alloc(cpu, size, self.protection())?;
        self.arenas.push(a);
        self.insert_free(a, vm.region(a).unwrap_or(size));
        Ok(())
    }
}

#[derive(Default)]
pub struct Heaps {
    heaps: FxHashMap<u32, Heap>,
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
        for a in h.arenas.into_iter().chain(h.large.into_keys()) {
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
        let handle = self.normalize(handle)?;
        let h = self.heap(handle)?;
        if let Some(slabs) = &h.slabs
            && (1..=512).contains(&size)
            && (slabs.len() < 128 || cpu.read_u32(handle + 0x110 + ((size - 1) / 8) * 8).ok()? != 0)
        {
            return h.small_alloc(cpu, vm, handle, size, zero);
        }
        let rsize = round(size)?;
        let mut freshly_mapped = false;
        let addr = if rsize >= LARGE {
            let a = vm.alloc(cpu, rsize, h.protection()).ok()?;
            freshly_mapped = true; // Guest::map guarantees zeroed memory, even without ZERO_MEMORY.
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
            self.free(cpu, vm, handle, addr);
            return None;
        }
        Some(addr)
    }

    pub fn free(&mut self, cpu: &mut dyn Guest, vm: &mut Vm, handle: u32, addr: u32) -> bool {
        let Some(handle) = self.normalize(handle) else {
            return false;
        };
        let h = self.heap(handle).unwrap();
        if let Some(result) = h.small_free(cpu, handle, addr) {
            return result;
        }
        let Some((rsize, _)) = h.used.remove(&addr) else {
            return false;
        };
        if h.large.remove(&addr).is_some() {
            let _ = vm.free(cpu, addr);
        } else {
            h.insert_free(addr, rsize);
        }
        true
    }

    /// Requested size of a live block.
    pub fn size(&self, cpu: &dyn Guest, handle: u32, addr: u32) -> Option<u32> {
        let handle = self.normalize(handle)?;
        let h = self.heaps.get(&handle)?;
        if let Some(bits) = h.small_header(cpu, handle, addr) {
            return (bits & (1 << 22) != 0).then_some(bits & 0xffff);
        }
        h.used.get(&addr).map(|&(_, s)| s)
    }

    /// Which heap a block belongs to (for LocalFree/GlobalFree style APIs without a handle).
    pub fn owner(&self, cpu: &dyn Guest, addr: u32) -> Option<u32> {
        self.heaps
            .iter()
            .find(|(heap, _)| self.size(cpu, **heap, addr).is_some())
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
        let handle = self.normalize(handle)?;
        let h = self.heap(handle)?;
        if let Some(bits) = h.small_header(cpu, handle, addr) {
            if bits & (1 << 22) == 0 {
                return None;
            }
            let old = bits & 0xffff;
            let capacity = (((bits >> 16) & 63) + 1) * 8;
            if size <= capacity {
                if zero && size > old {
                    zero_memory(cpu, addr + old, size - old).ok()?;
                }
                small::write_header(cpu, handle, addr, (bits & !0xffff) | size)?;
                return Some(addr);
            }
            if in_place_only {
                return None;
            }
            let new = self.move_block(cpu, vm, handle, addr, old, size, zero)?;
            if bits & (1 << 23) != 0 {
                self.account_small(cpu, new);
            }
            return Some(new);
        }
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
        self.move_block(cpu, vm, handle, addr, old, size, zero)
    }

    #[allow(clippy::too_many_arguments)]
    fn move_block(
        &mut self,
        cpu: &mut dyn Guest,
        vm: &mut Vm,
        handle: u32,
        addr: u32,
        old: u32,
        size: u32,
        zero: bool,
    ) -> Option<u32> {
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
                self.free(cpu, vm, handle, new);
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
    use adv_machine::layout::GRANULE;
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
            self.last_map = addr;
            self.memory.insert(addr, vec![0; len as usize]);
            Ok(())
        }
        fn protect(&mut self, _: u32, _: u32, _: Prot) -> Result<(), MemFault> {
            Ok(())
        }
        fn unmap(&mut self, addr: u32, _: u32) -> Result<(), MemFault> {
            self.memory.remove(&addr);
            Ok(())
        }
    }
    fn heap() -> (MemoryGuest, Vm, Heaps) {
        let (mut guest, mut vm, mut heaps) = (MemoryGuest::default(), Vm::new(), Heaps::default());
        heaps.process_heap = heaps.create(&mut guest, &mut vm).unwrap();
        (guest, vm, heaps)
    }

    #[test]
    fn large_zero_allocation_after_free_without_zero_memory() {
        let (mut guest, mut vm, mut heaps) = heap();
        let size = LARGE + 13;
        let block = heaps.alloc(&mut guest, &mut vm, 0, size, false).unwrap();
        guest.write(block, &vec![0xa5; size as usize]).unwrap();
        assert!(heaps.free(&mut guest, &mut vm, 0, block));
        let next = heaps.alloc(&mut guest, &mut vm, 0, size, false).unwrap();
        assert_eq!(heaps.size(&guest, 0, next), Some(size));
        assert!(
            guest
                .bytes(next, size as usize)
                .unwrap()
                .iter()
                .all(|&b| b == 0),
            "large allocations must retain fresh committed-page contents"
        );
    }

    #[test]
    fn large_zero_moving_realloc_preserves_data_and_zeroes_growth() {
        for zero in [false, true] {
            let (mut guest, mut vm, mut heaps) = heap();
            let old_size = LARGE + 1;
            let new_size = LARGE + GRANULE + 13;
            let old = heaps
                .alloc(&mut guest, &mut vm, 0, old_size, false)
                .unwrap();
            let pattern: Vec<_> = (0..old_size).map(|i| (i % 251) as u8).collect();
            guest.write(old, &pattern).unwrap();
            let spare = heaps
                .alloc(&mut guest, &mut vm, 0, new_size, false)
                .unwrap();
            guest.write(spare, &vec![0xa5; new_size as usize]).unwrap();
            assert!(heaps.free(&mut guest, &mut vm, 0, spare));
            let new = heaps
                .realloc(&mut guest, &mut vm, 0, old, new_size, zero, false)
                .unwrap();
            assert_ne!(new, old);
            assert_eq!(heaps.size(&guest, 0, old), None);
            assert_eq!(heaps.size(&guest, 0, new), Some(new_size));
            assert_eq!(guest.bytes(new, old_size as usize).unwrap(), pattern);
            assert!(
                guest
                    .bytes(new + old_size, (new_size - old_size) as usize)
                    .unwrap()
                    .iter()
                    .all(|&b| b == 0),
                "moving large realloc must expose zeroed growth even without ZERO_MEMORY"
            );
        }
    }

    #[test]
    fn large_mappings_are_zero_without_redundant_writes_and_retire_on_free() {
        for zero in [false, true] {
            let (mut guest, mut vm, mut heaps) = heap();
            let maps = guest.map_calls;
            let size = LARGE + 13;
            guest.fail_write_call = Some(1);
            for requested in [size, size + 9] {
                let block = heaps
                    .alloc(&mut guest, &mut vm, 0, requested, zero)
                    .unwrap();
                assert_eq!(guest.write_calls, 0, "a new mapping is already zeroed");
                assert!(
                    guest
                        .bytes(block, requested as usize)
                        .unwrap()
                        .iter()
                        .all(|&b| b == 0)
                );
                assert_eq!(heaps.size(&guest, 0, block), Some(requested));
                assert!(heaps.free(&mut guest, &mut vm, 0, block));
                assert_eq!(heaps.owner(&guest, block), None);
                assert_eq!(heaps.size(&guest, 0, block), None);
                assert_eq!(vm.region(block), None);
                assert!(!guest.memory.contains_key(&block));
                assert!(!heaps.free(&mut guest, &mut vm, 0, block));
            }
            assert_eq!(guest.map_calls, maps + 2);
        }
    }

    #[test]
    fn large_mapping_ownership_is_per_heap_and_destroy_retires_live_regions() {
        let (mut guest, mut vm, mut heaps) = heap();
        let other = heaps.create(&mut guest, &mut vm).unwrap();
        let first = heaps.alloc(&mut guest, &mut vm, 0, LARGE, false).unwrap();
        let live = heaps
            .alloc(&mut guest, &mut vm, 0, LARGE + 8, false)
            .unwrap();
        let foreign = heaps
            .alloc(&mut guest, &mut vm, other, LARGE, false)
            .unwrap();
        assert!(heaps.free(&mut guest, &mut vm, 0, first));
        assert_eq!(vm.region(first), None);
        assert!(!guest.memory.contains_key(&first));
        assert!(heaps.destroy(&mut guest, &mut vm, heaps.process_heap));
        assert_eq!(vm.region(live), None);
        assert!(!guest.memory.contains_key(&live));
        assert_eq!(vm.region(foreign), Some(LARGE));
        assert_eq!(heaps.owner(&guest, foreign), Some(other));
        assert!(heaps.destroy(&mut guest, &mut vm, other));
        assert_eq!(vm.region(foreign), None);
        assert!(guest.memory.is_empty());
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
        assert_eq!(heaps.size(&guest, 0, old), None);
        assert_eq!(heaps.size(&guest, 0, new), Some(21_000));
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
            assert_eq!(heaps.size(&guest, 0, old), Some(size));
            assert_eq!(guest.bytes(old, size as usize).unwrap(), pattern);
            assert_eq!(heaps.owner(&guest, failed_new), None);
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
        assert_eq!(heaps.size(&guest, 0, old), Some(8));
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
        assert_eq!(heaps.size(&guest, 0, old), Some(16));
        // New VM mappings cannot fail a redundant clear; exercise reused arena bytes.
        let spare = heaps.alloc(&mut guest, &mut vm, 0, 5000, false).unwrap();
        assert!(heaps.free(&mut guest, &mut vm, 0, spare));
        guest.reset_counts();
        guest.fail_write_call = Some(1);
        assert_eq!(heaps.alloc(&mut guest, &mut vm, 0, 5000, true), None);
        assert_eq!(heaps.owner(&guest, spare), None);
        assert_eq!(heaps.size(&guest, 0, old), Some(16));
        guest.fail_write_call = None;
        assert_eq!(heaps.alloc(&mut guest, &mut vm, 0, 5000, true), Some(spare));
        assert!(guest.bytes(spare, 5000).unwrap().iter().all(|&b| b == 0));
        assert!(guest.max_write <= 4096);
    }
}
