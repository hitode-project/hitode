//! GDI DIB reads over existing bitmap bytes; no private renderer or bitmap store.
use super::*;

fn fail(env: &mut Env<'_>, code: u32) -> Outcome {
    crate::util::set_last_error(env, code);
    Outcome::Return(0)
}
fn unsupported(env: &mut Env<'_>) -> Outcome {
    adv_abi::warn_unavailable(
        env,
        "GetDIBits",
        "only same-size 24/32-bit RGB or standard 32-bit BITFIELDS are supported",
    );
    fail(env, 50)
}

#[win32(gdi32, name = "GetDIBits", level = implement)]
#[expect(clippy::too_many_arguments, reason = "SDK bitmap readback signature")]
fn get_bits(
    env: &mut Env<'_>,
    dc: Handle,
    bitmap: Handle,
    start: u32,
    count: u32,
    output: GuestPtr<()>,
    info: GuestPtr<()>,
    usage: u32,
) -> Outcome {
    if !matches!(get(env, dc.0), Some(GdiObj::Dc { .. })) {
        return fail(env, 6);
    }
    let Some(GdiObj::Bitmap {
        width,
        height,
        bpp,
        bits,
        stride,
        owned,
        ..
    }) = get(env, bitmap.0)
    else {
        return fail(env, 87);
    };
    let (width, height, source_bpp, source_bits, source_pitch, owned) =
        (*width, height.unsigned_abs(), *bpp, *bits, *stride, *owned);
    if width <= 0
        || height == 0
        || env.sys.objects.all_hosts::<GdiObj>().any(
            |(_, object)| matches!(object, GdiObj::Dc { selected, .. } if selected[3] == bitmap.0),
        )
    {
        return fail(env, 87);
    }
    if usage != 0 || !matches!(source_bpp, 24 | 32) {
        return unsupported(env);
    }
    let mut header = [0; 40];
    if adv_abi::guest::check_range(env.cpu, info.addr(), 40, Prot::READ).is_err()
        || env.cpu.read(info.addr(), &mut header).is_err()
    {
        return fail(env, 87);
    }
    let word = |offset| u32::from_le_bytes(header[offset..offset + 4].try_into().unwrap());
    if word(0) != 40 {
        return unsupported(env);
    }
    let mut bpp = u16::from_le_bytes(header[14..16].try_into().unwrap());
    let attributes = bpp == 0 && (output.is_null() || count == 0);
    let (destination_height, destination_pitch) = if attributes {
        header.fill(0);
        header[..4].copy_from_slice(&40u32.to_le_bytes());
        header[4..8].copy_from_slice(&width.to_le_bytes());
        header[8..12].copy_from_slice(&height.to_le_bytes());
        header[12..14].copy_from_slice(&1u16.to_le_bytes());
        bpp = source_bpp;
        header[14..16].copy_from_slice(&bpp.to_le_bytes());
        if !owned && bpp == 32 {
            header[16..20].copy_from_slice(&3u32.to_le_bytes());
        }
        (height as i32, (width as u32 * u32::from(bpp) / 8 + 3) & !3)
    } else {
        let destination_height = word(8) as i32;
        let compression = word(16);
        if word(4) as i32 != width
            || destination_height.unsigned_abs() != height
            || destination_height == i32::MIN
            || header[12..14] != 1u16.to_le_bytes()
            || !matches!(bpp, 24 | 32)
            || !(compression == 0 || compression == 3 && bpp == 32)
        {
            return unsupported(env);
        }
        if compression == 3 {
            let expected: Vec<_> = [0x00ff0000u32, 0x0000ff00, 0x000000ff]
                .into_iter()
                .flat_map(u32::to_le_bytes)
                .collect();
            let mut actual = [0; 12];
            let Some(address) = info.addr().checked_add(40) else {
                return fail(env, 87);
            };
            if adv_abi::guest::check_range(env.cpu, address, 12, Prot::READ).is_err()
                || env.cpu.read(address, &mut actual).is_err()
            {
                return fail(env, 87);
            }
            if actual != expected.as_slice() {
                return unsupported(env);
            }
        }
        (
            destination_height,
            (width as u32 * u32::from(bpp) / 8 + 3) & !3,
        )
    };
    let Some(image_size) = destination_pitch.checked_mul(height) else {
        return fail(env, 87);
    };
    header[20..24].copy_from_slice(&image_size.to_le_bytes());
    // A zero-bit-count query fills only the header, even when native GDI
    // reports BITFIELDS for a DIB section; it does not append a mask table.
    let description = header;
    if adv_abi::guest::check_range(env.cpu, info.addr(), description.len() as u32, Prot::WRITE)
        .is_err()
    {
        return fail(env, 87);
    }
    let rows = if output.is_null() || count == 0 {
        0
    } else {
        count.min(height.saturating_sub(start))
    };
    let Some(length) = rows.checked_mul(destination_pitch) else {
        return fail(env, 87);
    };
    // Validate every input and output before changing either output buffer.
    if rows > 0
        && (adv_abi::guest::check_range(env.cpu, output.addr(), length, Prot::WRITE).is_err()
            || source_pitch.checked_mul(height).is_none_or(|len| {
                adv_abi::guest::check_range(env.cpu, source_bits, len, Prot::READ).is_err()
            }))
    {
        return fail(env, 87);
    }
    let source_bottom_up = env
        .ext
        .try_get::<SurfaceStore>()
        .and_then(|s| s.get(bitmap.0))
        .is_some_and(|s| s.bottom_up);
    let mut pixels = vec![0; length as usize];
    let mut source = vec![0; source_pitch as usize];
    for y in 0..rows {
        // start always counts from the bitmap bottom. A top-down result reverses
        // just the selected scan-line range, rather than changing start's origin.
        let top_y = if destination_height < 0 {
            height - start - rows + y
        } else {
            height - 1 - start - y
        };
        let source_y = if source_bottom_up {
            height - 1 - top_y
        } else {
            top_y
        };
        if env
            .cpu
            .read(source_bits + source_y * source_pitch, &mut source)
            .is_err()
        {
            return fail(env, 87);
        }
        let destination =
            &mut pixels[(y * destination_pitch) as usize..((y + 1) * destination_pitch) as usize];
        let source_bytes = usize::from(source_bpp / 8);
        let destination_bytes = usize::from(bpp / 8);
        for x in 0..width as usize {
            let from = &source[x * source_bytes..][..source_bytes];
            let to = &mut destination[x * destination_bytes..][..destination_bytes];
            to[..3].copy_from_slice(&from[..3]);
            if destination_bytes == 4 {
                to[3] = if source_bytes == 4 { from[3] } else { 0 };
            }
        }
    }
    if rows > 0 && env.cpu.write(output.addr(), &pixels).is_err() {
        return fail(env, 87);
    }
    if env.cpu.write(info.addr(), &description).is_err() {
        return fail(env, 87);
    }
    Outcome::Return(rows.max(1)) // Native no-copy queries also return 1.
}

pub(super) fn register(registry: &mut Registry) {
    registry.register(get_bits_ABI);
}

#[cfg(test)]
mod tests;
