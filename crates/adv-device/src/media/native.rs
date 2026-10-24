use std::collections::VecDeque;
use std::ffi::c_void;
use std::io::SeekFrom;
use std::ptr;

use ff::{Rational, codec, format, frame, media, software};
use ffmpeg_next as ff;

use super::{
    AudioFrame, AudioInfo, Error, ReadSeek, Stats, StreamInfo, Update, VideoFrame, VideoInfo,
};

impl From<ff::Error> for Error {
    fn from(value: ff::Error) -> Self {
        Self::Decode(value.to_string())
    }
}

/// Input must be dropped before its custom AVIO buffer and reader.
pub(super) struct Input {
    pub context: format::context::Input,
    _io: Io,
}

struct Io {
    context: *mut ff::ffi::AVIOContext,
    _reader: Box<Box<dyn ReadSeek>>,
}

impl Drop for Io {
    fn drop(&mut self) {
        // FFmpeg may replace the original buffer; free the current one, then the context.
        unsafe {
            ff::ffi::av_free((*self.context).buffer.cast());
            ff::ffi::avio_context_free(&mut self.context);
        }
    }
}

unsafe extern "C" fn read(opaque: *mut c_void, buf: *mut u8, len: i32) -> i32 {
    if len <= 0 {
        return ff::ffi::AVERROR(ff::ffi::EINVAL);
    }
    // AVIO owns this call; the reader and writable buffer outlive it. Catch reader
    // panics so unwinding never crosses an FFI boundary.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        (*opaque.cast::<Box<dyn ReadSeek>>())
            .read(std::slice::from_raw_parts_mut(buf, len as usize))
    }));
    match result {
        Ok(Ok(0)) => ff::ffi::AVERROR_EOF,
        Ok(Ok(n)) => n as i32,
        _ => ff::ffi::AVERROR(ff::ffi::EIO),
    }
}

unsafe extern "C" fn seek(opaque: *mut c_void, off: i64, whence: i32) -> i64 {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let reader = unsafe { &mut *opaque.cast::<Box<dyn ReadSeek>>() };
        let mode = whence & !ff::ffi::AVSEEK_FORCE;
        if mode == ff::ffi::AVSEEK_SIZE {
            let old = reader.stream_position()?;
            let end = reader.seek(SeekFrom::End(0))?;
            reader.seek(SeekFrom::Start(old))?;
            Ok(end)
        } else {
            let pos = match mode {
                0 if off >= 0 => SeekFrom::Start(off as u64),
                1 => SeekFrom::Current(off),
                2 => SeekFrom::End(off),
                _ => return Err(std::io::Error::from(std::io::ErrorKind::InvalidInput)),
            };
            reader.seek(pos)
        }
    }));
    match result {
        Ok(Ok(n)) if n <= i64::MAX as u64 => n as i64,
        _ => i64::from(ff::ffi::AVERROR(ff::ffi::EIO)),
    }
}

