//! Pure Ogg/Vorbis parsing and packet decoding. The caller owns I/O and scheduling.
//! No guest addresses, callback pointers, Windows structures, handles or API flags.
pub use ogg::Packet;
use ogg::reading::{BasePacketReader, PageParser};
use symphonia::core::audio::{AudioBufferRef, Signal};
use symphonia::core::codecs::{CODEC_TYPE_VORBIS, CodecParameters, Decoder, DecoderOptions};
use symphonia::core::formats::Packet as AudioPacket;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    NotVorbis,
    BadHeader,
    BadLink,
    UnexpectedSampleFormat,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ByteOrder {
    Little,
    Big,
}
#[derive(Clone, Copy, Debug)]
pub struct PcmFormat {
    pub bytes_per_sample: u8,
    pub byte_order: ByteOrder,
    pub signed: bool,
}
#[derive(Debug, PartialEq, Eq)]
pub struct Comments {
    pub vendor: Vec<u8>,
    pub values: Vec<Vec<u8>>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Format {
    pub channels: usize,
    pub sample_rate: u32,
    pub serial: u32,
}
pub enum Next {
    Packet(Packet),
    More,
    Eof,
    Error(DecodeError),
}

pub struct Stream {
    raw: Vec<u8>,
    raw_at: usize,
    source_pos: u64,
    data_offset: u64,
    eof: bool,
    reader: BasePacketReader,
    headers: Vec<Vec<u8>>,
    serial: u32,
    channels: usize,
    rate: u32,
    decoder: Option<Box<dyn Decoder>>,
    pcm: Vec<f32>,
    pcm_at: usize,
    decoded_frames: u64,
    pcm_offset: u64,
    total: Option<u64>,
    end: Option<u64>,
}
impl Stream {
    pub fn new(initial: Vec<u8>) -> Self {
        Self {
            source_pos: initial.len() as u64,
            raw: initial,
            raw_at: 0,
            data_offset: 0,
            eof: false,
            reader: BasePacketReader::new(),
            headers: Vec::new(),
            serial: 0,
            channels: 0,
            rate: 0,
            decoder: None,
            pcm: Vec::new(),
            pcm_at: 0,
            decoded_frames: 0,
            pcm_offset: 0,
            total: None,
            end: None,
        }
    }
    pub fn format(&self) -> Format {
        Format {
            channels: self.channels,
            sample_rate: self.rate,
            serial: self.serial,
        }
    }
    pub fn header_count(&self) -> usize {
        self.headers.len()
    }
    pub fn identification_header(&self) -> Option<&[u8]> {
        self.headers.first().map(Vec::as_slice)
    }
    pub fn raw_position(&self) -> u64 {
        self.source_pos
            .saturating_sub((self.raw.len() - self.raw_at) as u64)
    }
    pub fn data_position(&self) -> u64 {
        self.data_offset
    }
    pub fn pcm_position(&self) -> u64 {
        self.pcm_offset
    }
    pub fn decoded_frames(&self) -> u64 {
        self.decoded_frames
    }
    pub fn total_frames(&self) -> Option<u64> {
        self.total
    }
    pub fn source_length(&self) -> Option<u64> {
        self.end
    }
    pub fn set_source_position(&mut self, position: u64) {
        self.source_pos = position;
    }
    pub fn set_totals(&mut self, frames: u64, bytes: u64) {
        self.total = Some(frames);
        self.end = Some(bytes);
    }
    pub fn has_pcm(&self) -> bool {
        self.pcm_at < self.pcm.len()
    }
    pub fn next_packet(&mut self) -> Next {
        next_packet(self)
    }
    pub fn accept_header(&mut self, packet: Packet) -> Result<(), DecodeError> {
        header(self, packet)
    }
    pub fn validate_header(&self, packet: &Packet) -> Result<(), DecodeError> {
        let expected = *[1, 3, 5]
            .get(self.headers.len())
            .ok_or(DecodeError::BadHeader)?;
        if packet.data.len() < 7 || packet.data[0] != expected || &packet.data[1..7] != b"vorbis" {
            return Err(DecodeError::BadHeader);
        }
        if self.headers.is_empty() {
            if packet.data.len() != 30
                || packet.data[7..11] != [0; 4]
                || packet.data[11] == 0
                || packet.data[11] > 8
                || packet.data[12..16] == [0; 4]
            {
                return Err(DecodeError::BadHeader);
            }
        } else if packet.stream_serial() != self.serial {
            return Err(DecodeError::BadHeader);
        }
        Ok(())
    }
    pub fn decode_packet(&mut self, packet: Packet) -> Result<(), DecodeError> {
        decode(self, packet)
    }
    pub fn compact_input(&mut self) {
        if self.raw_at != 0 {
            self.raw.drain(..self.raw_at);
            self.raw_at = 0;
        }
    }
    pub fn feed(&mut self, bytes: Vec<u8>) {
        self.eof = bytes.is_empty();
        self.source_pos += bytes.len() as u64;
        self.raw.extend(bytes);
    }
    pub fn peek_pcm(&self, maximum_bytes: usize, format: PcmFormat) -> Vec<u8> {
        assert!(matches!(format.bytes_per_sample, 1 | 2));
        if self.channels == 0 {
            return Vec::new();
        }
        let samples = (maximum_bytes / usize::from(format.bytes_per_sample) / self.channels
            * self.channels)
            .min(self.pcm.len() - self.pcm_at);
        pcm_bytes(&self.pcm[self.pcm_at..self.pcm_at + samples], format)
    }
    pub fn consume_samples(&mut self, count: usize) {
        if count == 0 {
            return;
        }
        assert!(count <= self.pcm.len() - self.pcm_at && count % self.channels == 0);
        self.pcm_at += count;
        self.pcm_offset += (count / self.channels) as u64;
    }
    pub fn skip_to(&mut self, target: u64) -> bool {
        if self.channels == 0 {
            return target == self.pcm_offset;
        }
        let frames = ((self.pcm.len() - self.pcm_at) / self.channels) as u64;
        let skip = target.saturating_sub(self.pcm_offset).min(frames);
        self.consume_samples(skip as usize * self.channels);
        self.pcm_offset == target
    }
    pub fn reset(&mut self) {
        self.raw.clear();
        self.raw_at = 0;
        self.source_pos = 0;
        self.eof = false;
        self.reader = BasePacketReader::new();
        self.decoder = None;
        self.pcm.clear();
        self.pcm_at = 0;
        self.decoded_frames = 0;
        self.pcm_offset = 0;
        self.headers.clear();
    }
}

pub fn pcm_bytes(samples: &[f32], format: PcmFormat) -> Vec<u8> {
    assert!(matches!(format.bytes_per_sample, 1 | 2));
    let mut bytes = Vec::with_capacity(samples.len() * usize::from(format.bytes_per_sample));
    for &sample in samples {
        if format.bytes_per_sample == 1 {
            let value = (sample * 128.0).round().clamp(-128.0, 127.0) as i32;
            bytes.push((value + if format.signed { 0 } else { 128 }) as u8);
        } else {
            let signed = (sample * 32768.0).round().clamp(-32768.0, 32767.0) as i32;
            let value = (signed + if format.signed { 0 } else { 32768 }) as u16;
            bytes.extend_from_slice(&match format.byte_order {
                ByteOrder::Little => value.to_le_bytes(),
                ByteOrder::Big => value.to_be_bytes(),
            });
        }
    }
    bytes
}

fn next_packet(s: &mut Stream) -> Next {
    loop {
        if let Some(packet) = s.reader.read_packet() {
            return Next::Packet(packet);
        }
        let bytes = &s.raw[s.raw_at..];
        if bytes.len() < 27 {
            return if s.eof && bytes.is_empty() {
                Next::Eof
            } else if s.eof {
                Next::Error(DecodeError::BadLink)
            } else {
                Next::More
            };
        }
        let Ok((mut parser, segments)) = PageParser::new(bytes[..27].try_into().unwrap()) else {
            return Next::Error(if s.headers.is_empty() {
                DecodeError::NotVorbis
            } else {
                DecodeError::BadLink
            });
        };
        if bytes.len() < 27 + segments {
            return if s.eof {
                Next::Error(DecodeError::BadLink)
            } else {
                Next::More
            };
        }
        let body = parser.parse_segments(bytes[27..27 + segments].to_vec());
        let page_length = 27 + segments + body;
        if bytes.len() < page_length {
            return if s.eof {
                Next::Error(DecodeError::BadLink)
            } else {
                Next::More
            };
        }
        let Ok(page) = parser.parse_packet_data(bytes[27 + segments..page_length].to_vec()) else {
            return Next::Error(DecodeError::BadLink);
        };
        if s.reader.push_page(page).is_err() {
            return Next::Error(DecodeError::BadLink);
        }
        s.raw_at += page_length;
    }
}

fn header(s: &mut Stream, p: Packet) -> Result<(), DecodeError> {
    s.validate_header(&p)?;
    let expected = [1, 3, 5][s.headers.len()];
    if s.headers.is_empty() {
        s.serial = p.stream_serial();
        s.channels = usize::from(p.data[11]);
        s.rate = u32::from_le_bytes(p.data[12..16].try_into().unwrap());
    }
    if expected == 3 {
        parse_comments(&p.data)?;
    }
    s.headers.push(p.data);
    if s.headers.len() == 3 {
        s.data_offset = s.source_pos.saturating_sub((s.raw.len() - s.raw_at) as u64);
        let mut params = CodecParameters::new();
        params.for_codec(CODEC_TYPE_VORBIS).with_extra_data(
            [s.headers[0].as_slice(), s.headers[2].as_slice()]
                .concat()
                .into_boxed_slice(),
        );
        // Validate codec setup at open, but do not decode any audio packets.
        s.decoder = Some(
            symphonia::default::get_codecs()
                .make(&params, &DecoderOptions::default())
                .map_err(|_| DecodeError::BadHeader)?,
        );
    }
    Ok(())
}

fn decode(s: &mut Stream, p: Packet) -> Result<(), DecodeError> {
    if s.headers.len() != 3 {
        return Err(DecodeError::BadHeader);
    }
    if p.stream_serial() != s.serial {
        return Err(DecodeError::BadLink);
    }
    if s.decoder.is_none() {
        let mut params = CodecParameters::new();
        params.for_codec(CODEC_TYPE_VORBIS).with_extra_data(
            [s.headers[0].as_slice(), s.headers[2].as_slice()]
                .concat()
                .into_boxed_slice(),
        );
        s.decoder = Some(
            symphonia::default::get_codecs()
                .make(&params, &DecoderOptions::default())
                .map_err(|_| DecodeError::BadHeader)?,
        );
    }
    let eos = p.last_in_stream();
    let granule = p.absgp_page();
    let packet =
        AudioPacket::new_from_boxed_slice(0, s.decoded_frames, 0, p.data.into_boxed_slice());
    let decoded = s
        .decoder
        .as_mut()
        .unwrap()
        .decode(&packet)
        .map_err(|_| DecodeError::BadLink)?;
    let AudioBufferRef::F32(decoded) = decoded else {
        return Err(DecodeError::UnexpectedSampleFormat);
    };
    let frames = if eos && granule != u64::MAX {
        decoded
            .frames()
            .min(granule.saturating_sub(s.decoded_frames) as usize)
    } else {
        decoded.frames()
    };
    s.pcm.clear();
    s.pcm_at = 0;
    for frame in 0..frames {
        for channel in 0..s.channels {
            let mapped = match s.channels {
                3 => [0, 2, 1][channel],
                5 => [0, 2, 1, 3, 4][channel],
                6 => [0, 2, 1, 4, 5, 3][channel],
                7 => [0, 2, 1, 5, 6, 4, 3][channel],
                8 => [0, 2, 1, 6, 7, 4, 5, 3][channel],
                _ => channel,
            };
            s.pcm.push(decoded.chan(mapped)[frame]);
        }
    }
    s.decoded_frames += frames as u64;
    if eos && granule != u64::MAX {
        s.total = Some(granule);
    }
    Ok(())
}

pub fn parse_comments(data: &[u8]) -> Result<Comments, DecodeError> {
    if !data.starts_with(b"\x03vorbis") {
        return Err(DecodeError::BadHeader);
    }
    let mut at = 7usize;
    let read_length = |at: &mut usize| -> Result<usize, DecodeError> {
        let end = at.checked_add(4).ok_or(DecodeError::BadHeader)?;
        let value = u32::from_le_bytes(
            data.get(*at..end)
                .ok_or(DecodeError::BadHeader)?
                .try_into()
                .unwrap(),
        ) as usize;
        *at = end;
        Ok(value)
    };
    let vendor_len = read_length(&mut at)?;
    let vendor = data
        .get(at..at.checked_add(vendor_len).ok_or(DecodeError::BadHeader)?)
        .ok_or(DecodeError::BadHeader)?;
    at += vendor_len;
    let count = read_length(&mut at)?;
    if count > data.len() / 4 {
        return Err(DecodeError::BadHeader);
    }
    let mut comments = Vec::with_capacity(count);
    for _ in 0..count {
        let len = read_length(&mut at)?;
        comments.push(
            data.get(at..at.checked_add(len).ok_or(DecodeError::BadHeader)?)
                .ok_or(DecodeError::BadHeader)?,
        );
        at += len;
    }
    if data.get(at).is_none_or(|b| b & 1 == 0) {
        return Err(DecodeError::BadHeader);
    }
    Ok(Comments {
        vendor: vendor.to_vec(),
        values: comments.into_iter().map(<[u8]>::to_vec).collect(),
    })
}

pub fn tail_total(bytes: &[u8], serial: u32) -> Option<u64> {
    let mut at = 0usize;
    let mut total = None;
    while at + 27 <= bytes.len() {
        if &bytes[at..at + 4] != b"OggS" {
            at += 1;
            continue;
        }
        let header: &[u8] = &bytes[at..at + 27];
        let Ok((mut parser, n)) = PageParser::new(header.try_into().unwrap()) else {
            at += 1;
            continue;
        };
        if at + 27 + n > bytes.len() {
            break;
        }
        let body = parser.parse_segments(bytes[at + 27..at + 27 + n].to_vec());
        if at + 27 + n + body > bytes.len() {
            break;
        }
        if parser
            .parse_packet_data(bytes[at + 27 + n..at + 27 + n + body].to_vec())
            .is_ok()
            && header[5] & 4 != 0
            && u32::from_le_bytes(header[14..18].try_into().unwrap()) == serial
        {
            total = Some(u64::from_le_bytes(header[6..14].try_into().unwrap()));
        }
        at += 27 + n + body;
    }
    total.filter(|&frames| frames != u64::MAX)
}
