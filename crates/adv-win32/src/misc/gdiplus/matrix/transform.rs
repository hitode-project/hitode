//! Affine composition and in-place point/vector transforms preserve failed inputs.
use super::*;

fn combine(env: &Env<'_>, handle: u32, other: Matrix, order: i32) -> i32 {
    if !(0..=1).contains(&order) {
        return 2;
    }
    update(env, handle, |matrix| {
        Ok(if order == 0 {
            other.multiply(matrix)
        } else {
            matrix.multiply(other)
        })
    })
}
#[win32(gdiplus, name = "GdipMultiplyMatrix", level = implement)]
fn multiply(env: &mut Env<'_>, matrix: GuestPtr<()>, other: GuestPtr<()>, order: i32) -> i32 {
    match elements(env, other.addr()) {
        Ok(other) => combine(env, matrix.addr(), other, order),
        Err(status) => status,
    }
}
#[win32(gdiplus, name = "GdipInvertMatrix", level = implement)]
fn invert(env: &mut Env<'_>, matrix: GuestPtr<()>) -> i32 {
    update(env, matrix.addr(), |matrix| matrix.inverse().ok_or(2))
}
#[win32(gdiplus, name = "GdipTranslateMatrix", level = implement)]
fn translate(env: &mut Env<'_>, matrix: GuestPtr<()>, x: f32, y: f32, order: i32) -> i32 {
    combine(env, matrix.addr(), Matrix([1., 0., 0., 1., x, y]), order)
}
#[win32(gdiplus, name = "GdipScaleMatrix", level = implement)]
fn scale(env: &mut Env<'_>, matrix: GuestPtr<()>, x: f32, y: f32, order: i32) -> i32 {
    combine(env, matrix.addr(), Matrix([x, 0., 0., y, 0., 0.]), order)
}
#[win32(gdiplus, name = "GdipRotateMatrix", level = implement)]
fn rotate(env: &mut Env<'_>, matrix: GuestPtr<()>, angle: f32, order: i32) -> i32 {
    let (sin, cos) = f64::from(angle).to_radians().sin_cos();
    combine(
        env,
        matrix.addr(),
        Matrix([cos as f32, sin as f32, -sin as f32, cos as f32, 0., 0.]),
        order,
    )
}
#[win32(gdiplus, name = "GdipShearMatrix", level = implement)]
fn shear(env: &mut Env<'_>, matrix: GuestPtr<()>, x: f32, y: f32, order: i32) -> i32 {
    combine(env, matrix.addr(), Matrix([1., y, x, 1., 0., 0.]), order)
}
fn point_words(
    env: &Env<'_>,
    at: u32,
    matrix: Matrix,
    integer: bool,
    vector: bool,
) -> Result<[u32; 2], i32> {
    let mut bytes = [0; 8];
    env.cpu.read(at, &mut bytes).map_err(|_| 2)?;
    let mut input = [0.; 2];
    for (value, bytes) in input.iter_mut().zip(bytes.as_chunks::<4>().0.iter()) {
        let bits = u32::from_le_bytes(*bytes);
        *value = if integer {
            bits as i32 as f64
        } else {
            f64::from(f32::from_bits(bits))
        };
    }
    if input.iter().any(|x| !x.is_finite()) {
        return Err(2);
    }
    let output = matrix.point64(input, vector);
    let mut words = [0; 2];
    for (word, value) in words.iter_mut().zip(output) {
        *word = if integer {
            let rounded = (value + 0.5).floor();
            if !rounded.is_finite()
                || rounded < f64::from(i32::MIN)
                || rounded > f64::from(i32::MAX)
            {
                return Err(11);
            }
            rounded as i32 as u32
        } else {
            let value = value as f32;
            if !value.is_finite() {
                return Err(11);
            }
            value.to_bits()
        };
    }
    Ok(words)
}
fn points(
    env: &mut Env<'_>,
    matrix: u32,
    points: u32,
    count: i32,
    integer: bool,
    vector: bool,
) -> i32 {
    let result = (|| {
        if count <= 0 {
            return Err(2);
        }
        let length = (count as u32).checked_mul(8).ok_or(2)?;
        adv_abi::guest::check_range(env.cpu, points, length, Prot::READ | Prot::WRITE)
            .map_err(|_| 2)?;
        let matrix = elements(env, matrix)?;
        // Validate every result before writing, without allocating a second point array.
        for i in 0..count as u32 {
            point_words(env, points + i * 8, matrix, integer, vector)?;
        }
        for i in 0..count as u32 {
            let words = point_words(env, points + i * 8, matrix, integer, vector)?;
            let mut bytes = [0; 8];
            for (chunk, value) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(words) {
                chunk.copy_from_slice(&value.to_le_bytes());
            }
            env.cpu.write(points + i * 8, &bytes).map_err(|_| 2)?;
        }
        Ok(())
    })();
    result.err().unwrap_or(0)
}
#[win32(gdiplus, name = "GdipTransformMatrixPoints", level = implement)]
fn positions(env: &mut Env<'_>, matrix: GuestPtr<()>, data: GuestPtr<()>, count: i32) -> i32 {
    points(env, matrix.addr(), data.addr(), count, false, false)
}
#[win32(gdiplus, name = "GdipTransformMatrixPointsI", level = implement)]
fn positions_i(env: &mut Env<'_>, matrix: GuestPtr<()>, data: GuestPtr<()>, count: i32) -> i32 {
    points(env, matrix.addr(), data.addr(), count, true, false)
}
#[win32(gdiplus, name = "GdipVectorTransformMatrixPoints", level = implement)]
fn vectors(env: &mut Env<'_>, matrix: GuestPtr<()>, data: GuestPtr<()>, count: i32) -> i32 {
    points(env, matrix.addr(), data.addr(), count, false, true)
}
#[win32(gdiplus, name = "GdipVectorTransformMatrixPointsI", level = implement)]
fn vectors_i(env: &mut Env<'_>, matrix: GuestPtr<()>, data: GuestPtr<()>, count: i32) -> i32 {
    points(env, matrix.addr(), data.addr(), count, true, true)
}
pub(super) fn register(registry: &mut Registry) {
    for api in [
        multiply_ABI,
        invert_ABI,
        translate_ABI,
        scale_ABI,
        rotate_ABI,
        shear_ABI,
        positions_ABI,
        positions_i_ABI,
        vectors_ABI,
        vectors_i_ABI,
    ] {
        registry.register(api);
    }
}
