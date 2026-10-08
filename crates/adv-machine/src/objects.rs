//! Process-owned payloads and opaque alias namespaces.
//!
//! This table knows ownership, identity and aliasing, not API handle semantics.
//! The default namespace preserves the scheduler's existing handle interface.
//! Other namespaces can use the same numeric aliases without making them kernel
//! handles. An identity is a borrowed token: copying or looking it up does not
//! acquire a reference. Anonymous references and alias references are counted
//! separately, so releasing a borrowed token cannot destroy an open alias.

use std::any::Any;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::marker::PhantomData;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::sched::{Handle, Object};
mod timers;

static NEXT_TABLE: AtomicU64 = AtomicU64::new(1);

/// An opaque alias namespace, valid only in the table which created it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Namespace {
    table: u64,
    id: u32,
}

impl Namespace {
    /// The existing kernel-handle namespace. Its token is valid in every table.
    pub const DEFAULT: Self = Self { table: 0, id: 0 };
}

/// A type-checked, non-owning identity. Stale identities are never reused.
pub struct Identity<T: Any> {
    table: u64,
    id: u64,
    marker: PhantomData<fn() -> T>,
}

impl<T: Any> Copy for Identity<T> {}
impl<T: Any> Clone for Identity<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T: Any> PartialEq for Identity<T> {
    fn eq(&self, other: &Self) -> bool {
        self.table == other.table && self.id == other.id
    }
}
impl<T: Any> Eq for Identity<T> {}
impl<T: Any> std::hash::Hash for Identity<T> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.table.hash(state);
        self.id.hash(state);
    }
}
impl<T: Any> std::fmt::Debug for Identity<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Identity")
            .field("table", &self.table)
            .field("id", &self.id)
            .finish()
    }
}

/// Bounded process resources. Reference limits include both owners and aliases.
#[derive(Clone, Copy, Debug)]
pub struct ObjectLimits {
    pub objects: usize,
    pub aliases: usize,
    /// Includes the default namespace.
    pub namespaces: usize,
    pub references: u32,
}

impl Default for ObjectLimits {
    fn default() -> Self {
        Self {
            objects: 65_536,
            aliases: 262_144,
            namespaces: 1_024,
            references: u32::MAX,
        }
    }
}

struct NamespaceState {
    next: u32,
    last: u32,
    aliases: usize,
}

impl Default for NamespaceState {
    fn default() -> Self {
        Self {
            next: 0x100,
            last: u32::MAX & !3,
            aliases: 0,
        }
    }
}

struct Entry {
    value: Object,
    owners: u32,
    aliases: u32,
    /// Counts aliases per namespace, so iteration yields each payload once.
    namespaces: BTreeMap<u32, usize>,
    /// Stable identity for the legacy default-namespace interface.
    kernel_identity: Option<Handle>,
}

impl Entry {
    fn references(&self) -> u32 {
        // All mutations preflight the total, including its arithmetic overflow.
        self.owners + self.aliases
    }
}

/// One payload table for the process, with independent numeric alias namespaces.
pub struct Objects {
    table: u64,
    entries: HashMap<u64, Entry>,
    targets: HashMap<(u32, u32), u64>,
    kernel_identities: HashMap<Handle, u64>,
    /// Armed timer entries, indexed by deadline and stable payload identity.
    timer_deadlines: BTreeSet<(u64, u64)>,
    /// Kernel alias metadata. Each duplicate begins with independent flags;
    /// payload identities and nonkernel namespaces have no handle properties.
    handle_flags: BTreeMap<Handle, u32>,
    namespaces: HashMap<u32, NamespaceState>,
    next_object: u64,
    next_namespace: u32,
    limits: ObjectLimits,
}

impl Default for Objects {
    fn default() -> Self {
        Self::with_limits(ObjectLimits::default())
    }
}

