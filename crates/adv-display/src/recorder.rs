//! A bounded JSON-lines flight recorder of geometry and timings, never pixels.
//! Replay uses each recorded input ledger, so the first mismatch identifies a
//! planner decision rather than depending on a GPU or platform compositor.

use crate::plan::{CompositorCaps, CostModel, FrameInfo, Plan, PoolState, Settings, plan};
use serde::{Deserialize, Serialize};
use std::{collections::VecDeque, time::Duration};

pub const CAPACITY: usize = 600;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Event {
    Present(Box<PresentRecord>),
    /// Window and application lifecycle changes.
    Lifecycle(String),
    /// Release order, without image content or platform handles.
    Released {
        pool: u8,
        buffer: usize,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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

#[derive(Default)]
pub struct Recorder {
    events: VecDeque<Event>,
}

impl Recorder {
    pub fn record(&mut self, event: Event) {
        if self.events.len() == CAPACITY {
            self.events.pop_front();
        }
        self.events.push_back(event);
    }

    pub fn events(&self) -> impl Iterator<Item = &Event> {
        self.events.iter()
    }

    /// JSON lines, oldest first. All event fields have JSON representations.
    pub fn dump(&self) -> String {
        let mut dump = String::new();
        for event in &self.events {
            dump.push_str(&serde_json::to_string(event).expect("serializable recorder event"));
            dump.push('\n');
        }
        dump
    }
}

/// Parse the complete stream, without truncating it to the live ring capacity.
pub fn parse(json_lines: &str) -> Result<Vec<Event>, serde_json::Error> {
    json_lines.lines().map(serde_json::from_str).collect()
}

#[derive(Clone, Debug, PartialEq)]
pub struct ReplayMismatch {
    /// Zero-based event index, including lifecycle and release events.
    pub record: usize,
    pub serial: u64,
    pub recorded: Box<Plan>,
    pub replayed: Box<Plan>,
}

pub fn replay(events: &[Event]) -> Result<(), ReplayMismatch> {
    for (index, event) in events.iter().enumerate() {
        if let Event::Present(record) = event {
            let replayed = plan(
                &record.frame,
                &record.settings,
                &record.caps,
                &record.cost,
                &record.pools,
            );
            if replayed != record.plan {
                return Err(ReplayMismatch {
                    record: index,
                    serial: record.frame.serial,
                    recorded: Box::new(record.plan.clone()),
                    replayed: Box::new(replayed),
                });
            }
        }
    }
    Ok(())
}
