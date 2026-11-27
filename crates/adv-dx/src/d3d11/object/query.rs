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
    StreamStatistics(u8),
    StreamOverflow(Option<u8>),
    OcclusionPredicate,
}

impl Kind {
    pub fn size(self) -> u32 {
        match self {
            Self::Event | Self::OcclusionPredicate | Self::StreamOverflow(_) => 4,
            Self::Occlusion | Self::Timestamp => 8,
            Self::Disjoint | Self::StreamStatistics(_) => 16,
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
    pub samples: Vec<(adv_device::gpu::BufferRange, Sample)>,
    pub counters: [u64; 11],
}

#[cfg(feature = "gpu")]
pub struct Pending {
    pub buffer: Option<adv_gpu::Buffer>,
    pub completion: Option<std::sync::mpsc::Receiver<()>>,
    pub words: [u64; 11],
    pub samples: Vec<Sample>,
    pub native_count: usize,
    pub predicate: Option<adv_device::gpu::BufferRange>,
}

#[cfg(feature = "gpu")]
#[derive(Clone, Copy)]
pub enum Sample {
    Stream,
    Predicate,
    Expansion,
    Indirect,
}

impl Kind {
    fn is_predicate(self) -> bool {
        matches!(self, Self::OcclusionPredicate | Self::StreamOverflow(_))
    }
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
            6 => Kind::StreamStatistics(0),
            7 => Kind::StreamOverflow(None),
            n @ (8 | 10 | 12 | 14) => Kind::StreamStatistics((n as u8 - 8) / 2),
            n @ (9 | 11 | 13 | 15) => Kind::StreamOverflow(Some((n as u8 - 9) / 2)),
            _ => return Err(Error::InvalidArgument),
        };
        if desc.MiscFlags & !1 != 0
            || (desc.MiscFlags != 0 && kind != Kind::OcclusionPredicate)
            || (predicate && !kind.is_predicate())
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
