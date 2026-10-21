//! Legacy D3DX9 matrix/vector contracts and native binary Effect execution.
//! Effect shader objects feed the shared D3D9 programmable drawing frontend.
//! Authoritative prototypes and the scoped ABI input are in data/abi/sdk-source.json.
use adv_abi::{GuestPtr, Registry, types::D3DVIEWPORT9, win32};
use adv_machine::Env;
mod binary;
mod buffer;
mod effect;
mod matrix_stack;
mod preshader;
mod save_file;
mod sprite;
mod texture;
mod texture_create;

type Matrix = [f32; 16];
type Vector = [f32; 3];
type Vector4 = [f32; 4];
type Vector2 = [f32; 2];

pub fn register(registry: &mut Registry) {
    for api in [
        multiply_ABI,
        transpose_ABI,
        scaling_ABI,
        translation_ABI,
        rotation_yaw_pitch_roll_ABI,
        rotation_y_ABI,
        rotation_x_ABI,
        rotation_z_ABI,
        perspective_fov_lh_ABI,
        transform4_ABI,
        transform3_ABI,
        transform_coord_ABI,
        transform2_ABI,
        transform2_coord_ABI,
        transform2_normal_ABI,
        catmull_rom_ABI,
        look_at_ABI,
        project_ABI,
    ] {
        registry.register(adv_abi::Api {
            env_leaf: Some(math_leaf),
            ..api
        });
        registry.register(adv_abi::Api {
            module: "d3dx9_29.dll",
            env_leaf: Some(math_leaf),
            ..api
        });
    }
    buffer::register(registry);
    effect::register(registry);
    matrix_stack::register(registry);
    texture::register(registry);
    texture_create::register(registry);
    save_file::register(registry);
    sprite::register(registry);
}

