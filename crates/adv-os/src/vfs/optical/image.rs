//! Disc-image containers below the ISO9660 reader. Every container resolves to
//! the host file(s) holding the first track, and `Disc` presents that track's
//! 2048-byte user data as one contiguous logical image. The ISO parser, bounds
//! and volume identity therefore stay independent of the container.
//!
//! Alcohol 120% `.mds` descriptors are parsed for their first track's offset,
//! sector size, mode and data files (including multi-part `.mdf`/`.md0`...
//! splits); opening a `.mdf` directly also discovers a sibling `.mds` and any
//! `.md0`... continuation parts. Without a descriptor, raw sectors (2352,
//! 2368, 2448 and 2336 bytes) are recognized by their sync pattern and LBA 16
//! ISO9660 descriptor signature, tolerating zeroed or Mode-0 system-area
//! sectors and 150-sector pregaps. CUE sheets and Nero images carry the
//! track's offset and sector size; the Nero and MDS layouts follow libmirage
//! (https://github.com/cdemu/cdemu/tree/master/libmirage/images). Subchannel
//! data and audio tracks are ignored, and a disc whose first track is audio is
//! rejected.
use super::invalid;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

const USER: u64 = 2048;
const MODE2_NO_SYNC: u64 = 2336;
const RAW: u64 = 2352;
const PQ_SUBCHANNEL: u64 = RAW + 16;
const SUBCHANNEL: u64 = RAW + 96;
const SYNC: [u8; 12] = [0, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 0];
const MAX_CUE_BYTES: u64 = 1 << 20;
const MAX_MDS_BYTES: u64 = 16 << 20;
const MAX_NRG_BLOCKS: usize = 256;
const MAX_MDF_PARTS: usize = 100;
const PREGAP_SECTORS: u64 = 150;
/// Raw sectors gathered per host read; bounds the scratch buffer.
const BATCH_SECTORS: u64 = 32;

struct Segment {
    file: File,
    offset: u64,
    len: u64,
}

/// One host file or a contiguous sequence of split parts (e.g. `.mdf` + `.md0`).
struct Media {
    segments: Vec<Segment>,
    len: u64,
}

impl Media {
    fn single(file: File) -> io::Result<Self> {
        let len = file.metadata()?.len();
        Ok(Self {
            segments: vec![Segment {
                file,
                offset: 0,
                len,
            }],
            len,
        })
    }

    fn from_files(files: Vec<File>) -> io::Result<Self> {
        if files.is_empty() {
            return Err(io::ErrorKind::NotFound.into());
        }
        let mut segments = Vec::with_capacity(files.len());
        let mut offset = 0u64;
        for file in files {
            let len = file.metadata()?.len();
            segments.push(Segment { file, offset, len });
            offset = offset
                .checked_add(len)
                .ok_or_else(|| invalid("disc image size overflow"))?;
        }
        Ok(Self {
            segments,
            len: offset,
        })
    }

    /// Opens `path` and appends any Alcohol 120% split continuation parts
    /// (`.md0`, `.md1`, ...) that sit beside a `.mdf`.
    fn open_mdf(path: &Path) -> io::Result<Self> {
        let mut files = vec![File::open(path)?];
        if extension(path).as_deref() == Some("mdf") {
            append_mdf_parts(path, &mut files);
        }
        Self::from_files(files)
    }

    fn len(&self) -> u64 {
        self.len
    }

