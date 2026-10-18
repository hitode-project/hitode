//! Container packets and per-stream decoders. A front that hands compressed
//! samples to the guest (a Windows Media reader) demuxes here, and a front that
//! receives them back (a DMO decoder) rebuilds the decoder from the same format.
use ff::{Rational, codec, format, frame, media, software};
use ffmpeg_next as ff;

use super::native::Input;
use super::{AudioFrame, CodecFormat, Decoded, Demuxed, Error, Packet, PacketStream, VideoFrame};

const HUNDRED_NS: Rational = Rational(1, 10_000_000);
/// AV_INPUT_BUFFER_PADDING_SIZE: decoders may over-read extradata by this much.
const PADDING: usize = 64;

fn hundred_ns(pts: i64, tb: Rational) -> i64 {
    ((i128::from(pts) * i128::from(tb.numerator()) * 10_000_000)
        / i128::from(tb.denominator().max(1)))
    .clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
}

fn format_of(stream: &format::stream::Stream<'_>) -> Option<CodecFormat> {
    // SAFETY: the parameters belong to a live stream of an open input.
    let p = unsafe { &*stream.parameters().as_ptr() };
    let extradata = if p.extradata.is_null() || p.extradata_size <= 0 {
        Vec::new()
    } else {
        unsafe { std::slice::from_raw_parts(p.extradata, p.extradata_size as usize) }.to_vec()
    };
    match stream.parameters().medium() {
        media::Type::Video => {
            let rate = stream.avg_frame_rate();
            let frame_100ns = if rate.numerator() > 0 {
                (10_000_000 * i64::from(rate.denominator()) / i64::from(rate.numerator())) as u64
            } else {
                0
            };
            Some(CodecFormat::Video {
                fourcc: p.codec_tag,
                width: u32::try_from(p.width).ok()?,
                height: u32::try_from(p.height).ok()?,
                bit_count: u16::try_from(p.bits_per_coded_sample).unwrap_or(24),
                frame_100ns,
                extradata,
            })
        }
        media::Type::Audio => Some(CodecFormat::Audio {
            format_tag: u16::try_from(p.codec_tag).ok()?,
            rate: u32::try_from(p.sample_rate).ok()?,
            channels: u16::try_from(p.ch_layout.nb_channels).ok()?,
            block_align: u16::try_from(p.block_align).unwrap_or(0),
            bits: u16::try_from(p.bits_per_coded_sample).unwrap_or(16),
            bytes_per_second: u32::try_from(p.bit_rate / 8).unwrap_or(0),
            extradata,
        }),
        _ => None,
    }
}

pub(super) fn demux(bytes: Vec<u8>) -> Result<Demuxed, Error> {
    ff::init()?;
    let mut input = Input::open(Box::new(std::io::Cursor::new(bytes)))?;
    let context = &mut input.context;
    let origin = unsafe { (*context.as_ptr()).start_time };
    let origin = if origin == ff::ffi::AV_NOPTS_VALUE {
        0
    } else {
        origin * 10
    };
    let mut streams = Vec::new();
    let mut slots = vec![None; context.nb_streams() as usize];
    for stream in context.streams() {
        let Some(format) = format_of(&stream) else {
            continue;
        };
        let number = u16::try_from(stream.id())
            .ok()
            .filter(|n| (1..=127).contains(n))
            .unwrap_or(stream.index() as u16 + 1);
        slots[stream.index()] = Some((streams.len(), stream.time_base()));
        streams.push(PacketStream {
            number,
            format,
            bit_rate: unsafe { (*stream.parameters().as_ptr()).bit_rate }
                .clamp(0, i64::from(u32::MAX)) as u32,
            max_packet: 0,
            packets: Vec::new(),
        });
    }
    if streams.is_empty() {
        return Err(ff::Error::StreamNotFound.into());
    }
    for (stream, packet) in context.packets() {
        let Some((slot, tb)) = slots[stream.index()] else {
            continue;
        };
        let Some(data) = packet.data() else {
            continue;
        };
        let time = packet
            .pts()
            .or(packet.dts())
            .map_or(0, |pts| (hundred_ns(pts, tb) - origin).max(0) as u64);
        let target = &mut streams[slot];
        target.max_packet = target.max_packet.max(data.len() as u32);
        target.packets.push(Packet {
            time_100ns: time,
            duration_100ns: hundred_ns(packet.duration(), tb).max(0) as u64,
            key: packet.is_key(),
            data: data.to_vec(),
        });
    }
    let mut duration = (context.duration().max(0) as u64).saturating_mul(10);
    for stream in &mut streams {
        // Containers often leave video durations unset; derive them from the
        // next sample, falling back to the nominal frame time at the end.
        let fallback = match stream.format {
            CodecFormat::Video { frame_100ns, .. } => frame_100ns,
            CodecFormat::Audio { .. } => 0,
        };
        let times: Vec<u64> = stream.packets.iter().map(|p| p.time_100ns).collect();
        for (i, packet) in stream.packets.iter_mut().enumerate() {
            if packet.duration_100ns == 0 {
                packet.duration_100ns = times
                    .get(i + 1)
                    .map_or(fallback, |next| next.saturating_sub(packet.time_100ns));
            }
            duration = duration.max(packet.time_100ns + packet.duration_100ns);
        }
    }
    Ok(Demuxed {
        container: context.format().name().into(),
        duration_100ns: duration,
        streams,
    })
}

