//! USER and common-controls sprites share DC mappings, clipping and presentation.
use crate::{Win32, util::set_last_error};
use adv_device::surface::{BlitRect, CompositeError, FlushReason, Rect, Sprite, SurfaceStore};
use adv_machine::{Env, Outcome};
use adv_os::gdi::GdiObj;
fn fail(env: &mut Env<'_>, error: u32) -> Outcome {
    set_last_error(env, error);
    Outcome::Return(0)
}
pub(crate) fn color(env: &Env<'_>, dc: u32, color: u32) -> Result<[u8; 4], u32> {
    let rgb = match color >> 24 {
        0 => [color as u8, (color >> 8) as u8, (color >> 16) as u8],
        1 => {
            let Some(GdiObj::Dc { palette, .. }) =
                env.ext.get::<Win32>().gdi.get(&env.sys.objects, dc)
            else {
                return Err(6);
            };
            let entry = super::super::palette::entries(env, *palette)
                .and_then(|colors| colors.get((color & 0xffff) as usize))
                .ok_or(87u32)?;
            [entry[0], entry[1], entry[2]]
        }
        _ => return Err(50),
    };
    Ok([rgb[2], rgb[1], rgb[0], 0])
}
pub(crate) fn brush(env: &Env<'_>, dc: u32, handle: u32) -> Result<Option<[u8; 4]>, u32> {
    super::fill::solid_brush(env, handle)?
        .map(|value| color(env, dc, value))
        .transpose()
}
pub(crate) fn draw(
    env: &mut Env<'_>,
    dc: u32,
    point: [i32; 2],
    size: [u32; 2],
    source: Sprite<'_>,
    region: Rect,
    api: &str,
) -> Outcome {
    if super::front::unavailable_pixels(env, dc, api) {
        return Outcome::Return(0);
    }
    let Some(GdiObj::Dc {
        selected,
        modes,
        clip,
        hwnd,
        ..
    }) = env.ext.get::<Win32>().gdi.get(&env.sys.objects, dc)
    else {
        return fail(env, 6);
    };
    let rtl = env
        .ext
        .get::<Win32>()
        .user
        .windows
        .get(&env.sys.objects, hwnd)
        .is_some_and(|w| w.ex_style & 0x400000 != 0);
    if modes.map != 1 || rtl {
        adv_abi::warn_unavailable(
            env,
            api,
            "this DC has no modeled raster mapping for the requested layout",
        );
        return fail(env, 50);
    }
    let (target, modes, clip) = (selected[3], *modes, clip.clone());
    let Some(provider) = super::front::provider(env.ext) else {
        adv_abi::warn_unavailable(env, api, "CPU raster memory access is unavailable");
        return fail(env, 50);
    };
    let Some(surface) = env
        .ext
        .try_get::<SurfaceStore>()
        .and_then(|s| s.get(target))
        .cloned()
    else {
        return fail(env, 6);
    };
    if surface.pixels == 0 {
        adv_abi::warn_unavailable(env, api, "destination pixels have no CPU-readable backing");
        return fail(env, 50);
    }
    let window = env
        .ext
        .get::<Win32>()
        .gdi
        .is_window_dc(&env.sys.objects, dc);
    let [x, y] = modes.to_device(i64::from(point[0]), i64::from(point[1]));
    let result = {
        let store = env.ext.get_mut::<SurfaceStore>();
        let mut raw = (provider.memory)(env.cpu);
        store
            .flush(&raw, FlushReason::Immediate)
            .map_err(CompositeError::Storage)
            .and_then(|_| {
                let mut memory = super::clip::ClippedMemory::new(&mut raw, surface, &clip);
                store.sprite(
                    target,
                    source,
                    region,
                    BlitRect {
                        x,
                        y,
                        w: size[0],
                        h: size[1],
                    },
                    &mut memory,
                )?;
                if window {
                    store
                        .present_from(env.context, target, &memory, memory.damage())
                        .map_err(CompositeError::Storage)?;
                }
                Ok(())
            })
    };
    match result {
        Ok(()) => {
            if window {
                let serial = env.ext.get::<SurfaceStore>().stats.presentations;
                if let Some(outcome) = (provider.completed)(env, serial) {
                    return outcome;
                }
            }
            Outcome::Return(1)
        }
        Err(CompositeError::Allocation) => fail(env, 8),
        Err(CompositeError::Storage(_)) => fail(env, 87),
    }
}
