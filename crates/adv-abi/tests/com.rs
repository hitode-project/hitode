//! COM identity/reference rules from IUnknown's documented contract, with no
//! guest execution or native devices. Frontends keep vtables in guest memory.
use std::cell::Cell;
use std::rc::Rc;

use adv_abi::com::{Com, IUNKNOWN, QueryError};
use adv_machine::{Namespace, ObjectLimits, Objects};

const A: [u8; 16] = [1; 16];
const B: [u8; 16] = [2; 16];
const C: [u8; 16] = [3; 16];

#[test]
fn aggregated_interface_lookup_is_borrowed_even_at_reference_limit_and_seals_iids() {
    let mut objects = Objects::with_limits(ObjectLimits {
        references: 2,
        ..ObjectLimits::default()
    });
    let com = Com::new(&mut objects).unwrap();
    let id = com.create(&mut objects, 0x1000, A, 7_u32).unwrap();
    assert_eq!(
        com.resolve_interface::<u32>(&mut objects, 0x1000, A),
        Ok(0x1000)
    );
    assert_eq!(com.references(&objects, id), Some(1));
    assert_eq!(
        com.query::<u32>(&mut objects, 0x1000, A),
        Err(QueryError::ReferenceLimit)
    );
    assert!(!com.register_interface(&mut objects, id, B, 0x1000));
    assert_eq!(
        com.resolve_interface::<u32>(&mut objects, 0x1000, B),
        Err(QueryError::NoInterface)
    );
    assert_eq!(com.references(&objects, id), Some(1));
    assert!(
        com.release_identity(&mut objects, id)
            .unwrap()
            .destroyed
            .is_some()
    );
    assert_eq!(
        com.resolve_interface::<u32>(&mut objects, 0x1000, A),
        Err(QueryError::InvalidObject)
    );
    assert_eq!(objects.object_count(), 0);
}

#[test]
fn bindings_keep_zero_public_reference_identity_and_getters_can_retain_it_again() {
    let mut objects = Objects::default();
    let com = Com::new(&mut objects).unwrap();
    let id = com.create(&mut objects, 0x1000, A, 7_u32).unwrap();
    assert!(com.retain_internal(&mut objects, id));
    assert_eq!(com.references(&objects, id), Some(1));
    assert_eq!(objects.strong_count(id), Some(3));
    let release = com.release_identity(&mut objects, id).unwrap();
    assert_eq!(release.references, 0);
    assert!(release.destroyed.is_none());
    assert_eq!(com.get::<u32>(&objects, 0x1000), Some(&7));
    assert!(com.release_identity(&mut objects, id).is_none());
    assert_eq!(com.query::<u32>(&mut objects, 0x1000, A), Ok(0x1000));
    assert_eq!(com.references(&objects, id), Some(1));
    assert!(com.release_internal(&mut objects, id).unwrap().is_none());
    assert!(com.release_internal(&mut objects, id).is_none());
    assert_eq!(com.references(&objects, id), Some(1));
    assert!(
        com.release_identity(&mut objects, id)
            .unwrap()
            .destroyed
            .is_some()
    );
    assert_eq!(objects.object_count(), 0);
}

#[test]
fn last_binding_releases_zero_public_reference_payload_and_all_aliases_once() {
    let drops = Rc::new(Cell::new(0));
    let mut objects = Objects::default();
    let com = Com::new(&mut objects).unwrap();
    let id = com
        .create(&mut objects, 0x1000, A, Payload(drops.clone()))
        .unwrap();
    assert!(com.register_interface(&mut objects, id, B, 0x2000));
    assert!(com.retain_internal(&mut objects, id));
    assert!(com.retain_internal(&mut objects, id));
    assert_eq!(
        com.release_identity(&mut objects, id).unwrap().references,
        0
    );
    assert!(com.release_internal(&mut objects, id).unwrap().is_none());
    let destroyed = com.release_internal(&mut objects, id).unwrap().unwrap();
    assert_eq!(destroyed.interfaces, vec![0x1000, 0x2000]);
    assert_eq!(objects.object_count(), 0);
    assert_eq!(objects.alias_count(com.namespace()), 0);
    assert_eq!(drops.get(), 0);
    drop(destroyed);
    assert_eq!(drops.get(), 1);
}

#[test]
fn internal_ownership_refuses_limits_and_foreign_identities_transactionally() {
    let mut objects = Objects::with_limits(ObjectLimits {
        references: 2,
        ..ObjectLimits::default()
    });
    let com = Com::new(&mut objects).unwrap();
    let id = com.create(&mut objects, 0x1000, A, 7_u32).unwrap();
    assert!(!com.retain_internal(&mut objects, id));
    assert_eq!(com.references(&objects, id), Some(1));
    assert_eq!(objects.strong_count(id), Some(2));
    assert!(com.release_internal(&mut objects, id).is_none());
    let other = Com::new(&mut objects).unwrap();
    assert!(!other.retain_internal(&mut objects, id));
    assert!(other.release_internal(&mut objects, id).is_none());
    assert!(
        com.release_identity(&mut objects, id)
            .unwrap()
            .destroyed
            .is_some()
    );
}

