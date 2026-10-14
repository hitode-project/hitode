//! Heap inspection uses the same validated headers as allocation and release.
use super::*;
use adv_cpu::backend::GuestExt;

impl Heaps {
    /// Validate a busy block, or the complete heap when `addr` is zero.
    pub fn validate(&self, cpu: &dyn Guest, handle: u32, addr: u32) -> bool {
        let Some(handle) = self.normalize(handle) else {
            return false;
        };
        if addr != 0 {
            return self.size(cpu, handle, addr).is_some();
        }
        self.entries(cpu, handle).is_some()
    }
    fn entries(&self, cpu: &dyn Guest, heap: u32) -> Option<Vec<(u32, u32, u8, bool)>> {
        let h = self.heaps.get(&heap)?;
        let mut entries: Vec<_> = h
            .used
            .iter()
            .map(|(&p, &(capacity, _))| (p, capacity, 0, true))
            .collect();
        entries.extend(h.free_by_addr.iter().map(|(&p, &size)| (p, size, 0, false)));
        if let Some(slabs) = &h.slabs {
            if cpu.read_u32(heap + 0x100).ok()? != small::COOKIE
                || cpu.read_u32(heap + 0x10c).ok()? != slabs.len() as u32
            {
                return None;
            }
            let mut free = BTreeSet::new();
            for (i, slab) in slabs.iter().enumerate() {
                for (j, value) in [slab.base, slab.end, slab.stride(), slab.class << 16]
                    .into_iter()
                    .enumerate()
                {
                    if cpu
                        .read_u32(heap + small::RANGES + i as u32 * 16 + j as u32 * 4)
                        .ok()?
                        != value
                    {
                        return None;
                    }
                }
                for addr in (slab.base + 8..slab.end).step_by(slab.stride() as usize) {
                    let bits = small::header(cpu, heap, *slab, addr)?;
                    let busy = bits & small::BUSY != 0;
                    if !busy {
                        free.insert(addr);
                    }
                    entries.push((addr, slab.stride() - 8, 8, busy));
                }
            }
            for class in 0..64 {
                let mut p = cpu.read_u32(heap + small::HEADS + class * 8).ok()?;
                while p != 0 {
                    if !free.remove(&p) || h.small_header(cpu, heap, p)? != class << 16 {
                        return None;
                    }
                    p = cpu.read_u32(p + 4).ok()?;
                }
            }
            if !free.is_empty() {
                return None;
            }
        }
        entries.sort_unstable_by_key(|e| e.0);
        Some(entries)
    }
    /// Encode the next x86 PROCESS_HEAP_ENTRY, including arena and free entries.
    pub fn walk(
        &self,
        cpu: &dyn Guest,
        vm: &Vm,
        handle: u32,
        after: u32,
        after_flags: u16,
    ) -> Result<Option<[u8; 28]>, ()> {
        let heap = self.normalize(handle).ok_or(())?;
        let h = &self.heaps[&heap];
        let entries = self.entries(cpu, heap).ok_or(())?;
        let mut rows = Vec::new();
        for (index, &base) in h.arenas.iter().enumerate() {
            let size = vm.region(base).ok_or(())?;
            let first = entries
                .iter()
                .find(|e| e.0 >= base && e.0 < base + size)
                .map_or(base + size, |e| e.0);
            let mut row = [0; 28];
            row[..4].copy_from_slice(&base.to_le_bytes());
            row[4..8].copy_from_slice(&size.to_le_bytes());
            row[9] = index as u8;
            row[10] = 1;
            row[12..16].copy_from_slice(&size.to_le_bytes());
            row[20..24].copy_from_slice(&first.to_le_bytes());
            row[24..28].copy_from_slice(&(base + size).to_le_bytes());
            rows.push((base, row));
        }
        let mut large: Vec<_> = h.large.keys().copied().collect();
        large.sort_unstable();
        for (addr, size, overhead, busy) in entries {
            let mut row = [0; 28];
            row[..4].copy_from_slice(&addr.to_le_bytes());
            row[4..8].copy_from_slice(&size.to_le_bytes());
            row[8] = overhead;
            row[9] = h
                .arenas
                .iter()
                .position(|&base| addr >= base && addr < base + vm.region(base).unwrap_or(0))
                .unwrap_or_else(|| {
                    h.arenas.len() + large.iter().position(|&p| p == addr).unwrap_or(0)
                }) as u8;
            row[10] = if busy { 4 } else { 0 };
            rows.push((addr, row));
        }
        rows.sort_by_key(|e| e.0);
        let index = if after == 0 {
            0
        } else {
            rows.iter()
                .position(|e| e.0 == after && (e.1[10] & 1) == (after_flags as u8 & 1))
                .ok_or(())?
                + 1
        };
        Ok(rows.get(index).map(|e| e.1))
    }
}
