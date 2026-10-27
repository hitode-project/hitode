//! Vertex/index resource storage and binding edges over the common COM arena.
use super::*;
#[cfg(test)]
mod tests;
use adv_abi::types::{D3DINDEXBUFFER_DESC, D3DVERTEXBUFFER_DESC};
const MAX_BUFFER: u32 = 16 * 1024 * 1024;
const IID_VERTEX: Iid = [
    0xb5, 0xb1, 0x4b, 0xb6, 0x70, 0xfd, 0xf6, 0x4d, 0xbf, 0x91, 0x19, 0xd0, 0xa1, 0x24, 0x55, 0xe3,
];
const IID_INDEX: Iid = [
    0x5e, 0xd6, 0x9d, 0x7c, 0xf7, 0xd3, 0x29, 0x45, 0xac, 0xee, 0x78, 0x58, 0x30, 0xac, 0xde, 0x35,
];

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Vertex,
    Index,
}
pub(super) struct Buffer {
    pub pointer: u32,
    pub device: Id,
    pub kind: Kind,
    pub storage: u32,
    pub len: u32,
    pub usage: u32,
    pub pool: i32,
    pub format: i32,
    pub fvf: u32,
    pub locks: u32,
}
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Stream {
    pub buffer: Option<Id>,
    pub offset: u32,
    pub stride: u32,
}
#[derive(Clone, Default)]
pub(super) struct Bindings {
    pub streams: [Stream; 16],
    pub indices: Option<Id>,
    pub declaration: Option<Id>,
}
pub(super) fn register(registry: &mut Registry) {
    for api in [
        vertex_query_ABI,
        vertex_add_ref_ABI,
        vertex_release_ABI,
        index_query_ABI,
        index_add_ref_ABI,
        index_release_ABI,
        create_vertex_ABI,
        create_index_ABI,
        vertex_desc_ABI,
        index_desc_ABI,
        vertex_device_ABI,
        index_device_ABI,
        vertex_type_ABI,
        index_type_ABI,
        vertex_lock_ABI,
        index_lock_ABI,
        vertex_unlock_ABI,
        index_unlock_ABI,
        super::unchanged::unless_unchanged!(set_stream_ABI, super::unchanged::binding),
        get_stream_ABI,
        super::unchanged::unless_unchanged!(set_indices_ABI, super::unchanged::binding),
        get_indices_ABI,
        draw_ABI,
        draw_indexed_ABI,
        draw_indexed_up_ABI,
    ] {
        registry.register(api);
    }
}
unknown!(
    vertex_query,
    vertex_add_ref,
    vertex_release,
    "IDirect3DVertexBuffer9::QueryInterface",
    "IDirect3DVertexBuffer9::AddRef",
    "IDirect3DVertexBuffer9::Release"
);
unknown!(
    index_query,
    index_add_ref,
    index_release,
    "IDirect3DIndexBuffer9::QueryInterface",
    "IDirect3DIndexBuffer9::AddRef",
    "IDirect3DIndexBuffer9::Release"
);
pub(super) fn buffer<'a>(env: &'a Env<'_>, pointer: u32) -> Option<&'a Buffer> {
    match com(env).get::<Payload>(&env.sys.objects, pointer)? {
        Payload::Buffer(buffer) => Some(buffer),
        _ => None,
    }
}
pub(super) fn parent_on_last_public_release(env: &Env<'_>, pointer: u32) -> Option<Id> {
    let buffer = buffer(env, pointer)?;
    let id = com(env).identity::<Payload>(&env.sys.objects, pointer)?;
    (com(env).references(&env.sys.objects, id)? == 1).then_some(buffer.device)
}
pub(super) fn parent_on_first_public_retain(env: &Env<'_>, pointer: u32) -> Option<Id> {
    let buffer = buffer(env, pointer)?;
    let id = com(env).identity::<Payload>(&env.sys.objects, pointer)?;
    (com(env).references(&env.sys.objects, id)? == 0).then_some(buffer.device)
}
pub(super) fn destroy(env: &mut Env<'_>, buffer: Buffer) {
    free(env, buffer.storage);
}
pub(super) fn release_bindings(env: &mut Env<'_>, bindings: Bindings) {
    for id in bindings
        .streams
        .into_iter()
        .filter_map(|s| s.buffer)
        .chain(bindings.indices)
        .chain(bindings.declaration)
    {
        if let Some(Some(dead)) = com(env).release_internal(&mut env.sys.objects, id) {
            super::destroy(env, dead);
        }
    }
}