impl Objects {
    pub fn with_limits(limits: ObjectLimits) -> Self {
        let table = NEXT_TABLE
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .expect("object table identity space exhausted");
        Self {
            table,
            entries: HashMap::new(),
            targets: HashMap::new(),
            kernel_identities: HashMap::new(),
            timer_deadlines: BTreeSet::new(),
            handle_flags: BTreeMap::new(),
            namespaces: HashMap::from([(0, NamespaceState::default())]),
            next_object: 0,
            next_namespace: 0,
            limits,
        }
    }

    fn namespace_id(&self, ns: Namespace) -> Option<u32> {
        if ns == Namespace::DEFAULT {
            return Some(0);
        }
        (ns.table == self.table && self.namespaces.contains_key(&ns.id)).then_some(ns.id)
    }

    fn token<T: Any>(&self, id: u64) -> Identity<T> {
        Identity {
            table: self.table,
            id,
            marker: PhantomData,
        }
    }

    fn entry<T: Any>(&self, id: Identity<T>) -> Option<&Entry> {
        if id.table != self.table {
            return None;
        }
        let entry = self.entries.get(&id.id)?;
        match &entry.value {
            Object::Host(value) if value.is::<T>() => Some(entry),
            _ => None,
        }
    }

    fn can_reference(&self, entry: &Entry) -> bool {
        entry
            .references()
            .checked_add(1)
            .is_some_and(|count| count <= self.limits.references)
    }

    fn vacant_alias(&self, ns: u32, alias: u32) -> bool {
        // A closed original kernel handle remains an identity while its payload
        // survives. Reusing that number would make alias_identity ambiguous.
        !self.targets.contains_key(&(ns, alias))
            && (ns != 0 || (alias != 0 && !self.kernel_identities.contains_key(&alias)))
    }

    fn next_alias(&self, ns: u32) -> Option<u32> {
        if self.targets.len() >= self.limits.aliases {
            return None;
        }
        let state = self.namespaces.get(&ns)?;
        let mut next = state.next;
        loop {
            next = next.checked_add(4).filter(|next| *next <= state.last)?;
            if self.vacant_alias(ns, next) {
                return Some(next);
            }
        }
    }

    fn insert_aliased(&mut self, ns: u32, value: Object) -> Option<u32> {
        let alias = self.next_alias(ns)?;
        self.insert_at(ns, alias, value)?;
        self.namespaces.get_mut(&ns).unwrap().next = alias;
        Some(alias)
    }

    fn insert_at(&mut self, ns: u32, alias: u32, value: Object) -> Option<()> {
        if self.entries.len() >= self.limits.objects || self.limits.references == 0 {
            return None;
        }
        if self.targets.len() >= self.limits.aliases || !self.vacant_alias(ns, alias) {
            return None;
        }
        let id = self.next_object.checked_add(1)?;
        self.next_object = id;
        let kernel_identity = (ns == 0).then_some(alias);
        self.entries.insert(
            id,
            Entry {
                value,
                owners: 0,
                aliases: 1,
                namespaces: BTreeMap::from([(ns, 1)]),
                kernel_identity,
            },
        );
        self.targets.insert((ns, alias), id);
        let state = self.namespaces.get_mut(&ns).unwrap();
        state.aliases += 1;
        if let Some(kernel_identity) = kernel_identity {
            self.kernel_identities.insert(kernel_identity, id);
        }
        Some(())
    }

    fn add_alias(&mut self, ns: u32, alias: u32, id: u64) -> bool {
        if self.targets.len() >= self.limits.aliases || !self.vacant_alias(ns, alias) {
            return false;
        }
        let Some(entry) = self.entries.get(&id) else {
            return false;
        };
        if !self.can_reference(entry) {
            return false;
        }
        let entry = self.entries.get_mut(&id).unwrap();
        entry.aliases += 1;
        *entry.namespaces.entry(ns).or_default() += 1;
        if ns == 0 && entry.kernel_identity.is_none() {
            entry.kernel_identity = Some(alias);
            self.kernel_identities.insert(alias, id);
        }
        self.targets.insert((ns, alias), id);
        self.namespaces.get_mut(&ns).unwrap().aliases += 1;
        true
    }

