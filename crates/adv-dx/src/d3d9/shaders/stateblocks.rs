//! State blocks retain precisely the object slots they capture. Recording
//! redirects setters into an isolated snapshot, leaving current device state
//! unchanged until Apply. Capture refreshes only the recorded subset. Apply
//! borrows an immutable snapshot across device updates without copying maps.
//! An unchanged captured subset retains its snapshot, including exact float bits.
use super::*;
use std::collections::BTreeSet;

const IID_BLOCK: Iid = [
    0x8e, 0x41, 0xe6, 0xb0, 0xce, 0xbb, 0x10, 0x4c, 0xab, 0x3b, 0xab, 0xa8, 0x00, 0xb5, 0xbb, 0xca,
];
#[derive(Clone, Default)]
struct ConstantMask {
    floats: BTreeSet<usize>,
    integers: BTreeSet<usize>,
    booleans: BTreeSet<usize>,
}
impl ConstantMask {
    fn all(stage: Stage) -> Self {
        Self {
            floats: (0..constant_limit(stage, ConstantKind::Float) as usize).collect(),
            integers: (0..16).collect(),
            booleans: (0..16).collect(),
        }
    }
    fn mark(&mut self, kind: ConstantKind, range: std::ops::Range<usize>) {
        match kind {
            ConstantKind::Float => &mut self.floats,
            ConstantKind::Integer => &mut self.integers,
            ConstantKind::Boolean => &mut self.booleans,
        }
        .extend(range);
    }
}
#[derive(Clone, Default)]
struct Mask {
    vertex: bool,
    pixel: bool,
    vertex_constants: ConstantMask,
    pixel_constants: ConstantMask,
    fvf: bool,
    declaration: bool,
    streams: BTreeSet<usize>,
    indices: bool,
    texture: bool,
    textures: BTreeSet<usize>,
    render: BTreeSet<i32>,
    stage: BTreeSet<i32>,
    stage_slots: BTreeSet<(usize, i32)>,
    sampler: BTreeSet<i32>,
    sampler_slots: BTreeSet<(usize, i32)>,
    viewport: bool,
    scissor: bool,
    transforms: BTreeSet<usize>,
    clip_planes: BTreeSet<usize>,
}
pub(in crate::d3d9) struct Snapshot {
    pub(in crate::d3d9) fixed: fixed::Fixed,
    pub(in crate::d3d9) shaders: Bindings,
    buffers: buffers::Bindings,
    texture: Option<Id>,
    mask: Mask,
}
pub(in crate::d3d9) struct StateBlock {
    pointer: u32,
    pub(in crate::d3d9) device: Id,
    snapshot: Arc<Snapshot>,
}
impl Snapshot {
    fn current(env: &Env<'_>, id: Id, mask: Mask) -> Option<Self> {
        let Payload::Device(d) = com(env).get_identity(&env.sys.objects, id)? else {
            return None;
        };
        Some(Self {
            fixed: fixed::Fixed {
                fvf: d.fixed.fvf,
                render: mask
                    .render
                    .iter()
                    .map(|&i| (i, d.fixed.render[&i]))
                    .collect(),
                stage: mask.stage.iter().map(|&i| (i, d.fixed.stage[&i])).collect(),
                stage_slots: std::array::from_fn(|slot| {
                    mask.stage_slots
                        .range((slot, i32::MIN)..=(slot, i32::MAX))
                        .map(|&(_, i)| (i, d.fixed.stage_slots[slot][&i]))
                        .collect()
                }),
                sampler: mask
                    .sampler
                    .iter()
                    .map(|&i| (i, d.fixed.sampler[&i]))
                    .collect(),
                sampler_slots: std::array::from_fn(|slot| {
                    mask.sampler_slots
                        .range((slot, i32::MIN)..=(slot, i32::MAX))
                        .map(|&(_, i)| (i, d.fixed.sampler_slots[slot][&i]))
                        .collect()
                }),
                viewport: d.fixed.viewport,
                scissor: d.fixed.scissor,
                transforms: d.fixed.transforms,
                clip_planes: d.fixed.clip_planes,
            },
            shaders: d.shaders.clone(),
            buffers: d.buffers.clone(),
            texture: d.texture,
            mask,
        })
    }
    fn matches(&self, d: &super::super::Device) -> bool {
        let m = &self.mask;
        let a = &self.fixed;
        let b = &d.fixed;
        let constants = |a: &Constants, b: &Constants, m: &ConstantMask| {
            m.floats
                .iter()
                .all(|&i| a.floats[i].map(f32::to_bits) == b.floats[i].map(f32::to_bits))
                && m.integers.iter().all(|&i| a.integers[i] == b.integers[i])
                && m.booleans.iter().all(|&i| a.booleans[i] == b.booleans[i])
        };
        (!m.vertex || self.shaders.vertex == d.shaders.vertex)
            && (!m.pixel || self.shaders.pixel == d.shaders.pixel)
            && (!m.fvf || a.fvf == b.fvf)
            && (!m.declaration || self.buffers.declaration == d.buffers.declaration)
            && (!m.indices || self.buffers.indices == d.buffers.indices)
            && (!m.texture || self.texture == d.texture)
            && m.streams.iter().all(|&i| {
                let a = self.buffers.streams[i];
                let b = d.buffers.streams[i];
                (a.buffer, a.offset, a.stride) == (b.buffer, b.offset, b.stride)
            })
            && m.textures
                .iter()
                .all(|&i| self.shaders.textures[i] == d.shaders.textures[i])
            && m.render.iter().all(|i| a.render.get(i) == b.render.get(i))
            && m.stage.iter().all(|i| a.stage.get(i) == b.stage.get(i))
            && m.stage_slots
                .iter()
                .all(|&(slot, i)| a.stage_slots[slot].get(&i) == b.stage_slots[slot].get(&i))
            && m.sampler
                .iter()
                .all(|i| a.sampler.get(i) == b.sampler.get(i))
            && m.sampler_slots
                .iter()
                .all(|&(slot, i)| a.sampler_slots[slot].get(&i) == b.sampler_slots[slot].get(&i))
            && (!m.viewport
                || (
                    a.viewport.X,
                    a.viewport.Y,
                    a.viewport.Width,
                    a.viewport.Height,
                    a.viewport.MinZ.to_bits(),
                    a.viewport.MaxZ.to_bits(),
                ) == (
                    b.viewport.X,
                    b.viewport.Y,
                    b.viewport.Width,
                    b.viewport.Height,
                    b.viewport.MinZ.to_bits(),
                    b.viewport.MaxZ.to_bits(),
                ))
            && (!m.scissor
                || (
                    a.scissor.left,
                    a.scissor.top,
                    a.scissor.right,
                    a.scissor.bottom,
                ) == (
                    b.scissor.left,
                    b.scissor.top,
                    b.scissor.right,
                    b.scissor.bottom,
                ))
            && m.transforms
                .iter()
                .all(|&i| a.transforms[i].map(f32::to_bits) == b.transforms[i].map(f32::to_bits))
            && m.clip_planes
                .iter()
                .all(|&i| a.clip_planes[i].map(f32::to_bits) == b.clip_planes[i].map(f32::to_bits))
            && constants(
                &self.shaders.vertex_constants,
                &d.shaders.vertex_constants,
                &m.vertex_constants,
            )
            && constants(
                &self.shaders.pixel_constants,
                &d.shaders.pixel_constants,
                &m.pixel_constants,
            )
    }
    pub(super) fn mark_constants(
        &mut self,
        stage: Stage,
        kind: ConstantKind,
        range: std::ops::Range<usize>,
    ) {
        match stage {
            Stage::Vertex => &mut self.mask.vertex_constants,
            Stage::Pixel => &mut self.mask.pixel_constants,
        }
        .mark(kind, range);
    }
    fn identities(&self) -> Vec<Id> {
        let mut ids = Vec::new();
        for (captured, id) in [
            (self.mask.vertex, self.shaders.vertex),
            (self.mask.pixel, self.shaders.pixel),
            (self.mask.declaration, self.buffers.declaration),
            (self.mask.indices, self.buffers.indices),
            (self.mask.texture, self.texture),
        ] {
            if captured && let Some(id) = id {
                ids.push(id);
            }
        }
        for &stream in &self.mask.streams {
            if let Some(id) = self.buffers.streams[stream].buffer {
                ids.push(id);
            }
        }
        for &slot in &self.mask.textures {
            if let Some(id) = self.shaders.textures[slot] {
                ids.push(id);
            }
        }
        ids
    }
}
pub(in crate::d3d9) enum FixedKey {
    Fvf,
    Render(i32),
    Stage(i32),
    StageSlot(usize, i32),
    Sampler(i32),
    SamplerSlot(usize, i32),
    Viewport,
    Scissor,
    Transform(usize),
    ClipPlane(usize),
}
pub(in crate::d3d9) fn mark_fixed(env: &mut Env<'_>, pointer: u32, key: FixedKey) {
    let Some(Payload::Device(d)) = com(env).get_mut::<Payload>(&mut env.sys.objects, pointer)
    else {
        return;
    };
    let Some(r) = d.recording.as_mut() else {
        return;
    };
    match key {
        FixedKey::Fvf => r.mask.fvf = true,
        FixedKey::Render(i) => {
            r.mask.render.insert(i);
        }
        FixedKey::Stage(i) => {
            r.mask.stage.insert(i);
        }
        FixedKey::StageSlot(slot, i) => {
            r.mask.stage_slots.insert((slot, i));
        }
        FixedKey::Sampler(i) => {
            r.mask.sampler.insert(i);
        }
        FixedKey::SamplerSlot(slot, i) => {
            r.mask.sampler_slots.insert((slot, i));
        }
        FixedKey::Viewport => r.mask.viewport = true,
        FixedKey::Scissor => r.mask.scissor = true,
        FixedKey::Transform(i) => {
            r.mask.transforms.insert(i);
        }
        FixedKey::ClipPlane(i) => {
            r.mask.clip_planes.insert(i);
        }
    }
}
pub(in crate::d3d9) fn register(registry: &mut Registry) {
    for api in [
        block_query_ABI,
        block_add_ref_ABI,
        block_release_ABI,
        block_device_ABI,
        capture_ABI,
        apply_ABI,
        create_ABI,
        begin_ABI,
        end_ABI,
    ] {
        registry.register(api);
    }
}
unknown!(
    block_query,
    block_add_ref,
    block_release,
    "IDirect3DStateBlock9::QueryInterface",
    "IDirect3DStateBlock9::AddRef",
    "IDirect3DStateBlock9::Release"
);
fn block<'a>(env: &'a Env<'_>, pointer: u32) -> Option<&'a StateBlock> {
    match com(env).get::<Payload>(&env.sys.objects, pointer)? {
        Payload::StateBlock(b) => Some(b),
        _ => None,
    }
}
fn retain_snapshot(env: &mut Env<'_>, snapshot: &Snapshot) -> bool {
    let mut retained = Vec::new();
    for id in snapshot.identities() {
        if !com(env).retain_internal(&mut env.sys.objects, id) {
            for id in retained {
                release_binding(env, Some(id));
            }
            return false;
        }
        retained.push(id);
    }
    true
}
fn changes(
    shaders: &Bindings,
    buffers: &buffers::Bindings,
    texture: Option<Id>,
    new: &Snapshot,
) -> Vec<(Option<Id>, Option<Id>)> {
    let mut pairs = Vec::new();
    for (captured, old, new) in [
        (new.mask.vertex, shaders.vertex, new.shaders.vertex),
        (new.mask.pixel, shaders.pixel, new.shaders.pixel),
        (
            new.mask.declaration,
            buffers.declaration,
            new.buffers.declaration,
        ),
        (new.mask.indices, buffers.indices, new.buffers.indices),
        (new.mask.texture, texture, new.texture),
    ] {
        if captured && old != new {
            pairs.push((old, new));
        }
    }
    for &i in &new.mask.streams {
        let old = buffers.streams[i].buffer;
        let new = new.buffers.streams[i].buffer;
        if old != new {
            pairs.push((old, new));
        }
    }
    for &i in &new.mask.textures {
        let old = shaders.textures[i];
        let new = new.shaders.textures[i];
        if old != new {
            pairs.push((old, new));
        }
    }
    pairs
}
fn retain_changes(env: &mut Env<'_>, changes: &[(Option<Id>, Option<Id>)]) -> bool {
    let mut retained = Vec::new();
    for &(_, new) in changes {
        if let Some(id) = new {
            if !com(env).retain_internal(&mut env.sys.objects, id) {
                for id in retained {
                    release_binding(env, Some(id));
                }
                return false;
            }
            retained.push(id);
        }
    }
    true
}
fn release_changes(env: &mut Env<'_>, changes: &[(Option<Id>, Option<Id>)]) {
    for &(old, _) in changes {
        release_binding(env, old);
    }
}
pub(in crate::d3d9) fn release_snapshot(env: &mut Env<'_>, snapshot: Snapshot) {
    for id in snapshot.identities() {
        release_binding(env, Some(id));
    }
}
pub(in crate::d3d9) fn destroy(env: &mut Env<'_>, block: StateBlock) {
    for id in block.snapshot.identities() {
        release_binding(env, Some(id));
    }
    if let Some(released) = com(env).release_identity(&mut env.sys.objects, block.device)
        && let Some(dead) = released.destroyed
    {
        super::super::destroy(env, dead);
    }
}
pub(in crate::d3d9) fn has_outstanding(env: &Env<'_>, device: Id) -> bool {
    let com = com(env);
    env.sys.objects.hosts_in::<adv_os::com::ComObject<Payload>>(com.namespace()).any(|(id,_)|matches!(com.get_identity(&env.sys.objects,id),Some(Payload::StateBlock(b)) if b.device==device))
}
fn mask(env: &Env<'_>, device: Id, kind: i32) -> Option<Mask> {
    let Payload::Device(d) = com(env).get_identity(&env.sys.objects, device)? else {
        return None;
    };
    let all = kind == 1;
    let vertex = all || kind == 3;
    let pixel = all || kind == 2;
    if !all && !vertex && !pixel {
        return None;
    }
    // SDK vertex-state membership among the render states this frontend stores.
    let vertex_render = [
        9, 22, 28, 29, 34, 35, 36, 37, 38, 48, 136, 137, 139, 140, 141, 142, 143, 145, 146, 147,
        148, 151, 152, 154, 155, 156, 157, 158, 159, 160, 161, 162, 163, 166, 167, 170, 172, 173,
        178, 179, 180, 181, 182, 183, 184,
    ];
    Some(Mask {
        vertex,
        pixel,
        vertex_constants: if vertex {
            ConstantMask::all(Stage::Vertex)
        } else {
            ConstantMask::default()
        },
        pixel_constants: if pixel {
            ConstantMask::all(Stage::Pixel)
        } else {
            ConstantMask::default()
        },
        fvf: vertex,
        declaration: vertex,
        streams: if all {
            (0..16).collect()
        } else {
            BTreeSet::new()
        },
        indices: all,
        texture: all,
        textures: if all {
            (1..20).collect()
        } else {
            BTreeSet::new()
        },
        render: d
            .fixed
            .render
            .keys()
            .copied()
            .filter(|i| {
                all || (vertex && vertex_render.contains(i))
                    || (pixel && (matches!(*i, 9 | 36 | 37 | 38) || !vertex_render.contains(i)))
            })
            .collect(),
        stage: d
            .fixed
            .stage
            .keys()
            .copied()
            .filter(|i| all || (vertex && matches!(i, 11 | 24)) || (pixel && !matches!(i, 11 | 24)))
            .collect(),
        stage_slots: d
            .fixed
            .stage_slots
            .iter()
            .enumerate()
            .flat_map(|(i, map)| {
                map.keys()
                    .copied()
                    .filter(move |key| {
                        all || (vertex && matches!(key, 11 | 24))
                            || (pixel && !matches!(key, 11 | 24))
                    })
                    .map(move |key| (i, key))
            })
            .collect(),
        sampler: if pixel {
            d.fixed.sampler.keys().copied().collect()
        } else {
            BTreeSet::new()
        },
        sampler_slots: d
            .fixed
            .sampler_slots
            .iter()
            .enumerate()
            .skip(1)
            .filter(|(i, _)| all || (pixel && *i < 16) || (vertex && *i >= 16))
            .flat_map(|(i, map)| map.keys().map(move |&key| (i, key)))
            .collect(),
        viewport: all,
        scissor: all,
        transforms: if all {
            (0..3).collect()
        } else {
            BTreeSet::new()
        },
        clip_planes: if vertex {
            (0..6).collect()
        } else {
            BTreeSet::new()
        },
    })
}
fn make(env: &mut Env<'_>, device: Id, snapshot: Snapshot, output: GuestPtr<u32>) -> u32 {
    if com(env)
        .add_ref_identity(&mut env.sys.objects, device)
        .is_none()
    {
        release_snapshot(env, snapshot);
        return OUT_OF_MEMORY;
    }
    let retained = snapshot.identities();
    let Some((pointer, id)) = object(
        env,
        "IDirect3DStateBlock9",
        IID_BLOCK,
        Payload::StateBlock(Box::new(StateBlock {
            pointer: 0,
            device,
            snapshot: Arc::new(snapshot),
        })),
    ) else {
        for id in retained {
            release_binding(env, Some(id));
        }
        let _ = com(env).release_identity(&mut env.sys.objects, device);
        return OUT_OF_MEMORY;
    };
    if let Some(Payload::StateBlock(b)) = com(env).get_identity_mut(&mut env.sys.objects, id) {
        b.pointer = pointer;
    }
    if output.write(env.cpu, pointer).is_err() {
        release(env, pointer);
        return INVALID;
    }
    0
}
#[win32(d3d9,name="IDirect3DDevice9::CreateStateBlock",level=implement)]
pub(in crate::d3d9) fn create(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    kind: i32,
    output: GuestPtr<u32>,
) -> u32 {
    if output.write(env.cpu, 0).is_err() {
        return INVALID;
    }
    let Some(id) = device_id(env, this.addr()) else {
        return INVALID;
    };
    if device(env, this.addr()).unwrap().recording.is_some() {
        return INVALID;
    }
    let Some(mask) = mask(env, id, kind) else {
        return INVALID;
    };
    let snapshot = Snapshot::current(env, id, mask).unwrap();
    if !retain_snapshot(env, &snapshot) {
        return OUT_OF_MEMORY;
    }
    make(env, id, snapshot, output)
}
#[win32(d3d9,name="IDirect3DDevice9::BeginStateBlock",level=state)]
pub(in crate::d3d9) fn begin(env: &mut Env<'_>, this: GuestPtr<()>) -> u32 {
    let Some(id) = device_id(env, this.addr()) else {
        return INVALID;
    };
    if device(env, this.addr()).unwrap().recording.is_some() {
        return INVALID;
    }
    let snapshot = Snapshot::current(env, id, Mask::default()).unwrap();
    let Some(Payload::Device(d)) = com(env).get_identity_mut(&mut env.sys.objects, id) else {
        return INVALID;
    };
    d.recording = Some(snapshot);
    0
}
#[win32(d3d9,name="IDirect3DDevice9::EndStateBlock",level=implement)]
pub(in crate::d3d9) fn end(env: &mut Env<'_>, this: GuestPtr<()>, output: GuestPtr<u32>) -> u32 {
    if output.write(env.cpu, 0).is_err() {
        return INVALID;
    }
    let Some(id) = device_id(env, this.addr()) else {
        return INVALID;
    };
    let Some(Payload::Device(d)) = com(env).get_identity_mut(&mut env.sys.objects, id) else {
        return INVALID;
    };
    let Some(snapshot) = d.recording.take() else {
        return INVALID;
    };
    make(env, id, snapshot, output)
}
#[win32(d3d9,name="IDirect3DStateBlock9::GetDevice",level=implement)]
fn block_device(env: &mut Env<'_>, this: GuestPtr<()>, output: GuestPtr<u32>) -> u32 {
    if output.write(env.cpu, 0).is_err() {
        return INVALID;
    }
    let Some(b) = block(env, this.addr()) else {
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
#[win32(d3d9,name="IDirect3DStateBlock9::Capture",level=state)]
pub(in crate::d3d9) fn capture(env: &mut Env<'_>, this: GuestPtr<()>) -> u32 {
    let Some(b) = block(env, this.addr()) else {
        return INVALID;
    };
    let id = b.device;
    let old = Arc::clone(&b.snapshot);
    let Some(Payload::Device(d)) = com(env).get_identity(&env.sys.objects, id) else {
        return INVALID;
    };
    if d.recording.is_some() {
        return INVALID;
    }
    if old.matches(d) {
        return 0;
    }
    let snapshot = Snapshot::current(env, id, old.mask.clone()).unwrap();
    let changes = changes(&old.shaders, &old.buffers, old.texture, &snapshot);
    if !retain_changes(env, &changes) {
        return OUT_OF_MEMORY;
    }
    let Some(Payload::StateBlock(b)) =
        com(env).get_mut::<Payload>(&mut env.sys.objects, this.addr())
    else {
        return INVALID;
    };
    b.snapshot = Arc::new(snapshot);
    release_changes(env, &changes);
    0
}
fn copy_constants(target: &mut Constants, source: &Constants, mask: &ConstantMask) {
    for &i in &mask.floats {
        target.floats[i] = source.floats[i];
    }
    for &i in &mask.integers {
        target.integers[i] = source.integers[i];
    }
    for &i in &mask.booleans {
        target.booleans[i] = source.booleans[i];
    }
}
#[win32(d3d9,name="IDirect3DStateBlock9::Apply",level=state)]
pub(in crate::d3d9) fn apply(env: &mut Env<'_>, this: GuestPtr<()>) -> u32 {
    let Some(b) = block(env, this.addr()) else {
        return INVALID;
    };
    let id = b.device;
    let snapshot = Arc::clone(&b.snapshot);
    let mask = &snapshot.mask;
    let Some(Payload::Device(d)) = com(env).get_identity(&env.sys.objects, id) else {
        return INVALID;
    };
    if d.recording.is_some() {
        return INVALID;
    }
    let changes = changes(&d.shaders, &d.buffers, d.texture, &snapshot);
    if !retain_changes(env, &changes) {
        return OUT_OF_MEMORY;
    }
    let Some(Payload::Device(d)) = com(env).get_identity_mut(&mut env.sys.objects, id) else {
        return INVALID;
    };
    if mask.vertex {
        d.shaders.vertex = snapshot.shaders.vertex;
    }
    if mask.pixel {
        d.shaders.pixel = snapshot.shaders.pixel;
    }
    copy_constants(
        &mut d.shaders.vertex_constants,
        &snapshot.shaders.vertex_constants,
        &mask.vertex_constants,
    );
    copy_constants(
        &mut d.shaders.pixel_constants,
        &snapshot.shaders.pixel_constants,
        &mask.pixel_constants,
    );
    if mask.fvf {
        d.fixed.fvf = snapshot.fixed.fvf;
    }
    if mask.declaration {
        d.buffers.declaration = snapshot.buffers.declaration;
    }
    for &i in &mask.streams {
        d.buffers.streams[i] = snapshot.buffers.streams[i];
    }
    if mask.indices {
        d.buffers.indices = snapshot.buffers.indices;
    }
    if mask.texture {
        d.texture = snapshot.texture;
    }
    for &slot in &mask.textures {
        d.shaders.textures[slot] = snapshot.shaders.textures[slot];
    }
    for &i in &mask.render {
        d.fixed.render.insert(i, snapshot.fixed.render[&i]);
    }
    for &i in &mask.stage {
        d.fixed.stage.insert(i, snapshot.fixed.stage[&i]);
    }
    for &(slot, i) in &mask.stage_slots {
        d.fixed.stage_slots[slot].insert(i, snapshot.fixed.stage_slots[slot][&i]);
    }
    for &i in &mask.sampler {
        d.fixed.sampler.insert(i, snapshot.fixed.sampler[&i]);
    }
    for &(slot, i) in &mask.sampler_slots {
        d.fixed.sampler_slots[slot].insert(i, snapshot.fixed.sampler_slots[slot][&i]);
    }
    if mask.viewport {
        d.fixed.viewport = snapshot.fixed.viewport;
    }
    if mask.scissor {
        d.fixed.scissor = snapshot.fixed.scissor;
    }
    for &i in &mask.transforms {
        d.fixed.transforms[i] = snapshot.fixed.transforms[i];
    }
    for &i in &mask.clip_planes {
        d.fixed.clip_planes[i] = snapshot.fixed.clip_planes[i];
    }
    release_changes(env, &changes);
    0
}
#[derive(Clone, Copy)]
enum Slot {
    Vertex,
    Pixel,
    Declaration,
    Indices,
    Texture,
    TextureAt(usize),
    Stream(usize),
}
fn slot(snapshot: &Snapshot, slot: Slot) -> (bool, Option<Id>) {
    match slot {
        Slot::Vertex => (snapshot.mask.vertex, snapshot.shaders.vertex),
        Slot::Pixel => (snapshot.mask.pixel, snapshot.shaders.pixel),
        Slot::Declaration => (snapshot.mask.declaration, snapshot.buffers.declaration),
        Slot::Indices => (snapshot.mask.indices, snapshot.buffers.indices),
        Slot::Texture => (snapshot.mask.texture, snapshot.texture),
        Slot::TextureAt(i) => (
            snapshot.mask.textures.contains(&i),
            snapshot.shaders.textures[i],
        ),
        Slot::Stream(i) => (
            snapshot.mask.streams.contains(&i),
            snapshot.buffers.streams[i].buffer,
        ),
    }
}
fn record(env: &mut Env<'_>, device: Id, key: Slot, new: Option<Id>) -> Option<u32> {
    let Payload::Device(d) = com(env).get_identity(&env.sys.objects, device)? else {
        return None;
    };
    let (captured, old) = slot(d.recording.as_ref()?, key);
    let old = if captured { old } else { None };
    if new != old && new.is_some_and(|id| !com(env).retain_internal(&mut env.sys.objects, id)) {
        return Some(OUT_OF_MEMORY);
    }
    let Some(Payload::Device(d)) = com(env).get_identity_mut(&mut env.sys.objects, device) else {
        return Some(INVALID);
    };
    let r = d.recording.as_mut().unwrap();
    match key {
        Slot::Vertex => {
            r.mask.vertex = true;
            r.shaders.vertex = new;
        }
        Slot::Pixel => {
            r.mask.pixel = true;
            r.shaders.pixel = new;
        }
        Slot::Declaration => {
            r.mask.declaration = true;
            r.mask.fvf = true;
            r.buffers.declaration = new;
            r.fixed.fvf = 0;
        }
        Slot::Indices => {
            r.mask.indices = true;
            r.buffers.indices = new;
        }
        Slot::Texture => {
            r.mask.texture = true;
            r.texture = new;
        }
        Slot::TextureAt(i) => {
            r.mask.textures.insert(i);
            r.shaders.textures[i] = new;
        }
        Slot::Stream(i) => {
            r.mask.streams.insert(i);
            r.buffers.streams[i].buffer = new;
        }
    }
    if new != old {
        release_binding(env, old);
    }
    Some(0)
}
pub(in crate::d3d9) fn record_shader(
    env: &mut Env<'_>,
    device: Id,
    stage: Stage,
    new: Option<Id>,
) -> Option<u32> {
    record(
        env,
        device,
        match stage {
            Stage::Vertex => Slot::Vertex,
            Stage::Pixel => Slot::Pixel,
        },
        new,
    )
}
pub(in crate::d3d9) fn record_declaration(
    env: &mut Env<'_>,
    device: Id,
    new: Option<Id>,
) -> Option<u32> {
    record(env, device, Slot::Declaration, new)
}
pub(in crate::d3d9) fn record_indices(
    env: &mut Env<'_>,
    device: Id,
    new: Option<Id>,
) -> Option<u32> {
    record(env, device, Slot::Indices, new)
}
pub(in crate::d3d9) fn record_texture(
    env: &mut Env<'_>,
    device: Id,
    new: Option<Id>,
) -> Option<u32> {
    record(env, device, Slot::Texture, new)
}
pub(in crate::d3d9) fn record_texture_slot(
    env: &mut Env<'_>,
    device: Id,
    index: usize,
    new: Option<Id>,
) -> Option<u32> {
    if index == 0 {
        return record_texture(env, device, new);
    }
    if index >= 20 {
        return Some(INVALID);
    }
    record(env, device, Slot::TextureAt(index), new)
}
pub(in crate::d3d9) fn record_stream(
    env: &mut Env<'_>,
    device: Id,
    index: u32,
    stream: buffers::Stream,
) -> Option<u32> {
    if index >= 16 {
        return Some(INVALID);
    }
    let result = record(env, device, Slot::Stream(index as usize), stream.buffer)?;
    if result == 0
        && let Some(Payload::Device(d)) = com(env).get_identity_mut(&mut env.sys.objects, device)
    {
        d.recording.as_mut().unwrap().buffers.streams[index as usize] = stream;
    }
    Some(result)
}
