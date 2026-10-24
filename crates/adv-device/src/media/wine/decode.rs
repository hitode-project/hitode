//! Demand-driven container and sample workers for host media interfaces.
//! AVIO asks the execution owner for bytes; only the worker waits for replies.
use std::collections::VecDeque;
use std::io::{Read, Seek, SeekFrom};
use std::sync::{Arc, Mutex, mpsc};

use adv_sync::WakeEvent;
use ff::{codec, format, frame, media};
use ffmpeg_next as ff;

use super::super::native::Input;
use super::super::{CodecFormat, Error};
use super::{Command, Event, Info, Output, Track};

pub struct Worker {
    commands: mpsc::Sender<Command>,
    bytes: mpsc::Sender<Vec<u8>>,
    events: mpsc::Receiver<Event>,
}

#[derive(Clone)]
struct Reader {
    length: u64,
    position: u64,
    events: mpsc::Sender<Event>,
    bytes: Arc<Mutex<mpsc::Receiver<Vec<u8>>>>,
    wake: WakeEvent,
}
impl Read for Reader {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        let size = out
            .len()
            .min(self.length.saturating_sub(self.position) as usize);
        if size == 0 {
            return Ok(0);
        }
        self.events
            .send(Event::Read {
                offset: self.position,
                size: size as u32,
            })
            .map_err(|_| std::io::ErrorKind::BrokenPipe)?;
        self.wake.signal();
        let bytes = self
            .bytes
            .lock()
            .unwrap()
            .recv()
            .map_err(|_| std::io::ErrorKind::BrokenPipe)?;
        let size = size.min(bytes.len());
        out[..size].copy_from_slice(&bytes[..size]);
        self.position += size as u64;
        Ok(size)
    }
}
impl Seek for Reader {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        let pos = match pos {
            SeekFrom::Start(pos) => i128::from(pos),
            SeekFrom::Current(pos) => i128::from(self.position) + i128::from(pos),
            SeekFrom::End(pos) => i128::from(self.length) + i128::from(pos),
        };
        self.position = u64::try_from(pos).map_err(|_| std::io::ErrorKind::InvalidInput)?;
        Ok(self.position)
    }
}

impl Worker {
    pub fn parser(length: u64, wake: WakeEvent) -> Result<Self, Error> {
        let (commands, inbox) = mpsc::channel();
        let (events, replies) = mpsc::channel();
        let (bytes, input) = mpsc::channel();
        let reader = Reader {
            length,
            position: 0,
            events: events.clone(),
            bytes: Arc::new(Mutex::new(input)),
            wake: wake.clone(),
        };
        std::thread::Builder::new()
            .name("media-parser".into())
            .spawn(move || {
                let result = Container::open(reader);
                match result {
                    Err(error) => {
                        let _ = events.send(Event::Ready(Err(error)));
                        wake.signal();
                    }
                    Ok(mut source) => {
                        if events.send(Event::Ready(Ok(source.info.clone()))).is_err() {
                            return;
                        }
                        wake.signal();
                        while let Ok(command) = inbox.recv() {
                            let event = match command {
                                Command::Next(index, compressed) => {
                                    Event::Sample(index, source.next(index, compressed))
                                }
                                Command::Seek(time) => Event::Done(source.seek(time)),
                                _ => unreachable!(),
                            };
                            if events.send(event).is_err() {
                                break;
                            }
                            wake.signal();
                        }
                    }
                }
            })?;
        Ok(Self {
            commands,
            bytes,
            events: replies,
        })
    }

