//! Host observations, separate from the guest's presentation serial and clock.
use crate::window::WaitOutcome;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

#[derive(Default)]
struct Samples {
    recent: VecDeque<f64>,
    total_count: u64,
    total_ms: f64,
    max_ms: f64,
}
const SAMPLE_CAPACITY: usize = 4096;
/// Cumulative native compositor observations, independent of metric resets.
/// Queue delay is reported separately from nonpreemptible acquire/configure.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct NativePresentation {
    pub consumed: u64,
    pub uploads: u64,
    pub presented: u64,
    pub configurations: u64,
    pub max_queue_ns: u64,
    pub max_job_ns: u64,
    pub max_acquire_ns: u64,
    pub max_configure_ns: u64,
}
impl NativePresentation {
    fn json(&self) -> String {
        format!(
            concat!(
                "{{\"cumulative\": true, \"consumed_frames\": {}, \"software_uploads\": {}, ",
                "\"presentations\": {}, \"configurations\": {}, \"max_queue_ns\": {}, ",
                "\"max_job_ns\": {}, \"max_acquire_ns\": {}, \"max_configure_ns\": {}}}"
            ),
            self.consumed,
            self.uploads,
            self.presented,
            self.configurations,
            self.max_queue_ns,
            self.max_job_ns,
            self.max_acquire_ns,
            self.max_configure_ns
        )
    }
}
impl Samples {
    fn add(&mut self, elapsed: Duration) {
        let milliseconds = elapsed.as_secs_f64() * 1000.0;
        self.total_count += 1;
        self.total_ms += milliseconds;
        self.max_ms = self.max_ms.max(milliseconds);
        if self.recent.len() == SAMPLE_CAPACITY {
            self.recent.pop_front();
        }
        self.recent.push_back(milliseconds);
    }
    fn json(&self) -> String {
        if self.recent.is_empty() {
            return format!(
                "{{\"samples\": 0, \"total_ms\": 0, \"percentile_window_samples\": 0, \"percentile_window_capacity\": {SAMPLE_CAPACITY}, \"mean_ms\": null, \"p50_ms\": null, \"p95_ms\": null, \"p99_ms\": null, \"max_ms\": null}}"
            );
        }
        let mut values: Vec<_> = self.recent.iter().copied().collect();
        values.sort_by(f64::total_cmp);
        let rank = |percent: usize| (values.len() * percent).div_ceil(100).saturating_sub(1);
        format!(
            "{{\"samples\": {}, \"total_ms\": {:.6}, \"percentile_window_samples\": {}, \"percentile_window_capacity\": {}, \"mean_ms\": {:.6}, \"p50_ms\": {:.6}, \"p95_ms\": {:.6}, \"p99_ms\": {:.6}, \"max_ms\": {:.6}}}",
            self.total_count,
            self.total_ms,
            values.len(),
            SAMPLE_CAPACITY,
            self.total_ms / self.total_count as f64,
            values[rank(50)],
            values[rank(95)],
            values[rank(99)],
            self.max_ms
        )
    }
}

#[derive(Default)]
pub struct Performance {
    measurement_start: Option<Instant>,
    measurement_start_frame: u64,
    steps: u64,
    soft_yields: u64,
    step_total: Duration,
    step_max: Duration,
    pump_total: Duration,
    pump_max: Duration,
    ui_metrics_dropped_total: u64,
    native: Option<NativePresentation>,
    observed_serial: u64,
    observations: u64,
    serial_skipped: u64,
    serial_delta_max: u64,
    cold_step_total: Duration,
    warm_frame_steps: Samples,
    active_since_frame: Duration,
    warm_active_intervals: Samples,
    // Completion observations include host waits/idle; they are neither CPU
    // time nor physical display scanout. Inspect during an active interaction.
    last_frame_observation: Option<Instant>,
    warm_frame_wall_intervals: Samples,
    host_presentations: u64,
    host_present_total: Duration,
    host_present_max: Duration,
    last_host_serial: u64,
    last_host_present: Option<Instant>,
    warm_presents: Samples,
    host_intervals: Samples,
    host_repaints: u64,
    host_repaint_total: Duration,
    wait_requested: Duration,
    wait_overshoot: Samples,
    input_wakes: u64,
    deadline_wakes: u64,
}

