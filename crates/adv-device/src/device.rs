//! Optional CPAL device output. Only frontends enable this module.
//! Dropping a silent stream also joins CPAL backends whose pause keeps a worker alive.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

#[derive(Default)]
pub struct Metrics {
    pub callbacks: AtomicU64,
    pub lock_misses: AtomicU64,
    pub starts: AtomicU64,
    pub stops: AtomicU64,
    pub non_silent_samples: AtomicU64,
    pub device_errors: AtomicU64,
    pub device_changes: AtomicU64,
    pub device_sample_rate: AtomicU64,
    pub device_channels: AtomicU64,
    pub output_frames: AtomicU64,
    pub max_callback_frames: AtomicU64,
    pub capture_dropped: AtomicU64,
    pub hardware_muted: AtomicBool,
    pub native_clock_callbacks: AtomicU64,
    pub device_consumed_frames: AtomicU64,
    pub device_queued_frames: AtomicU64,
}

pub struct Output {
    source: crate::AudioStream,
    stream: Option<cpal::Stream>,
    device: Option<cpal::Device>,
    fault: Arc<AtomicBool>,
    next_check: Option<Instant>,
    retry_at: Option<Instant>,
    metrics: Arc<Metrics>,
    capture: Option<crate::capture::Capture>,
    mute: bool,
}

impl Output {
    pub fn new(source: impl Into<crate::AudioStream>) -> Self {
        let capture = crate::capture::Capture::from_env().unwrap_or_else(|error| {
            log::warn!("cannot create diagnostic callback capture: {error}");
            None
        });
        Self {
            source: source.into(),
            stream: None,
            device: None,
            fault: Arc::default(),
            next_check: None,
            retry_at: None,
            metrics: Arc::default(),
            capture,
            mute: std::env::var("ADV_AUDIO_MUTE_DEVICE")
                .ok()
                .is_some_and(|value| value == "1"),
        }
    }
    pub fn metrics(&self) -> Arc<Metrics> {
        self.metrics.clone()
    }
    pub fn is_running(&self) -> bool {
        self.stream.is_some()
    }

    /// Stop device callbacks while the application is in the background.
    /// The guest mixer and its queued PCM remain owned by the machine.
    pub fn suspend(&mut self) {
        self.stop();
        self.next_check = None;
        self.retry_at = None;
    }

    fn stop(&mut self) {
        if self.stream.take().is_some() {
            self.metrics.stops.fetch_add(1, Ordering::Relaxed);
        }
        self.drain_capture();
        self.device = None;
        self.fault.store(false, Ordering::Release);
    }

    /// Maintenance only while PCM is active: hotplug recovery must also wake a sleeping host.
    pub fn next_wake(&self) -> Option<Instant> {
        self.next_check
    }

    /// Call after the guest PCM pump. No polling thread exists while silent.
    pub fn update(&mut self) {
        self.drain_capture();
        self.update_at(Instant::now(), || {
            cpal::default_host().default_output_device()
        });
    }
    fn drain_capture(&mut self) {
        if let Some(capture) = self.capture.as_mut()
            && let Err(error) = capture.drain()
        {
            log::warn!("cannot drain diagnostic callback capture: {error}");
        }
    }

    fn update_at(&mut self, now: Instant, default_device: impl FnOnce() -> Option<cpal::Device>) {
        let active = self.source.active();
        if !active {
            self.suspend();
            return;
        }
        if self.fault.swap(false, Ordering::AcqRel) && self.stream.is_some() {
            self.stop();
            self.retry_at = Some(now + Duration::from_millis(250));
            log::warn!("audio device stream failed; reopening default output");
        }
        if self.stream.is_some() && self.next_check.is_some_and(|at| now < at) {
            return;
        }
        if self.retry_at.is_some_and(|at| now < at) {
            self.next_check = self.retry_at;
            return;
        }
        self.next_check = Some(now + Duration::from_millis(500));
        let device = default_device();
        if self.stream.is_some() {
            if self
                .device
                .as_ref()
                .zip(device.as_ref())
                .is_some_and(|(old, new)| same_device(old, new))
            {
                return;
            }
            self.metrics.device_changes.fetch_add(1, Ordering::Relaxed);
            log::info!("default audio output changed; rebuilding stream");
            self.stop();
        }
        let result = device
            .as_ref()
            .ok_or_else(|| "no output device".to_string())
            .and_then(|d| self.start(d));
        match result {
            Ok(stream) => {
                self.stream = Some(stream);
                self.device = device;
                self.retry_at = None;
                self.metrics.starts.fetch_add(1, Ordering::Relaxed);
                log::debug!("audio output started");
            }
            Err(e) => {
                self.retry_at = Some(now + Duration::from_secs(1));
                self.next_check = self.retry_at;
                log::warn!("audio output unavailable; retrying in one second: {e}");
            }
        }
    }

