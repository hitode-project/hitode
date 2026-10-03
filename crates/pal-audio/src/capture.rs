//! Opt-in callback PCM capture. Only the execution/frontend side writes files.
use pal_host::spsc::{Consumer, Producer, channel};
use std::io::Write;

pub(crate) struct Packet {
    pub callback: u64,
    pub frame_offset: u32,
    pub first_pts_us: u64,
    pub playback_delay_us: u64,
    pub host_us: u64,
    pub samples: usize,
    pub lock_miss: bool,
    pub missing_output_frames: u64,
    pub missing_stream_frames: u64,
    pub device_consumed_frames: Option<u64>,
    pub device_queued_frames: Option<u32>,
    pub data: [f32; 8192],
}
pub(crate) struct Capture {
    output: std::io::BufWriter<std::fs::File>,
    timeline: std::io::BufWriter<std::fs::File>,
    receiver: Option<Consumer<Packet>>,
    byte_buffer: [u8; 8192 * 4],
    file_samples: u64,
}
impl Capture {
    pub fn from_env() -> Result<Option<Self>, String> {
        let Some(path) = std::env::var_os("ADV_AUDIO_CAPTURE_CALLBACK") else {
            return Ok(None);
        };
        let path = std::path::PathBuf::from(path);
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        Ok(Some(Self {
            output: std::io::BufWriter::new(
                std::fs::File::create(&path).map_err(|error| error.to_string())?,
            ),
            timeline: std::io::BufWriter::new(
                std::fs::File::create(path.with_extension("timeline.txt"))
                    .map_err(|error| error.to_string())?,
            ),
            receiver: None,
            byte_buffer: [0; 8192 * 4],
            file_samples: 0,
        }))
    }
    pub fn begin(
        &mut self,
        rate: u32,
        channels: u16,
        muted: bool,
    ) -> Result<Producer<Packet>, String> {
        self.drain()?;
        let (tx, rx) = channel(256);
        self.receiver = Some(rx);
        writeln!(
            self.timeline,
            "device rate={rate} channels={channels} hardware_muted={muted} first_file_sample={}",
            self.file_samples
        )
        .map_err(|error| error.to_string())?;
        Ok(tx)
    }
    pub fn drain(&mut self) -> Result<(), String> {
        let Some(receiver) = self.receiver.as_mut() else {
            return Ok(());
        };
        while let Some(packet) = receiver.pop() {
            for (&sample, bytes) in packet.data[..packet.samples]
                .iter()
                .zip(self.byte_buffer.chunks_exact_mut(4))
            {
                bytes.copy_from_slice(&sample.to_le_bytes());
            }
            self.output
                .write_all(&self.byte_buffer[..packet.samples * 4])
                .map_err(|error| error.to_string())?;
            writeln!(self.timeline, "callback={} frame_offset={} pts_us={} playback_delay_us={} host_us={} file_sample={} samples={} lock_miss={} missing_output_frames={} missing_stream_frames={} native_clock={} device_consumed_frames={} device_queued_frames={}",
                packet.callback, packet.frame_offset, packet.first_pts_us, packet.playback_delay_us,
                packet.host_us, self.file_samples, packet.samples, packet.lock_miss,packet.missing_output_frames,packet.missing_stream_frames,packet.device_consumed_frames.is_some(),packet.device_consumed_frames.unwrap_or(0),packet.device_queued_frames.unwrap_or(0))
                .map_err(|error| error.to_string())?;
            self.file_samples += packet.samples as u64;
        }
        Ok(())
    }
}
