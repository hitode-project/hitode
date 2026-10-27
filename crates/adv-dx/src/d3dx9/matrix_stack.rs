//! Pure-math matrix stacks in the common COM object table. Every stack level
//! owns a writable guest matrix; GetTop exposes that same authoritative buffer.
use super::*;
use adv_abi::guest::check_range;
use adv_cpu::backend::Prot;
use adv_os::{
    Heaps,
    com::{Com, Iid, QueryError},
};

const OK: u32 = 0;
const INVALID: u32 = 0x8876086c;
const OOM: u32 = 0x8007000e;
const POINTER: u32 = 0x80004003;
const NO_INTERFACE: u32 = 0x80004002;
const MAX_DEPTH: usize = 4096; // 256 KiB of guest matrices per stack.
const IID_STACK: Iid = [
    0xa7, 0x5b, 0x88, 0xc7, 0x90, 0xf9, 0xe7, 0x4f, 0x92, 0x2d, 0x85, 0x15, 0xe4, 0x77, 0xdd, 0x85,
];
const IDENTITY: Matrix = [
    1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1.,
];

struct Store {
    com: Com,
    table: u32,
}
struct Stack {
    levels: Vec<u32>,
}

pub(super) fn register(registry: &mut Registry) {
    for api in [
        create_ABI,
        query_ABI,
        add_ref_ABI,
        release_ABI,
        pop_ABI,
        push_ABI,
        load_identity_ABI,
        load_matrix_ABI,
        multiply_ABI,
        multiply_local_ABI,
        rotate_axis_ABI,
        rotate_axis_local_ABI,
        rotate_ABI,
        rotate_local_ABI,
        scale_ABI,
        scale_local_ABI,
        translate_ABI,
        translate_local_ABI,
        top_ABI,
    ] {
        registry.register(api);
    }
}
fn warning(env: &mut Env<'_>, reason: &str) {
    let thunk = env.sys.thunks.get(env.thunk);
    let name = format!("{}!{}", thunk.module, thunk.name);
    adv_abi::warn_unavailable(env, &name, reason);
}
fn allocate(env: &mut Env<'_>, bytes: &[u8]) -> Option<u32> {
    let heaps = env.ext.try_get_mut::<Heaps>()?;
    let pointer = heaps.alloc(
        env.cpu,
        &mut env.sys.vm,
        heaps.process_heap,
        bytes.len() as u32,
        false,
    )?;
    if env.cpu.write(pointer, bytes).is_err() {
        free(env, pointer);
        return None;
    }
    Some(pointer)
}
fn free(env: &mut Env<'_>, pointer: u32) {
    let heaps = env.ext.get_mut::<Heaps>();
    heaps.free(env.cpu, &mut env.sys.vm, heaps.process_heap, pointer);
}
fn initialize(env: &mut Env<'_>) -> Option<()> {
    if env.ext.try_get::<Store>().is_some() {
        return Some(());
    }
    let mut methods = [0u32; 18];
    for signature in adv_abi::signature::com_signatures()
        .iter()
        .filter(|s| s.module == "ID3DXMatrixStack")
    {
        let slot = usize::from(signature.com_slot?);
        let name = format!("ID3DXMatrixStack::{}", signature.name);
        *methods.get_mut(slot)? = env
            .sys
            .thunks
            .publish(env.cpu, "d3dx9_43.dll", &name)
            .ok()?;
    }
    if methods.contains(&0) {
        return None;
    }
    let table = allocate(env, bytemuck::cast_slice(&methods))?;
    let Some(com) = Com::new(&mut env.sys.objects) else {
        free(env, table);
        return None;
    };
    env.ext.insert(Store { com, table });
    Some(())
}
fn com(env: &Env<'_>) -> Option<Com> {
    Some(env.ext.try_get::<Store>()?.com)
}
fn top_address(env: &Env<'_>, this: u32) -> Option<u32> {
    com(env)?
        .get::<Stack>(&env.sys.objects, this)?
        .levels
        .last()
        .copied()
}
fn put(env: &mut Env<'_>, this: u32, matrix: Matrix) -> u32 {
    let Some(top) = top_address(env, this) else {
        return INVALID;
    };
    if GuestPtr::new(top).write(env.cpu, matrix).is_ok() {
        OK
    } else {
        INVALID
    }
}
fn mult(env: &mut Env<'_>, this: u32, matrix: Matrix, local: bool) -> u32 {
    let Some(top) = top_address(env, this) else {
        return INVALID;
    };
    let Ok(current) = GuestPtr::<Matrix>::new(top).read(env.cpu) else {
        return INVALID;
    };
    put(
        env,
        this,
        if local {
            product(matrix, current)
        } else {
            product(current, matrix)
        },
    )
}
fn from_matrix(env: &mut Env<'_>, this: u32, matrix: GuestPtr<Matrix>, local: bool) -> u32 {
    let Ok(matrix) = matrix.read(env.cpu) else {
        return INVALID;
    };
    mult(env, this, matrix, local)
}
fn axis_matrix(axis: Vector, angle: f32) -> Matrix {
    let [x, y, z] = normalized(axis);
    let (sin, cos) = angle.sin_cos();
    let t = 1. - cos;
    [
        cos + t * x * x,
        t * x * y + sin * z,
        t * x * z - sin * y,
        0.,
        t * x * y - sin * z,
        cos + t * y * y,
        t * y * z + sin * x,
        0.,
        t * x * z + sin * y,
        t * y * z - sin * x,
        cos + t * z * z,
        0.,
        0.,
        0.,
        0.,
        1.,
    ]
}
fn rotate_about(
    env: &mut Env<'_>,
    this: u32,
    axis: GuestPtr<Vector>,
    angle: f32,
    local: bool,
) -> u32 {
    let Ok(axis) = axis.read(env.cpu) else {
        return INVALID;
    };
    mult(env, this, axis_matrix(axis, angle), local)
}

