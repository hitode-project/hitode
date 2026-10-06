//! ExtractIconEx reads guest files and creates ordinary, owned USER icon objects.
//! PE group resources and classic ICO DIBs share the same checked pixel decoder.
use adv_abi::{AnsiStr, GuestPtr, Handle, Registry, WideStr, guest::check_range, win32};
use adv_cpu::backend::{GuestExt, Prot};
use adv_machine::Env;
use adv_pe::{Pe, PeError, ResId, ResourceDir, ResourceNode};
use std::io::Read;

use crate::{
    Win32,
    user32::IconImage,
    util::{MAX_STR, set_last_error},
};

const MAX_FILE: u64 = 128 * 1024 * 1024;
const ERROR_NOACCESS: u32 = 998;

fn word(bytes: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        bytes.get(offset..offset.checked_add(2)?)?.try_into().ok()?,
    ))
}
fn dword(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?,
    ))
}

struct Candidate {
    width: u32,
    height: u32,
    bits: u16,
    bytes: Vec<u8>,
}
struct Group {
    id: Option<ResId>,
    images: Vec<Candidate>,
}

fn file_slice<'a>(pe: &Pe, file: &'a [u8], rva: u32, length: u32) -> Option<&'a [u8]> {
    let offset = if rva < pe.size_of_headers {
        (rva.checked_add(length)? <= pe.size_of_headers).then_some(rva)?
    } else {
        pe.sections.iter().find_map(|section| {
            let delta = rva.checked_sub(section.virtual_address)?;
            (delta.checked_add(length)? <= section.raw_size)
                .then(|| section.raw_offset.checked_add(delta))
                .flatten()
        })?
    } as usize;
    file.get(offset..offset.checked_add(length as usize)?)
}

fn pe_groups(file: &[u8]) -> Option<Vec<Group>> {
    let pe = Pe::parse_headers(file).ok()?;
    let resource = pe.dir(adv_pe::dir::RESOURCE);
    if !resource.is_present() {
        return Some(Vec::new());
    }
    let resources = ResourceDir::read(resource.rva, |rva, bytes| {
        let length = bytes.len() as u32;
        let source =
            file_slice(&pe, file, rva, length).ok_or(PeError::OutOfImage { rva, len: length })?;
        bytes.copy_from_slice(source);
        Ok(())
    })
    .ok()?;
    let Some(ResourceNode::Dir(names)) = resources.get(&ResId::Id(14)) else {
        return Some(Vec::new());
    };
    let mut groups = Vec::new();
    for entry in &names.entries {
        let Some((language, data)) = resources.find(&ResId::Id(14), &entry.id, None) else {
            continue;
        };
        let Some(group) = file_slice(&pe, file, data.rva, data.size) else {
            continue;
        };
        let mut images = Vec::new();
        if word(group, 0) == Some(0) && word(group, 2) == Some(1) {
            let count = usize::from(word(group, 4)?);
            for index in 0..count {
                let offset = 6usize.checked_add(index.checked_mul(14)?)?;
                let Some(raw) = group.get(offset..offset.checked_add(14)?) else {
                    break;
                };
                let image_id = word(raw, 12)?;
                let Some((_, data)) =
                    resources.find(&ResId::Id(3), &ResId::Id(image_id), Some(language))
                else {
                    continue;
                };
                let declared_size = dword(raw, 8)?;
                let Some(bytes) = file_slice(&pe, file, data.rva, data.size.min(declared_size))
                else {
                    continue;
                };
                images.push(Candidate {
                    width: if raw[0] == 0 { 256 } else { u32::from(raw[0]) },
                    height: if raw[1] == 0 { 256 } else { u32::from(raw[1]) },
                    bits: word(raw, 6)?,
                    bytes: bytes.to_vec(),
                });
            }
        }
        groups.push(Group {
            id: Some(entry.id.clone()),
            images,
        });
    }
    Some(groups)
}

fn ico_groups(file: &[u8]) -> Option<Vec<Group>> {
    if word(file, 0) != Some(0) || word(file, 2) != Some(1) {
        return None;
    }
    let count = usize::from(word(file, 4)?);
    if count == 0 || count > 4096 {
        return None;
    }
    let mut images = Vec::new();
    for index in 0..count {
        let offset = 6usize.checked_add(index.checked_mul(16)?)?;
        let raw = file.get(offset..offset.checked_add(16)?)?;
        let size = dword(raw, 8)? as usize;
        let data = dword(raw, 12)? as usize;
        let bytes = file.get(data..data.checked_add(size)?)?;
        images.push(Candidate {
            width: if raw[0] == 0 { 256 } else { u32::from(raw[0]) },
            height: if raw[1] == 0 { 256 } else { u32::from(raw[1]) },
            bits: word(raw, 6)?,
            bytes: bytes.to_vec(),
        });
    }
    Some(vec![Group { id: None, images }])
}

