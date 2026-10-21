//! Enabled heap slabs share guest headers and generation-tagged free lists.
//! Host slab registration bounds every header read; corrupt metadata is invalid.
//! Free links use body offset four, preserving retired COM interface vtable words.
use super::*;
use adv_cpu::backend::GuestExt;

pub(super) const COOKIE: u32 = 0x4845_4150;
pub(super) const BUSY: u32 = 1 << 22;
const ACCOUNTED: u32 = 1 << 23;
pub(super) const HEADS: u32 = 0x110;
pub(super) const RANGES: u32 = 0x310;
const SLAB: u32 = 0x4000;
const MAX_SLAB: u32 = 16 * 1024 * 1024;
const LIMIT: usize = 128;
const REGISTRY_GATE: u32 = 0xb10;
const REGISTRY_ACTIVE: u32 = 0xb14;
pub(super) const LAST_SLAB: u32 = 0xb18;
const REGISTRY: u32 = 0xb20;
const HEAPS: u32 = 16;

#[derive(Clone, Copy)]
pub(super) struct Slab {
    pub(super) base: u32,
    pub(super) end: u32,
    pub(super) class: u32,
}
impl Slab {
    pub(super) fn stride(self) -> u32 {
        (self.class + 1) * 8 + 8
    }
    fn contains(self, addr: u32) -> bool {
        addr >= self.base + 8
            && addr < self.end
            && (addr - self.base - 8).is_multiple_of(self.stride())
    }
}
pub(super) fn header(cpu: &dyn Guest, heap: u32, slab: Slab, addr: u32) -> Option<u32> {
    if !slab.contains(addr) {
        return None;
    }
    let bits = cpu.read_u32(addr - 8).ok()?;
    let check = cpu.read_u32(addr - 4).ok()?;
    if check != (addr - 8) ^ heap ^ bits ^ COOKIE
        || bits >> 24 != 0
        || (bits >> 16) & 63 != slab.class
    {
        return None;
    }
    let size = bits & 0xffff;
    ((bits & BUSY != 0 && size <= (slab.class + 1) * 8)
        || (bits & (BUSY | ACCOUNTED | 0xffff) == 0))
        .then_some(bits)
}
pub(super) fn write_header(cpu: &mut dyn Guest, heap: u32, addr: u32, bits: u32) -> Option<()> {
    let check = (addr - 8) ^ heap ^ bits ^ COOKIE;
    cpu.write(
        addr - 8,
        &((u64::from(check) << 32) | u64::from(bits)).to_le_bytes(),
    )
    .ok()
}
fn publish(cpu: &mut dyn Guest, head: u32, next: u32) -> Option<()> {
    let generation = cpu.read_u32(head + 4).ok()?.wrapping_add(1);
    cpu.write(
        head,
        &((u64::from(generation) << 32) | u64::from(next)).to_le_bytes(),
    )
    .ok()
}
impl Heap {
    pub(super) fn small_header(&self, cpu: &dyn Guest, heap: u32, addr: u32) -> Option<u32> {
        let slab = self.slabs.as_ref()?.iter().find(|s| s.contains(addr))?;
        header(cpu, heap, *slab, addr)
    }
    pub(super) fn small_alloc(
        &mut self,
        cpu: &mut dyn Guest,
        vm: &mut Vm,
        heap: u32,
        size: u32,
        zero: bool,
    ) -> Option<u32> {
        let class = (size - 1) / 8;
        let head = heap + HEADS + class * 8;
        let mut addr = cpu.read_u32(head).ok()?;
        if addr == 0 {
            let count = self.slabs.as_ref()?.len();
            if count == LIMIT {
                return None;
            }
            // Grow only exhausted classes; short-lived classes keep their first
            // small slab. A replenishment covers at least an eighth of all slab
            // memory, so the bounded range table fills only logarithmically.
            let slabs = self.slabs.as_ref()?;
            let total: u32 = slabs.iter().map(|slab| slab.end - slab.base).sum();
            let capacity =
                slabs
                    .iter()
                    .rev()
                    .find(|slab| slab.class == class)
                    .map_or(SLAB, |slab| {
                        ((slab.end - slab.base).next_power_of_two() * 4)
                            .max((total / 8).next_power_of_two())
                            .min(MAX_SLAB)
                    });
            if self.free_by_size.range((capacity, 0)..).next().is_none() {
                self.grow(cpu, vm, capacity).ok()?;
            }
            let stride = (class + 1) * 8 + 8;
            let length = capacity / stride * stride;
            let base = self.take_free(length)?;
            let slab = Slab {
                base,
                end: base + length,
                class,
            };
            let mut bytes = vec![0u8; length as usize];
            for offset in (0..slab.end - base).step_by(stride as usize) {
                let p = base + offset;
                let words = [
                    class << 16,
                    p ^ heap ^ (class << 16) ^ COOKIE,
                    0,
                    if p + stride < slab.end {
                        p + stride + 8
                    } else {
                        0
                    },
                ];
                for (i, word) in words.into_iter().enumerate() {
                    bytes[offset as usize + i * 4..offset as usize + i * 4 + 4]
                        .copy_from_slice(&word.to_le_bytes());
                }
            }
            if cpu.write(base, &bytes).is_err() {
                self.insert_free(base, length);
                return None;
            }
            self.slabs.as_mut()?.push(slab);
            for (i, value) in [base, slab.end, stride, class << 16]
                .into_iter()
                .enumerate()
            {
                cpu.write_u32(heap + RANGES + count as u32 * 16 + i as u32 * 4, value)
                    .ok()?;
            }
            cpu.write_u32(heap + 0x10c, count as u32 + 1).ok()?;
            cpu.write_u32(heap + LAST_SLAB, count as u32).ok()?;
            addr = base + 8;
            publish(cpu, head, addr)?;
        }
        let bits = self.small_header(cpu, heap, addr)?;
        if bits != class << 16 {
            return None;
        }
        let next = cpu.read_u32(addr + 4).ok()?;
        if next != 0 && self.small_header(cpu, heap, next)? != bits {
            return None;
        }
        if zero {
            zero_memory(cpu, addr, (class + 1) * 8).ok()?;
        }
        write_header(cpu, heap, addr, bits | BUSY | size)?;
        publish(cpu, head, next)?;
        Some(addr)
    }
    pub(super) fn small_free(&mut self, cpu: &mut dyn Guest, heap: u32, addr: u32) -> Option<bool> {
        let bits = self.small_header(cpu, heap, addr)?;
        if bits & BUSY == 0 {
            return Some(false);
        }
        let class = (bits >> 16) & 63;
        let head = heap + HEADS + class * 8;
        let next = cpu.read_u32(head).ok()?;
        if next != 0 && self.small_header(cpu, heap, next)? != class << 16 {
            return Some(false);
        }
        cpu.write_u32(addr + 4, next).ok()?;
        write_header(cpu, heap, addr, class << 16)?;
        publish(cpu, head, addr)?;
        Some(true)
    }
}
impl Heaps {
    /// Initialize the process-owned registry before guest execution.
    pub fn enable_small(&mut self, cpu: &mut dyn Guest) -> Result<(), MemFault> {
        cpu.write(self.process_heap + REGISTRY_GATE, &[0; 0x50])?;
        self.enable_small_heap(cpu, self.process_heap)?;
        Ok(())
    }
    /// Register an eligible heap only after its complete control page is initialized.
    pub fn enable_small_heap(&mut self, cpu: &mut dyn Guest, heap: u32) -> Result<bool, MemFault> {
        if !self.heaps.contains_key(&self.process_heap) {
            return Ok(false);
        }
        for i in 0..HEAPS {
            let slot = self.process_heap + REGISTRY + i * 4;
            if cpu.read_u32(slot)? != 0 {
                continue;
            }
            cpu.write(heap + 0x100, &[0; 0xa10])?;
            cpu.write_u32(heap + 0x100, COOKIE)?;
            cpu.write_u32(heap + LAST_SLAB, 0)?;
            cpu.write_u32(slot, heap)?;
            self.heaps.get_mut(&heap).expect("created heap").slabs = Some(Vec::new());
            return Ok(true);
        }
        Ok(false)
    }
    /// Protect lookups that have not yet reached the target heap's operation count.
    pub fn gate_small_registry(&self, cpu: &mut dyn Guest, hold: bool) -> Result<(), MemFault> {
        if self
            .heaps
            .get(&self.process_heap)
            .is_some_and(|h| h.slabs.is_some())
        {
            let gate = self.process_heap + REGISTRY_GATE;
            let n = cpu.read_u32(gate)?;
            cpu.write_u32(
                gate,
                if hold {
                    n.wrapping_add(1)
                } else {
                    n.wrapping_sub(1)
                },
            )?;
        }
        Ok(())
    }
    pub fn small_registry_active(&self, cpu: &dyn Guest) -> u32 {
        if self
            .heaps
            .get(&self.process_heap)
            .is_some_and(|h| h.slabs.is_some())
        {
            cpu.read_u32(self.process_heap + REGISTRY_ACTIVE)
                .unwrap_or(0)
        } else {
            0
        }
    }
    pub(super) fn unregister_small(&self, cpu: &mut dyn Guest, heap: u32) -> Result<(), MemFault> {
        if self
            .heaps
            .get(&self.process_heap)
            .is_some_and(|h| h.slabs.is_some())
        {
            for i in 0..HEAPS {
                let slot = self.process_heap + REGISTRY + i * 4;
                if cpu.read_u32(slot)? == heap {
                    cpu.write_u32(slot, 0)?;
                    break;
                }
            }
        }
        Ok(())
    }
    /// Prevent new guest operations before installing a persistent heap lock.
    pub fn gate_small(&self, cpu: &mut dyn Guest, heap: u32, locked: bool) -> Result<(), MemFault> {
        if self.heaps.get(&heap).is_some_and(|h| h.slabs.is_some()) {
            cpu.write_u32(
                heap + 0x104,
                u32::from(locked || self.heaps[&heap].synchronization.lock.is_some()),
            )?;
        }
        Ok(())
    }
    pub fn small_active(&self, cpu: &dyn Guest, heap: u32) -> u32 {
        if self.heaps.get(&heap).is_some_and(|h| h.slabs.is_some()) {
            cpu.read_u32(heap + 0x108).unwrap_or(0)
        } else {
            0
        }
    }
    /// Global/Local allocations require host bookkeeping before recycling.
    pub fn account_small(&self, cpu: &mut dyn Guest, addr: u32) {
        let Some(heap) = self.owner(cpu, addr) else {
            return;
        };
        if let Some(bits) = self
            .heaps
            .get(&heap)
            .and_then(|h| h.small_header(cpu, heap, addr))
            .filter(|b| b & BUSY != 0)
        {
            let _ = write_header(cpu, heap, addr, bits | ACCOUNTED);
        }
    }
}