struct Payload(Rc<Cell<u32>>);
impl Drop for Payload {
    fn drop(&mut self) {
        self.0.set(self.0.get() + 1);
    }
}

#[test]
fn multiple_interfaces_have_one_canonical_iunknown_and_one_payload() {
    let mut objects = Objects::default();
    let com = Com::new(&mut objects).unwrap();
    let id = com.create(&mut objects, 0x1000, A, 7_u32).unwrap();
    assert!(com.register_interface(&mut objects, id, B, 0x2000));
    assert_eq!(com.references(&objects, id), Some(1));
    assert_eq!(
        objects.strong_count(id),
        Some(3),
        "two internal aliases plus one client owner"
    );
    assert_eq!(objects.object_count(), 1);
    assert_eq!(com.query::<u32>(&mut objects, 0x2000, A), Ok(0x1000));
    assert_eq!(com.query::<u32>(&mut objects, 0x1000, B), Ok(0x2000));
    assert_eq!(com.query::<u32>(&mut objects, 0x2000, B), Ok(0x2000));
    assert_eq!(com.query::<u32>(&mut objects, 0x1000, IUNKNOWN), Ok(0x1000));
    assert_eq!(com.query::<u32>(&mut objects, 0x2000, IUNKNOWN), Ok(0x1000));
    assert_eq!(com.references(&objects, id), Some(6));
    assert_eq!(
        com.identity::<u32>(&objects, 0x1000),
        com.identity::<u32>(&objects, 0x2000)
    );
    assert_eq!(
        com.query::<u32>(&mut objects, 0x2000, C),
        Err(QueryError::NoInterface)
    );
    assert_eq!(com.references(&objects, id), Some(6));
    for count in (1..=5).rev() {
        let released = com.release::<u32>(&mut objects, 0x2000).unwrap();
        assert_eq!(released.references, count);
        assert!(released.destroyed.is_none());
        assert_eq!(com.get::<u32>(&objects, 0x2000), Some(&7));
    }
    let last = com.release::<u32>(&mut objects, 0x2000).unwrap();
    assert_eq!(last.references, 0);
    let destroyed = last.destroyed.unwrap();
    assert_eq!(destroyed.value, 7);
    assert_eq!(destroyed.interfaces, vec![0x1000, 0x2000]);
    assert_eq!(objects.alias_count(com.namespace()), 0);
    assert_eq!(objects.object_count(), 0);
    assert!(com.add_ref::<u32>(&mut objects, 0x1000).is_none());
    assert!(com.release::<u32>(&mut objects, 0x2000).is_none());
    assert!(com.get_identity(&objects, id).is_none());
}

#[test]
fn shared_interface_pointers_do_not_add_internal_references() {
    let mut objects = Objects::default();
    let com = Com::new(&mut objects).unwrap();
    let id = com.create(&mut objects, 0x1000, A, 7_u32).unwrap();
    assert!(com.register_interface(&mut objects, id, B, 0x1000));
    assert_eq!(objects.alias_count(com.namespace()), 1);
    assert_eq!(objects.strong_count(id), Some(2));
    assert_eq!(com.query::<u32>(&mut objects, 0x1000, B), Ok(0x1000));
    assert_eq!(
        com.release_identity(&mut objects, id).unwrap().references,
        1
    );
    let last = com
        .release_identity(&mut objects, id)
        .unwrap()
        .destroyed
        .unwrap();
    assert_eq!(last.interfaces, vec![0x1000]);
}

#[test]
fn interface_set_is_static_after_first_query_even_a_failed_query() {
    let mut objects = Objects::default();
    let com = Com::new(&mut objects).unwrap();
    let id = com.create(&mut objects, 0x1000, A, 7_u32).unwrap();
    assert_eq!(
        com.query::<u32>(&mut objects, 0x1000, B),
        Err(QueryError::NoInterface)
    );
    assert!(!com.register_interface(&mut objects, id, B, 0x2000));
    assert!(!com.register_interface(&mut objects, id, IUNKNOWN, 0x2000));
    assert!(com.register_interface(&mut objects, id, A, 0x1000));
    assert_eq!(com.references(&objects, id), Some(1));
}