impl Input {
    pub fn open(reader: Box<dyn ReadSeek>) -> Result<Self, Error> {
        let mut reader = Box::new(reader);
        unsafe {
            let buffer = ff::ffi::av_malloc(32768).cast::<u8>();
            if buffer.is_null() {
                return Err(Error::Decode("AVIO allocation failed".into()));
            }
            let context = ff::ffi::avio_alloc_context(
                buffer,
                32768,
                0,
                (&mut *reader as *mut Box<dyn ReadSeek>).cast(),
                Some(read),
                None,
                Some(seek),
            );
            if context.is_null() {
                ff::ffi::av_free(buffer.cast());
                return Err(Error::Decode("AVIO context allocation failed".into()));
            }
            let io = Io {
                context,
                _reader: reader,
            };
            let mut format = ff::ffi::avformat_alloc_context();
            if format.is_null() {
                return Err(Error::Decode("format allocation failed".into()));
            }
            (*format).pb = io.context;
            (*format).flags |= ff::ffi::AVFMT_FLAG_CUSTOM_IO;
            let result = ff::ffi::avformat_open_input(
                &mut format,
                ptr::null(),
                ptr::null(),
                ptr::null_mut(),
            );
            if result < 0 {
                ff::ffi::avformat_close_input(&mut format);
                return Err(ff::Error::from(result).into());
            }
            let mut input = Self {
                context: format::context::Input::wrap(format),
                _io: io,
            };
            let result =
                ff::ffi::avformat_find_stream_info(input.context.as_mut_ptr(), ptr::null_mut());
            if result < 0 {
                return Err(ff::Error::from(result).into());
            }
            Ok(input)
        }
    }
    /// Packet-tail duration for program streams whose sparse PTS underestimate
    /// their length. Restore byte zero before decoding starts.
    pub(super) fn packet_end_100ns(&mut self) -> Result<u64, Error> {
        const TAIL: i64 = 256 * 1024;
        // SAFETY: this open context owns its AVIO and supports byte seeks.
        let origin = unsafe {
            let context = self.context.as_mut_ptr();
            let length = ff::ffi::avio_size((*context).pb);
            ff::ffi::av_seek_frame(
                context,
                -1,
                length.saturating_sub(TAIL).max(0),
                ff::ffi::AVSEEK_FLAG_BYTE,
            );
            (*context).start_time
        };
        let mut end = 0;
        let mut packet = ff::Packet::empty();
        while packet.read(&mut self.context).is_ok() {
            let Some(stream) = self.context.stream(packet.stream()) else {
                continue;
            };
            let Some(stamp) = packet.pts().or(packet.dts()) else {
                continue;
            };
            let tb = stream.time_base();
            let time =
                ((i128::from(stamp + packet.duration()) * i128::from(tb.numerator()) * 10_000_000)
                    / i128::from(tb.denominator().max(1)))
                .clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64;
            end = end.max(time);
        }
        // SAFETY: restore this same open context before any decoder uses it.
        let result = unsafe {
            ff::ffi::av_seek_frame(self.context.as_mut_ptr(), -1, 0, ff::ffi::AVSEEK_FLAG_BYTE)
        };
        if result < 0 {
            return Err(ff::Error::from(result).into());
        }
        let origin = if origin == ff::ffi::AV_NOPTS_VALUE {
            0
        } else {
            origin * 10
        };
        Ok(end.saturating_sub(origin).max(0) as u64)
    }
}

struct Audio {
    decoder: ff::decoder::Audio,
    stream: usize,
    time_base: Rational,
    converter: software::resampling::Context,
    next_us: u64,
    pcm_next_us: u64,
    sample_clock: Option<super::audio_clock::AudioClock>,
}

struct Video {
    decoder: ff::decoder::Video,
    stream: usize,
    time_base: Rational,
    scaler: software::scaling::Context,
    frame_duration_us: u64,
}

pub(super) struct Native {
    input: Input,
    video: Option<Video>,
    audio: Option<Audio>,
    video_packets: VecDeque<ff::Packet>,
    video_packet_bytes: usize,
    /// Decoded future frames in pts order; `prefetch` keeps [`AHEAD`] of them.
    next: VecDeque<(u64, frame::Video)>,
    /// The front of `next`, already converted by `prefetch`.
    converted: Option<VideoFrame>,
    /// A decode error `prefetch` hit; the request that needs that frame reports it.
    deferred: Option<Error>,
    audio_out: Vec<AudioFrame>,
    origin_us: i64,
    last_request: Option<u64>,
    last_video_us: u64,
    discard_audio_before: u64,
    eof_sent: bool,
    audio_eof_sent: bool,
    input_eof: bool,
    eof: bool,
    seek_pending: bool,
    pub info: StreamInfo,
    pub stats: Stats,
}

