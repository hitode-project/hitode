//! AVIFile library lifetime and read-only file streams. A file opens through
//! the ordinary guest CreateFileA, is split by the common media demuxer, and
//! decodes frames for AVIStreamGetFrame through the common packet decoder.
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};

use adv_abi::{AnsiStr, GuestPtr, Registry, win32};
use adv_device::media::{self, CodecFormat, Decoded, Packet, PacketDecoder};
use adv_machine::{Env, Outcome};
use adv_os::Heaps;
use adv_os::vfs::{FileObj, FileStream};

const STREAM_VIDEO: u32 = u32::from_le_bytes(*b"vids");
const STREAM_AUDIO: u32 = u32::from_le_bytes(*b"auds");
const AVIERR_UNSUPPORTED: u32 = 0x8004_4065;
const AVIERR_BADFORMAT: u32 = 0x8004_4066;
const AVIERR_BADPARAM: u32 = 0x8004_406a;
const AVIERR_FILEOPEN: u32 = 0x8004_406f;
const AVIERR_NODATA: u32 = 0x8004_4073;
const AVIERR_BUFFERTOOSMALL: u32 = 0x8004_4074;
const STREAM_INFO_BYTES: usize = 140;

#[derive(Default)]
struct Library {
    references: u32,
    files: HashMap<u32, File>,
    streams: HashMap<u32, Stream>,
    frames: HashMap<u32, Frames>,
}
impl Library {
    fn initialize(&mut self) {
        self.references = self.references.saturating_add(1);
    }
    fn release(&mut self) {
        self.references = self.references.saturating_sub(1);
    }
}

struct File {
    references: u32,
    streams: Vec<u32>,
}

struct Stream {
    references: u32,
    format: CodecFormat,
    max_packet: u32,
    data: Data,
}

enum Data {
    /// Packet per frame; dropped frames have none.
    Video {
        packets: Vec<Packet>,
        frames: Vec<Option<usize>>,
    },
    /// The whole stream, as AVI audio is addressed in block-aligned bytes.
    Audio(Vec<u8>),
}

impl Stream {
    fn new(stream: media::PacketStream) -> Self {
        let data = match stream.format {
            CodecFormat::Video { frame_100ns, .. } => {
                let mut frames = Vec::new();
                for (i, packet) in stream.packets.iter().enumerate() {
                    let at = if frame_100ns == 0 {
                        i
                    } else {
                        ((packet.time_100ns + frame_100ns / 2) / frame_100ns) as usize
                    };
                    if frames.len() <= at {
                        frames.resize(at + 1, None);
                    }
                    frames[at] = Some(i);
                }
                Data::Video {
                    packets: stream.packets,
                    frames,
                }
            }
            CodecFormat::Audio { .. } => {
                Data::Audio(stream.packets.into_iter().flat_map(|p| p.data).collect())
            }
        };
        Self {
            references: 1,
            format: stream.format,
            max_packet: stream.max_packet,
            data,
        }
    }
    /// AVISTREAMINFO's sample rate as dwRate / dwScale.
    fn rate(&self) -> (u32, u32) {
        match self.format {
            CodecFormat::Video { frame_100ns, .. } => {
                let scale = frame_100ns.max(1);
                let divisor = gcd(10_000_000, scale);
                ((10_000_000 / divisor) as u32, (scale / divisor) as u32)
            }
            CodecFormat::Audio {
                block_align,
                bytes_per_second,
                ..
            } => (bytes_per_second, u32::from(block_align.max(1))),
        }
    }
    fn length(&self) -> u32 {
        match (&self.data, &self.format) {
            (Data::Video { frames, .. }, _) => frames.len() as u32,
            (Data::Audio(pcm), CodecFormat::Audio { block_align, .. }) => {
                (pcm.len() / usize::from((*block_align).max(1))) as u32
            }
            _ => 0,
        }
    }
    /// BITMAPINFOHEADER or WAVEFORMATEX followed by the codec's extra data.
    fn format_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        match &self.format {
            CodecFormat::Video {
                fourcc,
                width,
                height,
                bit_count,
                extradata,
                ..
            } => {
                let size = 40 + extradata.len() as u32;
                for value in [size, *width, *height] {
                    bytes.extend_from_slice(&value.to_le_bytes());
                }
                bytes.extend_from_slice(&1u16.to_le_bytes());
                bytes.extend_from_slice(&bit_count.to_le_bytes());
                let image = width * height * u32::from(*bit_count) / 8;
                for value in [*fourcc, image, 0, 0, 0, 0] {
                    bytes.extend_from_slice(&value.to_le_bytes());
                }
                bytes.extend_from_slice(extradata);
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
                bytes.extend_from_slice(&format_tag.to_le_bytes());
                bytes.extend_from_slice(&channels.to_le_bytes());
                bytes.extend_from_slice(&rate.to_le_bytes());
                bytes.extend_from_slice(&bytes_per_second.to_le_bytes());
                bytes.extend_from_slice(&block_align.to_le_bytes());
                bytes.extend_from_slice(&bits.to_le_bytes());
                // PCM carries the 16-byte PCMWAVEFORMAT, as AVIFile returns it.
                if *format_tag != 1 || !extradata.is_empty() {
                    bytes.extend_from_slice(&(extradata.len() as u16).to_le_bytes());
                    bytes.extend_from_slice(extradata);
                }
            }
        }
        bytes
    }
}

