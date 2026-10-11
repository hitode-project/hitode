//! Synchronous 32-bit ACM streams use Symphonia for built-in formats and guest
//! DriverProc callbacks for application-local codecs. Host Windows ACM is used
//! solely by the repository's native differential fixtures.
use std::collections::BTreeMap;

use adv_abi::{GuestPtr, Handle, Registry, win32};
use adv_cpu::backend::{Guest, GuestExt, Prot};
use adv_device::codec::{Codec, Decoder, DecoderConfig, PcmEncoding};
use adv_machine::{Conv, Env, Namespace, Objects, Outcome};

mod drivers;
mod guest;

const INVALID_HANDLE: u32 = 5;
const NO_MEMORY: u32 = 7;
const NOT_SUPPORTED: u32 = 8;
const INVALID_FLAG: u32 = 10;
const INVALID_PARAMETER: u32 = 11;
const NOT_POSSIBLE: u32 = 512;
const UNPREPARED: u32 = 514;
const PREPARED: u32 = 0x20000;
const DONE: u32 = 0x10000;
const HEADER_BYTES: usize = 84;
const MAX_CONVERT_BYTES: usize = 65536;
const MAX_STREAMS: usize = 128;
const MS_COEFFICIENTS: [(i16, i16); 7] = [
    (256, 0),
    (512, -256),
    (0, 0),
    (192, 64),
    (240, 0),
    (460, -208),
    (392, -232),
];

#[derive(Clone, Debug)]
struct Format {
    tag: u16,
    channels: u16,
    rate: u32,
    block: u16,
    bits: u16,
    frames_per_block: u16,
}
impl Format {
    fn read(cpu: &dyn Guest, address: u32) -> Result<Self, u32> {
        let mut base = [0; 16];
        if !cpu.accessible(address, 16, Prot::READ) {
            return Err(INVALID_PARAMETER);
        }
        cpu.read(address, &mut base)
            .map_err(|_| INVALID_PARAMETER)?;
        let word = |at| u16::from_le_bytes(base[at..at + 2].try_into().unwrap());
        let mut format = Self {
            tag: word(0),
            channels: word(2),
            rate: u32::from_le_bytes(base[4..8].try_into().unwrap()),
            block: word(12),
            bits: word(14),
            frames_per_block: 1,
        };
        if !matches!(format.channels, 1 | 2)
            || format.rate == 0
            || format.block == 0
            || format.rate.checked_mul(u32::from(format.block)).is_none()
        {
            return Err(NOT_POSSIBLE);
        }
        if format.tag == 1 {
            if !matches!(format.bits, 8 | 16) || format.block != format.channels * (format.bits / 8)
            {
                return Err(NOT_POSSIBLE);
            }
            return Ok(format);
        }
        if !matches!(format.tag, 2 | 0x11) || format.bits != 4 {
            return Err(NOT_POSSIBLE);
        }
        let extra_size = address.checked_add(16).ok_or(INVALID_PARAMETER)?;
        if !cpu.accessible(extra_size, 2, Prot::READ) {
            return Err(INVALID_PARAMETER);
        }
        let extra_len = cpu.read_u16(extra_size).map_err(|_| INVALID_PARAMETER)? as usize;
        if !(2..=1024).contains(&extra_len) {
            return Err(NOT_POSSIBLE);
        }
        let mut extra = vec![0; extra_len];
        let extra_address = address.checked_add(18).ok_or(INVALID_PARAMETER)?;
        if !cpu.accessible(extra_address, extra_len as u32, Prot::READ) {
            return Err(INVALID_PARAMETER);
        }
        cpu.read(extra_address, &mut extra)
            .map_err(|_| INVALID_PARAMETER)?;
        format.frames_per_block = u16::from_le_bytes(extra[..2].try_into().unwrap());
        let ch = usize::from(format.channels);
        let block = usize::from(format.block);
        let expected = if format.tag == 0x11 {
            if block < 4 * ch || (block - 4 * ch) % (4 * ch) != 0 {
                return Err(NOT_POSSIBLE);
            }
            ((block - 4 * ch) * 2 / ch) + 1
        } else {
            // Symphonia's MS decoder supports the seven standard coefficients.
            // Different predictors are rejected instead of silently substituted.
            if block < 7 * ch
                || extra_len < 32
                || u16::from_le_bytes(extra[2..4].try_into().unwrap()) != 7
            {
                return Err(NOT_POSSIBLE);
            }
            for (i, &(a, b)) in MS_COEFFICIENTS.iter().enumerate() {
                let at = 4 + i * 4;
                if i16::from_le_bytes(extra[at..at + 2].try_into().unwrap()) != a
                    || i16::from_le_bytes(extra[at + 2..at + 4].try_into().unwrap()) != b
                {
                    return Err(NOT_POSSIBLE);
                }
            }
            ((block - 7 * ch) * 2 / ch) + 2
        };
        if expected != usize::from(format.frames_per_block) || expected == 0 {
            return Err(NOT_POSSIBLE);
        }
        Ok(format)
    }
    fn decoder(&self) -> Result<Decoder, u32> {
        let codec = match (self.tag, self.bits) {
            (1, 8) => Codec::PcmU8,
            (1, 16) => Codec::PcmS16Le,
            (2, _) => Codec::MicrosoftAdpcm,
            (0x11, _) => Codec::ImaAdpcm,
            _ => return Err(NOT_POSSIBLE),
        };
        Decoder::new(DecoderConfig {
            codec,
            sample_rate: self.rate,
            channels: self.channels,
            bits_per_sample: self.bits,
            frames_per_block: self.frames_per_block,
            max_frames_per_packet: self.packet_frames(),
        })
        .map_err(|_| NOT_POSSIBLE)
    }
    fn packet_frames(&self) -> usize {
        if self.tag == 1 {
            MAX_CONVERT_BYTES / usize::from(self.block)
        } else {
            usize::from(self.frames_per_block)
        }
    }
    fn pcm_bytes(&self) -> [u8; 18] {
        let mut bytes = [0; 18];
        bytes[..2].copy_from_slice(&1u16.to_le_bytes());
        bytes[2..4].copy_from_slice(&self.channels.to_le_bytes());
        bytes[4..8].copy_from_slice(&self.rate.to_le_bytes());
        bytes[8..12].copy_from_slice(&(self.rate * u32::from(self.block)).to_le_bytes());
        bytes[12..14].copy_from_slice(&self.block.to_le_bytes());
        bytes[14..16].copy_from_slice(&self.bits.to_le_bytes());
        bytes
    }
}

