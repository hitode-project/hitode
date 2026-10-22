//! CPU implementations keep storage and vector geometry inside device objects.
use super::*;
use std::sync::Mutex;
mod path;
mod raster;
mod string_format;
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
    Format(string_format::Format),
}
#[derive(Clone)]
pub(super) struct Graphics {
    target: Arc<Object>,
    clip: Option<Vec<[f32; 4]>>,
    smoothing: i32,
    text_hint: i32,
    pixel_offset: i32,
    world: matrix::Matrix,
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
                    world: matrix::Matrix::IDENTITY,
                }),
                Some(image),
            )
        }
        Create::Solid(color) => (Kind::Brush, Data::Solid(color), None),
        Create::Path(mode) => (Kind::Path, Data::Path(Path::new(mode)?), None),
        Create::PathPoints(points, types, mode) => (
            Kind::Path,
            Data::Path(Path::from_points(points, types, mode)?),
            None,
        ),
        Create::Format(flags, _) => (
            Kind::Format,
            Data::Format(string_format::Format::new(flags)),
            None,
        ),
        Create::GenericFormat(typographic) => (
            Kind::Format,
            Data::Format(string_format::Format::generic(typographic)),
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
        (Data::Path(path), Op::PathData) => {
            let (points, types) = path.data();
            Ok(Reply::PathData(points, types))
        }
        (Data::Path(path), Op::PathEdit(edit)) => {
            path.edit(edit)?;
            Ok(Reply::None)
        }
        (Data::Format(format), op) => format.execute(op),
        (Data::Solid(_), Op::BrushType) => Ok(Reply::Count(0)),
        (Data::Solid(color), Op::SolidColor(value)) => {
            if let Some(value) = value {
                *color = value;
            }
            Ok(Reply::Count(*color as i32))
        }
        (Data::Image { pixels, .. }, Op::Pixels) => Ok(Reply::Pixels(pixels.clone())),
        (Data::Image { pixels, .. }, Op::Upload(bytes)) if pixels.len() == bytes.len() => {
            pixels.copy_from_slice(bytes);
            Ok(Reply::None)
        }
        (Data::Image { width, height, .. }, Op::Size) if object.kind == Kind::Image => {
            Ok(Reply::Size(*width, *height))
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
