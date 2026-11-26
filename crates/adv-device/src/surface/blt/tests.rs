use super::*;
use MemoryClass::{System, Video};
use PixelFormat::{Bgra32, Indexed8, Rgb565};

struct Memory(Vec<u8>);

impl SurfaceMemory for Memory {
    fn read(&self, addr: u32, len: usize) -> Result<Cow<'_, [u8]>, String> {
        let at = addr as usize;
        Ok(Cow::Borrowed(&self.0[at..at + len]))
    }

    fn write(&mut self, addr: u32, data: &[u8]) -> Result<(), String> {
        let at = addr as usize;
        self.0[at..at + data.len()].copy_from_slice(data);
        Ok(())
    }

    fn writable(&self, _: u32, _: usize) -> Result<(), String> {
        Ok(())
    }

    fn copy(&mut self, src: u32, dst: u32, len: usize) -> Result<(), String> {
        self.0
            .copy_within(src as usize..src as usize + len, dst as usize);
        Ok(())
    }
}

fn rectangle(x: u32, y: u32, w: u32, h: u32) -> Rect {
    Rect { x, y, w, h }
}

struct Pixels {
    format: PixelFormat,
    size: (u32, u32),
    data: Vec<u32>,
}

impl Pixels {
    fn new(format: PixelFormat, size: (u32, u32), pixel: impl Fn(u32, u32) -> u32) -> Self {
        let mut data = Vec::new();
        for y in 0..size.1 {
            for x in 0..size.0 {
                data.push(pixel(x, y));
            }
        }
        Self { format, size, data }
    }

    fn source(format: PixelFormat, size: (u32, u32)) -> Self {
        Self::new(format, size, |x, y| {
            if format == Indexed8 {
                y * 16 + x
            } else {
                y << 8 | x
            }
        })
    }

    fn solid(format: PixelFormat, size: (u32, u32), value: u32) -> Self {
        Self::new(format, size, |_, _| value)
    }

    fn load(&self, store: &mut SurfaceStore, m: &mut Memory) -> u32 {
        let (w, h) = self.size;
        let id = store.create(w, h, false, self.format, m).unwrap();
        let layout = &store[id];
        let bytes = self.format.bytes() as usize;
        for y in 0..h {
            for x in 0..w {
                let raw = self.data[(y * w + x) as usize].to_le_bytes();
                m.write(layout.row(x, y), &raw[..bytes]).unwrap();
            }
        }
        id
    }
}

fn dimensions(value: &str) -> (u32, u32) {
    value.split_once('x').map_or_else(
        || (value.parse().unwrap(), 1),
        |(w, h)| (w.parse().unwrap(), h.parse().unwrap()),
    )
}

fn hresult(error: BltError) -> u32 {
    match error {
        BltError::InvalidRect => 0x8876_0096,
        BltError::UnsupportedConversion => 0x8000_4001,
        BltError::ConversionFailed => 0x8000_4005,
        BltError::NoColorKeyHardware => 0x8876_00dc,
        BltError::InvalidArgument => 0x8007_0057,
    }
}

fn result_code(result: Result<Rect, BltFailure>) -> u32 {
    match result {
        Ok(_) => 0,
        Err(BltFailure::Rule(error)) => hresult(error),
        Err(BltFailure::Storage(error)) => panic!("{error}"),
    }
}

struct Case {
    source: Pixels,
    destination: Pixels,
    source_rect: Rect,
    destination_rect: Rect,
    mirror: [bool; 2],
    key: Option<u32>,
    fill: Option<u32>,
    self_copy: bool,
    destination_key: bool,
}

impl Case {
    fn copy(source: Pixels, destination: Pixels) -> Self {
        Self {
            source_rect: rectangle(0, 0, source.size.0, source.size.1),
            destination_rect: rectangle(0, 0, destination.size.0, destination.size.1),
            source,
            destination,
            mirror: [false; 2],
            key: None,
            fill: None,
            self_copy: false,
            destination_key: false,
        }
    }

