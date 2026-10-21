//! ID3DXSprite over the D3D9 COM device, following Wine's d3dx9 sprite: Draw
//! queues quads, Flush draws them per texture run with DrawPrimitiveUP, and
//! Begin/End save and restore the touched state through one recorded block.
//! Device calls go through the guest-visible vtable like Effect calls.
use super::effect::{Calls, alloc, calls, free, run};
use super::*;
use adv_abi::guest::check_range;
use adv_cpu::backend::{GuestExt, Prot};
use adv_machine::Outcome;
use adv_os::com::{Com, Iid, QueryError};

#[cfg(test)]
mod tests;

const OK: u32 = 0;
const INVALID: u32 = 0x8876086c;
const OOM: u32 = 0x8007000e;
const NO_INTERFACE: u32 = 0x80004002;
const IID_SPRITE: Iid = [
    0x2d, 0x76, 0x0b, 0xba, 0x28, 0x7d, 0xec, 0x43, 0xb9, 0xdc, 0x2f, 0x84, 0x44, 0x3b, 0x06, 0x14,
];
const DONOTSAVESTATE: u32 = 1;
const DONOTMODIFY_RENDERSTATE: u32 = 2;
const OBJECTSPACE: u32 = 4;
const ALPHABLEND: u32 = 0x10;
const DO_NOT_ADDREF_TEXTURE: u32 = 0x100;
/// D3DFVF_XYZ | D3DFVF_DIFFUSE | D3DFVF_TEX1: position, color, one UV pair.
const FVF: u32 = 0x142;
const STRIDE: u32 = 24;
const IDENTITY: Matrix = [
    1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1.,
];
// Device slots.
const GET_VIEWPORT: u32 = 48;
const SET_TRANSFORM: u32 = 44;
const SET_RENDER: u32 = 57;
const BEGIN_BLOCK: u32 = 60;
const END_BLOCK: u32 = 61;
const SET_TEXTURE: u32 = 65;
const SET_STAGE: u32 = 67;
const SET_SAMPLER: u32 = 69;
const DRAW_UP: u32 = 83;
const SET_FVF: u32 = 89;
const SET_VERTEX_SHADER: u32 = 92;
const SET_STREAM: u32 = 100;
const SET_INDICES: u32 = 104;
const SET_PIXEL_SHADER: u32 = 107;

struct Store {
    com: Com,
    table: u32,
}
struct Quad {
    texture: u32,
    owned: bool,
    rect: [i32; 4],
    size: [u32; 2],
    center: Vector,
    position: Vector,
    color: u32,
    transform: Matrix,
}
struct Sprite {
    device: u32,
    transform: Matrix,
    flags: u32,
    ready: bool,
    /// Recorded on the first saving Begin; captured and applied around each
    /// Begin/End pair.
    block: u32,
    queue: Vec<Quad>,
}

pub(super) fn register(registry: &mut Registry) {
    for api in [
        create_ABI,
        transformation_2d_ABI,
        query_ABI,
        add_ref_ABI,
        release_ABI,
        get_device_ABI,
        get_transform_ABI,
        set_transform_ABI,
        set_world_view_rh_ABI,
        set_world_view_lh_ABI,
        begin_ABI,
        draw_ABI,
        flush_ABI,
        end_ABI,
        lost_ABI,
        reset_ABI,
    ] {
        registry.register(api);
    }
}

