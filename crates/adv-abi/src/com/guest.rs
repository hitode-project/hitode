//! Guest counters retain one Objects owner until both public and internal
//! references vanish. The storage is worker-confined and survives guest unmap.
use super::*;
use adv_cpu::backend::{GuestExt, Prot};

pub use adv_machine::GuestWord as GuestReferences;

impl Com {
    /// A borrowed surface delegates public references to its controlling texture.
    /// Its own internal metadata ownership remains independent.
    pub fn alias_guest<T: Any>(
        &self,
        env: &mut Env<'_>,
        id: ComIdentity<T>,
        parent: ComIdentity<T>,
    ) {
        let Some(address) = self
            .object(&env.sys.objects, parent)
            .and_then(|o| o.guest.as_ref())
            .map(|c| c.address)
        else {
            return;
        };
        let pointers: Vec<_> = self
            .object(&env.sys.objects, id)
            .unwrap()
            .aliases
            .iter()
            .copied()
            .collect();
        for pointer in pointers {
            let _ = env.cpu.write_u32(pointer + 4, address);
        }
    }
    /// Interfaces reserve a zeroed second word for their shared counter address.
    /// Unsupported CPU backends keep ordinary vtable handlers via that zero word.
    pub fn enable_guest<T: Any>(&self, env: &mut Env<'_>, id: ComIdentity<T>) -> bool {
        let Some(object) = self.object(&env.sys.objects, id) else {
            return false;
        };
        if object.guest.is_some() {
            return true;
        }
        if self.references(&env.sys.objects, id) != Some(1)
            || env.sys.objects.reference_limit() != u32::MAX
        {
            return false;
        }
        let pointers: Vec<_> = object.aliases.iter().copied().collect();
        if pointers
            .iter()
            .any(|p| !env.cpu.accessible(p + 4, 4, Prot::WRITE))
        {
            return false;
        }
        let limit = u32::MAX - pointers.len() as u32 - object.internal;
        let separate: Vec<_> = object.separate.keys().copied().collect();
        let mut counters = Vec::new();
        for _ in 0..=separate.len() {
            let Some(counter) = env.sys.guest_words.allocate(env.cpu, &mut env.sys.vm) else {
                return false;
            };
            counter.set_limit(limit);
            counters.push(counter);
        }
        let shared = counters.pop().unwrap();
        shared.store(self.shared(&env.sys.objects, id).unwrap());
        for pointer in pointers {
            let address = separate
                .iter()
                .position(|&p| p == pointer)
                .map_or(shared.address, |at| counters[at].address);
            // All complete ranges were checked before any publication.
            env.cpu
                .write_u32(pointer + 4, address)
                .expect("validated COM header");
        }
        let object = env.sys.objects.owned_mut(id).unwrap();
        for (count, counter) in object.separate.values_mut().zip(counters) {
            counter.store(count.host);
            count.guest = Some(counter);
        }
        object.guest = Some(shared);
        true
    }
}
