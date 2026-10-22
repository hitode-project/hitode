//! CPU implementations keep storage and vector geometry inside device objects.
use super::*;
use std::sync::Mutex;
mod path;
mod raster;
use path::Path;

#[derive(Clone)]
pub(super) enum Data {
    Image {
        width: u32,
        height: u32,
        pixels: Vec<u8>,
    },
    Graphics(Graphics),
    Solid(u32),
    Path(Path),
    Format {
        flags: i32,
        language: u16,
        align: i32,
        line_align: i32,
    },
}
#[derive(Clone)]
pub(super) struct Graphics {
    target: Arc<Object>,
    clip: Option<Vec<[f32; 4]>>,
    smoothing: i32,
    text_hint: i32,
    pixel_offset: i32,
}

pub(super) fn create(runtime: &Arc<Runtime>, create: Create<'_>) -> Result<Arc<Object>, i32> {
    let (kind, data, backing) = match create {
        Create::Image(w, h, bytes) | Create::Canvas(w, h, bytes) => {
            let kind = if matches!(create, Create::Canvas(..)) {
                Kind::Canvas
            } else {
                Kind::Image
            };
            let len = u64::from(w) * u64::from(h) * 4;
            if w == 0 || h == 0 || len > crate::image::LIMIT as u64 || bytes.len() as u64 != len {
                return Err(2);
            }
            (
                kind,
                Data::Image {
                    width: w,
                    height: h,
                    pixels: bytes.to_vec(),
                },
                None,
            )
        }
        Create::Graphics(image) => {
            if !matches!(image.kind, Kind::Image | Kind::Canvas) {
                return Err(2);
            }
            (
                Kind::Graphics,
                Data::Graphics(Graphics {
                    target: image.clone(),
                    clip: None,
                    smoothing: 0,
                    text_hint: 0,
                    pixel_offset: 0,
                }),
                Some(image),
            )
        }
        Create::Solid(color) => (Kind::Brush, Data::Solid(color), None),
        Create::Path(mode) => (Kind::Path, Data::Path(Path::new(mode)?), None),
        Create::Format(flags, language) => (
            Kind::Format,
            Data::Format {
                flags,
                language,
                align: 0,
                line_align: 0,
            },
            None,
        ),
        Create::Clone(object) => {
            if !matches!(
                object.kind,
                Kind::Image | Kind::Brush | Kind::Path | Kind::Format
            ) {
                return Err(2);
            }
            (
                object.kind,
                object.data.lock().map_err(|_| 1)?.clone(),
                None,
            )
        }
        Create::Family(_) | Create::SansSerif | Create::Font(..) | Create::Gradient(_) => {
            return Err(6);
        }
    };
    Ok(Arc::new(Object {
        kind,
        _runtime: runtime.clone(),
        _backing: backing,
        data: Mutex::new(data),
    }))
}

pub(super) fn execute(object: &Object, operation: Op<'_>) -> Result<Reply, i32> {
    if object.kind == Kind::Graphics {
        return raster::execute(object, graphics(object)?, operation);
    }
    let mut data = object.data.lock().map_err(|_| 1)?;
    match (&mut *data, operation) {
        (Data::Image { pixels, .. }, Op::Pixels) => Ok(Reply::Pixels(pixels.clone())),
        (Data::Image { pixels, .. }, Op::Upload(bytes)) if pixels.len() == bytes.len() => {
            pixels.copy_from_slice(bytes);
            Ok(Reply::None)
        }
        (Data::Image { width, height, .. }, Op::Size) if object.kind == Kind::Image => {
            Ok(Reply::Size(*width, *height))
        }
        (Data::Format { align, .. }, Op::Align(value)) if (0..=2).contains(&value) => {
            *align = value;
            Ok(Reply::None)
        }
        (Data::Format { line_align, .. }, Op::LineAlign(value)) if (0..=2).contains(&value) => {
            *line_align = value;
            Ok(Reply::None)
        }
        (
            Data::Format {
                flags,
                language,
                align,
                line_align,
            },
            Op::StringProperty(index),
        ) => {
            let value = match index {
                0 => *flags,
                1 => i32::from(*language),
                2 => *align,
                3 => *line_align,
                _ => return Err(2),
            };
            Ok(Reply::Count(value))
        }
        (Data::Format { flags, .. }, Op::StringFlags(value)) => {
            *flags = value;
            Ok(Reply::None)
        }
        (Data::Path(path), Op::PathFill(value)) => {
            if let Some(value) = value {
                if !(0..=1).contains(&value) {
                    return Err(2);
                }
                path.winding = value == 1;
            }
            Ok(Reply::Count(i32::from(path.winding)))
        }
        (Data::Path(path), Op::ResetPath) => {
            *path = Path::new(0)?;
            Ok(Reply::None)
        }
        (Data::Path(path), Op::PathLine(points)) => {
            path.line(points)?;
            Ok(Reply::None)
        }
        (Data::Path(path), Op::PathArc(rect, start, sweep)) => {
            path.arc(rect, start, sweep)?;
            Ok(Reply::None)
        }
        (Data::Path(path), Op::ClosePath) => {
            path.close();
            Ok(Reply::None)
        }
        (Data::Path(path), Op::Bounds) => Ok(Reply::Bounds(path.bounds())),
        (Data::Path(path), Op::PointCount) => Ok(Reply::Count(path.count)),
        (Data::Path(_), Op::PathString { .. }) => Err(6),
        _ => Err(2),
    }
}

pub(super) fn graphics(object: &Object) -> Result<Graphics, i32> {
    match &*object.data.lock().map_err(|_| 1)? {
        Data::Graphics(g) => Ok(g.clone()),
        _ => Err(2),
    }
}