    fn read_exact_at(&mut self, offset: u64, mut output: &mut [u8]) -> io::Result<()> {
        if offset
            .checked_add(output.len() as u64)
            .is_none_or(|end| end > self.len)
        {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        if output.is_empty() {
            return Ok(());
        }
        if self.segments.len() == 1 {
            let file = &mut self.segments[0].file;
            file.seek(SeekFrom::Start(offset))?;
            return file.read_exact(output);
        }
        let mut cursor = offset;
        let index = self
            .segments
            .partition_point(|seg| seg.offset + seg.len <= cursor);
        for seg in &mut self.segments[index..] {
            if output.is_empty() {
                break;
            }
            if seg.len == 0 {
                continue;
            }
            let within = cursor - seg.offset;
            let take = (seg.len - within).min(output.len() as u64) as usize;
            seg.file.seek(SeekFrom::Start(within))?;
            seg.file.read_exact(&mut output[..take])?;
            cursor += take as u64;
            output = &mut output[take..];
        }
        if !output.is_empty() {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        Ok(())
    }
}

/// The first track's user data as a logical 2048-byte-sector image.
pub(super) struct Disc {
    media: Media,
    /// Host offset of logical sector 0.
    start: u64,
    /// Host bytes per sector.
    stride: u64,
    /// User-data offset inside a host sector.
    data: u64,
    len: u64,
    position: u64,
    /// Logical sector of a Volume Descriptor Set Terminator whose trailing
    /// body (`7..2048`) must be presented zero-filled for the ISO parser.
    zero_terminator_sector: Option<u64>,
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

/// Finds a same-stem sibling with `extension` in lowercase or uppercase.
fn sibling_path(path: &Path, ext: &str) -> Option<PathBuf> {
    let lower = path.with_extension(ext);
    if lower.is_file() {
        return Some(lower);
    }
    let upper = path.with_extension(ext.to_ascii_uppercase());
    if upper.is_file() {
        return Some(upper);
    }
    None
}

/// The descriptor's sibling with the same stem, in either extension case.
fn sibling(path: &Path, ext: &str) -> io::Result<File> {
    let lower = path.with_extension(ext);
    match File::open(&lower) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            File::open(path.with_extension(ext.to_ascii_uppercase()))
        }
        other => other,
    }
}

fn append_mdf_parts(path: &Path, files: &mut Vec<File>) {
    for index in 0..MAX_MDF_PARTS {
        let ext = format!("md{index}");
        let Ok(file) = sibling(path, &ext) else {
            break;
        };
        files.push(file);
    }
}

fn is_iso_descriptor(header: &[u8; 7], allow_terminator: bool) -> bool {
    &header[1..6] == b"CD001"
        && (header[6] == 1 || (header[0] == 2 && header[6] == 2))
        && (header[0] <= 3 || (allow_terminator && header[0] == 255))
}

fn has_iso_descriptor_at(media: &mut Media, offset: u64, end: u64, allow_terminator: bool) -> bool {
    if offset.checked_add(7).is_none_or(|stop| stop > end) {
        return false;
    }
    let mut buf = [0u8; 7];
    media.read_exact_at(offset, &mut buf).is_ok() && is_iso_descriptor(&buf, allow_terminator)
}

fn is_raw_stride(stride: u64) -> bool {
    matches!(stride, RAW | PQ_SUBCHANNEL | SUBCHANNEL)
}

/// Verifies that logical sectors 16 and 17 carry valid ISO9660 volume
/// descriptors (and a raw sync pattern with matching mode for raw strides).
fn matches_iso_layout(media: &mut Media, base: u64, stride: u64, data: u64, end: u64) -> bool {
    let Some(lba16) = stride.checked_mul(16).and_then(|off| base.checked_add(off)) else {
        return false;
    };
    let Some(lba17) = lba16.checked_add(stride) else {
        return false;
    };
    if lba17.checked_add(data + 7).is_none_or(|stop| stop > end) {
        return false;
    }
    if is_raw_stride(stride) {
        let mut header = [0u8; 16];
        if media.read_exact_at(lba16, &mut header).is_err() || header[..12] != SYNC {
            return false;
        }
        let valid_mode = match header[15] {
            1 => data == 16,
            2 => data == 24 || data == 16,
            _ => false,
        };
        if !valid_mode {
            return false;
        }
    }
    has_iso_descriptor_at(media, lba16 + data, end, false)
        && has_iso_descriptor_at(media, lba17 + data, end, true)
}

fn probe_raw_at(media: &mut Media, base: u64, strides: &[u64], end: u64) -> Option<(u64, u64)> {
    for &stride in strides {
        for data in [16, 24] {
            if matches_iso_layout(media, base, stride, data, end) {
                return Some((stride, data));
            }
        }
    }
    None
}

fn probe_mode2_no_sync(media: &mut Media, base: u64, end: u64) -> Option<(u64, u64)> {
    for data in [8, 0] {
        if matches_iso_layout(media, base, MODE2_NO_SYNC, data, end) {
            return Some((MODE2_NO_SYNC, data));
        }
    }
    None
}

impl Disc {
    pub(super) fn open(path: &Path) -> io::Result<Self> {
        match extension(path).as_deref() {
            Some("cue") => cue(path),
            Some("ccd") => Self::track(Media::single(sibling(path, "img")?)?, 0, None, None, &[]),
            Some("mds") => mds(path),
            Some("mdf") => {
                if let Some(descriptor) = sibling_path(path, "mds") {
                    match mds_for_mdf(&descriptor, path) {
                        Ok(Some(disc)) => return Ok(disc),
                        Err(error) if error.kind() == io::ErrorKind::Unsupported => {
                            return Err(error);
                        }
                        Ok(None) | Err(_) => {}
                    }
                }
                Self::track(Media::open_mdf(path)?, 0, None, None, &[])
            }
            Some("nrg") => nrg(path),
            _ => Self::track(Media::single(File::open(path)?)?, 0, None, None, &[]),
        }
    }