fn create(
    env: &mut Env<'_>,
    this: u32,
    len: u32,
    usage: u32,
    pool: i32,
    format: i32,
    fvf: u32,
    kind: Kind,
    output: GuestPtr<u32>,
    shared: GuestPtr<u32>,
) -> u32 {
    if output.write(env.cpu, 0).is_err() {
        return INVALID;
    }
    let Some(device) = com(env)
        .identity::<Payload>(&env.sys.objects, this)
        .filter(|_| super::device(env, this).is_some())
    else {
        return INVALID;
    };
    if len == 0 || len > MAX_BUFFER || !matches!(pool, 0..=2) || usage & 0x200 != 0 && pool == 1 {
        return INVALID;
    }
    if !super::device(env, this)
        .unwrap()
        .creation
        .allows_buffer_usage(usage)
    {
        return INVALID;
    }
    if !shared.is_null() || usage & !(8 | 0x10 | 0x20 | 0x40 | 0x80 | 0x100 | 0x200) != 0 {
        return UNAVAILABLE;
    }
    if kind == Kind::Index && !matches!(format, 101 | 102) {
        return INVALID;
    }
    let Some(storage) = alloc(env, len) else {
        return OUT_OF_MEMORY;
    };
    if com(env)
        .add_ref_identity(&mut env.sys.objects, device)
        .is_none()
    {
        free(env, storage);
        return OUT_OF_MEMORY;
    }
    let (interface, iid) = if kind == Kind::Vertex {
        ("IDirect3DVertexBuffer9", IID_VERTEX)
    } else {
        ("IDirect3DIndexBuffer9", IID_INDEX)
    };
    let payload = Buffer {
        pointer: 0,
        device,
        kind,
        storage,
        len,
        usage,
        pool,
        format,
        fvf,
        locks: 0,
    };
    let Some((pointer, id)) = object(env, interface, iid, Payload::Buffer(payload)) else {
        free(env, storage);
        let _ = com(env).release_identity(&mut env.sys.objects, device);
        return OUT_OF_MEMORY;
    };
    if let Some(Payload::Buffer(buffer)) = com(env).get_identity_mut(&mut env.sys.objects, id) {
        buffer.pointer = pointer;
    }
    if !com(env).register_interface(&mut env.sys.objects, id, resources::IID_RESOURCE, pointer) {
        release(env, pointer);
        return OUT_OF_MEMORY;
    }
    if output.write(env.cpu, pointer).is_err() {
        release(env, pointer);
        return INVALID;
    }
    0
}
#[win32(d3d9,name="IDirect3DDevice9::CreateVertexBuffer",level=implement)]
fn create_vertex(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    len: u32,
    usage: u32,
    fvf: u32,
    pool: i32,
    output: GuestPtr<u32>,
    shared: GuestPtr<u32>,
) -> u32 {
    create(
        env,
        this.addr(),
        len,
        usage,
        pool,
        100,
        fvf,
        Kind::Vertex,
        output,
        shared,
    )
}
#[win32(d3d9,name="IDirect3DDevice9::CreateIndexBuffer",level=implement)]
fn create_index(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    len: u32,
    usage: u32,
    format: i32,
    pool: i32,
    output: GuestPtr<u32>,
    shared: GuestPtr<u32>,
) -> u32 {
    create(
        env,
        this.addr(),
        len,
        usage,
        pool,
        format,
        0,
        Kind::Index,
        output,
        shared,
    )
}
#[win32(d3d9,name="IDirect3DVertexBuffer9::GetDesc",level=implement)]
fn vertex_desc(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    output: GuestPtr<D3DVERTEXBUFFER_DESC>,
) -> u32 {
    let Some(b) = buffer(env, this.addr()).filter(|b| b.kind == Kind::Vertex) else {
        return INVALID;
    };
    let desc = D3DVERTEXBUFFER_DESC {
        Format: b.format as u32,
        Type: 6,
        Usage: b.usage,
        Pool: b.pool,
        Size: b.len,
        FVF: b.fvf,
    };
    output.write(env.cpu, desc).map_or(INVALID, |()| 0)
}
#[win32(d3d9,name="IDirect3DIndexBuffer9::GetDesc",level=implement)]
fn index_desc(env: &mut Env<'_>, this: GuestPtr<()>, output: GuestPtr<D3DINDEXBUFFER_DESC>) -> u32 {
    let Some(b) = buffer(env, this.addr()).filter(|b| b.kind == Kind::Index) else {
        return INVALID;
    };
    let desc = D3DINDEXBUFFER_DESC {
        Format: b.format as u32,
        Type: 7,
        Usage: b.usage,
        Pool: b.pool,
        Size: b.len,
    };
    output.write(env.cpu, desc).map_or(INVALID, |()| 0)
}
fn get_device(env: &mut Env<'_>, this: u32, kind: Kind, output: GuestPtr<u32>) -> u32 {
    if output.write(env.cpu, 0).is_err() {
        return INVALID;
    }
    let Some(b) = buffer(env, this).filter(|b| b.kind == kind) else {
        return INVALID;
    };
    let Some(Payload::Device(d)) = com(env).get_identity(&env.sys.objects, b.device) else {
        return INVALID;
    };
    let pointer = d.pointer;
    if add_ref(env, pointer).is_none() {
        return OUT_OF_MEMORY;
    }
    if output.write(env.cpu, pointer).is_err() {
        release(env, pointer);
        return INVALID;
    }
    0
}
#[win32(d3d9,name="IDirect3DVertexBuffer9::GetDevice",level=implement)]
fn vertex_device(env: &mut Env<'_>, this: GuestPtr<()>, output: GuestPtr<u32>) -> u32 {
    get_device(env, this.addr(), Kind::Vertex, output)
}
#[win32(d3d9,name="IDirect3DIndexBuffer9::GetDevice",level=implement)]
fn index_device(env: &mut Env<'_>, this: GuestPtr<()>, output: GuestPtr<u32>) -> u32 {
    get_device(env, this.addr(), Kind::Index, output)
}
#[win32(d3d9,name="IDirect3DVertexBuffer9::GetType",level=implement)]
fn vertex_type(env: &mut Env<'_>, this: GuestPtr<()>) -> u32 {
    if buffer(env, this.addr()).is_some_and(|b| b.kind == Kind::Vertex) {
        6
    } else {
        0
    }
}
#[win32(d3d9,name="IDirect3DIndexBuffer9::GetType",level=implement)]
fn index_type(env: &mut Env<'_>, this: GuestPtr<()>) -> u32 {
    if buffer(env, this.addr()).is_some_and(|b| b.kind == Kind::Index) {
        7
    } else {
        0
    }
}