    fn take_unreferenced(&mut self, id: u64) -> Option<Object> {
        if self.entries.get(&id)?.references() != 0 {
            return None;
        }
        let entry = self.entries.remove(&id).unwrap();
        if let Object::Timer {
            deadline: Some(deadline),
            ..
        } = &entry.value
        {
            self.timer_deadlines.remove(&(*deadline, id));
        }
        if let Some(kernel_identity) = entry.kernel_identity {
            self.kernel_identities.remove(&kernel_identity);
        }
        Some(entry.value)
    }

    pub fn new_namespace(&mut self) -> Option<Namespace> {
        self.new_namespace_in(0x104, u32::MAX & !3)
    }

    /// Select a bounded, inclusive range for automatic aliases. Explicit
    /// bind_alias calls still accept any numeric alias (such as guest pointers).
    /// The OS personality chooses its ranges; this table only enforces them.
    pub fn new_namespace_in(&mut self, first: u32, last: u32) -> Option<Namespace> {
        if first == 0 || first > last || first & 3 != 0 || last & 3 != 0 {
            return None;
        }
        if self.namespaces.len() >= self.limits.namespaces {
            return None;
        }
        let id = self.next_namespace.checked_add(1)?;
        self.namespaces.insert(
            id,
            NamespaceState {
                next: first - 4,
                last,
                aliases: 0,
            },
        );
        self.next_namespace = id;
        Some(Namespace {
            table: self.table,
            id,
        })
    }

    /// Live aliases in one namespace; O(1). Invalid tokens report zero.
    pub fn alias_count(&self, ns: Namespace) -> usize {
        self.namespace_id(ns)
            .and_then(|id| self.namespaces.get(&id))
            .map_or(0, |state| state.aliases)
    }

    pub fn object_count(&self) -> usize {
        self.entries.len()
    }

    /// Retained kernel payloads, once per identity rather than once per alias.
    /// Closed original aliases remain visible while another real reference owns
    /// the payload; destroyed identities and nonkernel payloads are excluded.
    pub fn kernel_objects(&self) -> impl Iterator<Item = &Object> {
        self.entries
            .values()
            .filter(|entry| entry.kernel_identity.is_some())
            .map(|entry| &entry.value)
    }

    /// Preflight a default-namespace insertion before external side effects.
    /// Valid until the next mutation of this exclusively owned table.
    pub fn can_insert(&self) -> bool {
        self.entries.len() < self.limits.objects
            && self.limits.references != 0
            && self.next_object.checked_add(1).is_some()
            && self.next_alias(0).is_some()
    }

    /// Total anonymous and alias references, for ownership diagnostics.
    pub fn strong_count<T: Any>(&self, id: Identity<T>) -> Option<u32> {
        Some(self.entry(id)?.references())
    }

    /// Insert with one anonymous owner, to be explicitly retained or released.
    /// Binding an alias adds a separate reference; it does not consume this one.
    pub fn insert_owned<T: Any>(&mut self, value: T) -> Option<Identity<T>> {
        if self.entries.len() >= self.limits.objects || self.limits.references == 0 {
            return None;
        }
        let id = self.next_object.checked_add(1)?;
        self.next_object = id;
        self.entries.insert(
            id,
            Entry {
                value: Object::Host(Box::new(value)),
                owners: 1,
                aliases: 0,
                namespaces: BTreeMap::new(),
                kernel_identity: None,
            },
        );
        Some(self.token(id))
    }

    /// Retain an anonymous strong reference, after checking table, type and limits.
    pub fn retain<T: Any>(&mut self, id: Identity<T>) -> bool {
        let Some(entry) = self.entry(id) else {
            return false;
        };
        if !self.can_reference(entry) {
            return false;
        }
        self.entries.get_mut(&id.id).unwrap().owners += 1;
        true
    }

