//! D3D11 asynchronous object descriptions and retained results.
use crate::d3d11::ops::Error;
use adv_abi::types::D3D11_QUERY_DESC;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Event,
    Occlusion,
    Timestamp,
    Disjoint,
    PipelineStatistics,
    OcclusionPredicate,
}

impl Kind {
    pub fn size(self) -> u32 {
        match self {
            Self::Event | Self::OcclusionPredicate => 4,
            Self::Occlusion | Self::Timestamp => 8,
            Self::Disjoint => 16,
            Self::PipelineStatistics => 88,
        }
    }
}

pub struct Query {
    pub desc: D3D11_QUERY_DESC,
    pub predicate: bool,
    pub kind: Kind,
    #[cfg(feature = "gpu")]
    pub result: Option<Pending>,
    #[cfg(feature = "gpu")]
    pub active: Option<Active>,
}

#[cfg(feature = "gpu")]
#[derive(Default)]
pub struct Active {
    pub slots: Vec<adv_device::gpu::QuerySlot>,
    pub counters: [u64; 11],
}

#[cfg(feature = "gpu")]
pub struct Pending {
    pub buffer: Option<adv_gpu::Buffer>,
    pub completion: Option<std::sync::mpsc::Receiver<()>>,
    pub words: [u64; 11],
    pub last: Option<adv_device::gpu::QuerySlot>,
}

impl Query {
    pub fn new(desc: D3D11_QUERY_DESC, predicate: bool) -> Result<Self, Error> {
        let kind = match desc.Query {
            0 => Kind::Event,
            1 => Kind::Occlusion,
            2 => Kind::Timestamp,
            3 => Kind::Disjoint,
            4 => Kind::PipelineStatistics,
            5 => Kind::OcclusionPredicate,
            _ => return Err(Error::InvalidArgument),
        };
        if desc.MiscFlags & !1 != 0
            || (desc.MiscFlags != 0 && kind != Kind::OcclusionPredicate)
            || (predicate && kind != Kind::OcclusionPredicate)
        {
            return Err(Error::InvalidArgument);
        }
        Ok(Self {
            desc,
            predicate,
            kind,
            #[cfg(feature = "gpu")]
            result: None,
            #[cfg(feature = "gpu")]
            active: None,
        })
    }
}
