//! Flat geometry copies guest arrays into the existing device-owned path.
#![allow(clippy::too_many_arguments)]
use super::*;
mod curves;
use adv_device::gdiplus::PathEdit;

pub(super) fn numbers<const N: usize>(
    env: &Env<'_>,
    pointer: u32,
    count: i32,
    integer: bool,
) -> Result<Vec<[f32; N]>, i32> {
    if pointer == 0 || count <= 0 {
        return Err(2);
    }
    if count > 65536 {
        return Err(3);
    }
    let mut bytes = vec![0; count as usize * N * 4];
    env.cpu.read(pointer, &mut bytes).map_err(|_| 2)?;
    let mut result = Vec::with_capacity(count as usize);
    for bytes in bytes.chunks_exact(N * 4) {
        let mut item = [0.; N];
        for (value, bytes) in item.iter_mut().zip(bytes.as_chunks::<4>().0) {
            *value = if integer {
                i32::from_le_bytes(*bytes) as f32
            } else {
                f32::from_le_bytes(*bytes)
            };
        }
        if item.iter().any(|n| !n.is_finite()) {
            return Err(2);
        }
        result.push(item);
    }
    Ok(result)
}
fn edit(env: &mut Env<'_>, handle: u32, edit: PathEdit<'_>) -> Outcome {
    result(
        object(env, handle).and_then(|path| path.execute(Op::PathEdit(edit))),
        env,
    )
}
fn create_points(
    env: &mut Env<'_>,
    points: u32,
    types: u32,
    count: i32,
    mode: i32,
    out: u32,
    integer: bool,
) -> Outcome {
    if !clear(env, out) {
        return Outcome::Return(2);
    }
    let value = (|| {
        let points = numbers::<2>(env, points, count, integer)?;
        if types == 0 {
            return Err(2);
        }
        let mut types_owned = vec![0; count as usize];
        env.cpu.read(types, &mut types_owned).map_err(|_| 2)?;
        runtime(env)?
            .create(Create::PathPoints(&points, &types_owned, mode))
            .map(Value::Object)
    })();
    publish(env, out, value)
}
fn array(
    env: &mut Env<'_>,
    path: u32,
    pointer: u32,
    count: i32,
    integer: bool,
    kind: u8,
) -> Outcome {
    let points = match numbers::<2>(env, pointer, count, integer) {
        Ok(points) => points,
        Err(status) => return Outcome::Return(status as u32),
    };
    if points.len()
        < if kind == 1 {
            3
        } else if kind == 2 {
            4
        } else {
            2
        }
        || kind == 2 && points.len() % 3 != 1
    {
        return Outcome::Return(2);
    }
    edit(
        env,
        path,
        if kind == 2 {
            PathEdit::Beziers(&points)
        } else {
            PathEdit::Lines(&points, kind == 1)
        },
    )
}
fn rectangles(env: &mut Env<'_>, path: u32, pointer: u32, count: i32, integer: bool) -> Outcome {
    let rectangles = match numbers::<4>(env, pointer, count, integer) {
        Ok(rectangles) => rectangles,
        Err(status) => return Outcome::Return(status as u32),
    };
    edit(env, path, PathEdit::Rectangles(&rectangles))
}
fn data(env: &Env<'_>, path: u32) -> Result<(Vec<[f32; 2]>, Vec<u8>), i32> {
    match object(env, path)?.execute(Op::PathData)? {
        Reply::PathData(points, types) => Ok((points, types)),
        _ => Err(2),
    }
}
fn put_points(
    env: &mut Env<'_>,
    pointer: u32,
    points: &[[f32; 2]],
    integer: bool,
) -> Result<(), i32> {
    if pointer == 0 {
        return Err(2);
    }
    let bytes: Vec<_> = points
        .iter()
        .flatten()
        .flat_map(|&value| {
            if integer {
                (value.round_ties_even() as i32).to_le_bytes()
            } else {
                value.to_le_bytes()
            }
        })
        .collect();
    env.cpu.write(pointer, &bytes).map_err(|_| 2)
}
fn get_points(env: &mut Env<'_>, path: u32, pointer: u32, count: i32, integer: bool) -> i32 {
    let value = (|| {
        if pointer == 0 || count <= 0 {
            return Err(2);
        }
        let (points, _) = data(env, path)?;
        if (count as usize) < points.len() {
            return Err(5);
        }
        put_points(env, pointer, &points, integer)
    })();
    value.err().unwrap_or(0)
}

