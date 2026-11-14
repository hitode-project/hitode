//! On-demand movie decoding (FFmpeg, `media` feature). Times are always supplied by
//! the caller, never read from a wall clock. Without the feature, opening a source
//! reports [`Error::Unavailable`] so fronts can fail like a missing codec.

use std::io::{Read, Seek};

pub trait ReadSeek: Read + Seek + Send {}
impl<T: Read + Seek + Send> ReadSeek for T {}

#[derive(Debug)]
pub enum Error {
    Unavailable,
    Io(std::io::Error),
    Decode(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable => {
                f.write_str("movie decoding is disabled (build with the media feature)")
            }
            Self::Io(error) => write!(f, "media I/O: {error}"),
            Self::Decode(error) => write!(f, "media decode: {error}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

#[derive(Clone, Debug)]
pub struct StreamInfo {
    pub container: String,
    pub video: Option<VideoInfo>,
    pub duration_us: u64,
    pub audio: Option<AudioInfo>,
}

#[derive(Clone, Debug)]
pub struct VideoInfo {
    pub codec: String,
    pub width: u32,
    pub height: u32,
    pub frame_rate: (i32, i32),
}

#[derive(Clone, Debug)]
pub struct AudioInfo {
    pub codec: String,
    pub sample_rate: u32,
    pub channels: u16,
}

/// A decoded picture. GPU display retains YUV; CPU-only builds retain BGRA.
#[derive(Debug)]
pub struct VideoFrame {
    pub pts_us: u64,
    pub width: u32,
    pub height: u32,
    #[cfg(not(feature = "gpu"))]
    pub bgra: Vec<u8>,
    #[cfg(feature = "gpu")]
    pub picture: std::sync::Arc<crate::frame::DecodedPicture>,
}

impl VideoFrame {
    /// Guest pixel requests convert once on demand; display never requests BGRA.
    pub fn bgra(&self) -> std::borrow::Cow<'_, [u8]> {
        #[cfg(not(feature = "gpu"))]
        {
            std::borrow::Cow::Borrowed(&self.bgra)
        }
        #[cfg(feature = "gpu")]
        {
            std::borrow::Cow::Owned(self.picture.bgra())
        }
    }

    pub fn into_bgra(self) -> Vec<u8> {
        #[cfg(not(feature = "gpu"))]
        {
            self.bgra
        }
        #[cfg(feature = "gpu")]
        {
            self.picture.bgra()
        }
    }

    pub fn into_surface(self) -> crate::surface::VideoFrame {
        crate::surface::VideoFrame {
            width: self.width,
            height: self.height,
            pitch: self.width * 4,
            #[cfg(not(feature = "gpu"))]
            bgra: self.bgra.into(),
            #[cfg(feature = "gpu")]
            bgra: {
                static EMPTY: std::sync::OnceLock<std::sync::Arc<[u8]>> =
                    std::sync::OnceLock::new();
                EMPTY.get_or_init(|| std::sync::Arc::from([])).clone()
            },
            #[cfg(feature = "gpu")]
            picture: Some(self.picture),
        }
    }
}

/// Interleaved, source-rate PCM. `pts_us` is relative to the start of the media.
#[derive(Debug)]
pub struct AudioFrame {
    pub pts_us: u64,
    pub sample_rate: u32,
    pub channels: u16,
    pub samples: Vec<f32>,
}

#[derive(Default, Debug)]
pub struct Update {
    /// Only present when a new frame becomes due.
    pub video: Option<VideoFrame>,
    pub audio: Vec<AudioFrame>,
    pub ended: bool,
}

#[derive(Clone, Copy, Default, Debug)]
pub struct Stats {
    pub packets: u64,
    pub video_frames: u64,
    pub converted_frames: u64,
    pub audio_frames: u64,
    /// Damaged or partial audio packets, including a PES fragment after a container seek.
    pub dropped_audio_packets: u64,
    /// Undecodable video packets or pictures, such as those before the first
    /// sequence header after a seek.
    pub dropped_video_packets: u64,
    pub max_queued_video_packets: usize,
    pub max_queued_video_bytes: usize,
}

#[cfg(any(feature = "media", test))]
mod audio_clock;
#[cfg(feature = "media")]
mod native;
#[cfg(feature = "media")]
mod packets;
#[cfg(all(feature = "media", feature = "gpu"))]
pub(crate) mod planes;
#[cfg(feature = "media")]
mod worker;

/// How a container describes a compressed stream: enough to rebuild a decoder
/// and the BITMAPINFOHEADER or WAVEFORMATEX a Windows media type carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CodecFormat {
    Video {
        fourcc: u32,
        width: u32,
        height: u32,
        bit_count: u16,
        /// Nominal frame duration; zero when the container gives none.
        frame_100ns: u64,
        extradata: Vec<u8>,
    },
    Audio {
        format_tag: u16,
        rate: u32,
        channels: u16,
        block_align: u16,
        bits: u16,
        bytes_per_second: u32,
        extradata: Vec<u8>,
    },
}

/// One compressed sample. Times are relative to the start of the media.
#[derive(Clone, Debug)]
pub struct Packet {
    pub time_100ns: u64,
    pub duration_100ns: u64,
    pub key: bool,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct PacketStream {
    /// The container's stream number (ASF numbers start at 1).
    pub number: u16,
    pub format: CodecFormat,
    pub bit_rate: u32,
    pub max_packet: u32,
    pub packets: Vec<Packet>,
}

/// A whole container split into per-stream compressed samples.
#[derive(Clone, Debug)]
pub struct Demuxed {
    pub container: String,
    pub duration_100ns: u64,
    pub streams: Vec<PacketStream>,
}

/// Split a whole in-memory container into its audio and video samples.
pub fn demux(bytes: Vec<u8>) -> Result<Demuxed, Error> {
    #[cfg(feature = "media")]
    {
        packets::demux(bytes)
    }
    #[cfg(not(feature = "media"))]
    {
        let _ = bytes;
        Err(Error::Unavailable)
    }
}

#[derive(Debug)]
pub enum Decoded {
    Video(VideoFrame),
    Audio(AudioFrame),
}

/// Decodes the samples of one stream as they arrive, in decode order. Frame
/// times come from the sample times given to [`PacketDecoder::decode`].
pub struct PacketDecoder {
    #[cfg(feature = "media")]
    inner: packets::Decoder,
}

impl PacketDecoder {
    pub fn open(format: &CodecFormat) -> Result<Self, Error> {
        #[cfg(feature = "media")]
        {
            Ok(Self {
                inner: packets::Decoder::open(format)?,
            })
        }
        #[cfg(not(feature = "media"))]
        {
            let _ = format;
            Err(Error::Unavailable)
        }
    }

