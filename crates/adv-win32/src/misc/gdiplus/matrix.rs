//! Matrix identities use the existing GDI+ object namespace and neutral affine math.
use super::*;
use adv_device::gdiplus::matrix::Matrix;
use std::sync::Mutex;
mod transform;

pub(super) struct MatrixOwner {
    value: Mutex<Matrix>,
    _runtime: Arc<Runtime>,
}
fn get(env: &Env<'_>, handle: u32) -> Result<Arc<MatrixOwner>, i32> {
    match value(env, handle) {
        Some(Value::Matrix(matrix)) => Ok(matrix.clone()),
        _ => Err(2),
    }
}
pub(super) fn elements(env: &Env<'_>, handle: u32) -> Result<Matrix, i32> {
    Ok(*get(env, handle)?.value.lock().map_err(|_| 1)?)
}
fn update(env: &Env<'_>, handle: u32, change: impl FnOnce(Matrix) -> Result<Matrix, i32>) -> i32 {
    let result = (|| {
        let object = get(env, handle)?;
        let mut matrix = object.value.lock().map_err(|_| 1)?;
        let next = change(*matrix)?;
        if !next.finite() {
            return Err(11);
        }
        *matrix = next;
        Ok(())
    })();
    result.err().unwrap_or(0)
}
fn create(env: &mut Env<'_>, matrix: Matrix, out: GuestPtr<()>) -> Outcome {
    if !clear(env, out.addr()) || !matrix.finite() {
        return Outcome::Return(2);
    }
    let result = runtime(env).map(|runtime| {
        Value::Matrix(Arc::new(MatrixOwner {
            value: Mutex::new(matrix),
            _runtime: runtime,
        }))
    });
    publish(env, out.addr(), result)
}
#[win32(gdiplus, name = "GdipCreateMatrix", level = implement)]
fn identity(env: &mut Env<'_>, out: GuestPtr<()>) -> Outcome {
    create(env, Matrix::IDENTITY, out)
}
#[allow(
    clippy::too_many_arguments,
    reason = "the SDK defines six coefficients and an output pointer"
)]
#[win32(gdiplus, name = "GdipCreateMatrix2", level = implement)]
fn explicit(
    env: &mut Env<'_>,
    a: f32,
    b: f32,
    c: f32,
    d: f32,
    x: f32,
    y: f32,
    out: GuestPtr<()>,
) -> Outcome {
    create(env, Matrix([a, b, c, d, x, y]), out)
}
fn number(env: &Env<'_>, pointer: u32, integer: bool) -> Result<f32, i32> {
    let value = env.cpu.read_u32(pointer).map_err(|_| 2)?;
    Ok(if integer {
        value as i32 as f32
    } else {
        f32::from_bits(value)
    })
}
fn rectangle_matrix(
    env: &mut Env<'_>,
    rect: u32,
    points: u32,
    out: GuestPtr<()>,
    integer: bool,
) -> Outcome {
    if !clear(env, out.addr()) {
        return Outcome::Return(2);
    }
    let values = (|| {
        adv_abi::guest::check_range(env.cpu, rect, 16, Prot::READ).map_err(|_| 2)?;
        adv_abi::guest::check_range(env.cpu, points, 24, Prot::READ).map_err(|_| 2)?;
        let mut rectangle = [0.; 4];
        let mut destination = [[0.; 2]; 3];
        for (i, value) in rectangle.iter_mut().enumerate() {
            *value = number(env, rect + i as u32 * 4, integer)?;
        }
        for (i, value) in destination.iter_mut().flatten().enumerate() {
            *value = number(env, points + i as u32 * 4, integer)?;
        }
        Matrix::rectangle(rectangle, destination).ok_or(2)
    })();
    match values {
        Ok(matrix) => create(env, matrix, out),
        Err(status) => Outcome::Return(status as u32),
    }
}
#[win32(gdiplus, name = "GdipCreateMatrix3", level = implement)]
fn from_rectangle(
    env: &mut Env<'_>,
    rect: GuestPtr<()>,
    points: GuestPtr<()>,
    out: GuestPtr<()>,
) -> Outcome {
    rectangle_matrix(env, rect.addr(), points.addr(), out, false)
}
#[win32(gdiplus, name = "GdipCreateMatrix3I", level = implement)]
fn from_rectangle_i(
    env: &mut Env<'_>,
    rect: GuestPtr<()>,
    points: GuestPtr<()>,
    out: GuestPtr<()>,
) -> Outcome {
    rectangle_matrix(env, rect.addr(), points.addr(), out, true)
}
#[win32(gdiplus, name = "GdipCloneMatrix", level = implement)]
fn clone_matrix(env: &mut Env<'_>, source: GuestPtr<()>, out: GuestPtr<()>) -> Outcome {
    if !clear(env, out.addr()) {
        return Outcome::Return(2);
    }
    match elements(env, source.addr()) {
        Ok(matrix) => create(env, matrix, out),
        Err(status) => Outcome::Return(status as u32),
    }
}
#[win32(gdiplus, name = "GdipDeleteMatrix", level = implement)]
fn delete_matrix(env: &mut Env<'_>, matrix: GuestPtr<()>) -> i32 {
    if get(env, matrix.addr()).is_ok() && remove(env, matrix.addr()) {
        0
    } else {
        2
    }
}
#[win32(gdiplus, name = "GdipGetMatrixElements", level = implement)]
fn read(env: &mut Env<'_>, matrix: GuestPtr<()>, out: GuestPtr<f32>) -> i32 {
    let result = (|| {
        let values = elements(env, matrix.addr())?;
        let mut bytes = [0; 24];
        for (chunk, value) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(values.0) {
            chunk.copy_from_slice(&value.to_le_bytes());
        }
        if out.is_null() {
            return Err(2);
        }
        env.cpu.write(out.addr(), &bytes).map_err(|_| 2)
    })();
    result.err().unwrap_or(0)
}
#[allow(
    clippy::too_many_arguments,
    reason = "the SDK defines a receiver and six coefficients"
)]
#[win32(gdiplus, name = "GdipSetMatrixElements", level = implement)]
fn write(
    env: &mut Env<'_>,
    matrix: GuestPtr<()>,
    a: f32,
    b: f32,
    c: f32,
    d: f32,
    x: f32,
    y: f32,
) -> i32 {
    let next = Matrix([a, b, c, d, x, y]);
    if !next.finite() {
        return 2;
    }
    update(env, matrix.addr(), |_| Ok(next))
}
fn query(
    env: &mut Env<'_>,
    matrix: u32,
    out: GuestPtr<i32>,
    predicate: impl FnOnce(Matrix) -> bool,
) -> i32 {
    let result = (|| {
        if out.is_null() {
            return Err(2);
        }
        let value = predicate(elements(env, matrix)?);
        out.write(env.cpu, i32::from(value)).map_err(|_| 2)
    })();
    result.err().unwrap_or(0)
}
#[win32(gdiplus, name = "GdipIsMatrixIdentity", level = implement)]
fn is_identity(env: &mut Env<'_>, matrix: GuestPtr<()>, out: GuestPtr<i32>) -> i32 {
    query(env, matrix.addr(), out, |matrix| matrix == Matrix::IDENTITY)
}
#[win32(gdiplus, name = "GdipIsMatrixInvertible", level = implement)]
fn is_invertible(env: &mut Env<'_>, matrix: GuestPtr<()>, out: GuestPtr<i32>) -> i32 {
    query(env, matrix.addr(), out, Matrix::invertible)
}
#[win32(gdiplus, name = "GdipIsMatrixEqual", level = implement)]
fn equal(env: &mut Env<'_>, a: GuestPtr<()>, b: GuestPtr<()>, out: GuestPtr<i32>) -> i32 {
    match elements(env, b.addr()) {
        Ok(b) => query(env, a.addr(), out, |a| a == b),
        Err(status) => status,
    }
}
pub(super) fn register(registry: &mut Registry) {
    for api in [
        identity_ABI,
        explicit_ABI,
        from_rectangle_ABI,
        from_rectangle_i_ABI,
        clone_matrix_ABI,
        delete_matrix_ABI,
        read_ABI,
        write_ABI,
        is_identity_ABI,
        is_invertible_ABI,
        equal_ABI,
    ] {
        registry.register(api);
    }
    transform::register(registry);
}