/// An AVIStreamGetFrame session: the decoder, and the guest DIB it fills.
struct Frames {
    stream: u32,
    decoder: PacketDecoder,
    dib: u32,
    bits: u16,
    width: u32,
    height: u32,
    /// The last frame the decoder has consumed.
    position: Option<usize>,
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 { a } else { gcd(b, a % b) }
}

fn library<'a>(env: &'a mut Env<'_>) -> &'a mut Library {
    if env.ext.try_get::<Library>().is_none() {
        env.ext.insert(Library::default());
    }
    env.ext.get_mut::<Library>()
}

fn allocate(env: &mut Env<'_>, size: u32) -> u32 {
    let heaps = env.ext.get_mut::<Heaps>();
    let heap = heaps.process_heap;
    heaps
        .alloc(env.cpu, &mut env.sys.vm, heap, size, true)
        .unwrap_or(0)
}

fn free(env: &mut Env<'_>, address: u32) {
    let heaps = env.ext.get_mut::<Heaps>();
    let heap = heaps.process_heap;
    heaps.free(env.cpu, &mut env.sys.vm, heap, address);
}

fn unavailable(env: &mut Env<'_>, api: &str, reason: &str, error: u32) -> Outcome {
    adv_abi::warn_unavailable(env, api, reason);
    Outcome::Return(error)
}

pub fn register(registry: &mut Registry) {
    for api in [
        initialize_ABI,
        release_ABI,
        open_ABI,
        file_release_ABI,
        get_stream_ABI,
        stream_release_ABI,
        info_ABI,
        read_format_ABI,
        read_ABI,
        start_ABI,
        length_ABI,
        time_to_sample_ABI,
        frame_open_ABI,
        frame_ABI,
        frame_close_ABI,
    ] {
        registry.register(api);
    }
}
#[win32(avifil32, name = "AVIFileInit", level = state)]
fn initialize(env: &mut Env<'_>) {
    library(env).initialize();
}
#[win32(avifil32, name = "AVIFileExit", level = state)]
fn release(env: &mut Env<'_>) {
    if let Some(library) = env.ext.try_get_mut::<Library>() {
        library.release();
    }
}

