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

#[derive(Clone, Debug, Default)]
struct Api {
    name: String,
    ns: u64,
    calls: u64,
}
impl Api {
    fn json(&self) -> String {
        // Export names are untrusted input; never interpolate raw JSON strings.
        let name = self
            .name
            .chars()
            .map(|c| match c {
                '\\' => "\\\\".into(),
                '"' => "\\\"".into(),
                c if c.is_control() => format!("\\u{:04x}", c as u32),
                c => c.to_string(),
            })
            .collect::<String>();
        format!(
            "{{\"api\":\"{}\",\"ns\":{},\"calls\":{}}}",
            name, self.ns, self.calls
        )
    }
}

#[derive(Clone, Debug)]
struct Row {
    serial: u64,
    startup: bool,
    wall_ns: u64,
    present_interval_ns: Option<u64>,
    guest_delta_us: u64,
    file_read_bytes: u64,
    operation_ns: [u64; 3],
    active_ns: u64,
    guest_exec_ns: u64,
    service_audio_ns: u64,
    service_graphics_ns: u64,
    scheduler_ns: u64,
    frontend_ns: u64,
    control_pause_ns: u64,
    scene_check_ns: u64,
    accounting_excess_ns: u64,
    hle_ns: [u64; 4],
    top_apis: Vec<Api>,
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
                "\"present_interval_ns\":{},\"guest_delta_us\":{},\"file_read_bytes\":{},",
                "\"operation_ns\":{{\"text\":{},\"image_decode\":{},\"audio_decode\":{}}},",
                "\"service_audio_ns\":{},\"service_graphics_ns\":{},",
                "\"scheduler_ns\":{},\"frontend_ns\":{},\"control_pause_ns\":{},\"scene_check_ns\":{},\"accounting_excess_ns\":{},",
                "\"hle_ns\":{{\"graphics\":{},\"file\":{},\"audio\":{},\"other\":{}}},\"top_apis\":[{}],",
                "\"present_call_ns\":{},\"wait_ns\":{},\"wait_overshoot_ns\":{},\"wait_timeouts\":{},",
                "\"cpu\":{},\"core\":{},\"frequency_khz\":{},\"native_reference\":\"unmeasured\"}}"
            ),
            self.serial,
            self.startup,
            self.wall_ns,
            self.active_ns,
            self.guest_exec_ns,
            optional(self.present_interval_ns),
            self.guest_delta_us,
            self.file_read_bytes,
            self.operation_ns[0],
            self.operation_ns[1],
            self.operation_ns[2],
            self.service_audio_ns,
            self.service_graphics_ns,
            self.scheduler_ns,
            self.frontend_ns,
            self.control_pause_ns,
            self.scene_check_ns,
            self.accounting_excess_ns,
            self.hle_ns[0],
            self.hle_ns[1],
            self.hle_ns[2],
            self.hle_ns[3],
            self.top_apis
                .iter()
                .map(Api::json)
                .collect::<Vec<_>>()
                .join(","),
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
    last_present: Option<Instant>,
    pending: Option<(u64, Instant)>,
    previous: FrameRow,
    previous_cpu: Option<FrameCounters>,
    classes: Vec<usize>,
    operations: Vec<Option<usize>>,
    apis: Vec<Api>,
    touched: Vec<usize>,
    scheduler_started: Option<Instant>,
    machine_ns: u64,
    control_pause_ns: u64,
    scene_check_ns: u64,
    hle_ns: [u64; 4],
    operation_ns: [u64; 3],
    file_read_bytes: u64,
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
    sequence: VecDeque<Row>,
    longest: Option<Row>,
}
impl FrameBudget {
    pub fn new(previous: FrameRow, cpu: Option<FrameCounters>) -> Self {
        Self {
            last: Instant::now(),
            last_present: None,
            pending: None,
            previous,
            previous_cpu: cpu,
            classes: Vec::new(),
            operations: Vec::new(),
            apis: Vec::new(),
            touched: Vec::new(),
            scheduler_started: None,
            machine_ns: 0,
            control_pause_ns: 0,
            scene_check_ns: 0,
            hle_ns: [0; 4],
            operation_ns: [0; 3],
            file_read_bytes: 0,
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
            sequence: VecDeque::new(),
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
            self.operations.push(operation(&f.module, &f.name));
            self.apis.push(Api {
                name: format!("{}!{}", f.module, f.name),
                ..Default::default()
            });
        }
        let api = &mut self.apis[index];
        if api.calls == 0 {
            self.touched.push(index);
        }
        api.ns += ns;
        api.calls += 1;
        self.hle_ns[self.classes[index]] += ns;
        if let Some(operation) = self.operations[index] {
            self.operation_ns[operation] += ns;
        }
        // The full presenting handler/continuation is an inclusive subset of HLE.
        if self.pending.is_some() {
            self.present_call_ns += ns;
        }
    }
    pub fn enter_scheduler(&mut self) {
        debug_assert!(self.scheduler_started.is_none());
        self.scheduler_started = Some(Instant::now());
    }
    /// ReadFile bytes including the live NT bridge, not physical disk IO.
    pub fn file_read(&mut self, bytes: u64) {
        self.file_read_bytes += bytes;
    }
    pub fn leave_scheduler(&mut self) {
        if let Some(started) = self.scheduler_started.take() {
            self.machine_ns += started.elapsed().as_nanos() as u64;
        }
    }
    /// Explicit diagnostic freezes are not guest waits or scheduler work.
    pub fn control_pause(&mut self, elapsed: Duration) {
        self.control_pause_ns += elapsed.as_nanos() as u64;
    }
    /// Reuse the scene gate's existing timing; this adds no clock read.
    pub fn scene_check(&mut self, elapsed: Duration) {
        self.scene_check_ns += elapsed.as_nanos() as u64;
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
        let Some((serial, presented_at)) = self.pending.take() else {
            return;
        };
        let observer = Instant::now();
        // Align wall and all cost counters at the same CPU-return boundary.
        let at = observer;
        if let Some(started) = self.scheduler_started.replace(at) {
            self.machine_ns += at.saturating_duration_since(started).as_nanos() as u64;
        } else {
            self.scheduler_started = None;
        }
        let delta = total.sub(&self.previous);
        let active_ns = delta.run_ns + delta.resume_ns;
        let wall_ns = at.saturating_duration_since(self.last).as_nanos() as u64;
        let scheduler_ns = self.machine_ns.saturating_sub(active_ns);
        let accounted_ns = active_ns
            + scheduler_ns
            + self.wait_ns
            + self.control_pause_ns
            + self.scene_check_ns
            + delta.frontend_audio_ns
            + delta.frontend_render_ns;
        self.touched
            .sort_unstable_by_key(|&i| (std::cmp::Reverse(self.apis[i].ns), i));
        let top_apis = self
            .touched
            .iter()
            .take(5)
            .map(|&i| self.apis[i].clone())
            .collect();
        for i in self.touched.drain(..) {
            self.apis[i].ns = 0;
            self.apis[i].calls = 0;
        }
        let row = Row {
            serial,
            startup: self.previous.frame == u32::MAX && !self.skipped_partial,
            wall_ns,
            present_interval_ns: self
                .last_present
                .map(|last| presented_at.saturating_duration_since(last).as_nanos() as u64),
            guest_delta_us: total.guest_us.saturating_sub(self.previous.guest_us),
            file_read_bytes: self.file_read_bytes,
            operation_ns: self.operation_ns,
            active_ns,
            guest_exec_ns: delta.run_ns.saturating_sub(delta.hook_ns),
            service_audio_ns: delta.frontend_audio_ns,
            service_graphics_ns: delta.frontend_render_ns,
            scheduler_ns,
            frontend_ns: wall_ns.saturating_sub(accounted_ns),
            control_pause_ns: self.control_pause_ns,
            scene_check_ns: self.scene_check_ns,
            accounting_excess_ns: accounted_ns.saturating_sub(wall_ns),
            hle_ns: self.hle_ns,
            top_apis,
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
        self.last_present = Some(presented_at);
        self.previous = FrameRow {
            frame: serial as u32,
            ..total
        };
        self.previous_cpu = cpu;
        self.hle_ns = [0; 4];
        self.operation_ns = [0; 3];
        self.file_read_bytes = 0;
        self.present_call_ns = 0;
        self.wait_ns = 0;
        self.overshoot_ns = 0;
        self.timeouts = 0;
        self.machine_ns = 0;
        self.control_pause_ns = 0;
        self.scene_check_ns = 0;
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
        // Preserve the ordered fast frames too: sparse long-frame rows cannot
        // establish cadence, adjacency, loading boundaries or consecutive loss.
        if self.sequence.len() == PERCENTILE_CAPACITY {
            self.sequence.pop_front();
        }
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
        self.sequence.push_back(row.clone());
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
                "\"observer_ns\":{},\"longest\":{},\"rows\":[{}],",
                "\"sequence_capacity\":{},\"sequence_dropped\":{},\"sequence\":[{}]}}"
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
            rows,
            PERCENTILE_CAPACITY,
            self.frames.saturating_sub(self.sequence.len() as u64),
            self.sequence
                .iter()
                .map(Row::json)
                .collect::<Vec<_>>()
                .join(",")
        )
    }
}

