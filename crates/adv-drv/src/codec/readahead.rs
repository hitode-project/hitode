//! Read complete packets from the file without borrowing guest state.
use super::ogg::Packet;
use adv_nt::file::ReadSource;
use std::os::unix::fs::FileExt;

const PAGE_HEADER: usize = 27;
const MAX_PAGE: usize = PAGE_HEADER + 255 * 256;
const PAGES: usize = 4;

#[derive(Clone, Copy)]
pub(super) struct Page {
    identity: [u8; 12],
    len: u32,
    eos: bool,
}

impl Page {
    pub(super) fn new(header: &[u8; PAGE_HEADER], len: u32) -> Option<Self> {
        (header.starts_with(b"OggS\0") && len <= MAX_PAGE as u32).then(|| Self {
            identity: header[14..26].try_into().unwrap(),
            len,
            eos: header[5] & 4 != 0,
        })
    }

    pub(super) fn serial(self) -> u32 {
        word(&self.identity, 0)
    }

    pub(super) fn sequence(self) -> u32 {
        word(&self.identity, 4)
    }

    pub(super) fn tail(self, mut source: ReadSource) -> Tail {
        source.offset += u64::from(self.len);
        Tail {
            source,
            serial: self.serial(),
            sequence: self.sequence().wrapping_add(1),
            eos: self.eos,
        }
    }

    pub(super) fn locate(self, ranges: &[(ReadSource, u32)]) -> Option<ReadSource> {
        let mut bytes = [0; 8192];
        for (source, length) in ranges {
            let mut scanned = 0;
            let length = *length as usize;
            while length - scanned >= PAGE_HEADER {
                let count = bytes.len().min(length - scanned);
                let offset = source.offset + scanned as u64;
                if source
                    .file
                    .read_exact_at(&mut bytes[..count], offset)
                    .is_err()
                {
                    break;
                }
                if let Some(delta) = bytes[..count].windows(PAGE_HEADER).position(|header| {
                    header.starts_with(b"OggS\0") && header[14..26] == self.identity
                }) {
                    return Some(ReadSource {
                        file: source.file.clone(),
                        offset: offset + delta as u64,
                    });
                }
                scanned += count - (PAGE_HEADER - 1);
            }
        }
        None
    }
}

#[derive(Clone)]
pub(super) struct Tail {
    pub source: ReadSource,
    pub serial: u32,
    pub sequence: u32,
    pub eos: bool,
}

pub(super) struct Packets {
    tail: Tail,
    packetno: i64,
    partial: Vec<u8>,
    bos: u32,
    page: Vec<u8>,
    segment: usize,
    body: usize,
    last_packet: Option<usize>,
    stopped: bool,
}

impl Packets {
    pub(super) fn new(tail: Tail, packetno: i64, partial: Vec<u8>, bos: u32) -> Self {
        Self {
            tail,
            packetno,
            partial,
            bos,
            page: Vec::new(),
            segment: 0,
            body: 0,
            last_packet: None,
            stopped: false,
        }
    }

    pub(super) fn next(&mut self) -> Option<Packet> {
        if self.stopped {
            return None;
        }
        let packet = self.read_packet();
        self.stopped = packet.is_none();
        packet
    }

    fn read_packet(&mut self) -> Option<Packet> {
        let mut pages = 0;
        loop {
            if self.page.is_empty() || self.segment == self.page[26] as usize {
                if pages == PAGES || !self.page.is_empty() && self.page[5] & 4 != 0 {
                    return None;
                }
                self.read_page()?;
                pages += 1;
                if word(&self.page, 14) != self.tail.serial {
                    self.page.clear();
                    continue;
                }
                if word(&self.page, 18) != self.tail.sequence
                    || (self.page[5] & 1 != 0) == self.partial.is_empty()
                {
                    return None;
                }
                self.tail.sequence = self.tail.sequence.wrapping_add(1);
            }

            let flags = self.page[5];
            let segments = self.page[26] as usize;
            if flags & 3 == 2 && self.segment == 0 && segments != 0 {
                self.bos = 0x100;
            }
            while self.segment < segments {
                let segment = self.segment;
                let size = self.page[PAGE_HEADER + segment] as usize;
                if self.partial.len() + size > (PAGES - 1) * MAX_PAGE {
                    return None;
                }
                self.partial
                    .extend_from_slice(&self.page[self.body..self.body + size]);
                self.body += size;
                self.segment += 1;
                if size != 255 {
                    let packet = Packet {
                        bytes: std::mem::take(&mut self.partial),
                        bos: std::mem::take(&mut self.bos),
                        eos: u32::from(flags & 4 != 0 && self.segment == segments) * 0x200,
                        granulepos: if Some(segment) == self.last_packet {
                            i64::from_le_bytes(self.page[6..14].try_into().unwrap())
                        } else {
                            -1
                        },
                        packetno: self.packetno,
                    };
                    self.packetno = self.packetno.wrapping_add(1);
                    return Some(packet);
                }
            }
        }
    }

    fn read_page(&mut self) -> Option<()> {
        let source = &self.tail.source;
        self.page.reserve_exact(MAX_PAGE - self.page.len());
        self.page.resize(PAGE_HEADER, 0);
        source
            .file
            .read_exact_at(&mut self.page, source.offset)
            .ok()?;
        if &self.page[..5] != b"OggS\0" {
            return None;
        }
        let header = PAGE_HEADER + self.page[26] as usize;
        self.page.resize(header, 0);
        source
            .file
            .read_exact_at(
                &mut self.page[PAGE_HEADER..],
                source.offset + PAGE_HEADER as u64,
            )
            .ok()?;
        let laces = &self.page[PAGE_HEADER..];
        self.last_packet = laces.iter().rposition(|&lace| lace != 255);
        let size = header + laces.iter().map(|&lace| lace as usize).sum::<usize>();
        self.page.resize(size, 0);
        source
            .file
            .read_exact_at(&mut self.page[header..], source.offset + header as u64)
            .ok()?;
        let expected = word(&self.page, 22);
        self.page[22..26].fill(0);
        if crc(&self.page) != expected {
            return None;
        }
        self.tail.source.offset += size as u64;
        self.segment = 0;
        self.body = header;
        Some(())
    }
}

pub(super) fn word(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

pub(super) fn crc(bytes: &[u8]) -> u32 {
    const TABLE: [u32; 256] = {
        let mut table = [0; 256];
        let mut n = 0;
        while n < table.len() {
            let mut value = (n as u32) << 24;
            let mut bit = 0;
            while bit < 8 {
                value = (value << 1)
                    ^ if value & 0x8000_0000 != 0 {
                        0x04c1_1db7
                    } else {
                        0
                    };
                bit += 1;
            }
            table[n] = value;
            n += 1;
        }
        table
    };
    bytes.iter().fold(0u32, |crc, &byte| {
        (crc << 8) ^ TABLE[((crc >> 24) as u8 ^ byte) as usize]
    })
}