enum Kind {
    Video {
        decoder: ff::decoder::Video,
        scaler: Option<(format::Pixel, u32, u32, software::scaling::Context)>,
    },
    Audio {
        decoder: ff::decoder::Audio,
        converter: Option<software::resampling::Context>,
        clock: Option<super::audio_clock::AudioClock>,
    },
}

pub(super) struct Decoder {
    kind: Kind,
}

fn codec_id(format: &CodecFormat) -> codec::Id {
    // SAFETY: the RIFF tag tables are static and null-terminated by this array.
    unsafe {
        let (table, tag) = match format {
            CodecFormat::Video { fourcc, .. } => (ff::ffi::avformat_get_riff_video_tags(), *fourcc),
            CodecFormat::Audio { format_tag, .. } => (
                ff::ffi::avformat_get_riff_audio_tags(),
                u32::from(*format_tag),
            ),
        };
        let tables = [table, std::ptr::null()];
        ff::ffi::av_codec_get_id(tables.as_ptr(), tag).into()
    }
}

impl Decoder {
    pub fn open(format: &CodecFormat) -> Result<Self, Error> {
        ff::init()?;
        let id = codec_id(format);
        if id == codec::Id::None {
            return Err(Error::Decode(format!("no decoder for {format:?}")));
        }
        let mut parameters = codec::Parameters::new();
        // SAFETY: freshly allocated parameters; extradata is FFmpeg-allocated and
        // padded, and the parameters own it from here on.
        unsafe {
            let p = &mut *parameters.as_mut_ptr();
            p.codec_id = id.into();
            let extradata = match format {
                CodecFormat::Video {
                    fourcc,
                    width,
                    height,
                    bit_count,
                    extradata,
                    ..
                } => {
                    if *width == 0
                        || *height == 0
                        || u64::from(*width) * u64::from(*height) > 16_777_216
                    {
                        return Err(Error::Decode(
                            "invalid or excessive video dimensions".into(),
                        ));
                    }
                    p.codec_type = ff::ffi::AVMediaType::AVMEDIA_TYPE_VIDEO;
                    p.codec_tag = *fourcc;
                    p.width = *width as i32;
                    p.height = *height as i32;
                    p.bits_per_coded_sample = i32::from(*bit_count);
                    extradata
                }
                CodecFormat::Audio {
                    format_tag,
                    rate,
                    channels,
                    block_align,
                    bits,
                    bytes_per_second,
                    extradata,
                } => {
                    if *rate == 0 || *channels == 0 || *channels > 8 {
                        return Err(Error::Decode("invalid audio layout".into()));
                    }
                    p.codec_type = ff::ffi::AVMediaType::AVMEDIA_TYPE_AUDIO;
                    p.codec_tag = u32::from(*format_tag);
                    p.sample_rate = *rate as i32;
                    ff::ffi::av_channel_layout_default(&mut p.ch_layout, i32::from(*channels));
                    p.block_align = i32::from(*block_align);
                    p.bits_per_coded_sample = i32::from(*bits);
                    p.bit_rate = i64::from(*bytes_per_second) * 8;
                    extradata
                }
            };
            if !extradata.is_empty() {
                let buffer = ff::ffi::av_mallocz(extradata.len() + PADDING).cast::<u8>();
                if buffer.is_null() {
                    return Err(Error::Decode("extradata allocation failed".into()));
                }
                std::ptr::copy_nonoverlapping(extradata.as_ptr(), buffer, extradata.len());
                p.extradata = buffer;
                p.extradata_size = extradata.len() as i32;
            }
        }
        let mut context = codec::Context::from_parameters(parameters)?;
        context.set_threading(codec::threading::Config::count(1));
        let mut decoder = context.decoder();
        decoder.set_packet_time_base(HUNDRED_NS);
        let kind = match format {
            CodecFormat::Video { .. } => Kind::Video {
                decoder: decoder.video()?,
                scaler: None,
            },
            CodecFormat::Audio { .. } => Kind::Audio {
                decoder: decoder.audio()?,
                converter: None,
                clock: matches!(id, codec::Id::WMAV1 | codec::Id::WMAV2)
                    .then(super::audio_clock::AudioClock::default),
            },
        };
        Ok(Self { kind })
    }

    pub fn decode(&mut self, data: &[u8], time_100ns: Option<u64>) -> Result<Vec<Decoded>, Error> {
        let mut packet = ff::Packet::copy(data);
        packet.set_pts(time_100ns.map(|t| t as i64));
        packet.set_dts(None);
        let sent = match &mut self.kind {
            Kind::Video { decoder, .. } => decoder.send_packet(&packet),
            Kind::Audio { decoder, .. } => decoder.send_packet(&packet),
        };
        match sent {
            // A damaged sample is dropped, as a Windows decoder would.
            Ok(()) | Err(ff::Error::InvalidData) => {}
            Err(error) => return Err(error.into()),
        }
        self.receive()
    }