#[test]
fn invalid_collision_and_exhaustion_drop_unpublished_payloads_once() {
    let drops = Rc::new(Cell::new(0));
    let mut objects = Objects::default();
    let com = Com::new(&mut objects).unwrap();
    assert!(
        com.create(&mut objects, 0, A, Payload(drops.clone()))
            .is_none()
    );
    assert!(
        com.create(&mut objects, 3, A, Payload(drops.clone()))
            .is_none()
    );
    let id = com
        .create(&mut objects, 0x1000, A, Payload(drops.clone()))
        .unwrap();
    assert!(
        com.create(&mut objects, 0x1000, A, Payload(drops.clone()))
            .is_none()
    );
    assert_eq!(objects.object_count(), 1);
    assert_eq!(drops.get(), 3);
    let last = com
        .release_identity(&mut objects, id)
        .unwrap()
        .destroyed
        .unwrap();
    assert_eq!(drops.get(), 3);
    drop(last);
    assert_eq!(drops.get(), 4);

    let mut objects = Objects::with_limits(ObjectLimits {
        references: 1,
        ..ObjectLimits::default()
    });
    let com = Com::new(&mut objects).unwrap();
    assert!(
        com.create(&mut objects, 0x1000, A, Payload(drops.clone()))
            .is_none()
    );
    assert_eq!(objects.object_count(), 0);
    assert_eq!(objects.alias_count(com.namespace()), 0);
    assert_eq!(drops.get(), 5);
}

#[test]
fn query_and_add_ref_refuse_exhausted_limits_without_returning_success() {
    let mut objects = Objects::with_limits(ObjectLimits {
        references: 3,
        ..ObjectLimits::default()
    });
    let com = Com::new(&mut objects).unwrap();
    let id = com.create(&mut objects, 0x1000, A, 7_u32).unwrap();
    assert!(com.register_interface(&mut objects, id, B, 0x2000));
    assert_eq!(com.references(&objects, id), Some(1));
    assert!(com.add_ref::<u32>(&mut objects, 0x1000).is_none());
    assert_eq!(
        com.query::<u32>(&mut objects, 0x2000, A),
        Err(QueryError::ReferenceLimit)
    );
    assert_eq!(com.references(&objects, id), Some(1));
    assert_eq!(
        com.release_identity(&mut objects, id).unwrap().references,
        0
    );
    assert_eq!(objects.object_count(), 0);
}

#[test]
fn namespace_type_and_process_boundaries_are_enforced() {
    let mut objects = Objects::default();
    let com = Com::new(&mut objects).unwrap();
    let other = Com::new(&mut objects).unwrap();
    let id = com.create(&mut objects, 0x104, A, 7_u32).unwrap();
    let kernel = objects.insert_host(9_u32);
    assert_eq!(kernel, 0x104);
    assert!(objects.close(0x104).is_some());
    assert_eq!(com.get::<u32>(&objects, 0x104), Some(&7));
    assert!(com.get::<String>(&objects, 0x104).is_none());
    assert!(com.add_ref::<String>(&mut objects, 0x104).is_none());
    assert!(other.get::<u32>(&objects, 0x104).is_none());
    assert!(other.release_identity(&mut objects, id).is_none());
    let mut foreign = Objects::default();
    assert!(com.add_ref_identity(&mut foreign, id).is_none());
    assert!(com.create(&mut foreign, 0x104, A, 10_u32).is_none());
    assert_eq!(foreign.object_count(), 0);
    assert_eq!(objects.alias_count(Namespace::DEFAULT), 0);
    assert_eq!(com.references(&objects, id), Some(1));
}

#[test]
fn closing_an_interface_alias_outside_com_is_detected_before_partial_release() {
    let mut objects = Objects::default();
    let com = Com::new(&mut objects).unwrap();
    let id = com.create(&mut objects, 0x1000, A, 7_u32).unwrap();
    com.register_interface(&mut objects, id, B, 0x2000);
    com.add_ref_identity(&mut objects, id).unwrap();
    objects.close_alias(com.namespace(), 0x2000).unwrap();
    let count = objects.strong_count(id);
    assert!(com.release_identity(&mut objects, id).is_none());
    assert_eq!(objects.strong_count(id), count);
}

#[test]
fn process_exit_drops_live_com_payloads_once_with_outstanding_references() {
    let drops = Rc::new(Cell::new(0));
    {
        let mut objects = Objects::default();
        let com = Com::new(&mut objects).unwrap();
        let id = com
            .create(&mut objects, 0x1000, A, Payload(drops.clone()))
            .unwrap();
        com.register_interface(&mut objects, id, B, 0x2000);
        com.add_ref_identity(&mut objects, id).unwrap();
        com.query::<Payload>(&mut objects, 0x2000, IUNKNOWN)
            .unwrap();
        assert_eq!(drops.get(), 0);
    }
    assert_eq!(drops.get(), 1);
}