    /// Release one anonymous owner. A borrowed alias identity without an owner
    /// cannot be released. The last reference returns the payload for cleanup.
    pub fn release<T: Any>(&mut self, id: Identity<T>) -> Option<Option<T>> {
        if self.entry(id)?.owners == 0 {
            return None;
        }
        self.entries.get_mut(&id.id)?.owners -= 1;
        Some(self.take_unreferenced(id.id).map(|object| match object {
            Object::Host(value) => *value.downcast::<T>().ok().unwrap(),
            _ => unreachable!("typed identity must name a host payload"),
        }))
    }

    pub fn owned<T: Any>(&self, id: Identity<T>) -> Option<&T> {
        match &self.entry(id)?.value {
            Object::Host(value) => value.downcast_ref(),
            _ => None,
        }
    }

    pub fn owned_mut<T: Any>(&mut self, id: Identity<T>) -> Option<&mut T> {
        self.entry(id)?;
        match &mut self.entries.get_mut(&id.id)?.value {
            Object::Host(value) => value.downcast_mut(),
            _ => None,
        }
    }

    /// Bind a specified alias, retaining the payload. Existing aliases are never
    /// overwritten. A guest pointer may be an alias in a nondefault namespace.
    pub fn bind_alias<T: Any>(&mut self, ns: Namespace, alias: u32, id: Identity<T>) -> bool {
        let Some(ns) = self.namespace_id(ns) else {
            return false;
        };
        if self.entry(id).is_none() {
            return false;
        }
        self.add_alias(ns, alias, id.id)
    }

    /// Borrow an identity without retaining it; refuses stale, foreign or wrong types.
    pub fn lookup_alias<T: Any>(&self, ns: Namespace, alias: u32) -> Option<Identity<T>> {
        let ns = self.namespace_id(ns)?;
        let token = self.token(*self.targets.get(&(ns, alias))?);
        self.entry(token)?;
        Some(token)
    }

    /// Release one alias reference in precisely this namespace.
    pub fn close_alias(&mut self, ns: Namespace, alias: u32) -> Option<Option<Object>> {
        let ns = self.namespace_id(ns)?;
        let id = self.targets.remove(&(ns, alias))?;
        if ns == 0 {
            self.handle_flags.remove(&alias);
        }
        self.namespaces.get_mut(&ns).unwrap().aliases -= 1;
        let entry = self.entries.get_mut(&id).unwrap();
        entry.aliases -= 1;
        let count = entry.namespaces.get_mut(&ns).unwrap();
        *count -= 1;
        if *count == 0 {
            entry.namespaces.remove(&ns);
        }
        Some(self.take_unreferenced(id))
    }

    /// Insert a payload with an automatically allocated alias and exactly one
    /// reference. No anonymous owner is left behind; failures do not mutate the table.
    pub fn insert_host_in<T: Any>(&mut self, ns: Namespace, value: T) -> Option<u32> {
        let ns = self.namespace_id(ns)?;
        self.insert_aliased(ns, Object::Host(Box::new(value)))
    }

    /// Insert a new payload directly at a specified alias with one reference.
    /// Refuses collisions without replacing or leaking the previous payload.
    pub fn insert_host_at<T: Any>(&mut self, ns: Namespace, alias: u32, value: T) -> bool {
        self.namespace_id(ns).is_some_and(|ns| {
            self.insert_at(ns, alias, Object::Host(Box::new(value)))
                .is_some()
        })
    }

    /// Every typed payload with a live alias in `ns`, once regardless of alias count.
    pub fn hosts_in<T: Any>(&self, ns: Namespace) -> impl Iterator<Item = (Identity<T>, &T)> {
        let ns = self.namespace_id(ns);
        self.entries.iter().filter_map(move |(&id, entry)| {
            if !entry.namespaces.contains_key(&ns?) {
                return None;
            }
            match &entry.value {
                Object::Host(value) => Some((self.token(id), value.downcast_ref()?)),
                _ => None,
            }
        })
    }

