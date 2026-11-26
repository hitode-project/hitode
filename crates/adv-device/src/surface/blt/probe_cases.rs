use super::{PixelFormat, Rect};
use PixelFormat::{Bgra32, Indexed8, Rgb565};

pub fn rectangle(x: u32, y: u32, w: u32, h: u32) -> Rect {
    Rect { x, y, w, h }
}

pub struct Pixels {
    pub format: PixelFormat,
    pub size: (u32, u32),
    pub data: Vec<u32>,
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

    pub fn source(format: PixelFormat, size: (u32, u32)) -> Self {
        Self::new(format, size, |x, y| {
            if format == Indexed8 {
                y * 16 + x
            } else {
                y << 8 | x
            }
        })
    }

    pub fn solid(format: PixelFormat, size: (u32, u32), value: u32) -> Self {
        Self::new(format, size, |_, _| value)
    }
}

fn dimensions(value: &str) -> (u32, u32) {
    value.split_once('x').map_or_else(
        || (value.parse().unwrap(), 1),
        |(w, h)| (w.parse().unwrap(), h.parse().unwrap()),
    )
}

pub struct Case {
    pub source: Pixels,
    pub destination: Pixels,
    pub source_rect: Rect,
    pub destination_rect: Rect,
    pub mirror: [bool; 2],
    pub key: Option<u32>,
    pub fill: Option<u32>,
    pub self_copy: bool,
    pub destination_key: bool,
}

impl Case {
    pub fn copy(source: Pixels, destination: Pixels) -> Self {
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

    pub fn stretch(source: (u32, u32), destination: (u32, u32)) -> Self {
        Self::copy(
            Pixels::source(Bgra32, source),
            Pixels::solid(Bgra32, destination, 0xeeeeee),
        )
    }

    pub fn from_probe(name: &str, key: Option<u32>) -> Self {
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

pub const EXPECTATIONS: &str = include_str!("expectations.tsv");

/// One expectation column per declared memory-class pair.
pub fn rows(
    dump: &'static str,
    pair: usize,
) -> impl Iterator<Item = (&'static str, u32, Option<&'static str>)> {
    dump.lines()
        .filter(|line| !line.starts_with('#'))
        .filter_map(move |line| {
            let (name, result) = line.split_once('\t').unwrap();
            let fields: Vec<_> = result.split('\t').collect();
            let expected = if fields.len() == 1 {
                fields[0]
            } else {
                fields[pair]
            };
            if expected == "-" {
                return None;
            }
            let expected = expected
                .strip_prefix('=')
                .map_or(expected, |i| fields[i.parse::<usize>().unwrap()]);
            let (value, output) = expected
                .split_once(':')
                .map_or((expected, None), |(value, output)| (value, Some(output)));
            Some((name, u32::from_str_radix(value, 16).unwrap(), output))
        })
}
