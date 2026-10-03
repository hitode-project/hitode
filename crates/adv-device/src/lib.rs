//! Device layer: the audio engine (with execution/audio ownership transfer) and the
//! software surfaces every graphics front draws into.
mod actor;
#[cfg(feature = "device")]
mod capture;
pub mod clock;
pub mod codec;
#[cfg(feature = "device")]
mod device;
pub mod mixer;
mod owned;
mod resample;
pub mod ring;
pub mod surface;
pub mod vorbis;
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
