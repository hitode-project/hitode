//! DIB color tables select the same surface through every view of a GDI DC.
use crate::Win32;
use adv_abi::{Env, GuestPtr, Handle, Outcome, Registry, win32};
use adv_cpu::backend::Prot;
use adv_device::surface::SurfaceStore;
use adv_os::gdi::GdiObj;

pub(super) fn register(registry: &mut Registry) {
    registry.register(set_ABI);
    registry.register(get_ABI);
}

fn range(env: &Env<'_>, dc: u32, start: u32, count: u32) -> Option<(u32, usize)> {
    let gdi = &env.ext.get::<Win32>().gdi;
    let GdiObj::Dc { selected, .. } = gdi.get(&env.sys.objects, dc)? else {
        return None;
    };
    let bitmap = selected[3];
    let GdiObj::Bitmap {
        bpp: 8, dib_colors, ..
    } = gdi.get(&env.sys.objects, bitmap)?
    else {
        return None;
    };
    let capacity = u32::from(*dib_colors);
    if capacity == 0 || capacity > 256 || start >= capacity {
        return None;
    }
    let count = count.min(capacity - start);
    env.ext
        .try_get::<SurfaceStore>()?
        .palette_entries(bitmap, start, count)?;
    Some((bitmap, count as usize))
}

#[win32(gdi32, name = "SetDIBColorTable", level = implement)]
fn set(
    env: &mut Env<'_>,
    dc: Handle,
    start: u32,
    count: u32,
    colors: GuestPtr<[u8; 4]>,
) -> Outcome {
    let Some((bitmap, count)) = range(env, dc.0, start, count) else {
        return Outcome::Return(0);
    };
    if count == 0 {
        return Outcome::Return(0);
    }
    // Validate and read the complete effective range before changing the live
    // table. RGBQUAD and the neutral surface both store B,G,R; reserved is not alpha.
    if adv_abi::guest::check_range(env.cpu, colors.addr(), count as u32 * 4, Prot::READ).is_err() {
        return Outcome::Return(0);
    }
    let mut entries = [[0; 4]; 256];
    if env
        .cpu
        .read(colors.addr(), entries[..count].as_flattened_mut())
        .is_err()
    {
        return Outcome::Return(0);
    }
    for color in &mut entries[..count] {
        color[3] = 0;
    }
    match env
        .ext
        .get_mut::<SurfaceStore>()
        .set_palette(bitmap, start, &entries[..count])
    {
        Ok(changed) => Outcome::Return(changed as u32),
        Err(_) => Outcome::Return(0),
    }
}

#[win32(gdi32, name = "GetDIBColorTable", level = implement)]
fn get(
    env: &mut Env<'_>,
    dc: Handle,
    start: u32,
    count: u32,
    colors: GuestPtr<[u8; 4]>,
) -> Outcome {
    let Some((bitmap, count)) = range(env, dc.0, start, count) else {
        return Outcome::Return(0);
    };
    if count == 0 {
        return Outcome::Return(0);
    }
    if adv_abi::guest::check_range(env.cpu, colors.addr(), count as u32 * 4, Prot::WRITE).is_err() {
        return Outcome::Return(0);
    }
    let mut entries = [[0; 4]; 256];
    entries[..count].copy_from_slice(
        env.ext
            .get::<SurfaceStore>()
            .palette_entries(bitmap, start, count as u32)
            .unwrap(),
    );
    for color in &mut entries[..count] {
        color[3] = 0;
    }
    Outcome::Return(
        if env
            .cpu
            .write(colors.addr(), entries[..count].as_flattened())
            .is_ok()
        {
            count as u32
        } else {
            0
        },
    )
}