    pub fn transform(codec: CodecFormat, wake: WakeEvent) -> Result<Self, Error> {
        let (commands, inbox) = mpsc::channel();
        let (events, replies) = mpsc::channel();
        let (bytes, _) = mpsc::channel();
        std::thread::Builder::new()
            .name("media-transform".into())
            .spawn(move || {
                let mut decoder = match Decode::from_format(&codec) {
                    Ok(decoder) => decoder,
                    Err(error) => {
                        let _ = events.send(Event::Ready(Err(error)));
                        wake.signal();
                        return;
                    }
                };
                let nominal = match &codec {
                    CodecFormat::Video { frame_100ns, .. } => *frame_100ns as i64,
                    _ => 0,
                };
                let mut next_pts = 0;
                let info = Info {
                    tracks: vec![Track {
                        codec,
                        language: None,
                        name: None,
                    }],
                    duration: 0,
                    mime: String::new(),
                };
                let _ = events.send(Event::Ready(Ok(info)));
                wake.signal();
                while let Ok(command) = inbox.recv() {
                    let result = match command {
                        Command::Decode(data, pts, duration) => {
                            let mut packet = ff::Packet::copy(&data);
                            packet.set_pts(pts);
                            packet.set_duration(duration);
                            decoder.push(Some(&packet))
                        }
                        Command::Drain => decoder.push(None),
                        Command::Flush => {
                            decoder.flush();
                            Ok(Vec::new())
                        }
                        _ => unreachable!(),
                    };
                    match result {
                        Ok(frames) => {
                            for mut sample in frames {
                                sample.stamp(&mut next_pts, nominal);
                                let _ = events.send(Event::Sample(0, Ok(Some(sample))));
                            }
                        }
                        Err(error) => {
                            let _ = events.send(Event::Sample(0, Err(error)));
                        }
                    }
                    if events.send(Event::Done(Ok(()))).is_err() {
                        break;
                    }
                    wake.signal();
                }
            })?;
        Ok(Self {
            commands,
            bytes,
            events: replies,
        })
    }
    pub fn send(&self, command: Command) -> Result<(), Error> {
        self.commands
            .send(command)
            .map_err(|_| Error::Decode("media worker ended".into()))
    }
    pub fn push(&self, bytes: Vec<u8>) -> Result<(), Error> {
        self.bytes
            .send(bytes)
            .map_err(|_| Error::Decode("media input ended".into()))
    }
    pub fn poll(&self) -> Option<Event> {
        self.events.try_recv().ok()
    }
}

pub struct Sample {
    pub pts: i64,
    pub duration: i64,
    pub key: bool,
    data: Data,
}
enum Data {
    Packet(Vec<u8>),
    Video(frame::Video),
    Audio(frame::Audio),
}

