//! Wine's wg_format and WAVEFORMATEX / MFVIDEOFORMAT layouts, from Wine 11.
use super::*;
use adv_device::media::CodecFormat;

pub(super) fn word(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}
fn put(bytes: &mut [u8], at: usize, value: u32) {
    bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
}
fn short(bytes: &mut [u8], at: usize, value: u16) {
    bytes[at..at + 2].copy_from_slice(&value.to_le_bytes());
}
fn extra(format: &mut [u8], length: usize, at: usize, bytes: &[u8]) {
    let size = bytes.len().min(64);
    put(format, length, size as u32);
    format[at..at + size].copy_from_slice(&bytes[..size]);
}
pub(super) fn wg(codec: &CodecFormat, compressed: bool) -> [u8; 120] {
    let mut out = [0; 120];
    match codec {
        CodecFormat::Video {
            fourcc,
            width,
            height,
            frame_100ns,
            extradata,
            ..
        } => {
            let major = if !compressed {
                5
            } else {
                match *fourcc {
                    0x31564d57 | 0x32564d57 | 0x33564d57 | 0x31435657 | 0x41564d57 => 8,
                    0x34363248 | 0x31637661 => 7,
                    0x64697663 => 6,
                    _ => 10,
                }
            };
            put(&mut out, 0, major);
            put(
                &mut out,
                4,
                if major == 5 {
                    8
                } else if major == 8 {
                    match *fourcc {
                        0x31564d57 => 15,
                        0x32564d57 => 16,
                        0x33564d57 => 17,
                        0x41564d57 => 18,
                        _ => 19,
                    }
                } else {
                    0
                },
            );
            put(&mut out, 8, *width);
            put(&mut out, 12, *height);
            put(
                &mut out,
                16,
                if *frame_100ns == 0 { 25 } else { 10_000_000 },
            );
            put(
                &mut out,
                20,
                if *frame_100ns == 0 {
                    1
                } else {
                    *frame_100ns as u32
                },
            );
            extra(&mut out, 52, 56, extradata);
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
            let major = if !compressed {
                1
            } else {
                match *format_tag {
                    0x50 | 0x55 => 2,
                    0xff | 0x1610 => 3,
                    0x160..=0x164 => 4,
                    _ => 1,
                }
            };
            put(&mut out, 0, major);
            put(&mut out, 4, if major == 1 { 2 } else { 0 });
            put(&mut out, 8, u32::from(*channels));
            put(&mut out, 16, *rate);
            put(&mut out, 20, *bytes_per_second * 8);
            put(&mut out, 24, u32::from(*bits));
            put(&mut out, 28, u32::from(*block_align));
            put(
                &mut out,
                32,
                if major == 4 {
                    u32::from(*format_tag) - 0x15f
                } else {
                    0
                },
            );
            put(&mut out, 36, if *format_tag == 0x55 { 3 } else { 2 });
            extra(&mut out, 44, 48, extradata);
        }
    }
    out
}
pub(super) fn output(format: &[u8], align: u32) -> Output {
    match word(format, 0) {
        1 => Output::Audio {
            sample: word(format, 4),
            channels: word(format, 8),
            rate: word(format, 16),
        },
        5 => Output::Video {
            pixel: word(format, 4),
            width: word(format, 8),
            height: word(format, 12) as i32,
            align,
        },
        _ => Output::Compressed,
    }
}
const BASE: [u8; 12] = [0, 0, 0x10, 0, 0x80, 0, 0, 0xaa, 0, 0x38, 0x9b, 0x71];
const AUDIO: [u8; 16] = [
    0x61, 0x75, 0x64, 0x73, 0, 0, 0x10, 0, 0x80, 0, 0, 0xaa, 0, 0x38, 0x9b, 0x71,
];
const VIDEO: [u8; 16] = [
    0x76, 0x69, 0x64, 0x73, 0, 0, 0x10, 0, 0x80, 0, 0, 0xaa, 0, 0x38, 0x9b, 0x71,
];

