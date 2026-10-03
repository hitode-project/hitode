use std::cell::Cell;
use std::rc::Rc;

use adv_machine::{Namespace, Object, ObjectLimits, Objects};

struct Payload {
    value: u32,
    drops: Rc<Cell<u32>>,
}

impl Drop for Payload {
    fn drop(&mut self) {
        self.drops.set(self.drops.get() + 1);
    }
}

fn payload(value: u32, drops: &Rc<Cell<u32>>) -> Payload {
    Payload {
        value,
        drops: drops.clone(),
    }
}

#[test]
fn numeric_aliases_are_isolated_from_kernel_handles() {
    let mut objects = Objects::default();
    let first = objects.new_namespace().unwrap();
    let second = objects.new_namespace().unwrap();
    let kernel = objects.insert_host(10_u32);
    let a = objects.insert_host_in(first, 20_u32).unwrap();
    let b = objects.insert_host_in(second, 30_u32).unwrap();
    assert_eq!((kernel, a, b), (0x104, 0x104, 0x104));
    assert_eq!(objects.host::<u32>(kernel), Some(&10));
    assert_eq!(
        objects.owned(objects.lookup_alias::<u32>(first, a).unwrap()),
        Some(&20)
    );
    assert_eq!(
        objects.owned(objects.lookup_alias::<u32>(second, b).unwrap()),
        Some(&30)
    );
    let duplicate = objects.duplicate(kernel).unwrap();
    assert_eq!(objects.identity(duplicate), Some(kernel));
    assert!(matches!(objects.close(kernel), Some(None)));
    assert!(matches!(
        objects.close(duplicate),
        Some(Some(Object::Host(_)))
    ));
    assert!(objects.get(a).is_none());
    assert!(objects.close(a).is_none());
    assert!(objects.duplicate(a).is_none());
    assert_eq!(objects.alias_count(Namespace::DEFAULT), 0);
    assert_eq!(objects.alias_count(first), 1);
    assert_eq!(objects.alias_count(second), 1);
    assert_eq!(objects.object_count(), 2);
}

#[test]
fn automatic_alias_ranges_are_checked_bounded_and_do_not_reuse_stale_numbers() {
    let mut objects = Objects::with_limits(ObjectLimits {
        namespaces: 2,
        ..ObjectLimits::default()
    });
    for (first, last) in [(0, 4), (9, 16), (8, 15), (16, 8)] {
        assert!(objects.new_namespace_in(first, last).is_none());
    }
    // Rejected range definitions do not consume the namespace limit.
    let ns = objects.new_namespace_in(0xfffffff8, 0xfffffffc).unwrap();
    assert_eq!(objects.insert_host_in(ns, 1_u32), Some(0xfffffff8));
    assert_eq!(objects.insert_host_in(ns, 2_u32), Some(0xfffffffc));
    assert!(objects.insert_host_in(ns, 3_u32).is_none());
    assert_eq!(objects.alias_count(ns), 2);
    objects.close_alias(ns, 0xfffffff8).unwrap();
    assert!(objects.insert_host_in(ns, 4_u32).is_none());
    assert_eq!(objects.object_count(), 1);
    // Kernel allocation is unaffected by another namespace's exhaustion.
    assert_eq!(objects.insert_host(5_u32), 0x104);
}

#[test]
fn automatic_alias_transfers_owner_without_leaking() {
    let drops = Rc::new(Cell::new(0));
    let mut objects = Objects::default();
    let ns = objects.new_namespace().unwrap();
    let alias = objects.insert_host_in(ns, payload(7, &drops)).unwrap();
    let id = objects.lookup_alias::<Payload>(ns, alias).unwrap();
    assert_eq!(objects.strong_count(id), Some(1));
    assert!(
        objects.release(id).is_none(),
        "lookup did not acquire an anonymous owner"
    );
    assert_eq!(objects.strong_count(id), Some(1));
    let returned = objects.close_alias(ns, alias).unwrap().unwrap();
    assert_eq!(objects.object_count(), 0);
    assert_eq!(
        drops.get(),
        0,
        "caller owns the final payload until cleanup"
    );
    drop(returned);
    assert_eq!(drops.get(), 1);
    assert!(objects.owned(id).is_none());
    assert!(!objects.retain(id));
    assert!(objects.release(id).is_none());
    assert!(!objects.bind_alias(ns, alias, id));
}

