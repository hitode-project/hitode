//! Vertex declarations stored as created and mapped onto the fixed-function
//! vertex decoder at draw time. Without programmable shaders a declaration is
//! drawable only when every element names an input that decoder reads.
use super::*;
#[cfg(test)]
mod tests;
const IID_DECLARATION: Iid = [
    0x9c, 0xc5, 0x13, 0xdd, 0xfa, 0x36, 0x98, 0x40, 0xa8, 0xfb, 0xc7, 0xed, 0x39, 0xdc, 0x85, 0x46,
];
/// MAXD3DDECLLENGTH elements plus the D3DDECL_END terminator.
const MAX_ELEMENTS: usize = 65;

pub(super) struct Declaration {
    pub pointer: u32,
    pub device: Id,
    /// D3DVERTEXELEMENT9 records as created, including the terminator.
    pub elements: Vec<[u8; 8]>,
}
pub(super) fn register(registry: &mut Registry) {
    for api in [
        declaration_query_ABI,
        declaration_add_ref_ABI,
        declaration_release_ABI,
        declaration_device_ABI,
        declaration_elements_ABI,
        create_ABI,
        adv_abi::Api {
            env_leaf: Some(super::leaf_calls::binding),
            ..set_ABI
        },
        get_ABI,
    ] {
        registry.register(api);
    }
}
unknown!(
    declaration_query,
    declaration_add_ref,
    declaration_release,
    "IDirect3DVertexDeclaration9::QueryInterface",
    "IDirect3DVertexDeclaration9::AddRef",
    "IDirect3DVertexDeclaration9::Release"
);
fn declaration<'a>(env: &'a Env<'_>, pointer: u32) -> Option<&'a Declaration> {
    match com(env).get::<Payload>(&env.sys.objects, pointer)? {
        Payload::Declaration(declaration) => Some(declaration),
        _ => None,
    }
}
pub(super) fn parent_on_last_public_release(env: &Env<'_>, pointer: u32) -> Option<Id> {
    let device = declaration(env, pointer)?.device;
    let id = com(env).identity::<Payload>(&env.sys.objects, pointer)?;
    (com(env).references(&env.sys.objects, id)? == 1).then_some(device)
}
pub(super) fn parent_on_first_public_retain(env: &Env<'_>, pointer: u32) -> Option<Id> {
    let device = declaration(env, pointer)?.device;
    let id = com(env).identity::<Payload>(&env.sys.objects, pointer)?;
    (com(env).references(&env.sys.objects, id)? == 0).then_some(device)
}
fn end(element: &[u8; 8]) -> bool {
    element[..2] == [0xff, 0]
}
fn valid(element: &[u8; 8]) -> bool {
    let stream = u16::from_le_bytes([element[0], element[1]]);
    let offset = u16::from_le_bytes([element[2], element[3]]);
    // Stream, offset alignment, D3DDECLTYPE, D3DDECLMETHOD and D3DDECLUSAGE.
    stream < 16
        && offset.is_multiple_of(4)
        && element[4] <= 16
        && element[5] <= 6
        && element[6] <= 13
}
/// The fixed-function layout of a bound declaration, or why it has none.
pub(super) fn layout(env: &Env<'_>, id: Id) -> Result<fixed::Layout, &'static str> {
    let Some(Payload::Declaration(d)) = com(env).get_identity(&env.sys.objects, id) else {
        return Err("bound vertex declaration no longer exists");
    };
    fixed_layout(&d.elements)
}
fn fixed_layout(elements: &[[u8; 8]]) -> Result<fixed::Layout, &'static str> {
    if elements.iter().any(|e| !end(e) && e[6] == 9 && e[7] == 0) {
        let mut previous = None;
        for element in elements.iter().take_while(|e| !end(e)) {
            let offset = u16::from_le_bytes([element[2], element[3]]);
            if element[0] != 0 || element[1] != 0 || previous.is_some_and(|before| offset < before)
            {
                return Err("POSITIONT declaration requires ordered stream-zero inputs");
            }
            previous = Some(offset);
        }
    }
    let mut position = None;
    let mut color = None;
    let mut texcoords = [None; 8];
    let mut size = 0;
    let mut streams = [0; 2];
    for element in elements.iter().take_while(|e| !end(e)) {
        let stream = u16::from_le_bytes([element[0], element[1]]);
        if stream >= 16 {
            return Err("vertex declaration stream is outside the bound stream range");
        }
        if element[5] != 0 {
            return Err("vertex declaration uses a tessellation method");
        }
        let offset = u32::from(u16::from_le_bytes([element[2], element[3]]));
        // (usage, usage index, type): POSITION FLOAT3, POSITIONT FLOAT4,
        // COLOR0 D3DCOLOR and TEXCOORD0..7 FLOAT1..4 are FVF-equivalent inputs.
        let bytes = match (element[6], element[7], element[4]) {
            (0, 0, 2) if position.is_none() => {
                position = Some((offset, false));
                streams[0] = stream;
                12
            }
            (9, 0, 3) if position.is_none() => {
                position = Some((offset, true));
                streams[0] = stream;
                16
            }
            (10, 0, 4) if color.is_none() => {
                color = Some(offset);
                streams[1] = stream;
                4
            }
            (5, n @ 0..=7, kind @ 0..=3) if texcoords[n as usize].is_none() => {
                let dimensions = kind + 1;
                texcoords[n as usize] = Some((stream, offset, dimensions));
                u32::from(dimensions) * 4
            }
            _ => return Err("vertex declaration element has no fixed-function equivalent"),
        };
        size = size.max(offset + bytes);
    }
    let (position, transformed) =
        position.ok_or("vertex declaration has no POSITION or POSITIONT element")?;
    Ok(fixed::Layout {
        transformed,
        position,
        color,
        texcoords,
        streams,
        size,
    })
}
/// Short element list for draw diagnostics: stream:offset:type:usage.index.
pub(super) fn describe(env: &Env<'_>, id: Id) -> String {
    let Some(Payload::Declaration(d)) = com(env).get_identity(&env.sys.objects, id) else {
        return "missing".into();
    };
    let elements: Vec<_> = d
        .elements
        .iter()
        .take_while(|e| !end(e))
        .map(|e| {
            format!(
                "{}:{}:{}:{}.{}",
                u16::from_le_bytes([e[0], e[1]]),
                u16::from_le_bytes([e[2], e[3]]),
                e[4],
                e[6],
                e[7]
            )
        })
        .collect();
    format!("[{}]", elements.join(" "))
}
#[win32(d3d9,name="IDirect3DDevice9::CreateVertexDeclaration",level=implement)]
pub(super) fn create(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    input: GuestPtr<u8>,
    output: GuestPtr<u32>,
) -> u32 {
    if output.write(env.cpu, 0).is_err() {
        return INVALID;
    }
    let Some(device) = com(env)
        .identity::<Payload>(&env.sys.objects, this.addr())
        .filter(|_| super::device(env, this.addr()).is_some())
    else {
        return INVALID;
    };
    if input.is_null() {
        return INVALID;
    }
    let mut elements = Vec::new();
    loop {
        if elements.len() == MAX_ELEMENTS {
            return INVALID;
        }
        let mut element = [0; 8];
        let at = input.addr().wrapping_add(8 * elements.len() as u32);
        if env.cpu.read(at, &mut element).is_err() {
            return INVALID;
        }
        elements.push(element);
        if end(&element) {
            break;
        }
        if !valid(&element) {
            return INVALID;
        }
    }
    if com(env)
        .add_ref_identity(&mut env.sys.objects, device)
        .is_none()
    {
        return OUT_OF_MEMORY;
    }
    super::programmable::warmup::declaration(env, this.addr(), &elements);
    let payload = Declaration {
        pointer: 0,
        device,
        elements,
    };
    let Some((pointer, id)) = object(
        env,
        "IDirect3DVertexDeclaration9",
        IID_DECLARATION,
        Payload::Declaration(payload),
    ) else {
        let _ = com(env).release_identity(&mut env.sys.objects, device);
        return OUT_OF_MEMORY;
    };
    if let Some(Payload::Declaration(d)) = com(env).get_identity_mut(&mut env.sys.objects, id) {
        d.pointer = pointer;
    }
    if output.write(env.cpu, pointer).is_err() {
        release(env, pointer);
        return INVALID;
    }
    0
}
#[win32(d3d9,name="IDirect3DVertexDeclaration9::GetDevice",level=implement)]
fn declaration_device(env: &mut Env<'_>, this: GuestPtr<()>, output: GuestPtr<u32>) -> u32 {
    if output.write(env.cpu, 0).is_err() {
        return INVALID;
    }
    let Some(d) = declaration(env, this.addr()) else {
        return INVALID;
    };
    let Some(Payload::Device(device)) = com(env).get_identity(&env.sys.objects, d.device) else {
        return INVALID;
    };
    let pointer = device.pointer;
    if add_ref(env, pointer).is_none() {
        return OUT_OF_MEMORY;
    }
    if output.write(env.cpu, pointer).is_err() {
        release(env, pointer);
        return INVALID;
    }
    0
}
#[win32(d3d9,name="IDirect3DVertexDeclaration9::GetDeclaration",level=implement)]
fn declaration_elements(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    output: GuestPtr<u8>,
    count: GuestPtr<u32>,
) -> u32 {
    let Some(d) = declaration(env, this.addr()) else {
        return INVALID;
    };
    if count.is_null() {
        return INVALID;
    }
    // The count includes D3DDECL_END; a null element pointer queries it.
    let length = d.elements.len() as u32;
    let bytes = d.elements.concat();
    if !output.is_null() && env.cpu.write(output.addr(), &bytes).is_err() {
        return INVALID;
    }
    count.write(env.cpu, length).map_or(INVALID, |()| 0)
}
/// Bind a declaration (or none) with an internal reference. Either binding
/// replaces the FVF: GetFVF reports 0 until SetFVF selects one again.
pub(super) fn bind(env: &mut Env<'_>, device: Id, new: Option<Id>) -> u32 {
    let Some(Payload::Device(_)) = com(env).get_identity(&env.sys.objects, device) else {
        return INVALID;
    };
    if let Some(result) = super::shaders::stateblocks::record_declaration(env, device, new) {
        return result;
    }
    let Some(Payload::Device(d)) = com(env).get_identity(&env.sys.objects, device) else {
        return INVALID;
    };
    let old = d.buffers.declaration;
    if old == new && d.fixed.fvf == 0 {
        return 0;
    }
    if new != old && new.is_some_and(|id| !com(env).retain_internal(&mut env.sys.objects, id)) {
        return OUT_OF_MEMORY;
    }
    let Some(Payload::Device(d)) = com(env).get_identity_mut(&mut env.sys.objects, device) else {
        return INVALID;
    };
    d.versions[7] += 1;
    d.buffers.declaration = new;
    d.fixed.clear_fvf();
    if new != old
        && let Some(id) = old
        && let Some(Some(dead)) = com(env).release_internal(&mut env.sys.objects, id)
    {
        super::destroy(env, dead);
    }
    0
}
#[win32(d3d9,name="IDirect3DDevice9::SetVertexDeclaration",level=state)]
pub(super) fn set(env: &mut Env<'_>, this: GuestPtr<()>, pointer: GuestPtr<()>) -> u32 {
    let Some(device) = com(env)
        .identity::<Payload>(&env.sys.objects, this.addr())
        .filter(|_| super::device(env, this.addr()).is_some())
    else {
        return INVALID;
    };
    let new = if pointer.is_null() {
        None
    } else {
        if declaration(env, pointer.addr()).is_none_or(|d| d.device != device) {
            return INVALID;
        }
        com(env).identity::<Payload>(&env.sys.objects, pointer.addr())
    };
    bind(env, device, new)
}
/// Reports only declarations bound by SetVertexDeclaration; after SetFVF it
/// returns null where native D3D9 would return an implicit FVF declaration.
#[win32(d3d9,name="IDirect3DDevice9::GetVertexDeclaration",level=implement,leaf)]
pub(super) fn get(env: &mut Env<'_>, this: GuestPtr<()>, output: GuestPtr<u32>) -> u32 {
    if output.write(env.cpu, 0).is_err() {
        return INVALID;
    }
    let Some(d) = super::device(env, this.addr()) else {
        return INVALID;
    };
    let Some(id) = d.buffers.declaration else {
        return 0;
    };
    let Some(Payload::Declaration(declaration)) = com(env).get_identity(&env.sys.objects, id)
    else {
        return INVALID;
    };
    let pointer = declaration.pointer;
    if add_ref(env, pointer).is_none() {
        return OUT_OF_MEMORY;
    }
    if output.write(env.cpu, pointer).is_err() {
        release(env, pointer);
        return INVALID;
    }
    0
}
