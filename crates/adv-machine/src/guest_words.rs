//! Worker-confined words shared with authored guest code. Device callbacks
//! cannot receive the Rc storage. The VM retains mappings for process lifetime.
use crate::Vm;
use adv_cpu::backend::{CounterStorage, Guest};
use std::cell::RefCell;
use std::rc::Rc;

type Slot = (u32, usize, Rc<dyn CounterStorage>);
type FreeSlots = Rc<RefCell<Vec<Slot>>>;

pub struct GuestWord {
    pub address: u32,
    offset: usize,
    storage: Rc<dyn CounterStorage>,
    free: FreeSlots,
}
impl Drop for GuestWord {
    fn drop(&mut self) {
        self.free
            .borrow_mut()
            .push((self.address, self.offset, self.storage.clone()));
    }
}
impl GuestWord {
    pub fn load(&self) -> u32 {
        self.storage.load(self.offset)
    }
    pub fn store(&self, value: u32) {
        self.storage.store(self.offset, value);
    }
    pub fn limit(&self) -> u32 {
        self.storage.load(self.offset + 4)
    }
    pub fn set_limit(&self, value: u32) {
        self.storage.store(self.offset + 4, value);
    }
}

#[derive(Default)]
pub struct GuestWordPool {
    slab: Option<(u32, Rc<dyn CounterStorage>)>,
    next: usize,
    free: FreeSlots,
}
impl GuestWordPool {
    pub fn allocate(&mut self, cpu: &mut dyn Guest, vm: &mut Vm) -> Option<GuestWord> {
        const SIZE: u32 = 65536;
        if let Some((address, offset, storage)) = self.free.borrow_mut().pop() {
            let word = GuestWord {
                address,
                offset,
                storage,
                free: self.free.clone(),
            };
            word.store(0);
            word.set_limit(0);
            return Some(word);
        }
        if self.slab.is_none() || self.next == SIZE as usize {
            let address = vm.reserve(SIZE)?;
            let storage = match cpu.map_shared_counters(address, SIZE) {
                Ok(storage) => storage,
                Err(_) => {
                    vm.cancel_reservation(address);
                    return None;
                }
            };
            self.slab = Some((address, storage));
            self.next = 0;
        }
        let (base, storage) = self.slab.as_ref()?;
        let counter = GuestWord {
            address: *base + self.next as u32,
            offset: self.next,
            storage: storage.clone(),
            free: self.free.clone(),
        };
        self.next += 8;
        Some(counter)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Clock, Machine, layout::PARK_ADDR};
    use adv_cpu::{
        JitCpu,
        backend::{GuestExt, Prot},
    };
    struct Empty;
    impl crate::Resolver for Empty {
        fn provides(&self, _: &str) -> bool {
            false
        }
        fn host_fn(&self, _: &str, _: &str) -> Option<crate::HostFn> {
            None
        }
    }
    // spec: PROC-MACH-8
    #[test]
    fn counter_alias_survives_guest_unmap_and_audio_markers_stay_context_local() {
        let mut m = Machine::new(
            JitCpu::new(PARK_ADDR, false).unwrap(),
            Empty,
            Clock::virtual_time(),
        )
        .unwrap();
        let a = m.spawn("audio", PARK_ADDR, &[], 65536).unwrap();
        let b = m.spawn("other", PARK_ADDR, &[], 65536).unwrap();
        let address = m.sys.context(a).audio_guest.as_ref().unwrap().address;
        assert!(!m.sys.audio_called_after(a, 0));
        m.cpu.write_u32(address, 1).unwrap();
        assert!(m.sys.audio_called_after(a, u64::MAX));
        assert!(!m.sys.audio_called_after(b, 0));
        let word = m
            .sys
            .guest_words
            .allocate(&mut m.cpu, &mut m.sys.vm)
            .unwrap();
        word.store(123);
        assert_eq!(m.cpu.read_u32(word.address).unwrap(), 123);
        let base = word.address & !65535;
        m.cpu.unmap(base, 65536).unwrap();
        word.store(456);
        assert_eq!(word.load(), 456);
        assert!(!m.cpu.accessible(word.address, 4, Prot::READ));
    }

    // spec: PROC-MACH-8, PERS-SCOPE-4
    #[test]
    fn failed_counter_mapping_does_not_unmap_existing_cpu_storage() {
        let mut m = Machine::new(
            JitCpu::new(PARK_ADDR, false).unwrap(),
            Empty,
            Clock::virtual_time(),
        )
        .unwrap();
        let base = crate::layout::DYN_START;
        m.cpu.map(base, 65536, Prot::RW).unwrap();
        m.cpu.write_u32(base, 0x12345678).unwrap();
        assert!(
            m.sys
                .guest_words
                .allocate(&mut m.cpu, &mut m.sys.vm)
                .is_none()
        );
        assert_eq!(m.cpu.read_u32(base).unwrap(), 0x12345678);
        assert_eq!(m.sys.vm.region(base), None);
    }

    // spec: PROC-MACH-8, PERS-SCOPE-4
    #[test]
    fn dead_counter_slots_are_zeroed_and_reused_without_growing_the_mapping() {
        let mut m = Machine::new(
            JitCpu::new(PARK_ADDR, false).unwrap(),
            Empty,
            Clock::virtual_time(),
        )
        .unwrap();
        let word = m
            .sys
            .guest_words
            .allocate(&mut m.cpu, &mut m.sys.vm)
            .unwrap();
        let address = word.address;
        word.store(123);
        word.set_limit(456);
        drop(word);
        let reused = m
            .sys
            .guest_words
            .allocate(&mut m.cpu, &mut m.sys.vm)
            .unwrap();
        assert_eq!(reused.address, address);
        assert_eq!((reused.load(), reused.limit()), (0, 0));
        assert_eq!(m.cpu.read_u32(address).unwrap(), 0);
    }
}