    pub fn drain(&mut self) -> Result<Vec<Decoded>, Error> {
        let sent = match &mut self.kind {
            Kind::Video { decoder, .. } => decoder.send_eof(),
            Kind::Audio { decoder, .. } => decoder.send_eof(),
        };
        match sent {
            Ok(()) | Err(ff::Error::Eof) => {}
            Err(error) => return Err(error.into()),
        }
        let frames = self.receive();
        self.reset();
        frames
    }

    pub fn reset(&mut self) {
        match &mut self.kind {
            Kind::Video { decoder, .. } => decoder.flush(),
            Kind::Audio { decoder, clock, .. } => {
                decoder.flush();
                if clock.is_some() {
                    *clock = Some(super::audio_clock::AudioClock::default());
                }
            }
        }
    }

    fn receive(&mut self) -> Result<Vec<Decoded>, Error> {
        let mut out = Vec::new();
        loop {
            let result = match &mut self.kind {
                Kind::Video { decoder, scaler } => {
                    let mut decoded = frame::Video::empty();
                    match decoder.receive_frame(&mut decoded) {
                        Ok(()) => convert_video(scaler, &decoded).map(|f| Some(Decoded::Video(f))),
                        Err(error) => Err(error),
                    }
                }
                Kind::Audio {
                    decoder,
                    converter,
                    clock,
                } => {
                    let mut decoded = frame::Audio::empty();
                    match decoder.receive_frame(&mut decoded) {
                        Ok(()) => {
                            convert_audio(converter, clock, &decoded).map(|f| f.map(Decoded::Audio))
                        }
                        Err(error) => Err(error),
                    }
                }
            };
            match result {
                Ok(Some(frame)) => out.push(frame),
                Ok(None) => {}
                Err(
                    ff::Error::Eof
                    | ff::Error::Other {
                        errno: ff::ffi::EAGAIN,
                    },
                ) => break,
                Err(ff::Error::InvalidData) => continue,
                Err(error) => return Err(error.into()),
            }
        }
        Ok(out)
    }
}

fn convert_video(
    scaler: &mut Option<(format::Pixel, u32, u32, software::scaling::Context)>,
    decoded: &frame::Video,
) -> Result<VideoFrame, ff::Error> {
    let (pixel, w, h) = (decoded.format(), decoded.width(), decoded.height());
    if scaler
        .as_ref()
        .is_none_or(|(p, sw, sh, _)| (*p, *sw, *sh) != (pixel, w, h))
    {
        let context = software::scaling::Context::get(
            pixel,
            w,
            h,
            format::Pixel::BGRA,
            w,
            h,
            software::scaling::Flags::BILINEAR,
        )?;
        *scaler = Some((pixel, w, h, context));
    }
    let mut bgra = frame::Video::empty();
    scaler.as_mut().unwrap().3.run(decoded, &mut bgra)?;
    let row = w as usize * 4;
    let mut pixels = Vec::with_capacity(row * h as usize);
    for line in bgra.data(0).chunks(bgra.stride(0)).take(h as usize) {
        pixels.extend_from_slice(&line[..row]);
    }
    Ok(VideoFrame {
        pts_us: decoded.timestamp().map_or(0, |t| (t.max(0) / 10) as u64),
        width: w,
        height: h,
        bgra: pixels,
    })
}

fn convert_audio(
    converter: &mut Option<software::resampling::Context>,
    clock: &mut Option<super::audio_clock::AudioClock>,
    decoded: &frame::Audio,
) -> Result<Option<AudioFrame>, ff::Error> {
    let rate = decoded.rate();
    let mut layout = decoded.channel_layout();
    if layout.is_empty() {
        layout = ff::ChannelLayout::default(i32::from(decoded.channels()));
    }
    let channels = layout.channels() as u16;
    if rate == 0 || channels == 0 || decoded.samples() == 0 {
        return Ok(None);
    }
    let stale = converter.as_ref().is_none_or(|c| {
        let input = c.input();
        (input.format, input.rate) != (decoded.format(), rate) || input.channel_layout != layout
    });
    if stale {
        *converter = Some(software::resampling::Context::get(
            decoded.format(),
            layout,
            rate,
            format::Sample::F32(format::sample::Type::Packed),
            layout,
            rate,
        )?);
    }
    let mut pcm = frame::Audio::empty();
    converter.as_mut().unwrap().run(decoded, &mut pcm)?;
    let pts = decoded
        .pts()
        .or(decoded.timestamp())
        .map_or(0, |t| (t.max(0) / 10) as u64);
    let pts = clock
        .as_mut()
        .map_or(pts, |clock| clock.stamp(pts, pcm.samples(), rate));
    let count = pcm.samples() * usize::from(channels);
    let samples = pcm.data(0)[..count * 4]
        .as_chunks::<4>()
        .0
        .iter()
        .map(|v| f32::from_ne_bytes(*v))
        .collect();
    Ok(Some(AudioFrame {
        pts_us: pts,
        sample_rate: rate,
        channels,
        samples,
    }))
}

#[cfg(test)]
mod tests;
