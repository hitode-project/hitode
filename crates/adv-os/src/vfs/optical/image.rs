//! Disc-image containers below the ISO9660 reader. Every container resolves to
//! the host file holding the first track, and `Disc` presents that track's
//! 2048-byte user data as one contiguous logical image. The ISO parser, bounds
//! and volume identity therefore stay independent of the container.
//!
//! Raw 2352-byte sectors are recognized by their sync pattern, so a lone
//! `.bin`, `.img` or `.mdf` mounts without its descriptor. Descriptors that
//! only name a sibling (`.ccd` → `.img`, `.mds` → `.mdf`) resolve by file stem.
//! CUE sheets and Nero images carry the track's offset and sector size; the
//! Nero block layout follows libmirage
//! (https://github.com/cdemu/cdemu/tree/master/libmirage/images/image-nrg).
//! Subchannel data and audio tracks are ignored, and a disc whose first track
//! is audio is rejected.
use super::invalid;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

const USER: u64 = 2048;
const RAW: u64 = 2352;
const SUBCHANNEL: u64 = RAW + 96;
const SYNC: [u8; 12] = [0, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 0];
const MAX_CUE_BYTES: u64 = 1 << 20;
const MAX_NRG_BLOCKS: usize = 256;
/// Raw sectors gathered per host read; bounds the scratch buffer.
const BATCH_SECTORS: u64 = 32;

/// The first track's user data as a logical 2048-byte-sector image.
pub(super) struct Disc {
    file: File,
    /// Host offset of logical sector 0.
    start: u64,
    /// Host bytes per sector.
    stride: u64,
    /// User-data offset inside a host sector.
    data: u64,
    len: u64,
    position: u64,
    scratch: Vec<u8>,
}

fn unsupported(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, message)
}

fn extension(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
}

/// The descriptor's sibling with the same stem, in either extension case.
fn sibling(path: &Path, extension: &str) -> io::Result<File> {
    let lower = path.with_extension(extension);
    match File::open(&lower) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            File::open(path.with_extension(extension.to_ascii_uppercase()))
        }
        other => other,
    }
}

impl Disc {
    pub(super) fn open(path: &Path) -> io::Result<Self> {
        match extension(path).as_deref() {
            Some("cue") => cue(path),
            Some("ccd") => Self::track(sibling(path, "img")?, 0, None, None),
            Some("mds") => Self::track(sibling(path, "mdf")?, 0, None, None),
            Some("nrg") => nrg(path),
            _ => Self::track(File::open(path)?, 0, None, None),
        }
    }

    pub(super) fn len(&self) -> u64 {
        self.len
    }

    /// A track at host offset `start`. Without a stated sector size, a sync
    /// pattern selects raw sectors and its absence selects 2048-byte sectors.
    fn track(
        mut file: File,
        start: u64,
        stride: Option<u64>,
        end: Option<u64>,
    ) -> io::Result<Self> {
        let end = match end {
            Some(end) => end,
            None => file.metadata()?.len(),
        };
        if end < start {
            return Err(invalid("disc track ends before it starts"));
        }
        let mut header = [0; 16];
        file.seek(SeekFrom::Start(start))?;
        let synced = file.read_exact(&mut header).is_ok() && header[..12] == SYNC;
        let (stride, data) = match stride {
            Some(USER) => (USER, 0),
            // Mode 2 without sync and header: an 8-byte subheader precedes data.
            Some(2336) => (2336, 8),
            None if !synced => (USER, 0),
            Some(RAW | SUBCHANNEL) | None => {
                if !synced {
                    return Err(unsupported("first disc track is not a data track"));
                }
                let stride = match stride {
                    Some(stride) => stride,
                    None => {
                        let mut next = [0; 12];
                        let mut at = |offset| {
                            file.seek(SeekFrom::Start(start + offset))?;
                            file.read_exact(&mut next)?;
                            io::Result::Ok(next == SYNC)
                        };
                        if at(RAW)? {
                            RAW
                        } else if at(SUBCHANNEL)? {
                            SUBCHANNEL
                        } else {
                            return Err(invalid("raw disc sectors have no fixed size"));
                        }
                    }
                };
                let data = match header[15] {
                    1 => 16,
                    // Mode 2 Form 1: 4-byte subheader repeated after the header.
                    2 => 24,
                    _ => return Err(unsupported("unsupported raw sector mode")),
                };
                (stride, data)
            }
            Some(_) => return Err(unsupported("unsupported disc sector size")),
        };
        let bytes = end - start;
        let len = if stride == USER {
            bytes
        } else {
            bytes / stride * USER
        };
        Ok(Self {
            file,
            start,
            stride,
            data,
            len,
            position: 0,
            scratch: Vec::new(),
        })
    }
}

