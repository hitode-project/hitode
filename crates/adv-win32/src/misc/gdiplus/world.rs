//! Graphics transforms copy matrix values and use the same affine device service.
use super::*;
use adv_device::gdiplus::matrix::Matrix;

fn apply(env: &mut Env<'_>, graphics: u32, matrix: Matrix, order: i32) -> Outcome {
    if !(0..=1).contains(&order) || !matrix.finite() {
        return Outcome::Return(2);
    }
    result(
        object(env, graphics)
            .and_then(|graphics| graphics.execute(Op::ComposeWorld(matrix, order == 0))),
        env,
    )
}
#[win32(gdiplus,name="GdipGetWorldTransform",level=implement)]
fn get(env: &mut Env<'_>, graphics: GuestPtr<()>, out: GuestPtr<()>) -> Outcome {
    let value = object(env, graphics.addr()).and_then(|graphics| graphics.execute(Op::World(None)));
    match value {
        Ok(Reply::Matrix(value)) => Outcome::Return(matrix::assign(env, out.addr(), value) as u32),
        Err(6) => missing_device_operation(env),
        Err(status) => Outcome::Return(status as u32),
        _ => Outcome::Return(2),
    }
}
#[win32(gdiplus,name="GdipSetWorldTransform",level=implement)]
fn set(env: &mut Env<'_>, graphics: GuestPtr<()>, source: GuestPtr<()>) -> Outcome {
    let value = matrix::elements(env, source.addr())
        .and_then(|matrix| object(env, graphics.addr())?.execute(Op::World(Some(matrix))));
    result(value, env)
}
#[win32(gdiplus,name="GdipResetWorldTransform",level=implement)]
fn reset(env: &mut Env<'_>, graphics: GuestPtr<()>) -> Outcome {
    result(
        object(env, graphics.addr())
            .and_then(|graphics| graphics.execute(Op::World(Some(Matrix::IDENTITY)))),
        env,
    )
}
#[win32(gdiplus,name="GdipMultiplyWorldTransform",level=implement)]
fn multiply(env: &mut Env<'_>, graphics: GuestPtr<()>, other: GuestPtr<()>, order: i32) -> Outcome {
    match matrix::elements(env, other.addr()) {
        Ok(matrix) => apply(env, graphics.addr(), matrix, order),
        Err(status) => Outcome::Return(status as u32),
    }
}
#[win32(gdiplus,name="GdipTranslateWorldTransform",level=implement)]
fn translate(env: &mut Env<'_>, graphics: GuestPtr<()>, x: f32, y: f32, order: i32) -> Outcome {
    apply(env, graphics.addr(), Matrix([1., 0., 0., 1., x, y]), order)
}
#[win32(gdiplus,name="GdipScaleWorldTransform",level=implement)]
fn scale(env: &mut Env<'_>, graphics: GuestPtr<()>, x: f32, y: f32, order: i32) -> Outcome {
    apply(env, graphics.addr(), Matrix([x, 0., 0., y, 0., 0.]), order)
}
#[win32(gdiplus,name="GdipRotateWorldTransform",level=implement)]
fn rotate(env: &mut Env<'_>, graphics: GuestPtr<()>, angle: f32, order: i32) -> Outcome {
    if !angle.is_finite() {
        return Outcome::Return(2);
    }
    let (sin, cos) = f64::from(angle).to_radians().sin_cos();
    apply(
        env,
        graphics.addr(),
        Matrix([cos as f32, sin as f32, -sin as f32, cos as f32, 0., 0.]),
        order,
    )
}
pub(super) fn register(registry: &mut Registry) {
    for api in [
        get_ABI,
        set_ABI,
        reset_ABI,
        multiply_ABI,
        translate_ABI,
        scale_ABI,
        rotate_ABI,
    ] {
        registry.register(api);
    }
}