    fn start(&mut self, device: &cpal::Device) -> Result<cpal::Stream, String> {
        let supported = device.default_output_config().map_err(|e| e.to_string())?;
        let config: cpal::StreamConfig = supported.clone().into();
        let stream = match supported.sample_format() {
            cpal::SampleFormat::I8 => self.build::<i8>(device, &config),
            cpal::SampleFormat::F32 => self.build::<f32>(device, &config),
            cpal::SampleFormat::F64 => self.build::<f64>(device, &config),
            cpal::SampleFormat::I16 => self.build::<i16>(device, &config),
            cpal::SampleFormat::I24 => self.build::<cpal::I24>(device, &config),
            cpal::SampleFormat::I32 => self.build::<i32>(device, &config),
            cpal::SampleFormat::I64 => self.build::<i64>(device, &config),
            cpal::SampleFormat::U8 => self.build::<u8>(device, &config),
            cpal::SampleFormat::U16 => self.build::<u16>(device, &config),
            cpal::SampleFormat::U32 => self.build::<u32>(device, &config),
            cpal::SampleFormat::U64 => self.build::<u64>(device, &config),
            f => return Err(format!("unsupported output sample format {f:?}")),
        }?;
        stream.play().map_err(|e| e.to_string())?;
        Ok(stream)
    }

