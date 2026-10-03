//! COM identity and interface aliases over the process's shared object table.
//!
//! Interface storage and vtables belong to guest memory and are owned by the
//! personality frontend. This module only associates initialized pointers with
//! IIDs and a typed payload. Anonymous references in `Objects` carry COM strong
//! ownership; interface aliases carry internal references until the last Release.
//! There is no second payload table or independent reference counter here.

use std::any::Any;
use std::collections::{BTreeMap, BTreeSet};

use adv_machine::{Identity, Namespace, Objects};

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
    /// Once queried, the observable interface set must remain static.
    sealed: bool,
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

/// The process's COM namespace. Keep this token in OS assembly state; all object
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
            sealed: false,
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
        if needs_alias && !objects.bind_alias(self.namespace, pointer, id) {
            return false;
        }
        let object = objects.owned_mut(id).unwrap();
        object.interfaces.insert(iid, pointer);
        object.aliases.insert(pointer);
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
        objects
            .strong_count(id)?
            .checked_sub(object.aliases.len().try_into().ok()?)
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
        objects
            .owned_mut(id)
            .ok_or(QueryError::InvalidObject)?
            .sealed = true;
        let target = *self
            .object(objects, id)
            .ok_or(QueryError::InvalidObject)?
            .interfaces
            .get(&iid)
            .ok_or(QueryError::NoInterface)?;
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
        if references != 0 {
            objects.release(id)?;
            return Some(Released {
                references,
                destroyed: None,
            });
        }
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
        Some(Released {
            references: 0,
            destroyed: Some(Destroyed {
                value: object.value,
                interfaces: pointers,
            }),
        })
    }
}