fn lock(
    env: &mut Env<'_>,
    this: u32,
    kind: Kind,
    offset: u32,
    size: u32,
    output: GuestPtr<u32>,
    flags: u32,
) -> u32 {
    let Some(b) = buffer(env, this).filter(|b| b.kind == kind) else {
        return INVALID;
    };
    // Native optimized drivers may omit this validation. Never expose a Lock
    // range outside the resource's real allocation or silently wrap arithmetic.
    let size = if size == 0 {
        b.len.checked_sub(offset)
    } else {
        Some(size)
    };
    if offset > b.len
        || size.is_none_or(|n| offset.checked_add(n).is_none_or(|end| end > b.len))
        || flags & !(0x10 | 0x800 | 0x1000 | 0x2000 | 0x8000) != 0
    {
        return INVALID;
    }
    let Some(locks) = b.locks.checked_add(1) else {
        return OUT_OF_MEMORY;
    };
    let Some(pointer) = b.storage.checked_add(offset) else {
        return INVALID;
    };
    if output.write(env.cpu, pointer).is_err() {
        return INVALID;
    }
    // Storage is guest memory, not a host slice. Draw snapshots have already
    // consumed earlier data, so DISCARD/NOOVERWRITE never race the GPU reader.
    let Some(Payload::Buffer(b)) = com(env).get_mut::<Payload>(&mut env.sys.objects, this) else {
        return INVALID;
    };
    b.locks = locks;
    // A retained signature prefix has already checked its stream locks.
    let owner = b.device;
    if let Some(Payload::Device(d)) = com(env).get_identity_mut(&mut env.sys.objects, owner) {
        d.frame.invalidate_streams();
    }
    0
}
fn unlock(env: &mut Env<'_>, this: u32, kind: Kind) -> u32 {
    let Some(Payload::Buffer(b)) = com(env).get_mut::<Payload>(&mut env.sys.objects, this) else {
        return INVALID;
    };
    if b.kind != kind {
        return INVALID;
    }
    b.locks = b.locks.saturating_sub(1);
    0
}
#[win32(d3d9,name="IDirect3DVertexBuffer9::Lock",level=implement)]
fn vertex_lock(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    offset: u32,
    size: u32,
    output: GuestPtr<u32>,
    flags: u32,
) -> u32 {
    lock(env, this.addr(), Kind::Vertex, offset, size, output, flags)
}
#[win32(d3d9,name="IDirect3DIndexBuffer9::Lock",level=implement)]
fn index_lock(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    offset: u32,
    size: u32,
    output: GuestPtr<u32>,
    flags: u32,
) -> u32 {
    lock(env, this.addr(), Kind::Index, offset, size, output, flags)
}
#[win32(d3d9,name="IDirect3DVertexBuffer9::Unlock",level=implement)]
fn vertex_unlock(env: &mut Env<'_>, this: GuestPtr<()>) -> u32 {
    unlock(env, this.addr(), Kind::Vertex)
}
#[win32(d3d9,name="IDirect3DIndexBuffer9::Unlock",level=implement)]
fn index_unlock(env: &mut Env<'_>, this: GuestPtr<()>) -> u32 {
    unlock(env, this.addr(), Kind::Index)
}