#[win32(gdiplus, name = "GdipGetPathTypes", level = implement)]
fn types(env: &mut Env<'_>, path: GuestPtr<()>, out: GuestPtr<u8>, count: i32) -> i32 {
    let value = (|| {
        if out.is_null() || count <= 0 {
            return Err(2);
        }
        let (_, types) = data(env, path.addr())?;
        if (count as usize) < types.len() {
            return Err(5);
        }
        env.cpu.write(out.addr(), &types).map_err(|_| 2)
    })();
    value.err().unwrap_or(0)
}
#[win32(gdiplus, name = "GdipGetPathData", level = implement)]
fn path_data(env: &mut Env<'_>, path: GuestPtr<()>, out: GuestPtr<()>) -> i32 {
    let value = (|| {
        if out.is_null() {
            return Err(2);
        }
        let count = env.cpu.read_u32(out.addr()).map_err(|_| 2)? as i32;
        let points_at = env.cpu.read_u32(out.addr() + 4).map_err(|_| 2)?;
        let types_at = env.cpu.read_u32(out.addr() + 8).map_err(|_| 2)?;
        let (points, types) = data(env, path.addr())?;
        if count < 0 || (count as usize) < points.len() {
            return Err(5);
        }
        if points_at == 0 || types_at == 0 {
            return Err(2);
        }
        adv_abi::guest::check_range(env.cpu, points_at, points.len() as u32 * 8, Prot::WRITE)
            .map_err(|_| 2)?;
        adv_abi::guest::check_range(env.cpu, types_at, types.len() as u32, Prot::WRITE)
            .map_err(|_| 2)?;
        put_points(env, points_at, &points, false)?;
        env.cpu.write(types_at, &types).map_err(|_| 2)?;
        env.cpu
            .write_u32(out.addr(), points.len() as u32)
            .map_err(|_| 2)
    })();
    value.err().unwrap_or(0)
}
#[win32(gdiplus, name = "GdipGetPathLastPoint", level = implement)]
fn last_point(env: &mut Env<'_>, path: GuestPtr<()>, out: GuestPtr<()>) -> i32 {
    let value = (|| {
        let (points, _) = data(env, path.addr())?;
        let point = points.last().ok_or(2)?;
        put_points(env, out.addr(), std::slice::from_ref(point), false)
    })();
    value.err().unwrap_or(0)
}
#[win32(gdiplus, name = "GdipTransformPath", level = implement)]
fn transform(env: &mut Env<'_>, path: GuestPtr<()>, matrix: GuestPtr<()>) -> Outcome {
    if matrix.is_null() {
        return Outcome::Return(
            if object(env, path.addr()).is_ok_and(|path| path.kind == Kind::Path) {
                0
            } else {
                2
            },
        );
    }
    let matrix = match matrix::elements(env, matrix.addr()) {
        Ok(matrix) => matrix,
        Err(status) => return Outcome::Return(status as u32),
    };
    edit(env, path.addr(), PathEdit::Transform(matrix))
}

#[win32(gdiplus, name = "GdipCreatePath2", level = implement)]
fn gdipcreatepath2_f(
    env: &mut Env<'_>,
    points: GuestPtr<()>,
    types: GuestPtr<u8>,
    count: i32,
    mode: i32,
    out: GuestPtr<()>,
) -> Outcome {
    create_points(
        env,
        points.addr(),
        types.addr(),
        count,
        mode,
        out.addr(),
        false,
    )
}

#[win32(gdiplus, name = "GdipAddPathLine2", level = implement)]
fn gdipaddpathline2_f(
    env: &mut Env<'_>,
    path: GuestPtr<()>,
    points: GuestPtr<()>,
    count: i32,
) -> Outcome {
    array(env, path.addr(), points.addr(), count, false, 0)
}

#[win32(gdiplus, name = "GdipAddPathPolygon", level = implement)]
fn gdipaddpathpolygon_f(
    env: &mut Env<'_>,
    path: GuestPtr<()>,
    points: GuestPtr<()>,
    count: i32,
) -> Outcome {
    array(env, path.addr(), points.addr(), count, false, 1)
}