// Some ASF files interleave audio packets more than two seconds behind video packets.
// Hold compressed video packets while reading just enough audio for the current time.
const AUDIO_LEAD_US: u64 = 100_000;
/// Audio-only streams have no interleaving to respect; decode about as far
/// ahead as quartz's DirectSound renderer buffers (one second), so a busy
/// execution thread during a load does not drain music that just started.
const AUDIO_ONLY_LEAD_US: u64 = 1_000_000;
/// Decoded frames `prefetch` holds, so a request finds its frame and the one
/// after it ready.
const AHEAD: usize = 2;
const MAX_QUEUED_VIDEO_PACKETS: usize = 180;
const MAX_QUEUED_VIDEO_BYTES: usize = 8 << 20;

fn micros(pts: i64, tb: Rational) -> i64 {
    ((i128::from(pts) * i128::from(tb.numerator()) * 1_000_000)
        / i128::from(tb.denominator().max(1)))
    .clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
}

fn convert(video: &mut Video, pts_us: u64, decoded: &frame::Video) -> Result<VideoFrame, Error> {
    let mut bgra = frame::Video::empty();
    video.scaler.run(decoded, &mut bgra)?;
    let (width, height) = (bgra.width(), bgra.height());
    let row = width as usize * 4;
    let mut pixels = Vec::with_capacity(row * height as usize);
    for line in bgra.data(0).chunks(bgra.stride(0)).take(height as usize) {
        pixels.extend_from_slice(&line[..row]);
    }
    Ok(VideoFrame {
        pts_us,
        width,
        height,
        bgra: pixels,
    })
}

fn open_video(stream: &format::stream::Stream<'_>) -> Result<Video, Error> {
    let (index, time_base, rate) = (stream.index(), stream.time_base(), stream.avg_frame_rate());
    let mut context = codec::Context::from_parameters(stream.parameters())?;
    // One decoder thread: a visual novel never needs a worker per logical CPU.
    context.set_threading(codec::threading::Config::count(1));
    let mut decoder = context.decoder();
    decoder.set_packet_time_base(time_base);
    let decoder = decoder.video()?;
    let (w, h) = (decoder.width(), decoder.height());
    if w == 0 || h == 0 || u64::from(w) * u64::from(h) > 16_777_216 {
        return Err(Error::Decode(
            "invalid or excessive video dimensions".into(),
        ));
    }
    let scaler = software::scaling::Context::get(
        decoder.format(),
        w,
        h,
        format::Pixel::BGRA,
        w,
        h,
        software::scaling::Flags::BILINEAR,
    )?;
    let frame_duration_us = if rate.numerator() > 0 {
        (1_000_000 * rate.denominator() as u64 / rate.numerator() as u64).max(1)
    } else {
        40_000
    };
    Ok(Video {
        decoder,
        stream: index,
        time_base,
        scaler,
        frame_duration_us,
    })
}

fn open_audio(stream: &format::stream::Stream<'_>, asf: bool) -> Result<Audio, Error> {
    let mut decoder = codec::Context::from_parameters(stream.parameters())?.decoder();
    // Codec parameters do not carry the packet time base. FFmpeg needs it before
    // opening to adjust timestamps when removing codec priming samples.
    decoder.set_packet_time_base(stream.time_base());
    let mut decoder = decoder.audio()?;
    let mut layout = decoder.channel_layout();
    if layout.is_empty() {
        layout = ff::ChannelLayout::default(i32::from(decoder.channels()));
        decoder.set_channel_layout(layout);
    }
    if decoder.rate() == 0 || decoder.channels() == 0 || decoder.channels() > 8 {
        return Err(Error::Decode("invalid audio layout".into()));
    }
    let converter = software::resampling::Context::get(
        decoder.format(),
        layout,
        decoder.rate(),
        format::Sample::F32(format::sample::Type::Packed),
        layout,
        decoder.rate(),
    )?;
    let sample_clock = (asf && matches!(decoder.id(), codec::Id::WMAV1 | codec::Id::WMAV2))
        .then(super::audio_clock::AudioClock::default);
    Ok(Audio {
        decoder,
        stream: stream.index(),
        time_base: stream.time_base(),
        converter,
        next_us: 0,
        pcm_next_us: 0,
        sample_clock,
    })
}

