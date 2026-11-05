//! Independently owned depth/stencil images. Color targets borrow these handles;
//! switching a target never copies or reinitializes the attachment's contents.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum DepthFormat {
    Float32,
    Unorm16,
    Depth24Stencil8,
}
impl DepthFormat {
    pub fn has_stencil(self) -> bool {
        self == Self::Depth24Stencil8
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum StencilOp {
    Keep,
    Zero,
    Replace,
    IncrementClamp,
    DecrementClamp,
    Invert,
    IncrementWrap,
    DecrementWrap,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct StencilFace {
    pub compare: Compare,
    pub fail: StencilOp,
    pub depth_fail: StencilOp,
    pub pass: StencilOp,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Stencil {
    pub front: StencilFace,
    pub back: StencilFace,
    pub read_mask: u32,
    pub write_mask: u32,
}