#[win32(gdiplus, name = "GdipAddPathBeziers", level = implement)]
fn gdipaddpathbeziers_f(
    env: &mut Env<'_>,
    path: GuestPtr<()>,
    points: GuestPtr<()>,
    count: i32,
) -> Outcome {
    array(env, path.addr(), points.addr(), count, false, 2)
}

#[win32(gdiplus, name = "GdipAddPathRectangles", level = implement)]
fn gdipaddpathrectangles_f(
    env: &mut Env<'_>,
    path: GuestPtr<()>,
    rectangles_at: GuestPtr<()>,
    count: i32,
) -> Outcome {
    rectangles(env, path.addr(), rectangles_at.addr(), count, false)
}

#[win32(gdiplus, name = "GdipAddPathRectangle", level = implement)]
fn gdipaddpathrectangle_f(
    env: &mut Env<'_>,
    path: GuestPtr<()>,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
) -> Outcome {
    edit(
        env,
        path.addr(),
        PathEdit::Rectangles(&[[x, y, width, height]]),
    )
}

#[win32(gdiplus, name = "GdipAddPathEllipse", level = implement)]
fn gdipaddpathellipse_f(
    env: &mut Env<'_>,
    path: GuestPtr<()>,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
) -> Outcome {
    edit(env, path.addr(), PathEdit::Ellipse([x, y, width, height]))
}

#[win32(gdiplus, name = "GdipAddPathPie", level = implement)]
fn gdipaddpathpie_f(
    env: &mut Env<'_>,
    path: GuestPtr<()>,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    start: f32,
    sweep: f32,
) -> Outcome {
    edit(
        env,
        path.addr(),
        PathEdit::Pie([x, y, width, height], start, sweep),
    )
}

#[win32(gdiplus, name = "GdipAddPathBezier", level = implement)]
fn gdipaddpathbezier_f(
    env: &mut Env<'_>,
    path: GuestPtr<()>,
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
    x3: f32,
    y3: f32,
    x4: f32,
    y4: f32,
) -> Outcome {
    edit(
        env,
        path.addr(),
        PathEdit::Beziers(&[[x1, y1], [x2, y2], [x3, y3], [x4, y4]]),
    )
}

#[win32(gdiplus, name = "GdipGetPathPoints", level = implement)]
fn get_points_f(env: &mut Env<'_>, path: GuestPtr<()>, out: GuestPtr<()>, count: i32) -> i32 {
    get_points(env, path.addr(), out.addr(), count, false)
}

#[win32(gdiplus, name = "GdipCreatePath2I", level = implement)]
fn gdipcreatepath2_i(
    env: &mut Env<'_>,
    points: GuestPtr<()>,
    types: GuestPtr<u8>,
    count: i32,
    mode: i32,
    out: GuestPtr<()>,
) -> Outcome {
    create_points(
        env,
        points.addr(),
        types.addr(),
        count,
        mode,
        out.addr(),
        true,
    )
}

#[win32(gdiplus, name = "GdipAddPathLine2I", level = implement)]
fn gdipaddpathline2_i(
    env: &mut Env<'_>,
    path: GuestPtr<()>,
    points: GuestPtr<()>,
    count: i32,
) -> Outcome {
    array(env, path.addr(), points.addr(), count, true, 0)
}

#[win32(gdiplus, name = "GdipAddPathPolygonI", level = implement)]
fn gdipaddpathpolygon_i(
    env: &mut Env<'_>,
    path: GuestPtr<()>,
    points: GuestPtr<()>,
    count: i32,
) -> Outcome {
    array(env, path.addr(), points.addr(), count, true, 1)
}

#[win32(gdiplus, name = "GdipAddPathBeziersI", level = implement)]
fn gdipaddpathbeziers_i(
    env: &mut Env<'_>,
    path: GuestPtr<()>,
    points: GuestPtr<()>,
    count: i32,
) -> Outcome {
    array(env, path.addr(), points.addr(), count, true, 2)
}

#[win32(gdiplus, name = "GdipAddPathRectanglesI", level = implement)]
fn gdipaddpathrectangles_i(
    env: &mut Env<'_>,
    path: GuestPtr<()>,
    rectangles_at: GuestPtr<()>,
    count: i32,
) -> Outcome {
    rectangles(env, path.addr(), rectangles_at.addr(), count, true)
}

