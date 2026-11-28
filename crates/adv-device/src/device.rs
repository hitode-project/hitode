//! Optional CPAL device output. Only frontends enable this module.
//! Dropping a silent stream also joins CPAL backends whose pause keeps a worker alive.

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

/// Keep the host audio stream open across brief silence gaps (such as pauses
/// between dialogue lines or sound effects) to avoid repeated teardown/reopen
/// storms while still releasing hardware resources after sustained silence.
const IDLE_GRACE: Duration = Duration::from_secs(8);
const CHECK_INTERVAL: Duration = Duration::from_millis(500);
const FAULT_RETRY: Duration = Duration::from_millis(250);
const ERROR_RETRY: Duration = Duration::from_secs(1);

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    None,
    /// Immediate synchronous teardown (explicit suspend or fault while silent).
    StopSync {
        clear_cache: bool,
    },
    /// Sustained idle teardown; may run off the execution thread.
    StopIdle,
    /// Callback fault while active; tear down, clear cached config, and back off.
    FaultBackoff,
    /// Open the stream if stopped, or verify the default device if running.
    EnsureOrCheck,
}

#[derive(Default)]
struct Policy {
    idle_since: Option<Instant>,
    next_check: Option<Instant>,
    retry_at: Option<Instant>,
}

impl Policy {
    fn reset(&mut self) {
        self.idle_since = None;
        self.next_check = None;
        self.retry_at = None;
    }

    fn next_wake(&self) -> Option<Instant> {
        self.next_check
    }

    fn on_start_ok(&mut self) {
        self.retry_at = None;
    }

    fn on_start_error(&mut self, now: Instant) {
        self.retry_at = Some(now + ERROR_RETRY);
        self.next_check = self.retry_at;
    }

    fn step(
        &mut self,
        now: Instant,
        active: bool,
        suspended: bool,
        running_or_opening: bool,
        fault: bool,
    ) -> Action {
        if suspended {
            self.reset();
            return Action::StopSync { clear_cache: false };
        }
        if !active {
            if fault {
                self.reset();
                return Action::StopSync { clear_cache: true };
            }
            if !running_or_opening {
                self.reset();
                return Action::None;
            }
            let since = *self.idle_since.get_or_insert(now);
            let stop_at = since + IDLE_GRACE;
            if now >= stop_at {
                self.reset();
                return Action::StopIdle;
            }
            self.next_check = Some(stop_at);
            return Action::None;
        }
        let resumed_from_idle = self.idle_since.take().is_some();
        if fault {
            self.retry_at = Some(now + FAULT_RETRY);
            self.next_check = self.retry_at;
            return Action::FaultBackoff;
        }
        if running_or_opening {
            if resumed_from_idle {
                self.next_check = Some(now + CHECK_INTERVAL);
                return Action::None;
            }
            if self.next_check.is_some_and(|at| now < at) {
                return Action::None;
            }
        }
        if self.retry_at.is_some_and(|at| now < at) {
            self.next_check = self.retry_at;
            return Action::None;
        }
        self.next_check = Some(now + CHECK_INTERVAL);
        Action::EnsureOrCheck
    }
}

/// Owns the live CPAL stream and negotiated device config on a single thread.
struct StreamSession {
    source: crate::AudioStream,
    stream: Option<cpal::Stream>,
    device: Option<cpal::Device>,
    cached_config: Option<(cpal::Device, cpal::SupportedStreamConfig)>,
    fault: Arc<AtomicBool>,
    metrics: Arc<Metrics>,
    capture: Option<crate::capture::Capture>,
    mute: bool,
    volume: Arc<AtomicU8>,
}

impl StreamSession {
    fn is_running(&self) -> bool {
        self.stream.is_some()
    }

    fn clear_cache(&mut self) {
        self.cached_config = None;
    }

    fn stop(&mut self, clear_cache: bool) {
        if self.stream.take().is_some() {
            self.metrics.stops.fetch_add(1, Ordering::Relaxed);
        }
        self.drain_capture();
        self.device = None;
        if clear_cache {
            self.clear_cache();
        }
        self.fault.store(false, Ordering::Release);
    }

    fn drain_capture(&mut self) {
        if let Some(capture) = self.capture.as_mut()
            && let Err(error) = capture.drain()
        {
            log::warn!("cannot drain diagnostic callback capture: {error}");
        }
    }