#[win32(d3d9,name="IDirect3DDevice9::SetStreamSource",level=state)]
fn set_stream(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    stream: u32,
    pointer: GuestPtr<()>,
    offset: u32,
    stride: u32,
) -> u32 {
    let Some(d) = device(env, this.addr()) else {
        return INVALID;
    };
    let Some(old) = d.buffers.streams.get(stream as usize).copied() else {
        return INVALID;
    };
    let device = com(env)
        .identity::<Payload>(&env.sys.objects, this.addr())
        .unwrap();
    let new = if pointer.is_null() {
        None
    } else {
        let Some(new) = com(env).identity::<Payload>(&env.sys.objects, pointer.addr()) else {
            return INVALID;
        };
        Some(new)
    };
    if d.recording.is_none()
        && new == old.buffer
        && (new.is_none() || (old.offset == offset && old.stride == stride))
    {
        return 0;
    }
    if let Some(new) = new {
        let Some(Payload::Buffer(b)) = com(env).get_identity(&env.sys.objects, new) else {
            return INVALID;
        };
        if b.kind != Kind::Vertex || b.device != device || offset > b.len || stride > 2048 {
            return INVALID;
        }
    }
    let recorded = Stream {
        buffer: new,
        offset: if new.is_some() { offset } else { old.offset },
        stride: if new.is_some() { stride } else { old.stride },
    };
    if let Some(result) = super::shaders::stateblocks::record_stream(env, device, stream, recorded)
    {
        return result;
    }
    if new != old.buffer
        && new.is_some_and(|id| !com(env).retain_internal(&mut env.sys.objects, id))
    {
        return OUT_OF_MEMORY;
    }
    let Some(Payload::Device(d)) = com(env).get_identity_mut(&mut env.sys.objects, device) else {
        return INVALID;
    };
    d.buffers.streams[stream as usize] = Stream {
        buffer: new,
        offset: if new.is_some() { offset } else { old.offset },
        stride: if new.is_some() { stride } else { old.stride },
    };
    d.touch(10);
    if new != old.buffer
        && let Some(id) = old.buffer
        && let Some(Some(dead)) = com(env).release_internal(&mut env.sys.objects, id)
    {
        super::destroy(env, dead);
    }
    0
}
#[win32(d3d9,name="IDirect3DDevice9::GetStreamSource",level=implement)]
fn get_stream(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    stream: u32,
    output: GuestPtr<u32>,
    offset: GuestPtr<u32>,
    stride: GuestPtr<u32>,
) -> u32 {
    let Some(d) = device(env, this.addr()) else {
        return INVALID;
    };
    if d.needs_reset {
        return INVALID;
    }
    let Some(s) = d.buffers.streams.get(stream as usize).copied() else {
        return INVALID;
    };
    if [output, offset, stride]
        .iter()
        .any(|p| adv_abi::guest::check_range(env.cpu, p.addr(), 4, Prot::WRITE).is_err())
    {
        return INVALID;
    }
    let pointer = match s.buffer {
        Some(id) => match com(env).get_identity(&env.sys.objects, id) {
            Some(Payload::Buffer(b)) => b.pointer,
            _ => return INVALID,
        },
        None => 0,
    };
    if pointer != 0 && add_ref(env, pointer).is_none() {
        return OUT_OF_MEMORY;
    }
    if output
        .write(env.cpu, pointer)
        .and_then(|()| offset.write(env.cpu, s.offset))
        .and_then(|()| stride.write(env.cpu, s.stride))
        .is_err()
    {
        if pointer != 0 {
            release(env, pointer);
        }
        return INVALID;
    }
    0
}
#[win32(d3d9,name="IDirect3DDevice9::SetIndices",level=state)]
fn set_indices(env: &mut Env<'_>, this: GuestPtr<()>, pointer: GuestPtr<()>) -> u32 {
    let Some(d) = device(env, this.addr()) else {
        return INVALID;
    };
    let old = d.buffers.indices;
    let device = com(env)
        .identity::<Payload>(&env.sys.objects, this.addr())
        .unwrap();
    let new = if pointer.is_null() {
        None
    } else {
        let Some(new) = com(env).identity::<Payload>(&env.sys.objects, pointer.addr()) else {
            return INVALID;
        };
        Some(new)
    };
    if d.recording.is_none() && new == old {
        return 0;
    }
    if new.is_some_and(|new| !matches!(com(env).get_identity(&env.sys.objects, new), Some(Payload::Buffer(b)) if b.kind == Kind::Index && b.device == device)) { return INVALID; }
    if let Some(result) = super::shaders::stateblocks::record_indices(env, device, new) {
        return result;
    }
    if new != old && new.is_some_and(|id| !com(env).retain_internal(&mut env.sys.objects, id)) {
        return OUT_OF_MEMORY;
    }
    let Some(Payload::Device(d)) = com(env).get_identity_mut(&mut env.sys.objects, device) else {
        return INVALID;
    };
    d.buffers.indices = new;
    d.touch(11);
    if new != old
        && let Some(id) = old
        && let Some(Some(dead)) = com(env).release_internal(&mut env.sys.objects, id)
    {
        super::destroy(env, dead);
    }
    0
}
#[win32(d3d9,name="IDirect3DDevice9::GetIndices",level=implement)]
fn get_indices(env: &mut Env<'_>, this: GuestPtr<()>, output: GuestPtr<u32>) -> u32 {
    let Some(d) = device(env, this.addr()) else {
        return INVALID;
    };
    let pointer = match d.buffers.indices {
        Some(id) => match com(env).get_identity(&env.sys.objects, id) {
            Some(Payload::Buffer(b)) => b.pointer,
            _ => return INVALID,
        },
        None => 0,
    };
    if adv_abi::guest::check_range(env.cpu, output.addr(), 4, Prot::WRITE).is_err() {
        return INVALID;
    }
    if pointer != 0 && add_ref(env, pointer).is_none() {
        return OUT_OF_MEMORY;
    }
    if output.write(env.cpu, pointer).is_err() {
        if pointer != 0 {
            release(env, pointer);
        }
        return INVALID;
    }
    0
}
pub(super) fn clear_up_bindings(env: &mut Env<'_>, this: u32, indexed: bool) {
    let Some(Payload::Device(d)) = com(env).get_mut::<Payload>(&mut env.sys.objects, this) else {
        return;
    };
    let old = std::mem::take(&mut d.buffers.streams[0]);
    if old.buffer.is_some() || old.offset != 0 || old.stride != 0 {
        d.touch(10);
    }
    let stream = old.buffer;
    let indices = if indexed {
        d.buffers.indices.take()
    } else {
        None
    };
    if indices.is_some() {
        d.touch(11);
    }
    for id in [stream, indices].into_iter().flatten() {
        if let Some(Some(dead)) = com(env).release_internal(&mut env.sys.objects, id) {
            super::destroy(env, dead);
        }
    }
}

