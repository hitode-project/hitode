//! Owned system raster objects. All native pointers stay inside this adapter.
use super::*;
use std::ffi::c_void;
use windows_sys::Win32::Graphics::{Gdi as gdi, GdiPlus as gp};
mod world;
const ARGB: i32 = 0x26200a;
fn status(result: i32) -> Result<(), i32> {
    if result == 0 { Ok(()) } else { Err(result) }
}
fn require(object: &Object, kind: Kind) -> Result<*mut c_void, i32> {
    if object.kind == kind {
        Ok(object.pointer)
    } else {
        Err(2)
    }
}
fn brush(object: &Object) -> Result<*mut c_void, i32> {
    if matches!(object.kind, Kind::Brush | Kind::Gradient) {
        Ok(object.pointer)
    } else {
        Err(2)
    }
}
fn format(object: Option<&Object>) -> Result<*mut c_void, i32> {
    object.map_or(Ok(std::ptr::null_mut()), |o| require(o, Kind::Format))
}
fn rect([x, y, width, height]: [f32; 4]) -> gp::RectF {
    gp::RectF {
        X: x,
        Y: y,
        Width: width,
        Height: height,
    }
}
fn bounds(rect: gp::RectF) -> [f32; 4] {
    [rect.X, rect.Y, rect.Width, rect.Height]
}
pub(super) struct Canvas {
    dc: gdi::HDC,
    bitmap: gdi::HBITMAP,
    previous: gdi::HGDIOBJ,
    pixels: *mut u8,
    bytes: usize,
}
impl Canvas {
    fn new(width: u32, height: u32, bytes: &[u8]) -> Result<Self, i32> {
        let len = width as usize * height as usize * 4;
        if width == 0 || height == 0 || len > super::super::image::LIMIT || bytes.len() != len {
            return Err(2);
        }
        // Own a memory DC and top-down DIB. This is never a desktop/window DC.
        unsafe {
            let dc = gdi::CreateCompatibleDC(std::ptr::null_mut());
            if dc.is_null() {
                return Err(3);
            }
            let mut info: gdi::BITMAPINFO = std::mem::zeroed();
            info.bmiHeader.biSize = 40;
            info.bmiHeader.biWidth = width as i32;
            info.bmiHeader.biHeight = -(height as i32);
            info.bmiHeader.biPlanes = 1;
            info.bmiHeader.biBitCount = 32;
            let mut bits = std::ptr::null_mut();
            let bitmap = gdi::CreateDIBSection(
                dc,
                &info,
                gdi::DIB_RGB_COLORS,
                &mut bits,
                std::ptr::null_mut(),
                0,
            );
            if bitmap.is_null() || bits.is_null() {
                gdi::DeleteDC(dc);
                return Err(3);
            }
            let previous = gdi::SelectObject(dc, bitmap);
            if previous.is_null() {
                gdi::DeleteObject(bitmap);
                gdi::DeleteDC(dc);
                return Err(3);
            }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), bits.cast(), len);
            Ok(Self {
                dc,
                bitmap,
                previous,
                pixels: bits.cast(),
                bytes: len,
            })
        }
    }
}
impl Drop for Canvas {
    fn drop(&mut self) {
        unsafe {
            gdi::SelectObject(self.dc, self.previous);
            gdi::DeleteObject(self.bitmap);
            gdi::DeleteDC(self.dc);
        }
    }
}
pub(super) fn startup() -> Result<Arc<Runtime>, i32> {
    let mut token = 0;
    let input = gp::GdiplusStartupInput {
        GdiplusVersion: 1,
        DebugEventCallback: 0,
        SuppressBackgroundThread: 0,
        SuppressExternalCodecs: 1,
    };
    // SAFETY: the SDK input and output are private initialized host storage.
    status(unsafe { gp::GdiplusStartup(&mut token, &input, std::ptr::null_mut()) })?;
    Ok(Arc::new(Runtime { token }))
}
pub(super) fn shutdown(token: usize) {
    unsafe { gp::GdiplusShutdown(token) }
}
pub(super) fn destroy(object: &Object) {
    // SAFETY: kind was captured at construction; Drop runs exactly once and the
    // runtime and any image backing outlive this call.
    unsafe {
        match object.kind {
            Kind::Canvas => {}
            Kind::Image => {
                gp::GdipDisposeImage(object.pointer.cast());
            }
            Kind::Graphics => {
                gp::GdipDeleteGraphics(object.pointer.cast());
            }
            Kind::Family => {
                gp::GdipDeleteFontFamily(object.pointer.cast());
            }
            Kind::Font => {
                gp::GdipDeleteFont(object.pointer.cast());
            }
            Kind::Format => {
                gp::GdipDeleteStringFormat(object.pointer.cast());
            }
            Kind::Brush | Kind::Gradient => {
                gp::GdipDeleteBrush(object.pointer.cast());
            }
            Kind::Path => {
                gp::GdipDeletePath(object.pointer.cast());
            }
        }
    }
}
#[allow(
    clippy::arc_with_non_send_sync,
    reason = "GDI handles are shared only within the owning execution thread"
)]
pub(super) fn create(runtime: &Arc<Runtime>, create: Create<'_>) -> Result<Arc<Object>, i32> {
    if let Create::Canvas(w, h, bytes) = &create {
        let canvas = Canvas::new(*w, *h, bytes)?;
        return Ok(Arc::new(Object {
            kind: Kind::Canvas,
            pointer: canvas.dc.cast(),
            canvas: Some(canvas),
            _runtime: Arc::clone(runtime),
            _backing: None,
        }));
    }
    let mut pointer: *mut c_void = std::ptr::null_mut();
    let mut backing = None;
    let upload = match &create {
        Create::Image(_, _, bytes) => Some(*bytes),
        _ => None,
    };
    // SAFETY: strings/slices are owned host values; every object parameter is
    // type checked and retains its native runtime. No guest address is passed.
    let (kind, hr) = unsafe {
        match create {
            Create::Canvas(_, _, _) => unreachable!(),
            Create::Image(w, h, bytes) => {
                if w == 0
                    || h == 0
                    || u64::from(w) * u64::from(h) * 4 > super::super::image::LIMIT as u64
                    || bytes.len() != w as usize * h as usize * 4
                {
                    return Err(2);
                }
                (
                    Kind::Image,
                    gp::GdipCreateBitmapFromScan0(
                        w as i32,
                        h as i32,
                        0,
                        ARGB,
                        std::ptr::null(),
                        (&mut pointer as *mut *mut c_void).cast(),
                    ),
                )
            }
            Create::Graphics(image) => {
                let hdc = image.canvas.as_ref().map(|c| c.dc);
                let image_pointer = if hdc.is_some() {
                    image.pointer
                } else {
                    require(&image, Kind::Image)?
                };
                backing = Some(image);
                (
                    Kind::Graphics,
                    if let Some(dc) = hdc {
                        gp::GdipCreateFromHDC(dc, (&mut pointer as *mut *mut c_void).cast())
                    } else {
                        gp::GdipGetImageGraphicsContext(
                            image_pointer.cast(),
                            (&mut pointer as *mut *mut c_void).cast(),
                        )
                    },
                )
            }
            Create::Family(name) => {
                if name.last() != Some(&0) {
                    return Err(2);
                }
                (
                    Kind::Family,
                    gp::GdipCreateFontFamilyFromName(
                        name.as_ptr(),
                        std::ptr::null_mut(),
                        (&mut pointer as *mut *mut c_void).cast(),
                    ),
                )
            }
            Create::SansSerif => (
                Kind::Family,
                gp::GdipGetGenericFontFamilySansSerif((&mut pointer as *mut *mut c_void).cast()),
            ),
            Create::Font(family, size, style, unit) => (
                Kind::Font,
                gp::GdipCreateFont(
                    require(family, Kind::Family)?.cast(),
                    size,
                    style,
                    unit,
                    (&mut pointer as *mut *mut c_void).cast(),
                ),
            ),
            Create::Format(flags, language) => (
                Kind::Format,
                gp::GdipCreateStringFormat(
                    flags,
                    language,
                    (&mut pointer as *mut *mut c_void).cast(),
                ),
            ),
            Create::Solid(color) => (
                Kind::Brush,
                gp::GdipCreateSolidFill(color, (&mut pointer as *mut *mut c_void).cast()),
            ),
            Create::Path(mode) => (
                Kind::Path,
                gp::GdipCreatePath(mode, (&mut pointer as *mut *mut c_void).cast()),
            ),
            Create::Gradient(path) => (
                Kind::Gradient,
                gp::GdipCreatePathGradientFromPath(
                    require(path, Kind::Path)?.cast(),
                    (&mut pointer as *mut *mut c_void).cast(),
                ),
            ),
            Create::Clone(object) => match object.kind {
                Kind::Image => (
                    Kind::Image,
                    gp::GdipCloneImage(
                        object.pointer.cast(),
                        (&mut pointer as *mut *mut c_void).cast(),
                    ),
                ),
                Kind::Brush | Kind::Gradient => (
                    object.kind,
                    gp::GdipCloneBrush(
                        object.pointer.cast(),
                        (&mut pointer as *mut *mut c_void).cast(),
                    ),
                ),
                Kind::Path => (
                    Kind::Path,
                    gp::GdipClonePath(
                        object.pointer.cast(),
                        (&mut pointer as *mut *mut c_void).cast(),
                    ),
                ),
                Kind::Format => (
                    Kind::Format,
                    gp::GdipCloneStringFormat(
                        object.pointer.cast(),
                        (&mut pointer as *mut *mut c_void).cast(),
                    ),
                ),
                _ => return Err(2),
            },
        }
    };
    status(hr)?;
    if pointer.is_null() {
        return Err(3);
    }
    let object = Arc::new(Object {
        kind,
        _runtime: Arc::clone(runtime),
        pointer,
        _backing: backing,
        canvas: None,
    });
    if let Some(bytes) = upload {
        execute(&object, Op::Upload(bytes))?;
    }
    Ok(object)
}
fn pixel_transfer(object: &Object, input: Option<&[u8]>) -> Result<Vec<u8>, i32> {
    if let Some(canvas) = &object.canvas {
        if let Some(input) = input {
            if input.len() != canvas.bytes {
                return Err(2);
            }
            unsafe {
                std::ptr::copy_nonoverlapping(input.as_ptr(), canvas.pixels, canvas.bytes);
            }
            return Ok(Vec::new());
        }
        return Ok(unsafe { std::slice::from_raw_parts(canvas.pixels, canvas.bytes) }.to_vec());
    }
    let pointer = require(object, Kind::Image)?;
    let (mut w, mut h) = (0, 0);
    unsafe {
        status(gp::GdipGetImageWidth(pointer.cast(), &mut w))?;
        status(gp::GdipGetImageHeight(pointer.cast(), &mut h))?;
    }
    let len = w as usize * h as usize * 4;
    if len > super::super::image::LIMIT || input.is_some_and(|p| p.len() != len) {
        return Err(2);
    }
    let rectangle = gp::Rect {
        X: 0,
        Y: 0,
        Width: w as i32,
        Height: h as i32,
    };
    let mut data: gp::BitmapData = unsafe { std::mem::zeroed() };
    unsafe {
        status(gp::GdipBitmapLockBits(
            pointer.cast(),
            &rectangle,
            if input.is_some() { 2 } else { 1 },
            ARGB,
            &mut data,
        ))?;
    }
    let result = (|| {
        if data.Scan0.is_null() || (data.Stride.unsigned_abs() as usize) < (w as usize * 4) {
            return Err(2);
        }
        let mut bytes = if input.is_none() {
            vec![0; len]
        } else {
            Vec::new()
        };
        for row in 0..h as usize {
            // The native lock owns this whole signed-stride row until Unlock.
            let address = unsafe {
                data.Scan0
                    .cast::<u8>()
                    .offset(row as isize * data.Stride as isize)
            };
            let begin = row * w as usize * 4;
            unsafe {
                if let Some(input) = input {
                    std::ptr::copy_nonoverlapping(input[begin..].as_ptr(), address, w as usize * 4);
                } else {
                    std::ptr::copy_nonoverlapping(
                        address,
                        bytes[begin..].as_mut_ptr(),
                        w as usize * 4,
                    );
                }
            }
        }
        Ok(bytes)
    })();
    let unlock = unsafe { gp::GdipBitmapUnlockBits(pointer.cast(), &mut data) };
    result.and_then(|bytes| {
        status(unlock)?;
        Ok(bytes)
    })
}
pub(super) fn execute(object: &Object, operation: Op<'_>) -> Result<Reply, i32> {
    let p = object.pointer;
    // SAFETY: methods validate receiver/argument kinds, sizes and owned host
    // buffers before calling the flat system API. Output structs stay private.
    unsafe {
        match operation {
            Op::World(value) => {
                require(object, Kind::Graphics)?;
                match value {
                    Some(matrix) => world::set(p, matrix),
                    None => world::get(p),
                }
            }
            Op::ComposeWorld(matrix, prepend) => {
                require(object, Kind::Graphics)?;
                world::compose(p, matrix, prepend)
            }
            Op::BrushType => {
                let pointer = brush(object)?;
                let mut kind = 0;
                status(gp::GdipGetBrushType(pointer.cast(), &mut kind))?;
                Ok(Reply::Count(kind))
            }
            Op::SolidColor(value) => {
                let pointer = brush(object)?;
                let mut kind = 0;
                status(gp::GdipGetBrushType(pointer.cast(), &mut kind))?;
                if kind != 0 {
                    return Err(2);
                }
                if let Some(color) = value {
                    status(gp::GdipSetSolidFillColor(pointer.cast(), color))?;
                }
                let mut color = 0;
                status(gp::GdipGetSolidFillColor(pointer.cast(), &mut color))?;
                Ok(Reply::Count(color as i32))
            }
            Op::Pixels => Ok(Reply::Pixels(pixel_transfer(object, None)?)),
            Op::Upload(bytes) => {
                pixel_transfer(object, Some(bytes))?;
                Ok(Reply::None)
            }
            Op::Size => {
                require(object, Kind::Image)?;
                let (mut w, mut h) = (0, 0);
                status(gp::GdipGetImageWidth(p.cast(), &mut w))?;
                status(gp::GdipGetImageHeight(p.cast(), &mut h))?;
                Ok(Reply::Size(w, h))
            }
            Op::Align(value) => {
                require(object, Kind::Format)?;
                status(gp::GdipSetStringFormatAlign(p.cast(), value))?;
                Ok(Reply::None)
            }
            Op::LineAlign(value) => {
                require(object, Kind::Format)?;
                status(gp::GdipSetStringFormatLineAlign(p.cast(), value))?;
                Ok(Reply::None)
            }
            Op::StringProperty(index) => {
                require(object, Kind::Format)?;
                let mut value = 0;
                status(match index {
                    0 => gp::GdipGetStringFormatFlags(p.cast(), &mut value),
                    1 => {
                        let mut language = 0u16;
                        let result = gp::GdipGetStringFormatDigitSubstitution(
                            p.cast(),
                            &mut language,
                            &mut value,
                        );
                        value = i32::from(language);
                        result
                    }
                    2 => gp::GdipGetStringFormatAlign(p.cast(), &mut value),
                    3 => gp::GdipGetStringFormatLineAlign(p.cast(), &mut value),
                    _ => return Err(2),
                })?;
                Ok(Reply::Count(value))
            }
            Op::StringFlags(value) => {
                require(object, Kind::Format)?;
                status(gp::GdipSetStringFormatFlags(p.cast(), value))?;
                Ok(Reply::None)
            }
            Op::PathFill(value) => {
                require(object, Kind::Path)?;
                if let Some(value) = value {
                    status(gp::GdipSetPathFillMode(p.cast(), value))?;
                }
                let mut mode = 0;
                status(gp::GdipGetPathFillMode(p.cast(), &mut mode))?;
                Ok(Reply::Count(mode))
            }
            Op::ResetPath => {
                require(object, Kind::Path)?;
                status(gp::GdipResetPath(p.cast()))?;
                Ok(Reply::None)
            }
            Op::PathLine([x1, y1, x2, y2]) => {
                require(object, Kind::Path)?;
                status(gp::GdipAddPathLine(p.cast(), x1, y1, x2, y2))?;
                Ok(Reply::None)
            }
            Op::PathArc(r, start, sweep) => {
                require(object, Kind::Path)?;
                let [x, y, w, h] = r;
                status(gp::GdipAddPathArc(p.cast(), x, y, w, h, start, sweep))?;
                Ok(Reply::None)
            }
            Op::ClosePath => {
                require(object, Kind::Path)?;
                status(gp::GdipClosePathFigure(p.cast()))?;
                Ok(Reply::None)
            }
            Op::PathString {
                text,
                family,
                style,
                size,
                rect: r,
                format: f,
            } => {
                require(object, Kind::Path)?;
                status(gp::GdipAddPathString(
                    p.cast(),
                    text.as_ptr(),
                    text.len() as i32,
                    require(family, Kind::Family)?.cast(),
                    style,
                    size,
                    &rect(r),
                    format(f)?.cast(),
                ))?;
                Ok(Reply::None)
            }
            Op::Bounds => {
                require(object, Kind::Path)?;
                let mut r = rect([0.; 4]);
                status(gp::GdipGetPathWorldBounds(
                    p.cast(),
                    &mut r,
                    std::ptr::null(),
                    std::ptr::null(),
                ))?;
                Ok(Reply::Bounds(bounds(r)))
            }
            Op::PointCount => {
                let mut n = 0;
                status(match object.kind {
                    Kind::Path => gp::GdipGetPointCount(p.cast(), &mut n),
                    Kind::Gradient => gp::GdipGetPathGradientPointCount(p.cast(), &mut n),
                    _ => return Err(2),
                })?;
                Ok(Reply::Count(n))
            }
            Op::CenterColor(color) => {
                require(object, Kind::Gradient)?;
                status(gp::GdipSetPathGradientCenterColor(p.cast(), color))?;
                Ok(Reply::None)
            }
            Op::Focus(x, y) => {
                require(object, Kind::Gradient)?;
                status(gp::GdipSetPathGradientFocusScales(p.cast(), x, y))?;
                Ok(Reply::None)
            }
            Op::Gamma(value) => {
                require(object, Kind::Gradient)?;
                status(gp::GdipSetPathGradientGammaCorrection(
                    p.cast(),
                    i32::from(value),
                ))?;
                Ok(Reply::None)
            }
            Op::Surround(colors) => {
                require(object, Kind::Gradient)?;
                let mut n = colors.len() as i32;
                status(gp::GdipSetPathGradientSurroundColorsWithCount(
                    p.cast(),
                    colors.as_ptr(),
                    &mut n,
                ))?;
                Ok(Reply::Count(n))
            }
            Op::FillPath(b, path) => {
                require(object, Kind::Graphics)?;
                status(gp::GdipFillPath(
                    p.cast(),
                    brush(b)?.cast(),
                    require(path, Kind::Path)?.cast(),
                ))?;
                Ok(Reply::None)
            }
            Op::DrawImage(image, [x, y, w, h]) => {
                require(object, Kind::Graphics)?;
                status(gp::GdipDrawImageRect(
                    p.cast(),
                    require(image, Kind::Image)?.cast(),
                    x,
                    y,
                    w,
                    h,
                ))?;
                Ok(Reply::None)
            }
            Op::DrawString {
                text,
                font,
                rect: r,
                format: f,
                brush: b,
            } => {
                require(object, Kind::Graphics)?;
                status(gp::GdipDrawString(
                    p.cast(),
                    text.as_ptr(),
                    text.len() as i32,
                    require(font, Kind::Font)?.cast(),
                    &rect(r),
                    format(f)?.cast(),
                    brush(b)?.cast(),
                ))?;
                Ok(Reply::None)
            }
            Op::Measure {
                text,
                font,
                rect: r,
                format: f,
            } => {
                require(object, Kind::Graphics)?;
                let (mut result, mut fitted, mut lines) = (rect([0.; 4]), 0, 0);
                status(gp::GdipMeasureString(
                    p.cast(),
                    text.as_ptr(),
                    text.len() as i32,
                    require(font, Kind::Font)?.cast(),
                    &rect(r),
                    format(f)?.cast(),
                    &mut result,
                    &mut fitted,
                    &mut lines,
                ))?;
                Ok(Reply::Measure(bounds(result), fitted, lines))
            }
            Op::Clear(color) => {
                require(object, Kind::Graphics)?;
                status(gp::GdipGraphicsClear(p.cast(), color))?;
                Ok(Reply::None)
            }
            Op::Clip(rectangles) => {
                require(object, Kind::Graphics)?;
                status(gp::GdipResetClip(p.cast()))?;
                status(gp::GdipSetClipRect(p.cast(), 0., 0., 0., 0., 0))?;
                for r in rectangles {
                    let [x, y, w, h] = r;
                    status(gp::GdipSetClipRect(p.cast(), x, y, w, h, 2))?;
                }
                Ok(Reply::None)
            }
            Op::Smoothing(value) => {
                require(object, Kind::Graphics)?;
                status(gp::GdipSetSmoothingMode(p.cast(), value))?;
                Ok(Reply::None)
            }
            Op::TextHint(value) => {
                require(object, Kind::Graphics)?;
                status(gp::GdipSetTextRenderingHint(p.cast(), value))?;
                Ok(Reply::None)
            }
            Op::PixelOffset(value) => {
                require(object, Kind::Graphics)?;
                if let Some(value) = value {
                    status(gp::GdipSetPixelOffsetMode(p.cast(), value))?;
                }
                let mut mode = 0;
                status(gp::GdipGetPixelOffsetMode(p.cast(), &mut mode))?;
                Ok(Reply::Count(mode))
            }
        }
    }
}