fn initialize(env: &mut Env<'_>) -> Option<()> {
    if env.ext.try_get::<Store>().is_some() {
        return Some(());
    }
    let mut methods = [0u32; 14];
    for signature in adv_abi::signature::com_signatures()
        .iter()
        .filter(|s| s.module == "ID3DXSprite")
    {
        let name = format!("ID3DXSprite::{}", signature.name);
        *methods.get_mut(usize::from(signature.com_slot?))? = env
            .sys
            .thunks
            .publish(env.cpu, "d3dx9_43.dll", &name)
            .ok()?;
    }
    if methods.contains(&0) {
        return None;
    }
    let table = alloc(env, bytemuck::cast_slice(&methods))?;
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
fn sprite<'a>(env: &'a Env<'_>, this: u32) -> Option<&'a Sprite> {
    com(env)?.get::<Sprite>(&env.sys.objects, this)
}
fn sprite_mut<'a>(env: &'a mut Env<'_>, this: u32) -> Option<&'a mut Sprite> {
    com(env)?.get_mut::<Sprite>(&mut env.sys.objects, this)
}
fn read<T: bytemuck::Pod>(env: &Env<'_>, pointer: u32) -> Option<Option<T>> {
    if pointer == 0 {
        return Some(None);
    }
    GuestPtr::<T>::new(pointer).read(env.cpu).ok().map(Some)
}

#[win32(d3dx9_43, name = "D3DXCreateSprite", level = implement)]
fn create(env: &mut Env<'_>, device: GuestPtr<()>, output: GuestPtr<u32>) -> Outcome {
    if device.addr() == 0 || check_range(env.cpu, output.addr(), 4, Prot::WRITE).is_err() {
        return Outcome::Return(INVALID);
    }
    if initialize(env).is_none() {
        return Outcome::Return(OOM);
    }
    let table = env.ext.get::<Store>().table;
    let Some(pointer) = alloc(env, &table.to_le_bytes()) else {
        return Outcome::Return(OOM);
    };
    let value = Sprite {
        device: device.addr(),
        transform: IDENTITY,
        flags: 0,
        ready: false,
        block: 0,
        queue: Vec::new(),
    };
    let com = com(env).unwrap();
    if com
        .create(&mut env.sys.objects, pointer, IID_SPRITE, value)
        .is_none()
    {
        free(env, pointer);
        return Outcome::Return(OOM);
    }
    let _ = output.write(env.cpu, pointer);
    let mut c = Calls::new();
    c.push(device.addr(), 1, []);
    run(env, c, Box::new(|_| Outcome::Return(OK)))
}

#[win32(d3dx9_43, name = "ID3DXSprite::QueryInterface", level = implement)]
fn query(env: &mut Env<'_>, this: GuestPtr<()>, iid: GuestPtr<Iid>, output: GuestPtr<u32>) -> u32 {
    if check_range(env.cpu, output.addr(), 4, Prot::WRITE).is_err() {
        return INVALID;
    }
    let requested = iid.read(env.cpu);
    let _ = output.write(env.cpu, 0);
    let (Ok(requested), Some(com)) = (requested, com(env)) else {
        return INVALID;
    };
    match com.query::<Sprite>(&mut env.sys.objects, this.addr(), requested) {
        Ok(pointer) => {
            let _ = output.write(env.cpu, pointer);
            OK
        }
        Err(QueryError::NoInterface) => NO_INTERFACE,
        Err(QueryError::ReferenceLimit) => OOM,
        Err(QueryError::InvalidObject) => INVALID,
    }
}
#[win32(d3dx9_43, name = "ID3DXSprite::AddRef", level = implement)]
fn add_ref(env: &mut Env<'_>, this: GuestPtr<()>) -> u32 {
    com(env)
        .and_then(|com| com.add_ref::<Sprite>(&mut env.sys.objects, this.addr()))
        .unwrap_or(0)
}
#[win32(d3dx9_43, name = "ID3DXSprite::Release", level = implement)]
fn release(env: &mut Env<'_>, this: GuestPtr<()>) -> Outcome {
    let Some(com) = com(env) else {
        return Outcome::Return(0);
    };
    let Some(released) = com.release::<Sprite>(&mut env.sys.objects, this.addr()) else {
        return Outcome::Return(0);
    };
    let Some(dead) = released.destroyed else {
        return Outcome::Return(released.references);
    };
    let mut c = Calls::new();
    c.tolerant = true;
    release_queue(&mut c, dead.value.queue);
    if dead.value.block != 0 {
        c.push(dead.value.block, 2, []);
    }
    c.push(dead.value.device, 2, []);
    for pointer in dead.interfaces {
        free(env, pointer);
    }
    calls(env, c, 0)
}
fn release_queue(c: &mut Calls, queue: Vec<Quad>) {
    for quad in queue.into_iter().filter(|q| q.owned) {
        c.push(quad.texture, 2, []);
    }
}
#[win32(d3dx9_43, name = "ID3DXSprite::GetDevice", level = implement)]
fn get_device(env: &mut Env<'_>, this: GuestPtr<()>, output: GuestPtr<u32>) -> Outcome {
    let Some(device) = sprite(env, this.addr()).map(|s| s.device) else {
        return Outcome::Return(INVALID);
    };
    if output.addr() == 0 || output.write(env.cpu, device).is_err() {
        return Outcome::Return(INVALID);
    }
    let mut c = Calls::new();
    c.push(device, 1, []);
    run(env, c, Box::new(|_| Outcome::Return(OK)))
}
#[win32(d3dx9_43, name = "ID3DXSprite::GetTransform", level = implement)]
fn get_transform(env: &mut Env<'_>, this: GuestPtr<()>, output: GuestPtr<Matrix>) -> u32 {
    let Some(transform) = sprite(env, this.addr()).map(|s| s.transform) else {
        return INVALID;
    };
    if output.addr() == 0 || output.write(env.cpu, transform).is_err() {
        return INVALID;
    }
    OK
}
#[win32(d3dx9_43, name = "ID3DXSprite::SetTransform", level = implement)]
fn set_transform(env: &mut Env<'_>, this: GuestPtr<()>, input: GuestPtr<Matrix>) -> u32 {
    let Some(Some(transform)) = read::<Matrix>(env, input.addr()) else {
        return INVALID;
    };
    match sprite_mut(env, this.addr()) {
        Some(s) => {
            s.transform = transform;
            OK
        }
        None => INVALID,
    }
}
// World and view matter only for billboards and depth sorting, which are not
// implemented; screen-space sprites ignore them, as in Wine.
#[win32(d3dx9_43, name = "ID3DXSprite::SetWorldViewRH", level = implement)]
fn set_world_view_rh(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    _world: GuestPtr<Matrix>,
    _view: GuestPtr<Matrix>,
) -> u32 {
    if sprite(env, this.addr()).is_some() {
        OK
    } else {
        INVALID
    }
}
#[win32(d3dx9_43, name = "ID3DXSprite::SetWorldViewLH", level = implement)]
fn set_world_view_lh(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    _world: GuestPtr<Matrix>,
    _view: GuestPtr<Matrix>,
) -> u32 {
    if sprite(env, this.addr()).is_some() {
        OK
    } else {
        INVALID
    }
}

/// D3DXMatrixOrthoOffCenterLH over the viewport, shifted half a pixel so
/// texel centers land on pixel centers.
fn screen_projection(viewport: D3DVIEWPORT9) -> Matrix {
    let left = viewport.X as f32 + 0.5;
    let right = viewport.X as f32 + viewport.Width as f32 + 0.5;
    let top = viewport.Y as f32 + 0.5;
    let bottom = viewport.Y as f32 + viewport.Height as f32 + 0.5;
    let (near, far) = (viewport.MinZ, viewport.MaxZ);
    [
        2. / (right - left),
        0.,
        0.,
        0.,
        0.,
        2. / (top - bottom),
        0.,
        0.,
        0.,
        0.,
        1. / (far - near),
        0.,
        (left + right) / (left - right),
        (top + bottom) / (bottom - top),
        near / (near - far),
        1.,
    ]
}
/// The device state Wine's sprite sets, minus states this device only
/// supports at their defaults. Transforms are staged in `matrices`.
fn push_states(c: &mut Calls, device: u32, flags: u32, matrices: u32) {
    let blend = u32::from(flags & ALPHABLEND != 0);
    c.push(device, SET_VERTEX_SHADER, [0]);
    c.push(device, SET_PIXEL_SHADER, [0]);
    for (state, value) in [
        (27, blend), // ALPHABLENDENABLE
        (25, 5),     // ALPHAFUNC GREATER
        (24, 0),     // ALPHAREF
        (15, blend), // ALPHATESTENABLE
        (171, 1),    // BLENDOP ADD
        (136, 1),    // CLIPPING
        (152, 0),    // CLIPPLANEENABLE
        (168, 15),   // COLORWRITEENABLE
        (22, 1),     // CULLMODE NONE
        (20, 6),     // DESTBLEND INVSRCALPHA
        (145, 1),    // DIFFUSEMATERIALSOURCE COLOR1
        (8, 3),      // FILLMODE SOLID
        (28, 0),     // FOGENABLE
        (167, 0),    // INDEXEDVERTEXBLENDENABLE
        (137, 0),    // LIGHTING
        (206, 0),    // SEPARATEALPHABLENDENABLE
        (9, 2),      // SHADEMODE GOURAUD
        (29, 0),     // SPECULARENABLE
        (19, 5),     // SRCBLEND SRCALPHA
        (52, 0),     // STENCILENABLE
        (151, 0),    // VERTEXBLEND
        (128, 0),    // WRAP0
    ] {
        c.push(device, SET_RENDER, [state, value]);
    }
    for (stage, kind, value) in [
        (0, 5, 2),  // ALPHAARG1 TEXTURE
        (0, 6, 0),  // ALPHAARG2 DIFFUSE
        (0, 4, 4),  // ALPHAOP MODULATE
        (0, 2, 2),  // COLORARG1 TEXTURE
        (0, 3, 0),  // COLORARG2 DIFFUSE
        (0, 1, 4),  // COLOROP MODULATE
        (0, 11, 0), // TEXCOORDINDEX
        (0, 24, 0), // TEXTURETRANSFORMFLAGS DISABLE
        (1, 4, 1),  // ALPHAOP DISABLE
        (1, 1, 1),  // COLOROP DISABLE
    ] {
        c.push(device, SET_STAGE, [stage, kind, value]);
    }
    for (kind, value) in [
        (1, 3), // ADDRESSU CLAMP
        (2, 3), // ADDRESSV CLAMP
        (5, 2), // MAGFILTER LINEAR
        (9, 0), // MAXMIPLEVEL
        (10, 1),
        (6, 2), // MINFILTER LINEAR
        (7, 2), // MIPFILTER LINEAR
        (8, 0), // MIPMAPLODBIAS
        (11, 0),
    ] {
        c.push(device, SET_SAMPLER, [0, kind, value]);
    }
    if flags & OBJECTSPACE == 0 {
        c.push(device, SET_TRANSFORM, [256, matrices]); // WORLD
        c.push(device, SET_TRANSFORM, [2, matrices]); // VIEW
        c.push(device, SET_TRANSFORM, [3, matrices + 64]); // PROJECTION
    }
}

#[win32(d3dx9_43, name = "ID3DXSprite::Begin", level = implement)]
fn begin(env: &mut Env<'_>, this: GuestPtr<()>, flags: u32) -> Outcome {
    let this = this.addr();
    let Some(s) = sprite(env, this) else {
        return Outcome::Return(INVALID);
    };
    if s.ready {
        return Outcome::Return(INVALID);
    }
    let device = s.device;
    let Some(viewport) = alloc(env, &[0; size_of::<D3DVIEWPORT9>()]) else {
        return Outcome::Return(OOM);
    };
    // Calls free their temporaries before continuing, so continuations
    // that read results free those buffers themselves.
    let mut c = Calls::new();
    c.push(device, GET_VIEWPORT, [viewport]);
    run(
        env,
        c,
        Box::new(move |env| {
            let view = GuestPtr::<D3DVIEWPORT9>::new(viewport).read(env.cpu);
            free(env, viewport);
            let Ok(view) = view else {
                return Outcome::Return(INVALID);
            };
            let mut bytes = bytemuck::bytes_of(&IDENTITY).to_vec();
            bytes.extend_from_slice(bytemuck::bytes_of(&screen_projection(view)));
            // Holds the matrices, then the recorded block pointer.
            bytes.extend_from_slice(&[0; 4]);
            let Some(matrices) = alloc(env, &bytes) else {
                return Outcome::Return(OOM);
            };
            let mut c = Calls::new();
            let save = flags & DONOTSAVESTATE == 0;
            let recorded = sprite(env, this).map_or(0, |s| s.block);
            if save && recorded == 0 {
                // Tell the block what it must store, as Wine does.
                c.push(device, BEGIN_BLOCK, []);
                c.tolerant = true;
                push_states(&mut c, device, flags & !OBJECTSPACE, matrices);
                c.push(device, SET_FVF, [FVF]);
                c.push(device, SET_STREAM, [0, 0, 0, STRIDE]);
                c.push(device, SET_INDICES, [0]);
                c.push(device, SET_TEXTURE, [0, 0]);
                c.push(device, END_BLOCK, [matrices + 128]);
            }
            run(
                env,
                c,
                Box::new(move |env| {
                    let mut block = recorded;
                    if save && block == 0 {
                        block = env.cpu.read_u32(matrices + 128).unwrap_or(0);
                        if block == 0 {
                            free(env, matrices);
                            return Outcome::Return(INVALID);
                        }
                    }
                    let mut c = Calls::new();
                    c.temporary.push(matrices);
                    if save {
                        c.push(block, 4, []);
                    }
                    c.tolerant = true;
                    if flags & DONOTMODIFY_RENDERSTATE == 0 {
                        push_states(&mut c, device, flags, matrices);
                    }
                    let s = sprite_mut(env, this).unwrap();
                    s.block = block;
                    s.flags = flags;
                    s.ready = true;
                    calls(env, c, OK)
                }),
            )
        }),
    )
}

#[win32(d3dx9_43, name = "ID3DXSprite::Draw", level = implement)]
fn draw(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    texture: GuestPtr<()>,
    rect: GuestPtr<[i32; 4]>,
    center: GuestPtr<Vector>,
    position: GuestPtr<Vector>,
    color: u32,
) -> Outcome {
    let this = this.addr();
    let texture = texture.addr();
    let Some(s) = sprite(env, this) else {
        return Outcome::Return(INVALID);
    };
    if !s.ready || texture == 0 {
        return Outcome::Return(INVALID);
    }
    let (owned, transform) = (s.flags & DO_NOT_ADDREF_TEXTURE == 0, s.transform);
    let (Some(rect), Some(center), Some(position)) = (
        read::<[i32; 4]>(env, rect.addr()),
        read::<Vector>(env, center.addr()),
        read::<Vector>(env, position.addr()),
    ) else {
        return Outcome::Return(INVALID);
    };
    let Some(desc) = alloc(env, &[0; 32]) else {
        return Outcome::Return(OOM);
    };
    let mut c = Calls::new();
    c.push(texture, 17, [0, desc]); // GetLevelDesc(0)
    if owned {
        c.push(texture, 1, []);
    }
    run(
        env,
        c,
        Box::new(move |env| {
            let mut size = [0u32; 2];
            let read = env.cpu.read(desc + 24, bytemuck::cast_slice_mut(&mut size));
            free(env, desc);
            if read.is_err() {
                return Outcome::Return(INVALID);
            }
            let quad = Quad {
                texture,
                owned,
                rect: rect.unwrap_or([0, 0, size[0] as i32, size[1] as i32]),
                size,
                center: center.unwrap_or_default(),
                position: position.unwrap_or_default(),
                color,
                transform,
            };
            // Sorting flags keep submission order, as in Wine.
            sprite_mut(env, this).unwrap().queue.push(quad);
            Outcome::Return(OK)
        }),
    )
}

/// Six corner vertices (two triangles) for one quad.
fn vertices(q: &Quad) -> [[u32; 6]; 6] {
    let width = (q.rect[2] - q.rect[0]) as f32;
    let height = (q.rect[3] - q.rect[1]) as f32;
    let (u0, v0) = (
        q.rect[0] as f32 / q.size[0] as f32,
        q.rect[1] as f32 / q.size[1] as f32,
    );
    let (u1, v1) = (
        q.rect[2] as f32 / q.size[0] as f32,
        q.rect[3] as f32 / q.size[1] as f32,
    );
    let corner = |dx: f32, dy: f32, u: f32, v: f32| {
        let p = transform_value(
            [
                q.position[0] - q.center[0] + dx,
                q.position[1] - q.center[1] + dy,
                q.position[2] - q.center[2],
                1.,
            ],
            q.transform,
        );
        [
            (p[0] / p[3]).to_bits(),
            (p[1] / p[3]).to_bits(),
            (p[2] / p[3]).to_bits(),
            q.color,
            u.to_bits(),
            v.to_bits(),
        ]
    };
    let a = corner(0., 0., u0, v0);
    let b = corner(width, 0., u1, v0);
    let c = corner(width, height, u1, v1);
    let d = corner(0., height, u0, v1);
    [a, b, c, d, a, c]
}
/// Queue the draws of every pending quad, then release their textures.
fn push_flush(env: &mut Env<'_>, this: u32, c: &mut Calls) -> Result<(), u32> {
    let s = sprite_mut(env, this).ok_or(INVALID)?;
    let device = s.device;
    let queue = std::mem::take(&mut s.queue);
    if queue.is_empty() {
        return Ok(());
    }
    let data: Vec<[[u32; 6]; 6]> = queue.iter().map(vertices).collect();
    let Some(buffer) = alloc(env, bytemuck::cast_slice(&data)) else {
        release_queue(c, queue);
        return Err(OOM);
    };
    c.temporary.push(buffer);
    let mut start = 0;
    while start < queue.len() {
        let texture = queue[start].texture;
        let count = queue[start..]
            .iter()
            .take_while(|q| q.texture == texture)
            .count();
        c.push(device, SET_TEXTURE, [0, texture]);
        c.push(device, SET_FVF, [FVF]);
        c.push(
            device,
            DRAW_UP,
            [
                4, // TRIANGLELIST
                2 * count as u32,
                buffer + start as u32 * 6 * STRIDE,
                STRIDE,
            ],
        );
        start += count;
    }
    release_queue(c, queue);
    Ok(())
}
#[win32(d3dx9_43, name = "ID3DXSprite::Flush", level = implement)]
fn flush(env: &mut Env<'_>, this: GuestPtr<()>) -> Outcome {
    if !sprite(env, this.addr()).is_some_and(|s| s.ready) {
        return Outcome::Return(INVALID);
    }
    let mut c = Calls::new();
    c.tolerant = true;
    let result = push_flush(env, this.addr(), &mut c).err().unwrap_or(OK);
    calls(env, c, result)
}
#[win32(d3dx9_43, name = "ID3DXSprite::End", level = implement)]
fn end(env: &mut Env<'_>, this: GuestPtr<()>) -> Outcome {
    let this = this.addr();
    if !sprite(env, this).is_some_and(|s| s.ready) {
        return Outcome::Return(INVALID);
    }
    let mut c = Calls::new();
    c.tolerant = true;
    let result = push_flush(env, this, &mut c).err().unwrap_or(OK);
    let s = sprite_mut(env, this).unwrap();
    s.ready = false;
    if s.block != 0 && s.flags & DONOTSAVESTATE == 0 {
        c.push(s.block, 5, []);
    }
    calls(env, c, result)
}
fn reset_calls(env: &mut Env<'_>, this: u32, c: &mut Calls) -> bool {
    let Some(s) = sprite_mut(env, this) else {
        return false;
    };
    let queue = std::mem::take(&mut s.queue);
    s.flags = 0;
    s.ready = false;
    release_queue(c, queue);
    true
}
#[win32(d3dx9_43, name = "ID3DXSprite::OnLostDevice", level = implement)]
fn lost(env: &mut Env<'_>, this: GuestPtr<()>) -> Outcome {
    let mut c = Calls::new();
    c.tolerant = true;
    if let Some(s) = sprite_mut(env, this.addr()) {
        let block = std::mem::take(&mut s.block);
        if block != 0 {
            c.push(block, 2, []);
        }
    }
    if !reset_calls(env, this.addr(), &mut c) {
        return Outcome::Return(INVALID);
    }
    calls(env, c, OK)
}
#[win32(d3dx9_43, name = "ID3DXSprite::OnResetDevice", level = implement)]
fn reset(env: &mut Env<'_>, this: GuestPtr<()>) -> Outcome {
    let mut c = Calls::new();
    c.tolerant = true;
    if !reset_calls(env, this.addr(), &mut c) {
        return Outcome::Return(INVALID);
    }
    calls(env, c, OK)
}

#[win32(d3dx9_43, name = "D3DXMatrixTransformation2D", level = implement)]
fn transformation_2d(
    env: &mut Env<'_>,
    output: GuestPtr<Matrix>,
    scaling_center: GuestPtr<Vector2>,
    scaling_rotation: f32,
    scaling: GuestPtr<Vector2>,
    rotation_center: GuestPtr<Vector2>,
    rotation: f32,
    translation: GuestPtr<Vector2>,
) -> GuestPtr<Matrix> {
    let (Some(sc), Some(s), Some(rc), Some(t)) = (
        read::<Vector2>(env, scaling_center.addr()),
        read::<Vector2>(env, scaling.addr()),
        read::<Vector2>(env, rotation_center.addr()),
        read::<Vector2>(env, translation.addr()),
    ) else {
        return GuestPtr::default();
    };
    let value = transformation_2d_value(
        sc.unwrap_or_default(),
        scaling_rotation,
        s.unwrap_or([1., 1.]),
        rc.unwrap_or_default(),
        rotation,
        t.unwrap_or_default(),
    );
    if output.write(env.cpu, value).is_ok() {
        output
    } else {
        GuestPtr::default()
    }
}
/// D3DXMatrixTransformation restricted to the XY plane:
/// Msc⁻¹ · Msr⁻¹ · Ms · Msr · Msc · Mrc⁻¹ · Mr · Mrc · Mt.
fn transformation_2d_value(
    sc: Vector2,
    scaling_rotation: f32,
    s: Vector2,
    rc: Vector2,
    rotation: f32,
    t: Vector2,
) -> Matrix {
    let rotate = |angle: f32| {
        let (sin, cos) = angle.sin_cos();
        [
            cos, sin, 0., 0., -sin, cos, 0., 0., 0., 0., 1., 0., 0., 0., 0., 1.,
        ]
    };
    [
        translation_value(-sc[0], -sc[1], 0.),
        rotate(-scaling_rotation),
        scaling_value(s[0], s[1], 1.),
        rotate(scaling_rotation),
        translation_value(sc[0] - rc[0], sc[1] - rc[1], 0.),
        rotate(rotation),
        translation_value(rc[0] + t[0], rc[1] + t[1], 0.),
    ]
    .into_iter()
    .fold(IDENTITY, product)
}
