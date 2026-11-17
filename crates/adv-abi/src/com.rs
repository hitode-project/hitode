//! COM identity and interface aliases over the process's shared object table.
//!
//! Interface storage and vtables belong to guest memory and are owned by the
//! personality frontend. This module only associates initialized pointers with
//! IIDs and a typed payload. Anonymous references in `Objects` carry COM strong
//! ownership; interface aliases carry internal references until the last Release.
//! Guest-counted objects retain one table owner and share their public count
//! with authored vtable code through execution-owner-only counter storage.
//! Interfaces registered as separately counted keep their own public count,
//! as some Windows runtimes do per interface; the object lives while any
//! count or internal reference remains.

use std::any::Any;
use std::collections::{BTreeMap, BTreeSet};

use adv_machine::{Env, Identity, Namespace, Objects};

mod guest;
pub use guest::GuestReferences;

/// Bytes in the guest GUID's little-endian in-memory layout.
pub type Iid = [u8; 16];
pub const IUNKNOWN: Iid = [0, 0, 0, 0, 0, 0, 0, 0, 0xc0, 0, 0, 0, 0, 0, 0, 0x46];

/// A GUID in registry form, `{xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx}`, as
/// CLSIDFromString reads it.
pub fn parse_guid(text: &str) -> Option<Iid> {
    let inner = text.strip_prefix('{')?.strip_suffix('}')?;
    let parts: Vec<&str> = inner.split('-').collect();
    if parts.iter().map(|p| p.len()).ne([8, 4, 4, 4, 12])
        || !inner.chars().all(|c| c == '-' || c.is_ascii_hexdigit())
    {
        return None;
    }
    let mut guid = [0; 16];
    guid[..4].copy_from_slice(&u32::from_str_radix(parts[0], 16).ok()?.to_le_bytes());
    guid[4..6].copy_from_slice(&u16::from_str_radix(parts[1], 16).ok()?.to_le_bytes());
    guid[6..8].copy_from_slice(&u16::from_str_radix(parts[2], 16).ok()?.to_le_bytes());
    let tail = [parts[3], parts[4]].concat();
    for (i, byte) in guid[8..].iter_mut().enumerate() {
        *byte = u8::from_str_radix(&tail[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(guid)
}

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
    /// Separately counted interface pointers. Every other alias shares the
    /// object's count.
    separate: BTreeMap<u32, Count>,
}

/// One separately counted interface: a host count until guest counting.
#[derive(Default)]
struct Count {
    host: u32,
    guest: Option<GuestReferences>,
}

impl Count {
    fn load(&self) -> u32 {
        self.guest.as_ref().map_or(self.host, GuestReferences::load)
    }
}

impl<T: Any> ComObject<T> {
    fn counters(&self) -> impl Iterator<Item = &GuestReferences> {
        self.guest
            .iter()
            .chain(self.separate.values().filter_map(|c| c.guest.as_ref()))
    }
    fn at_limit(&self) -> bool {
        self.counters().any(|c| c.load() >= c.limit())
    }
    fn adjust_limits(&self, by: i64) {
        for counter in self.counters() {
            counter.set_limit((i64::from(counter.limit()) + by) as u32);
        }
    }
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
            separate: BTreeMap::new(),
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
        objects
            .aliases(self.namespace, object.canonical, id)
            .then_some(object)
    }

    pub fn get<'a, T: Any>(&self, objects: &'a Objects, pointer: u32) -> Option<&'a T> {
        let (_, object) = objects.resolve_alias::<ComObject<T>>(self.namespace, pointer)?;
        Some(&object.value)
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
    /// reference. Multiple IIDs may share one pointer. IUnknown is canonical
    /// unless [`Self::register_separate`] moves it.
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
        if needs_alias && object.at_limit() {
            return false;
        }
        if needs_alias && !objects.bind_alias(self.namespace, pointer, id) {
            return false;
        }
        let object = objects.owned_mut(id).unwrap();
        object.interfaces.insert(iid, pointer);
        object.aliases.insert(pointer);
        if needs_alias {
            object.adjust_limits(-1);
        }
        true
    }

    /// Declare an interface whose pointer keeps its own public count, starting
    /// at zero. IIDs later registered on the same pointer share that count.
    /// IUnknown may move off the canonical pointer this way. Call before
    /// guest counting and before the pointer is published.
    pub fn register_separate<T: Any>(
        &self,
        objects: &mut Objects,
        id: ComIdentity<T>,
        iid: Iid,
        pointer: u32,
    ) -> bool {
        let Some(object) = self.object(objects, id) else {
            return false;
        };
        let canonical = object.canonical;
        if pointer == canonical || object.guest.is_some() {
            return false;
        }
        let moved = iid == IUNKNOWN
            && !object.sealed
            && object.interfaces.get(&IUNKNOWN) == Some(&canonical);
        if moved {
            objects.owned_mut(id).unwrap().interfaces.remove(&IUNKNOWN);
        }
        if !self.register_interface(objects, id, iid, pointer) {
            if moved {
                objects
                    .owned_mut(id)
                    .unwrap()
                    .interfaces
                    .insert(IUNKNOWN, canonical);
            }
            return false;
        }
        objects
            .owned_mut(id)
            .unwrap()
            .separate
            .entry(pointer)
            .or_default();
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
            return object
                .separate
                .values()
                .try_fold(counter.load(), |sum, c| sum.checked_add(c.load()));
        }
        objects
            .strong_count(id)?
            .checked_sub(object.aliases.len().try_into().ok()?)?
            .checked_sub(object.internal)
    }

    /// The count shared by every interface not counted separately.
    pub(crate) fn shared<T: Any>(&self, objects: &Objects, id: ComIdentity<T>) -> Option<u32> {
        let object = self.object(objects, id)?;
        if let Some(counter) = &object.guest {
            return Some(counter.load());
        }
        object
            .separate
            .values()
            .try_fold(self.references(objects, id)?, |rest, c| {
                rest.checked_sub(c.host)
            })
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
        if self.object(objects, id).is_some_and(ComObject::at_limit) {
            return false;
        }
        if !objects.retain(id) {
            return false;
        }
        objects.owned_mut(id).unwrap().internal = internal;
        self.object(objects, id).unwrap().adjust_limits(-1);
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
        self.object(objects, id)?.adjust_limits(1);
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
        let target = self.resolve_interface::<T>(objects, pointer, iid)?;
        self.add_ref::<T>(objects, target)
            .ok_or(QueryError::ReferenceLimit)?;
        Ok(target)
    }

    /// Retain the count `pointer` belongs to and return that count.
    pub fn add_ref<T: Any>(&self, objects: &mut Objects, pointer: u32) -> Option<u32> {
        let id = self.identity::<T>(objects, pointer)?;
        let Some(count) = self.object(objects, id)?.separate.get(&pointer) else {
            return self.add_ref_identity(objects, id);
        };
        let references = count.load().checked_add(1)?;
        if let Some(counter) = &count.guest {
            if references > counter.limit() {
                return None;
            }
            counter.store(references);
            return Some(references);
        }
        if !objects.retain(id) {
            return None;
        }
        objects.owned_mut(id)?.separate.get_mut(&pointer)?.host = references;
        Some(references)
    }

    pub fn add_ref_identity<T: Any>(
        &self,
        objects: &mut Objects,
        id: ComIdentity<T>,
    ) -> Option<u32> {
        let references = self.shared(objects, id)?.checked_add(1)?;
        if let Some(counter) = &self.object(objects, id)?.guest {
            if references > counter.limit() {
                return None;
            }
            counter.store(references);
            return Some(references);
        }
        objects.retain(id).then_some(references)
    }

    /// Release the count `pointer` belongs to. `references` is that count.
    pub fn release<T: Any>(&self, objects: &mut Objects, pointer: u32) -> Option<Released<T>> {
        let id = self.identity::<T>(objects, pointer)?;
        let total = self.references(objects, id)?;
        let Some(count) = self.object(objects, id)?.separate.get(&pointer) else {
            return self.release_identity(objects, id);
        };
        let references = count.load().checked_sub(1)?;
        let guest = count.guest.is_some();
        match &count.guest {
            Some(counter) => counter.store(references),
            None => objects.owned_mut(id)?.separate.get_mut(&pointer)?.host = references,
        }
        self.released(objects, id, total - 1, references, guest)
    }

    pub fn release_identity<T: Any>(
        &self,
        objects: &mut Objects,
        id: ComIdentity<T>,
    ) -> Option<Released<T>> {
        let total = self.references(objects, id)?;
        let references = self.shared(objects, id)?.checked_sub(1)?;
        let guest = self.object(objects, id)?.guest.as_ref();
        if let Some(counter) = guest {
            counter.store(references);
        }
        let guest = guest.is_some();
        self.released(objects, id, total - 1, references, guest)
    }

    /// Finish a public release: `remaining` counts every interface.
    fn released<T: Any>(
        &self,
        objects: &mut Objects,
        id: ComIdentity<T>,
        remaining: u32,
        references: u32,
        guest: bool,
    ) -> Option<Released<T>> {
        if remaining != 0 || self.object(objects, id)?.internal != 0 {
            if !guest {
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

#[cfg(test)]
mod guid_tests {
    use super::*;

    #[test]
    fn registry_guids_parse_in_memory_order() {
        let guid = parse_guid("{56A868B0-0AD4-11CE-B03A-0020AF0BA770}").unwrap();
        assert_eq!(
            guid,
            [
                0xb0, 0x68, 0xa8, 0x56, 0xd4, 0x0a, 0xce, 0x11, 0xb0, 0x3a, 0x00, 0x20, 0xaf, 0x0b,
                0xa7, 0x70
            ]
        );
        assert_eq!(parse_guid("56a868b0-0ad4-11ce-b03a-0020af0ba770"), None);
        assert_eq!(parse_guid("{56a868b0-0ad4-11ce-b03a-0020af0ba77g}"), None);
    }
}