#[test]
fn explicit_owner_transfer_and_last_release_preserve_alias_lifetime() {
    let drops = Rc::new(Cell::new(0));
    let mut objects = Objects::default();
    let ns = objects.new_namespace().unwrap();
    let id = objects.insert_owned(payload(1, &drops)).unwrap();
    assert!(objects.bind_alias(ns, 0x8000_0000, id));
    assert_eq!(objects.strong_count(id), Some(2));
    assert!(matches!(objects.release(id), Some(None)));
    assert!(objects.release(id).is_none());
    assert_eq!(objects.owned(id).unwrap().value, 1);
    assert!(objects.retain(id));
    assert!(matches!(objects.close_alias(ns, 0x8000_0000), Some(None)));
    assert!(objects.lookup_alias::<Payload>(ns, 0x8000_0000).is_none());
    assert_eq!(objects.strong_count(id), Some(1));
    let last = objects.release(id).unwrap().unwrap();
    assert_eq!(last.value, 1);
    drop(last);
    assert_eq!(drops.get(), 1);
}

#[test]
fn alias_collision_never_replaces_a_payload() {
    let mut objects = Objects::default();
    let ns = objects.new_namespace().unwrap();
    let first = objects.insert_owned(1_u32).unwrap();
    let second = objects.insert_owned(2_u32).unwrap();
    assert!(objects.bind_alias(ns, 42, first));
    assert!(!objects.bind_alias(ns, 42, second));
    assert_eq!(objects.lookup_alias::<u32>(ns, 42), Some(first));
    assert_eq!(objects.strong_count(first), Some(2));
    assert_eq!(objects.strong_count(second), Some(1));
    assert_eq!(objects.alias_count(ns), 1);
    assert!(objects.lookup_alias::<String>(ns, 42).is_none());
}

#[test]
fn table_tokens_cannot_cross_processes_and_stale_ids_do_not_revive() {
    let mut first = Objects::default();
    let mut second = Objects::default();
    let ns = first.new_namespace().unwrap();
    let second_ns = second.new_namespace().unwrap();
    let id = first.insert_owned(1_u32).unwrap();
    let other = second.insert_owned(2_u32).unwrap();
    assert!(second.owned(id).is_none());
    assert!(!second.retain(id));
    assert!(second.release(id).is_none());
    assert!(!second.bind_alias(second_ns, 12, id));
    assert!(!second.bind_alias(ns, 12, other));
    assert!(second.insert_host_in(ns, 3_u32).is_none());
    assert_eq!(second.alias_count(ns), 0);
    assert_eq!(first.release(id), Some(Some(1)));
    let replacement = first.insert_owned(4_u32).unwrap();
    assert_ne!(id, replacement);
    assert!(first.owned(id).is_none());
    assert!(!first.bind_alias(ns, 12, id));
    assert_eq!(first.owned(replacement), Some(&4));
}

#[test]
fn typed_namespace_iteration_visits_a_payload_once_and_excludes_other_namespaces() {
    let mut objects = Objects::default();
    let ns = objects.new_namespace().unwrap();
    let other = objects.new_namespace().unwrap();
    let id = objects.insert_owned(1_u32).unwrap();
    assert!(objects.bind_alias(ns, 10, id));
    assert!(objects.bind_alias(ns, 20, id));
    assert!(objects.bind_alias(other, 30, id));
    assert!(matches!(objects.release(id), Some(None)));
    objects.insert_host_in(other, 100_u32).unwrap();
    objects
        .insert_host_in(ns, String::from("other type"))
        .unwrap();
    objects.insert_owned(1000_u32).unwrap();
    assert_eq!(objects.hosts_in::<u32>(ns).count(), 1);
    assert_eq!(objects.hosts_in::<u32>(other).count(), 2);
    assert_eq!(objects.all_hosts::<u32>().count(), 3);
    assert_eq!(objects.hosts::<u32>().count(), 0);
    for (_, value) in objects.hosts_in_mut::<u32>(ns) {
        *value += 1;
    }
    assert_eq!(objects.owned(id), Some(&2));
    for (_, value) in objects.all_hosts_mut::<u32>() {
        *value += 10;
    }
    assert_eq!(objects.owned(id), Some(&12));
    assert!(matches!(objects.close_alias(ns, 10), Some(None)));
    assert_eq!(objects.hosts_in::<u32>(ns).count(), 1);
    assert!(matches!(objects.close_alias(ns, 20), Some(None)));
    assert_eq!(objects.hosts_in::<u32>(ns).count(), 0);
    assert_eq!(objects.hosts_in::<u32>(other).count(), 2);
}

