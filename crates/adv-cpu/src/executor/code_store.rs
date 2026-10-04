//! Process-owned translated code and lookup metadata. No guest register state lives here.
//! Publication stays on the controller; retirement drains native leases first.
use crate::{Region, compile::Compiled};
use std::collections::{BTreeMap, BTreeSet};

pub(super) struct CodeStore {
    pub(super) compiled: BTreeMap<u32, Compiled>,
    pub(super) regions: BTreeMap<u32, Region>,
    pub(super) page_regions: BTreeMap<u32, BTreeSet<u32>>,
    pub(super) region_pages: BTreeMap<u32, BTreeSet<u32>>,
    pub(super) entry_owner: BTreeMap<u32, u32>,
    pub(super) links: crate::linking::Table,
    pub(super) links_revision: u64,
    pub(super) cache_revision: u64,
}

impl Default for CodeStore {
    fn default() -> Self {
        Self {
            compiled: BTreeMap::new(),
            regions: BTreeMap::new(),
            page_regions: BTreeMap::new(),
            region_pages: BTreeMap::new(),
            entry_owner: BTreeMap::new(),
            links: crate::linking::Table::default(),
            links_revision: 0,
            cache_revision: 1,
        }
    }
}