fn bitmap(bytes: &[u8], target: u32) -> Option<IconImage> {
    let header = dword(bytes, 0)? as usize;
    let (width, total_height, planes, bits, colors, palette_entry) = if header == 12 {
        (
            u32::from(word(bytes, 4)?),
            u32::from(word(bytes, 6)?),
            word(bytes, 8)?,
            word(bytes, 10)?,
            0,
            3usize,
        )
    } else if matches!(header, 40 | 52 | 56 | 108 | 124) {
        if dword(bytes, 16)? != 0 {
            return None;
        } // Compressed/bitfield DIBs are unsupported.
        let width = dword(bytes, 4)? as i32;
        let height = dword(bytes, 8)? as i32;
        if width <= 0 || height <= 0 {
            return None;
        }
        (
            width as u32,
            height as u32,
            word(bytes, 12)?,
            word(bytes, 14)?,
            dword(bytes, 32)?,
            4usize,
        )
    } else {
        return None;
    }; // PNG and unknown encodings contain no supported icon.
    if planes != 1
        || width == 0
        || total_height == 0
        || total_height % 2 != 0
        || width > 1024
        || total_height > 2048
        || !matches!(bits, 1 | 4 | 8 | 24 | 32)
    {
        return None;
    }
    let height = total_height / 2;
    let colors = if bits <= 8 {
        let maximum = 1u32 << bits;
        if colors > maximum {
            return None;
        }
        if colors == 0 { maximum } else { colors }
    } else {
        0
    } as usize;
    let pixels = header.checked_add(colors.checked_mul(palette_entry)?)?;
    let stride = ((width.checked_mul(u32::from(bits))?.checked_add(31)? / 32) * 4) as usize;
    let mask_stride = (width.checked_add(31)? / 32 * 4) as usize;
    let mask = pixels.checked_add(stride.checked_mul(height as usize)?)?;
    bytes.get(..mask.checked_add(mask_stride.checked_mul(height as usize)?)?)?;
    let has_alpha = bits == 32
        && (0..height as usize)
            .any(|row| (0..width as usize).any(|x| bytes[pixels + row * stride + x * 4 + 3] != 0));
    let mut rgba = Vec::with_capacity((target * target * 4) as usize);
    let mut and_mask = Vec::with_capacity((target * target) as usize);
    for y in 0..target {
        let sy = (height - 1 - y * height / target) as usize;
        for x in 0..target {
            let sx = (x * width / target) as usize;
            let row = pixels + sy * stride;
            let and = (bytes[mask + sy * mask_stride + sx / 8] >> (7 - sx % 8)) & 1;
            let (r, g, b, a) = if matches!(bits, 24 | 32) {
                let offset = row + sx * usize::from(bits / 8);
                (
                    bytes[offset + 2],
                    bytes[offset + 1],
                    bytes[offset],
                    if has_alpha {
                        bytes[offset + 3]
                    } else if and != 0 {
                        0
                    } else {
                        255
                    },
                )
            } else {
                let index = match bits {
                    1 => (bytes[row + sx / 8] >> (7 - sx % 8)) & 1,
                    4 => (bytes[row + sx / 2] >> if sx % 2 == 0 { 4 } else { 0 }) & 15,
                    _ => bytes[row + sx],
                } as usize;
                if index >= colors {
                    return None;
                }
                let offset = header + index * palette_entry;
                (
                    bytes[offset + 2],
                    bytes[offset + 1],
                    bytes[offset],
                    if and != 0 { 0 } else { 255 },
                )
            };
            rgba.extend_from_slice(&[r, g, b, a]);
            and_mask.push(and);
        }
    }
    Some(IconImage {
        width: target,
        height: target,
        pixels: rgba,
        mask: and_mask,
    })
}

fn image(group: &Group, size: u32) -> Option<IconImage> {
    let mut candidates: Vec<_> = group.images.iter().collect();
    candidates.sort_by_key(|entry| {
        (
            entry.width.abs_diff(size) + entry.height.abs_diff(size),
            u16::MAX - entry.bits,
        )
    });
    candidates
        .into_iter()
        .find_map(|entry| bitmap(&entry.bytes, size))
}

