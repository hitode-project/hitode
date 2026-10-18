//! Shared checked ICO/PE icon directory and DIB pixel decoding.
use super::super::IconImage;
use adv_pe::{Pe, PeError, ResId, ResourceDir, ResourceNode};

// Entries may repeat the same payload range. Bound total owned copies, rather
// than assuming the input file length bounds the decoder's allocations.
pub(crate) const MAX_CANDIDATE_BYTES: usize = 16 * 1024 * 1024;

pub(crate) fn word(bytes: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        bytes.get(offset..offset.checked_add(2)?)?.try_into().ok()?,
    ))
}
pub(crate) fn dword(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?,
    ))
}

pub(crate) struct Candidate {
    pub width: u32,
    pub height: u32,
    pub bits: u16,
    pub bytes: Vec<u8>,
}
pub(crate) struct Group {
    pub id: Option<ResId>,
    pub images: Vec<Candidate>,
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

pub(crate) fn pe_groups(file: &[u8]) -> Option<Vec<Group>> {
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
    let mut copied = 0_usize;
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
                copied = copied.checked_add(bytes.len())?;
                if copied > MAX_CANDIDATE_BYTES {
                    return None;
                }
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

pub(crate) fn ico_groups(file: &[u8]) -> Option<Vec<Group>> {
    if word(file, 0) != Some(0) || word(file, 2) != Some(1) {
        return None;
    }
    let count = usize::from(word(file, 4)?);
    if count == 0 || count > 4096 {
        return None;
    }
    let mut images = Vec::new();
    let mut copied = 0_usize;
    for index in 0..count {
        let offset = 6usize.checked_add(index.checked_mul(16)?)?;
        let raw = file.get(offset..offset.checked_add(16)?)?;
        let size = dword(raw, 8)? as usize;
        let data = dword(raw, 12)? as usize;
        let bytes = file.get(data..data.checked_add(size)?)?;
        copied = copied.checked_add(bytes.len())?;
        if copied > MAX_CANDIDATE_BYTES {
            return None;
        }
        images.push(Candidate {
            width: if raw[0] == 0 { 256 } else { u32::from(raw[0]) },
            height: if raw[1] == 0 { 256 } else { u32::from(raw[1]) },
            bits: word(raw, 6)?,
            bytes: bytes.to_vec(),
        });
    }
    Some(vec![Group { id: None, images }])
}

#[cfg(test)]
pub(crate) fn bitmap(bytes: &[u8], target: u32) -> Option<IconImage> {
    bitmap_size(bytes, target, target)
}

fn bitmap_size(bytes: &[u8], target_width: u32, target_height: u32) -> Option<IconImage> {
    if target_width == 0 || target_height == 0 || target_width > 1024 || target_height > 1024 {
        return None;
    }
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
    let count = target_width.checked_mul(target_height)? as usize;
    let mut rgba = Vec::with_capacity(count.checked_mul(4)?);
    let mut and_mask = Vec::with_capacity(count);
    for y in 0..target_height {
        let sy = (height - 1 - y * height / target_height) as usize;
        for x in 0..target_width {
            let sx = (x * width / target_width) as usize;
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
                    4 => (bytes[row + sx / 2] >> if sx.is_multiple_of(2) { 4 } else { 0 }) & 15,
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
        width: target_width,
        height: target_height,
        pixels: rgba,
        mask: and_mask,
    })
}

pub(crate) fn image(group: &Group, size: u32) -> Option<IconImage> {
    image_size(group, size, size)
}

pub(crate) fn image_size(group: &Group, width: u32, height: u32) -> Option<IconImage> {
    let first = group.images.first()?;
    let width = if width == 0 { first.width } else { width };
    let height = if height == 0 { first.height } else { height };
    let mut candidates: Vec<_> = group.images.iter().collect();
    candidates.sort_by_key(|entry| {
        (
            entry.width.abs_diff(width) + entry.height.abs_diff(height),
            u16::MAX - entry.bits,
        )
    });
    candidates
        .into_iter()
        .find_map(|entry| bitmap_size(&entry.bytes, width, height))
}
