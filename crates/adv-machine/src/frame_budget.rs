//! Bounded frame diagnostics over the existing scheduler and HLE accounting.
//! No guest address, instruction bytes, resource names or host paths are exported.
use crate::stats::FrameRow;
use crate::thunk::ThunkTable;
use adv_cpu::frame_counters::FrameCounters;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

pub const BUDGET_NS: u64 = 33_300_000;
const CAPACITY: usize = 2048;
const NORMAL_CAPACITY: usize = 16;
const PERCENTILE_CAPACITY: usize = 4096;

#[derive(Clone, Debug)]
struct Row {
    serial: u64,
    startup: bool,
    wall_ns: u64,
    active_ns: u64,
    guest_exec_ns: u64,
    hle_ns: [u64; 4],
    present_call_ns: u64,
    wait_ns: u64,
    wait_overshoot_ns: u64,
    wait_timeouts: u64,
    cpu: Option<FrameCounters>,
    core: Option<u32>,
    frequency_khz: Option<u64>,
}
impl Row {
    fn json(&self) -> String {
        let optional = |v: Option<u64>| v.map_or_else(|| "null".into(), |v| v.to_string());
        format!(
            concat!(
                "{{\"serial\":{},\"startup\":{},\"wall_ns\":{},\"active_ns\":{},\"guest_exec_ns\":{},",
                "\"hle_ns\":{{\"graphics\":{},\"file\":{},\"audio\":{},\"other\":{}}},",
                "\"present_call_ns\":{},\"wait_ns\":{},\"wait_overshoot_ns\":{},\"wait_timeouts\":{},",
                "\"cpu\":{},\"core\":{},\"frequency_khz\":{},\"native_reference\":\"unmeasured\"}}"
            ),
            self.serial,
            self.startup,
            self.wall_ns,
            self.active_ns,
            self.guest_exec_ns,
            self.hle_ns[0],
            self.hle_ns[1],
            self.hle_ns[2],
            self.hle_ns[3],
            self.present_call_ns,
            self.wait_ns,
            self.wait_overshoot_ns,
            self.wait_timeouts,
            self.cpu.map_or_else(|| "null".into(), |s| s.json()),
            optional(self.core.map(u64::from)),
            optional(self.frequency_khz)
        )
    }
}