impl Native {
    pub fn open(reader: Box<dyn ReadSeek>) -> Result<Self, Error> {
        ff::init()?;
        let mut input = Input::open(reader)?;
        let mut duration_us = input.context.duration().max(0) as u64;
        // SAFETY: the context is open; this reads its estimation method.
        let method = unsafe { (*input.context.as_ptr()).duration_estimation_method };
        if method == ff::ffi::AVDurationEstimationMethod::AVFMT_DURATION_FROM_PTS {
            duration_us = duration_us.max(input.packet_end_100ns()? / 10);
        }
        let asf = input.context.format().name() == "asf";
        let video = input
            .context
            .streams()
            .best(media::Type::Video)
            .map(|stream| open_video(&stream))
            .transpose()?;
        let audio = match input.context.streams().best(media::Type::Audio) {
            // A damaged or unsupported audio track leaves a playable picture.
            Some(stream) if video.is_some() => open_audio(&stream, asf)
                .map_err(|error| log::warn!("movie audio disabled: {error}"))
                .ok(),
            Some(stream) => Some(open_audio(&stream, asf)?),
            None => None,
        };
        if video.is_none() && audio.is_none() {
            return Err(ff::Error::StreamNotFound.into());
        }
        let origin_us = unsafe { (*input.context.as_ptr()).start_time };
        let origin_us = if origin_us == ff::ffi::AV_NOPTS_VALUE {
            0
        } else {
            origin_us
        };
        let info = StreamInfo {
            container: input.context.format().name().into(),
            duration_us,
            video: video.as_ref().map(|v| {
                let rate = input
                    .context
                    .stream(v.stream)
                    .map_or(Rational::new(0, 1), |s| s.avg_frame_rate());
                VideoInfo {
                    codec: v.decoder.id().name().into(),
                    width: v.decoder.width(),
                    height: v.decoder.height(),
                    frame_rate: (rate.numerator(), rate.denominator()),
                }
            }),
            audio: audio.as_ref().map(|a| AudioInfo {
                codec: a.decoder.id().name().into(),
                sample_rate: a.decoder.rate(),
                channels: a.decoder.channels(),
            }),
        };
        Ok(Self {
            input,
            video,
            audio,
            video_packets: VecDeque::new(),
            video_packet_bytes: 0,
            next: VecDeque::new(),
            converted: None,
            deferred: None,
            audio_out: Vec::new(),
            origin_us,
            last_request: None,
            last_video_us: 0,
            discard_audio_before: 0,
            eof_sent: false,
            audio_eof_sent: false,
            input_eof: false,
            eof: false,
            seek_pending: false,
            info,
            stats: Stats::default(),
        })
    }