impl Read for Disc {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let wanted = (output.len() as u64).min(self.len.saturating_sub(self.position)) as usize;
        if wanted == 0 {
            return Ok(0);
        }
        if self.stride == USER {
            self.file
                .seek(SeekFrom::Start(self.start + self.position))?;
            self.file.read_exact(&mut output[..wanted])?;
            self.position += wanted as u64;
            return Ok(wanted);
        }
        let first = self.position / USER;
        let within = (self.position % USER) as usize;
        let sectors = (within as u64 + wanted as u64)
            .div_ceil(USER)
            .min(BATCH_SECTORS);
        self.scratch.resize((sectors * self.stride) as usize, 0);
        self.file
            .seek(SeekFrom::Start(self.start + first * self.stride))?;
        self.file.read_exact(&mut self.scratch)?;
        let mut written = 0;
        for sector in self.scratch.chunks_exact(self.stride as usize) {
            let skip = if written == 0 { within } else { 0 };
            let from = self.data as usize + skip;
            let count = (USER as usize - skip).min(wanted - written);
            output[written..written + count].copy_from_slice(&sector[from..from + count]);
            written += count;
            if written == wanted {
                break;
            }
        }
        self.position += written as u64;
        Ok(written)
    }
}

impl Seek for Disc {
    fn seek(&mut self, seek: SeekFrom) -> io::Result<u64> {
        self.position = match seek {
            SeekFrom::Start(position) => Some(position),
            SeekFrom::Current(offset) => self.position.checked_add_signed(offset),
            SeekFrom::End(offset) => self.len.checked_add_signed(offset),
        }
        .ok_or(io::ErrorKind::InvalidInput)?;
        Ok(self.position)
    }
}

/// One whitespace-separated or double-quoted CUE token and the rest of the line.
fn token(line: &[u8]) -> (&[u8], &[u8]) {
    let line = line.trim_ascii_start();
    if let Some(quoted) = line.strip_prefix(b"\"") {
        let end = quoted
            .iter()
            .position(|&b| b == b'"')
            .unwrap_or(quoted.len());
        (&quoted[..end], quoted.get(end + 1..).unwrap_or_default())
    } else {
        let end = line
            .iter()
            .position(u8::is_ascii_whitespace)
            .unwrap_or(line.len());
        (&line[..end], &line[end..])
    }
}

fn msf(text: &[u8]) -> io::Result<u64> {
    let mut parts = text.split(|&b| b == b':').map(|part| {
        std::str::from_utf8(part)
            .ok()
            .and_then(|part| part.parse::<u64>().ok())
    });
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(Some(m)), Some(Some(s)), Some(Some(f)), None) if s < 60 && f < 75 => {
            Ok((m * 60 + s) * 75 + f)
        }
        _ => Err(invalid("invalid CUE index time")),
    }
}

/// CUE `FILE` names carry no encoding; the first decoding naming an existing
/// file wins, then the sheet's own stem with the named extension.
fn cue_file(sheet: &Path, name: &[u8]) -> io::Result<File> {
    let directory = sheet.parent().unwrap_or(Path::new(""));
    let decoders = [
        encoding_rs::UTF_8,
        encoding_rs::SHIFT_JIS,
        encoding_rs::GBK,
        encoding_rs::BIG5,
    ];
    for encoding in decoders {
        if let Some(name) = encoding.decode_without_bom_handling_and_without_replacement(name)
            && let Ok(file) = File::open(directory.join(&*name))
        {
            return Ok(file);
        }
    }
    let extension = Path::new(&*String::from_utf8_lossy(name))
        .extension()
        .map(|e| e.to_owned())
        .unwrap_or_default();
    File::open(PathBuf::from(sheet).with_extension(extension))
}

fn cue(path: &Path) -> io::Result<Disc> {
    let mut sheet = Vec::new();
    File::open(path)?
        .take(MAX_CUE_BYTES + 1)
        .read_to_end(&mut sheet)?;
    if sheet.len() as u64 > MAX_CUE_BYTES {
        return Err(invalid("CUE sheet exceeds metadata limit"));
    }
    let sheet = sheet.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&sheet);
    let mut file: Option<&[u8]> = None;
    // The first track's file, sector size and INDEX 01 frame.
    let mut first: Option<(&[u8], u64, Option<u64>)> = None;
    let mut next_track = false;
    let mut end = None;
    for line in sheet.split(|&b| b == b'\n') {
        let (keyword, rest) = token(line);
        match keyword.to_ascii_uppercase().as_slice() {
            b"FILE" => {
                if first.is_some() {
                    break;
                }
                file = Some(token(rest).0);
            }
            b"TRACK" if first.is_none() => {
                let file = file.ok_or_else(|| invalid("CUE track precedes FILE"))?;
                let mode = token(token(rest).1).0.to_ascii_uppercase();
                let stride = match mode.as_slice() {
                    b"MODE1/2048" | b"MODE2/2048" => USER,
                    b"MODE2/2336" => 2336,
                    b"MODE1/2352" | b"MODE2/2352" => RAW,
                    b"AUDIO" => return Err(unsupported("first disc track is not a data track")),
                    _ => return Err(unsupported("unsupported CUE track mode")),
                };
                first = Some((file, stride, None));
            }
            b"TRACK" => next_track = true,
            b"INDEX" => {
                let (number, rest) = token(rest);
                let frame = msf(token(rest).0)?;
                let Some((_, _, index)) = &mut first else {
                    return Err(invalid("CUE index precedes TRACK"));
                };
                if next_track {
                    end = Some(frame);
                    break;
                }
                if number == b"01" || number == b"1" {
                    *index = Some(frame);
                }
            }
            _ => {}
        }
    }
    let (name, stride, index) = first.ok_or_else(|| invalid("CUE sheet has no track"))?;
    let index = index.ok_or_else(|| invalid("CUE track has no INDEX 01"))?;
    Disc::track(
        cue_file(path, name)?,
        index * stride,
        Some(stride),
        end.map(|frame| frame * stride),
    )
}