pub(super) fn stream_data(env: &Env<'_>, this: u32, number: u16) -> Result<(u32, u32, u32), u32> {
    let d = device(env, this).ok_or(INVALID)?;
    let stream = *d.buffers.streams.get(number as usize).ok_or(INVALID)?;
    let id = stream.buffer.ok_or(INVALID)?;
    let Some(Payload::Buffer(b)) = com(env).get_identity(&env.sys.objects, id) else {
        return Err(INVALID);
    };
    if b.kind != Kind::Vertex || b.locks != 0 || stream.offset > b.len {
        return Err(INVALID);
    }
    Ok((
        b.storage.checked_add(stream.offset).ok_or(INVALID)?,
        stream.stride,
        b.len - stream.offset,
    ))
}
fn submit(env: &mut Env<'_>, this: u32, draw: fixed::Draw, up: Option<bool>) -> Outcome {
    let id = com(env)
        .identity::<Payload>(&env.sys.objects, this)
        .unwrap();
    if let Some(indexed) = up {
        clear_up_bindings(env, this, indexed);
    }
    dispatch(env, id, Operation::Draw(draw))
}
#[win32(d3d9,name="IDirect3DDevice9::DrawPrimitive",level=implement)]
fn draw(env: &mut Env<'_>, this: GuestPtr<()>, kind: i32, start: u32, count: u32) -> Outcome {
    if count == 0 {
        return Outcome::Return(
            fixed::empty_draw(env, this.addr(), kind).map_or_else(|e| e, |()| 0),
        );
    }
    if super::programmable::active(env, this.addr()) {
        return match super::programmable::prepare(env, this.addr(), kind, count, None, None, start)
        {
            Ok(draw) => submit(env, this.addr(), draw, None),
            Err(e) => Outcome::Return(e),
        };
    }
    let result = fixed::prepare_bound(env, this.addr(), kind, count, start, None);
    match result {
        Ok(draw) => submit(env, this.addr(), draw, None),
        Err(error) => Outcome::Return(error),
    }
}
fn decode_indices(
    env: &Env<'_>,
    data: u32,
    format: i32,
    count: u32,
    minimum: u32,
    vertices: u32,
    base: i32,
) -> Result<Vec<u32>, u32> {
    let width = match format {
        101 => 2,
        102 => 4,
        _ => return Err(INVALID),
    };
    let bytes = count.checked_mul(width).ok_or(INVALID)?;
    adv_abi::guest::check_range(env.cpu, data, bytes, Prot::READ).map_err(|_| INVALID)?;
    let end = minimum
        .checked_add(vertices)
        .filter(|_| vertices > 0)
        .ok_or(INVALID)?;
    let mut indices = Vec::with_capacity(count as usize);
    for ordinal in 0..count {
        let at = data + ordinal * width;
        let index = if width == 2 {
            u32::from(env.cpu.read_u16(at).map_err(|_| INVALID)?)
        } else {
            env.cpu.read_u32(at).map_err(|_| INVALID)?
        };
        if index < minimum || index >= end || index > 0x000f_ffff {
            return Err(INVALID);
        }
        let adjusted = i64::from(index) + i64::from(base);
        let physical = u32::try_from(adjusted).map_err(|_| INVALID)?;
        indices.push(physical);
    }
    Ok(indices)
}
#[win32(d3d9,name="IDirect3DDevice9::DrawIndexedPrimitive",level=implement)]
fn draw_indexed(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    kind: i32,
    base: i32,
    minimum: u32,
    vertices: u32,
    start: u32,
    count: u32,
) -> Outcome {
    if count == 0 {
        return Outcome::Return(
            fixed::empty_draw(env, this.addr(), kind).map_or_else(|e| e, |()| 0),
        );
    }
    let result = (|| {
        if kind == 1 {
            return Err(INVALID);
        }
        let d = device(env, this.addr()).ok_or(INVALID)?;
        let Some(Payload::Buffer(b)) =
            com(env).get_identity(&env.sys.objects, d.buffers.indices.ok_or(INVALID)?)
        else {
            return Err(INVALID);
        };
        if b.kind != Kind::Index || b.locks != 0 {
            return Err(INVALID);
        }
        let width = if b.format == 101 { 2 } else { 4 };
        let length = fixed::primitive_length(kind, count)?;
        let first = start.checked_mul(width).ok_or(INVALID)?;
        if start
            .checked_add(length)
            .and_then(|v| v.checked_mul(width))
            .is_none_or(|end| end > b.len)
        {
            return Err(INVALID);
        }
        let indices = decode_indices(
            env,
            b.storage.checked_add(first).ok_or(INVALID)?,
            b.format,
            length,
            minimum,
            vertices,
            base,
        )?;
        Ok(indices)
    })();
    let indices = match result {
        Ok(indices) => indices,
        Err(error) => return Outcome::Return(error),
    };
    if super::programmable::active(env, this.addr()) {
        return recording::indexed(env, this.addr(), (kind, count, &indices), |env| {
            super::programmable::prepare(env, this.addr(), kind, count, None, Some(&indices), 0)
        });
    }
    let result = fixed::prepare_bound(env, this.addr(), kind, count, 0, Some(&indices));
    match result {
        Ok(draw) => submit(env, this.addr(), draw, None),
        Err(error) => Outcome::Return(error),
    }
}
#[win32(d3d9,name="IDirect3DDevice9::DrawIndexedPrimitiveUP",level=implement)]
fn draw_indexed_up(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    kind: i32,
    minimum: u32,
    vertices: u32,
    count: u32,
    indices: GuestPtr<u8>,
    format: i32,
    data: GuestPtr<u8>,
    stride: u32,
) -> Outcome {
    if count == 0 {
        if let Err(error) = fixed::empty_draw(env, this.addr(), kind) {
            return Outcome::Return(error);
        }
        clear_up_bindings(env, this.addr(), true);
        return Outcome::Return(0);
    }
    let result = (|| {
        if kind == 1 {
            return Err(INVALID);
        }
        let length = fixed::primitive_length(kind, count)?;
        let indices = decode_indices(env, indices.addr(), format, length, minimum, vertices, 0)?;
        if super::programmable::active(env, this.addr()) {
            super::programmable::prepare(
                env,
                this.addr(),
                kind,
                count,
                Some((data, stride)),
                Some(&indices),
                0,
            )
        } else {
            fixed::prepare(env, this.addr(), kind, count, data, stride, Some(&indices))
        }
    })();
    match result {
        Ok(draw) => submit(env, this.addr(), draw, Some(true)),
        Err(error) => Outcome::Return(error),
    }
}
pub(super) fn reset_bindings(env: &mut Env<'_>, device: Id) {
    let Some(Payload::Device(device)) = com(env).get_identity_mut(&mut env.sys.objects, device)
    else {
        return;
    };
    let bindings = std::mem::take(&mut device.buffers);
    release_bindings(env, bindings);
}
pub(super) fn has_outstanding_default(env: &Env<'_>, device: Id) -> bool {
    let com = com(env);
    let Some(Payload::Device(d)) = com.get_identity(&env.sys.objects, device) else {
        return false;
    };
    d.children.values().any(|&id| {
        matches!(com.get_identity(&env.sys.objects, id),
            Some(Payload::Buffer(buffer)) if buffer.pool == 0
                && com.references(&env.sys.objects, id).is_some_and(|n| n != 0))
    })
}