    fn receive_audio(&mut self) -> Result<(), Error> {
        let Some(audio) = self.audio.as_mut() else {
            return Ok(());
        };
        let mut decoded = frame::Audio::empty();
        loop {
            match audio.decoder.receive_frame(&mut decoded) {
                Ok(()) => {}
                Err(
                    ff::Error::Eof
                    | ff::Error::Other {
                        errno: ff::ffi::EAGAIN,
                    },
                ) => break,
                Err(ff::Error::InvalidData) => {
                    self.stats.dropped_audio_packets += 1;
                    break;
                }
                Err(e) => return Err(e.into()),
            }
            self.stats.audio_frames += 1;
            let pts = decoded
                .timestamp()
                .map(|p| {
                    micros(p, audio.time_base)
                        .saturating_sub(self.origin_us)
                        .max(0) as u64
                })
                .unwrap_or(audio.next_us);
            let mut pcm = frame::Audio::empty();
            audio.converter.run(&decoded, &mut pcm)?;
            let (rate, channels) = (audio.decoder.rate(), audio.decoder.channels());
            // The container timeline decides guest-visible end and seek; only the
            // device PCM timeline is de-jittered.
            audio.next_us = pts.saturating_add(pcm.samples() as u64 * 1_000_000 / u64::from(rate));
            // best_effort_timestamp may precede FFmpeg's priming adjustment; pts includes it.
            let pts = decoded
                .pts()
                .map(|p| {
                    micros(p, audio.time_base)
                        .saturating_sub(self.origin_us)
                        .max(0) as u64
                })
                .unwrap_or(pts);
            let pts = audio
                .sample_clock
                .as_mut()
                .map_or(pts, |clock| clock.stamp(pts, pcm.samples(), rate));
            audio.pcm_next_us =
                pts.saturating_add(pcm.samples() as u64 * 1_000_000 / u64::from(rate));
            let skip = (self.discard_audio_before.saturating_sub(pts) * u64::from(rate) / 1_000_000)
                .min(pcm.samples() as u64) as usize;
            if skip == pcm.samples() {
                continue;
            }
            let count = pcm.samples() * usize::from(channels);
            let samples = pcm.data(0)[skip * usize::from(channels) * 4..count * 4]
                .as_chunks::<4>()
                .0
                .iter()
                .map(|v| f32::from_ne_bytes(*v))
                .collect();
            self.audio_out.push(AudioFrame {
                pts_us: pts + skip as u64 * 1_000_000 / u64::from(rate),
                sample_rate: rate,
                channels,
                samples,
            });
            // Malformed files with no next video timestamp must not accumulate PCM.
            if self.audio_out.len() > 512 {
                return Err(Error::Decode("excessive audio interleave".into()));
            }
        }
        Ok(())
    }

    fn next_video(&mut self) -> Result<Option<(u64, frame::Video)>, Error> {
        loop {
            let video = self.video.as_mut().expect("video stream");
            let mut decoded = frame::Video::empty();
            match video.decoder.receive_frame(&mut decoded) {
                Ok(()) => {
                    self.stats.video_frames += 1;
                    let pts = decoded
                        .timestamp()
                        .map(|p| {
                            micros(p, video.time_base)
                                .saturating_sub(self.origin_us)
                                .max(0) as u64
                        })
                        .unwrap_or(self.last_video_us + video.frame_duration_us);
                    self.last_video_us = pts;
                    return Ok(Some((pts, decoded)));
                }
                Err(ff::Error::Eof) => {
                    self.eof = true;
                    return Ok(None);
                }
                Err(ff::Error::Other {
                    errno: ff::ffi::EAGAIN,
                }) => {}
                // A picture decoded before any sequence header (after a container
                // seek into an unindexed stream) is skipped, as players do.
                Err(ff::Error::InvalidData) => self.stats.dropped_video_packets += 1,
                Err(e) => return Err(e.into()),
            }
            match self.next_packet()? {
                Some(packet) => {
                    let video = self.video.as_mut().unwrap();
                    if packet.stream() == video.stream {
                        match video.decoder.send_packet(&packet) {
                            Ok(()) => {}
                            Err(ff::Error::InvalidData) => self.stats.dropped_video_packets += 1,
                            Err(e) => return Err(e.into()),
                        }
                    } else if self
                        .audio
                        .as_ref()
                        .is_some_and(|a| a.stream == packet.stream())
                    {
                        self.send_audio_packet(&packet)?;
                    }
                }
                None if !self.eof_sent => {
                    self.eof_sent = true;
                    self.video.as_mut().unwrap().decoder.send_eof()?;
                    self.finish_audio()?;
                }
                None => {
                    self.eof = true;
                    return Ok(None);
                }
            }
        }
    }

