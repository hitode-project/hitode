//! Legacy D3DX9 matrix/vector contracts and native binary Effect execution.
//! Effect shader objects feed the shared D3D9 programmable drawing frontend.
//! Authoritative prototypes and the scoped ABI input are in data/abi/sdk-source.json.
use adv_abi::{GuestPtr, Registry, types::D3DVIEWPORT9, win32};
use adv_machine::Env;
mod binary;
mod effect;
mod preshader;

type Matrix = [f32; 16];
type Vector = [f32; 3];
type Vector4 = [f32; 4];

pub fn register(registry: &mut Registry) {
    for api in [
        multiply_ABI,
        transpose_ABI,
        scaling_ABI,
        translation_ABI,
        rotation_yaw_pitch_roll_ABI,
        rotation_y_ABI,
        perspective_fov_lh_ABI,
        transform4_ABI,
        look_at_ABI,
        project_ABI,
    ] {
        registry.register(api);
    }
    effect::register(registry);
}

fn product(left: Matrix, right: Matrix) -> Matrix {
    std::array::from_fn(|at| {
        let row = at / 4;
        let column = at % 4;
        (0..4)
            .map(|k| left[row * 4 + k] * right[k * 4 + column])
            .sum()
    })
}

#[win32(d3dx9_43, name = "D3DXMatrixScaling", level = implement)]
fn scaling(
    env: &mut Env<'_>,
    output: GuestPtr<Matrix>,
    sx: f32,
    sy: f32,
    sz: f32,
) -> GuestPtr<Matrix> {
    // Scalar assignments retain their IEEE bits, including negative zero and
    // NaN payloads. Every other coefficient is explicitly initialized.
    let value = [
        sx, 0., 0., 0., 0., sy, 0., 0., 0., 0., sz, 0., 0., 0., 0., 1.,
    ];
    if output.write(env.cpu, value).is_ok() {
        output
    } else {
        GuestPtr::default()
    }
}

#[win32(d3dx9_43, name = "D3DXMatrixTranslation", level = implement)]
fn translation(
    env: &mut Env<'_>,
    output: GuestPtr<Matrix>,
    x: f32,
    y: f32,
    z: f32,
) -> GuestPtr<Matrix> {
    // D3DX uses row vectors: the translation occupies the fourth matrix row.
    let value = [1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., x, y, z, 1.];
    if output.write(env.cpu, value).is_ok() {
        output
    } else {
        GuestPtr::default()
    }
}

#[win32(d3dx9_43, name = "D3DXMatrixRotationYawPitchRoll", level = implement)]
fn rotation_yaw_pitch_roll(
    env: &mut Env<'_>,
    output: GuestPtr<Matrix>,
    yaw: f32,
    pitch: f32,
    roll: f32,
) -> GuestPtr<Matrix> {
    let (sy, cy) = yaw.sin_cos();
    let (sp, cp) = pitch.sin_cos();
    let (sr, cr) = roll.sin_cos();
    // Row vectors apply roll, then pitch, then yaw: Rz * Rx * Ry. Expanding
    // that product keeps the affine coefficients explicitly zero/one even
    // when the trigonometric part produces an IEEE non-finite result.
    let value = [
        cr * cy + sr * sp * sy,
        sr * cp,
        -cr * sy + sr * sp * cy,
        0.,
        -sr * cy + cr * sp * sy,
        cr * cp,
        sr * sy + cr * sp * cy,
        0.,
        cp * sy,
        -sp,
        cp * cy,
        0.,
        0.,
        0.,
        0.,
        1.,
    ];
    if output.write(env.cpu, value).is_ok() {
        output
    } else {
        GuestPtr::default()
    }
}

#[win32(d3dx9_43, name = "D3DXMatrixRotationY", level = implement)]
fn rotation_y(env: &mut Env<'_>, output: GuestPtr<Matrix>, angle: f32) -> GuestPtr<Matrix> {
    let (sin, cos) = angle.sin_cos();
    let value = [
        cos, 0., -sin, 0., 0., 1., 0., 0., sin, 0., cos, 0., 0., 0., 0., 1.,
    ];
    if output.write(env.cpu, value).is_ok() {
        output
    } else {
        GuestPtr::default()
    }
}

#[win32(d3dx9_43, name = "D3DXMatrixPerspectiveFovLH", level = implement)]
fn perspective_fov_lh(
    env: &mut Env<'_>,
    output: GuestPtr<Matrix>,
    fovy: f32,
    aspect: f32,
    near: f32,
    far: f32,
) -> GuestPtr<Matrix> {
    let y_scale = 1. / (fovy * 0.5).tan();
    let x_scale = y_scale / aspect;
    let depth = far / (far - near);
    // D3DX produces arithmetic infinities/NaNs for degenerate inputs rather
    // than substituting an identity projection or a clipping result.
    let value = [
        x_scale,
        0.,
        0.,
        0.,
        0.,
        y_scale,
        0.,
        0.,
        0.,
        0.,
        depth,
        1.,
        0.,
        0.,
        -near * depth,
        0.,
    ];
    if output.write(env.cpu, value).is_ok() {
        output
    } else {
        GuestPtr::default()
    }
}

