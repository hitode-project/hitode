//! COM identity and interface aliases over the process's shared object table.
//!
//! Interface storage and vtables belong to guest memory and are owned by the
//! personality frontend. This module only associates initialized pointers with
//! IIDs and a typed payload. Anonymous references in `Objects` carry COM strong
//! ownership; interface aliases carry internal references until the last Release.
//! Guest-counted objects retain one table owner and share their public count
//! with authored vtable code through execution-owner-only counter storage.

use std::any::Any;
use std::collections::{BTreeMap, BTreeSet};

use adv_machine::{Env, Identity, Namespace, Objects};

mod guest;
pub use guest::GuestReferences;

/// Bytes in the guest GUID's little-endian in-memory layout.
pub type Iid = [u8; 16];
pub const IUNKNOWN: Iid = [0, 0, 0, 0, 0, 0, 0, 0, 0xc0, 0, 0, 0, 0, 0, 0, 0x46];

pub type ComIdentity<T> = Identity<ComObject<T>>;

/// Typed payload and interface metadata stored in `adv_machine::Objects`.
pub struct ComObject<T: Any> {
    value: T,
    canonical: u32,
    interfaces: BTreeMap<Iid, u32>,
    aliases: BTreeSet<u32>,
    /// Device bindings retain storage without changing observable COM counts.
    internal: u32,
    /// Once queried, the observable interface set must remain static.
    sealed: bool,
    guest: Option<GuestReferences>,
}

/// The last Release returns the payload and interface allocations for cleanup.
/// Dropping the payload releases its owned device resources; the frontend frees
/// these guest pointers with the allocator which originally created them.
pub struct Destroyed<T> {
    pub value: T,
    pub interfaces: Vec<u32>,
}

pub struct Released<T> {
    pub references: u32,
    pub destroyed: Option<Destroyed<T>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueryError {
    InvalidObject,
    NoInterface,
    ReferenceLimit,
}

/// The process's COM namespace. Keep this token in process assembly state; all object
/// storage and actual ownership remain in the machine's common object table.
#[derive(Clone, Copy, Debug)]
pub struct Com {
    namespace: Namespace,
}

impl Com {
    pub fn new(objects: &mut Objects) -> Option<Self> {
        Some(Self {
            namespace: objects.new_namespace()?,
        })
    }

    pub fn namespace(&self) -> Namespace {
        self.namespace
    }

    /// Associate a fully initialized guest interface with a new payload and one
    /// visible COM reference. On failure the frontend must free `pointer` itself.
    /// No caller may publish that pointer before this returns successfully.
    pub fn create<T: Any>(
        &self,
        objects: &mut Objects,
        pointer: u32,
        iid: Iid,
        value: T,
    ) -> Option<ComIdentity<T>> {
        if pointer == 0 || pointer & 3 != 0 {
            return None;
        }
        let id = objects.insert_owned(ComObject {
            value,
            canonical: pointer,
            interfaces: BTreeMap::from([(IUNKNOWN, pointer), (iid, pointer)]),
            aliases: BTreeSet::from([pointer]),
            internal: 0,
            sealed: false,
            guest: None,
        })?;
        if !objects.bind_alias(self.namespace, pointer, id) {
            // The initial owner was never published and must not be leaked.
            objects.release(id);
            return None;
        }
        Some(id)
    }

    pub fn identity<T: Any>(&self, objects: &Objects, pointer: u32) -> Option<ComIdentity<T>> {
        objects.lookup_alias(self.namespace, pointer)
    }

    /// Borrow the canonical guest interface without acquiring a COM reference.
    pub fn canonical<T: Any>(&self, objects: &Objects, id: ComIdentity<T>) -> Option<u32> {
        Some(self.object(objects, id)?.canonical)
    }