    pub fn decode(&mut self, data: &[u8], time_100ns: Option<u64>) -> Result<Vec<Decoded>, Error> {
        #[cfg(feature = "media")]
        {
            self.inner.decode(data, time_100ns)
        }
        #[cfg(not(feature = "media"))]
        {
            let _ = (data, time_100ns);
            Err(Error::Unavailable)
        }
    }

    /// Output every frame still held for reordering, then accept a new stream.
    pub fn drain(&mut self) -> Result<Vec<Decoded>, Error> {
        #[cfg(feature = "media")]
        {
            self.inner.drain()
        }
        #[cfg(not(feature = "media"))]
        {
            Err(Error::Unavailable)
        }
    }

    /// Discard held frames, as after a flush or seek.
    pub fn reset(&mut self) {
        #[cfg(feature = "media")]
        self.inner.reset();
    }
}

/// A seekable source decoded on a thread of its own. `frame_at` answers as if it
/// decoded only through the first frame after the requested time; repeated calls
/// for the same time do no work. Audio-only sources are supported.
pub struct Decoder {
    #[cfg(feature = "media")]
    inner: worker::Worker,
}

impl Decoder {
    pub fn available() -> bool {
        cfg!(feature = "media")
    }

    pub fn open_memory(bytes: Vec<u8>) -> Result<Self, Error> {
        Self::open_reader(std::io::Cursor::new(bytes))
    }

    pub fn open_reader(reader: impl ReadSeek + 'static) -> Result<Self, Error> {
        #[cfg(feature = "media")]
        {
            Ok(Self {
                inner: worker::Worker::open(Box::new(reader))?,
            })
        }
        #[cfg(not(feature = "media"))]
        {
            let _ = reader;
            Err(Error::Unavailable)
        }
    }

    pub fn info(&self) -> &StreamInfo {
        #[cfg(feature = "media")]
        {
            &self.inner.info
        }
        #[cfg(not(feature = "media"))]
        {
            unreachable!("a disabled decoder cannot be constructed")
        }
    }