    pub fn hosts_in_mut<T: Any>(
        &mut self,
        ns: Namespace,
    ) -> impl Iterator<Item = (Identity<T>, &mut T)> {
        let ns = self.namespace_id(ns);
        let table = self.table;
        self.entries.iter_mut().filter_map(move |(&id, entry)| {
            if !entry.namespaces.contains_key(&ns?) {
                return None;
            }
            match &mut entry.value {
                Object::Host(value) => Some((
                    Identity {
                        table,
                        id,
                        marker: PhantomData,
                    },
                    value.downcast_mut()?,
                )),
                _ => None,
            }
        })
    }

    /// All typed payloads, including anonymous owners and every namespace, once each.
    pub fn all_hosts<T: Any>(&self) -> impl Iterator<Item = (Identity<T>, &T)> {
        self.entries
            .iter()
            .filter_map(|(&id, entry)| match &entry.value {
                Object::Host(value) => Some((self.token(id), value.downcast_ref()?)),
                _ => None,
            })
    }

    pub fn all_hosts_mut<T: Any>(&mut self) -> impl Iterator<Item = (Identity<T>, &mut T)> {
        let table = self.table;
        self.entries
            .iter_mut()
            .filter_map(move |(&id, entry)| match &mut entry.value {
                Object::Host(value) => Some((
                    Identity {
                        table,
                        id,
                        marker: PhantomData,
                    },
                    value.downcast_mut()?,
                )),
                _ => None,
            })
    }

    /// Existing default-namespace insertion; zero denotes resource exhaustion.
    pub fn insert(&mut self, object: Object) -> Handle {
        self.insert_aliased(0, object).unwrap_or(0)
    }

    /// Insert a transferred kernel object at the inherited numeric handle.
    /// Existing aliases/identities are never replaced on collision.
    pub fn insert_handle_at(&mut self, handle: Handle, object: Object) -> bool {
        self.insert_at(0, handle, object).is_some()
    }

    /// Properties belong to the live kernel alias, rather than its payload.
    pub fn handle_flags(&self, handle: Handle) -> Option<u32> {
        self.targets.get(&(0, handle))?;
        Some(self.handle_flags.get(&handle).copied().unwrap_or(0))
    }

    pub fn set_handle_flags(&mut self, handle: Handle, mask: u32, flags: u32) -> bool {
        let Some(previous) = self.handle_flags(handle) else {
            return false;
        };
        let flags = (previous & !mask) | (flags & mask);
        if flags == 0 {
            self.handle_flags.remove(&handle);
        } else {
            self.handle_flags.insert(handle, flags);
        }
        true
    }