    fn read_source_packet(&mut self) -> Result<Option<ff::Packet>, Error> {
        if self.input_eof {
            return Ok(None);
        }
        let mut packet = ff::Packet::empty();
        match packet.read(&mut self.input.context) {
            Ok(()) => {
                self.stats.packets += 1;
                Ok(Some(packet))
            }
            Err(ff::Error::Eof) => {
                self.input_eof = true;
                Ok(None)
            }
            Err(err) => Err(err.into()),
        }
    }

    fn next_packet(&mut self) -> Result<Option<ff::Packet>, Error> {
        if let Some(packet) = self.video_packets.pop_front() {
            self.video_packet_bytes -= packet.size();
            Ok(Some(packet))
        } else {
            self.read_source_packet()
        }
    }

    fn send_audio_packet(&mut self, packet: &ff::Packet) -> Result<(), Error> {
        match self.audio.as_mut().unwrap().decoder.send_packet(packet) {
            Ok(()) => self.receive_audio(),
            // Seeking MPEG-PS can expose a partial audio PES packet.
            Err(ff::Error::InvalidData) => {
                self.stats.dropped_audio_packets += 1;
                Ok(())
            }
            Err(err) => Err(err.into()),
        }
    }

    fn finish_audio(&mut self) -> Result<(), Error> {
        if !self.audio_eof_sent {
            self.audio_eof_sent = true;
            if let Some(audio) = self.audio.as_mut() {
                audio.decoder.send_eof()?;
                self.receive_audio()?;
            }
        }
        Ok(())
    }

    fn read_ahead_audio(&mut self, media_us: u64) -> Result<(), Error> {
        let lead = if self.video.is_none() {
            AUDIO_ONLY_LEAD_US
        } else {
            AUDIO_LEAD_US
        };
        let target = media_us.saturating_add(lead);
        let video_stream = self.video.as_ref().map(|v| v.stream);
        while self
            .audio
            .as_ref()
            .is_some_and(|audio| audio.next_us.min(audio.pcm_next_us) < target)
            && self.video_packets.len() < MAX_QUEUED_VIDEO_PACKETS
            && self.video_packet_bytes < MAX_QUEUED_VIDEO_BYTES
        {
            let Some(packet) = self.read_source_packet()? else {
                self.finish_audio()?;
                break;
            };
            if Some(packet.stream()) == video_stream {
                self.video_packet_bytes += packet.size();
                self.video_packets.push_back(packet);
                self.stats.max_queued_video_packets = self
                    .stats
                    .max_queued_video_packets
                    .max(self.video_packets.len());
                self.stats.max_queued_video_bytes = self
                    .stats
                    .max_queued_video_bytes
                    .max(self.video_packet_bytes);
            } else if self
                .audio
                .as_ref()
                .is_some_and(|audio| audio.stream == packet.stream())
            {
                self.send_audio_packet(&packet)?;
            }
        }
        Ok(())
    }

    pub fn seek(&mut self, media_us: u64) -> Result<(), Error> {
        if media_us == 0 {
            // Rewind by bytes: timestamp seeks in unindexed MPEG-PS can land past
            // the first sequence header.
            let result = unsafe {
                ff::ffi::avformat_seek_file(
                    self.input.context.as_mut_ptr(),
                    -1,
                    0,
                    0,
                    0,
                    ff::ffi::AVSEEK_FLAG_BYTE,
                )
            };
            if result < 0 {
                return Err(ff::Error::from(result).into());
            }
        } else {
            let ts = (media_us.min(i64::MAX as u64) as i64).saturating_add(self.origin_us);
            self.input.context.seek(ts, ..ts)?;
        }
        if let Some(v) = self.video.as_mut() {
            v.decoder.flush();
        }
        if let Some(a) = self.audio.as_mut() {
            a.decoder.flush();
            a.next_us = media_us;
            a.pcm_next_us = media_us;
            if let Some(clock) = &mut a.sample_clock {
                *clock = super::audio_clock::AudioClock::default();
            }
        }
        self.next.clear();
        self.converted = None;
        self.deferred = None;
        self.video_packets.clear();
        self.video_packet_bytes = 0;
        self.audio_out.clear();
        self.discard_audio_before = media_us;
        self.eof_sent = false;
        self.audio_eof_sent = false;
        self.input_eof = false;
        self.eof = false;
        self.last_request = None;
        self.last_video_us = media_us;
        self.seek_pending = true;
        Ok(())
    }

