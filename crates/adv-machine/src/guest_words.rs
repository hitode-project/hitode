//! Worker-confined words shared with authored guest code. Device callbacks
//! cannot receive the Rc storage. The VM retains mappings for process lifetime.
use crate::Vm;
use adv_cpu::backend::{CounterStorage, Guest};
use std::rc::Rc;

pub struct GuestWord {
    pub address: u32,
    offset: usize,
    storage: Rc<dyn CounterStorage>,
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
}
impl GuestWordPool {
    pub fn allocate(&mut self, cpu: &mut dyn Guest, vm: &mut Vm) -> Option<GuestWord> {
        const SIZE: u32 = 65536;
        if self.slab.is_none() || self.next == SIZE as usize {
            let address = vm.reserve(SIZE)?;
            let storage = match cpu.map_shared_counters(address, SIZE) {
                Ok(storage) => storage,
                Err(_) => {
                    let _ = vm.free(cpu, address);
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
}