    /// Sorted live kernel aliases carrying the inheritance flag. Enumeration is
    /// bounded by the existing alias limit and excludes anonymous identities.
    pub fn inheritable_handles(&self) -> impl Iterator<Item = Handle> + '_ {
        self.handle_flags
            .iter()
            .filter_map(|(&handle, &flags)| (flags & 1 != 0).then_some(handle))
    }

    pub fn insert_host<T: Any>(&mut self, value: T) -> Handle {
        self.insert(Object::Host(Box::new(value)))
    }

    pub fn get(&self, handle: Handle) -> Option<&Object> {
        Some(&self.entries.get(self.targets.get(&(0, handle))?)?.value)
    }

    pub fn get_mut(&mut self, handle: Handle) -> Option<&mut Object> {
        let id = self.targets.get(&(0, handle))?;
        Some(&mut self.entries.get_mut(id)?.value)
    }

    pub fn host<T: Any>(&self, handle: Handle) -> Option<&T> {
        match self.get(handle)? {
            Object::Host(value) => value.downcast_ref(),
            _ => None,
        }
    }

    pub fn host_mut<T: Any>(&mut self, handle: Handle) -> Option<&mut T> {
        match self.get_mut(handle)? {
            Object::Host(value) => value.downcast_mut(),
            _ => None,
        }
    }

    /// Legacy iteration by original default-namespace identity, once each.
    /// Use `all_hosts` to include payloads created only in other namespaces.
    pub fn hosts<T: Any>(&self) -> impl Iterator<Item = (Handle, &T)> {
        self.entries
            .values()
            .filter_map(|entry| match &entry.value {
                Object::Host(value) => Some((entry.kernel_identity?, value.downcast_ref()?)),
                _ => None,
            })
    }

    pub fn hosts_mut<T: Any>(&mut self) -> impl Iterator<Item = (Handle, &mut T)> {
        self.entries
            .values_mut()
            .filter_map(|entry| match &mut entry.value {
                Object::Host(value) => Some((entry.kernel_identity?, value.downcast_mut()?)),
                _ => None,
            })
    }

    pub fn close(&mut self, handle: Handle) -> Option<Option<Object>> {
        self.close_alias(Namespace::DEFAULT, handle)
    }

    pub fn duplicate(&mut self, handle: Handle) -> Option<Handle> {
        self.alias_identity(self.identity(handle)?)
    }

    pub fn identity(&self, handle: Handle) -> Option<Handle> {
        self.entries
            .get(self.targets.get(&(0, handle))?)?
            .kernel_identity
    }

    /// Borrow the process-local, never-reused owner ID behind a kernel alias.
    /// Looking it up neither retains the object nor creates a handle alias.
    pub fn object_id(&self, handle: Handle) -> Option<u64> {
        self.targets.get(&(0, handle)).copied()
    }

    pub fn has_identity(&self, identity: Handle) -> bool {
        self.kernel_identities.contains_key(&identity)
    }

    pub fn same_object(&self, first: Handle, second: Handle) -> bool {
        self.identity(first)
            .is_some_and(|object| self.identity(second) == Some(object))
    }

    pub fn alias_identity(&mut self, identity: Handle) -> Option<Handle> {
        let id = *self.kernel_identities.get(&identity)?;
        let alias = self.next_alias(0)?;
        if !self.add_alias(0, alias, id) {
            return None;
        }
        self.namespaces.get_mut(&0).unwrap().next = alias;
        Some(alias)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn borrowed_owner_ids_share_aliases_without_retention_and_survive_numeric_reuse() {
        let mut objects = Objects::default();
        let handle = objects.insert_host(7u32);
        let owner = objects.object_id(handle).unwrap();
        let token = objects
            .lookup_alias::<u32>(Namespace::DEFAULT, handle)
            .unwrap();
        assert_eq!(objects.strong_count(token), Some(1));
        assert_eq!(objects.object_id(handle), Some(owner));
        assert_eq!(objects.strong_count(token), Some(1));
        let alias = objects.duplicate(handle).unwrap();
        assert_eq!(objects.object_id(alias), Some(owner));
        assert_eq!(objects.strong_count(token), Some(2));
        objects.close(handle);
        assert_eq!(objects.object_id(handle), None);
        assert_eq!(objects.object_id(alias), Some(owner));
        objects.close(alias);
        assert!(
            objects.owned(token).is_none(),
            "borrowed ID does not retain ownership"
        );
        assert!(objects.insert_host_at(Namespace::DEFAULT, handle, 8u32));
        assert_ne!(objects.object_id(handle), Some(owner));
        assert_eq!(objects.object_id(0), None);
    }

    #[test]
    fn kernel_payload_iteration_tracks_retained_identity_not_closed_original_alias() {
        let mut objects = Objects::default();
        let ns = objects.new_namespace().unwrap();
        objects.insert_host_in(ns, 1_u32).unwrap();
        let original = objects.insert_host(2_u32);
        let duplicate = objects.duplicate(original).unwrap();
        assert_eq!(objects.kernel_objects().count(), 1);
        objects.close(original);
        assert_eq!(objects.kernel_objects().count(), 1);
        objects.close(duplicate);
        assert_eq!(objects.kernel_objects().count(), 0);
        assert_eq!(objects.object_count(), 1);
    }

    #[test]
    fn kernel_alias_flags_are_independent_masked_and_removed_with_the_alias() {
        let mut objects = Objects::default();
        let first = objects.insert_host(7_u32);
        assert_eq!(objects.handle_flags(first), Some(0));
        assert!(objects.set_handle_flags(first, 3, 3));
        let alias = objects.duplicate(first).unwrap();
        assert_eq!(objects.handle_flags(alias), Some(0));
        assert_eq!(objects.inheritable_handles().collect::<Vec<_>>(), [first]);
        assert!(objects.set_handle_flags(alias, 1, 1));
        assert!(objects.set_handle_flags(first, 1, 0));
        assert_eq!(objects.handle_flags(first), Some(2));
        assert_eq!(objects.inheritable_handles().collect::<Vec<_>>(), [alias]);
        assert!(objects.same_object(first, alias));
        // Internal ownership cleanup remains independent of API close flags.
        assert!(objects.close(first).is_some());
        assert_eq!(objects.handle_flags(first), None);
        assert!(objects.host::<u32>(alias).is_some());
        assert!(objects.close(alias).is_some());
        assert!(objects.inheritable_handles().next().is_none());
        assert!(!objects.set_handle_flags(alias, 3, 3));
        assert_eq!(objects.handle_flags(0), None);
    }

    #[test]
    fn inherited_numeric_aliases_refuse_collisions_and_do_not_mark_other_namespaces() {
        let mut objects = Objects::default();
        assert!(objects.insert_handle_at(0x204, Object::Host(Box::new(7_u32))));
        assert!(!objects.insert_handle_at(0x204, Object::Host(Box::new(8_u32))));
        assert_eq!(objects.host::<u32>(0x204), Some(&7));
        assert!(!objects.insert_handle_at(0, Object::Host(Box::new(8_u32))));
        let ns = objects.new_namespace().unwrap();
        assert!(objects.insert_host_at(ns, 0x204, 9_u32));
        assert!(objects.set_handle_flags(0x204, 1, 1));
        assert!(objects.close(0x204).is_some());
        assert_eq!(objects.handle_flags(0x204), None);
        assert!(objects.inheritable_handles().next().is_none());
        assert!(objects.lookup_alias::<u32>(ns, 0x204).is_some());
    }

    #[test]
    fn counter_and_reference_overflow_refuse_without_mutation() {
        let mut objects = Objects::default();
        let id = objects.insert_owned(7_u32).unwrap();
        objects.entries.get_mut(&id.id).unwrap().owners = u32::MAX;
        assert!(!objects.retain(id));
        assert!(!objects.bind_alias(Namespace::DEFAULT, 0x104, id));
        assert_eq!(objects.strong_count(id), Some(u32::MAX));
        assert_eq!(objects.alias_count(Namespace::DEFAULT), 0);

        let mut objects = Objects::default();
        objects.next_object = u64::MAX;
        assert!(objects.insert_owned(1_u32).is_none());
        assert_eq!(objects.insert_host(2_u32), 0);
        assert_eq!(objects.object_count(), 0);
        objects.next_namespace = u32::MAX;
        assert!(objects.new_namespace().is_none());
        objects.namespaces.get_mut(&0).unwrap().next = u32::MAX - 3;
        objects.next_object = 0;
        assert_eq!(objects.insert_host(3_u32), 0);
        assert_eq!(objects.object_count(), 0);
    }

    #[test]
    fn wrong_type_identity_is_rejected_by_every_typed_mutation() {
        let mut objects = Objects::default();
        let id = objects.insert_owned(7_u32).unwrap();
        let wrong: Identity<String> = Identity {
            table: id.table,
            id: id.id,
            marker: PhantomData,
        };
        assert!(objects.owned(wrong).is_none());
        assert!(objects.owned_mut(wrong).is_none());
        assert!(!objects.retain(wrong));
        assert!(objects.release(wrong).is_none());
        assert!(!objects.bind_alias(Namespace::DEFAULT, 0x104, wrong));
        assert_eq!(objects.strong_count(id), Some(1));
    }
}
