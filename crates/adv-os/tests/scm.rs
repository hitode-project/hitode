//! Owned catalogs and process-local SCM aliases; no host-service probes.
use adv_machine::{ObjectLimits, Objects};
use adv_os::scm::*;
use std::sync::Arc;

fn service(name: &str, kind: u32, state: u32, group: &str) -> Service {
    Service {
        name: name.into(),
        allowed_access: SERVICE_READ,
        status: Status {
            service_type: kind,
            current_state: state,
            controls_accepted: 0,
            win32_exit_code: 0,
            service_specific_exit_code: 0,
            checkpoint: 0,
            wait_hint: 0,
            process_id: 0,
            service_flags: 0,
        },
        configuration: Configuration {
            service_type: kind,
            start_type: 3,
            error_control: 1,
            binary_path: "C:\\Fixture\\owned.exe".into(),
            load_order_group: group.into(),
            tag_id: 0,
            dependencies: vec![],
            account_name: "LocalSystem".into(),
            display_name: format!("{name} display"),
        },
    }
}
fn units(name: &str) -> Vec<u16> {
    name.encode_utf16().collect()
}

#[test]
fn default_database_has_a_real_shared_owner_and_kernel_opaque_connections() {
    let mut first = Scm::default();
    let second = Scm::default();
    assert!(Arc::ptr_eq(first.catalog(), second.catalog()));
    let mut objects = Objects::default();
    let manager = first.open_manager(&mut objects, 0).unwrap();
    assert_eq!(first.manager(&objects, manager, CONNECT).unwrap().len(), 0);
    assert_eq!(
        first
            .manager(&objects, manager, ENUMERATE_SERVICE)
            .unwrap_err(),
        ACCESS_DENIED
    );
    assert!(objects.get(manager).is_none());
    assert!(objects.duplicate(manager).is_none());
    assert!(objects.close(manager).is_none());
    assert_eq!(first.handle_count(&objects), 1);
    first.close(&mut objects, manager).unwrap();
    assert_eq!(objects.object_count(), 0);
    assert_eq!(first.close(&mut objects, manager), Err(INVALID_HANDLE));
}

#[test]
fn implicit_connect_read_mapping_and_write_denial_are_actual_grants() {
    assert_eq!(manager_access(0), Ok(CONNECT));
    assert_eq!(
        manager_access(ENUMERATE_SERVICE),
        Ok(CONNECT | ENUMERATE_SERVICE)
    );
    assert_eq!(manager_access(0x80000000), Ok(MANAGER_READ));
    assert_eq!(manager_access(0x02000000), Ok(MANAGER_READ));
    for rights in [2, 8, 0x20, 0x40, 0xf003f, 0x10000000, 0x40000000] {
        assert_eq!(manager_access(rights), Err(ACCESS_DENIED));
    }
}