#[derive(Clone, Copy)]
struct Header {
    status: u32,
    src: u32,
    src_len: u32,
    dst: u32,
    dst_len: u32,
}
impl Header {
    fn read(cpu: &dyn Guest, address: u32) -> Result<Self, u32> {
        let mut bytes = [0; HEADER_BYTES];
        cpu.read(address, &mut bytes)
            .map_err(|_| INVALID_PARAMETER)?;
        let word = |at| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
        if word(0) < HEADER_BYTES as u32 || !cpu.accessible(address, HEADER_BYTES as u32, Prot::RW)
        {
            return Err(INVALID_PARAMETER);
        }
        Ok(Self {
            status: word(4),
            src: word(12),
            src_len: word(16),
            dst: word(28),
            dst_len: word(32),
        })
    }
    fn buffers_valid(&self, cpu: &dyn Guest) -> bool {
        (self.src_len == 0 || (self.src != 0 && cpu.accessible(self.src, self.src_len, Prot::READ)))
            && (self.dst_len == 0
                || (self.dst != 0 && cpu.accessible(self.dst, self.dst_len, Prot::WRITE)))
    }
}

struct Stream {
    source: Format,
    destination: Format,
    decoder: Decoder,
    headers: BTreeMap<u32, Header>,
}
#[derive(Default)]
struct Acm {
    namespace: Option<Namespace>,
    drivers: drivers::Catalog,
}

