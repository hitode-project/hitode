//! A bounded JSON-lines flight recorder of geometry and timings, never pixels.
//! Replay uses each recorded input ledger, so the first mismatch identifies a
//! planner decision rather than depending on a GPU or platform compositor.

use crate::plan::{CompositorCaps, FrameInfo, Plan, PoolState, Settings, plan};
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlanKind {
    #[default]
    Direct,
    Merged,
    Upscaled,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StageStats {
    pub tiles: u32,
    pub pixels: u64,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Statistics {
    pub kind: PlanKind,
    pub stages: [StageStats; 4],
    pub passes: u32,
    pub submissions: u32,
    pub latched: Option<Duration>,
    pub shown: Option<Duration>,
    /// This present's serial: the number of presents up to it.
    pub presents: u64,
}

/// Running totals since the display was created.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Totals {
    pub presents: u64,
    pub software_frames: u64,
    pub software_bytes: u64,
    pub software_started: Option<Duration>,
    pub direct_presents: u64,
    pub configurations: u64,
    pub max_cpu: Duration,
    pub max_queue: Duration,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PresentRecord {
    pub statistics: Statistics,
    pub token: u64,
    pub published_at: Duration,
    pub at: Duration,
    pub cpu_time: Duration,
    pub frame: FrameInfo,
    pub settings: Settings,
    pub caps: CompositorCaps,
    pub pools: PoolState,
    pub plan: Plan,
    /// GPU time per stage, in plan order, once known.
    pub stage_times: Vec<Duration>,
}

#[derive(Default)]
pub struct Recorder {
    events: VecDeque<Event>,
    present_history: bool,
    presents: usize,
}

impl Recorder {
    /// Executor history retains 600 presents independently of release traffic.
    pub(crate) fn present_history() -> Self {
        Self {
            present_history: true,
            ..Self::default()
        }
    }
    pub fn record(&mut self, event: Event) {
        if self.present_history {
            if matches!(event, Event::Present(_)) {
                if self.presents == CAPACITY {
                    while let Some(old) = self.events.pop_front() {
                        if matches!(old, Event::Present(_)) {
                            self.presents -= 1;
                            break;
                        }
                    }
                }
                self.presents += 1;
            } else if self.events.len() - self.presents == CAPACITY * 8 {
                // A submission hands off at most eight images. Bound ancillary
                // events separately so releases cannot evict planner records.
                let oldest = self
                    .events
                    .iter()
                    .position(|e| !matches!(e, Event::Present(_)))
                    .unwrap();
                self.events.remove(oldest);
            }
        } else if self.events.len() == CAPACITY {
            self.events.pop_front();
        }
        self.events.push_back(event);
    }

    pub fn latest(&self) -> Option<&PresentRecord> {
        self.events.iter().rev().find_map(|e| {
            if let Event::Present(r) = e {
                Some(r.as_ref())
            } else {
                None
            }
        })
    }
    pub fn get(&self, serial: u64) -> Option<&PresentRecord> {
        self.events.iter().rev().find_map(|e| match e {
            Event::Present(r) if r.statistics.presents == serial => Some(r.as_ref()),
            _ => None,
        })
    }
    pub(crate) fn get_mut(&mut self, serial: u64) -> Option<&mut PresentRecord> {
        self.events.iter_mut().rev().find_map(|e| {
            if let Event::Present(r) = e {
                (r.statistics.presents == serial).then_some(r.as_mut())
            } else {
                None
            }
        })
    }
    pub(crate) fn submission_mut(&mut self, token: u64) -> Option<&mut PresentRecord> {
        self.events.iter_mut().rev().find_map(|e| {
            if let Event::Present(r) = e {
                (r.token == token).then_some(r.as_mut())
            } else {
                None
            }
        })
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
            let replayed = plan(&record.frame, &record.settings, &record.caps, &record.pools);
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