#[win32(d3dx9_43, name="D3DXCreateMatrixStack", level=implement)]
fn create(env: &mut Env<'_>, flags: u32, output: GuestPtr<u32>) -> u32 {
    if check_range(env.cpu, output.addr(), 4, Prot::WRITE).is_err()
        || output.write(env.cpu, 0).is_err()
    {
        return INVALID;
    }
    if flags != 0 {
        warning(
            env,
            "matrix-stack flags must be zero; other flag profiles are unavailable",
        );
        return INVALID;
    }
    if initialize(env).is_none() {
        return OOM;
    }
    let Some(level) = allocate(env, bytemuck::bytes_of(&IDENTITY)) else {
        return OOM;
    };
    let table = env.ext.get::<Store>().table;
    let Some(pointer) = allocate(env, &table.to_le_bytes()) else {
        free(env, level);
        return OOM;
    };
    let com = com(env).unwrap();
    if com
        .create(
            &mut env.sys.objects,
            pointer,
            IID_STACK,
            Stack {
                levels: vec![level],
            },
        )
        .is_none()
    {
        free(env, pointer);
        free(env, level);
        return OOM;
    }
    if output.write(env.cpu, pointer).is_err() {
        release(env, GuestPtr::new(pointer));
        return INVALID;
    }
    OK
}

