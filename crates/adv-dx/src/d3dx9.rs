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

pub fn register(registry: &mut Registry) {
    for api in [multiply_ABI, transpose_ABI, look_at_ABI, project_ABI] {
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