    fn stretch(source: (u32, u32), destination: (u32, u32)) -> Self {
        Self::copy(
            Pixels::source(Bgra32, source),
            Pixels::solid(Bgra32, destination, 0xeeeeee),
        )
    }

    fn from_probe(name: &str, key: Option<u32>) -> Self {
        if let Some((src, dst)) = name
            .strip_prefix("sweep-")
            .or_else(|| name.strip_prefix("stretch-"))
            .and_then(|sizes| sizes.split_once("-to-"))
        {
            return Self::stretch(dimensions(src), dimensions(dst));
        }
        let small = (4, 2);
        if name.starts_with("convert-") || name.starts_with("copy-8") {
            let (source, dst, clear) = if name.starts_with("convert-565") {
                (
                    Pixels {
                        format: Rgb565,
                        size: small,
                        data: vec![
                            0xffff, 0x8410, 0x7bef, 0x0841, 0xf800, 0x07e0, 0x001f, 0x4208,
                        ],
                    },
                    Bgra32,
                    0xeeeeee,
                )
            } else if name.starts_with("convert-x888") {
                (
                    Pixels {
                        format: Bgra32,
                        size: small,
                        data: vec![
                            0xffffff, 0x808080, 0x7f7f7f, 0x040404, 0x070707, 0xf8fcf8, 0x0c0c0c,
                            0x84c284,
                        ],
                    },
                    Rgb565,
                    0,
                )
            } else if name.starts_with("convert-8") {
                (Pixels::source(Indexed8, small), Bgra32, 0xeeeeee)
            } else {
                (Pixels::source(Indexed8, small), Indexed8, 0)
            };
            return Self::copy(source, Pixels::solid(dst, small, clear));
        }
        let mut case = Self::stretch(small, small);
        match name {
            "mirror-lr-4x2" => case.mirror[0] = true,
            "mirror-ud-4x2" => case.mirror[1] = true,
            "mirror-both-3x2-to-6x2" => {
                case = Self::stretch((3, 2), (6, 2));
                case.mirror = [true; 2];
            }
            "keysrc-0x0101-stretch-4x2-to-8x4"
            | "keysrc-range-0x0001-0x0102"
            | "keysrc-0xff000101-x8-bits"
            | "keysrcoverride-0x0002" => {
                case = Self::stretch(small, (8, 4));
                case.key = if name.starts_with("keysrcoverride") {
                    Some(2)
                } else {
                    key
                };
            }
            "keydest-0x123456-at-(1,0),(2,1)" => {
                case = Self::stretch(small, (8, 4));
                case.destination_key = true;
                case.destination.data[1] = 0x123456;
                case.destination.data[10] = 0x123456;
            }
            "colorfill-32-0xff123456-rect-1,0,3,1"
            | "colorfill-32-rect-past-edge"
            | "colorfill-16-0x12345678-rect"
            | "colorfill-8-0x1234-rect" => {
                let (format, fill) = if name.starts_with("colorfill-16") {
                    (Rgb565, 0x12345678)
                } else if name.starts_with("colorfill-8") {
                    (Indexed8, 0x1234)
                } else {
                    (Bgra32, 0xff123456)
                };
                case.destination = Pixels::solid(format, small, 0);
                case.fill = Some(fill);
                case.destination_rect = rectangle(1, 0, 2, 1);
                if name.ends_with("past-edge") {
                    case.destination.data[1..3].fill(0xff123456);
                    case.destination_rect = rectangle(2, 0, 4, 2);
                }
            }
            "overlap-same-surface-0..6-to-2..8" | "overlap-same-surface-2..8-to-0..6" => {
                case.self_copy = true;
                case.destination = Pixels::source(Bgra32, (8, 2));
                let x = if name.ends_with("to-2..8") { 0 } else { 2 };
                case.source_rect = rectangle(x, 0, 6, 2);
                case.destination_rect = rectangle(2 - x, 0, 6, 2);
            }
            "inverted-dest-rect" => case.destination_rect = rectangle(3, 0, 0, 2),
            "dest-rect-past-edge" => case.destination_rect = rectangle(0, 0, 9, 2),
            "src-rect-past-edge" => case.source_rect = rectangle(0, 0, 9, 2),
            "bltfast-srccolorkey-0x0001-at-1,0" | "bltfast-past-edge-at-7,0" => {
                case = Self::stretch(small, (8, 2));
                case.source_rect = rectangle(0, 0, 2, 2);
                case.destination_rect = rectangle(1, 0, 2, 2);
                case.key = Some(1);
                if name.ends_with("7,0") {
                    case.destination_rect.x = 7;
                    case.destination.data[1] = 0;
                    case.destination.data[9] = 0x100;
                    case.destination.data[10] = 0x101;
                }
            }
            _ => panic!("unknown probe case {name}"),
        }
        case
    }
}