    fn ensure_or_check(
        &mut self,
        default_device: impl FnOnce() -> Option<cpal::Device>,
    ) -> Result<(), String> {
        let device = default_device();
        if self.stream.is_some() {
            if self.fault.load(Ordering::Acquire) {
                log::warn!("audio stream faulted before check; closing and renegotiating");
                self.stop(true);
            } else if self
                .device
                .as_ref()
                .zip(device.as_ref())
                .is_some_and(|(old, new)| same_device(old, new))
            {
                return Ok(());
            } else {
                self.metrics.device_changes.fetch_add(1, Ordering::Relaxed);
                log::info!("default audio output changed; rebuilding stream");
                self.stop(true);
            }
        }
        let Some(target) = device.as_ref() else {
            self.clear_cache();
            return Err("no output device".to_string());
        };
        let stream = self.start(target)?;
        self.stream = Some(stream);
        self.device = device;
        self.metrics.starts.fetch_add(1, Ordering::Relaxed);
        log::debug!("audio output started");
        Ok(())
    }

    fn start(&mut self, device: &cpal::Device) -> Result<cpal::Stream, String> {
        let mut cached = self.cached_config.take();
        let result = open_with_cache(
            &mut cached,
            device,
            same_device,
            |d| d.default_output_config().map_err(|e| e.to_string()),
            |d, c| self.open_with_config(d, c),
        );
        self.cached_config = cached;
        result
    }