impl Acm {
    fn insert(&mut self, objects: &mut Objects, stream: Stream) -> Result<u32, u32> {
        if self.namespace.is_none() {
            self.namespace = objects.new_namespace_in(0xac00_0004, 0xacff_fffc);
        }
        let namespace = self.namespace.ok_or(NO_MEMORY)?;
        if objects.alias_count(namespace) >= MAX_STREAMS {
            return Err(NO_MEMORY);
        }
        objects.insert_host_in(namespace, stream).ok_or(NO_MEMORY)
    }
    fn get<'a>(&self, objects: &'a Objects, handle: u32) -> Option<&'a Stream> {
        objects.owned(objects.lookup_alias::<Stream>(self.namespace?, handle)?)
    }
    fn get_mut<'a>(&self, objects: &'a mut Objects, handle: u32) -> Option<&'a mut Stream> {
        let identity = objects.lookup_alias::<Stream>(self.namespace?, handle)?;
        objects.owned_mut(identity)
    }
    fn remove(&self, objects: &mut Objects, handle: u32) -> Result<(), u32> {
        self.get(objects, handle).ok_or(INVALID_HANDLE)?;
        objects
            .close_alias(self.namespace.unwrap(), handle)
            .ok_or(INVALID_HANDLE)?;
        Ok(())
    }
}

fn ensure(e: &mut Env<'_>) {
    if e.ext.try_get::<Acm>().is_none() {
        e.ext.insert(Acm::default());
    }
}
fn write_word(cpu: &mut dyn Guest, address: u32, offset: u32, value: u32) -> Result<(), u32> {
    cpu.write_u32(address.checked_add(offset).ok_or(INVALID_PARAMETER)?, value)
        .map_err(|_| INVALID_PARAMETER)
}
fn supported(source: &Format, destination: &Format) -> bool {
    destination.tag == 1
        && source.channels == destination.channels
        && source.rate == destination.rate
        && (source.tag == 1 || destination.bits == 16)
}

