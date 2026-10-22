//! Flat image, path and format queries reuse the existing GDI+ object owner.
use super::*;

#[win32(gdiplus, name = "GdipBitmapGetPixel", level = implement)]
fn pixel(env: &mut Env<'_>, bitmap: GuestPtr<()>, x: i32, y: i32, out: GuestPtr<u32>) -> i32 {
    let read = (|| {
        if out.is_null() {
            return Err(2);
        }
        let image = image(env, bitmap.addr())?;
        if x < 0 || y < 0 || x as u32 >= image.width || y as u32 >= image.height {
            return Err(2);
        }
        refresh_image(env, &image)?;
        let Reply::Pixels(bytes) = image.object.execute(Op::Pixels)? else {
            return Err(2);
        };
        let at = (y as usize * image.width as usize + x as usize) * 4;
        out.write(
            env.cpu,
            u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()),
        )
        .map_err(|_| 2)
    })();
    read.err().unwrap_or(0)
}
#[win32(gdiplus, name = "GdipBitmapSetPixel", level = implement)]
fn set_pixel(env: &mut Env<'_>, bitmap: GuestPtr<()>, x: i32, y: i32, color: u32) -> i32 {
    let write = (|| {
        let image = image(env, bitmap.addr())?;
        if x < 0 || y < 0 || x as u32 >= image.width || y as u32 >= image.height {
            return Err(2);
        }
        refresh_image(env, &image)?;
        let Reply::Pixels(mut bytes) = image.object.execute(Op::Pixels)? else {
            return Err(2);
        };
        let at = (y as usize * image.width as usize + x as usize) * 4;
        bytes[at..at + 4].copy_from_slice(&color.to_le_bytes());
        image.object.execute(Op::Upload(&bytes))?;
        finish_image(env, &image)
    })();
    write.err().unwrap_or(0)
}
#[win32(gdiplus, name = "GdipGetImageDimension", level = implement)]
fn dimensions(
    env: &mut Env<'_>,
    handle: GuestPtr<()>,
    width: GuestPtr<f32>,
    height: GuestPtr<f32>,
) -> i32 {
    let value = (|| {
        if width.is_null() || height.is_null() {
            return Err(2);
        }
        let image = image(env, handle.addr())?;
        width.write(env.cpu, image.width as f32).map_err(|_| 2)?;
        height.write(env.cpu, image.height as f32).map_err(|_| 2)
    })();
    value.err().unwrap_or(0)
}
#[win32(gdiplus, name = "GdipGetImageBounds", level = implement)]
fn bounds(env: &mut Env<'_>, handle: GuestPtr<()>, rect: GuestPtr<()>, unit: GuestPtr<i32>) -> i32 {
    let value = (|| {
        if rect.is_null() || unit.is_null() {
            return Err(2);
        }
        let image = image(env, handle.addr())?;
        if !put_rect(
            env,
            rect.addr(),
            [0., 0., image.width as f32, image.height as f32],
        ) {
            return Err(2);
        }
        unit.write(env.cpu, 2).map_err(|_| 2)
    })();
    value.err().unwrap_or(0)
}
#[win32(gdiplus, name = "GdipGetImageType", level = implement)]
fn image_type(env: &mut Env<'_>, handle: GuestPtr<()>, out: GuestPtr<i32>) -> i32 {
    if image(env, handle.addr()).is_err() || out.is_null() || out.write(env.cpu, 1).is_err() {
        2
    } else {
        0
    }
}

