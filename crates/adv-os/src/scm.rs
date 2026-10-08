//! Read-only ServicesActive database and process-local SCM connections.
//!
//! The immutable catalog is an actual virtual OS owner, never a host-service
//! snapshot. Providers may share its Arc across a process family. The default
//! profile has no installed services; it still has a valid empty database.
//! Manager and service aliases belong to a separate Objects namespace.
use adv_machine::{Namespace, Objects};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, OnceLock};

pub const ACCESS_DENIED: u32 = 5;
pub const INVALID_HANDLE: u32 = 6;
pub const NO_MEMORY: u32 = 8;
pub const INVALID_PARAMETER: u32 = 87;
pub const INVALID_NAME: u32 = 123;
pub const SERVICE_DOES_NOT_EXIST: u32 = 1060;
pub const CONNECT: u32 = 1;
pub const ENUMERATE_SERVICE: u32 = 4;
pub const QUERY_CONFIG: u32 = 1;
pub const QUERY_STATUS: u32 = 4;
pub const READ_CONTROL: u32 = 0x20000;
pub const MANAGER_READ: u32 = READ_CONTROL | CONNECT | ENUMERATE_SERVICE | 0x10;
pub const SERVICE_READ: u32 = READ_CONTROL | QUERY_CONFIG | QUERY_STATUS | 8 | 0x80;
pub const MAX_ENUM_BYTES: usize = 256 * 1024;
pub const MAX_CONFIG_BYTES: usize = 8 * 1024;

/// Owned service metadata. A provider supplies the recorded status, not the SCM
/// frontend. Querying this record does not launch or control a host process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Status {
    pub service_type: u32,
    pub current_state: u32,
    pub controls_accepted: u32,
    pub win32_exit_code: u32,
    pub service_specific_exit_code: u32,
    pub checkpoint: u32,
    pub wait_hint: u32,
    pub process_id: u32,
    pub service_flags: u32,
}