#[win32(avifil32, name = "AVIFileOpenA", level = implement)]
fn open(
    env: &mut Env<'_>,
    output: GuestPtr<u32>,
    path: AnsiStr,
    mode: u32,
    handler: GuestPtr<()>,
) -> Outcome {
    let (path, handler) = (path.0.addr(), handler.addr());
    const API: &str = "avifil32.dll!AVIFileOpenA";
    if output.is_null() || output.write(env.cpu, 0).is_err() || path == 0 {
        return Outcome::Return(AVIERR_BADPARAM);
    }
    // OF_WRITE, OF_READWRITE and OF_CREATE.
    if mode & 0x1003 != 0 || handler != 0 {
        return unavailable(
            env,
            API,
            "only reading with the default handler is available",
            AVIERR_UNSUPPORTED,
        );
    }
    let Ok(target) = env
        .sys
        .thunks
        .publish(env.cpu, "kernel32.dll", "CreateFileA")
    else {
        return Outcome::Return(AVIERR_FILEOPEN);
    };
    Outcome::call(
        target,
        vec![path, 0x8000_0000, 1, 0, 3, 0x80, 0],
        move |env, resume| {
            let handle = resume.value();
            if handle == u32::MAX {
                return Outcome::Return(AVIERR_FILEOPEN);
            }
            let bytes = read_all(env, handle);
            env.sys.objects.close(handle);
            let Some(bytes) = bytes else {
                return Outcome::Return(AVIERR_FILEOPEN);
            };
            let demuxed = match media::demux(bytes) {
                Ok(demuxed) => demuxed,
                Err(media::Error::Unavailable) => {
                    return unavailable(
                        env,
                        API,
                        "movie decoding is disabled (build with the media feature)",
                        AVIERR_UNSUPPORTED,
                    );
                }
                Err(error) => return unavailable(env, API, &error.to_string(), AVIERR_BADFORMAT),
            };
            let file = allocate(env, 16);
            if file == 0 {
                return Outcome::Return(0x8007_000e);
            }
            let mut streams = Vec::new();
            for stream in demuxed.streams {
                let address = allocate(env, 16);
                if address == 0 {
                    break;
                }
                library(env).streams.insert(address, Stream::new(stream));
                streams.push(address);
            }
            library(env).files.insert(
                file,
                File {
                    references: 1,
                    streams,
                },
            );
            let _ = output.write(env.cpu, file);
            Outcome::Return(0)
        },
    )
}

fn read_all(env: &mut Env<'_>, handle: u32) -> Option<Vec<u8>> {
    let file = env.sys.objects.host::<FileObj>(handle)?;
    if let Some(bytes) = file.file.memory_bytes() {
        return Some(bytes.to_vec());
    }
    let mut stream = FileStream::from(file.file.host()?.try_clone().ok()?);
    let mut bytes = Vec::new();
    stream.seek(SeekFrom::Start(0)).ok()?;
    stream.read_to_end(&mut bytes).ok()?;
    Some(bytes)
}

#[win32(avifil32, name = "AVIFileRelease", level = implement)]
fn file_release(env: &mut Env<'_>, file: GuestPtr<()>) -> Outcome {
    Outcome::Return(release_file(env, file.addr()))
}

fn release_file(env: &mut Env<'_>, file: u32) -> u32 {
    let Some(entry) = library(env).files.get_mut(&file) else {
        return 0;
    };
    entry.references -= 1;
    let remaining = entry.references;
    if remaining == 0 {
        let entry = library(env).files.remove(&file).unwrap();
        for stream in entry.streams {
            release_stream(env, stream);
        }
        free(env, file);
    }
    remaining
}

#[win32(avifil32, name = "AVIFileGetStream", level = implement)]
fn get_stream(
    env: &mut Env<'_>,
    file: GuestPtr<()>,
    output: GuestPtr<u32>,
    kind: u32,
    index: i32,
) -> Outcome {
    Outcome::Return(stream_of(env, file.addr(), output, kind, index))
}

fn stream_of(env: &mut Env<'_>, file: u32, output: GuestPtr<u32>, kind: u32, index: i32) -> u32 {
    if output.is_null() || output.write(env.cpu, 0).is_err() {
        return AVIERR_BADPARAM;
    }
    let library = library(env);
    let Some(entry) = library.files.get(&file) else {
        return AVIERR_BADPARAM;
    };
    let found = entry
        .streams
        .iter()
        .copied()
        .filter(|address| {
            let video = matches!(library.streams[address].format, CodecFormat::Video { .. });
            kind == 0 || kind == if video { STREAM_VIDEO } else { STREAM_AUDIO }
        })
        .nth(index.max(0) as usize);
    let Some(stream) = found else {
        return AVIERR_NODATA;
    };
    library.streams.get_mut(&stream).unwrap().references += 1;
    let _ = output.write(env.cpu, stream);
    0
}