fn pixels(layout: &Surface, memory: &Memory) -> Vec<u32> {
    let bytes = layout.format.bytes() as usize;
    let mut result = Vec::new();
    for y in 0..layout.height {
        let row = layout.read_pixels(memory, 0, y, layout.width).unwrap();
        for pixel in row.chunks_exact(bytes) {
            let mut extended = [0; 4];
            extended[..bytes].copy_from_slice(pixel);
            if layout.format == Bgra32 {
                extended[3] = 0; // The probe prints X8R8G8B8 under its color mask.
            }
            result.push(u32::from_le_bytes(extended));
        }
    }
    result
}

fn probe(resident: bool) {
    let mut store = SurfaceStore::new(16, 16, 16, 16384, Snapshot::default()).unwrap();
    let mut memory = Memory(vec![0; 16400]);
    let pairs = [
        (Video, Video),
        (System, System),
        (System, Video),
        (Video, System),
    ];
    for (index, (src_class, dst_class)) in pairs.into_iter().enumerate() {
        let rules = BltRules::for_memory(src_class, dst_class);
        let mut key = Some(0x0101);
        for line in include_str!("expectations.tsv")
            .lines()
            .filter(|line| !line.starts_with('#'))
        {
            let mut fields = line.split('\t');
            let name = fields.next().unwrap();
            if name == "hal" || name == "hel" {
                assert_eq!(
                    u32::from_str_radix(fields.next().unwrap(), 16).unwrap(),
                    u32::from(rules.supports_key(BltKey::Source)) << 9
                );
                continue;
            }
            let fields: Vec<_> = fields.collect();
            let expected = fields[index];
            let expected = expected
                .strip_prefix('=')
                .map_or(expected, |i| fields[i.parse::<usize>().unwrap()]);
            let (expected, output) = expected
                .split_once(':')
                .map_or((expected, None), |(hr, pixels)| (hr, Some(pixels)));
            let expected = u32::from_str_radix(expected, 16).unwrap();
            let label = (name, src_class, dst_class, resident);
            if name.starts_with("setcolorkey") {
                let result = match name {
                    "setcolorkey-range" => {
                        rules.set_source_key(&mut key, Some(1), BltKey::SourceRange)
                    }
                    "setcolorkey-0xff000101" => {
                        rules.set_source_key(&mut key, Some(0xff000101), BltKey::Source)
                    }
                    "setcolorkey-destblt" => {
                        rules.set_source_key(&mut key, Some(0x123456), BltKey::Destination)
                    }
                    _ => unreachable!(),
                };
                assert_eq!(result.err().map_or(0, hresult), expected, "{label:?}");
                continue;
            }
            if name.starts_with("getcolorkey") {
                assert_eq!(key, Some(expected), "{label:?}");
                assert_eq!(
                    key,
                    Some(u32::from_str_radix(output.unwrap(), 16).unwrap()),
                    "{label:?}"
                );
                continue;
            }
            let case = Case::from_probe(name, key);
            let dst = case.destination.load(&mut store, &mut memory);
            let src = if case.self_copy {
                dst
            } else {
                case.source.load(&mut store, &mut memory)
            };
            let source = BltSurface {
                id: src,
                rect: case.source_rect,
                memory: if case.self_copy { dst_class } else { src_class },
            };
            let destination = BltSurface {
                id: dst,
                rect: case.destination_rect,
                memory: dst_class,
            };
            let operation = if let Some(fill) = case.fill {
                Blt::Fill(fill)
            } else if case.destination_key {
                Blt::DestinationKey
            } else {
                Blt::Copy {
                    source,
                    mirror: case.mirror,
                    key: case.key,
                }
            };
            #[cfg(feature = "gpu")]
            if resident {
                for id in [src, dst] {
                    let full = store[id].rect();
                    store.gpu_access(id, full, Access::Read, &memory).unwrap();
                }
            }
            let result = result_code(store.blt(destination, operation, &mut memory));
            assert_eq!(result, expected, "{label:?}");
            let full = store[dst].rect();
            let layout = store
                .cpu_access(dst, full, Access::Read, &mut memory)
                .unwrap();
            let expected = output.map_or_else(
                || case.destination.data.clone(),
                |output| {
                    output
                        .split('.')
                        .map(|p| u32::from_str_radix(p, 16).unwrap())
                        .collect()
                },
            );
            assert_eq!(pixels(layout, &memory), expected, "{label:?}");
            store.delete(src);
            if dst != src {
                store.delete(dst);
            }
        }
    }
}