    pub(super) fn len(&self) -> u64 {
        self.len
    }

    pub(super) fn normalize_terminator(&mut self, sector: u64) {
        self.zero_terminator_sector = Some(sector);
    }

    /// A track at host offset `start`. Without a stated sector size, sync
    /// patterns and the LBA 16 ISO9660 descriptor signature select raw or
    /// cooked 2048-byte sectors.
    fn track(
        mut media: Media,
        start: u64,
        stride: Option<u64>,
        end: Option<u64>,
        extra_pregaps: &[u64],
    ) -> io::Result<Self> {
        let end = match end {
            Some(end) => end.min(media.len()),
            None => media.len(),
        };
        if end < start {
            return Err(invalid("disc track ends before it starts"));
        }
        let mut header = [0; 16];
        let has_header = media.read_exact_at(start, &mut header).is_ok();
        let synced = has_header && header[..12] == SYNC;
        let all_zero = has_header && header == [0; 16];

        let (start, stride, data) = match stride {
            Some(USER) => {
                let mut chosen = start;
                if !matches_iso_layout(&mut media, start, USER, 0, end) {
                    for &sectors in extra_pregaps {
                        if let Some(base) = sectors
                            .checked_mul(USER)
                            .and_then(|off| start.checked_add(off))
                            && matches_iso_layout(&mut media, base, USER, 0, end)
                        {
                            chosen = base;
                            break;
                        }
                    }
                }
                (chosen, USER, 0)
            }
            // Mode 2 without sync and header: an 8-byte subheader precedes data
            // for Form 1, or 0 bytes for formless Mode 2.
            Some(MODE2_NO_SYNC) => {
                let mut found = None;
                for &sectors in std::iter::once(&0).chain(extra_pregaps.iter()) {
                    if let Some(base) = sectors
                        .checked_mul(MODE2_NO_SYNC)
                        .and_then(|off| start.checked_add(off))
                        && let Some((_, data)) = probe_mode2_no_sync(&mut media, base, end)
                    {
                        found = Some((base, MODE2_NO_SYNC, data));
                        break;
                    }
                }
                found.unwrap_or((start, MODE2_NO_SYNC, 8))
            }
            Some(s @ (RAW | PQ_SUBCHANNEL | SUBCHANNEL)) => {
                let mut found = None;
                if synced || all_zero {
                    for &sectors in std::iter::once(&0).chain(extra_pregaps.iter()) {
                        if let Some(base) = sectors
                            .checked_mul(s)
                            .and_then(|off| start.checked_add(off))
                            && let Some((_, data)) = probe_raw_at(&mut media, base, &[s], end)
                        {
                            found = Some((base, s, data));
                            break;
                        }
                    }
                }
                if let Some(hit) = found {
                    hit
                } else {
                    if !synced {
                        return Err(unsupported("first disc track is not a data track"));
                    }
                    let data = match header[15] {
                        1 => 16,
                        // Mode 2 Form 1: 4-byte subheader repeated after the header.
                        2 => 24,
                        _ => return Err(unsupported("unsupported raw sector mode")),
                    };
                    (start, s, data)
                }
            }
            None if synced => {
                const RAW_STRIDES: [u64; 3] = [RAW, SUBCHANNEL, PQ_SUBCHANNEL];
                let mut found = None;
                for &sectors in std::iter::once(&0)
                    .chain(extra_pregaps.iter())
                    .chain(std::iter::once(&PREGAP_SECTORS))
                {
                    for &s in &RAW_STRIDES {
                        if let Some(base) = sectors
                            .checked_mul(s)
                            .and_then(|off| start.checked_add(off))
                            && let Some((_, data)) = probe_raw_at(&mut media, base, &[s], end)
                        {
                            found = Some((base, s, data));
                            break;
                        }
                    }
                    if found.is_some() {
                        break;
                    }
                }
                if let Some(hit) = found {
                    hit
                } else {
                    let mut next = [0; 12];
                    let mut at = |offset: u64| {
                        media.read_exact_at(start + offset, &mut next)?;
                        io::Result::Ok(next == SYNC)
                    };
                    let stride = if at(RAW)? {
                        RAW
                    } else if at(SUBCHANNEL)? {
                        SUBCHANNEL
                    } else if at(PQ_SUBCHANNEL)? {
                        PQ_SUBCHANNEL
                    } else {
                        return Err(invalid("raw disc sectors have no fixed size"));
                    };
                    let data = match header[15] {
                        1 => 16,
                        2 => 24,
                        _ => return Err(unsupported("unsupported raw sector mode")),
                    };
                    (start, stride, data)
                }
            }
            None if all_zero => {
                if matches_iso_layout(&mut media, start, USER, 0, end) {
                    (start, USER, 0)
                } else {
                    const CANDIDATES: [(u64, u64); 9] = [
                        (RAW, 16),
                        (RAW, 24),
                        (SUBCHANNEL, 16),
                        (SUBCHANNEL, 24),
                        (PQ_SUBCHANNEL, 16),
                        (PQ_SUBCHANNEL, 24),
                        (MODE2_NO_SYNC, 8),
                        (MODE2_NO_SYNC, 0),
                        (USER, 0),
                    ];
                    let mut found = None;
                    for &sectors in std::iter::once(&0)
                        .chain(extra_pregaps.iter())
                        .chain(std::iter::once(&PREGAP_SECTORS))
                    {
                        for &(s, d) in &CANDIDATES {
                            if sectors == 0 && s == USER {
                                continue;
                            }
                            if let Some(base) = sectors
                                .checked_mul(s)
                                .and_then(|off| start.checked_add(off))
                                && matches_iso_layout(&mut media, base, s, d, end)
                            {
                                found = Some((base, s, d));
                                break;
                            }
                        }
                        if found.is_some() {
                            break;
                        }
                    }
                    found.unwrap_or((start, USER, 0))
                }
            }
            None => {
                if matches_iso_layout(&mut media, start, USER, 0, end) {
                    (start, USER, 0)
                } else if let Some((s, d)) = probe_mode2_no_sync(&mut media, start, end) {
                    (start, s, d)
                } else {
                    (start, USER, 0)
                }
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
            media,
            start,
            stride,
            data,
            len,
            position: 0,
            zero_terminator_sector: None,
            scratch: Vec::new(),
        })
    }