fn suggest(e: &mut Env<'_>) -> Result<(), u32> {
    let flags = e.arg(4);
    if flags & !0xf0000 != 0 {
        return Err(INVALID_FLAG);
    }
    if e.arg(0) != 0 {
        return Err(INVALID_HANDLE);
    }
    ensure(e);
    if !e.ext.get::<Acm>().drivers.enabled(&e.sys.objects) {
        return Err(NOT_POSSIBLE);
    }
    let source = Format::read(e.cpu, e.arg(1))?;
    let address = e.arg(2);
    let available = e.arg(3);
    if available < 16 || !e.cpu.accessible(address, available.min(18), Prot::RW) {
        return Err(INVALID_PARAMETER);
    }
    let channels = if flags & 0x20000 != 0 {
        e.cpu.read_u16(address + 2).map_err(|_| INVALID_PARAMETER)?
    } else {
        source.channels
    };
    let rate = if flags & 0x40000 != 0 {
        e.cpu.read_u32(address + 4).map_err(|_| INVALID_PARAMETER)?
    } else {
        source.rate
    };
    let bits = if flags & 0x80000 != 0 {
        e.cpu
            .read_u16(address + 14)
            .map_err(|_| INVALID_PARAMETER)?
    } else {
        16
    };
    if (flags & 0x10000 != 0 && e.cpu.read_u16(address).map_err(|_| INVALID_PARAMETER)? != 1)
        || channels != source.channels
        || rate != source.rate
        || !matches!(bits, 8 | 16)
        || (source.tag != 1 && bits != 16)
    {
        return Err(NOT_POSSIBLE);
    }
    let destination = Format {
        tag: 1,
        channels,
        rate,
        block: channels * (bits / 8),
        bits,
        frames_per_block: 1,
    };
    if rate.checked_mul(u32::from(destination.block)).is_none() {
        return Err(NOT_POSSIBLE);
    }
    e.cpu
        .write(
            address,
            &destination.pcm_bytes()[..available.min(18) as usize],
        )
        .map_err(|_| INVALID_PARAMETER)
}
fn open(e: &mut Env<'_>) -> Result<(), u32> {
    let flags = e.arg(7);
    if flags & !7 != 0 {
        return Err(INVALID_FLAG);
    }
    if e.arg(1) != 0 {
        return Err(INVALID_HANDLE);
    }
    ensure(e);
    if !e.ext.get::<Acm>().drivers.enabled(&e.sys.objects) {
        return Err(NOT_POSSIBLE);
    }
    if flags & 2 != 0 || e.arg(4) != 0 {
        return Err(NOT_SUPPORTED);
    }
    if e.arg(5) != 0 || e.arg(6) != 0 {
        return Err(INVALID_PARAMETER);
    }
    let source = Format::read(e.cpu, e.arg(2))?;
    let destination = Format::read(e.cpu, e.arg(3))?;
    if !supported(&source, &destination) {
        return Err(NOT_POSSIBLE);
    }
    let decoder = source.decoder()?;
    if flags & 1 != 0 {
        return Ok(());
    }
    let output = e.arg(0);
    if output == 0 || !e.cpu.accessible(output, 4, Prot::WRITE) {
        return Err(INVALID_PARAMETER);
    }
    ensure(e);
    let acm = e.ext.get_mut::<Acm>();
    let handle = acm.insert(
        &mut e.sys.objects,
        Stream {
            source,
            destination,
            decoder,
            headers: BTreeMap::new(),
        },
    )?;
    if e.cpu.write_u32(output, handle).is_err() {
        acm.remove(&mut e.sys.objects, handle)?;
        return Err(INVALID_PARAMETER);
    }
    Ok(())
}
fn size_impl(e: &mut Env<'_>, handle: u32, input: u32, output: u32, flags: u32) -> Result<(), u32> {
    if output == 0 || !e.cpu.accessible(output, 4, Prot::WRITE) {
        return Err(INVALID_PARAMETER);
    }
    e.cpu.write_u32(output, 0).map_err(|_| INVALID_PARAMETER)?;
    if flags > 1 {
        return Err(INVALID_FLAG);
    }
    if input == 0 {
        return Err(INVALID_PARAMETER);
    }
    ensure(e);
    let stream = e
        .ext
        .get::<Acm>()
        .get(&e.sys.objects, handle)
        .ok_or(INVALID_HANDLE)?;
    let src = u64::from(stream.source.block);
    let dst = u64::from(stream.source.frames_per_block) * u64::from(stream.destination.block);
    let value = if flags == 0 {
        u64::from(input).div_ceil(src) * dst
    } else {
        u64::from(input).div_ceil(dst) * src
    };
    let value = u32::try_from(value).map_err(|_| NOT_POSSIBLE)?;
    e.cpu
        .write_u32(output, value)
        .map_err(|_| INVALID_PARAMETER)
}
fn prepare(e: &mut Env<'_>) -> Result<(), u32> {
    let (handle, address, flags) = (e.arg(0), e.arg(1), e.arg(2));
    if flags != 0 {
        return Err(INVALID_FLAG);
    }
    ensure(e);
    if e.ext.get::<Acm>().get(&e.sys.objects, handle).is_none() {
        return Err(INVALID_HANDLE);
    }
    let header = Header::read(e.cpu, address)?;
    let stream = e
        .ext
        .get::<Acm>()
        .get_mut(&mut e.sys.objects, handle)
        .unwrap();
    if stream.headers.contains_key(&address) {
        return Ok(());
    }
    if header.status & PREPARED != 0 || !header.buffers_valid(e.cpu) {
        return Err(INVALID_PARAMETER);
    }
    stream.headers.insert(address, header);
    write_word(e.cpu, address, 4, header.status | PREPARED)
}
fn unprepare(e: &mut Env<'_>) -> Result<(), u32> {
    let (handle, address, flags) = (e.arg(0), e.arg(1), e.arg(2));
    if flags != 0 {
        return Err(INVALID_FLAG);
    }
    ensure(e);
    let header = Header::read(e.cpu, address)?;
    let stream = e
        .ext
        .get::<Acm>()
        .get_mut(&mut e.sys.objects, handle)
        .ok_or(INVALID_HANDLE)?;
    let prepared = stream.headers.get(&address).ok_or(UNPREPARED)?;
    if (header.src, header.dst, header.src_len, header.dst_len)
        != (
            prepared.src,
            prepared.dst,
            prepared.src_len,
            prepared.dst_len,
        )
    {
        return Err(INVALID_PARAMETER);
    }
    stream.headers.remove(&address);
    write_word(e.cpu, address, 4, header.status & !PREPARED)
}

