//! Filled primitives use transient paths and the same raster/writeback owner.
#![allow(clippy::too_many_arguments)]
use super::*;
use adv_device::gdiplus::PathEdit;
fn fill(env: &mut Env<'_>, graphics: u32, brush: u32, mode: i32, edit: PathEdit<'_>) -> Outcome {
    let prepared = (|| {
        let brush = object(env, brush)?;
        let path = runtime(env)?.create(Create::Path(mode))?;
        path.execute(Op::PathEdit(edit))?;
        Ok::<_, i32>((brush, path))
    })();
    match prepared {
        Ok((brush, path)) => draw(env, graphics, Op::FillPath(&brush, &path)),
        Err(6) => missing_device_operation(env),
        Err(status) => Outcome::Return(status as u32),
    }
}

#[win32(gdiplus, name="GdipFillRectangle", level=implement)]
fn rectangle_f(
    env: &mut Env<'_>,
    graphics: GuestPtr<()>,
    brush: GuestPtr<()>,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
) -> Outcome {
    fill(
        env,
        graphics.addr(),
        brush.addr(),
        0,
        PathEdit::Rectangles(&[[x, y, width, height]]),
    )
}

#[win32(gdiplus, name="GdipFillEllipse", level=implement)]
fn ellipse_f(
    env: &mut Env<'_>,
    graphics: GuestPtr<()>,
    brush: GuestPtr<()>,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
) -> Outcome {
    fill(
        env,
        graphics.addr(),
        brush.addr(),
        0,
        PathEdit::Ellipse([x, y, width, height]),
    )
}

#[win32(gdiplus, name="GdipFillPie", level=implement)]
fn pie_f(
    env: &mut Env<'_>,
    graphics: GuestPtr<()>,
    brush: GuestPtr<()>,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    start: f32,
    sweep: f32,
) -> Outcome {
    fill(
        env,
        graphics.addr(),
        brush.addr(),
        0,
        PathEdit::Pie([x, y, width, height], start, sweep),
    )
}

#[win32(gdiplus, name="GdipFillPolygon", level=implement)]
fn polygon1_f(
    env: &mut Env<'_>,
    graphics: GuestPtr<()>,
    brush: GuestPtr<()>,
    points: GuestPtr<()>,
    count: i32,
    mode: i32,
) -> Outcome {
    let points = match path::numbers::<2>(env, points.addr(), count, false) {
        Ok(p) => p,
        Err(status) => return Outcome::Return(status as u32),
    };
    fill(
        env,
        graphics.addr(),
        brush.addr(),
        mode,
        PathEdit::Lines(&points, true),
    )
}

#[win32(gdiplus, name="GdipFillPolygon2", level=implement)]
fn polygon2_f(
    env: &mut Env<'_>,
    graphics: GuestPtr<()>,
    brush: GuestPtr<()>,
    points: GuestPtr<()>,
    count: i32,
) -> Outcome {
    let points = match path::numbers::<2>(env, points.addr(), count, false) {
        Ok(p) => p,
        Err(status) => return Outcome::Return(status as u32),
    };
    fill(
        env,
        graphics.addr(),
        brush.addr(),
        0,
        PathEdit::Lines(&points, true),
    )
}

#[win32(gdiplus, name="GdipFillClosedCurve", level=implement)]
fn closedcurve1_f(
    env: &mut Env<'_>,
    graphics: GuestPtr<()>,
    brush: GuestPtr<()>,
    points: GuestPtr<()>,
    count: i32,
) -> Outcome {
    let points = match path::numbers::<2>(env, points.addr(), count, false) {
        Ok(p) => p,
        Err(status) => return Outcome::Return(status as u32),
    };
    fill(
        env,
        graphics.addr(),
        brush.addr(),
        0,
        PathEdit::Curve {
            points: &points,
            offset: 0,
            segments: count,
            tension: 0.5,
            closed: true,
        },
    )
}

#[win32(gdiplus, name="GdipFillClosedCurve2", level=implement)]
fn closedcurve2_f(
    env: &mut Env<'_>,
    graphics: GuestPtr<()>,
    brush: GuestPtr<()>,
    points: GuestPtr<()>,
    count: i32,
    tension: f32,
    mode: i32,
) -> Outcome {
    let points = match path::numbers::<2>(env, points.addr(), count, false) {
        Ok(p) => p,
        Err(status) => return Outcome::Return(status as u32),
    };
    fill(
        env,
        graphics.addr(),
        brush.addr(),
        mode,
        PathEdit::Curve {
            points: &points,
            offset: 0,
            segments: count,
            tension,
            closed: true,
        },
    )
}