#[win32(d3dx9_43, name = "D3DXVec4Transform", level = implement)]
fn transform4(
    env: &mut Env<'_>,
    output: GuestPtr<Vector4>,
    vector: GuestPtr<Vector4>,
    matrix: GuestPtr<Matrix>,
) -> GuestPtr<Vector4> {
    let (Ok(vector), Ok(matrix)) = (vector.read(env.cpu), matrix.read(env.cpu)) else {
        return GuestPtr::default();
    };
    // Both full inputs are snapshots, including a partially overlapping
    // destination. This is homogeneous multiplication, without a W divide.
    let value = std::array::from_fn(|column| {
        (0..4)
            .map(|row| vector[row] * matrix[row * 4 + column])
            .sum()
    });
    if output.write(env.cpu, value).is_ok() {
        output
    } else {
        GuestPtr::default()
    }
}

#[win32(d3dx9_43, name = "D3DXMatrixMultiply", level = implement)]
fn multiply(
    env: &mut Env<'_>,
    output: GuestPtr<Matrix>,
    left: GuestPtr<Matrix>,
    right: GuestPtr<Matrix>,
) -> GuestPtr<Matrix> {
    let (Ok(left), Ok(right)) = (left.read(env.cpu), right.read(env.cpu)) else {
        return GuestPtr::default();
    };
    // All sources are snapshots before the first write, including aliases.
    if output.write(env.cpu, product(left, right)).is_ok() {
        output
    } else {
        GuestPtr::default()
    }
}

#[win32(d3dx9_43, name = "D3DXMatrixTranspose", level = implement)]
fn transpose(
    env: &mut Env<'_>,
    output: GuestPtr<Matrix>,
    input: GuestPtr<Matrix>,
) -> GuestPtr<Matrix> {
    let Ok(input) = input.read(env.cpu) else {
        return GuestPtr::default();
    };
    let value = std::array::from_fn(|at| input[at % 4 * 4 + at / 4]);
    if output.write(env.cpu, value).is_ok() {
        output
    } else {
        GuestPtr::default()
    }
}

fn dot(left: Vector, right: Vector) -> f32 {
    left.into_iter().zip(right).map(|(a, b)| a * b).sum()
}
fn cross(left: Vector, right: Vector) -> Vector {
    [
        left[1] * right[2] - left[2] * right[1],
        left[2] * right[0] - left[0] * right[2],
        left[0] * right[1] - left[1] * right[0],
    ]
}
fn normalized(vector: Vector) -> Vector {
    let length = dot(vector, vector).sqrt();
    if length == 0. {
        [0.; 3]
    } else {
        vector.map(|v| v / length)
    }
}

#[win32(d3dx9_43, name = "D3DXMatrixLookAtLH", level = implement)]
fn look_at(
    env: &mut Env<'_>,
    output: GuestPtr<Matrix>,
    eye: GuestPtr<Vector>,
    at: GuestPtr<Vector>,
    up: GuestPtr<Vector>,
) -> GuestPtr<Matrix> {
    let (Ok(eye), Ok(at), Ok(up)) = (eye.read(env.cpu), at.read(env.cpu), up.read(env.cpu)) else {
        return GuestPtr::default();
    };
    let forward = normalized(std::array::from_fn(|i| at[i] - eye[i]));
    let right = normalized(cross(up, forward));
    let up = cross(forward, right);
    let value = [
        right[0],
        up[0],
        forward[0],
        0.,
        right[1],
        up[1],
        forward[1],
        0.,
        right[2],
        up[2],
        forward[2],
        0.,
        -dot(right, eye),
        -dot(up, eye),
        -dot(forward, eye),
        1.,
    ];
    if output.write(env.cpu, value).is_ok() {
        output
    } else {
        GuestPtr::default()
    }
}

#[win32(d3dx9_43, name = "D3DXVec3Project", level = implement)]
fn project(
    env: &mut Env<'_>,
    output: GuestPtr<Vector>,
    vector: GuestPtr<Vector>,
    viewport: GuestPtr<D3DVIEWPORT9>,
    projection: GuestPtr<Matrix>,
    view: GuestPtr<Matrix>,
    world: GuestPtr<Matrix>,
) -> GuestPtr<Vector> {
    let (Ok(vector), Ok(viewport), Ok(projection), Ok(view), Ok(world)) = (
        vector.read(env.cpu),
        viewport.read(env.cpu),
        projection.read(env.cpu),
        view.read(env.cpu),
        world.read(env.cpu),
    ) else {
        return GuestPtr::default();
    };
    let matrix = product(product(world, view), projection);
    let vector = [vector[0], vector[1], vector[2], 1.];
    let clip: [f32; 4] = std::array::from_fn(|column| {
        (0..4)
            .map(|row| vector[row] * matrix[row * 4 + column])
            .sum()
    });
    // The native D3DX reciprocal path produces NaNs at homogeneous W=0.
    // This is an arithmetic result, not a clipping/rejection or fake success.
    let value = if clip[3] == 0. {
        [f32::NAN; 3]
    } else {
        [
            viewport.X as f32 + (clip[0] / clip[3] + 1.) * viewport.Width as f32 * 0.5,
            viewport.Y as f32 + (1. - clip[1] / clip[3]) * viewport.Height as f32 * 0.5,
            viewport.MinZ + clip[2] / clip[3] * (viewport.MaxZ - viewport.MinZ),
        ]
    };
    if output.write(env.cpu, value).is_ok() {
        output
    } else {
        GuestPtr::default()
    }
}

#[cfg(test)]
mod tests;