pub struct FrameBudget {
    last: Instant,
    pending: Option<(u64, Instant)>,
    previous: FrameRow,
    previous_cpu: Option<FrameCounters>,
    classes: Vec<usize>,
    hle_ns: [u64; 4],
    present_call_ns: u64,
    wait_ns: u64,
    overshoot_ns: u64,
    timeouts: u64,
    frames: u64,
    over: u64,
    active_over: u64,
    coalesced: u64,
    skipped_partial: bool,
    wall_total_ns: u64,
    active_total_ns: u64,
    max_wall_ns: u64,
    max_active_ns: u64,
    observer_ns: u64,
    rows: VecDeque<Row>,
    normal: VecDeque<Row>,
    percentiles: VecDeque<(u64, u64)>,
    longest: Option<Row>,
}
impl FrameBudget {
    pub fn new(previous: FrameRow, cpu: Option<FrameCounters>) -> Self {
        Self {
            last: Instant::now(),
            pending: None,
            previous,
            previous_cpu: cpu,
            classes: Vec::new(),
            hle_ns: [0; 4],
            present_call_ns: 0,
            wait_ns: 0,
            overshoot_ns: 0,
            timeouts: 0,
            frames: 0,
            over: 0,
            active_over: 0,
            coalesced: 0,
            skipped_partial: false,
            wall_total_ns: 0,
            active_total_ns: 0,
            max_wall_ns: 0,
            max_active_ns: 0,
            observer_ns: 0,
            rows: VecDeque::new(),
            normal: VecDeque::new(),
            percentiles: VecDeque::new(),
            longest: None,
        }
    }
    /// Reset excludes the first partial interval, exactly like shell Performance.
    pub fn reset(&mut self, previous: FrameRow, cpu: Option<FrameCounters>) {
        *self = Self::new(previous, cpu);
        self.skipped_partial = true;
    }
    pub fn mark_frame(&mut self, serial: u64) {
        self.coalesced += u64::from(self.pending.is_some());
        self.pending = Some((serial, Instant::now()));
    }
    pub fn pending(&self) -> bool {
        self.pending.is_some()
    }
    pub fn host(&mut self, ns: u64, thunk: u32, thunks: &ThunkTable) {
        let index = thunk as usize;
        while self.classes.len() <= index {
            let f = thunks.get(self.classes.len() as u32);
            self.classes.push(classify(&f.module, &f.name));
        }
        self.hle_ns[self.classes[index]] += ns;
        // The full presenting handler/continuation is an inclusive subset of HLE.
        if self.pending.is_some() {
            self.present_call_ns += ns;
        }
    }
    pub fn wait(&mut self, requested: Option<Duration>, elapsed: Duration, timed_out: bool) {
        self.wait_ns += elapsed.as_nanos() as u64;
        if timed_out {
            self.timeouts += 1;
            if let Some(requested) = requested {
                self.overshoot_ns += elapsed.saturating_sub(requested).as_nanos() as u64;
            }
        }
    }
    pub fn finish(&mut self, total: FrameRow, cpu: Option<FrameCounters>) {
        let Some((serial, at)) = self.pending.take() else {
            return;
        };
        let observer = Instant::now();
        let delta = total.sub(&self.previous);
        let row = Row {
            serial,
            startup: self.previous.frame == u32::MAX && !self.skipped_partial,
            wall_ns: at.saturating_duration_since(self.last).as_nanos() as u64,
            active_ns: delta.run_ns + delta.resume_ns,
            guest_exec_ns: delta.run_ns.saturating_sub(delta.hook_ns),
            hle_ns: self.hle_ns,
            present_call_ns: self.present_call_ns,
            wait_ns: self.wait_ns,
            wait_overshoot_ns: self.overshoot_ns,
            wait_timeouts: self.timeouts,
            cpu: cpu
                .zip(self.previous_cpu)
                .map(|(now, before)| now.since(before)),
            core: None,
            frequency_khz: None,
        };
        self.last = at;
        self.previous = FrameRow {
            frame: serial as u32,
            ..total
        };
        self.previous_cpu = cpu;
        self.hle_ns = [0; 4];
        self.present_call_ns = 0;
        self.wait_ns = 0;
        self.overshoot_ns = 0;
        self.timeouts = 0;
        if self.skipped_partial {
            self.skipped_partial = false;
        } else {
            self.record(row);
        }
        self.observer_ns += observer.elapsed().as_nanos() as u64;
    }
    fn record(&mut self, mut row: Row) {
        self.frames += 1;
        self.wall_total_ns += row.wall_ns;
        self.active_total_ns += row.active_ns;
        self.max_wall_ns = self.max_wall_ns.max(row.wall_ns);
        self.max_active_ns = self.max_active_ns.max(row.active_ns);
        self.active_over += u64::from(row.active_ns > BUDGET_NS);
        if self.percentiles.len() == PERCENTILE_CAPACITY {
            self.percentiles.pop_front();
        }
        self.percentiles.push_back((row.wall_ns, row.active_ns));
        if row.wall_ns > BUDGET_NS || row.active_ns > BUDGET_NS {
            // sysfs is never read on an ordinary frame or when disabled.
            (row.core, row.frequency_khz) = platform_sample();
            if row.wall_ns > BUDGET_NS {
                self.over += 1;
            }
            if self.rows.len() == CAPACITY {
                self.rows.pop_front();
            }
            self.rows.push_back(row.clone());
        } else if self.frames <= 4 || self.frames.is_multiple_of(120) {
            if self.normal.len() == NORMAL_CAPACITY {
                self.normal.pop_front();
            }
            self.normal.push_back(row.clone());
        }
        if self
            .longest
            .as_ref()
            .is_none_or(|longest| row.wall_ns > longest.wall_ns)
        {
            self.longest = Some(row);
        }
    }
    pub fn json(&self) -> String {
        let mut wall: Vec<_> = self.percentiles.iter().map(|x| x.0).collect();
        let mut active: Vec<_> = self.percentiles.iter().map(|x| x.1).collect();
        wall.sort_unstable();
        active.sort_unstable();
        let percentile = |values: &[u64], p: usize| {
            values
                .get((values.len() * p).div_ceil(100).saturating_sub(1))
                .map_or_else(|| "null".into(), |v| v.to_string())
        };
        let rows = self
            .rows
            .iter()
            .chain(&self.normal)
            .map(Row::json)
            .collect::<Vec<_>>()
            .join(",");
        format!(
            concat!(
                "{{\"schema\":\"adv-frame-budget-v1\",\"budget_ns\":{},\"frames\":{},",
                "\"over_budget_frames\":{},\"active_over_budget_frames\":{},\"max_wall_ns\":{},\"max_active_ns\":{},",
                "\"wall_total_ns\":{},\"active_total_ns\":{},\"p95_wall_ns\":{},\"p99_wall_ns\":{},\"p99_active_ns\":{},",
                "\"percentile_window\":{},\"percentile_capacity\":{},\"over_capacity\":{},\"coalesced_frames\":{},",
                "\"observer_ns\":{},\"longest\":{},\"rows\":[{}]}}"
            ),
            BUDGET_NS,
            self.frames,
            self.over,
            self.active_over,
            self.max_wall_ns,
            self.max_active_ns,
            self.wall_total_ns,
            self.active_total_ns,
            percentile(&wall, 95),
            percentile(&wall, 99),
            percentile(&active, 99),
            wall.len(),
            PERCENTILE_CAPACITY,
            CAPACITY,
            self.coalesced,
            self.observer_ns,
            self.longest
                .as_ref()
                .map_or_else(|| "null".into(), Row::json),
            rows
        )
    }
}