fn operation(module: &str, name: &str) -> Option<usize> {
    if name.contains("Glyph")
        || name.contains("TextOut")
        || name.contains("DrawText")
        || name.contains("TextExtent")
        || name.contains("TextMetrics")
    {
        Some(0)
    } else if module.to_ascii_lowercase().starts_with("d3dx") && name.contains("From")
        || name.contains("GdipLoadImage")
        || name.contains("GdipCreateBitmapFrom")
    {
        Some(1)
    } else if name == "ov_read" || module.starts_with("adv-codec-kernel") {
        Some(2)
    } else {
        None
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
    fn api_top_five_and_scheduler_accounting_reset_at_cpu_return() {
        let mut b = FrameBudget::new(FrameRow::default(), None);
        b.apis = (0..7)
            .map(|n| Api {
                name: format!("fixture.dll!api{n}"),
                ns: n * 100,
                calls: 2,
            })
            .collect();
        b.touched = (0..7).collect();
        b.machine_ns = 20_000_000;
        b.wait_ns = 10_000_000;
        b.control_pause_ns = 5_000_000;
        b.scene_check(Duration::from_millis(1));
        b.last = Instant::now() - Duration::from_millis(50);
        b.mark_frame(1);
        b.finish(
            FrameRow {
                run_ns: 15_000_000,
                frontend_audio_ns: 8_000_000,
                frontend_render_ns: 2_000_000,
                ..Default::default()
            },
            None,
        );
        let row = b.rows.back().unwrap();
        assert_eq!(row.scheduler_ns, 5_000_000);
        assert!(row.frontend_ns >= 4_000_000);
        assert_eq!(row.scene_check_ns, 1_000_000);
        assert_eq!(
            row.wall_ns,
            row.active_ns
                + row.scheduler_ns
                + row.frontend_ns
                + row.wait_ns
                + row.control_pause_ns
                + row.scene_check_ns
                + row.service_audio_ns
                + row.service_graphics_ns
        );
        assert_eq!(
            (row.service_audio_ns, row.service_graphics_ns),
            (8_000_000, 2_000_000)
        );
        assert_eq!(row.top_apis.len(), 5);
        assert_eq!(row.top_apis[0].name, "fixture.dll!api6");
        assert!(b.apis.iter().all(|a| a.ns == 0 && a.calls == 0));
        assert!(b.touched.is_empty());
        assert_eq!(
            (b.machine_ns, b.control_pause_ns, b.scene_check_ns),
            (0, 0, 0)
        );
        assert!(
            Api {
                name: "quote\"\n".into(),
                ..Default::default()
            }
            .json()
            .contains("quote\\\"\\u000a")
        );
    }
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
            b.last = Instant::now() - Duration::from_millis(50);
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
        assert_eq!(b.sequence.len(), 2200);
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
        for serial in 1..=5000 {
            b.mark_frame(serial);
            b.last = Instant::now() - Duration::from_millis(16);
            b.finish(FrameRow::default(), None);
        }
        assert_eq!((b.frames, b.over), (4999, 0));
        assert_eq!(b.sequence.len(), PERCENTILE_CAPACITY);
        assert!(b.json().contains("\"sequence_dropped\":903"));
        assert_eq!(b.normal.len(), NORMAL_CAPACITY);
        assert!(b.rows.is_empty());
    }
    #[test]
    fn host_groups_are_coarse_and_stable() {
        assert_eq!(classify("gdi32.dll", "BitBlt"), 0);
        assert_eq!(classify("kernel32.dll", "ReadFile"), 1);
        assert_eq!(classify("dsound.dll", "Unlock"), 2);
        assert_eq!(classify("user32.dll", "PeekMessageW"), 3);
        assert_eq!(operation("gdi32.dll", "GetGlyphOutlineA"), Some(0));
        assert_eq!(
            operation("d3dx9.dll", "D3DXCreateTextureFromFileA"),
            Some(1)
        );
        assert_eq!(operation("d3d9.dll", "UpdateTexture"), None);
        assert_eq!(operation("vorbisfile.dll", "ov_read"), Some(2));
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