#[win32(d3dx9_43, name="ID3DXMatrixStack::QueryInterface", level=implement)]
fn query(env: &mut Env<'_>, this: GuestPtr<()>, iid: GuestPtr<Iid>, output: GuestPtr<u32>) -> u32 {
    if check_range(env.cpu, output.addr(), 4, Prot::WRITE).is_err() {
        return POINTER;
    }
    // Snapshot the IID before clearing an output which can overlap it.
    let requested = iid.read(env.cpu);
    if output.write(env.cpu, 0).is_err() {
        return POINTER;
    }
    let (Ok(requested), Some(com)) = (requested, com(env)) else {
        return POINTER;
    };
    match com.query::<Stack>(&mut env.sys.objects, this.addr(), requested) {
        Ok(pointer) => {
            if output.write(env.cpu, pointer).is_ok() {
                OK
            } else {
                let _ = com.release::<Stack>(&mut env.sys.objects, pointer);
                POINTER
            }
        }
        Err(QueryError::NoInterface) => {
            warning(
                env,
                "matrix stack exposes IUnknown and ID3DXMatrixStack only",
            );
            NO_INTERFACE
        }
        Err(QueryError::ReferenceLimit) => OOM,
        Err(QueryError::InvalidObject) => POINTER,
    }
}
#[win32(d3dx9_43, name="ID3DXMatrixStack::AddRef", level=implement)]
fn add_ref(env: &mut Env<'_>, this: GuestPtr<()>) -> u32 {
    com(env)
        .and_then(|com| com.add_ref::<Stack>(&mut env.sys.objects, this.addr()))
        .unwrap_or(0)
}
#[win32(d3dx9_43, name="ID3DXMatrixStack::Release", level=implement)]
fn release(env: &mut Env<'_>, this: GuestPtr<()>) -> u32 {
    let Some(com) = com(env) else {
        return 0;
    };
    let Some(released) = com.release::<Stack>(&mut env.sys.objects, this.addr()) else {
        return 0;
    };
    if let Some(dead) = released.destroyed {
        for pointer in dead.value.levels.into_iter().chain(dead.interfaces) {
            free(env, pointer);
        }
    }
    released.references
}
#[win32(d3dx9_43, name="ID3DXMatrixStack::Pop", level=implement)]
fn pop(env: &mut Env<'_>, this: GuestPtr<()>) -> u32 {
    let Some(com) = com(env) else {
        return INVALID;
    };
    let Some(stack) = com.get_mut::<Stack>(&mut env.sys.objects, this.addr()) else {
        return INVALID;
    };
    // Measured D3DX9 retains a base matrix; Pop there succeeds without change.
    if stack.levels.len() > 1 {
        let pointer = stack.levels.pop().unwrap();
        free(env, pointer);
    }
    OK
}
#[win32(d3dx9_43, name="ID3DXMatrixStack::Push", level=implement)]
fn push(env: &mut Env<'_>, this: GuestPtr<()>) -> u32 {
    let Some(com) = com(env) else {
        return INVALID;
    };
    let Some(stack) = com.get_mut::<Stack>(&mut env.sys.objects, this.addr()) else {
        return INVALID;
    };
    if stack.levels.len() >= MAX_DEPTH {
        warning(
            env,
            "matrix-stack depth exceeds the 4096-level/256KiB supported bound",
        );
        return OOM;
    }
    if stack.levels.try_reserve(1).is_err() {
        return OOM;
    }
    let top = *stack.levels.last().unwrap();
    let Ok(value) = GuestPtr::<Matrix>::new(top).read(env.cpu) else {
        return INVALID;
    };
    let Some(pointer) = allocate(env, bytemuck::bytes_of(&value)) else {
        return OOM;
    };
    com.get_mut::<Stack>(&mut env.sys.objects, this.addr())
        .unwrap()
        .levels
        .push(pointer);
    OK
}
#[win32(d3dx9_43, name="ID3DXMatrixStack::LoadIdentity", level=implement)]
fn load_identity(env: &mut Env<'_>, this: GuestPtr<()>) -> u32 {
    put(env, this.addr(), IDENTITY)
}
#[win32(d3dx9_43, name="ID3DXMatrixStack::LoadMatrix", level=implement)]
fn load_matrix(env: &mut Env<'_>, this: GuestPtr<()>, matrix: GuestPtr<Matrix>) -> u32 {
    match matrix.read(env.cpu) {
        Ok(value) => put(env, this.addr(), value),
        Err(_) => INVALID,
    }
}
#[win32(d3dx9_43, name="ID3DXMatrixStack::MultMatrix", level=implement)]
fn multiply(env: &mut Env<'_>, this: GuestPtr<()>, matrix: GuestPtr<Matrix>) -> u32 {
    from_matrix(env, this.addr(), matrix, false)
}
#[win32(d3dx9_43, name="ID3DXMatrixStack::MultMatrixLocal", level=implement)]
fn multiply_local(env: &mut Env<'_>, this: GuestPtr<()>, matrix: GuestPtr<Matrix>) -> u32 {
    from_matrix(env, this.addr(), matrix, true)
}
#[win32(d3dx9_43, name="ID3DXMatrixStack::RotateAxis", level=implement)]
fn rotate_axis(env: &mut Env<'_>, this: GuestPtr<()>, axis: GuestPtr<Vector>, angle: f32) -> u32 {
    rotate_about(env, this.addr(), axis, angle, false)
}
#[win32(d3dx9_43, name="ID3DXMatrixStack::RotateAxisLocal", level=implement)]
fn rotate_axis_local(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    axis: GuestPtr<Vector>,
    angle: f32,
) -> u32 {
    rotate_about(env, this.addr(), axis, angle, true)
}
#[win32(d3dx9_43, name="ID3DXMatrixStack::RotateYawPitchRoll", level=implement)]
fn rotate(env: &mut Env<'_>, this: GuestPtr<()>, yaw: f32, pitch: f32, roll: f32) -> u32 {
    mult(
        env,
        this.addr(),
        rotation_yaw_pitch_roll_value(yaw, pitch, roll),
        false,
    )
}
#[win32(d3dx9_43, name="ID3DXMatrixStack::RotateYawPitchRollLocal", level=implement)]
fn rotate_local(env: &mut Env<'_>, this: GuestPtr<()>, yaw: f32, pitch: f32, roll: f32) -> u32 {
    mult(
        env,
        this.addr(),
        rotation_yaw_pitch_roll_value(yaw, pitch, roll),
        true,
    )
}
#[win32(d3dx9_43, name="ID3DXMatrixStack::Scale", level=implement)]
fn scale(env: &mut Env<'_>, this: GuestPtr<()>, x: f32, y: f32, z: f32) -> u32 {
    mult(env, this.addr(), scaling_value(x, y, z), false)
}
#[win32(d3dx9_43, name="ID3DXMatrixStack::ScaleLocal", level=implement)]
fn scale_local(env: &mut Env<'_>, this: GuestPtr<()>, x: f32, y: f32, z: f32) -> u32 {
    mult(env, this.addr(), scaling_value(x, y, z), true)
}
#[win32(d3dx9_43, name="ID3DXMatrixStack::Translate", level=implement)]
fn translate(env: &mut Env<'_>, this: GuestPtr<()>, x: f32, y: f32, z: f32) -> u32 {
    mult(env, this.addr(), translation_value(x, y, z), false)
}
#[win32(d3dx9_43, name="ID3DXMatrixStack::TranslateLocal", level=implement)]
fn translate_local(env: &mut Env<'_>, this: GuestPtr<()>, x: f32, y: f32, z: f32) -> u32 {
    mult(env, this.addr(), translation_value(x, y, z), true)
}
#[win32(d3dx9_43, name="ID3DXMatrixStack::GetTop", level=implement)]
fn top(env: &mut Env<'_>, this: GuestPtr<()>) -> GuestPtr<Matrix> {
    GuestPtr::new(top_address(env, this.addr()).unwrap_or(0))
}

#[cfg(test)]
mod tests;
