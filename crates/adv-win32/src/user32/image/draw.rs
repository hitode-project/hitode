//! Icons retain their own pixels while drawing through the shared GDI sprite path.
use super::*;
use adv_device::surface::{PixelFormat, Rect, Sprite, SpriteAlpha, SpriteMode};
use adv_machine::Outcome;
fn error(env: &mut Env<'_>, code: u32) -> Outcome {
    set_last_error(env, code);
    Outcome::Return(0)
}
#[expect(clippy::too_many_arguments, reason = "shared SDK drawing parameters")]
fn draw(
    env: &mut Env<'_>,
    dc: u32,
    x: i32,
    y: i32,
    icon: u32,
    width: i32,
    height: i32,
    brush: u32,
    flags: u32,
    api: &str,
) -> Outcome {
    if flags & !0x1f != 0 || width < 0 || height < 0 || flags & 3 == 0 {
        return error(env, 87);
    }
    if !user(env).objects.contains(&env.sys.objects, icon) {
        return error(env, 1402);
    }
    let Some(image) = user(env).objects.image(&env.sys.objects, icon) else {
        adv_abi::warn_unavailable(env, api, "icon or cursor has no decoded pixels");
        return error(env, 50);
    };
    let size = |given: i32, actual: u32| {
        if given != 0 {
            given as u32
        } else if flags & 8 != 0 {
            32
        } else {
            actual
        }
    };
    let size = [size(width, image.width), size(height, image.height)];
    let alpha = if image.alpha {
        SpriteAlpha::Straight
    } else {
        SpriteAlpha::Ignore
    };
    let mode = match flags & 3 {
        1 => SpriteMode::Mask {
            foreground: [0, 0, 0, 0],
            background: Some([255, 255, 255, 0]),
        },
        2 => SpriteMode::Image { alpha },
        _ => SpriteMode::Color {
            alpha,
            masked: true,
        },
    };
    let background = if brush != 0 {
        match crate::gdi32::raster::sprite::brush(env, dc, brush) {
            Ok(color) => color,
            Err(code) => {
                if code == 50 {
                    adv_abi::warn_unavailable(
                        env,
                        api,
                        "flicker-free brush color conversion is unavailable",
                    );
                }
                return error(env, code);
            }
        }
    } else {
        None
    };
    // Keep the image owner alive across any device/presentation callback.
    let pixels = image.pixels.clone();
    let mask = image.mask.clone();
    let (w, h) = (image.width, image.height);
    crate::gdi32::raster::sprite::draw(
        env,
        dc,
        [x, y],
        size,
        Sprite {
            width: w,
            height: h,
            rgba: &pixels,
            mask: Some(&mask),
            mode,
            background,
            preserve_alpha: false,
        },
        Rect { x: 0, y: 0, w, h },
        api,
    )
}
#[win32(user32,name="DrawIcon",level=implement)]
fn icon(env: &mut Env<'_>, dc: Handle, x: i32, y: i32, image: Handle) -> Outcome {
    draw(
        env,
        dc.0,
        x,
        y,
        image.0,
        0,
        0,
        0,
        0xf,
        "user32.dll!DrawIcon",
    )
}
#[win32(user32,name="DrawIconEx",level=implement)]
fn icon_ex(
    env: &mut Env<'_>,
    dc: Handle,
    x: i32,
    y: i32,
    image: Handle,
    width: i32,
    height: i32,
    _step: u32,
    brush: Handle,
    flags: u32,
) -> Outcome {
    draw(
        env,
        dc.0,
        x,
        y,
        image.0,
        width,
        height,
        brush.0,
        flags,
        "user32.dll!DrawIconEx",
    )
}
#[win32(user32,name="CreateIcon",level=implement)]
fn create(
    env: &mut Env<'_>,
    _instance: Handle,
    width: i32,
    height: i32,
    planes: u32,
    bits: u32,
    and: GuestPtr<u8>,
    xor: GuestPtr<u8>,
) -> Handle {
    const API: &str = "user32.dll!CreateIcon";
    let planes = planes as u8;
    let bits = bits as u8;
    if width <= 0 || height <= 0 || planes == 0 || bits == 0 {
        return fail(env, 87);
    }
    if width > 1024 || height > 1024 || planes != 1 || !matches!(bits, 1 | 16 | 24 | 32) {
        return unsupported(
            env,
            API,
            "icon dimensions, planes or device-dependent palette are outside the retained pixel representation",
            50,
        );
    }
    let (width, height) = (width as u32, height as u32);
    let and_stride = width.div_ceil(16) * 2;
    let xor_stride = (width * u32::from(bits)).div_ceil(16) * 2;
    let mut and_bytes = vec![0; (and_stride * height) as usize];
    let mut xor_bytes = vec![0; (xor_stride * height) as usize];
    if check_range(env.cpu, and.addr(), and_bytes.len() as u32, Prot::READ).is_err()
        || check_range(env.cpu, xor.addr(), xor_bytes.len() as u32, Prot::READ).is_err()
        || env.cpu.read(and.addr(), &mut and_bytes).is_err()
        || env.cpu.read(xor.addr(), &mut xor_bytes).is_err()
    {
        return fail(env, 87);
    }
    let alpha = bits == 32
        && (0..height)
            .any(|y| (0..width).any(|x| xor_bytes[(y * xor_stride + x * 4 + 3) as usize] != 0));
    let mut pixels = Vec::with_capacity((width * height * 4) as usize);
    let mut mask = Vec::with_capacity((width * height) as usize);
    for y in 0..height {
        for x in 0..width {
            let and = (and_bytes[(y * and_stride + x / 8) as usize] >> (7 - x % 8)) & 1;
            mask.push(and);
            let mut bgra = if bits == 1 {
                let bit = (xor_bytes[(y * xor_stride + x / 8) as usize] >> (7 - x % 8)) & 1;
                [bit * 255, bit * 255, bit * 255, 0]
            } else {
                let format = match bits {
                    16 => PixelFormat::Rgb555,
                    24 => PixelFormat::Bgr24,
                    _ => PixelFormat::Bgra32,
                };
                format.decode(&xor_bytes[(y * xor_stride + x * u32::from(bits) / 8) as usize..])
            };
            if alpha {
                let a = u32::from(bgra[3]);
                for value in &mut bgra[..3] {
                    *value = (u32::from(*value) * 255 + a / 2)
                        .checked_div(a)
                        .unwrap_or(u32::from(*value))
                        .min(255) as u8;
                }
            } else {
                bgra[3] = if and == 0 { 255 } else { 0 };
            }
            pixels.extend_from_slice(&[bgra[2], bgra[1], bgra[0], bgra[3]]);
        }
    }
    let image = IconImage {
        alpha,
        monochrome: bits == 1,
        width,
        height,
        pixels,
        mask,
    };
    match env.ext.get_mut::<Win32>().user.objects.create_icon(
        &mut env.sys.objects,
        "created icon".into(),
        image,
    ) {
        Some(icon) => Handle(icon),
        None => fail(env, 8),
    }
}
pub(super) fn register(registry: &mut Registry) {
    for api in [icon_ABI, icon_ex_ABI, create_ABI] {
        registry.register(api);
    }
}
