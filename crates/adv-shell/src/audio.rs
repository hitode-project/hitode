//! Opt-in device output; headless sessions never open an audio device.
use adv_runtime::boot::Runtime;
use std::time::{Duration, Instant};

pub struct AudioOutput {
    #[cfg(target_os = "macos")]
    suspended: bool,
    #[cfg(any(windows, target_os = "macos"))]
    output: Option<adv_device::Output>,
}

impl AudioOutput {
    pub fn new(enabled: bool, headless: bool, runtime: &Runtime) -> Self {
        #[cfg(any(windows, target_os = "macos"))]
        {
            let output = if enabled && !headless {
                Some(adv_device::Output::new(runtime.audio_stream()))
            } else {
                None
            };
            Self {
                output,
                #[cfg(target_os = "macos")]
                suspended: false,
            }
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        {
            let _ = runtime;
            if enabled && !headless {
                eprintln!(
                    "audio: native output is available on Windows and macOS; continuing with simulated PCM"
                );
            }
            Self {}
        }
    }

    #[cfg(target_os = "macos")]
    pub fn set_suspended(&mut self, suspended: bool) {
        self.suspended = suspended;
        if let Some(output) = self.output.as_mut() {
            output.suspend();
        }
    }
    #[cfg(target_os = "macos")]
    pub fn set_volume(&self, percent: u8) {
        if let Some(output) = &self.output {
            output.set_volume(percent);
        }
    }
    pub fn update(&mut self) {
        #[cfg(target_os = "macos")]
        if self.suspended {
            return;
        }
        #[cfg(any(windows, target_os = "macos"))]
        if let Some(output) = self.output.as_mut() {
            output.update();
            if output.is_running() {
                // Permit a fresh warning if a recovered device fails again.
                crate::logging::reset_audio_warning();
            }
        }
    }

    pub fn wait_duration(&self, delay: Option<Duration>) -> Option<Duration> {
        #[cfg(any(windows, target_os = "macos"))]
        let wake = self.output.as_ref().and_then(adv_device::Output::next_wake);
        #[cfg(not(any(windows, target_os = "macos")))]
        let wake = None;
        cap_delay(delay, wake, Instant::now())
    }

    pub fn json(&self) -> String {
        #[cfg(any(windows, target_os = "macos"))]
        {
            use std::sync::atomic::Ordering::Relaxed;
            let Some(output) = &self.output else {
                return "{\"device_requested\": false, \"device_running\": false}".into();
            };
            let metrics = output.metrics();
            format!(
                concat!(
                    "{{\"device_requested\": true, \"device_running\": {}, ",
                    "\"starts\": {}, \"stops\": {}, \"callbacks\": {}, ",
                    "\"non_silent_samples\": {}, \"lock_misses\": {}, ",
                    "\"device_errors\": {}, \"device_changes\": {}, ",
                    "\"device_sample_rate\": {}, \"device_channels\": {}, ",
                    "\"output_frames\": {}, \"max_callback_frames\": {}, ",
                    "\"native_clock_callbacks\": {}, \"device_consumed_frames\": {}, \"device_queued_frames\": {}, ",
                    "\"hardware_muted\": {}, \"capture_dropped\": {}}}"
                ),
                output.is_running(),
                metrics.starts.load(Relaxed),
                metrics.stops.load(Relaxed),
                metrics.callbacks.load(Relaxed),
                metrics.non_silent_samples.load(Relaxed),
                metrics.lock_misses.load(Relaxed),
                metrics.device_errors.load(Relaxed),
                metrics.device_changes.load(Relaxed),
                metrics.device_sample_rate.load(Relaxed),
                metrics.device_channels.load(Relaxed),
                metrics.output_frames.load(Relaxed),
                metrics.max_callback_frames.load(Relaxed),
                metrics.native_clock_callbacks.load(Relaxed),
                metrics.device_consumed_frames.load(Relaxed),
                metrics.device_queued_frames.load(Relaxed),
                metrics.hardware_muted.load(Relaxed),
                metrics.capture_dropped.load(Relaxed)
            )
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        {
            "{\"device_requested\": false, \"device_running\": false}".into()
        }
    }
}

fn cap_delay(delay: Option<Duration>, wake: Option<Instant>, now: Instant) -> Option<Duration> {
    wake.map_or(delay, |at| {
        let remaining = at.saturating_duration_since(now);
        Some(delay.map_or(remaining, |delay| delay.min(remaining)))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn device_maintenance_caps_unbounded_waits_without_delaying_guest_deadlines() {
        let now = Instant::now();
        let wake = Some(now + Duration::from_secs(1));
        assert_eq!(cap_delay(None, wake, now), Some(Duration::from_secs(1)));
        assert_eq!(
            cap_delay(Some(Duration::from_millis(16)), wake, now),
            Some(Duration::from_millis(16))
        );
        assert_eq!(cap_delay(None, Some(now), now), Some(Duration::ZERO));
        assert_eq!(cap_delay(None, None, now), None);
    }
}
