//! System path editing uses private SDK arrays, never guest memory.
use super::*;
fn points(input: &[[f32; 2]]) -> Vec<gp::PointF> {
    input
        .iter()
        .map(|&[x, y]| gp::PointF { X: x, Y: y })
        .collect()
}
pub(super) unsafe fn create(
    input: &[[f32; 2]],
    types: &[u8],
    mode: i32,
    out: &mut *mut c_void,
) -> i32 {
    if input.is_empty() || input.len() != types.len() || input.len() > i32::MAX as usize {
        return 2;
    }
    let points = points(input);
    unsafe {
        gp::GdipCreatePath2(
            points.as_ptr(),
            types.as_ptr(),
            points.len() as i32,
            mode,
            (out as *mut *mut c_void).cast(),
        )
    }
}
pub(super) fn data(path: *mut c_void) -> Result<Reply, i32> {
    let mut count = 0;
    unsafe {
        status(gp::GdipGetPointCount(path.cast(), &mut count))?;
    }
    if !(0..=65536).contains(&count) {
        return Err(3);
    }
    let mut points: Vec<gp::PointF> = (0..count).map(|_| gp::PointF { X: 0., Y: 0. }).collect();
    let mut types = vec![0; count as usize];
    if count != 0 {
        unsafe {
            status(gp::GdipGetPathPoints(
                path.cast(),
                points.as_mut_ptr(),
                count,
            ))?;
            status(gp::GdipGetPathTypes(path.cast(), types.as_mut_ptr(), count))?;
        }
    }
    Ok(Reply::PathData(
        points.iter().map(|p| [p.X, p.Y]).collect(),
        types,
    ))
}
pub(super) fn edit(path: *mut c_void, edit: PathEdit<'_>) -> Result<Reply, i32> {
    let hr = unsafe {
        match edit {
            PathEdit::Lines(input, polygon) => {
                let points = points(input);
                if polygon {
                    gp::GdipAddPathPolygon(path.cast(), points.as_ptr(), points.len() as i32)
                } else {
                    gp::GdipAddPathLine2(path.cast(), points.as_ptr(), points.len() as i32)
                }
            }
            PathEdit::Curve {
                points: input,
                offset,
                segments,
                tension,
                closed,
            } => {
                let points = points(input);
                if closed {
                    gp::GdipAddPathClosedCurve2(
                        path.cast(),
                        points.as_ptr(),
                        points.len() as i32,
                        tension,
                    )
                } else {
                    gp::GdipAddPathCurve3(
                        path.cast(),
                        points.as_ptr(),
                        points.len() as i32,
                        offset,
                        segments,
                        tension,
                    )
                }
            }
            PathEdit::Beziers(input) => {
                let points = points(input);
                gp::GdipAddPathBeziers(path.cast(), points.as_ptr(), points.len() as i32)
            }
            PathEdit::Rectangles(input) => {
                let rectangles: Vec<_> = input.iter().copied().map(rect).collect();
                gp::GdipAddPathRectangles(path.cast(), rectangles.as_ptr(), rectangles.len() as i32)
            }
            PathEdit::Ellipse([x, y, w, h]) => gp::GdipAddPathEllipse(path.cast(), x, y, w, h),
            PathEdit::Pie([x, y, w, h], start, sweep) => {
                gp::GdipAddPathPie(path.cast(), x, y, w, h, start, sweep)
            }
            PathEdit::Start => gp::GdipStartPathFigure(path.cast()),
            PathEdit::CloseAll => gp::GdipClosePathFigures(path.cast()),
            PathEdit::Marker(true) => gp::GdipSetPathMarker(path.cast()),
            PathEdit::Marker(false) => gp::GdipClearPathMarkers(path.cast()),
            PathEdit::Transform(matrix) => {
                let [a, b, c, d, x, y] = matrix.0;
                let mut native = std::ptr::null_mut();
                status(gp::GdipCreateMatrix2(a, b, c, d, x, y, &mut native))?;
                let hr = gp::GdipTransformPath(path.cast(), native);
                gp::GdipDeleteMatrix(native);
                hr
            }
        }
    };
    status(hr)?;
    Ok(Reply::None)
}