fn fail(env: &mut Env<'_>, error: u32) -> u32 {
    set_last_error(env, error);
    u32::MAX
}

fn extract(
    env: &mut Env<'_>,
    path: Result<String, u32>,
    index: i32,
    large: u32,
    small: u32,
    count: u32,
) -> u32 {
    let query = index == -1 && large == 0 && small == 0;
    // Native rejects a zero extraction count without altering caller outputs or
    // LastError, while its -1/NULL count query ignores nIcons.
    if !query && count == 0 {
        return u32::MAX;
    }
    let path = match path {
        Ok(path) => path,
        Err(error) => return fail(env, error),
    };
    let Some(host) = env.ext.get::<Win32>().vfs.lookup(&path) else {
        set_last_error(env, 2);
        return if query { 0 } else { u32::MAX };
    };
    let file = std::fs::File::open(host);
    let mut file = match file {
        Ok(file) => file,
        Err(error) => {
            return fail(
                env,
                if error.kind() == std::io::ErrorKind::NotFound {
                    2
                } else {
                    5
                },
            );
        }
    };
    let mut bytes = Vec::new();
    if file
        .by_ref()
        .take(MAX_FILE + 1)
        .read_to_end(&mut bytes)
        .is_err()
    {
        return fail(env, 5);
    }
    if bytes.len() as u64 > MAX_FILE {
        return fail(env, 8);
    }
    let portable_executable = bytes.starts_with(b"MZ");
    let groups = if portable_executable {
        pe_groups(&bytes)
    } else {
        ico_groups(&bytes)
    }
    .unwrap_or_default();
    if query || (!portable_executable && large == 0 && small == 0) {
        return groups.len() as u32;
    }
    if large == 0 && small == 0 {
        return 0;
    }
    let Some(length) = count.checked_mul(4) else {
        return fail(env, ERROR_NOACCESS);
    };
    if [large, small]
        .into_iter()
        .any(|address| address != 0 && check_range(env.cpu, address, length, Prot::WRITE).is_err())
    {
        return fail(env, ERROR_NOACCESS);
    }
    // ICO's native negative-index path is a count query, even with output arrays;
    // it clears only the first slot and creates no HICON.
    if !portable_executable && index == -1 {
        for address in [large, small] {
            if address != 0 && env.cpu.write_u32(address, 0).is_err() {
                return fail(env, ERROR_NOACCESS);
            }
        }
        return groups.len() as u32;
    }
    let zero = [0; 4096];
    for address in [large, small].into_iter().filter(|&address| address != 0) {
        let mut offset = 0;
        while offset < length {
            let size = (length - offset).min(zero.len() as u32);
            if env
                .cpu
                .write(address + offset, &zero[..size as usize])
                .is_err()
            {
                return fail(env, ERROR_NOACCESS);
            }
            offset += size;
        }
    }
    let start = if index < 0 {
        let Some(id) = index.checked_neg().and_then(|id| u16::try_from(id).ok()) else {
            return 0;
        };
        let Some(start) = groups
            .iter()
            .position(|group| group.id == Some(ResId::Id(id)))
        else {
            return 0;
        };
        start
    } else {
        index as usize
    };
    let available = groups.len().saturating_sub(start).min(count as usize);
    let mut extracted = 0;
    for group in groups.iter().skip(start).take(available) {
        let mut handles = [0; 2];
        for (which, (address, size)) in [(large, 32), (small, 16)].into_iter().enumerate() {
            if address == 0 {
                continue;
            }
            let Some(image) = image(group, size) else {
                continue;
            };
            let description = format!("icon extracted {path} {:?} {size}x{size}", group.id);
            let Some(handle) = env.ext.get_mut::<Win32>().user.objects.create_icon(
                &mut env.sys.objects,
                description,
                image,
            ) else {
                for handle in handles {
                    env.ext
                        .get_mut::<Win32>()
                        .user
                        .objects
                        .destroy_icon(&mut env.sys.objects, handle);
                }
                return if extracted == 0 {
                    fail(env, 8)
                } else {
                    extracted
                };
            };
            handles[which] = handle;
        }
        for (address, handle) in [large, small].into_iter().zip(handles) {
            if address != 0 && env.cpu.write_u32(address + extracted * 4, handle).is_err() {
                for handle in handles {
                    env.ext
                        .get_mut::<Win32>()
                        .user
                        .objects
                        .destroy_icon(&mut env.sys.objects, handle);
                }
                return if extracted == 0 {
                    fail(env, ERROR_NOACCESS)
                } else {
                    extracted
                };
            }
        }
        if handles == [0; 2] {
            break;
        }
        extracted += 1;
    }
    // PE's dual-size path reports actual HICON count when the complete request
    // succeeds; an exhausted group enumeration reports the available group count.
    // ICO extraction reports logical icon count regardless of the two sizes.
    if portable_executable && large != 0 && small != 0 && extracted == count {
        extracted * 2
    } else {
        extracted
    }
}