    fn object<'a, T: Any>(
        &self,
        objects: &'a Objects,
        id: ComIdentity<T>,
    ) -> Option<&'a ComObject<T>> {
        let object = objects.owned(id)?;
        (self.identity::<T>(objects, object.canonical) == Some(id)).then_some(object)
    }

    pub fn get<'a, T: Any>(&self, objects: &'a Objects, pointer: u32) -> Option<&'a T> {
        let id = self.identity::<T>(objects, pointer)?;
        Some(&objects.owned(id)?.value)
    }

    pub fn get_mut<'a, T: Any>(&self, objects: &'a mut Objects, pointer: u32) -> Option<&'a mut T> {
        let id = self.identity::<T>(objects, pointer)?;
        Some(&mut objects.owned_mut(id)?.value)
    }

    pub fn get_identity<'a, T: Any>(
        &self,
        objects: &'a Objects,
        id: ComIdentity<T>,
    ) -> Option<&'a T> {
        Some(&self.object(objects, id)?.value)
    }

    pub fn get_identity_mut<'a, T: Any>(
        &self,
        objects: &'a mut Objects,
        id: ComIdentity<T>,
    ) -> Option<&'a mut T> {
        self.object(objects, id)?;
        Some(&mut objects.owned_mut(id)?.value)
    }

    /// Declare an additional initialized interface, without acquiring a client
    /// reference. Multiple IIDs may share one pointer. IUnknown is always canonical.
    pub fn register_interface<T: Any>(
        &self,
        objects: &mut Objects,
        id: ComIdentity<T>,
        iid: Iid,
        pointer: u32,
    ) -> bool {
        if pointer == 0 || pointer & 3 != 0 {
            return false;
        }
        let Some(object) = self.object(objects, id) else {
            return false;
        };
        if let Some(existing) = object.interfaces.get(&iid) {
            return *existing == pointer;
        }
        if object.sealed {
            return false;
        }
        let needs_alias = !object.aliases.contains(&pointer);
        if needs_alias && object.guest.as_ref().is_some_and(|c| c.load() >= c.limit()) {
            return false;
        }
        if needs_alias && !objects.bind_alias(self.namespace, pointer, id) {
            return false;
        }
        let object = objects.owned_mut(id).unwrap();
        object.interfaces.insert(iid, pointer);
        object.aliases.insert(pointer);
        if needs_alias && let Some(counter) = &object.guest {
            counter.set_limit(counter.limit() - 1);
        }
        true
    }

    pub fn references<T: Any>(&self, objects: &Objects, id: ComIdentity<T>) -> Option<u32> {
        let object = self.object(objects, id)?;
        if object
            .aliases
            .iter()
            .any(|&pointer| self.identity::<T>(objects, pointer) != Some(id))
        {
            return None;
        }
        if let Some(counter) = &object.guest {
            return Some(counter.load());
        }
        objects
            .strong_count(id)?
            .checked_sub(object.aliases.len().try_into().ok()?)?
            .checked_sub(object.internal)
    }

    /// A device/state binding owns the payload, but is not an AddRef visible to
    /// the guest. A zero-public-reference object can remain bound and later be
    /// returned by a getter. Frontends manage parent ownership on that 0/1 edge.
    pub fn retain_internal<T: Any>(&self, objects: &mut Objects, id: ComIdentity<T>) -> bool {
        let Some(internal) = self
            .object(objects, id)
            .and_then(|o| o.internal.checked_add(1))
        else {
            return false;
        };
        if self
            .object(objects, id)
            .and_then(|o| o.guest.as_ref())
            .is_some_and(|c| c.load() >= c.limit())
        {
            return false;
        }
        if !objects.retain(id) {
            return false;
        }
        objects.owned_mut(id).unwrap().internal = internal;
        if let Some(counter) = &self.object(objects, id).unwrap().guest {
            counter.set_limit(counter.limit() - 1);
        }
        true
    }

    /// Release a binding. `None` is invalid ownership; `Some(None)` keeps a
    /// live payload, and `Some(Some(..))` transfers final cleanup to the caller.
    pub fn release_internal<T: Any>(
        &self,
        objects: &mut Objects,
        id: ComIdentity<T>,
    ) -> Option<Option<Destroyed<T>>> {
        let internal = self.object(objects, id)?.internal.checked_sub(1)?;
        let references = self.references(objects, id)?;
        if internal == 0 && references == 0 {
            if self.object(objects, id)?.guest.is_some() {
                objects.owned_mut(id)?.internal = 0;
                objects.release(id)?;
            }
            return Some(Some(self.destroy(objects, id)?));
        }
        objects.owned_mut(id)?.internal = internal;
        if let Some(counter) = &self.object(objects, id)?.guest {
            counter.set_limit(counter.limit() + 1);
        }
        objects.release(id)?;
        Some(None)
    }

    /// Resolve and seal the declared interface set without taking ownership.
    /// Aggregated frontend interfaces can then retain their controlling object
    /// using the same object table. The returned pointer is only borrowed.
    pub fn resolve_interface<T: Any>(
        &self,
        objects: &mut Objects,
        pointer: u32,
        iid: Iid,
    ) -> Result<u32, QueryError> {
        let id = self
            .identity::<T>(objects, pointer)
            .ok_or(QueryError::InvalidObject)?;
        objects
            .owned_mut(id)
            .ok_or(QueryError::InvalidObject)?
            .sealed = true;
        self.object(objects, id)
            .ok_or(QueryError::InvalidObject)?
            .interfaces
            .get(&iid)
            .copied()
            .ok_or(QueryError::NoInterface)
    }

    /// QueryInterface succeeds only for declared IIDs and retains exactly once.
    /// Unsupported IIDs and exhausted limits do not change reference ownership.
    /// Distinct errors let the frontend return E_NOINTERFACE or E_OUTOFMEMORY.
    pub fn query<T: Any>(
        &self,
        objects: &mut Objects,
        pointer: u32,
        iid: Iid,
    ) -> Result<u32, QueryError> {
        let id = self
            .identity::<T>(objects, pointer)
            .ok_or(QueryError::InvalidObject)?;
        let target = self.resolve_interface::<T>(objects, pointer, iid)?;
        self.add_ref_identity(objects, id)
            .ok_or(QueryError::ReferenceLimit)?;
        Ok(target)
    }

    pub fn add_ref<T: Any>(&self, objects: &mut Objects, pointer: u32) -> Option<u32> {
        self.add_ref_identity(objects, self.identity::<T>(objects, pointer)?)
    }

    pub fn add_ref_identity<T: Any>(
        &self,
        objects: &mut Objects,
        id: ComIdentity<T>,
    ) -> Option<u32> {
        let references = self.references(objects, id)?.checked_add(1)?;
        if let Some(counter) = &self.object(objects, id)?.guest {
            if references > counter.limit() {
                return None;
            }
            counter.store(references);
            return Some(references);
        }
        objects.retain(id).then_some(references)
    }

    pub fn release<T: Any>(&self, objects: &mut Objects, pointer: u32) -> Option<Released<T>> {
        self.release_identity(objects, self.identity::<T>(objects, pointer)?)
    }

    pub fn release_identity<T: Any>(
        &self,
        objects: &mut Objects,
        id: ComIdentity<T>,
    ) -> Option<Released<T>> {
        let references = self.references(objects, id)?.checked_sub(1)?;
        let guest = self.object(objects, id)?.guest.as_ref();
        if let Some(counter) = guest {
            counter.store(references);
        }
        if references != 0 || self.object(objects, id)?.internal != 0 {
            if guest.is_none() {
                objects.release(id)?;
            }
            return Some(Released {
                references,
                destroyed: None,
            });
        }
        Some(Released {
            references: 0,
            destroyed: Some(self.destroy(objects, id)?),
        })
    }

    fn destroy<T: Any>(&self, objects: &mut Objects, id: ComIdentity<T>) -> Option<Destroyed<T>> {
        // Preflight before removing any alias, including metadata corruption
        // caused by another layer closing the COM namespace directly.
        let pointers: Vec<_> = self.object(objects, id)?.aliases.iter().copied().collect();
        if pointers
            .iter()
            .any(|&ptr| self.identity::<T>(objects, ptr) != Some(id))
        {
            return None;
        }
        for &pointer in &pointers {
            objects.close_alias(self.namespace, pointer)?;
        }
        let object = objects.release(id)??;
        Some(Destroyed {
            value: object.value,
            interfaces: pointers,
        })
    }
}