    pub fn frame_at(&mut self, media_us: u64) -> Result<Update, Error> {
        if self.last_request.is_some_and(|t| media_us < t)
            || media_us.saturating_sub(self.last_request.unwrap_or(0)) > 1_000_000
        {
            self.seek(media_us)?;
        }
        self.last_request = Some(media_us);
        if self.video.is_none() {
            self.seek_pending = false;
            self.read_ahead_audio(media_us)?;
            let audio_end = self.audio.as_ref().map_or(0, |a| a.next_us);
            return Ok(Update {
                video: None,
                audio: std::mem::take(&mut self.audio_out),
                ended: self.input_eof && self.audio_eof_sent && media_us >= audio_end,
            });
        }
        let (mut due, mut ready) = (None, None);
        loop {
            if self.next.is_empty() && !self.eof {
                if let Some(error) = self.deferred.take() {
                    return Err(error);
                }
                match self.next_video() {
                    Ok(next) => self.next.extend(next),
                    Err(_) if self.seek_pending && media_us > 0 => {
                        return self.restart_at(media_us);
                    }
                    Err(e) => return Err(e),
                };
            }
            if self.next.front().is_none_or(|(pts, _)| *pts > media_us) {
                break;
            }
            due = self.next.pop_front();
            ready = self.converted.take();
        }
        // Some MPEG-PS inputs have no usable index and report a successful seek to a
        // future keyframe. Decode from the beginning rather than show it early.
        if self.seek_pending && media_us > 0 && due.is_none() {
            return self.restart_at(media_us);
        }
        self.seek_pending = false;
        self.read_ahead_audio(media_us)?;
        let video = match (ready, due) {
            (Some(frame), _) => Some(frame),
            (None, Some((pts_us, decoded))) => {
                Some(convert(self.video.as_mut().unwrap(), pts_us, &decoded)?)
            }
            (None, None) => None,
        };
        if video.is_some() {
            self.stats.converted_frames += 1;
        }
        let frame_duration = self.video.as_ref().map_or(0, |v| v.frame_duration_us);
        let end = self
            .last_video_us
            .saturating_add(frame_duration)
            .max(self.audio.as_ref().map_or(0, |a| a.next_us));
        Ok(Update {
            video,
            audio: std::mem::take(&mut self.audio_out),
            ended: self.eof && media_us >= end,
        })
    }

    /// Work a request would otherwise do while its caller waits: decode up to
    /// [`AHEAD`] future frames and convert the next one. Results depend only on
    /// the requests made, never on when this runs.
    pub fn prefetch(&mut self) {
        if self.video.is_none() || self.seek_pending || self.deferred.is_some() {
            return;
        }
        while self.next.len() < AHEAD && !self.eof {
            match self.next_video() {
                Ok(next) => self.next.extend(next),
                Err(error) => {
                    self.deferred = Some(error);
                    break;
                }
            }
        }
        if self.converted.is_none()
            && let Some((pts_us, decoded)) = self.next.front()
        {
            // A failed conversion is retried, and reported, by the request.
            self.converted = convert(self.video.as_mut().unwrap(), *pts_us, decoded).ok();
        }
    }

    fn restart_at(&mut self, media_us: u64) -> Result<Update, Error> {
        self.seek(0)?;
        self.seek_pending = false;
        self.discard_audio_before = media_us;
        self.last_request = Some(media_us);
        self.frame_at(media_us)
    }
}