#[win32(shell32, name = "ExtractIconExA", level = implement)]
fn extract_a(
    env: &mut Env<'_>,
    file: AnsiStr,
    index: i32,
    large: GuestPtr<Handle>,
    small: GuestPtr<Handle>,
    count: u32,
) -> u32 {
    let path = file
        .read(env.cpu, MAX_STR)
        .map(|bytes| env.ext.get::<Win32>().codec.decode(&bytes))
        .map_err(|_| ERROR_NOACCESS);
    extract(env, path, index, large.addr(), small.addr(), count)
}
#[win32(shell32, name = "ExtractIconExW", level = implement)]
fn extract_w(
    env: &mut Env<'_>,
    file: WideStr,
    index: i32,
    large: GuestPtr<Handle>,
    small: GuestPtr<Handle>,
    count: u32,
) -> u32 {
    let path = file
        .read(env.cpu, MAX_STR)
        .map(|units| String::from_utf16_lossy(&units))
        .map_err(|_| ERROR_NOACCESS);
    extract(env, path, index, large.addr(), small.addr(), count)
}

pub(super) fn register(registry: &mut Registry) {
    registry.register(extract_a_ABI);
    registry.register(extract_w_ABI);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn color_dib(size: u32, color: [u8; 4]) -> Vec<u8> {
        let mask_stride = (size + 31) / 32 * 4;
        let mut bytes = vec![0; (40 + size * size * 4 + mask_stride * size) as usize];
        bytes[..4].copy_from_slice(&40u32.to_le_bytes());
        bytes[4..8].copy_from_slice(&size.to_le_bytes());
        bytes[8..12].copy_from_slice(&(size * 2).to_le_bytes());
        bytes[12..14].copy_from_slice(&1u16.to_le_bytes());
        bytes[14..16].copy_from_slice(&32u16.to_le_bytes());
        for pixel in bytes[40..40 + (size * size * 4) as usize].chunks_exact_mut(4) {
            pixel.copy_from_slice(&color);
        }
        bytes
    }

    #[test]
    fn dib_pixels_alpha_and_size_selection_are_real_and_bounds_checked() {
        let large = color_dib(32, [0, 255, 0, 123]);
        let small = color_dib(16, [0, 0, 255, 255]);
        let group = Group {
            id: None,
            images: vec![
                Candidate {
                    width: 16,
                    height: 16,
                    bits: 32,
                    bytes: small,
                },
                Candidate {
                    width: 32,
                    height: 32,
                    bits: 32,
                    bytes: large.clone(),
                },
            ],
        };
        let extracted = image(&group, 32).unwrap();
        assert_eq!((extracted.width, extracted.height), (32, 32));
        assert!(
            extracted
                .pixels
                .chunks_exact(4)
                .all(|pixel| pixel == [0, 255, 0, 123])
        );
        assert!(extracted.mask.iter().all(|&value| value == 0));
        assert_eq!(&image(&group, 16).unwrap().pixels[..4], &[255, 0, 0, 255]);
        assert!(bitmap(&large[..large.len() - 1], 32).is_none());
        assert!(bitmap(b"\x89PNG\r\n\x1a\n", 32).is_none());
        let mut compressed = large;
        compressed[16..20].copy_from_slice(&3u32.to_le_bytes());
        assert!(bitmap(&compressed, 32).is_none());
    }

    #[test]
    fn core_monochrome_icon_retains_its_and_xor_information() {
        let mut bytes = vec![0; 26];
        bytes[..4].copy_from_slice(&12u32.to_le_bytes());
        for (offset, value) in [(4, 1u16), (6, 2), (8, 1), (10, 1)] {
            bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
        }
        bytes[15..18].fill(255); // Palette 1 is white.
        bytes[18] = 0x80; // XOR 1.
        bytes[22] = 0x80; // AND 1, the invert pixel rather than an opaque white.
        let decoded = bitmap(&bytes, 1).unwrap();
        assert_eq!(decoded.pixels, [255, 255, 255, 0]);
        assert_eq!(decoded.mask, [1]);
    }
}