fn math_leaf(env: &mut Env<'_>) -> Option<u64> {
    use adv_abi::guest::check_range;
    use adv_cpu::backend::Prot;
    let thunk = env.sys.thunks.get(env.thunk);
    let name = thunk.name.as_str();
    let binding = thunk.f;
    let mut arguments = adv_abi::Arguments::new(env.cpu);
    let mut args = [0u32; 7];
    let adv_abi::Conv::Stdcall { argc } = binding.conv else {
        unreachable!()
    };
    for arg in &mut args[..usize::from(argc)] {
        *arg = arguments.next().ok()?;
    }
    let (output, inputs): (u32, &[(usize, u32)]) = match name {
        "D3DXMatrixMultiply" => (64, &[(1, 64), (2, 64)]),
        "D3DXMatrixTranspose" => (64, &[(1, 64)]),
        "D3DXMatrixLookAtLH" => (64, &[(1, 12), (2, 12), (3, 12)]),
        "D3DXVec2Transform" => (16, &[(1, 8), (2, 64)]),
        "D3DXVec2TransformCoord" | "D3DXVec2TransformNormal" => (8, &[(1, 8), (2, 64)]),
        "D3DXVec3Transform" => (16, &[(1, 12), (2, 64)]),
        "D3DXVec3TransformCoord" => (12, &[(1, 12), (2, 64)]),
        "D3DXVec4Transform" => (16, &[(1, 16), (2, 64)]),
        "D3DXVec3CatmullRom" => (12, &[(1, 12), (2, 12), (3, 12), (4, 12)]),
        "D3DXVec3Project" => (12, &[(1, 12), (2, 24), (3, 64), (4, 64), (5, 64)]),
        _ => (64, &[]),
    };
    check_range(env.cpu, args[0], output, Prot::WRITE).ok()?;
    let mut input = [0; 64];
    for &(index, length) in inputs {
        check_range(env.cpu, args[index], length, Prot::READ).ok()?;
        env.cpu
            .read(args[index], &mut input[..length as usize])
            .ok()?;
    }
    let adv_machine::Outcome::Return(result) = (binding.handler)(env) else {
        return None;
    };
    (result != 0).then(|| (u64::from(adv_abi::leaf_edx(env)) << 32) | u64::from(result))
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

fn transform_value(vector: Vector4, matrix: Matrix) -> Vector4 {
    std::array::from_fn(|column| {
        (0..4)
            .map(|row| vector[row] * matrix[row * 4 + column])
            .sum()
    })
}

#[win32(d3dx9_43, name = "D3DXMatrixScaling", level = implement, leaf)]
fn scaling(
    env: &mut Env<'_>,
    output: GuestPtr<Matrix>,
    sx: f32,
    sy: f32,
    sz: f32,
) -> GuestPtr<Matrix> {
    // Scalar assignments retain their IEEE bits, including negative zero and
    // NaN payloads. Every other coefficient is explicitly initialized.
    let value = scaling_value(sx, sy, sz);
    if output.write(env.cpu, value).is_ok() {
        output
    } else {
        GuestPtr::default()
    }
}

fn scaling_value(sx: f32, sy: f32, sz: f32) -> Matrix {
    [
        sx, 0., 0., 0., 0., sy, 0., 0., 0., 0., sz, 0., 0., 0., 0., 1.,
    ]
}

#[win32(d3dx9_43, name = "D3DXMatrixTranslation", level = implement, leaf)]
fn translation(
    env: &mut Env<'_>,
    output: GuestPtr<Matrix>,
    x: f32,
    y: f32,
    z: f32,
) -> GuestPtr<Matrix> {
    // D3DX uses row vectors: the translation occupies the fourth matrix row.
    let value = translation_value(x, y, z);
    if output.write(env.cpu, value).is_ok() {
        output
    } else {
        GuestPtr::default()
    }
}

fn translation_value(x: f32, y: f32, z: f32) -> Matrix {
    [1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., x, y, z, 1.]
}

#[win32(d3dx9_43, name = "D3DXMatrixRotationYawPitchRoll", level = implement, leaf)]
fn rotation_yaw_pitch_roll(
    env: &mut Env<'_>,
    output: GuestPtr<Matrix>,
    yaw: f32,
    pitch: f32,
    roll: f32,
) -> GuestPtr<Matrix> {
    let value = rotation_yaw_pitch_roll_value(yaw, pitch, roll);
    if output.write(env.cpu, value).is_ok() {
        output
    } else {
        GuestPtr::default()
    }
}

fn rotation_yaw_pitch_roll_value(yaw: f32, pitch: f32, roll: f32) -> Matrix {
    let (sy, cy) = yaw.sin_cos();
    let (sp, cp) = pitch.sin_cos();
    let (sr, cr) = roll.sin_cos();
    // Row vectors apply roll, then pitch, then yaw: Rz * Rx * Ry. Expanding
    // that product keeps the affine coefficients explicitly zero/one even
    // when the trigonometric part produces an IEEE non-finite result.
    [
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
    ]
}

#[win32(d3dx9_43, name = "D3DXMatrixRotationY", level = implement, leaf)]
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

#[win32(d3dx9_43, name = "D3DXMatrixRotationX", level = implement, leaf)]
fn rotation_x(env: &mut Env<'_>, output: GuestPtr<Matrix>, angle: f32) -> GuestPtr<Matrix> {
    let (s, c) = angle.sin_cos();
    let value = [1., 0., 0., 0., 0., c, s, 0., 0., -s, c, 0., 0., 0., 0., 1.];
    if output.write(env.cpu, value).is_ok() {
        output
    } else {
        GuestPtr::default()
    }
}

#[win32(d3dx9_43, name = "D3DXMatrixRotationZ", level = implement, leaf)]
fn rotation_z(env: &mut Env<'_>, output: GuestPtr<Matrix>, angle: f32) -> GuestPtr<Matrix> {
    let (s, c) = angle.sin_cos();
    let value = [c, s, 0., 0., -s, c, 0., 0., 0., 0., 1., 0., 0., 0., 0., 1.];
    if output.write(env.cpu, value).is_ok() {
        output
    } else {
        GuestPtr::default()
    }
}

fn transform2_value(vector: Vector2, matrix: Matrix, translate: bool) -> Vector4 {
    std::array::from_fn(|column| {
        // Vec2 never reads Z terms; normal transformation also omits translation.
        let value = vector[0] * matrix[column] + vector[1] * matrix[4 + column];
        if translate {
            value + matrix[12 + column]
        } else {
            value
        }
    })
}

#[win32(d3dx9_43, name = "D3DXVec2Transform", level = implement, leaf)]
fn transform2(
    env: &mut Env<'_>,
    output: GuestPtr<Vector4>,
    vector: GuestPtr<Vector2>,
    matrix: GuestPtr<Matrix>,
) -> GuestPtr<Vector4> {
    let (Ok(vector), Ok(matrix)) = (vector.read(env.cpu), matrix.read(env.cpu)) else {
        return GuestPtr::default();
    };
    if output
        .write(env.cpu, transform2_value(vector, matrix, true))
        .is_ok()
    {
        output
    } else {
        GuestPtr::default()
    }
}

#[win32(d3dx9_43, name = "D3DXVec2TransformCoord", level = implement, leaf)]
fn transform2_coord(
    env: &mut Env<'_>,
    output: GuestPtr<Vector2>,
    vector: GuestPtr<Vector2>,
    matrix: GuestPtr<Matrix>,
) -> GuestPtr<Vector2> {
    let (Ok(vector), Ok(matrix)) = (vector.read(env.cpu), matrix.read(env.cpu)) else {
        return GuestPtr::default();
    };
    let clip = transform2_value(vector, matrix, true);
    let value = if clip[3] == 0. {
        [f32::from_bits(0xffc00000); 2]
    } else {
        [clip[0] / clip[3], clip[1] / clip[3]]
    };
    if output.write(env.cpu, value).is_ok() {
        output
    } else {
        GuestPtr::default()
    }
}

#[win32(d3dx9_43, name = "D3DXVec2TransformNormal", level = implement, leaf)]
fn transform2_normal(
    env: &mut Env<'_>,
    output: GuestPtr<Vector2>,
    vector: GuestPtr<Vector2>,
    matrix: GuestPtr<Matrix>,
) -> GuestPtr<Vector2> {
    let (Ok(vector), Ok(matrix)) = (vector.read(env.cpu), matrix.read(env.cpu)) else {
        return GuestPtr::default();
    };
    let value = transform2_value(vector, matrix, false);
    if output.write(env.cpu, [value[0], value[1]]).is_ok() {
        output
    } else {
        GuestPtr::default()
    }
}

#[win32(d3dx9_43, name = "D3DXMatrixPerspectiveFovLH", level = implement, leaf)]
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

#[win32(d3dx9_43, name = "D3DXVec4Transform", level = implement, leaf)]
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
    let value = transform_value(vector, matrix);
    if output.write(env.cpu, value).is_ok() {
        output
    } else {
        GuestPtr::default()
    }
}