pub(super) fn media(codec: &CodecFormat) -> (Vec<u8>, Vec<u8>) {
    match codec {
        CodecFormat::Audio {
            format_tag,
            rate,
            channels,
            block_align,
            bits,
            bytes_per_second,
            extradata,
        } => {
            let aac = matches!(*format_tag, 0xff | 0x1610);
            let header = if aac { 30 } else { 18 };
            let mut out = vec![0; header + extradata.len()];
            short(&mut out, 0, if aac { 0x1610 } else { *format_tag });
            short(&mut out, 2, *channels);
            put(&mut out, 4, *rate);
            put(&mut out, 8, *bytes_per_second);
            short(&mut out, 12, *block_align);
            short(&mut out, 14, *bits);
            let extra = (out.len() - 18) as u16;
            short(&mut out, 16, extra);
            out[header..].copy_from_slice(extradata);
            (AUDIO.to_vec(), out)
        }
        CodecFormat::Video {
            fourcc,
            width,
            height,
            frame_100ns,
            extradata,
            ..
        } => {
            let mut out = vec![0; 176 + extradata.len()];
            let size = out.len();
            put(&mut out, 0, size as u32);
            put(&mut out, 8, *width);
            put(&mut out, 12, *height);
            put(&mut out, 16, 1);
            put(&mut out, 20, 1);
            put(&mut out, 48, 10_000_000);
            put(&mut out, 52, *frame_100ns as u32);
            put(&mut out, 120, *fourcc);
            out[124..136].copy_from_slice(&BASE);
            out[176..].copy_from_slice(extradata);
            (VIDEO.to_vec(), out)
        }
    }
}
pub(super) fn parse_media(
    env: &Env<'_>,
    at: u32,
) -> Result<(CodecFormat, Output, Vec<u8>, Vec<u8>), String> {
    let major = bytes(env, at, 16)?;
    let size = read(env, at + 16)? as usize;
    let data = bytes(env, read(env, at + 20)?, size)?;
    if major == AUDIO {
        let tag = word(&data, 0) as u16;
        let channels = word(&data, 0) >> 16;
        let rate = word(&data, 4);
        let bits = u32::from(u16::from_le_bytes(data[14..16].try_into().unwrap()));
        let codec = CodecFormat::Audio {
            format_tag: tag,
            channels: channels as u16,
            rate,
            block_align: word(&data, 12) as u16,
            bits: bits as u16,
            bytes_per_second: word(&data, 8),
            extradata: data[if tag == 0x1610 { 30 } else { 18 }..].to_vec(),
        };
        let sample = match (tag, bits) {
            (1, 8) => 1,
            (1, 16) => 2,
            (1, 24) => 3,
            (1, 32) => 4,
            (3, 32) => 5,
            (3, 64) => 6,
            _ => 0,
        };
        Ok((
            codec,
            Output::Audio {
                sample,
                channels,
                rate,
            },
            major,
            data,
        ))
    } else if major == VIDEO {
        let width = word(&data, 8);
        let mut height = word(&data, 12) as i32;
        if word(&data, 112) & 0x0008_0000 != 0 {
            height = -height.abs();
        }
        let fourcc = word(&data, 120);
        let codec = CodecFormat::Video {
            fourcc,
            width,
            height: height.unsigned_abs(),
            bit_count: 24,
            frame_100ns: if word(&data, 48) == 0 {
                0
            } else {
                u64::from(word(&data, 52)) * 10_000_000 / u64::from(word(&data, 48))
            },
            extradata: if fourcc == 0xe436eb81 {
                data.get(200..).unwrap_or_default().to_vec()
            } else {
                data[176..].to_vec()
            },
        };
        let pixel = match fourcc {
            0x3231564e => 9,
            0x30323449 => 8,
            0x32315659 => 13,
            0x32595559 => 12,
            0x59565955 => 11,
            0x00000016 => 2,
            0x00000015 => 1,
            0x00000014 => 3,
            0x00000017 => 5,
            0x00000018 => 4,
            _ => 0,
        };
        Ok((
            codec,
            Output::Video {
                pixel,
                width,
                height,
                align: 0,
            },
            major,
            data,
        ))
    } else {
        Err("unsupported media major type".into())
    }
}