impl Status {
    pub fn words(&self) -> [u32; 9] {
        [
            self.service_type,
            self.current_state,
            self.controls_accepted,
            self.win32_exit_code,
            self.service_specific_exit_code,
            self.checkpoint,
            self.wait_hint,
            self.process_id,
            self.service_flags,
        ]
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Configuration {
    pub service_type: u32,
    pub start_type: u32,
    pub error_control: u32,
    pub binary_path: String,
    pub load_order_group: String,
    pub tag_id: u32,
    pub dependencies: Vec<String>,
    pub account_name: String,
    pub display_name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Service {
    pub name: String,
    pub status: Status,
    pub configuration: Configuration,
    /// Read permissions granted by this provider's ordinary guest account.
    pub allowed_access: u32,
}

#[derive(Default, Debug)]
pub struct Catalog {
    services: BTreeMap<Vec<u16>, Arc<Service>>,
    groups: BTreeSet<Vec<u16>>,
}

fn fold(name: &[u16]) -> Vec<u16> {
    name.iter()
        .map(|&u| crate::nls::unit_case(u, true))
        .collect()
}

pub fn valid_service_name(name: &[u16]) -> bool {
    !name.is_empty() && name.len() <= 256 && !name.iter().any(|u| matches!(u, 0 | 47 | 92))
}

impl Catalog {
    /// Publish a bounded, immutable provider catalog. Names, status and config
    /// are validated before any record is published. No guest API installs rows.
    pub fn from_services(services: Vec<Service>) -> Result<Self, u32> {
        if services.len() > 16_384 {
            return Err(NO_MEMORY);
        }
        let mut catalog = Self::default();
        let mut total = 0usize;
        for service in services {
            let name: Vec<_> = service.name.encode_utf16().collect();
            let config = &service.configuration;
            let base_type = config.service_type & 0x3f;
            let strings = [
                &config.binary_path,
                &config.load_order_group,
                &config.account_name,
                &config.display_name,
            ];
            let units = strings
                .iter()
                .map(|s| s.encode_utf16().count() + 1)
                .sum::<usize>()
                + config
                    .dependencies
                    .iter()
                    .map(|s| s.encode_utf16().count() + 1)
                    .sum::<usize>()
                + 2;
            if !valid_service_name(&name) || catalog.services.contains_key(&fold(&name)) {
                return Err(INVALID_NAME);
            }
            if service.status.service_type != config.service_type
                || !matches!(base_type, 1 | 2 | 4 | 8 | 0x10 | 0x20)
                || config.service_type & !0x3ff != 0
                || (config.service_type & 0x3c0 != 0 && base_type < 0x10)
                || (config.service_type & 0x80 != 0 && config.service_type & 0x40 == 0)
                || (config.start_type <= 1 && base_type >= 0x10)
                || !(1..=7).contains(&service.status.current_state)
                || ((service.status.current_state == 1 || base_type < 0x10)
                    && service.status.process_id != 0)
                || config.start_type > 4
                || config.error_control > 3
                || config.display_name.encode_utf16().count() > 256
                || strings
                    .iter()
                    .copied()
                    .chain(config.dependencies.iter())
                    .any(|s| s.contains('\0'))
                || config.dependencies.iter().any(|s| s.is_empty())
                || service.allowed_access & !SERVICE_READ != 0
                || units * 2 + 36 > MAX_CONFIG_BYTES
            {
                return Err(INVALID_PARAMETER);
            }
            total += units * 2 + name.len() * 2 + 80;
            if total > 32 * 1024 * 1024 {
                return Err(NO_MEMORY);
            }
            if !config.load_order_group.is_empty() {
                catalog.groups.insert(fold(
                    &config.load_order_group.encode_utf16().collect::<Vec<_>>(),
                ));
            }
            catalog.services.insert(fold(&name), Arc::new(service));
        }
        Ok(catalog)
    }

    pub fn len(&self) -> usize {
        self.services.len()
    }
    pub fn is_empty(&self) -> bool {
        self.services.is_empty()
    }

    pub fn service(&self, name: &[u16]) -> Result<Arc<Service>, u32> {
        if !valid_service_name(name) {
            return Err(INVALID_NAME);
        }
        self.services
            .get(&fold(name))
            .cloned()
            .ok_or(SERVICE_DOES_NOT_EXIST)
    }

    /// Sorted, stable enumeration of this immutable database. A nonempty group
    /// which is not installed fails with 1060, as the native SCM does.
    pub fn enumerate(
        &self,
        types: u32,
        state: u32,
        group: Option<&[u16]>,
    ) -> Result<Vec<Arc<Service>>, u32> {
        if types == 0 || types & !0x3ff != 0 || !(1..=3).contains(&state) {
            return Err(INVALID_PARAMETER);
        }
        let folded = group.map(fold);
        if let Some(group) = &folded {
            if !group.is_empty() && !self.groups.contains(group) {
                return Err(SERVICE_DOES_NOT_EXIST);
            }
        }
        Ok(self.services.values().filter(|service| {
            let status = &service.status;
            let state_bit = if status.current_state == 1 { 2 } else { 1 };
            status.service_type & types != 0 && state & state_bit != 0
                // Plain WIN32 includes per-user instances, but templates require
                // the USER_SERVICE selector. Native read-only enumeration agrees.
                && !(status.service_type & 0xc0 == 0x40 && types & 0x40 == 0)
                && service.allowed_access & QUERY_STATUS != 0
                && folded.as_ref().is_none_or(|group| {
                    *group == fold(&service.configuration.load_order_group.encode_utf16().collect::<Vec<_>>())
                })
        }).cloned().collect())
    }
}

fn map_access(mut access: u32, service: bool, allowed: u32) -> Result<u32, u32> {
    let mappings = if service {
        [
            SERVICE_READ,
            READ_CONTROL | 2,
            READ_CONTROL | 0x170,
            0xf01ff,
        ]
    } else {
        [
            READ_CONTROL | 0x14,
            READ_CONTROL | 0x22,
            READ_CONTROL | 9,
            0xf003f,
        ]
    };
    for (bit, mapped) in [0x80000000, 0x40000000, 0x20000000, 0x10000000]
        .into_iter()
        .zip(mappings)
    {
        if access & bit != 0 {
            access = (access & !bit) | mapped;
        }
    }
    if access & 0x02000000 != 0 {
        access = (access & !0x02000000) | allowed;
    }
    if !service {
        access |= CONNECT;
    }
    if access & !allowed != 0 {
        Err(ACCESS_DENIED)
    } else {
        Ok(access)
    }
}

pub fn manager_access(access: u32) -> Result<u32, u32> {
    map_access(access, false, MANAGER_READ)
}

#[derive(Debug)]
enum OpenObject {
    Manager { catalog: Arc<Catalog>, access: u32 },
    Service { service: Arc<Service>, access: u32 },
}

/// Process-local connection owner. Only the immutable catalog crosses process
/// boundaries; a Namespace token and all its aliases stay with their Objects.
pub struct Scm {
    catalog: Arc<Catalog>,
    namespace: Option<Namespace>,
}

impl Default for Scm {
    fn default() -> Self {
        static EMPTY: OnceLock<Arc<Catalog>> = OnceLock::new();
        Self::with_catalog(EMPTY.get_or_init(|| Arc::new(Catalog::default())).clone())
    }
}

impl Scm {
    pub fn with_catalog(catalog: Arc<Catalog>) -> Self {
        Self {
            catalog,
            namespace: None,
        }
    }
    pub fn catalog(&self) -> &Arc<Catalog> {
        &self.catalog
    }
    pub fn handle_count(&self, objects: &Objects) -> usize {
        self.namespace.map_or(0, |ns| objects.alias_count(ns))
    }

    fn allocate(&mut self, objects: &mut Objects, payload: OpenObject) -> Result<u32, u32> {
        let namespace = match self.namespace {
            Some(namespace) => namespace,
            None => {
                let namespace = objects
                    .new_namespace_in(0x6900_0000, 0x69ff_fffc)
                    .ok_or(NO_MEMORY)?;
                self.namespace = Some(namespace);
                namespace
            }
        };
        objects.insert_host_in(namespace, payload).ok_or(NO_MEMORY)
    }

    fn opened<'a>(&self, objects: &'a Objects, handle: u32) -> Result<&'a OpenObject, u32> {
        let identity = objects
            .lookup_alias::<OpenObject>(self.namespace.ok_or(INVALID_HANDLE)?, handle)
            .ok_or(INVALID_HANDLE)?;
        objects.owned(identity).ok_or(INVALID_HANDLE)
    }

    pub fn open_manager(&mut self, objects: &mut Objects, desired: u32) -> Result<u32, u32> {
        let access = manager_access(desired)?;
        self.allocate(
            objects,
            OpenObject::Manager {
                catalog: self.catalog.clone(),
                access,
            },
        )
    }

    pub fn manager(
        &self,
        objects: &Objects,
        handle: u32,
        required: u32,
    ) -> Result<Arc<Catalog>, u32> {
        match self.opened(objects, handle)? {
            OpenObject::Manager { catalog, access } if access & required == required => {
                Ok(catalog.clone())
            }
            OpenObject::Manager { .. } => Err(ACCESS_DENIED),
            _ => Err(INVALID_HANDLE),
        }
    }

    pub fn open_service(
        &mut self,
        objects: &mut Objects,
        manager: u32,
        name: &[u16],
        desired: u32,
    ) -> Result<u32, u32> {
        let catalog = self.manager(objects, manager, CONNECT)?;
        let service = catalog.service(name)?;
        let access = map_access(desired, true, service.allowed_access)?;
        self.allocate(objects, OpenObject::Service { service, access })
    }

    pub fn service(
        &self,
        objects: &Objects,
        handle: u32,
        required: u32,
    ) -> Result<Arc<Service>, u32> {
        match self.opened(objects, handle)? {
            OpenObject::Service { service, access } if access & required == required => {
                Ok(service.clone())
            }
            OpenObject::Service { .. } => Err(ACCESS_DENIED),
            _ => Err(INVALID_HANDLE),
        }
    }

    pub fn close(&self, objects: &mut Objects, handle: u32) -> Result<(), u32> {
        self.opened(objects, handle)?;
        objects
            .close_alias(self.namespace.ok_or(INVALID_HANDLE)?, handle)
            .ok_or(INVALID_HANDLE)?;
        Ok(())
    }
}