#[test]
fn kernel_identity_survives_original_handle_close_until_last_reference() {
    let mut objects = Objects::default();
    let handle = objects.insert_host(7_u32);
    let duplicate = objects.duplicate(handle).unwrap();
    let ns = objects.new_namespace().unwrap();
    let id = objects
        .lookup_alias::<u32>(Namespace::DEFAULT, handle)
        .unwrap();
    assert!(objects.bind_alias(ns, handle, id));
    assert!(matches!(objects.close(handle), Some(None)));
    assert!(objects.has_identity(handle));
    assert_eq!(objects.identity(duplicate), Some(handle));
    assert!(!objects.bind_alias(Namespace::DEFAULT, handle, id));
    assert!(matches!(objects.close(duplicate), Some(None)));
    let reopened = objects.alias_identity(handle).unwrap();
    assert_eq!(objects.host::<u32>(reopened), Some(&7));
    assert!(matches!(objects.close(reopened), Some(None)));
    assert!(matches!(objects.close_alias(ns, handle), Some(Some(_))));
    assert!(!objects.has_identity(handle));
    assert!(objects.alias_identity(handle).is_none());
}

#[test]
fn resource_failure_is_atomic_and_namespace_counts_are_reclaimed() {
    let mut objects = Objects::with_limits(ObjectLimits {
        objects: 1,
        aliases: 2,
        namespaces: 2,
        references: 2,
    });
    let ns = objects.new_namespace().unwrap();
    assert!(objects.new_namespace().is_none());
    let alias = objects.insert_host_in(ns, 7_u32).unwrap();
    assert!(objects.insert_owned(8_u32).is_none());
    assert_eq!(objects.insert_host(9_u32), 0);
    let id = objects.lookup_alias::<u32>(ns, alias).unwrap();
    assert!(objects.bind_alias(ns, 0x200, id));
    assert!(!objects.bind_alias(Namespace::DEFAULT, 0x300, id));
    assert!(!objects.retain(id));
    assert_eq!(objects.strong_count(id), Some(2));
    assert_eq!(objects.object_count(), 1);
    assert_eq!(objects.alias_count(ns), 2);
    assert!(matches!(objects.close_alias(ns, alias), Some(None)));
    assert!(matches!(objects.close_alias(ns, 0x200), Some(Some(_))));
    assert_eq!(objects.alias_count(ns), 0);
    assert_eq!(objects.object_count(), 0);
    // Failed insertions did not advance the allocator or leave an anonymous owner.
    assert_eq!(objects.insert_host_in(ns, 10_u32), Some(0x108));
}

#[test]
fn reference_limit_also_applies_to_anonymous_owners() {
    let mut objects = Objects::with_limits(ObjectLimits {
        references: 1,
        ..ObjectLimits::default()
    });
    let id = objects.insert_owned(7_u32).unwrap();
    assert!(!objects.retain(id));
    assert!(!objects.bind_alias(Namespace::DEFAULT, 0x104, id));
    assert_eq!(objects.alias_count(Namespace::DEFAULT), 0);
    assert_eq!(objects.release(id), Some(Some(7)));
}

#[test]
fn process_drop_cleans_every_payload_once_regardless_of_namespace_and_refcount() {
    let drops = Rc::new(Cell::new(0));
    {
        let mut objects = Objects::default();
        let ns = objects.new_namespace().unwrap();
        let kernel = objects.insert_host(payload(1, &drops));
        objects.duplicate(kernel).unwrap();
        let id = objects.insert_owned(payload(2, &drops)).unwrap();
        objects.retain(id);
        objects.bind_alias(ns, 0x104, id);
        objects.insert_host_in(ns, payload(3, &drops)).unwrap();
        assert_eq!(objects.object_count(), 3);
        assert_eq!(drops.get(), 0);
    }
    assert_eq!(drops.get(), 3);
}