    fn build<T: cpal::SizedSample + cpal::FromSample<f32>>(
        &mut self,
        device: &cpal::Device,
        config: &cpal::StreamConfig,
    ) -> Result<cpal::Stream, String> {
        let metrics = self.metrics.clone();
        let errors = self.metrics.clone();
        let fault = self.fault.clone();
        let (rate, channels) = (config.sample_rate.0, config.channels);
        self.metrics
            .device_sample_rate
            .store(u64::from(rate), Ordering::Relaxed);
        self.metrics
            .device_channels
            .store(u64::from(channels), Ordering::Relaxed);
        let mut reader = self.source.device_reader(rate, channels)?;
        let mut capture = self
            .capture
            .as_mut()
            .map(|capture| capture.begin(rate, channels, self.mute))
            .transpose()?;
        let mute = self.mute;
        self.metrics.hardware_muted.store(mute, Ordering::Relaxed);
        let started = Instant::now();
        // A fixed scratch buffer avoids allocating on the callback, even for large device buffers.
        let mut scratch = vec![0.0f32; channels as usize * 4096];
        device
            .build_output_stream(
                config,
                move |out: &mut [T], info| {
                    let callback = metrics.callbacks.fetch_add(1, Ordering::Relaxed);
                    let frames = out.len() as u64 / u64::from(channels);
                    metrics.output_frames.fetch_add(frames, Ordering::Relaxed);
                    metrics
                        .max_callback_frames
                        .fetch_max(frames, Ordering::Relaxed);
                    let timestamp = info.timestamp();
                    let clock = info.device_clock().map(|clock| crate::DeviceClock {
                        consumed_frames: clock.consumed_frames,
                        queued_frames: clock.queued_frames,
                    });
                    if let Some(clock) = clock {
                        metrics
                            .native_clock_callbacks
                            .fetch_add(1, Ordering::Relaxed);
                        metrics
                            .device_consumed_frames
                            .store(clock.consumed_frames, Ordering::Relaxed);
                        metrics
                            .device_queued_frames
                            .store(u64::from(clock.queued_frames), Ordering::Relaxed);
                    }
                    let delay = timestamp
                        .playback
                        .duration_since(&timestamp.callback)
                        .map_or(0, |duration| duration.as_micros() as u64);
                    let mut offset = 0;
                    let mut frame_offset = 0;
                    for block in out.chunks_mut(scratch.len()) {
                        let read = reader.read_with_clock(
                            &mut scratch[..block.len()],
                            delay + offset,
                            clock.filter(|_| frame_offset == 0),
                        );
                        metrics
                            .lock_misses
                            .fetch_add(u64::from(read.lock_miss), Ordering::Relaxed);
                        offset +=
                            block.len() as u64 / u64::from(channels) * 1_000_000 / u64::from(rate);
                        if let Some(capture) = capture.as_mut() {
                            if block.len() <= 8192 {
                                let mut packet = crate::capture::Packet {
                                    callback,
                                    frame_offset,
                                    first_pts_us: read.first_pts_us,
                                    playback_delay_us: delay,
                                    host_us: started.elapsed().as_micros() as u64,
                                    samples: block.len(),
                                    lock_miss: read.lock_miss,
                                    missing_output_frames: read.missing_output_frames,
                                    missing_stream_frames: read.missing_stream_frames,
                                    device_consumed_frames: clock
                                        .map(|clock| clock.consumed_frames),
                                    device_queued_frames: clock.map(|clock| clock.queued_frames),
                                    data: [0.0; 8192],
                                };
                                packet.data[..block.len()].copy_from_slice(&scratch[..block.len()]);
                                metrics.capture_dropped.fetch_add(
                                    u64::from(capture.push(packet).is_err()),
                                    Ordering::Relaxed,
                                );
                            } else {
                                metrics.capture_dropped.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                        frame_offset += (block.len() / usize::from(channels)) as u32;
                        let non_silent = scratch[..block.len()]
                            .iter()
                            .filter(|s| s.abs() > 0.00001)
                            .count();
                        metrics
                            .non_silent_samples
                            .fetch_add(non_silent as u64, Ordering::Relaxed);
                        convert_output(block, &scratch, mute);
                    }
                },
                move |_| {
                    errors.device_errors.fetch_add(1, Ordering::Relaxed);
                    fault.store(true, Ordering::Release);
                },
                None,
            )
            .map_err(|e| e.to_string())
    }
}

fn convert_output<T: cpal::SizedSample + cpal::FromSample<f32>>(
    out: &mut [T],
    samples: &[f32],
    muted: bool,
) {
    for (output, &sample) in out.iter_mut().zip(samples) {
        *output = if muted {
            T::EQUILIBRIUM
        } else {
            T::from_sample(sample)
        };
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        self.stream.take();
        self.drain_capture();
        log::debug!(
            "audio metrics: callbacks={} non_silent_samples={} lock_misses={} starts={} stops={} device_errors={} device_changes={}",
            self.metrics.callbacks.load(Ordering::Relaxed),
            self.metrics.non_silent_samples.load(Ordering::Relaxed),
            self.metrics.lock_misses.load(Ordering::Relaxed),
            self.metrics.starts.load(Ordering::Relaxed),
            self.metrics.stops.load(Ordering::Relaxed),
            self.metrics.device_errors.load(Ordering::Relaxed),
            self.metrics.device_changes.load(Ordering::Relaxed)
        );
    }
}

fn same_device(a: &cpal::Device, b: &cpal::Device) -> bool {
    #[cfg(target_os = "windows")]
    {
        // WASAPI compares endpoint IDs, including two devices with the same friendly name.
        let (cpal::platform::DeviceInner::Wasapi(a), cpal::platform::DeviceInner::Wasapi(b)) =
            (a.as_inner(), b.as_inner());
        a == b
    }
    #[cfg(target_os = "macos")]
    {
        let (cpal::platform::DeviceInner::CoreAudio(a), cpal::platform::DeviceInner::CoreAudio(b)) =
            (a.as_inner(), b.as_inner());
        a == b
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        a.name()
            .ok()
            .zip(b.name().ok())
            .is_some_and(|(a, b)| a == b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mixer::PcmBlock;
    use cpal::Sample;

    fn conversion<T: cpal::SizedSample + cpal::FromSample<f32>>(tolerance: f32)
    where
        f32: cpal::FromSample<T>,
    {
        let samples = [-1.0, -0.75, -0.25, 0.0, 0.25, 0.75, 1.0];
        let mut output = [T::EQUILIBRIUM; 7];
        convert_output(&mut output, &samples, false);
        for (got, want) in output
            .iter()
            .map(|sample| f32::from_sample(*sample))
            .zip(samples)
        {
            assert!(
                (got - want).abs() <= tolerance,
                "format conversion {got} {want}"
            );
        }
        convert_output(&mut output, &samples, true);
        assert!(
            output.iter().all(|sample| f32::from_sample(*sample) == 0.0),
            "mute submits true equilibrium for every sample type"
        );
    }
    #[test]
    fn negotiated_device_formats_preserve_sign_gain_and_true_muted_silence() {
        conversion::<i8>(1.0 / 128.0);
        conversion::<u8>(1.0 / 128.0);
        conversion::<i16>(1.0 / 32768.0);
        conversion::<u16>(1.0 / 32768.0);
        conversion::<cpal::I24>(1.0 / 8388608.0);
        conversion::<i32>(0.000001);
        conversion::<u32>(0.000001);
        conversion::<i64>(0.000001);
        conversion::<u64>(0.000001);
        conversion::<f32>(0.0);
        conversion::<f64>(0.0);
    }

    fn source() -> crate::AudioStream {
        let mixer = crate::AudioControl::new(false);
        mixer.sync_clock(0);
        assert!(
            mixer
                .push_pcm(PcmBlock {
                    stream_id: 1,
                    guest_pts_us: 0,
                    sample_rate: 48000,
                    channels: 1,
                    samples: vec![0.0; 96000]
                })
                .is_ok()
        );
        mixer.stream()
    }

    #[test]
    fn missing_device_retries_with_backoff_and_suspend_cancels_wakes() {
        let mut output = Output::new(source());
        let now = Instant::now();
        output.update_at(now, || None);
        assert_eq!(output.next_wake(), Some(now + Duration::from_secs(1)));
        output.update_at(now + Duration::from_millis(500), || {
            panic!("retried too soon")
        });
        output.update_at(now + Duration::from_secs(1), || None);
        assert_eq!(output.next_wake(), Some(now + Duration::from_secs(2)));
        output.suspend();
        assert!(output.next_wake().is_none());
        // No permanent failure latch: resume immediately probes again.
        output.update_at(now + Duration::from_secs(1), || None);
        assert!(output.retry_at.is_some());
    }

    #[test]
    #[ignore = "opens the real default device with silent PCM; injects unplug/error notifications"]
    fn device_loss_and_callback_error_reopen_the_real_stream() {
        let mut output = Output::new(source());
        let now = Instant::now();
        let device = cpal::default_host()
            .default_output_device()
            .expect("test needs a default device");
        output.update_at(now, || Some(device.clone()));
        assert!(output.is_running());
        output.update_at(now + Duration::from_millis(500), || Some(device.clone()));
        assert_eq!(
            output.metrics.starts.load(Ordering::Relaxed),
            1,
            "same endpoint must not restart"
        );
        output.update_at(now + Duration::from_secs(1), || None);
        assert!(!output.is_running());
        output.update_at(now + Duration::from_secs(2), || Some(device.clone()));
        assert!(
            output.is_running(),
            "device restored after missing-device backoff"
        );
        output.fault.store(true, Ordering::Release);
        output.update_at(now + Duration::from_millis(2100), || {
            panic!("error recovery must back off")
        });
        assert!(!output.is_running());
        output.update_at(now + Duration::from_millis(2350), || Some(device));
        assert!(
            output.is_running(),
            "callback failure must discard and reopen even the same endpoint"
        );
        assert_eq!(output.metrics.starts.load(Ordering::Relaxed), 3);
        output.suspend();
        assert_eq!(output.metrics.stops.load(Ordering::Relaxed), 3);
        assert!(output.next_wake().is_none());
    }
}