fn convert(e: &mut Env<'_>) -> Result<(), u32> {
    let (handle, address, flags) = (e.arg(0), e.arg(1), e.arg(2));
    if flags & !(4 | 16 | 32) != 0 {
        return Err(INVALID_FLAG);
    }
    ensure(e);
    let header = Header::read(e.cpu, address)?;
    let stream = e
        .ext
        .get::<Acm>()
        .get_mut(&mut e.sys.objects, handle)
        .ok_or(INVALID_HANDLE)?;
    let prepared = stream.headers.get(&address).ok_or(UNPREPARED)?;
    if header.status & PREPARED == 0 {
        return Err(UNPREPARED);
    }
    if header.src != prepared.src
        || header.dst != prepared.dst
        || header.src_len > prepared.src_len
        || header.dst_len != prepared.dst_len
        || !header.buffers_valid(e.cpu)
    {
        return Err(INVALID_PARAMETER);
    }
    write_word(e.cpu, address, 20, 0)?;
    write_word(e.cpu, address, 36, 0)?;
    if flags & 16 != 0 {
        stream.decoder.reset();
    }
    let source_block = usize::from(stream.source.block);
    let output_block =
        usize::from(stream.source.frames_per_block) * usize::from(stream.destination.block);
    let blocks = (header.src_len as usize / source_block)
        .min(header.dst_len as usize / output_block)
        .min(MAX_CONVERT_BYTES / source_block);
    if blocks == 0
        && header.src_len != 0
        && stream.source.tag != 1
        && flags & 32 != 0
        && flags & 4 == 0
    {
        return Err(NOT_POSSIBLE);
    }
    let used = blocks * source_block;
    let mut source = vec![0; used];
    e.cpu
        .read(header.src, &mut source)
        .map_err(|_| INVALID_PARAMETER)?;
    let mut output = Vec::with_capacity(blocks * output_block);
    let frames = if stream.source.tag == 1 {
        blocks
    } else {
        usize::from(stream.source.frames_per_block)
    };
    let chunks = if stream.source.tag == 1 {
        used.max(1)
    } else {
        source_block
    };
    let encoding = if stream.destination.bits == 16 {
        PcmEncoding::S16Le
    } else {
        PcmEncoding::U8
    };
    for chunk in source.chunks(chunks) {
        stream
            .decoder
            .decode_into(chunk, frames as u64, encoding, &mut output)
            .map_err(|_| NOT_POSSIBLE)?;
    }
    if output.len() != blocks * output_block {
        return Err(NOT_POSSIBLE);
    }
    e.cpu
        .write(header.dst, &output)
        .map_err(|_| INVALID_PARAMETER)?;
    write_word(e.cpu, address, 20, used as u32)?;
    write_word(e.cpu, address, 36, output.len() as u32)?;
    write_word(
        e.cpu,
        address,
        4,
        (header.status | DONE | PREPARED) & !0x100000,
    )
}
fn close_impl(e: &mut Env<'_>, handle: u32, flags: u32) -> Result<(), u32> {
    if flags != 0 {
        return Err(INVALID_FLAG);
    }
    ensure(e);
    e.ext.get::<Acm>().remove(&mut e.sys.objects, handle)
}