#[win32(gdiplus, name = "GdipAddPathRectangleI", level = implement)]
fn gdipaddpathrectangle_i(
    env: &mut Env<'_>,
    path: GuestPtr<()>,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
) -> Outcome {
    edit(
        env,
        path.addr(),
        PathEdit::Rectangles(&[[x as f32, y as f32, width as f32, height as f32]]),
    )
}

#[win32(gdiplus, name = "GdipAddPathEllipseI", level = implement)]
fn gdipaddpathellipse_i(
    env: &mut Env<'_>,
    path: GuestPtr<()>,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
) -> Outcome {
    edit(
        env,
        path.addr(),
        PathEdit::Ellipse([x as f32, y as f32, width as f32, height as f32]),
    )
}

#[win32(gdiplus, name = "GdipAddPathPieI", level = implement)]
fn gdipaddpathpie_i(
    env: &mut Env<'_>,
    path: GuestPtr<()>,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    start: f32,
    sweep: f32,
) -> Outcome {
    edit(
        env,
        path.addr(),
        PathEdit::Pie(
            [x as f32, y as f32, width as f32, height as f32],
            start,
            sweep,
        ),
    )
}

#[win32(gdiplus, name = "GdipAddPathBezierI", level = implement)]
fn gdipaddpathbezier_i(
    env: &mut Env<'_>,
    path: GuestPtr<()>,
    x1: i32,
    y1: i32,
    x2: i32,
    y2: i32,
    x3: i32,
    y3: i32,
    x4: i32,
    y4: i32,
) -> Outcome {
    edit(
        env,
        path.addr(),
        PathEdit::Beziers(&[
            [x1 as f32, y1 as f32],
            [x2 as f32, y2 as f32],
            [x3 as f32, y3 as f32],
            [x4 as f32, y4 as f32],
        ]),
    )
}

#[win32(gdiplus, name = "GdipGetPathPointsI", level = implement)]
fn get_points_i(env: &mut Env<'_>, path: GuestPtr<()>, out: GuestPtr<()>, count: i32) -> i32 {
    get_points(env, path.addr(), out.addr(), count, true)
}

#[win32(gdiplus, name = "GdipStartPathFigure", level = implement)]
fn gdipstartpathfigure(env: &mut Env<'_>, path: GuestPtr<()>) -> Outcome {
    edit(env, path.addr(), PathEdit::Start)
}

#[win32(gdiplus, name = "GdipClosePathFigures", level = implement)]
fn gdipclosepathfigures(env: &mut Env<'_>, path: GuestPtr<()>) -> Outcome {
    edit(env, path.addr(), PathEdit::CloseAll)
}

#[win32(gdiplus, name = "GdipSetPathMarker", level = implement)]
fn gdipsetpathmarker(env: &mut Env<'_>, path: GuestPtr<()>) -> Outcome {
    edit(env, path.addr(), PathEdit::Marker(true))
}

#[win32(gdiplus, name = "GdipClearPathMarkers", level = implement)]
fn gdipclearpathmarkers(env: &mut Env<'_>, path: GuestPtr<()>) -> Outcome {
    edit(env, path.addr(), PathEdit::Marker(false))
}

pub(super) fn register(registry: &mut Registry) {
    curves::register(registry);
    for api in [
        types_ABI,
        path_data_ABI,
        last_point_ABI,
        transform_ABI,
        gdipcreatepath2_f_ABI,
        gdipaddpathline2_f_ABI,
        gdipaddpathpolygon_f_ABI,
        gdipaddpathbeziers_f_ABI,
        gdipaddpathrectangles_f_ABI,
        gdipaddpathrectangle_f_ABI,
        gdipaddpathellipse_f_ABI,
        gdipaddpathpie_f_ABI,
        gdipaddpathbezier_f_ABI,
        get_points_f_ABI,
        gdipcreatepath2_i_ABI,
        gdipaddpathline2_i_ABI,
        gdipaddpathpolygon_i_ABI,
        gdipaddpathbeziers_i_ABI,
        gdipaddpathrectangles_i_ABI,
        gdipaddpathrectangle_i_ABI,
        gdipaddpathellipse_i_ABI,
        gdipaddpathpie_i_ABI,
        gdipaddpathbezier_i_ABI,
        get_points_i_ABI,
        gdipstartpathfigure_ABI,
        gdipclosepathfigures_ABI,
        gdipsetpathmarker_ABI,
        gdipclearpathmarkers_ABI,
    ] {
        registry.register(api);
    }
}