fn clone_object(env: &mut Env<'_>, handle: u32, out: u32, kind: Kind) -> Outcome {
    if !clear(env, out) {
        return Outcome::Return(2);
    }
    let cloned = (|| {
        let original = object(env, handle)?;
        if original.kind != kind {
            return Err(2);
        }
        runtime(env)?
            .create(Create::Clone(&original))
            .map(Value::Object)
    })();
    publish(env, out, cloned)
}
#[win32(gdiplus, name = "GdipClonePath", level = implement)]
fn clone_path(env: &mut Env<'_>, path: GuestPtr<()>, out: GuestPtr<()>) -> Outcome {
    clone_object(env, path.addr(), out.addr(), Kind::Path)
}
#[win32(gdiplus, name = "GdipCloneStringFormat", level = implement)]
fn clone_format(env: &mut Env<'_>, format: GuestPtr<()>, out: GuestPtr<()>) -> Outcome {
    clone_object(env, format.addr(), out.addr(), Kind::Format)
}
pub(super) fn property(
    env: &mut Env<'_>,
    handle: u32,
    out: GuestPtr<i32>,
    operation: Op<'_>,
) -> i32 {
    if out.is_null() {
        return 2;
    }
    match object(env, handle).and_then(|object| object.execute(operation)) {
        Ok(Reply::Count(value)) => {
            if out.write(env.cpu, value).is_ok() {
                0
            } else {
                2
            }
        }
        Err(status) => status,
        _ => 2,
    }
}
#[win32(gdiplus, name = "GdipGetPathFillMode", level = implement)]
fn fill_mode(env: &mut Env<'_>, path: GuestPtr<()>, out: GuestPtr<i32>) -> i32 {
    property(env, path.addr(), out, Op::PathFill(None))
}
#[win32(gdiplus, name = "GdipGetPointCount", level = implement)]
fn point_count(env: &mut Env<'_>, path: GuestPtr<()>, out: GuestPtr<i32>) -> i32 {
    if !object(env, path.addr()).is_ok_and(|object| object.kind == Kind::Path) {
        return 2;
    }
    property(env, path.addr(), out, Op::PointCount)
}
#[win32(gdiplus, name = "GdipSetPathFillMode", level = implement)]
fn set_fill_mode(env: &mut Env<'_>, path: GuestPtr<()>, mode: i32) -> Outcome {
    result(
        object(env, path.addr()).and_then(|object| object.execute(Op::PathFill(Some(mode)))),
        env,
    )
}
#[win32(gdiplus, name = "GdipResetPath", level = implement)]
fn reset_path(env: &mut Env<'_>, path: GuestPtr<()>) -> Outcome {
    result(
        object(env, path.addr()).and_then(|object| object.execute(Op::ResetPath)),
        env,
    )
}
#[win32(gdiplus, name = "GdipGetStringFormatAlign", level = implement)]
fn alignment(env: &mut Env<'_>, format: GuestPtr<()>, out: GuestPtr<i32>) -> i32 {
    property(env, format.addr(), out, Op::StringProperty(2))
}
#[win32(gdiplus, name = "GdipGetStringFormatLineAlign", level = implement)]
fn line_alignment(env: &mut Env<'_>, format: GuestPtr<()>, out: GuestPtr<i32>) -> i32 {
    property(env, format.addr(), out, Op::StringProperty(3))
}
#[win32(gdiplus, name = "GdipGetStringFormatFlags", level = implement)]
fn flags(env: &mut Env<'_>, format: GuestPtr<()>, out: GuestPtr<i32>) -> i32 {
    property(env, format.addr(), out, Op::StringProperty(0))
}
#[win32(gdiplus, name = "GdipSetStringFormatFlags", level = implement)]
fn set_flags(env: &mut Env<'_>, format: GuestPtr<()>, flags: i32) -> Outcome {
    result(
        object(env, format.addr()).and_then(|object| object.execute(Op::StringFlags(flags))),
        env,
    )
}
#[win32(gdiplus, name = "GdipGetPixelOffsetMode", level = implement)]
fn pixel_offset(env: &mut Env<'_>, graphics: GuestPtr<()>, out: GuestPtr<i32>) -> i32 {
    property(env, graphics.addr(), out, Op::PixelOffset(None))
}
#[win32(gdiplus, name = "GdipSetPixelOffsetMode", level = implement)]
fn set_pixel_offset(env: &mut Env<'_>, graphics: GuestPtr<()>, mode: i32) -> Outcome {
    result(
        object(env, graphics.addr()).and_then(|object| object.execute(Op::PixelOffset(Some(mode)))),
        env,
    )
}

pub(super) fn register(registry: &mut Registry) {
    for api in [
        pixel_ABI,
        set_pixel_ABI,
        dimensions_ABI,
        bounds_ABI,
        image_type_ABI,
        clone_path_ABI,
        clone_format_ABI,
        fill_mode_ABI,
        point_count_ABI,
        set_fill_mode_ABI,
        reset_path_ABI,
        alignment_ABI,
        line_alignment_ABI,
        flags_ABI,
        set_flags_ABI,
        pixel_offset_ABI,
        set_pixel_offset_ABI,
        brush_type_ABI,
        solid_color_ABI,
        set_solid_color_ABI,
    ] {
        registry.register(api);
    }
}

#[win32(gdiplus, name = "GdipGetBrushType", level = implement)]
fn brush_type(env: &mut Env<'_>, brush: GuestPtr<()>, out: GuestPtr<i32>) -> i32 {
    property(env, brush.addr(), out, Op::BrushType)
}
#[win32(gdiplus, name = "GdipGetSolidFillColor", level = implement)]
fn solid_color(env: &mut Env<'_>, brush: GuestPtr<()>, out: GuestPtr<u32>) -> i32 {
    property(env, brush.addr(), out.cast(), Op::SolidColor(None))
}
#[win32(gdiplus, name = "GdipSetSolidFillColor", level = implement)]
fn set_solid_color(env: &mut Env<'_>, brush: GuestPtr<()>, color: u32) -> Outcome {
    result(
        object(env, brush.addr()).and_then(|brush| brush.execute(Op::SolidColor(Some(color)))),
        env,
    )
}
