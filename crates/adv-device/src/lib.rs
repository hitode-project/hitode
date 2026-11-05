//! Device layer: the audio engine (with execution/audio ownership transfer) and the
//! software surfaces every graphics front draws into.
mod actor;
#[cfg(feature = "device")]
mod capture;
pub mod clock;
pub mod codec;
pub mod codec_kernel;
pub mod constants;
#[cfg(feature = "device")]
mod device;
#[cfg(feature = "gpu")]
pub mod frame;
#[cfg(feature = "gpu")]
pub mod gpu;
pub mod media;
pub mod midi;
pub mod mixer;
mod owned;
mod resample;
pub mod ring;
pub mod surface;
pub mod text;
pub mod theora;
pub mod ui;
pub mod vorbis;
pub mod xaudio2;
pub use actor::{AudioControl, AudioEvent, RingState};
#[cfg(feature = "device")]
pub use device::{Metrics, Output};
pub use mixer::PcmMetrics;

/// Sendable frontend/callback handle. All mutable DSP state has a single owner.
#[derive(Clone)]
pub struct AudioStream {
    pub(crate) shared: std::sync::Arc<actor::Shared>,
}
pub struct DeviceReader {
    pub(crate) engine: std::mem::ManuallyDrop<Box<actor::Engine>>,
    pub(crate) shared: std::sync::Arc<actor::Shared>,
    pub(crate) channels: u16,
    pub(crate) device_time: bool,
    pub(crate) device_origin_us: f64,
    pub(crate) native_clock: bool,
}
#[derive(Clone, Copy, Debug)]
pub struct DeviceRead {
    pub first_pts_us: u64,
    pub lock_miss: bool,
    pub missing_output_frames: u64,
    pub missing_stream_frames: u64,
}

/// A consumed-frame observation from the current device stream's own epoch.
#[derive(Clone, Copy, Debug)]
pub struct DeviceClock {
    pub consumed_frames: u64,
    pub queued_frames: u32,
}

/// Set-once guest events shared by every process of a run.
pub struct Milestones {
    pub started: std::time::Instant,
    pub window: std::sync::OnceLock<u64>,
    pub audio: std::sync::OnceLock<u64>,
    pub video: std::sync::OnceLock<u64>,
    pub input: std::sync::OnceLock<u64>,
}
impl Default for Milestones {
    fn default() -> Self {
        Self {
            started: std::time::Instant::now(),
            window: Default::default(),
            audio: Default::default(),
            video: Default::default(),
            input: Default::default(),
        }
    }
}
impl Milestones {
    pub fn elapsed_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }
}