impl Performance {
    /// Start a fresh observation window without changing guest execution or time.
    /// The first new frame establishes a boundary: its partial interval is excluded.
    pub fn reset(&mut self, serial: u64) {
        *self = Self {
            measurement_start: Some(Instant::now()),
            measurement_start_frame: serial,
            observed_serial: serial,
            last_host_serial: serial,
            // This channel-health counter is cumulative for the process,
            // independent of when delayed metrics reach the measurement window.
            ui_metrics_dropped_total: self.ui_metrics_dropped_total,
            native: self.native,
            ..Self::default()
        };
    }
    pub fn soft_yield(&mut self) {
        self.soft_yields += 1;
    }
    pub fn pump(&mut self, elapsed: Duration, completed: Instant) {
        if self
            .measurement_start
            .is_some_and(|start| completed < start)
        {
            return;
        }
        self.pump_total += elapsed;
        self.pump_max = self.pump_max.max(elapsed);
    }
    pub fn set_ui_metrics_dropped(&mut self, total: u64) {
        self.ui_metrics_dropped_total = total;
    }
    #[cfg(all(windows, feature = "gpu"))]
    pub(crate) fn native(&mut self, stats: NativePresentation) {
        self.native = Some(stats);
    }
    pub fn step(&mut self, elapsed: Duration, serial: u64) {
        self.steps += 1;
        self.step_total += elapsed;
        self.step_max = self.step_max.max(elapsed);
        if self.observations == 0 {
            self.cold_step_total += elapsed;
        } else {
            self.active_since_frame += elapsed;
        }
        if serial > self.observed_serial {
            let now = Instant::now();
            if let Some(previous) = self.last_frame_observation.replace(now) {
                self.warm_frame_wall_intervals
                    .add(now.saturating_duration_since(previous));
            }
            let delta = serial - self.observed_serial;
            self.observations += 1;
            self.serial_skipped += delta - 1;
            self.serial_delta_max = self.serial_delta_max.max(delta);
            if self.observations > 1 {
                self.warm_frame_steps.add(elapsed);
                self.warm_active_intervals.add(self.active_since_frame);
            }
            self.active_since_frame = Duration::ZERO;
            self.observed_serial = serial;
        }
    }
    pub fn present(&mut self, serial: u64, elapsed: Duration, completed: Instant) {
        // UI delivery may lag behind reset-metrics. Its timestamp, rather than
        // queue arrival, determines which observation window owns the sample.
        if self
            .measurement_start
            .is_some_and(|start| completed < start)
        {
            return;
        }
        if serial == 0 || serial == self.last_host_serial {
            self.host_repaints += 1;
            self.host_repaint_total += elapsed;
            return;
        }
        self.host_presentations += 1;
        self.host_present_total += elapsed;
        self.host_present_max = self.host_present_max.max(elapsed);
        if let Some(previous) = self.last_host_present {
            self.host_intervals
                .add(completed.saturating_duration_since(previous));
            self.warm_presents.add(elapsed);
        }
        self.last_host_present = Some(completed);
        self.last_host_serial = serial;
    }
    pub fn wait(&mut self, requested: Option<Duration>, elapsed: Duration, outcome: WaitOutcome) {
        if let Some(requested) = requested {
            self.wait_requested += requested;
        }
        match outcome {
            WaitOutcome::Input => self.input_wakes += 1,
            WaitOutcome::Deadline => {
                self.deadline_wakes += 1;
                if let Some(requested) = requested {
                    self.wait_overshoot.add(elapsed.saturating_sub(requested));
                }
            }
        }
    }
    pub fn json(&self) -> String {
        format!(
            concat!(
                "{{\n    \"measurement_start_frame\": {}, \"measurement_elapsed_seconds\": {},\n",
                "    \"runtime_steps\": {}, \"runtime_soft_yields\": {}, \"runtime_step_seconds\": {:.6}, \"runtime_step_max_seconds\": {:.6},\n",
                "    \"cold_step_seconds_before_first_observation\": {:.6}, \"event_pump_seconds\": {:.6}, \"event_pump_max_seconds\": {:.6},\n",
                "    \"ui_metrics_dropped_total\": {},\n",
                "    \"native_presentation\": {},\n",
                "    \"frame_observations\": {}, \"guest_serial_skipped_between_observations\": {}, \"guest_serial_delta_max\": {},\n",
                "    \"warm_steps_producing_available_frames\": {},\n",
                "    \"warm_active_runtime_between_frame_observations\": {},\n",
                "    \"warm_wall_time_between_frame_observations\": {},\n",
                "    \"host_presentations\": {}, \"host_present_seconds\": {:.6}, \"host_present_max_seconds\": {:.6},\n",
                "    \"warm_host_present_cost\": {}, \"warm_host_present_intervals\": {},\n",
                "    \"host_repaints\": {}, \"host_repaint_seconds\": {:.6},\n",
                "    \"wait_requested_seconds\": {:.6}, \"wait_deadline_wakes\": {}, \"wait_input_wakes\": {}, \"wait_deadline_overshoot\": {}\n  }}"
            ),
            self.measurement_start_frame,
            self.measurement_start.map_or_else(
                || "null".into(),
                |start| format!("{:.6}", start.elapsed().as_secs_f64()),
            ),
            self.steps,
            self.soft_yields,
            self.step_total.as_secs_f64(),
            self.step_max.as_secs_f64(),
            self.cold_step_total.as_secs_f64(),
            self.pump_total.as_secs_f64(),
            self.pump_max.as_secs_f64(),
            self.ui_metrics_dropped_total,
            self.native
                .map_or_else(|| "null".into(), |stats| stats.json()),
            self.observations,
            self.serial_skipped,
            self.serial_delta_max,
            self.warm_frame_steps.json(),
            self.warm_active_intervals.json(),
            self.warm_frame_wall_intervals.json(),
            self.host_presentations,
            self.host_present_total.as_secs_f64(),
            self.host_present_max.as_secs_f64(),
            self.warm_presents.json(),
            self.host_intervals.json(),
            self.host_repaints,
            self.host_repaint_total.as_secs_f64(),
            self.wait_requested.as_secs_f64(),
            self.deadline_wakes,
            self.input_wakes,
            self.wait_overshoot.json()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn delayed_ui_delivery_cannot_reenter_a_reset_measurement_window() {
        let mut stats = Performance::default();
        let old = Instant::now();
        stats.reset(100);
        let new = stats.measurement_start.unwrap();
        stats.present(
            99,
            Duration::from_millis(50),
            old - Duration::from_millis(1),
        );
        stats.present(
            101,
            Duration::from_millis(1),
            new + Duration::from_millis(2),
        );
        assert_eq!(stats.host_presentations, 1);
        assert_eq!(stats.host_present_total, Duration::from_millis(1));
        assert!(stats.host_intervals.recent.is_empty());
    }
    #[test]
    fn reset_filters_delayed_pump_metrics_and_preserves_total_channel_drops() {
        let mut stats = Performance::default();
        let old = Instant::now() - Duration::from_millis(1);
        stats.set_ui_metrics_dropped(15);
        stats.reset(100);
        let boundary = stats.measurement_start.unwrap();
        stats.pump(Duration::from_secs(1), old);
        assert_eq!(stats.pump_total, Duration::ZERO);
        stats.pump(Duration::from_millis(2), boundary);
        stats.pump(
            Duration::from_millis(3),
            boundary + Duration::from_millis(1),
        );
        assert_eq!(stats.pump_total, Duration::from_millis(5));
        assert_eq!(stats.pump_max, Duration::from_millis(3));
        assert!(stats.json().contains("\"ui_metrics_dropped_total\": 15"));
        stats.set_ui_metrics_dropped(20);
        stats.reset(101);
        assert!(stats.json().contains("\"ui_metrics_dropped_total\": 20"));
        assert_eq!(stats.pump_total, Duration::ZERO);
    }
    #[test]
    fn resetting_measurement_excludes_prior_frames_and_the_first_partial_interval() {
        let mut stats = Performance::default();
        let now = Instant::now();
        stats.step(Duration::from_secs(2), 100);
        stats.present(100, Duration::from_millis(2), now);
        stats.reset(100);
        let now = stats.measurement_start.unwrap();
        stats.step(Duration::from_millis(5), 100);
        stats.present(100, Duration::from_millis(1), now);
        stats.step(Duration::from_millis(3), 101);
        stats.present(
            101,
            Duration::from_millis(1),
            now + Duration::from_millis(8),
        );
        assert!(stats.warm_active_intervals.recent.is_empty());
        assert!(stats.host_intervals.recent.is_empty());
        stats.step(Duration::from_millis(2), 101);
        stats.step(Duration::from_millis(4), 102);
        stats.present(
            102,
            Duration::from_millis(1),
            now + Duration::from_millis(24),
        );
        assert_eq!(stats.measurement_start_frame, 100);
        assert_eq!(stats.observations, 2);
        assert_eq!(stats.serial_skipped, 0);
        assert_eq!(stats.step_total, Duration::from_millis(14));
        assert_eq!(stats.cold_step_total, Duration::from_millis(8));
        assert_eq!(stats.warm_active_intervals.recent, [6.0]);
        assert_eq!(stats.host_intervals.recent, [16.0]);
        assert_eq!(stats.host_presentations, 2);
        assert_eq!(stats.host_repaints, 1);
    }
    #[test]
    fn frame_metrics_exclude_idle_steps_placeholders_and_same_serial_repaints() {
        let mut stats = Performance::default();
        stats.step(Duration::from_millis(5), 0);
        stats.step(Duration::from_millis(20), 3);
        stats.step(Duration::from_micros(1), 3);
        stats.step(Duration::from_millis(7), 5);
        let now = Instant::now();
        stats.present(0, Duration::from_millis(1), now);
        stats.present(3, Duration::from_millis(2), now);
        stats.present(3, Duration::from_millis(3), now + Duration::from_millis(4));
        stats.present(5, Duration::from_millis(1), now + Duration::from_millis(17));
        assert_eq!(stats.steps, 4);
        assert_eq!(stats.observations, 2);
        assert_eq!(stats.serial_skipped, 3);
        assert_eq!(stats.serial_delta_max, 3);
        assert_eq!(stats.warm_frame_steps.recent, [7.0]);
        assert_eq!(stats.warm_active_intervals.recent, [7.001]);
        assert_eq!(stats.host_presentations, 2);
        assert_eq!(stats.host_repaints, 2);
        assert_eq!(stats.warm_presents.recent, [1.0]);
        assert_eq!(stats.host_intervals.recent, [17.0]);
    }

    #[test]
    fn percentile_window_is_bounded_but_totals_and_maximum_are_cumulative() {
        let mut samples = Samples::default();
        samples.add(Duration::from_millis(99));
        for _ in 0..SAMPLE_CAPACITY {
            samples.add(Duration::from_millis(1));
        }
        assert_eq!(samples.total_count, SAMPLE_CAPACITY as u64 + 1);
        assert_eq!(samples.recent.len(), SAMPLE_CAPACITY);
        assert_eq!(samples.total_ms, SAMPLE_CAPACITY as f64 + 99.0);
        assert!(samples.recent.iter().all(|&value| value == 1.0));
        let json = samples.json();
        assert!(json.contains("\"p95_ms\": 1.000000"));
        assert!(json.contains("\"max_ms\": 99.000000"));
    }
}