pub fn register(registry: &mut Registry) {
    drivers::register(registry);
    guest::register(registry);
    registry.add(
        "msacm32.dll",
        "acmFormatSuggest",
        Conv::Stdcall { argc: 5 },
        |e| {
            let args = std::array::from_fn(|i| e.arg(i as u32));
            if args[0] != 0 {
                return guest::suggest(e, args);
            }
            match suggest(e) {
                Ok(()) => Outcome::Return(0),
                Err(NOT_POSSIBLE) => guest::suggest(e, args),
                Err(code) => Outcome::Return(code),
            }
        },
    );
    registry.add(
        "msacm32.dll",
        "acmStreamOpen",
        Conv::Stdcall { argc: 8 },
        |e| {
            let args = std::array::from_fn(|i| e.arg(i as u32));
            if args[1] != 0 {
                return guest::open_stream(e, args);
            }
            match open(e) {
                Ok(()) => Outcome::Return(0),
                Err(NOT_POSSIBLE) => guest::open_stream(e, args),
                Err(code) => Outcome::Return(code),
            }
        },
    );
    registry.register(size_ABI);
    registry.add(
        "msacm32.dll",
        "acmStreamPrepareHeader",
        Conv::Stdcall { argc: 3 },
        stream_front::<0x6051>,
    );
    registry.add(
        "msacm32.dll",
        "acmStreamConvert",
        Conv::Stdcall { argc: 3 },
        stream_front::<0x604f>,
    );
    registry.add(
        "msacm32.dll",
        "acmStreamUnprepareHeader",
        Conv::Stdcall { argc: 3 },
        stream_front::<0x6052>,
    );
    registry.register(reset_ABI);
    registry.register(close_ABI);
}
fn stream_front<const MESSAGE: u32>(e: &mut Env<'_>) -> Outcome {
    let (h, a, f) = (e.arg(0), e.arg(1), e.arg(2));
    if let Some(outcome) = guest::stream_call(e, h, MESSAGE, a, f) {
        return outcome;
    }
    let result = match MESSAGE {
        0x6051 => prepare(e),
        0x604f => convert(e),
        _ => unprepare(e),
    };
    Outcome::Return(result.err().unwrap_or(0))
}
#[win32(msacm32, name="acmStreamReset", level=implement)]
fn reset(e: &mut Env<'_>, handle: Handle, flags: u32) -> Outcome {
    if let Some(outcome) = guest::stream_call(e, handle.0, 0x6050, 0, flags) {
        return outcome;
    }
    if flags != 0 {
        return Outcome::Return(INVALID_FLAG);
    }
    ensure(e);
    let Some(stream) = e.ext.get::<Acm>().get_mut(&mut e.sys.objects, handle.0) else {
        return Outcome::Return(INVALID_HANDLE);
    };
    stream.decoder.reset();
    Outcome::Return(0)
}

#[win32(msacm32, name = "acmStreamSize", level = implement)]
fn size(
    env: &mut Env<'_>,
    stream: Handle,
    input: u32,
    output: GuestPtr<u32>,
    flags: u32,
) -> Outcome {
    if let Some(outcome) = guest::stream_call(env, stream.0, 0x604e, output.addr(), flags) {
        return outcome;
    }
    Outcome::Return(
        size_impl(env, stream.0, input, output.addr(), flags)
            .err()
            .unwrap_or(0),
    )
}
#[win32(msacm32, name = "acmStreamClose", level = implement)]
fn close(env: &mut Env<'_>, stream: Handle, flags: u32) -> Outcome {
    if let Some(outcome) = guest::stream_call(env, stream.0, 0x604d, 0, flags) {
        return outcome;
    }
    Outcome::Return(close_impl(env, stream.0, flags).err().unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use adv_machine::ObjectLimits;

    fn pcm() -> Stream {
        let source = Format {
            tag: 1,
            channels: 1,
            rate: 22050,
            block: 2,
            bits: 16,
            frames_per_block: 1,
        };
        Stream {
            decoder: source.decoder().unwrap(),
            destination: source.clone(),
            source,
            headers: BTreeMap::new(),
        }
    }

    #[test]
    fn common_limits_and_stale_handles_do_not_corrupt_another_decoder() {
        for limits in [
            ObjectLimits {
                objects: 1,
                ..ObjectLimits::default()
            },
            ObjectLimits {
                aliases: 1,
                ..ObjectLimits::default()
            },
        ] {
            let mut objects = Objects::with_limits(limits);
            let mut acm = Acm::default();
            let first = acm.insert(&mut objects, pcm()).unwrap();
            assert_eq!(acm.insert(&mut objects, pcm()), Err(NO_MEMORY));
            assert!(objects.close(first).is_none());
            acm.get_mut(&mut objects, first).unwrap().source.rate = 11025;
            assert_eq!(acm.get(&objects, first).unwrap().source.rate, 11025);
            acm.remove(&mut objects, first).unwrap();
            assert_eq!(objects.object_count(), 0);
            let second = acm.insert(&mut objects, pcm()).unwrap();
            assert_ne!(first, second);
            assert_eq!(acm.remove(&mut objects, first), Err(INVALID_HANDLE));
            assert_eq!(acm.get(&objects, second).unwrap().source.rate, 22050);
            acm.remove(&mut objects, second).unwrap();
        }
    }
}