#[win32(gdiplus, name="GdipFillRectangleI", level=implement)]
fn rectangle_i(
    env: &mut Env<'_>,
    graphics: GuestPtr<()>,
    brush: GuestPtr<()>,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
) -> Outcome {
    fill(
        env,
        graphics.addr(),
        brush.addr(),
        0,
        PathEdit::Rectangles(&[[x as f32, y as f32, width as f32, height as f32]]),
    )
}

#[win32(gdiplus, name="GdipFillEllipseI", level=implement)]
fn ellipse_i(
    env: &mut Env<'_>,
    graphics: GuestPtr<()>,
    brush: GuestPtr<()>,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
) -> Outcome {
    fill(
        env,
        graphics.addr(),
        brush.addr(),
        0,
        PathEdit::Ellipse([x as f32, y as f32, width as f32, height as f32]),
    )
}

#[win32(gdiplus, name="GdipFillPieI", level=implement)]
fn pie_i(
    env: &mut Env<'_>,
    graphics: GuestPtr<()>,
    brush: GuestPtr<()>,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    start: f32,
    sweep: f32,
) -> Outcome {
    fill(
        env,
        graphics.addr(),
        brush.addr(),
        0,
        PathEdit::Pie(
            [x as f32, y as f32, width as f32, height as f32],
            start,
            sweep,
        ),
    )
}

#[win32(gdiplus, name="GdipFillPolygonI", level=implement)]
fn polygon1_i(
    env: &mut Env<'_>,
    graphics: GuestPtr<()>,
    brush: GuestPtr<()>,
    points: GuestPtr<()>,
    count: i32,
    mode: i32,
) -> Outcome {
    let points = match path::numbers::<2>(env, points.addr(), count, true) {
        Ok(p) => p,
        Err(status) => return Outcome::Return(status as u32),
    };
    fill(
        env,
        graphics.addr(),
        brush.addr(),
        mode,
        PathEdit::Lines(&points, true),
    )
}

#[win32(gdiplus, name="GdipFillPolygon2I", level=implement)]
fn polygon2_i(
    env: &mut Env<'_>,
    graphics: GuestPtr<()>,
    brush: GuestPtr<()>,
    points: GuestPtr<()>,
    count: i32,
) -> Outcome {
    let points = match path::numbers::<2>(env, points.addr(), count, true) {
        Ok(p) => p,
        Err(status) => return Outcome::Return(status as u32),
    };
    fill(
        env,
        graphics.addr(),
        brush.addr(),
        0,
        PathEdit::Lines(&points, true),
    )
}

#[win32(gdiplus, name="GdipFillClosedCurveI", level=implement)]
fn closedcurve1_i(
    env: &mut Env<'_>,
    graphics: GuestPtr<()>,
    brush: GuestPtr<()>,
    points: GuestPtr<()>,
    count: i32,
) -> Outcome {
    let points = match path::numbers::<2>(env, points.addr(), count, true) {
        Ok(p) => p,
        Err(status) => return Outcome::Return(status as u32),
    };
    fill(
        env,
        graphics.addr(),
        brush.addr(),
        0,
        PathEdit::Curve {
            points: &points,
            offset: 0,
            segments: count,
            tension: 0.5,
            closed: true,
        },
    )
}

#[win32(gdiplus, name="GdipFillClosedCurve2I", level=implement)]
fn closedcurve2_i(
    env: &mut Env<'_>,
    graphics: GuestPtr<()>,
    brush: GuestPtr<()>,
    points: GuestPtr<()>,
    count: i32,
    tension: f32,
    mode: i32,
) -> Outcome {
    let points = match path::numbers::<2>(env, points.addr(), count, true) {
        Ok(p) => p,
        Err(status) => return Outcome::Return(status as u32),
    };
    fill(
        env,
        graphics.addr(),
        brush.addr(),
        mode,
        PathEdit::Curve {
            points: &points,
            offset: 0,
            segments: count,
            tension,
            closed: true,
        },
    )
}

pub(super) fn register(registry: &mut Registry) {
    for api in [
        rectangle_f_ABI,
        ellipse_f_ABI,
        pie_f_ABI,
        polygon1_f_ABI,
        polygon2_f_ABI,
        closedcurve1_f_ABI,
        closedcurve2_f_ABI,
        rectangle_i_ABI,
        ellipse_i_ABI,
        pie_i_ABI,
        polygon1_i_ABI,
        polygon2_i_ABI,
        closedcurve1_i_ABI,
        closedcurve2_i_ABI,
    ] {
        registry.register(api);
    }
}
