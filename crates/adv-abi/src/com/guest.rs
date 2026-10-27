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
        let Some(counter) = env.sys.guest_words.allocate(env.cpu, &mut env.sys.vm) else {
            return false;
        };
        counter.store(1);
        counter.set_limit(
            u32::MAX
                - self.object(&env.sys.objects, id).unwrap().aliases.len() as u32
                - self.object(&env.sys.objects, id).unwrap().internal,
        );
        for pointer in pointers {
            // All complete ranges were checked before any publication.
            env.cpu
                .write_u32(pointer + 4, counter.address)
                .expect("validated COM header");
        }
        env.sys.objects.owned_mut(id).unwrap().guest = Some(counter);
        true
    }
}
