//! Same-depth indexed readback over the existing surface and logical palette.
use super::*;

pub(super) fn read(
    env: &mut Env<'_>,
    dc: u32,
    bitmap: u32,
    start: u32,
    count: u32,
    output: u32,
    info: u32,
) -> Outcome {
    let Some(surface) = env
        .ext
        .try_get::<SurfaceStore>()
        .and_then(|s| s.get(bitmap))
        .cloned()
    else {
        return unsupported(env);
    };
    let mut header = [0; 40];
    if adv_abi::guest::check_range(env.cpu, info, 40, Prot::READ).is_err()
        || env.cpu.read(info, &mut header).is_err()
    {
        return fail(env, 87);
    }
    let word = |at| u32::from_le_bytes(header[at..at + 4].try_into().unwrap());
    let bpp = surface.format.bits();
    let requested = u16::from_le_bytes(header[14..16].try_into().unwrap());
    let attributes = requested == 0 && (output == 0 || count == 0);
    let height = if attributes {
        surface.height as i32
    } else {
        word(8) as i32
    };
    if word(0) != 40
        || (!attributes
            && (word(4) != surface.width
                || height.unsigned_abs() != surface.height
                || height == i32::MIN
                || requested != bpp as u16
                || word(16) != 0
                || header[12..14] != 1u16.to_le_bytes()))
    {
        return unsupported(env);
    }
    let colors = if attributes || word(32) == 0 {
        surface.format.colors()
    } else {
        word(32) as usize
    };
    if colors > surface.format.colors() {
        return fail(env, 87);
    }
    if attributes {
        header.fill(0);
        header[..4].copy_from_slice(&40u32.to_le_bytes());
        header[4..8].copy_from_slice(&surface.width.to_le_bytes());
        header[8..12].copy_from_slice(&surface.height.to_le_bytes());
        header[12..14].copy_from_slice(&1u16.to_le_bytes());
        header[14..16].copy_from_slice(&(bpp as u16).to_le_bytes());
        header[32..36].copy_from_slice(&(colors as u32).to_le_bytes());
        header[36..40].copy_from_slice(&(colors as u32).to_le_bytes());
    }
    let pitch = (surface.format.row_bytes(surface.width) + 3) & !3;
    header[20..24].copy_from_slice(&(pitch * surface.height).to_le_bytes());
    let usage = env.arg(6);
    let mut description = header.to_vec();
    let Some(table) = surface.palette.as_deref() else {
        return unsupported(env);
    };
    let mut indices = [0u8; 256];
    if usage == 1 && !attributes {
        let logical = super::super::palette::dc_entries(env, dc);
        for (mapped, color) in indices.iter_mut().zip(table) {
            let index = logical
                .iter()
                .take(surface.format.colors())
                .enumerate()
                .min_by_key(|(_, entry)| {
                    (0..3)
                        .map(|i| {
                            let delta = i32::from(color[i]) - i32::from(entry[2 - i]);
                            delta * delta
                        })
                        .sum::<i32>()
                })
                .map_or(0, |(index, _)| index as u8);
            *mapped = index;
        }
        for index in 0..colors as u16 {
            description.extend_from_slice(&index.to_le_bytes());
        }
    } else if !attributes {
        for color in &table[..colors] {
            description.extend_from_slice(color);
        }
    }
    let rows = if output == 0 || count == 0 {
        0
    } else {
        count.min(surface.height.saturating_sub(start))
    };
    let len = rows * pitch;
    if adv_abi::guest::check_range(env.cpu, info, description.len() as u32, Prot::WRITE).is_err()
        || (rows != 0 && adv_abi::guest::check_range(env.cpu, output, len, Prot::WRITE).is_err())
    {
        return fail(env, 87);
    }
    let mut pixels = vec![0; len as usize];
    let bytes = surface.format.row_bytes(surface.width) as usize;
    for y in 0..rows {
        let top = if height < 0 {
            surface.height - start - rows + y
        } else {
            surface.height - 1 - start - y
        };
        let address = surface.row(0, top);
        let row = &mut pixels[(y * pitch) as usize..(y * pitch) as usize + bytes];
        if adv_abi::guest::check_range(env.cpu, address, bytes as u32, Prot::READ).is_err()
            || env.cpu.read(address, row).is_err()
        {
            return fail(env, 87);
        }
        if usage == 1 {
            let raw = row.to_vec();
            row.fill(0);
            for x in 0..surface.width {
                let offset = surface.format.byte_offset(x) as usize;
                let shift = 8 - bpp - (x * bpp) % 8;
                let mask = ((1u16 << bpp) - 1) as u8;
                let index = (raw[offset] >> shift) & mask;
                row[offset] |= indices[index as usize] << shift;
            }
        } else if !(surface.width * bpp).is_multiple_of(8) {
            // Native readback clears unused low bits of the final packed byte.
            row[bytes - 1] &= 255 << (8 - (surface.width * bpp) % 8);
        }
    }
    if rows != 0 && env.cpu.write(output, &pixels).is_err() {
        return fail(env, 87);
    }
    if env.cpu.write(info, &description).is_err() {
        return fail(env, 87);
    }
    Outcome::Return(rows.max(1))
}