#[test]
fn catalog_filtering_respects_status_groups_user_templates_and_service_permissions() {
    let mut hidden = service("hidden", 0x10, 1, "");
    hidden.allowed_access = QUERY_CONFIG;
    let catalog = Catalog::from_services(vec![
        service("ordinary", 0x10, 1, "OwnedGroup"),
        service("driver", 1, 4, "ownedgroup"),
        service("interactive", 0x110, 1, ""),
        service("template", 0x50, 1, ""),
        service("instance", 0xd0, 1, ""),
        hidden,
    ])
    .unwrap();
    let names = |types, state, group: Option<&[u16]>| {
        catalog
            .enumerate(types, state, group)
            .unwrap()
            .iter()
            .map(|s| s.name.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        names(0x30, 3, None),
        ["instance", "interactive", "ordinary"]
    );
    assert_eq!(
        names(0x130, 3, None),
        ["instance", "interactive", "ordinary"]
    );
    assert_eq!(
        names(0x3ff, 3, None),
        ["driver", "instance", "interactive", "ordinary", "template"]
    );
    assert_eq!(names(0x30, 3, Some(&[])), ["instance", "interactive"]);
    assert_eq!(names(0x3ff, 1, Some(&units("OWNEDGROUP"))), ["driver"]);
    assert_eq!(
        catalog
            .enumerate(0x30, 3, Some(&units("missing")))
            .unwrap_err(),
        SERVICE_DOES_NOT_EXIST
    );
    assert_eq!(
        catalog.enumerate(0, 3, None).unwrap_err(),
        INVALID_PARAMETER
    );
    assert_eq!(
        catalog.enumerate(0x30, 0, None).unwrap_err(),
        INVALID_PARAMETER
    );
}

#[test]
fn service_connections_survive_manager_close_and_reject_wrong_kind_or_process_table() {
    let catalog = Arc::new(Catalog::from_services(vec![service("Owned", 0x10, 1, "")]).unwrap());
    let mut parent = Scm::with_catalog(catalog.clone());
    let mut child = Scm::with_catalog(catalog.clone());
    let mut parent_objects = Objects::default();
    let mut child_objects = Objects::default();
    let manager = parent.open_manager(&mut parent_objects, 0).unwrap();
    assert_eq!(
        parent.manager(&child_objects, manager, 0).unwrap_err(),
        INVALID_HANDLE
    );
    assert_eq!(
        child.manager(&child_objects, manager, 0).unwrap_err(),
        INVALID_HANDLE
    );
    let service = parent
        .open_service(&mut parent_objects, manager, &units("oWnEd"), QUERY_CONFIG)
        .unwrap();
    assert_eq!(
        parent.manager(&parent_objects, service, 0).unwrap_err(),
        INVALID_HANDLE
    );
    assert_eq!(
        parent.service(&parent_objects, manager, 0).unwrap_err(),
        INVALID_HANDLE
    );
    assert_eq!(
        parent.open_service(&mut parent_objects, manager, &units("missing"), 1),
        Err(SERVICE_DOES_NOT_EXIST)
    );
    assert_eq!(
        parent.open_service(&mut parent_objects, manager, &units("Owned"), 2),
        Err(ACCESS_DENIED)
    );
    parent.close(&mut parent_objects, manager).unwrap();
    assert_eq!(
        parent
            .service(&parent_objects, service, QUERY_CONFIG)
            .unwrap()
            .name,
        "Owned"
    );
    assert_eq!(
        parent
            .service(&parent_objects, service, QUERY_STATUS)
            .unwrap_err(),
        ACCESS_DENIED
    );
    let child_manager = child
        .open_manager(&mut child_objects, ENUMERATE_SERVICE)
        .unwrap();
    assert!(Arc::ptr_eq(
        &parent.catalog().clone(),
        &child
            .manager(&child_objects, child_manager, ENUMERATE_SERVICE)
            .unwrap()
    ));
    parent.close(&mut parent_objects, service).unwrap();
    child.close(&mut child_objects, child_manager).unwrap();
    assert_eq!(parent_objects.object_count(), 0);
    assert_eq!(child_objects.object_count(), 0);
}

#[test]
fn invalid_provider_records_and_failed_allocations_publish_no_connection() {
    assert_eq!(
        Catalog::from_services(vec![service("bad/name", 0x10, 1, "")]).unwrap_err(),
        INVALID_NAME
    );
    assert_eq!(
        Catalog::from_services(vec![
            service("same", 0x10, 1, ""),
            service("SAME", 0x10, 1, "")
        ])
        .unwrap_err(),
        INVALID_NAME
    );
    let mut invalid = service("owned", 0x10, 1, "");
    invalid.status.process_id = 12;
    assert_eq!(
        Catalog::from_services(vec![invalid]).unwrap_err(),
        INVALID_PARAMETER
    );
    let mut objects = Objects::with_limits(ObjectLimits {
        objects: 0,
        ..ObjectLimits::default()
    });
    let mut owner = Scm::default();
    assert_eq!(owner.open_manager(&mut objects, 4), Err(NO_MEMORY));
    assert_eq!(owner.handle_count(&objects), 0);
    assert_eq!(objects.object_count(), 0);
}