    fn open_with_config(
        &mut self,
        device: &cpal::Device,
        supported: &cpal::SupportedStreamConfig,
    ) -> Result<cpal::Stream, String> {
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
        let volume = self.volume.clone();
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
                        convert_output(block, &scratch, mute, volume.load(Ordering::Relaxed));
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

fn open_with_cache<D, C, S>(
    cached: &mut Option<(D, C)>,
    device: &D,
    same_device: impl Fn(&D, &D) -> bool,
    default_config: impl FnOnce(&D) -> Result<C, String>,
    mut open: impl FnMut(&D, &C) -> Result<S, String>,
) -> Result<S, String>
where
    D: Clone,
    C: Clone,
{
    if let Some((cached_device, cached_config)) = cached.clone() {
        if same_device(&cached_device, device) {
            match open(device, &cached_config) {
                Ok(stream) => return Ok(stream),
                Err(_) => {
                    *cached = None;
                }
            }
        } else {
            *cached = None;
        }
    }
    let supported = default_config(device)?;
    let stream = match open(device, &supported) {
        Ok(stream) => stream,
        Err(err) => {
            *cached = None;
            return Err(err);
        }
    };
    *cached = Some((device.clone(), supported));
    Ok(stream)
}

trait AudioBackend {
    fn is_running(&self) -> bool;
    fn drain_capture(&mut self);
    fn clear_cache(&mut self);
    fn stop(&mut self, clear_cache: bool);
    fn ensure_or_check(&mut self) -> Result<(), String>;
}

impl AudioBackend for StreamSession {
    fn is_running(&self) -> bool {
        StreamSession::is_running(self)
    }

    fn drain_capture(&mut self) {
        StreamSession::drain_capture(self);
    }

    fn clear_cache(&mut self) {
        StreamSession::clear_cache(self);
    }

    fn stop(&mut self, clear_cache: bool) {
        StreamSession::stop(self, clear_cache);
    }

    fn ensure_or_check(&mut self) -> Result<(), String> {
        StreamSession::ensure_or_check(self, || cpal::default_host().default_output_device())
    }
}

#[derive(Default)]
struct WorkerShared {
    want_running: bool,
    /// Non-cancellable teardown requested by a stream fault/disconnect.
    /// Even if `want_running` flips back to `true` before the worker wakes,
    /// the worker must close the old stream and invalidate cached config first.
    pending_close: bool,
    clear_cache: bool,
    request_serial: u64,
    handled_serial: u64,
    busy: bool,
    shutdown: bool,
    last_ok_serial: u64,
    last_error_serial: u64,
    last_error_at: Option<Instant>,
}

/// Runs CPAL stream open, device checks, and idle teardown off the execution
/// thread so JNI/Binder/HAL latency never stalls guest slices.
struct AudioWorker {
    state: Arc<(Mutex<WorkerShared>, Condvar, Condvar)>,
    running: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    seen_ok_serial: u64,
    seen_error_serial: u64,
}

impl AudioWorker {
    fn spawn(
        source: crate::AudioStream,
        fault: Arc<AtomicBool>,
        metrics: Arc<Metrics>,
        capture: Option<crate::capture::Capture>,
        mute: bool,
        volume: Arc<AtomicU8>,
    ) -> Self {
        Self::spawn_with(move || StreamSession {
            source,
            stream: None,
            device: None,
            cached_config: None,
            fault,
            metrics,
            capture,
            mute,
            volume,
        })
    }

    fn spawn_with<B: AudioBackend>(make_backend: impl FnOnce() -> B + Send + 'static) -> Self {
        let state = Arc::new((
            Mutex::new(WorkerShared::default()),
            Condvar::new(),
            Condvar::new(),
        ));
        let running = Arc::new(AtomicBool::new(false));
        let worker_state = state.clone();
        let worker_running = running.clone();
        let handle = std::thread::Builder::new()
            .name("adv-audio-device".into())
            .spawn(move || {
                let mut session = make_backend();
                worker_loop(&mut session, &worker_state, &worker_running);
            })
            .expect("spawn audio device worker");
        Self {
            state,
            running,
            handle: Some(handle),
            seen_ok_serial: 0,
            seen_error_serial: 0,
        }
    }

    fn is_running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }

    fn is_running_or_opening(&self) -> bool {
        let shared = self.state.0.lock().unwrap_or_else(|e| e.into_inner());
        shared.want_running
            && (self.is_running() || shared.busy || shared.handled_serial < shared.request_serial)
    }

    fn poll_outcomes(&mut self, policy: &mut Policy) {
        let shared = self.state.0.lock().unwrap_or_else(|e| e.into_inner());
        if shared.last_ok_serial > self.seen_ok_serial {
            self.seen_ok_serial = shared.last_ok_serial;
            policy.on_start_ok();
        }
        if shared.last_error_serial > self.seen_error_serial {
            self.seen_error_serial = shared.last_error_serial;
            if let Some(at) = shared.last_error_at {
                policy.on_start_error(at);
            }
        }
    }

    fn request_open_or_check(&mut self) {
        let (lock, work_cv, _) = &*self.state;
        let mut shared = lock.lock().unwrap_or_else(|e| e.into_inner());
        shared.want_running = true;
        shared.request_serial += 1;
        work_cv.notify_one();
    }

    fn request_stop_async(&mut self) {
        let (lock, work_cv, _) = &*self.state;
        let mut shared = lock.lock().unwrap_or_else(|e| e.into_inner());
        shared.want_running = false;
        shared.request_serial += 1;
        work_cv.notify_one();
    }

    fn request_fault_stop(&mut self) {
        let (lock, work_cv, _) = &*self.state;
        let mut shared = lock.lock().unwrap_or_else(|e| e.into_inner());
        shared.want_running = false;
        shared.pending_close = true;
        shared.clear_cache = true;
        shared.request_serial += 1;
        work_cv.notify_one();
    }

    fn stop_sync(&mut self, clear_cache: bool) {
        let (lock, work_cv, done_cv) = &*self.state;
        let mut shared = lock.lock().unwrap_or_else(|e| e.into_inner());
        shared.want_running = false;
        shared.pending_close |= clear_cache;
        shared.clear_cache |= clear_cache;
        shared.request_serial += 1;
        let target = shared.request_serial;
        work_cv.notify_one();
        while shared.handled_serial < target || shared.busy || self.is_running() {
            shared = done_cv.wait(shared).unwrap_or_else(|e| e.into_inner());
        }
    }

    fn shutdown(&mut self) {
        {
            let (lock, work_cv, _) = &*self.state;
            let mut shared = lock.lock().unwrap_or_else(|e| e.into_inner());
            shared.want_running = false;
            shared.shutdown = true;
            shared.request_serial += 1;
            work_cv.notify_one();
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn worker_loop<B: AudioBackend>(
    session: &mut B,
    state: &(Mutex<WorkerShared>, Condvar, Condvar),
    running: &AtomicBool,
) {
    let (lock, work_cv, done_cv) = state;
    let mut shared = lock.lock().unwrap_or_else(|e| e.into_inner());
    loop {
        if shared.handled_serial == shared.request_serial {
            if shared.shutdown {
                break;
            }
            shared = work_cv.wait(shared).unwrap_or_else(|e| e.into_inner());
            continue;
        }
        let target = shared.request_serial;
        let want_running = shared.want_running;
        let pending_close = shared.pending_close;
        let clear_cache = shared.clear_cache;
        shared.pending_close = false;
        shared.clear_cache = false;
        shared.busy = true;
        drop(shared);

        session.drain_capture();
        if pending_close {
            // A faulted or disconnected stream must be closed and its cached
            // config invalidated before any replacement stream is opened.
            session.stop(true);
            running.store(session.is_running(), Ordering::Release);
        }
        let outcome = if want_running {
            if clear_cache && !pending_close {
                session.clear_cache();
            }
            Some(session.ensure_or_check())
        } else {
            if !pending_close {
                session.stop(clear_cache);
            }
            None
        };
        running.store(session.is_running(), Ordering::Release);

        shared = lock.lock().unwrap_or_else(|e| e.into_inner());
        shared.busy = false;
        shared.handled_serial = target;
        match outcome {
            Some(Ok(())) => {
                shared.last_ok_serial += 1;
            }
            Some(Err(e)) => {
                shared.last_error_serial += 1;
                shared.last_error_at = Some(Instant::now());
                log::warn!("audio output unavailable; retrying in one second: {e}");
            }
            None => {}
        }
        done_cv.notify_all();
    }
    drop(shared);
    session.stop(false);
    running.store(false, Ordering::Release);
}

pub struct Output {
    source: crate::AudioStream,
    fault: Arc<AtomicBool>,
    policy: Policy,
    metrics: Arc<Metrics>,
    pending_capture: Option<crate::capture::Capture>,
    mute: bool,
    volume: Arc<AtomicU8>,
    worker: Option<AudioWorker>,
}

impl Output {
    pub fn new(source: impl Into<crate::AudioStream>) -> Self {
        let capture = crate::capture::Capture::from_env().unwrap_or_else(|error| {
            log::warn!("cannot create diagnostic callback capture: {error}");
            None
        });
        Self {
            source: source.into(),
            fault: Arc::default(),
            policy: Policy::default(),
            metrics: Arc::default(),
            pending_capture: capture,
            volume: Arc::new(AtomicU8::new(100)),
            mute: std::env::var("ADV_AUDIO_MUTE_DEVICE")
                .ok()
                .is_some_and(|value| value == "1"),
            worker: None,
        }
    }

    pub fn set_volume(&self, percent: u8) {
        self.volume.store(percent.min(100), Ordering::Relaxed);
    }

    pub fn metrics(&self) -> Arc<Metrics> {
        self.metrics.clone()
    }

    pub fn is_running(&self) -> bool {
        self.worker.as_ref().is_some_and(AudioWorker::is_running)
    }

    /// Stop device callbacks while the application is in the background or sleeping.
    /// The guest mixer and its queued PCM remain owned by the machine.
    pub fn suspend(&mut self) {
        self.policy.reset();
        if let Some(worker) = self.worker.as_mut() {
            worker.stop_sync(false);
        }
        self.fault.store(false, Ordering::Release);
    }

    /// Maintenance only while PCM is active or within the idle grace period.
    pub fn next_wake(&self) -> Option<Instant> {
        self.policy.next_wake()
    }

    /// Call after the guest PCM pump. No worker thread is spawned while silent.
    pub fn update(&mut self) {
        if let Some(worker) = self.worker.as_mut() {
            worker.poll_outcomes(&mut self.policy);
        }
        let now = Instant::now();
        let active = self.source.active();
        let suspended = self.source.is_suspended();
        let running_or_opening = self
            .worker
            .as_ref()
            .is_some_and(AudioWorker::is_running_or_opening);
        let fault = self.fault.swap(false, Ordering::AcqRel);
        match self
            .policy
            .step(now, active, suspended, running_or_opening, fault)
        {
            Action::None => {}
            Action::StopSync { clear_cache } => {
                if let Some(worker) = self.worker.as_mut() {
                    worker.stop_sync(clear_cache);
                }
            }
            Action::StopIdle => {
                if let Some(worker) = self.worker.as_mut() {
                    worker.request_stop_async();
                }
            }
            Action::FaultBackoff => {
                log::warn!("audio device stream failed; reopening default output");
                if let Some(worker) = self.worker.as_mut() {
                    worker.request_fault_stop();
                }
            }
            Action::EnsureOrCheck => {
                let worker = self.worker.get_or_insert_with(|| {
                    AudioWorker::spawn(
                        self.source.clone(),
                        self.fault.clone(),
                        self.metrics.clone(),
                        self.pending_capture.take(),
                        self.mute,
                        self.volume.clone(),
                    )
                });
                worker.request_open_or_check();
            }
        }
    }
}

fn convert_output<T: cpal::SizedSample + cpal::FromSample<f32>>(
    out: &mut [T],
    samples: &[f32],
    muted: bool,
    volume: u8,
) {
    let gain = f32::from(volume) / 100.0;
    for (output, &sample) in out.iter_mut().zip(samples) {
        *output = if muted {
            T::EQUILIBRIUM
        } else {
            T::from_sample(sample * gain)
        };
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        if let Some(mut worker) = self.worker.take() {
            worker.shutdown();
        }
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
    #[cfg(target_os = "macos")]
    {
        let (cpal::platform::DeviceInner::CoreAudio(a), cpal::platform::DeviceInner::CoreAudio(b)) =
            (a.as_inner(), b.as_inner());
        a == b
    }
    #[cfg(not(target_os = "macos"))]
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
        convert_output(&mut output, &samples, false, 100);
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
        convert_output(&mut output, &samples, true, 100);
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

    // spec: HOST-RUN-1
    #[test]
    fn host_volume_scales_device_samples_without_changing_source() {
        let source = [-0.8, 0.4, 0.0];
        let mut actual = [0.0_f32; 3];
        convert_output(&mut actual, &source, false, 50);
        assert_eq!(actual, [-0.4, 0.2, 0.0]);
        convert_output(&mut actual, &source, false, 0);
        assert_eq!(actual, [0.0; 3]);
        convert_output(&mut actual, &source, false, 100);
        assert_eq!(actual, source);
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

    struct SyncOutput {
        policy: Policy,
        session: StreamSession,
    }

    impl SyncOutput {
        fn new(source: crate::AudioStream) -> Self {
            Self {
                policy: Policy::default(),
                session: StreamSession {
                    source,
                    stream: None,
                    device: None,
                    cached_config: None,
                    fault: Arc::default(),
                    metrics: Arc::default(),
                    capture: None,
                    mute: false,
                    volume: Arc::new(AtomicU8::new(100)),
                },
            }
        }

        fn is_running(&self) -> bool {
            self.session.is_running()
        }

        fn next_wake(&self) -> Option<Instant> {
            self.policy.next_wake()
        }

        fn suspend(&mut self) {
            self.policy.reset();
            self.session.stop(false);
        }

        fn update_at(
            &mut self,
            now: Instant,
            default_device: impl FnOnce() -> Option<cpal::Device>,
        ) {
            let active = self.session.source.active();
            let suspended = self.session.source.is_suspended();
            let fault = self.session.fault.swap(false, Ordering::AcqRel);
            match self
                .policy
                .step(now, active, suspended, self.session.is_running(), fault)
            {
                Action::None => {}
                Action::StopSync { clear_cache } => self.session.stop(clear_cache),
                Action::StopIdle => self.session.stop(false),
                Action::FaultBackoff => self.session.stop(true),
                Action::EnsureOrCheck => match self.session.ensure_or_check(default_device) {
                    Ok(()) => self.policy.on_start_ok(),
                    Err(_) => self.policy.on_start_error(now),
                },
            }
        }
    }

    #[test]
    fn missing_device_retries_with_backoff_and_suspend_cancels_wakes() {
        let mut output = SyncOutput::new(source());
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
        assert!(output.policy.retry_at.is_some());
    }

    // spec: HOST-SES-7
    #[test]
    fn idle_hysteresis_keeps_stream_alive_across_brief_gaps_and_stops_on_sustained_idle_or_suspend()
    {
        let mut policy = Policy::default();
        let now = Instant::now();

        // Active playback opens the stream.
        assert_eq!(
            policy.step(now, true, false, false, false),
            Action::EnsureOrCheck
        );
        policy.on_start_ok();

        // Brief silence gap keeps the running stream alive and schedules teardown at IDLE_GRACE.
        let t_idle = now + Duration::from_millis(100);
        assert_eq!(policy.step(t_idle, false, false, true, false), Action::None);
        assert_eq!(policy.next_wake(), Some(t_idle + IDLE_GRACE));
        assert_eq!(
            policy.step(
                t_idle + IDLE_GRACE - Duration::from_millis(1),
                false,
                false,
                true,
                false
            ),
            Action::None
        );

        // Resuming before IDLE_GRACE cancels teardown and restores normal device check cadence.
        let t_resume = t_idle + Duration::from_secs(3);
        assert_eq!(
            policy.step(t_resume, true, false, true, false),
            Action::None
        );
        assert_eq!(policy.next_wake(), Some(t_resume + CHECK_INTERVAL));

        // A second silence window starts a fresh IDLE_GRACE countdown.
        let t_idle2 = t_resume + Duration::from_millis(50);
        assert_eq!(
            policy.step(t_idle2, false, false, true, false),
            Action::None
        );
        assert_eq!(
            policy.step(
                t_idle2 + IDLE_GRACE - Duration::from_millis(1),
                false,
                false,
                true,
                false
            ),
            Action::None
        );
        assert_eq!(
            policy.step(t_idle2 + IDLE_GRACE, false, false, true, false),
            Action::StopIdle
        );
        assert!(policy.next_wake().is_none());

        // Once stopped, continued silence performs no work and schedules no wakes.
        assert_eq!(
            policy.step(
                t_idle2 + IDLE_GRACE + Duration::from_secs(1),
                false,
                false,
                false,
                false
            ),
            Action::None
        );
        assert!(policy.next_wake().is_none());

        // Explicit suspension (pause, sleep, background) tears down immediately with zero grace.
        let t_active2 = t_idle2 + IDLE_GRACE + Duration::from_secs(2);
        assert_eq!(
            policy.step(t_active2, true, false, false, false),
            Action::EnsureOrCheck
        );
        assert_eq!(
            policy.step(
                t_active2 + Duration::from_millis(10),
                false,
                true,
                true,
                false
            ),
            Action::StopSync { clear_cache: false }
        );
        assert!(policy.next_wake().is_none());

        // Callback fault during idle grace tears down immediately and clears cached config
        // instead of reopening while silent.
        let t_active3 = t_active2 + Duration::from_secs(1);
        assert_eq!(
            policy.step(t_active3, true, false, false, false),
            Action::EnsureOrCheck
        );
        assert_eq!(
            policy.step(
                t_active3 + Duration::from_millis(10),
                false,
                false,
                true,
                true
            ),
            Action::StopSync { clear_cache: true }
        );
        assert!(policy.next_wake().is_none());
    }

    // spec: HOST-SES-7
    #[test]
    fn source_suspend_triggers_immediate_teardown_and_silent_output_spawns_no_worker() {
        let mixer = crate::AudioControl::new(false);
        let mut output = Output::new(mixer.stream());
        output.update();
        assert!(
            output.worker.is_none(),
            "silent output must not spawn a worker thread"
        );
        assert!(!output.is_running());
        assert!(output.next_wake().is_none());

        mixer.suspend(true);
        assert!(mixer.stream().is_suspended());
        assert!(!mixer.stream().active());
        output.update();
        assert!(output.worker.is_none());
        assert!(output.next_wake().is_none());
    }

    #[test]
    fn async_stop_clears_running_or_opening_before_worker_drains() {
        let worker = AudioWorker {
            state: Arc::new((
                Mutex::new(WorkerShared {
                    want_running: false,
                    busy: true,
                    request_serial: 2,
                    handled_serial: 1,
                    ..Default::default()
                }),
                Condvar::new(),
                Condvar::new(),
            )),
            running: Arc::new(AtomicBool::new(true)),
            handle: None,
            seen_ok_serial: 0,
            seen_error_serial: 0,
        };
        assert!(worker.is_running());
        assert!(
            !worker.is_running_or_opening(),
            "in-flight async stop must not re-arm idle hysteresis"
        );
    }

    #[test]
    fn config_cache_invalidates_on_open_failure_and_renegotiates_fresh_params() {
        let mut cached: Option<(&'static str, u32)> = None;
        let mut probes = 0u32;

        // First open probes default config (48_000) and caches it.
        let s1 = open_with_cache(
            &mut cached,
            &"default",
            |a, b| a == b,
            |_| {
                probes += 1;
                Ok(48_000)
            },
            |_, &rate| Ok(rate),
        );
        assert_eq!(s1, Ok(48_000));
        assert_eq!(probes, 1);
        assert_eq!(cached, Some(("default", 48_000)));

        // Second open reuses cached config without re-probing.
        let s2 = open_with_cache(
            &mut cached,
            &"default",
            |a, b| a == b,
            |_| {
                probes += 1;
                Ok(44_100)
            },
            |_, &rate| Ok(rate),
        );
        assert_eq!(s2, Ok(48_000));
        assert_eq!(probes, 1);

        // Route change makes stale cached config (48_000) fail open; cache must
        // invalidate and re-negotiate fresh params (44_100) immediately.
        let s3 = open_with_cache(
            &mut cached,
            &"default",
            |a, b| a == b,
            |_| {
                probes += 1;
                Ok(44_100)
            },
            |_, &rate| {
                if rate == 48_000 {
                    Err("stale route config".into())
                } else {
                    Ok(rate)
                }
            },
        );
        assert_eq!(s3, Ok(44_100));
        assert_eq!(probes, 2);
        assert_eq!(cached, Some(("default", 44_100)));

        // If fresh open also fails, cached config must remain cleared rather than
        // retaining the previous route's parameters.
        let s4: Result<u32, String> = open_with_cache(
            &mut cached,
            &"default",
            |a, b| a == b,
            |_| {
                probes += 1;
                Ok(44_100)
            },
            |_, _| Err("audio route transiently unavailable".into()),
        );
        assert!(s4.is_err());
        assert_eq!(probes, 3);
        assert_eq!(cached, None);

        // Next attempt re-probes fresh config (96_000) and succeeds.
        let s5 = open_with_cache(
            &mut cached,
            &"default",
            |a, b| a == b,
            |_| {
                probes += 1;
                Ok(96_000)
            },
            |_, &rate| Ok(rate),
        );
        assert_eq!(s5, Ok(96_000));
        assert_eq!(probes, 4);
        assert_eq!(cached, Some(("default", 96_000)));
    }

    #[test]
    fn reopen_during_pending_fault_stop_closes_old_stream_and_renegotiates_first() {
        #[derive(Clone, Debug, PartialEq, Eq)]
        enum Event {
            Probe(u32),
            Open(u32),
            Stop { clear_cache: bool },
        }

        struct Gate {
            entered: Mutex<bool>,
            entered_cv: Condvar,
            release: Mutex<bool>,
            release_cv: Condvar,
        }

        impl Gate {
            fn new() -> Self {
                Self {
                    entered: Mutex::new(false),
                    entered_cv: Condvar::new(),
                    release: Mutex::new(false),
                    release_cv: Condvar::new(),
                }
            }

            fn arrive_and_wait(&self) {
                {
                    let mut entered = self.entered.lock().unwrap();
                    *entered = true;
                    self.entered_cv.notify_all();
                }
                let mut release = self.release.lock().unwrap();
                while !*release {
                    release = self.release_cv.wait(release).unwrap();
                }
            }

            fn wait_until_entered(&self) {
                let mut entered = self.entered.lock().unwrap();
                while !*entered {
                    entered = self.entered_cv.wait(entered).unwrap();
                }
            }

            fn release(&self) {
                let mut release = self.release.lock().unwrap();
                *release = true;
                self.release_cv.notify_all();
            }
        }

        struct FakeBackend {
            running: bool,
            cached_config: Option<u32>,
            next_config: u32,
            checks: u32,
            events: Arc<Mutex<Vec<Event>>>,
            check_gate: Arc<Gate>,
        }

        impl AudioBackend for FakeBackend {
            fn is_running(&self) -> bool {
                self.running
            }

            fn drain_capture(&mut self) {}

            fn clear_cache(&mut self) {
                self.cached_config = None;
            }

            fn stop(&mut self, clear_cache: bool) {
                if self.running || clear_cache {
                    self.events
                        .lock()
                        .unwrap()
                        .push(Event::Stop { clear_cache });
                }
                self.running = false;
                if clear_cache {
                    self.cached_config = None;
                }
            }

            fn ensure_or_check(&mut self) -> Result<(), String> {
                if self.running {
                    self.checks += 1;
                    if self.checks == 1 {
                        // Pause the worker mid-check so the caller can queue a
                        // fault-stop followed immediately by a reopen request.
                        self.check_gate.arrive_and_wait();
                    }
                    return Ok(());
                }
                let config = match self.cached_config {
                    Some(cfg) => cfg,
                    None => {
                        let cfg = self.next_config;
                        self.next_config += 1;
                        self.events.lock().unwrap().push(Event::Probe(cfg));
                        self.cached_config = Some(cfg);
                        cfg
                    }
                };
                self.events.lock().unwrap().push(Event::Open(config));
                self.running = true;
                Ok(())
            }
        }

        let events = Arc::new(Mutex::new(Vec::new()));
        let check_gate = Arc::new(Gate::new());
        let worker_events = events.clone();
        let worker_gate = check_gate.clone();
        let mut worker = AudioWorker::spawn_with(move || FakeBackend {
            running: false,
            cached_config: None,
            next_config: 1,
            checks: 0,
            events: worker_events,
            check_gate: worker_gate,
        });

        let wait_idle = |worker: &AudioWorker| {
            let (lock, _, done_cv) = &*worker.state;
            let mut shared = lock.lock().unwrap();
            while shared.handled_serial < shared.request_serial || shared.busy {
                shared = done_cv.wait(shared).unwrap();
            }
        };

        // 1. Initial open negotiates config 1 and starts stream 1.
        worker.request_open_or_check();
        wait_idle(&worker);
        assert!(worker.is_running());

        // 2. Start a periodic check that pauses the worker thread inside ensure_or_check.
        worker.request_open_or_check();
        check_gate.wait_until_entered();

        // 3. While the worker is busy, a stream fault/disconnect queues a fault-stop,
        //    and a subsequent reopen arrives BEFORE the worker processes the fault-stop.
        worker.request_fault_stop();
        worker.request_open_or_check();
        check_gate.release();
        wait_idle(&worker);

        assert!(
            worker.is_running(),
            "replacement stream must be open after fault teardown + reopen"
        );
        let log = events.lock().unwrap().clone();
        assert_eq!(
            log,
            vec![
                Event::Probe(1),
                Event::Open(1),
                Event::Stop { clear_cache: true },
                Event::Probe(2),
                Event::Open(2),
            ],
            "reopen during pending fault-stop must still close old stream and renegotiate first"
        );

        worker.shutdown();
    }

    #[test]
    fn fault_during_pending_idle_stop_still_triggers_backoff_and_teardown() {
        let mut policy = Policy::default();
        let now = Instant::now();

        // Even if an idle stop cleared running_or_opening, a late fault while active
        // must still trigger FaultBackoff (non-cancellable close + cache clear).
        assert_eq!(
            policy.step(now, true, false, false, true),
            Action::FaultBackoff
        );
        assert_eq!(policy.next_wake(), Some(now + FAULT_RETRY));

        // And a late fault while silent must still tear down synchronously and clear cache.
        assert_eq!(
            policy.step(now + Duration::from_millis(10), false, false, false, true),
            Action::StopSync { clear_cache: true }
        );
        assert!(policy.next_wake().is_none());
    }

    #[test]
    #[ignore = "opens the real default device with silent PCM; injects unplug/error notifications"]
    fn device_loss_and_callback_error_reopen_the_real_stream() {
        let mut output = SyncOutput::new(source());
        let now = Instant::now();
        let device = cpal::default_host()
            .default_output_device()
            .expect("test needs a default device");
        output.update_at(now, || Some(device.clone()));
        assert!(output.is_running());
        output.update_at(now + Duration::from_millis(500), || Some(device.clone()));
        assert_eq!(
            output.session.metrics.starts.load(Ordering::Relaxed),
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
        output.session.fault.store(true, Ordering::Release);
        output.update_at(now + Duration::from_millis(2100), || {
            panic!("error recovery must back off")
        });
        assert!(!output.is_running());
        output.update_at(now + Duration::from_millis(2350), || Some(device));
        assert!(
            output.is_running(),
            "callback failure must discard and reopen even the same endpoint"
        );
        assert_eq!(output.session.metrics.starts.load(Ordering::Relaxed), 3);
        output.suspend();
        assert_eq!(output.session.metrics.stops.load(Ordering::Relaxed), 3);
        assert!(output.next_wake().is_none());
    }
}