#[win32(avifil32, name = "AVIStreamRelease", level = implement)]
fn stream_release(env: &mut Env<'_>, stream: GuestPtr<()>) -> Outcome {
    Outcome::Return(release_stream(env, stream.addr()))
}

fn release_stream(env: &mut Env<'_>, stream: u32) -> u32 {
    let Some(entry) = library(env).streams.get_mut(&stream) else {
        return 0;
    };
    entry.references -= 1;
    let remaining = entry.references;
    if remaining == 0 {
        library(env).streams.remove(&stream);
        free(env, stream);
    }
    remaining
}

#[win32(avifil32, name = "AVIStreamInfoA", level = implement)]
fn info(env: &mut Env<'_>, stream: GuestPtr<()>, output: GuestPtr<()>, size: i32) -> Outcome {
    Outcome::Return(stream_info(env, stream.addr(), output.addr(), size))
}

fn stream_info(env: &mut Env<'_>, stream: u32, output: u32, size: i32) -> u32 {
    let Some(entry) = library(env).streams.get(&stream) else {
        return AVIERR_BADPARAM;
    };
    let (rate, scale) = entry.rate();
    let (kind, handler, sample, frame) = match entry.format {
        CodecFormat::Video {
            fourcc,
            width,
            height,
            ..
        } => (STREAM_VIDEO, fourcc, 0, [0, 0, width as u16, height as u16]),
        CodecFormat::Audio { block_align, .. } => (STREAM_AUDIO, 0, u32::from(block_align), [0; 4]),
    };
    let mut bytes = Vec::with_capacity(STREAM_INFO_BYTES);
    for value in [
        kind,
        handler,
        0,
        0,
        0,
        scale,
        rate,
        0,
        entry.length(),
        0,
        entry.max_packet,
        u32::MAX,
        sample,
    ] {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    // rcFrame is a RECT of 16-bit shorts in AVISTREAMINFO.
    for value in frame {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes.resize(STREAM_INFO_BYTES, 0);
    let count = (size.max(0) as usize).min(STREAM_INFO_BYTES);
    if env.cpu.write(output, &bytes[..count]).is_err() {
        return AVIERR_BADPARAM;
    }
    0
}

#[win32(avifil32, name = "AVIStreamReadFormat", level = implement)]
fn read_format(
    env: &mut Env<'_>,
    stream: GuestPtr<()>,
    _position: i32,
    output: GuestPtr<()>,
    size: GuestPtr<i32>,
) -> Outcome {
    Outcome::Return(format_of(env, stream.addr(), output.addr(), size))
}

fn format_of(env: &mut Env<'_>, stream: u32, output: u32, size: GuestPtr<i32>) -> u32 {
    let Some(bytes) = library(env).streams.get(&stream).map(Stream::format_bytes) else {
        return AVIERR_BADPARAM;
    };
    let Ok(capacity) = size.read(env.cpu) else {
        return AVIERR_BADPARAM;
    };
    if output == 0 {
        let _ = size.write(env.cpu, bytes.len() as i32);
        return 0;
    }
    let count = (capacity.max(0) as usize).min(bytes.len());
    if env.cpu.write(output, &bytes[..count]).is_err() {
        return AVIERR_BADPARAM;
    }
    let _ = size.write(env.cpu, count as i32);
    if count < bytes.len() {
        AVIERR_BUFFERTOOSMALL
    } else {
        0
    }
}

#[expect(clippy::too_many_arguments, reason = "SDK AVIStreamRead signature")]
#[win32(avifil32, name = "AVIStreamRead", level = implement)]
fn read(
    env: &mut Env<'_>,
    stream: GuestPtr<()>,
    start: i32,
    samples: i32,
    output: GuestPtr<()>,
    capacity: i32,
    bytes_read: GuestPtr<i32>,
    samples_read: GuestPtr<i32>,
) -> Outcome {
    let outputs = (bytes_read, samples_read);
    Outcome::Return(read_samples(
        env,
        stream.addr(),
        start,
        samples,
        output.addr(),
        capacity,
        outputs,
    ))
}

fn read_samples(
    env: &mut Env<'_>,
    stream: u32,
    start: i32,
    samples: i32,
    output: u32,
    capacity: i32,
    (bytes_read, samples_read): (GuestPtr<i32>, GuestPtr<i32>),
) -> u32 {
    let Some(entry) = library(env).streams.get(&stream) else {
        return AVIERR_BADPARAM;
    };
    let start = start.max(0) as usize;
    let capacity = capacity.max(0) as usize;
    // Err carries the byte count a too-small buffer needed.
    let read: Result<(Vec<u8>, usize), usize> = match (&entry.data, &entry.format) {
        (Data::Video { packets, frames }, _) => {
            let data = frames
                .get(start)
                .copied()
                .flatten()
                .map_or(&[][..], |i| &packets[i].data[..]);
            if output != 0 && data.len() > capacity {
                Err(data.len())
            } else {
                Ok((data.to_vec(), usize::from(start < frames.len())))
            }
        }
        (Data::Audio(pcm), CodecFormat::Audio { block_align, .. }) => {
            let align = usize::from((*block_align).max(1));
            let available = (pcm.len() / align).saturating_sub(start);
            // AVISTREAMREAD_CONVENIENT reads as many samples as fit.
            let wanted = if samples < 0 {
                available
            } else {
                (samples as usize).min(available)
            };
            let fit = if output == 0 {
                wanted
            } else {
                wanted.min(capacity / align)
            };
            if output != 0 && fit == 0 && wanted > 0 {
                Err(align)
            } else {
                Ok((pcm[start * align..(start + fit) * align].to_vec(), fit))
            }
        }
        _ => return AVIERR_BADPARAM,
    };
    let (data, count) = match read {
        Ok(read) => read,
        Err(needed) => {
            let _ = bytes_read.write(env.cpu, needed as i32);
            return AVIERR_BUFFERTOOSMALL;
        }
    };
    if output != 0 && env.cpu.write(output, &data).is_err() {
        return AVIERR_BADPARAM;
    }
    if !bytes_read.is_null() {
        let _ = bytes_read.write(env.cpu, data.len() as i32);
    }
    if !samples_read.is_null() {
        let _ = samples_read.write(env.cpu, count as i32);
    }
    0
}

#[win32(avifil32, name = "AVIStreamStart", level = implement)]
fn start(env: &mut Env<'_>, stream: GuestPtr<()>) -> Outcome {
    let known = library(env).streams.contains_key(&stream.addr());
    Outcome::Return(if known { 0 } else { u32::MAX })
}

#[win32(avifil32, name = "AVIStreamLength", level = implement)]
fn length(env: &mut Env<'_>, stream: GuestPtr<()>) -> Outcome {
    Outcome::Return(
        library(env)
            .streams
            .get(&stream.addr())
            .map_or(u32::MAX, Stream::length),
    )
}

/// Rounds like AVIFile: down for slow streams, up for audio-like rates.
#[win32(avifil32, name = "AVIStreamTimeToSample", level = implement)]
fn time_to_sample(env: &mut Env<'_>, stream: GuestPtr<()>, time: i32) -> Outcome {
    let Some(entry) = library(env).streams.get(&stream.addr()) else {
        return Outcome::Return(u32::MAX);
    };
    let (rate, scale) = entry.rate();
    let (numerator, denominator) = (i64::from(rate) * i64::from(time), i64::from(scale) * 1000);
    let sample = if rate / scale < 1000 {
        numerator / denominator
    } else {
        (numerator + denominator - 1) / denominator
    };
    Outcome::Return(sample.clamp(0, i64::from(entry.length())) as u32)
}

#[win32(avifil32, name = "AVIStreamGetFrameOpen", level = implement)]
fn frame_open(env: &mut Env<'_>, stream: GuestPtr<()>, wanted: GuestPtr<()>) -> Outcome {
    Outcome::Return(open_frames(env, stream.addr(), wanted.addr()))
}

fn open_frames(env: &mut Env<'_>, stream: u32, wanted: u32) -> u32 {
    const API: &str = "avifil32.dll!AVIStreamGetFrameOpen";
    let Some(CodecFormat::Video { width, height, .. }) =
        library(env).streams.get(&stream).map(|s| s.format.clone())
    else {
        return 0;
    };
    // NULL and AVIGETFRAMEF_BESTDISPLAYFMT take 24-bit RGB.
    let bits = if wanted > 1 {
        let mut header = [0; 16];
        if env.cpu.read(wanted, &mut header).is_err() {
            return 0;
        }
        u16::from_le_bytes([header[14], header[15]])
    } else {
        24
    };
    if !matches!(bits, 24 | 32) {
        adv_abi::warn_unavailable(env, API, "only 24- and 32-bit RGB frames are available");
        return 0;
    }
    let format = library(env).streams[&stream].format.clone();
    let decoder = match PacketDecoder::open(&format) {
        Ok(decoder) => decoder,
        Err(error) => {
            adv_abi::warn_unavailable(env, API, &error.to_string());
            return 0;
        }
    };
    let stride = (width * u32::from(bits) / 8).div_ceil(4) * 4;
    let dib = allocate(env, 40 + stride * height);
    if dib == 0 {
        return 0;
    }
    let mut header = Vec::with_capacity(40);
    for value in [40, width, height] {
        header.extend_from_slice(&value.to_le_bytes());
    }
    header.extend_from_slice(&1u16.to_le_bytes());
    header.extend_from_slice(&bits.to_le_bytes());
    for value in [0, stride * height, 0, 0, 0, 0] {
        header.extend_from_slice(&value.to_le_bytes());
    }
    let _ = env.cpu.write(dib, &header);
    library(env).streams.get_mut(&stream).unwrap().references += 1;
    library(env).frames.insert(
        dib,
        Frames {
            stream,
            decoder,
            dib,
            bits,
            width,
            height,
            position: None,
        },
    );
    dib
}

/// Decodes from the nearest key frame, or onward from the last frame, and
/// returns the packed DIB; dropped frames keep the previous picture.
#[win32(avifil32, name = "AVIStreamGetFrame", level = implement)]
fn frame(env: &mut Env<'_>, session: GuestPtr<()>, position: i32) -> Outcome {
    Outcome::Return(decode_frame(env, session.addr(), position))
}

fn decode_frame(env: &mut Env<'_>, session: u32, position: i32) -> u32 {
    let library = library(env);
    let Some(frames) = library.frames.get_mut(&session) else {
        return 0;
    };
    let Some(Stream {
        data: Data::Video {
            packets,
            frames: table,
        },
        ..
    }) = library.streams.get(&frames.stream)
    else {
        return 0;
    };
    let Some(target) = usize::try_from(position).ok().filter(|&p| p < table.len()) else {
        return 0;
    };
    let key = (0..=target)
        .rev()
        .find(|&i| table[i].is_some_and(|p| packets[p].key))
        .unwrap_or(0);
    let from = match frames.position {
        Some(at) if at < target && at >= key => at + 1,
        Some(at) if at == target => target + 1,
        _ => {
            frames.decoder.reset();
            key
        }
    };
    let mut picture = None;
    for at in from..=target {
        if let Some(index) = table[at] {
            let packet = &packets[index];
            match frames.decoder.decode(&packet.data, Some(packet.time_100ns)) {
                Ok(decoded) => {
                    for item in decoded {
                        if let Decoded::Video(frame) = item {
                            picture = Some(frame);
                        }
                    }
                }
                Err(error) => log::warn!("AVIStreamGetFrame: frame {at}: {error}"),
            }
        }
        frames.position = Some(at);
    }
    let (dib, bits, width, height) = (frames.dib, frames.bits, frames.width, frames.height);
    if let Some(picture) = picture.filter(|p| p.width == width && p.height == height) {
        let pixel = usize::from(bits / 8);
        let stride = (width as usize * pixel).div_ceil(4) * 4;
        let mut rows = vec![0; stride * height as usize];
        for (y, source) in picture.bgra.chunks_exact(width as usize * 4).enumerate() {
            // Bottom-up DIB rows.
            let row = &mut rows[(height as usize - 1 - y) * stride..][..width as usize * pixel];
            for (out, bgra) in row.chunks_exact_mut(pixel).zip(source.chunks_exact(4)) {
                out.copy_from_slice(&bgra[..pixel]);
            }
        }
        let _ = env.cpu.write(dib + 40, &rows);
    }
    dib
}

#[win32(avifil32, name = "AVIStreamGetFrameClose", level = implement)]
fn frame_close(env: &mut Env<'_>, session: GuestPtr<()>) -> Outcome {
    let Some(frames) = library(env).frames.remove(&session.addr()) else {
        return Outcome::Return(AVIERR_BADPARAM);
    };
    free(env, frames.dib);
    release_stream(env, frames.stream);
    Outcome::Return(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet(time_100ns: u64, key: bool, data: &[u8]) -> Packet {
        Packet {
            time_100ns,
            duration_100ns: 0,
            key,
            data: data.to_vec(),
        }
    }

    // spec: MEDIA-AVI-1
    #[test]
    fn streams_index_frames_by_time_and_address_audio_in_blocks() {
        let video = Stream::new(media::PacketStream {
            number: 1,
            format: CodecFormat::Video {
                fourcc: u32::from_le_bytes(*b"cvid"),
                width: 4,
                height: 2,
                bit_count: 24,
                frame_100ns: 833_333,
                extradata: Vec::new(),
            },
            bit_rate: 0,
            max_packet: 3,
            // The third frame was dropped by the encoder.
            packets: vec![
                packet(0, true, b"a"),
                packet(833_333, false, b"bb"),
                packet(2_500_000, false, b"ccc"),
            ],
        });
        assert_eq!(video.length(), 4);
        assert_eq!(video.rate(), (10_000_000, 833_333));
        let Data::Video { frames, .. } = &video.data else {
            unreachable!()
        };
        assert_eq!(frames, &[Some(0), Some(1), None, Some(2)]);
        let format = video.format_bytes();
        assert_eq!(format.len(), 40);
        assert_eq!(&format[16..20], b"cvid");
        let audio = Stream::new(media::PacketStream {
            number: 2,
            format: CodecFormat::Audio {
                format_tag: 1,
                rate: 22_050,
                channels: 2,
                block_align: 4,
                bits: 16,
                bytes_per_second: 88_200,
                extradata: Vec::new(),
            },
            bit_rate: 0,
            max_packet: 8,
            packets: vec![packet(0, true, &[1; 8]), packet(0, true, &[2; 6])],
        });
        // Whole blocks only; the PCMWAVEFORMAT has no cbSize.
        assert_eq!(audio.length(), 3);
        assert_eq!(audio.rate(), (88_200, 4));
        assert_eq!(audio.format_bytes().len(), 16);
    }

    #[test]
    fn nested_initialization_keeps_the_library_until_the_last_exit() {
        let mut library = Library::default();
        library.initialize();
        library.initialize();
        assert_eq!(library.references, 2);
        library.release();
        assert_eq!(library.references, 1);
        library.release();
        assert_eq!(library.references, 0);
        library.initialize();
        assert_eq!(library.references, 1);
    }
    #[test]
    fn initialization_belongs_to_each_guest_session() {
        let mut first = Library::default();
        let second = Library::default();
        first.initialize();
        assert_eq!(first.references, 1);
        assert_eq!(second.references, 0);
    }
}
