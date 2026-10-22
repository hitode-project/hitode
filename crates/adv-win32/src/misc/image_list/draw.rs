//! Image list drawing and icon extraction share retained pixels and GDI sprites.
use super::*;
use crate::gdi32::raster::sprite;
use adv_device::surface::{Rect, Sprite, SpriteMode};
#[expect(clippy::too_many_arguments, reason = "shared SDK drawing parameters")]
fn draw(
    env: &mut Env<'_>,
    handle: i32,
    index: i32,
    dc: u32,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    background: u32,
    _foreground: u32,
    flags: u32,
    api: &str,
) -> Outcome {
    if width < 0 || height < 0 || flags & !0xff77 != 0 {
        return Outcome::Return(fail(env, 87, 0) as u32);
    }
    let Some(list) = env
        .ext
        .try_get::<ImageLists>()
        .and_then(|lists| lists.get(&env.sys.objects, handle as u32))
    else {
        return Outcome::Return(fail(env, 6, 0) as u32);
    };
    let Some(image) = usize::try_from(index).ok().and_then(|i| list.images.get(i)) else {
        return Outcome::Return(fail(env, 6, 0) as u32);
    };
    if flags & (6 | 0xf00) != 0 {
        return Outcome::Return(unsupported(
            env,
            api,
            "legacy blend stipples and serialized overlay slots have no modeled pixel representation",
            50,
            0,
        ) as u32);
    }
    let (w, h, alpha) = (list.width, list.height, image.alpha);
    let rgba = match copy_bytes(&image.rgba) {
        Ok(bytes) => bytes,
        Err(_) => return Outcome::Return(fail(env, 8, 0) as u32),
    };
    let mask = match image.mask.as_deref().map(copy_bytes).transpose() {
        Ok(mask) => mask,
        Err(_) => return Outcome::Return(fail(env, 8, 0) as u32),
    };
    let background = if flags & 1 != 0 {
        u32::MAX
    } else if background == 0xff000000 {
        list.background
    } else {
        background
    };
    let background = if background == u32::MAX {
        None
    } else {
        match sprite::color(env, dc, background) {
            Ok(color) => Some(color),
            Err(error) => {
                if error == 50 {
                    adv_abi::warn_unavailable(
                        env,
                        api,
                        "palette-relative image-list colors are unavailable",
                    );
                }
                return Outcome::Return(fail(env, error, 0) as u32);
            }
        }
    };
    let mode = if flags & 0x10 != 0 {
        let text = match env.ext.get::<Win32>().gdi.get(&env.sys.objects, dc) {
            Some(adv_os::gdi::GdiObj::Dc { text_color, .. }) => *text_color,
            _ => return Outcome::Return(fail(env, 6, 0) as u32),
        };
        let color = match sprite::color(env, dc, text) {
            Ok(color) => color,
            Err(code) => return Outcome::Return(fail(env, code, 0) as u32),
        };
        SpriteMode::Mask {
            foreground: color,
            background,
        }
    } else if flags & (0x20 | 0x40) != 0 {
        SpriteMode::Image { alpha }
    } else {
        SpriteMode::Color {
            alpha,
            masked: true,
        }
    };
    let size = [
        if width == 0 { w } else { width as u32 },
        if height == 0 { h } else { height as u32 },
    ];
    let region = Rect {
        x: 0,
        y: 0,
        w: if flags & 0x2000 != 0 {
            w
        } else {
            size[0].min(w)
        },
        h: if flags & 0x2000 != 0 {
            h
        } else {
            size[1].min(h)
        },
    };
    let size = if flags & 0x2000 != 0 {
        size
    } else {
        [region.w, region.h]
    };
    sprite::draw(
        env,
        dc,
        [x, y],
        size,
        Sprite {
            width: w,
            height: h,
            rgba: &rgba,
            mask: mask.as_deref(),
            mode,
            background: if flags & 0x10 != 0 { None } else { background },
            preserve_alpha: flags & 0x1000 != 0,
        },
        region,
        api,
    )
}
#[win32(comctl32,name="ImageList_Draw",level=implement)]
fn normal(
    env: &mut Env<'_>,
    handle: i32,
    index: i32,
    dc: Handle,
    x: i32,
    y: i32,
    flags: u32,
) -> Outcome {
    draw(
        env,
        handle,
        index,
        dc.0,
        x,
        y,
        0,
        0,
        0xff000000,
        0xff000000,
        flags,
        "comctl32.dll!ImageList_Draw",
    )
}
#[win32(comctl32,name="ImageList_DrawEx",level=implement)]
fn extended(
    env: &mut Env<'_>,
    handle: i32,
    index: i32,
    dc: Handle,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    background: u32,
    foreground: u32,
    flags: u32,
) -> Outcome {
    draw(
        env,
        handle,
        index,
        dc.0,
        x,
        y,
        width,
        height,
        background,
        foreground,
        flags,
        "comctl32.dll!ImageList_DrawEx",
    )
}
#[win32(comctl32,name="ImageList_GetIcon",level=implement)]
fn get_icon(env: &mut Env<'_>, handle: i32, index: i32, flags: u32) -> Handle {
    let Some(list) = env
        .ext
        .try_get::<ImageLists>()
        .and_then(|lists| lists.get(&env.sys.objects, handle as u32))
    else {
        fail(env, 6, 0);
        return Handle(0);
    };
    let Some(image) = usize::try_from(index)
        .ok()
        .and_then(|index| list.images.get(index))
    else {
        fail(env, 6, 0);
        return Handle(0);
    };
    if flags & !1 != 0 {
        unsupported(
            env,
            "comctl32.dll!ImageList_GetIcon",
            "style-specific icon extraction has no modeled bitmap representation",
            50,
            0,
        );
        return Handle(0);
    }
    let mut pixels = match copy_bytes(&image.rgba) {
        Ok(bytes) => bytes,
        Err(_) => {
            fail(env, 8, 0);
            return Handle(0);
        }
    };
    if image.alpha == SpriteAlpha::Premultiplied {
        for pixel in pixels.as_chunks_mut::<4>().0 {
            let alpha = u32::from(pixel[3]);
            for channel in &mut pixel[..3] {
                *channel = (u32::from(*channel) * 255 + alpha / 2)
                    .checked_div(alpha)
                    .unwrap_or(u32::from(*channel))
                    .min(255) as u8;
            }
        }
    }
    let mask = match image.mask.as_deref().map(copy_bytes).transpose() {
        Ok(Some(mask)) => mask,
        Ok(None) => vec![0; (list.width * list.height) as usize],
        Err(_) => {
            fail(env, 8, 0);
            return Handle(0);
        }
    };
    let icon = crate::user32::IconImage {
        width: list.width,
        height: list.height,
        pixels,
        mask,
        alpha: image.alpha != SpriteAlpha::Ignore,
        monochrome: false,
    };
    let handle = env.ext.get_mut::<Win32>().user.objects.create_icon(
        &mut env.sys.objects,
        "image list icon".into(),
        icon,
    );
    if handle.is_none() {
        fail(env, 8, 0);
    }
    Handle(handle.unwrap_or(0))
}
pub(super) fn register(registry: &mut Registry) {
    for api in [normal_ABI, extended_ABI, get_icon_ABI] {
        registry.register(api);
    }
}