// spec: GFX-BLT-1
#[test]
fn cpu_matches_all_windows_blt_rows() {
    probe(false);
}

// spec: GFX-BLT-1
#[cfg(feature = "gpu")]
#[test]
fn gpu_matches_all_windows_blt_rows() {
    probe(true);
}

// spec: GFX-BLT-1 DEV-FRAME-13
#[cfg(feature = "gpu")]
#[test]
fn a_cpu_write_selects_the_cpu_until_its_cells_are_uploaded() {
    let mut store = SurfaceStore::new(24, 8, 16, 16384, Snapshot::default()).unwrap();
    let mut memory = Memory(vec![0; 16400]);
    let src = Pixels::source(Bgra32, (16, 8)).load(&mut store, &mut memory);
    let dst = Pixels::solid(Bgra32, (24, 8), 0).load(&mut store, &mut memory);
    let source = BltSurface {
        id: src,
        rect: store[src].rect(),
        memory: System,
    };
    let destination = BltSurface {
        id: dst,
        rect: store[dst].rect(),
        memory: System,
    };
    let stretch = Blt::Copy {
        source,
        mirror: [false; 2],
        key: None,
    };
    for blt in [source, destination] {
        store
            .gpu_access(blt.id, blt.rect, Access::Read, &memory)
            .unwrap();
    }
    let pixel = rectangle(0, 0, 1, 1);
    let layout = store
        .cpu_access(src, pixel, Access::Write, &mut memory)
        .unwrap();
    memory.put_u32(layout.pixels, 0xff112233).unwrap();
    let draws = store.gpu.draw_count();
    store.blt(destination, stretch, &mut memory).unwrap();
    assert_eq!(store.gpu.draw_count(), draws);
    let cpu = pixels(&store[dst], &memory);
    assert_eq!(cpu[0], 0x112233);

    for blt in [source, destination] {
        store
            .gpu_access(blt.id, blt.rect, Access::Read, &memory)
            .unwrap();
    }
    store.blt(destination, stretch, &mut memory).unwrap();
    assert_eq!(store.gpu.draw_count(), draws + 1);
    let layout = store
        .cpu_access(dst, destination.rect, Access::Read, &mut memory)
        .unwrap();
    assert_eq!(pixels(layout, &memory), cpu);
}
