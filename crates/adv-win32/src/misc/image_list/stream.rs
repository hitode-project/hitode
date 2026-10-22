//! Legacy common-controls ILHEAD/BMP streams, read through the guest IStream.
//! No host COM identity or borrow survives a guest callback.
use super::{Image, ImageList, ImageLists, MAX_DIMENSION, MAX_ENTRIES, MAX_PIXEL_BYTES, fail};
use adv_abi::guest::check_range;
use adv_cpu::backend::{GuestExt, Prot, Reg};
use adv_device::surface::PixelFormat;
use adv_machine::{Env, Outcome};

const API: &str = "comctl32.dll!ImageList_Read";
const CHUNK: usize = 4096;
type BytesThen = Box<dyn FnOnce(&mut Env<'_>, Vec<u8>) -> Outcome>;
type BitmapThen = Box<dyn FnOnce(&mut Env<'_>, Bitmap) -> Outcome>;

fn word(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap())
}
fn dword(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}
fn error(env: &mut Env<'_>, code: u32) -> Outcome {
    Outcome::Return(fail(env, code, 0) as u32)
}
fn unsupported(env: &mut Env<'_>, reason: &str) -> Outcome {
    adv_abi::warn_unavailable(env, API, reason);
    error(env, 50)
}
fn method(env: &Env<'_>, stream: u32) -> Option<u32> {
    check_range(env.cpu, stream, 4, Prot::READ).ok()?;
    let table = env.cpu.read_u32(stream).ok()?;
    let address = table.checked_add(12)?; // ISequentialStream::Read follows IUnknown.
    check_range(env.cpu, address, 4, Prot::READ).ok()?;
    let target = env.cpu.read_u32(address).ok()?;
    check_range(env.cpu, target, 1, Prot::EXEC).ok()?;
    Some(target)
}

fn exact(env: &mut Env<'_>, stream: u32, size: usize, then: BytesThen) -> Outcome {
    let mut bytes = Vec::new();
    if bytes.try_reserve_exact(size).is_err() {
        return error(env, 8);
    }
    bytes.resize(size, 0);
    if size == 0 {
        return then(env, bytes);
    }
    let length = size.min(CHUNK) as u32;
    let Some(buffer) = env
        .cpu
        .reg(Reg::Esp)
        .checked_sub(length + 15)
        .map(|sp| sp & !15)
    else {
        return error(env, 8);
    };
    if buffer < env.ctx().stack_lo || !env.cpu.accessible(buffer, length, Prot::RW) {
        return error(env, 8);
    }
    // Scratch belongs to the current callback stack. Dropping a continuation on
    // thread exit releases its lifetime without leaking a process-heap block.
    chunk(env, stream, buffer, bytes, 0, then)
}

fn chunk(
    env: &mut Env<'_>,
    stream: u32,
    buffer: u32,
    mut bytes: Vec<u8>,
    offset: usize,
    then: BytesThen,
) -> Outcome {
    let length = (bytes.len() - offset).min(CHUNK);
    let Some(target) = method(env, stream) else {
        return error(env, 998);
    };
    if check_range(env.cpu, buffer, length as u32, Prot::RW).is_err()
        || env
            .cpu
            .write(buffer, &bytes[offset..offset + length])
            .is_err()
    {
        return error(env, 998);
    }
    Outcome::call_on_stack(
        target,
        [stream, buffer, length as u32, 0],
        buffer,
        move |env, result| {
            // The native reader passes NULL for pcbRead and accepts S_FALSE.
            // Zeroed scratch makes an unwritten tail deterministic and safe.
            let copied = result.value() as i32 >= 0
                && check_range(env.cpu, buffer, length as u32, Prot::READ).is_ok()
                && env
                    .cpu
                    .read(buffer, &mut bytes[offset..offset + length])
                    .is_ok();
            if !copied || offset + length == bytes.len() {
                if copied {
                    then(env, bytes)
                } else {
                    Outcome::Return(0)
                }
            } else {
                chunk(env, stream, buffer, bytes, offset + length, then)
            }
        },
    )
}

struct Header {
    width: u32,
    height: u32,
    count: usize,
    capacity: usize,
    grow: usize,
    mask: bool,
    extended: bool,
    background: u32,
}

pub(super) fn read(env: &mut Env<'_>, stream: u32) -> Outcome {
    if method(env, stream).is_none() {
        return error(env, 998);
    }
    exact(
        env,
        stream,
        28,
        Box::new(move |env, bytes| {
            if word(&bytes, 0) != 0x4c49 {
                return Outcome::Return(0);
            }
            if word(&bytes, 2) != 0x101 {
                return unsupported(
                    env,
                    "only the legacy 0x101 image-list stream format is supported",
                );
            }
            let header = Header {
                count: word(&bytes, 4) as usize,
                capacity: word(&bytes, 6) as usize,
                grow: (word(&bytes, 8) as usize).max(1),
                width: u32::from(word(&bytes, 10)),
                height: u32::from(word(&bytes, 12)),
                mask: word(&bytes, 18) & 1 != 0,
                extended: word(&bytes, 18) & 0x1000 != 0,
                background: dword(&bytes, 14),
            };
            if header.width == 0 || header.height == 0 || header.count > header.capacity {
                return Outcome::Return(0);
            }
            if header.width > MAX_DIMENSION as u32
                || header.height > MAX_DIMENSION as u32
                || header.capacity > MAX_ENTRIES
                || header.grow > MAX_ENTRIES
                || u64::from(header.width)
                    * u64::from(header.height)
                    * header.count as u64
                    * if header.mask { 5 } else { 4 }
                    > MAX_PIXEL_BYTES as u64
            {
                return error(env, 8);
            }
            if word(&bytes, 18) & 0xa000 != 0 {
                return unsupported(
                    env,
                    "mirrored/high-flag image-list stream semantics are unavailable",
                );
            }
            bitmap(
                env,
                stream,
                false,
                MAX_PIXEL_BYTES,
                Box::new(move |env, color| {
                    if header.mask {
                        let remaining = MAX_PIXEL_BYTES - color.bits.len();
                        bitmap(
                            env,
                            stream,
                            true,
                            remaining,
                            Box::new(move |env, mask| tail(env, stream, header, color, Some(mask))),
                        )
                    } else {
                        tail(env, stream, header, color, None)
                    }
                }),
            )
        }),
    )
}

fn tail(
    env: &mut Env<'_>,
    stream: u32,
    header: Header,
    color: Bitmap,
    mask: Option<Bitmap>,
) -> Outcome {
    if header.extended {
        // The v5 writer's internal 0x1000 flag carries a 22-byte trailer.
        // Native reads it even at EOF; it does not change the decoded pixels.
        exact(
            env,
            stream,
            22,
            Box::new(move |env, _| finish(env, header, color, mask)),
        )
    } else {
        finish(env, header, color, mask)
    }
}

struct Bitmap {
    width: usize,
    height: usize,
    bottom_up: bool,
    bpp: u16,
    stride: usize,
    colors: Vec<u8>,
    bits: Vec<u8>,
}

fn bitmap(env: &mut Env<'_>, stream: u32, mask: bool, budget: usize, then: BitmapThen) -> Outcome {
    // Serialized common-controls BMPs are consecutive headers/palette/bits.
    // bfSize/bfOffBits are not reliable total lengths (including native writers).
    exact(
        env,
        stream,
        14,
        Box::new(move |env, file| {
            if word(&file, 0) != 0x4d42 {
                return Outcome::Return(0);
            }
            exact(
                env,
                stream,
                40,
                Box::new(move |env, info| {
                    if dword(&info, 0) != 40 {
                        return unsupported(
                            env,
                            "only BITMAPINFOHEADER image-list bitmaps are supported",
                        );
                    }
                    let width = dword(&info, 4) as i32;
                    let height = dword(&info, 8) as i32;
                    let bpp = word(&info, 14);
                    if width <= 0 || height == 0 || height == i32::MIN || word(&info, 12) != 1 {
                        return Outcome::Return(0);
                    }
                    if dword(&info, 16) != 0
                        || !matches!(bpp, 1 | 4 | 8 | 16 | 24 | 32)
                        || (mask && bpp != 1)
                    {
                        return unsupported(
                            env,
                            "only uncompressed 1/4/8/16/24/32-bit color and 1-bit mask BMPs are supported",
                        );
                    }
                    let width = width as usize;
                    let height_abs = height.unsigned_abs() as usize;
                    let Some(stride) = width
                        .checked_mul(bpp as usize)
                        .and_then(|bits| bits.checked_add(31))
                        .and_then(|bits| (bits / 32).checked_mul(4))
                    else {
                        return error(env, 8);
                    };
                    let Some(size) = stride
                        .checked_mul(height_abs)
                        .filter(|&size| size <= budget)
                    else {
                        return error(env, 8);
                    };
                    let colors = if bpp <= 8 { 1usize << bpp } else { 0 };
                    if dword(&info, 32) as usize > colors {
                        return Outcome::Return(0);
                    }
                    // Legacy writers store the complete palette, even with biClrUsed.
                    exact(
                        env,
                        stream,
                        colors * 4,
                        Box::new(move |env, colors| {
                            exact(
                                env,
                                stream,
                                size,
                                Box::new(move |env, bits| {
                                    then(
                                        env,
                                        Bitmap {
                                            width,
                                            height: height_abs,
                                            bottom_up: height > 0,
                                            bpp,
                                            stride,
                                            colors,
                                            bits,
                                        },
                                    )
                                }),
                            )
                        }),
                    )
                }),
            )
        }),
    )
}

impl Bitmap {
    fn row(&self, y: usize) -> usize {
        (if self.bottom_up {
            self.height - 1 - y
        } else {
            y
        }) * self.stride
    }
    fn index(&self, x: usize, y: usize) -> usize {
        let row = self.row(y);
        match self.bpp {
            1 => ((self.bits[row + x / 8] >> (7 - x % 8)) & 1) as usize,
            4 => {
                ((self.bits[row + x / 2] >> (if x.is_multiple_of(2) { 4 } else { 0 })) & 15)
                    as usize
            }
            _ => self.bits[row + x] as usize,
        }
    }
    fn pixel(&self, x: usize, y: usize) -> [u8; 4] {
        let mut bgra = if self.bpp <= 8 {
            let at = self.index(x, y) * 4;
            self.colors[at..at + 4].try_into().unwrap()
        } else {
            let at = self.row(y) + x * (self.bpp as usize / 8);
            let format = match self.bpp {
                16 => PixelFormat::Rgb555,
                24 => PixelFormat::Bgr24,
                _ => PixelFormat::Bgra32,
            };
            format.decode(&self.bits[at..])
        };
        bgra.swap(0, 2); // ImageList snapshots use RGBA, unlike GDI surfaces.
        if self.bpp != 32 {
            bgra[3] = 255;
        }
        bgra
    }
}

fn finish(env: &mut Env<'_>, header: Header, color: Bitmap, mask: Option<Bitmap>) -> Outcome {
    let width = header.width as usize;
    let height = header.height as usize;
    let rows = header.count.div_ceil(4);
    if color.width < width * 4
        || color.height < rows * height
        || mask
            .as_ref()
            .is_some_and(|mask| mask.width < width * 4 || mask.height < rows * height)
    {
        return Outcome::Return(0);
    }
    let mut images = Vec::new();
    if images.try_reserve_exact(header.capacity).is_err() {
        return error(env, 8);
    }
    for index in 0..header.count {
        let (left, top) = (index % 4 * width, index / 4 * height);
        let mut rgba = Vec::new();
        let mut and_mask = Vec::new();
        if rgba.try_reserve_exact(width * height * 4).is_err()
            || (header.mask && and_mask.try_reserve_exact(width * height).is_err())
        {
            return error(env, 8);
        }
        let alpha = color.bpp == 32
            && (top..top + height).any(|y| (left..left + width).any(|x| color.pixel(x, y)[3] != 0));
        for y in top..top + height {
            for x in left..left + width {
                let mut pixel = color.pixel(x, y);
                let and = mask.as_ref().map_or(0, |mask| mask.index(x, y) as u8);
                if !alpha {
                    pixel[3] = if and == 0 { 255 } else { 0 };
                }
                rgba.extend_from_slice(&pixel);
                if header.mask {
                    and_mask.push(and);
                }
            }
        }
        images.push(Image {
            rgba,
            mask: header.mask.then_some(and_mask),
        });
    }
    let list = ImageList {
        width: header.width,
        height: header.height,
        mask: header.mask,
        grow: header.grow,
        images,
        background: header.background,
    };
    // Recheck the process budget after callbacks, which may create sibling lists.
    let lists = env.ext.get_or_default::<ImageLists>();
    let bytes: usize = list.images.iter().map(Image::bytes).sum();
    if lists.bytes(&env.sys.objects) + bytes > MAX_PIXEL_BYTES {
        return error(env, 8);
    }
    match lists.insert(&mut env.sys.objects, list) {
        Some(handle) => Outcome::Return(handle),
        None => error(env, 8),
    }
}