#[win32(d3dx9_43, name = "D3DXVec3Transform", level = implement, leaf)]
fn transform3(
    env: &mut Env<'_>,
    output: GuestPtr<Vector4>,
    vector: GuestPtr<Vector>,
    matrix: GuestPtr<Matrix>,
) -> GuestPtr<Vector4> {
    let (Ok(vector), Ok(matrix)) = (vector.read(env.cpu), matrix.read(env.cpu)) else {
        return GuestPtr::default();
    };
    // The source is exactly three FLOATs; W is supplied rather than read from
    // the following guest DWORD. The output is four FLOATs without a W divide.
    let value = transform_value([vector[0], vector[1], vector[2], 1.], matrix);
    if output.write(env.cpu, value).is_ok() {
        output
    } else {
        GuestPtr::default()
    }
}

#[win32(d3dx9_43, name = "D3DXVec3TransformCoord", level = implement, leaf)]
fn transform_coord(
    env: &mut Env<'_>,
    output: GuestPtr<Vector>,
    vector: GuestPtr<Vector>,
    matrix: GuestPtr<Matrix>,
) -> GuestPtr<Vector> {
    let (Ok(vector), Ok(matrix)) = (vector.read(env.cpu), matrix.read(env.cpu)) else {
        return GuestPtr::default();
    };
    let clip = transform_value([vector[0], vector[1], vector[2], 1.], matrix);
    // The native D3DX reciprocal refinement at W=0 yields an indefinite NaN
    // for all three coordinates, including nonzero homogeneous numerators.
    let value = if clip[3] == 0. {
        [f32::from_bits(0xffc00000); 3]
    } else {
        std::array::from_fn(|i| clip[i] / clip[3])
    };
    if output.write(env.cpu, value).is_ok() {
        output
    } else {
        GuestPtr::default()
    }
}

#[win32(d3dx9_43, name = "D3DXVec3CatmullRom", level = implement, leaf)]
fn catmull_rom(
    env: &mut Env<'_>,
    output: GuestPtr<Vector>,
    v0: GuestPtr<Vector>,
    v1: GuestPtr<Vector>,
    v2: GuestPtr<Vector>,
    v3: GuestPtr<Vector>,
    s: f32,
) -> GuestPtr<Vector> {
    let (Ok(v0), Ok(v1), Ok(v2), Ok(v3)) = (
        v0.read(env.cpu),
        v1.read(env.cpu),
        v2.read(env.cpu),
        v3.read(env.cpu),
    ) else {
        return GuestPtr::default();
    };
    // SDK cubic coefficients are evaluated for the supplied s, without
    // clamping. Snapshot all four sources before publishing any destination.
    let s2 = s * s;
    let s3 = s2 * s;
    let c0 = -s3 + 2. * s2 - s;
    let c1 = 3. * s3 - 5. * s2 + 2.;
    let c2 = -3. * s3 + 4. * s2 + s;
    let c3 = s3 - s2;
    let value = std::array::from_fn(|i| (c0 * v0[i] + c1 * v1[i] + c2 * v2[i] + c3 * v3[i]) * 0.5);
    if output.write(env.cpu, value).is_ok() {
        output
    } else {
        GuestPtr::default()
    }
}

#[win32(d3dx9_43, name = "D3DXMatrixMultiply", level = implement, leaf)]
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

#[win32(d3dx9_43, name = "D3DXMatrixTranspose", level = implement, leaf)]
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

#[win32(d3dx9_43, name = "D3DXMatrixLookAtLH", level = implement, leaf)]
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

#[win32(d3dx9_43, name = "D3DXVec3Project", level = implement, leaf)]
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

#[cfg(feature = "gpu")]
pub(crate) fn reap_file_exports(env: &mut Env<'_>, cancel_all: bool) {
    save_file::reap(env, cancel_all);
}
