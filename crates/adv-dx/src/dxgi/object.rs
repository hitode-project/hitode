//! COM payloads. An adapter retains its factory and an output retains its adapter.
use super::caps::Adapter;
use super::state::Factory;
use adv_abi::com::Iid;
use std::collections::BTreeMap;

pub(super) enum Kind {
    Factory(Factory),
    Adapter(Adapter),
    Output,
}

pub(super) enum PrivateData {
    Bytes(Vec<u8>),
    Interface(u32),
}

pub(super) struct Object {
    pub kind: Kind,
    pub parent: Option<u32>,
    pub private: BTreeMap<Iid, PrivateData>,
    pub notifications: BTreeMap<u32, Notification>,
    pub next_cookie: u32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Notification {
    Stereo,
    Occlusion,
    Adapters,
    ContentProtection,
    MemoryBudget,
}
