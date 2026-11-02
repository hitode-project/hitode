//! The flight recorder: a ring of recent planner inputs and outcomes. It holds
//! geometry and timings only, never image content, so it can go into public
//! reports. Tests replay it through [`crate::plan::plan`] and a headless
//! compositor; replays reproduce decision and bookkeeping bugs, not driver or
//! pixel bugs.

use crate::plan::{CompositorCaps, CostModel, FrameInfo, Plan, PoolState, Settings};
use std::time::Duration;

pub const CAPACITY: usize = 600;

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Present(Box<PresentRecord>),
    /// Window created, replaced, resized or lost; app backgrounded or resumed.
    Lifecycle(&'static str),
    /// A pool buffer came back from the compositor.
    Released {
        pool: u8,
        buffer: u8,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct PresentRecord {
    pub frame: FrameInfo,
    pub settings: Settings,
    pub caps: CompositorCaps,
    pub cost: CostModel,
    pub pools: PoolState,
    pub plan: Plan,
    /// GPU time per stage, in plan order, once known.
    pub stage_times: Vec<Duration>,
}

pub struct Recorder {
    _inner: (),
}

impl Recorder {
    pub fn record(&mut self, _event: Event) {
        todo!("D")
    }
    /// The ring, oldest first, as JSON lines for reports.
    pub fn dump(&self) -> String {
        todo!("D")
    }
}