fn classify(module: &str, name: &str) -> usize {
    let module = module.to_ascii_lowercase();
    if ["gdi32", "gdiplus", "ddraw", "d3d", "windowscodecs"]
        .iter()
        .any(|m| module.starts_with(m))
    {
        0
    } else if ["dsound", "winmm", "xaudio", "vorbis", "quartz"]
        .iter()
        .any(|m| module.starts_with(m))
    {
        2
    } else if name.contains("File")
        || name.contains("Directory")
        || name.contains("Mapping")
        || matches!(
            name,
            "fopen" | "fread" | "fwrite" | "fseek" | "fclose" | "_open" | "_read" | "_write"
        )
    {
        1
    } else {
        3
    }
}

#[cfg(target_os = "android")]
fn platform_sample() -> (Option<u32>, Option<u64>) {
    let core = std::fs::read_to_string("/proc/thread-self/stat")
        .ok()
        .and_then(|s| {
            s.rsplit_once(')')
                .and_then(|(_, fields)| fields.split_whitespace().nth(36)?.parse::<u32>().ok())
        });
    let frequency = core.and_then(|cpu| {
        std::fs::read_to_string(format!(
            "/sys/devices/system/cpu/cpu{cpu}/cpufreq/scaling_cur_freq"
        ))
        .ok()?
        .trim()
        .parse()
        .ok()
    });
    (core, frequency)
}
#[cfg(not(target_os = "android"))]
fn platform_sample() -> (Option<u32>, Option<u64>) {
    (None, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn counts_survive_ring_eviction_and_waits_are_separate() {
        let mut b = FrameBudget::new(FrameRow::default(), Some(FrameCounters::default()));
        for serial in 1..=2200 {
            b.mark_frame(serial);
            b.wait(
                Some(Duration::from_millis(40)),
                Duration::from_millis(42),
                true,
            );
            b.pending.as_mut().unwrap().1 = b.last + Duration::from_millis(50);
            b.finish(
                FrameRow {
                    run_ns: serial * 8_000_000,
                    hook_ns: serial * 2_000_000,
                    ..Default::default()
                },
                Some(FrameCounters::default()),
            );
        }
        assert_eq!((b.frames, b.over, b.active_over), (2200, 2200, 0));
        assert_eq!(b.rows.len(), CAPACITY);
        let row = b.rows.back().unwrap();
        assert_eq!((row.active_ns, row.guest_exec_ns), (8_000_000, 6_000_000));
        assert_eq!(
            (row.wait_ns, row.wait_overshoot_ns),
            (42_000_000, 2_000_000)
        );
        assert!(b.json().contains("\"over_budget_frames\":2200"));
    }
    #[test]
    fn reset_discards_partial_frame_and_bounds_normal_controls() {
        let mut b = FrameBudget::new(FrameRow::default(), None);
        b.reset(FrameRow::default(), None);
        for serial in 1..=3000 {
            b.mark_frame(serial);
            b.pending.as_mut().unwrap().1 = b.last + Duration::from_millis(16);
            b.finish(FrameRow::default(), None);
        }
        assert_eq!((b.frames, b.over), (2999, 0));
        assert_eq!(b.normal.len(), NORMAL_CAPACITY);
        assert!(b.rows.is_empty());
    }
    #[test]
    fn host_groups_are_coarse_and_stable() {
        assert_eq!(classify("gdi32.dll", "BitBlt"), 0);
        assert_eq!(classify("kernel32.dll", "ReadFile"), 1);
        assert_eq!(classify("dsound.dll", "Unlock"), 2);
        assert_eq!(classify("user32.dll", "PeekMessageW"), 3);
    }

    #[test]
    fn first_run_presenting_immediately_marks_startup_only_once() {
        let total = FrameRow {
            frame: u32::MAX,
            ..Default::default()
        };
        let mut b = FrameBudget::new(total, None);
        b.mark_frame(1);
        b.finish(total, None);
        b.mark_frame(2);
        b.finish(FrameRow { frame: 1, ..total }, None);
        assert!(b.normal[0].startup);
        assert!(!b.normal[1].startup);
    }
}