fn be(bytes: &[u8], offset: usize, width: usize) -> io::Result<u64> {
    let field = bytes
        .get(offset..offset + width)
        .ok_or_else(|| invalid("truncated Nero track block"))?;
    Ok(field.iter().fold(0, |value, &b| value << 8 | u64::from(b)))
}

/// Nero mode codes: (bytes per sector, audio).
fn nrg_mode(code: u8) -> io::Result<(u64, bool)> {
    match code {
        0x00 | 0x02 => Ok((USER, false)),
        0x03 => Ok((2336, false)),
        0x05 | 0x06 => Ok((RAW, false)),
        0x0f | 0x11 => Ok((SUBCHANNEL, false)),
        0x07 => Ok((RAW, true)),
        0x10 => Ok((SUBCHANNEL, true)),
        _ => Err(unsupported("unsupported Nero track mode")),
    }
}

fn nrg(path: &Path) -> io::Result<Disc> {
    let mut file = File::open(path)?;
    let size = file.metadata()?.len();
    let mut tail = [0; 12];
    if size < tail.len() as u64 {
        return Err(invalid("missing Nero image trailer"));
    }
    file.seek(SeekFrom::End(-12))?;
    file.read_exact(&mut tail)?;
    // NER5 images use 64-bit offsets; NERO images use 32-bit offsets.
    let (wide, trailer, blocks_end) = if &tail[..4] == b"NER5" {
        (true, be(&tail, 4, 8)?, size - 12)
    } else if &tail[4..8] == b"NERO" {
        (false, be(&tail, 8, 4)?, size - 8)
    } else {
        return Err(invalid("missing Nero image trailer"));
    };
    if trailer >= blocks_end || blocks_end - trailer > MAX_CUE_BYTES {
        return Err(invalid("Nero block list exceeds image bounds"));
    }
    let mut blocks = vec![0; (blocks_end - trailer) as usize];
    file.seek(SeekFrom::Start(trailer))?;
    file.read_exact(&mut blocks)?;
    let mut offset = 0;
    for _ in 0..MAX_NRG_BLOCKS {
        let id = blocks
            .get(offset..offset + 4)
            .ok_or_else(|| invalid("Nero image has no track block"))?;
        let length = be(&blocks, offset + 4, 4)? as usize;
        let body = blocks
            .get(offset + 8..offset + 8 + length)
            .ok_or_else(|| invalid("Nero block exceeds image bounds"))?;
        let width = if wide { 8 } else { 4 };
        let track = match id {
            // Disc-at-once: 22-byte header, then sector size, mode and
            // pregap/start/end offsets per track.
            b"DAOX" | b"DAOI" => {
                let block = body.get(22..).unwrap_or_default();
                let (stride, audio) = nrg_mode(be(block, 14, 1)? as u8)?;
                if be(block, 12, 2)? != stride {
                    return Err(invalid("Nero sector size does not match its mode"));
                }
                let start = be(block, 18 + width, width)?;
                Some((
                    start,
                    Some(be(block, 18 + 2 * width, width)?),
                    stride,
                    audio,
                ))
            }
            // Track-at-once: offset, size and mode per track.
            b"ETN2" | b"ETNF" => {
                let (stride, audio) = nrg_mode(be(body, 2 * width + 3, 1)? as u8)?;
                let start = be(body, 0, width)?;
                let size = be(body, width, width)?;
                Some((start, start.checked_add(size), stride, audio))
            }
            b"END!" => return Err(invalid("Nero image has no track block")),
            _ => None,
        };
        if let Some((start, end, stride, audio)) = track {
            if audio {
                return Err(unsupported("first disc track is not a data track"));
            }
            let end = end.filter(|&end| end <= trailer);
            if end.is_none() {
                return Err(invalid("Nero track exceeds image bounds"));
            }
            return Disc::track(file, start, Some(stride), end);
        }
        offset += 8 + length;
    }
    Err(invalid("Nero block count exceeds metadata limit"))
}