    fn apply_terminator_normalization(&self, logical_start: u64, output: &mut [u8]) {
        let Some(sector) = self.zero_terminator_sector else {
            return;
        };
        let zero_start = sector * USER + 7;
        let zero_end = (sector + 1) * USER;
        let read_end = logical_start + output.len() as u64;
        let overlap_start = logical_start.max(zero_start);
        let overlap_end = read_end.min(zero_end);
        if overlap_start < overlap_end {
            let from = (overlap_start - logical_start) as usize;
            let to = (overlap_end - logical_start) as usize;
            output[from..to].fill(0);
        }
    }
}

impl Read for Disc {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let wanted = (output.len() as u64).min(self.len.saturating_sub(self.position)) as usize;
        if wanted == 0 {
            return Ok(0);
        }
        let logical_start = self.position;
        if self.stride == USER {
            self.media
                .read_exact_at(self.start + self.position, &mut output[..wanted])?;
            self.apply_terminator_normalization(logical_start, &mut output[..wanted]);
            self.position += wanted as u64;
            return Ok(wanted);
        }
        let first = self.position / USER;
        let within = (self.position % USER) as usize;
        let sectors = (within as u64 + wanted as u64)
            .div_ceil(USER)
            .min(BATCH_SECTORS);
        self.scratch.resize((sectors * self.stride) as usize, 0);
        self.media
            .read_exact_at(self.start + first * self.stride, &mut self.scratch)?;
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
        self.apply_terminator_normalization(logical_start, &mut output[..written]);
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

fn le16(bytes: &[u8], offset: usize) -> Option<u16> {
    let field = bytes.get(offset..offset + 2)?;
    Some(u16::from_le_bytes([field[0], field[1]]))
}

fn le32(bytes: &[u8], offset: usize) -> Option<u32> {
    let field = bytes.get(offset..offset + 4)?;
    Some(u32::from_le_bytes([field[0], field[1], field[2], field[3]]))
}

fn le64(bytes: &[u8], offset: usize) -> Option<u64> {
    let field = bytes.get(offset..offset + 8)?;
    Some(u64::from_le_bytes(field.try_into().ok()?))
}

struct MdsTrack {
    mode: u8,
    sector_size: u64,
    pregap: u64,
    start_offset: u64,
    end_offset: Option<u64>,
    filenames: Vec<PathBuf>,
}

fn decode_mds_filename(
    descriptor: &Path,
    bytes: &[u8],
    offset: usize,
    wide: bool,
) -> Option<PathBuf> {
    let directory = descriptor.parent().unwrap_or(Path::new(""));
    let raw = bytes.get(offset..)?;
    let resolve = |name: &str| -> Option<PathBuf> {
        let name = name.trim();
        if name.is_empty() {
            return None;
        }
        if let Some(ext) = name.strip_prefix("*.") {
            return sibling_path(descriptor, ext).or_else(|| Some(descriptor.with_extension(ext)));
        }
        let direct = directory.join(name);
        if direct.is_file() {
            return Some(direct);
        }
        let leaf = name.rsplit(['\\', '/']).next().unwrap_or(name);
        let candidate = directory.join(leaf);
        if candidate.is_file() {
            return Some(candidate);
        }
        if let Some(ext) = Path::new(leaf).extension().and_then(|e| e.to_str())
            && let Some(sib) = sibling_path(descriptor, ext)
        {
            return Some(sib);
        }
        Some(candidate)
    };
    if wide {
        let mut units = Vec::new();
        for pair in raw.as_chunks::<2>().0.iter().take(1024) {
            let unit = u16::from_le_bytes([pair[0], pair[1]]);
            if unit == 0 {
                break;
            }
            units.push(unit);
        }
        let name = String::from_utf16(&units).ok()?;
        resolve(&name)
    } else {
        let end = raw
            .iter()
            .take(1024)
            .position(|&b| b == 0)
            .unwrap_or(raw.len().min(1024));
        let slice = &raw[..end];
        for encoding in [
            encoding_rs::UTF_8,
            encoding_rs::SHIFT_JIS,
            encoding_rs::GBK,
            encoding_rs::BIG5,
        ] {
            if let Some(name) = encoding.decode_without_bom_handling_and_without_replacement(slice)
                && let Some(path) = resolve(&name)
                && path.is_file()
            {
                return Some(path);
            }
        }
        resolve(&String::from_utf8_lossy(slice))
    }
}

fn parse_mds(path: &Path, bytes: &[u8]) -> Option<MdsTrack> {
    if bytes.len() < 88 || &bytes[..16] != b"MEDIA DESCRIPTOR" || bytes[16] != 1 {
        return None;
    }
    let medium_type = le16(bytes, 0x12)?;
    let num_sessions = le16(bytes, 0x14)?;
    if num_sessions == 0 {
        return None;
    }
    let sessions_offset = le32(bytes, 0x50)? as usize;
    let session = bytes.get(sessions_offset..sessions_offset + 24)?;
    let num_all_blocks = usize::from(session[0x0A]);
    let tracks_offset = le32(session, 0x14)? as usize;
    let mut first: Option<(usize, u8, u64, u64, u64, usize, usize)> = None;
    let mut second_start: Option<u64> = None;
    for i in 0..num_all_blocks {
        let offset = tracks_offset.checked_add(i.checked_mul(80)?)?;
        let block = bytes.get(offset..offset + 80)?;
        let point = block[0x04];
        if point == 0 || point >= 0xA0 {
            continue;
        }
        let start_offset = le64(block, 0x28)?;
        if first.is_none() {
            let mode = block[0x00];
            let extra_offset = le32(block, 0x0C)? as usize;
            let sector_size = u64::from(le16(block, 0x10)?);
            let pregap = if medium_type < 0x10 && extra_offset != 0 {
                le32(bytes, extra_offset).map(u64::from).unwrap_or(0)
            } else {
                0
            };
            let num_files = le32(block, 0x30)? as usize;
            let footer_offset = le32(block, 0x34)? as usize;
            first = Some((
                i,
                mode,
                sector_size,
                pregap,
                start_offset,
                num_files,
                footer_offset,
            ));
        } else {
            second_start = Some(start_offset);
            break;
        }
    }
    let (_, mode, sector_size, pregap, start_offset, num_files, footer_offset) = first?;
    let mut filenames = Vec::new();
    for index in 0..num_files.min(MAX_MDF_PARTS) {
        let Some(off) = footer_offset.checked_add(index * 16) else {
            break;
        };
        let Some(footer) = bytes.get(off..off + 16) else {
            break;
        };
        let Some(name_offset) = le32(footer, 0).map(|v| v as usize) else {
            break;
        };
        let wide = le32(footer, 4).unwrap_or(0) != 0;
        if let Some(file) = decode_mds_filename(path, bytes, name_offset, wide) {
            filenames.push(file);
        }
    }
    let end_offset = second_start.filter(|&end| end > start_offset);
    Some(MdsTrack {
        mode,
        sector_size,
        pregap,
        start_offset,
        end_offset,
        filenames,
    })
}

fn open_mds_media(descriptor: &Path, filenames: &[PathBuf]) -> io::Result<Media> {
    if !filenames.is_empty() {
        let mut files = Vec::with_capacity(filenames.len());
        let mut opened_all = true;
        for name in filenames {
            match File::open(name) {
                Ok(file) => files.push(file),
                Err(_) => {
                    opened_all = false;
                    break;
                }
            }
        }
        if opened_all {
            if filenames.len() == 1 && extension(&filenames[0]).as_deref() == Some("mdf") {
                append_mdf_parts(&filenames[0], &mut files);
            }
            return Media::from_files(files);
        }
    }
    let fallback =
        sibling_path(descriptor, "mdf").unwrap_or_else(|| descriptor.with_extension("mdf"));
    Media::open_mdf(&fallback)
}

fn mds_stride(sector_size: u64) -> Option<u64> {
    match sector_size {
        USER | MODE2_NO_SYNC | RAW | PQ_SUBCHANNEL | SUBCHANNEL => Some(sector_size),
        _ => None,
    }
}

fn mds(path: &Path) -> io::Result<Disc> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(MAX_MDS_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_MDS_BYTES {
        return Err(invalid("MDS descriptor exceeds metadata limit"));
    }
    let Some(track) = parse_mds(path, &bytes) else {
        let fallback = sibling_path(path, "mdf").unwrap_or_else(|| path.with_extension("mdf"));
        return Disc::track(Media::open_mdf(&fallback)?, 0, None, None, &[]);
    };
    if track.mode == 0xA9 {
        return Err(unsupported("first disc track is not a data track"));
    }
    let media = open_mds_media(path, &track.filenames)?;
    let pregaps = if track.pregap != 0 {
        vec![track.pregap]
    } else {
        Vec::new()
    };
    Disc::track(
        media,
        track.start_offset,
        mds_stride(track.sector_size),
        track.end_offset,
        &pregaps,
    )
}

/// Applies a sibling `.mds` when opening `.mdf` directly, if it parses cleanly
/// and refers to `mdf_path` (or names no conflicting existing file).
fn mds_for_mdf(descriptor: &Path, mdf_path: &Path) -> io::Result<Option<Disc>> {
    let mut bytes = Vec::new();
    File::open(descriptor)?
        .take(MAX_MDS_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_MDS_BYTES {
        return Ok(None);
    }
    let Some(track) = parse_mds(descriptor, &bytes) else {
        return Ok(None);
    };
    if let Some(first) = track.filenames.first()
        && let (Ok(a), Ok(b)) = (first.canonicalize(), mdf_path.canonicalize())
        && a != b
    {
        return Ok(None);
    }
    if track.mode == 0xA9 {
        return Err(unsupported("first disc track is not a data track"));
    }
    let media = if track.filenames.len() > 1 {
        open_mds_media(descriptor, &track.filenames)?
    } else {
        Media::open_mdf(mdf_path)?
    };
    let pregaps = if track.pregap != 0 {
        vec![track.pregap]
    } else {
        Vec::new()
    };
    Disc::track(
        media,
        track.start_offset,
        mds_stride(track.sector_size),
        track.end_offset,
        &pregaps,
    )
    .map(Some)
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
                    b"MODE2/2336" => MODE2_NO_SYNC,
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
        Media::single(cue_file(path, name)?)?,
        index * stride,
        Some(stride),
        end.map(|frame| frame * stride),
        &[],
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
        0x03 => Ok((MODE2_NO_SYNC, false)),
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
            return Disc::track(Media::single(file)?, start, Some(stride), end, &[]);
        }
        offset += 8 + length;
    }
    Err(invalid("Nero block count exceeds metadata limit"))
}
