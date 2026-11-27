//! COM payloads. An adapter retains its factory and an output retains its adapter.
use super::abi::DXGI_MODE_DESC;
use super::caps::Adapter;
use super::state::Factory;
use adv_abi::com::Iid;
use std::collections::BTreeMap;

pub(super) enum Kind {
    Factory(Factory),
    Adapter(Adapter),
    Output,
    SwapChain(Box<super::swap::Chain>),
    Duplication(Box<Duplication>),
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

#[cfg(feature = "gpu")]
type Capture = std::sync::mpsc::Receiver<Result<adv_device::frame::Captured, String>>;
#[cfg(feature = "gpu")]
pub(super) type Publication = (u64, u64);

pub(super) struct Duplication {
    pub device: u32,
    pub mode: DXGI_MODE_DESC,
    pub epoch: u64,
    pub held: bool,
    #[cfg(feature = "gpu")]
    pub last: Option<Publication>,
    #[cfg(feature = "gpu")]
    pub capture: Option<(Publication, Capture)>,
}