fn pixel(id: u32) -> Result<format::Pixel, Error> {
    Ok(match id {
        1 => format::Pixel::BGRA,
        2 => format::Pixel::BGRZ,
        3 => format::Pixel::BGR24,
        4 => format::Pixel::RGB555LE,
        5 => format::Pixel::RGB565LE,
        6 => format::Pixel::RGBA,
        8 | 13 => format::Pixel::YUV420P,
        9 => format::Pixel::NV12,
        10 => format::Pixel::P010LE,
        11 => format::Pixel::UYVY422,
        12 => format::Pixel::YUYV422,
        14 => format::Pixel::YVYU422,
        _ => return Err(Error::Decode(format!("unsupported pixel layout {id}"))),
    })
}
fn audio(id: u32) -> Result<format::Sample, Error> {
    use format::sample::Type::Packed;
    Ok(match id {
        1 => format::Sample::U8(Packed),
        2 => format::Sample::I16(Packed),
        4 => format::Sample::I32(Packed),
        5 => format::Sample::F32(Packed),
        6 => format::Sample::F64(Packed),
        _ => return Err(Error::Decode(format!("unsupported sample layout {id}"))),
    })
}
impl Sample {
    fn stamp(&mut self, next: &mut i64, nominal: i64) {
        if self.pts == ff::ffi::AV_NOPTS_VALUE {
            self.pts = *next;
        }
        if self.duration <= 0 {
            self.duration = match &self.data {
                Data::Audio(frame) => frame.samples() as i64 * 10_000_000 / i64::from(frame.rate()),
                _ => nominal,
            };
        }
        *next = self.pts.saturating_add(self.duration);
    }
    pub fn size(&self, output: Output) -> Result<usize, Error> {
        match (&self.data, output) {
            (Data::Packet(bytes), _) => Ok(bytes.len()),
            (
                Data::Video(_),
                Output::Video {
                    pixel: id,
                    width,
                    height,
                    align,
                },
            ) => {
                let size = unsafe {
                    ff::ffi::av_image_get_buffer_size(
                        pixel(id)?.into(),
                        width as i32,
                        height.unsigned_abs() as i32,
                        align.max(4) as i32,
                    )
                };
                if size < 0 {
                    Err(ff::Error::from(size).into())
                } else {
                    Ok(size as usize)
                }
            }
            (
                Data::Audio(frame),
                Output::Audio {
                    sample,
                    channels,
                    rate,
                },
            ) => Ok(
                (frame.samples() as u64 * u64::from(rate)).div_ceil(u64::from(frame.rate()))
                    as usize
                    * channels as usize
                    * audio(sample)?.bytes(),
            ),
            _ => Err(Error::Decode("output does not match the sample".into())),
        }
    }
    pub fn copy(&self, output: Output, out: &mut [u8]) -> Result<usize, Error> {
        let size = self.size(output)?;
        if size > out.len() {
            return Err(Error::Decode("sample buffer too small".into()));
        }
        match (&self.data, output) {
            (Data::Packet(bytes), _) => out[..size].copy_from_slice(bytes),
            (
                Data::Video(frame),
                Output::Video {
                    pixel: id,
                    width,
                    height,
                    align,
                },
            ) => unsafe {
                let pixel = pixel(id)?;
                let mut data = [std::ptr::null_mut(); 4];
                let mut lines = [0; 4];
                ff::ffi::av_image_fill_arrays(
                    data.as_mut_ptr(),
                    lines.as_mut_ptr(),
                    out.as_mut_ptr(),
                    pixel.into(),
                    width as i32,
                    height.unsigned_abs() as i32,
                    align.max(4) as i32,
                );
                if id == 13 {
                    data.swap(1, 2);
                    lines.swap(1, 2);
                }
                if height < 0 {
                    data[0] = data[0].offset(
                        isize::try_from(i64::from(lines[0]) * i64::from(height.unsigned_abs() - 1))
                            .unwrap(),
                    );
                    lines[0] = -lines[0];
                }
                let context = ff::ffi::sws_getContext(
                    frame.width() as i32,
                    frame.height() as i32,
                    frame.format().into(),
                    width as i32,
                    height.unsigned_abs() as i32,
                    pixel.into(),
                    ff::software::scaling::Flags::BILINEAR.bits(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null(),
                );
                if context.is_null() {
                    return Err(Error::Decode("video conversion failed".into()));
                }
                let source = &*frame.as_ptr();
                let result = ff::ffi::sws_scale(
                    context,
                    source.data.as_ptr().cast(),
                    source.linesize.as_ptr(),
                    0,
                    frame.height() as i32,
                    data.as_ptr(),
                    lines.as_ptr(),
                );
                ff::ffi::sws_freeContext(context);
                if result < 0 {
                    return Err(ff::Error::from(result).into());
                }
            },
            (
                Data::Audio(frame),
                Output::Audio {
                    sample,
                    channels,
                    rate,
                },
            ) => {
                let mut layout = frame.channel_layout();
                if layout.is_empty() {
                    layout = ff::ChannelLayout::default(i32::from(frame.channels()));
                }
                let mut context = ff::software::resampling::Context::get(
                    frame.format(),
                    layout,
                    frame.rate(),
                    audio(sample)?,
                    ff::ChannelLayout::default(channels as i32),
                    rate,
                )?;
                let mut data = [out.as_mut_ptr()];
                let result = unsafe {
                    ff::ffi::swr_convert(
                        context.as_mut_ptr(),
                        data.as_mut_ptr(),
                        (size / (channels as usize * audio(sample)?.bytes())) as i32,
                        (*frame.as_ptr()).extended_data.cast(),
                        frame.samples() as i32,
                    )
                };
                if result < 0 {
                    return Err(ff::Error::from(result).into());
                }
                return Ok(result as usize * channels as usize * audio(sample)?.bytes());
            }
            _ => unreachable!(),
        }
        Ok(size)
    }
}

enum Decode {
    Video(ff::decoder::Video),
    Audio(ff::decoder::Audio),
}
impl Decode {
    fn open(parameters: codec::Parameters, tb: ff::Rational) -> Result<Self, Error> {
        let medium = parameters.medium();
        let mut context = codec::Context::from_parameters(parameters)?;
        context.set_threading(codec::threading::Config::count(1));
        #[cfg(target_vendor = "apple")]
        if medium == media::Type::Video {
            hardware(&mut context);
        }
        let mut decoder = context.decoder();
        decoder.set_packet_time_base(tb);
        match medium {
            media::Type::Video => Ok(Self::Video(decoder.video()?)),
            media::Type::Audio => Ok(Self::Audio(decoder.audio()?)),
            _ => Err(ff::Error::StreamNotFound.into()),
        }
    }
    fn from_format(format: &CodecFormat) -> Result<Self, Error> {
        let mut parameters = codec::Parameters::new();
        unsafe {
            let p = &mut *parameters.as_mut_ptr();
            let extra = match format {
                CodecFormat::Video {
                    fourcc,
                    width,
                    height,
                    bit_count,
                    extradata,
                    ..
                } => {
                    let tables = [ff::ffi::avformat_get_riff_video_tags(), std::ptr::null()];
                    p.codec_id = ff::ffi::av_codec_get_id(tables.as_ptr(), *fourcc);
                    if matches!(*fourcc, 0x31435641 | 0x31637661) {
                        p.codec_id = ff::ffi::AVCodecID::AV_CODEC_ID_H264;
                    }
                    if matches!(*fourcc, 0x3147504d | 0xe436eb81) {
                        p.codec_id = ff::ffi::AVCodecID::AV_CODEC_ID_MPEG1VIDEO;
                    }
                    let raw_pixel = match *fourcc {
                        20 => Some(format::Pixel::BGR24),
                        21 => Some(format::Pixel::BGRA),
                        22 => Some(format::Pixel::BGRZ),
                        23 => Some(format::Pixel::RGB565LE),
                        24 => Some(format::Pixel::RGB555LE),
                        0x3231564e => Some(format::Pixel::NV12),
                        0x30323449 | 0x32315659 => Some(format::Pixel::YUV420P),
                        0x32595559 => Some(format::Pixel::YUYV422),
                        0x59565955 => Some(format::Pixel::UYVY422),
                        _ => None,
                    };
                    if let Some(pixel) = raw_pixel {
                        p.codec_id = ff::ffi::AVCodecID::AV_CODEC_ID_RAWVIDEO;
                        p.format = ff::ffi::AVPixelFormat::from(pixel) as i32;
                    }
                    p.codec_type = ff::ffi::AVMediaType::AVMEDIA_TYPE_VIDEO;
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
                    let tables = [ff::ffi::avformat_get_riff_audio_tags(), std::ptr::null()];
                    p.codec_id = ff::ffi::av_codec_get_id(tables.as_ptr(), u32::from(*format_tag));
                    if *format_tag == 0x1610 {
                        p.codec_id = ff::ffi::AVCodecID::AV_CODEC_ID_AAC;
                    }
                    if *format_tag == 1 {
                        p.codec_id = match *bits {
                            8 => ff::ffi::AVCodecID::AV_CODEC_ID_PCM_U8,
                            24 => ff::ffi::AVCodecID::AV_CODEC_ID_PCM_S24LE,
                            32 => ff::ffi::AVCodecID::AV_CODEC_ID_PCM_S32LE,
                            _ => ff::ffi::AVCodecID::AV_CODEC_ID_PCM_S16LE,
                        };
                    } else if *format_tag == 3 {
                        p.codec_id = if *bits == 64 {
                            ff::ffi::AVCodecID::AV_CODEC_ID_PCM_F64LE
                        } else {
                            ff::ffi::AVCodecID::AV_CODEC_ID_PCM_F32LE
                        };
                    }
                    p.codec_type = ff::ffi::AVMediaType::AVMEDIA_TYPE_AUDIO;
                    p.sample_rate = *rate as i32;
                    p.block_align = i32::from(*block_align);
                    p.bits_per_coded_sample = i32::from(*bits);
                    p.bit_rate = i64::from(*bytes_per_second) * 8;
                    ff::ffi::av_channel_layout_default(&mut p.ch_layout, i32::from(*channels));
                    extradata
                }
            };
            if !extra.is_empty() {
                p.extradata = ff::ffi::av_mallocz(extra.len() + 64).cast();
                if p.extradata.is_null() {
                    return Err(ff::Error::Bug.into());
                }
                std::ptr::copy_nonoverlapping(extra.as_ptr(), p.extradata, extra.len());
                p.extradata_size = extra.len() as i32;
            }
        }
        Self::open(parameters, ff::Rational(1, 10_000_000))
    }
    fn push(&mut self, packet: Option<&ff::Packet>) -> Result<Vec<Sample>, Error> {
        match self {
            Self::Video(decoder) => match packet {
                Some(p) => decoder.send_packet(p)?,
                None => decoder.send_eof()?,
            },
            Self::Audio(decoder) => match packet {
                Some(p) => decoder.send_packet(p)?,
                None => decoder.send_eof()?,
            },
        }
        let mut samples = Vec::new();
        loop {
            let (data, result) = match self {
                Self::Video(decoder) => {
                    let mut frame = frame::Video::empty();
                    let result = decoder.receive_frame(&mut frame);
                    (Data::Video(frame), result)
                }
                Self::Audio(decoder) => {
                    let mut frame = frame::Audio::empty();
                    let result = decoder.receive_frame(&mut frame);
                    (Data::Audio(frame), result)
                }
            };
            match result {
                Ok(()) => {
                    let raw = match &data {
                        Data::Video(f) => unsafe { &*f.as_ptr() },
                        Data::Audio(f) => unsafe { &*f.as_ptr() },
                        _ => unreachable!(),
                    };
                    let mut sample = Sample {
                        pts: raw.best_effort_timestamp,
                        duration: raw.duration,
                        key: raw.flags & ff::ffi::AV_FRAME_FLAG_KEY != 0,
                        data,
                    };
                    if let Data::Video(frame) = &sample.data
                        && frame.format() == format::Pixel::VIDEOTOOLBOX
                    {
                        let mut software = frame::Video::empty();
                        let result = unsafe {
                            ff::ffi::av_hwframe_transfer_data(
                                software.as_mut_ptr(),
                                frame.as_ptr(),
                                0,
                            )
                        };
                        if result < 0 {
                            return Err(ff::Error::from(result).into());
                        }
                        sample.data = Data::Video(software);
                    }
                    samples.push(sample);
                }
                Err(
                    ff::Error::Eof
                    | ff::Error::Other {
                        errno: ff::ffi::EAGAIN,
                    },
                ) => break,
                Err(error) => return Err(error.into()),
            }
        }
        Ok(samples)
    }
    fn flush(&mut self) {
        match self {
            Self::Video(d) => d.flush(),
            Self::Audio(d) => d.flush(),
        }
    }
}

#[cfg(target_vendor = "apple")]
fn hardware(context: &mut codec::Context) {
    unsafe extern "C" fn select(
        _context: *mut ff::ffi::AVCodecContext,
        mut formats: *const ff::ffi::AVPixelFormat,
    ) -> ff::ffi::AVPixelFormat {
        unsafe {
            let first = *formats;
            while *formats != ff::ffi::AVPixelFormat::AV_PIX_FMT_NONE {
                if *formats == ff::ffi::AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX {
                    return *formats;
                }
                formats = formats.add(1);
            }
            first
        }
    }
    unsafe {
        let raw = context.as_mut_ptr();
        let decoder = ff::ffi::avcodec_find_decoder((*raw).codec_id);
        if decoder.is_null() {
            return;
        }
        for index in 0.. {
            let config = ff::ffi::avcodec_get_hw_config(decoder, index);
            if config.is_null() {
                break;
            }
            if (*config).device_type != ff::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VIDEOTOOLBOX {
                continue;
            }
            let mut device = std::ptr::null_mut();
            if ff::ffi::av_hwdevice_ctx_create(
                &mut device,
                (*config).device_type,
                std::ptr::null(),
                std::ptr::null_mut(),
                0,
            ) >= 0
            {
                (*raw).hw_device_ctx = device;
                (*raw).get_format = Some(select);
            }
            break;
        }
    }
}

struct Stream {
    input: Input,
    decoder: Option<Decode>,
    queue: VecDeque<Sample>,
    eof: bool,
    next_pts: i64,
}
struct Container {
    reader: Reader,
    info: Info,
    streams: Vec<Option<Stream>>,
    indices: Vec<usize>,
    time: u64,
}
fn time(value: i64, tb: ff::Rational) -> i64 {
    if value == ff::ffi::AV_NOPTS_VALUE {
        return value;
    }
    (i128::from(value) * i128::from(tb.numerator()) * 10_000_000 / i128::from(tb.denominator()))
        as i64
}
impl Container {
    fn open(reader: Reader) -> Result<Self, Error> {
        ff::init()?;
        let input = Input::open(Box::new(reader.clone()))?;
        let mut tracks = Vec::new();
        let mut indices = Vec::new();
        for stream in input.context.streams() {
            let params = stream.parameters();
            let p = unsafe { &*params.as_ptr() };
            let extra = if p.extradata_size > 0 {
                unsafe {
                    std::slice::from_raw_parts(p.extradata, p.extradata_size as usize).to_vec()
                }
            } else {
                Vec::new()
            };
            let codec = match params.medium() {
                media::Type::Video => {
                    let tables = [
                        unsafe { ff::ffi::avformat_get_riff_video_tags() },
                        std::ptr::null(),
                    ];
                    let tag = unsafe { ff::ffi::av_codec_get_tag(tables.as_ptr(), p.codec_id) };
                    let rate = stream.avg_frame_rate();
                    CodecFormat::Video {
                        fourcc: tag,
                        width: p.width as u32,
                        height: p.height as u32,
                        bit_count: p.bits_per_coded_sample as u16,
                        frame_100ns: if rate.numerator() > 0 {
                            time(
                                i64::from(rate.denominator()),
                                ff::Rational(1, rate.numerator()),
                            ) as u64
                        } else {
                            0
                        },
                        extradata: extra,
                    }
                }
                media::Type::Audio => {
                    let tables = [
                        unsafe { ff::ffi::avformat_get_riff_audio_tags() },
                        std::ptr::null(),
                    ];
                    let tag = unsafe { ff::ffi::av_codec_get_tag(tables.as_ptr(), p.codec_id) };
                    CodecFormat::Audio {
                        format_tag: tag as u16,
                        rate: p.sample_rate as u32,
                        channels: p.ch_layout.nb_channels as u16,
                        block_align: p.block_align as u16,
                        bits: p.bits_per_coded_sample as u16,
                        bytes_per_second: (p.bit_rate / 8) as u32,
                        extradata: extra,
                    }
                }
                _ => continue,
            };
            tracks.push(Track {
                codec,
                language: stream.metadata().get("language").map(str::to_owned),
                name: stream.metadata().get("title").map(str::to_owned),
            });
            indices.push(stream.index());
        }
        let mime = match input.context.format().name() {
            "asf" => "video/x-ms-asf",
            "avi" => "video/avi",
            "mpeg" => "video/mpeg",
            _ => "video/mp4",
        }
        .into();
        let duration = input.context.duration().max(0) as u64 * 10;
        let streams = (0..tracks.len()).map(|_| None).collect();
        Ok(Self {
            reader,
            info: Info {
                tracks,
                duration,
                mime,
            },
            streams,
            indices,
            time: 0,
        })
    }
    fn next(&mut self, index: usize, compressed: bool) -> Result<Option<Sample>, Error> {
        let source_index = self.indices[index];
        if self.streams[index].is_none() {
            let mut input = Input::open(Box::new(self.reader.clone()))?;
            let source = input
                .context
                .stream(source_index)
                .ok_or(ff::Error::StreamNotFound)?;
            let decoder = if compressed {
                None
            } else {
                Some(Decode::open(
                    source.parameters().clone(),
                    ff::Rational(1, 10_000_000),
                )?)
            };
            if self.time != 0 {
                input.context.seek(self.time as i64 / 10, ..)?;
            }
            self.streams[index] = Some(Stream {
                input,
                decoder,
                queue: VecDeque::new(),
                eof: false,
                next_pts: self.time as i64,
            });
        }
        let stream = self.streams[index].as_mut().unwrap();
        loop {
            if let Some(mut sample) = stream.queue.pop_front() {
                let nominal = match &self.info.tracks[index].codec {
                    CodecFormat::Video { frame_100ns, .. } => *frame_100ns as i64,
                    _ => 0,
                };
                sample.stamp(&mut stream.next_pts, nominal);
                return Ok(Some(sample));
            }
            if stream.eof {
                return Ok(None);
            }
            let mut packet = ff::Packet::empty();
            match packet.read(&mut stream.input.context) {
                Ok(()) => {
                    if packet.stream() != source_index {
                        continue;
                    }
                    let source = stream.input.context.stream(source_index).unwrap();
                    packet.rescale_ts(source.time_base(), ff::Rational(1, 10_000_000));
                    // Container start timestamps are offsets, not presentation delays.
                    let origin = unsafe { (*stream.input.context.as_ptr()).start_time };
                    let origin = if origin == ff::ffi::AV_NOPTS_VALUE {
                        0
                    } else {
                        origin * 10
                    };
                    packet.set_pts(packet.pts().map(|pts| pts - origin));
                    packet.set_dts(packet.dts().map(|pts| pts - origin));
                    if let Some(decoder) = &mut stream.decoder {
                        stream.queue.extend(decoder.push(Some(&packet))?);
                    } else {
                        stream.queue.push_back(Sample {
                            pts: packet.pts().unwrap_or(ff::ffi::AV_NOPTS_VALUE),
                            duration: packet.duration(),
                            key: packet.is_key(),
                            data: Data::Packet(packet.data().unwrap_or_default().to_vec()),
                        });
                    }
                }
                Err(ff::Error::Eof) => {
                    stream.eof = true;
                    if let Some(decoder) = &mut stream.decoder {
                        stream.queue.extend(decoder.push(None)?);
                    }
                }
                Err(error) => return Err(error.into()),
            }
        }
    }
    fn seek(&mut self, time: u64) -> Result<(), Error> {
        self.time = time;
        // Reopening also changes between compressed and decoded negotiation.
        self.streams.iter_mut().for_each(|stream| *stream = None);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn receive(worker: &Worker, wake: &WakeEvent, data: &[u8]) -> Event {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            wake.take_interrupt();
            while let Some(event) = worker.poll() {
                if let Event::Read { offset, size } = event {
                    let start = (offset as usize).min(data.len());
                    let end = (start + size as usize).min(data.len());
                    worker.push(data[start..end].to_vec()).unwrap();
                } else {
                    return event;
                }
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(!remaining.is_zero(), "media worker did not answer");
            wake.wait(Some(remaining)).unwrap();
        }
    }

    // spec: WINE-MEDIA-3, WINE-MEDIA-6
    #[test]
    fn container_workers_decode_only_requested_streams() {
        let clips: [&[u8]; 4] = [
            include_bytes!("../../../../adv-runtime/tests/fixtures/media/synthetic.mpg"),
            include_bytes!("../../../../adv-runtime/tests/fixtures/media/synthetic.wmv"),
            include_bytes!("../../../../adv-runtime/tests/fixtures/media/synthetic.mp4"),
            include_bytes!("../../../../adv-runtime/tests/fixtures/media/synthetic.avi"),
        ];
        for clip in clips {
            let wake = WakeEvent::new().unwrap();
            let worker = Worker::parser(clip.len() as u64, wake.clone()).unwrap();
            let info = match receive(&worker, &wake, clip) {
                Event::Ready(info) => info.unwrap(),
                _ => panic!("expected metadata"),
            };
            assert_eq!(info.tracks.len(), 2);
            let video = info
                .tracks
                .iter()
                .position(|track| matches!(track.codec, CodecFormat::Video { .. }))
                .unwrap();
            assert!(worker.poll().is_none());
            let mut frames = 0;
            let mut last = -1;
            loop {
                worker.send(Command::Next(video, false)).unwrap();
                let sample = match receive(&worker, &wake, clip) {
                    Event::Sample(index, sample) => {
                        assert_eq!(index, video);
                        sample.unwrap()
                    }
                    _ => panic!("expected sample"),
                };
                let Some(sample) = sample else {
                    break;
                };
                assert!(sample.pts >= last);
                last = sample.pts;
                let output = Output::Video {
                    pixel: 9,
                    width: 64,
                    height: 48,
                    align: 4,
                };
                let mut pixels = vec![0; sample.size(output).unwrap()];
                sample.copy(output, &mut pixels).unwrap();
                assert_eq!(pixels.len(), 4608);
                assert!((75..=90).contains(&pixels[0]), "red luma: {}", pixels[0]);
                frames += 1;
                assert!(worker.poll().is_none());
            }
            assert_eq!(frames, 5);
        }
    }
}
