//! Section-backed DIBs share the ordinary file-mapping pages and surface store.
use super::*;
use crate::kernel32::file_mapping::{map_dib_section, unmap_section};
use adv_cpu::backend::GuestExt;
use adv_os::gdi::GdiObj;

pub(super) fn create(
    env: &mut Env<'_>,
    dib: super::super::dib::Dib,
    output: u32,
    section: u32,
    offset: u32,
) -> Outcome {
    let Some(bytes) = dib.pitch.checked_mul(dib.height) else {
        return fail(env, 87);
    };
    let (base, pixels) = match map_dib_section(env, section, offset, bytes) {
        Ok(view) => view,
        Err(error) => return fail(env, error),
    };
    let provider = env.ext.get::<State>().provider.unwrap();
    let surface = Surface {
        pixels,
        width: dib.width,
        height: dib.height,
        pitch: dib.pitch,
        bottom_up: dib.bottom_up,
        locked: None,
        version: 0,
        format: dib.format,
        palette: dib.palette,
    };
    let bitmap = match env
        .ext
        .get_mut::<SurfaceStore>()
        .import_guest(surface, &mut (provider.memory)(env.cpu))
    {
        Ok(id) => id,
        Err(_) => {
            let _ = unmap_section(env, base);
            return fail(env, 87);
        }
    };
    let metadata = GdiObj::Bitmap {
        width: dib.width as i32,
        height: if dib.bottom_up {
            dib.height as i32
        } else {
            -(dib.height as i32)
        },
        bpp: dib.format.bits() as u16,
        bits: pixels,
        stride: dib.pitch,
        owned: false,
        mapping_view: Some(base),
        dib_colors: dib.palette_count,
    };
    let published =
        env.ext
            .get_mut::<State>()
            .dcs
            .register_bitmap(&mut env.sys.objects, bitmap, metadata);
    if !published || env.cpu.write_u32(output, pixels).is_err() {
        env.ext
            .get_mut::<State>()
            .dcs
            .remove_bitmap(&mut env.sys.objects, bitmap);
        env.ext.get_mut::<SurfaceStore>().delete(bitmap);
        let _ = unmap_section(env, base);
        return fail(env, if published { 87 } else { 8 });
    }
    Outcome::Return(bitmap)
}

#[cfg(test)]
mod tests;

pub(super) fn release(env: &mut Env<'_>, bitmap: u32) -> Option<Outcome> {
    let GdiObj::Bitmap {
        mapping_view: Some(base),
        ..
    } = env.ext.get::<Win32>().gdi.get(&env.sys.objects, bitmap)?
    else {
        return None;
    };
    let base = *base;
    if env
        .ext
        .get::<State>()
        .dcs
        .selects(&env.sys.objects, bitmap, None)
    {
        return Some(fail(env, 170));
    }
    if !matches!(unmap_section(env, base), Outcome::Return(1)) {
        return Some(Outcome::Return(0));
    }
    // There is no guest yield between the selection check and retirement.
    Some(raster_call(env, |r, s, d, o, m| {
        r.delete_bitmap(s, d, o, m, bitmap)
    }))
}
