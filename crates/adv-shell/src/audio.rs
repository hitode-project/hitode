//! Opt-in device output; headless sessions never open an audio device.
pub struct AudioOutput {
    #[cfg(host_view)]
    suspended: bool,
    #[cfg(host_view)]
    output: Option<adv_device::Output>,
}

impl AudioOutput {
    #[cfg(host_view)]
    pub fn from_stream(enabled: bool, headless: bool, stream: adv_device::AudioStream) -> Self {
        Self {
            output: (enabled && !headless).then(|| adv_device::Output::new(stream)),
            #[cfg(host_view)]
            suspended: false,
        }
    }

    #[cfg(host_view)]
    pub fn set_suspended(&mut self, suspended: bool) {
        if self.suspended == suspended {
            return;
        }
        self.suspended = suspended;
        if let Some(output) = self.output.as_mut() {
            output.suspend();
        }
    }
    #[cfg(host_view)]
    pub fn set_volume(&self, percent: u8) {
        if let Some(output) = &self.output {
            output.set_volume(percent);
        }
    }
    pub fn update(&mut self) {
        #[cfg(host_view)]
        if self.suspended {
            return;
        }
        #[cfg(host_view)]
        if let Some(output) = self.output.as_mut() {
            output.update();
            if output.is_running() {
                // Permit a fresh warning if a recovered device fails again.
                crate::logging::reset_audio_warning();
            }
        }
    }

    pub fn json(&self) -> String {
        #[cfg(host_view)]
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
    }
}
