//! Cardinal spline aliases share array validation and device path ownership.
use super::*;
fn curve(
    env: &mut Env<'_>,
    path: u32,
    pointer: u32,
    count: i32,
    offset: i32,
    segments: i32,
    tension: f32,
    closed: bool,
    integer: bool,
) -> Outcome {
    let points = match numbers::<2>(env, pointer, count, integer) {
        Ok(points) => points,
        Err(status) => return Outcome::Return(status as u32),
    };
    edit(
        env,
        path,
        PathEdit::Curve {
            points: &points,
            offset,
            segments,
            tension,
            closed,
        },
    )
}

#[win32(gdiplus, name = "GdipAddPathCurve", level = implement)]
fn open1_f(env: &mut Env<'_>, path: GuestPtr<()>, points: GuestPtr<()>, count: i32) -> Outcome {
    curve(
        env,
        path.addr(),
        points.addr(),
        count,
        0,
        count.saturating_sub(1),
        0.5,
        false,
        false,
    )
}

#[win32(gdiplus, name = "GdipAddPathCurve2", level = implement)]
fn open2_f(
    env: &mut Env<'_>,
    path: GuestPtr<()>,
    points: GuestPtr<()>,
    count: i32,
    tension: f32,
) -> Outcome {
    curve(
        env,
        path.addr(),
        points.addr(),
        count,
        0,
        count.saturating_sub(1),
        tension,
        false,
        false,
    )
}

#[win32(gdiplus, name = "GdipAddPathCurve3", level = implement)]
fn open3_f(
    env: &mut Env<'_>,
    path: GuestPtr<()>,
    points: GuestPtr<()>,
    count: i32,
    offset: i32,
    segments: i32,
    tension: f32,
) -> Outcome {
    curve(
        env,
        path.addr(),
        points.addr(),
        count,
        offset,
        segments,
        tension,
        false,
        false,
    )
}

#[win32(gdiplus, name = "GdipAddPathClosedCurve", level = implement)]
fn closed1_f(env: &mut Env<'_>, path: GuestPtr<()>, points: GuestPtr<()>, count: i32) -> Outcome {
    curve(
        env,
        path.addr(),
        points.addr(),
        count,
        0,
        count,
        0.5,
        true,
        false,
    )
}

#[win32(gdiplus, name = "GdipAddPathClosedCurve2", level = implement)]
fn closed2_f(
    env: &mut Env<'_>,
    path: GuestPtr<()>,
    points: GuestPtr<()>,
    count: i32,
    tension: f32,
) -> Outcome {
    curve(
        env,
        path.addr(),
        points.addr(),
        count,
        0,
        count,
        tension,
        true,
        false,
    )
}

#[win32(gdiplus, name = "GdipAddPathCurveI", level = implement)]
fn open1_i(env: &mut Env<'_>, path: GuestPtr<()>, points: GuestPtr<()>, count: i32) -> Outcome {
    curve(
        env,
        path.addr(),
        points.addr(),
        count,
        0,
        count.saturating_sub(1),
        0.5,
        false,
        true,
    )
}

#[win32(gdiplus, name = "GdipAddPathCurve2I", level = implement)]
fn open2_i(
    env: &mut Env<'_>,
    path: GuestPtr<()>,
    points: GuestPtr<()>,
    count: i32,
    tension: f32,
) -> Outcome {
    curve(
        env,
        path.addr(),
        points.addr(),
        count,
        0,
        count.saturating_sub(1),
        tension,
        false,
        true,
    )
}

#[win32(gdiplus, name = "GdipAddPathCurve3I", level = implement)]
fn open3_i(
    env: &mut Env<'_>,
    path: GuestPtr<()>,
    points: GuestPtr<()>,
    count: i32,
    offset: i32,
    segments: i32,
    tension: f32,
) -> Outcome {
    curve(
        env,
        path.addr(),
        points.addr(),
        count,
        offset,
        segments,
        tension,
        false,
        true,
    )
}

#[win32(gdiplus, name = "GdipAddPathClosedCurveI", level = implement)]
fn closed1_i(env: &mut Env<'_>, path: GuestPtr<()>, points: GuestPtr<()>, count: i32) -> Outcome {
    curve(
        env,
        path.addr(),
        points.addr(),
        count,
        0,
        count,
        0.5,
        true,
        true,
    )
}

#[win32(gdiplus, name = "GdipAddPathClosedCurve2I", level = implement)]
fn closed2_i(
    env: &mut Env<'_>,
    path: GuestPtr<()>,
    points: GuestPtr<()>,
    count: i32,
    tension: f32,
) -> Outcome {
    curve(
        env,
        path.addr(),
        points.addr(),
        count,
        0,
        count,
        tension,
        true,
        true,
    )
}

pub(super) fn register(registry: &mut Registry) {
    for api in [
        open1_f_ABI,
        open2_f_ABI,
        open3_f_ABI,
        closed1_f_ABI,
        closed2_f_ABI,
        open1_i_ABI,
        open2_i_ABI,
        open3_i_ABI,
        closed1_i_ABI,
        closed2_i_ABI,
    ] {
        registry.register(api);
    }
}