    /// Submit or collect a frame without waiting on the execution owner.
    pub fn poll_frame(&mut self, media_us: u64) -> Result<Option<Update>, Error> {
        #[cfg(feature = "media")]
        {
            self.inner.poll_frame(media_us)
        }
        #[cfg(not(feature = "media"))]
        {
            let _ = media_us;
            Err(Error::Unavailable)
        }
    }

    /// Queue a seek before the next asynchronous frame request.
    pub fn request_seek(&mut self, media_us: u64) -> Result<(), Error> {
        #[cfg(feature = "media")]
        {
            self.inner.request_seek(media_us)
        }
        #[cfg(not(feature = "media"))]
        {
            let _ = media_us;
            Err(Error::Unavailable)
        }
    }

    pub fn frame_at(&mut self, media_us: u64) -> Result<Update, Error> {
        #[cfg(feature = "media")]
        {
            self.inner.frame_at(media_us)
        }
        #[cfg(not(feature = "media"))]
        {
            let _ = media_us;
            Err(Error::Unavailable)
        }
    }

    pub fn seek(&mut self, media_us: u64) -> Result<(), Error> {
        #[cfg(feature = "media")]
        {
            self.inner.seek(media_us)
        }
        #[cfg(not(feature = "media"))]
        {
            let _ = media_us;
            Err(Error::Unavailable)
        }
    }

    pub fn stats(&self) -> Stats {
        #[cfg(feature = "media")]
        {
            self.inner.stats
        }
        #[cfg(not(feature = "media"))]
        {
            Stats::default()
        }
    }
}

/// Converts pushed host BGRA frames into presentable [`VideoFrame`]s.
#[derive(Default)]
pub struct RawVideoOutput {
    #[cfg(all(feature = "gpu", feature = "media"))]
    output: Option<planes::Output>,
    #[cfg(all(feature = "gpu", feature = "media"))]
    dimensions: (u32, u32),
}

impl RawVideoOutput {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push_bgra(
        &mut self,
        pts_us: u64,
        width: u32,
        height: u32,
        bgra: Vec<u8>,
    ) -> Result<VideoFrame, Error> {
        let expected = (width as usize)
            .checked_mul(height as usize)
            .and_then(|pixels| pixels.checked_mul(4));
        if width == 0 || height == 0 || expected != Some(bgra.len()) {
            return Err(Error::Decode("invalid pushed BGRA frame dimensions".into()));
        }
        #[cfg(not(feature = "gpu"))]
        {
            Ok(VideoFrame {
                pts_us,
                width,
                height,
                bgra,
            })
        }
        #[cfg(all(feature = "gpu", feature = "media"))]
        {
            if self.output.is_none() || self.dimensions != (width, height) {
                let device = crate::gpu::Gpu::execution(false)
                    .map_err(|error| Error::Decode(error.to_string()))?
                    .device()
                    .clone();
                let output = planes::Output::new_raw(
                    &device,
                    width,
                    height,
                    ffmpeg_next::format::Pixel::BGRA,
                    adv_gpu::present::YuvColor::Bt709Full,
                )?;
                self.output = Some(output);
                self.dimensions = (width, height);
            }
            let mut frame =
                ffmpeg_next::frame::Video::new(ffmpeg_next::format::Pixel::BGRA, width, height);
            let row_bytes = width as usize * 4;
            let dst_stride = frame.stride(0);
            let dst = frame.data_mut(0);
            for y in 0..height as usize {
                let src_start = y * row_bytes;
                let dst_start = y * dst_stride;
                dst[dst_start..dst_start + row_bytes]
                    .copy_from_slice(&bgra[src_start..src_start + row_bytes]);
            }
            self.output.as_mut().unwrap().convert(pts_us, &mut frame)
        }
        #[cfg(all(feature = "gpu", not(feature = "media")))]
        {
            let _ = pts_us;
            Err(Error::Unavailable)
        }
    }
}

#[cfg(test)]
mod tests {
    #[cfg(not(feature = "media"))]
    #[test]
    fn disabled_backend_is_explicit() {
        assert!(matches!(
            super::Decoder::open_memory(vec![]),
            Err(super::Error::Unavailable)
        ));
    }
}

pub mod wine;
