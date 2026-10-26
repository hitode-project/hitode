//! State blocks retain precisely the object slots they capture. Recording
//! redirects setters into an isolated snapshot, leaving current device state
//! unchanged until Apply. Capture refreshes only the recorded subset. Apply
//! borrows an immutable snapshot across device updates without copying maps.
//! An unchanged captured subset retains its snapshot, including exact float bits.
use super::*;
mod slots;
use slots::Slots;
use std::rc::Rc;

const IID_BLOCK: Iid = [
    0x8e, 0x41, 0xe6, 0xb0, 0xce, 0xbb, 0x10, 0x4c, 0xab, 0x3b, 0xab, 0xa8, 0x00, 0xb5, 0xbb, 0xca,
];
#[derive(Clone, Default)]
struct ConstantMask {
    floats: Slots<usize, 4>,
    integers: Slots<usize, 1>,
    booleans: Slots<usize, 1>,
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
            ConstantKind::Float => self.floats.extend(range),
            ConstantKind::Integer => self.integers.extend(range),
            ConstantKind::Boolean => self.booleans.extend(range),
        }
    }
}
#[derive(Clone, Default)]
struct Mask {
    dirty: u16,
    vertex: bool,
    pixel: bool,
    vertex_constants: ConstantMask,
    pixel_constants: ConstantMask,
    fvf: bool,
    declaration: bool,
    streams: Slots<usize, 1>,
    indices: bool,
    texture: bool,
    textures: Slots<usize, 1>,
    render: Slots<i32, 4>,
    stage: Slots<i32, 1>,
    stage_slots: Slots<(usize, i32), 10>,
    sampler: Slots<i32, 1>,
    sampler_slots: Slots<(usize, i32), 10>,
    viewport: bool,
    scissor: bool,
    transforms: Slots<usize, 1>,
    clip_planes: Slots<usize, 1>,
}
#[cfg(feature = "d3d9-counters")]
impl Mask {
    fn items(&self) -> u64 {
        let constants = |m: &ConstantMask| m.floats.len() + m.integers.len() + m.booleans.len();
        ([
            self.vertex,
            self.pixel,
            self.fvf,
            self.declaration,
            self.indices,
            self.texture,
            self.viewport,
            self.scissor,
        ]
        .into_iter()
        .filter(|v| *v)
        .count()
            + self.streams.len()
            + self.textures.len()
            + self.render.len()
            + self.stage.len()
            + self.stage_slots.len()
            + self.sampler.len()
            + self.sampler_slots.len()
            + self.transforms.len()
            + self.clip_planes.len()
            + constants(&self.vertex_constants)
            + constants(&self.pixel_constants)) as u64
    }
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
    snapshot: Option<Rc<Snapshot>>,
    fingerprints: [u128; 15],
}
impl StateBlock {
    fn members(&self, d: &super::super::Device) -> Mask {
        let mut m = self.snapshot.as_ref().unwrap().mask.clone();
        let changed = |class| self.fingerprints[class] != d.fingerprints[class];
        m.dirty = (0..15).fold(0, |bits, c| bits | (u16::from(changed(c)) << c));
        if !changed(0) {
            m.render = Default::default();
        }
        if !changed(1) {
            m.stage = Default::default();
            m.stage_slots = Default::default();
        }
        if !changed(2) {
            m.sampler = Default::default();
            m.sampler_slots = Default::default();
        }
        m.viewport &= changed(3);
        m.scissor &= changed(4);
        if !changed(5) {
            m.transforms = Default::default();
        }
        if !changed(6) {
            m.clip_planes = Default::default();
        }
        m.fvf &= changed(7);
        m.declaration &= changed(7);
        m.vertex &= changed(8);
        m.pixel &= changed(9);
        if !changed(10) {
            m.streams = Default::default();
        }
        m.indices &= changed(11);
        m.texture &= changed(12);
        if !changed(12) {
            m.textures = Default::default();
        }
        if !changed(13) {
            m.vertex_constants = Default::default();
        }
        if !changed(14) {
            m.pixel_constants = Default::default();
        }
        m
    }
}
impl Snapshot {
    fn current(env: &Env<'_>, id: Id, mask: Mask) -> Option<Self> {
        let Payload::Device(d) = com(env).get_identity(&env.sys.objects, id)? else {
            return None;
        };
        Some(Self {
            fixed: fixed::Fixed {
                fvf: d.fixed.fvf,
                render: d.fixed.render.capture(mask.render.iter()),
                stage: d.fixed.stage.capture(mask.stage.iter()),
                stage_slots: std::array::from_fn(|slot| {
                    let keys: Slots<i32, 4> = mask
                        .stage_slots
                        .iter()
                        .filter(|&(n, _)| n == slot)
                        .map(|(_, i)| i)
                        .collect();
                    d.fixed.stage_slots[slot].capture(keys.iter())
                }),
                sampler: d.fixed.sampler.capture(mask.sampler.iter()),
                sampler_slots: std::array::from_fn(|slot| {
                    let keys: Slots<i32, 4> = mask
                        .sampler_slots
                        .iter()
                        .filter(|&(n, _)| n == slot)
                        .map(|(_, i)| i)
                        .collect();
                    d.fixed.sampler_slots[slot].capture(keys.iter())
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
        for stream in &self.mask.streams {
            if let Some(id) = self.buffers.streams[stream].buffer {
                ids.push(id);
            }
        }
        for slot in &self.mask.textures {
            if let Some(id) = self.shaders.textures[slot] {
                ids.push(id);
            }
        }
        ids
    }
}
impl Snapshot {
    pub(in crate::d3d9) fn binding_state(&self) -> (&Bindings, &buffers::Bindings, Option<Id>) {
        (&self.shaders, &self.buffers, self.texture)
    }
}
pub(in crate::d3d9) fn admits_leaf(env: &Env<'_>, pointer: u32, capture: bool) -> bool {
    let Some(b) = block(env, pointer) else {
        return true;
    };
    let Some(Payload::Device(d)) = com(env).get_identity(&env.sys.objects, b.device) else {
        return true;
    };
    if d.recording.is_some() || b.fingerprints == d.fingerprints {
        return true;
    }
    let mask = b.members(d);
    if mask.dirty == 0 {
        return true;
    }
    let s = b.snapshot.as_ref().unwrap();
    // Only captured binding members can release storage.
    let pairs = if capture {
        changes(
            &s.shaders,
            &s.buffers,
            s.texture,
            (&d.shaders, &d.buffers, d.texture),
            &mask,
        )
    } else {
        changes(
            &d.shaders,
            &d.buffers,
            d.texture,
            (&s.shaders, &s.buffers, s.texture),
            &mask,
        )
    };
    // Public ownership guarantees that none of these releases frees guest storage.
    pairs.iter().all(|(old, _)| {
        old.is_none_or(|id| {
            com(env)
                .references(&env.sys.objects, id)
                .is_some_and(|n| n > 0)
        })
    })
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
pub(in crate::d3d9) fn mark_fixed(env: &mut Env<'_>, pointer: u32, key: FixedKey, changed: bool) {
    let Some(Payload::Device(d)) = com(env).get_mut::<Payload>(&mut env.sys.objects, pointer)
    else {
        return;
    };
    let Some(r) = d.recording.as_mut() else {
        if !changed {
            return;
        }
        let class = match key {
            FixedKey::Fvf => 7,
            FixedKey::Render(_) => 0,
            FixedKey::Stage(_) | FixedKey::StageSlot(_, _) => 1,
            FixedKey::Sampler(_) | FixedKey::SamplerSlot(_, _) => 2,
            FixedKey::Viewport => 3,
            FixedKey::Scissor => 4,
            FixedKey::Transform(_) => 5,
            FixedKey::ClipPlane(_) => 6,
        };
        d.touch(class);
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
        adv_abi::Api {
            env_leaf: Some(super::super::leaf_calls::state_block),
            ..capture_ABI
        },
        adv_abi::Api {
            env_leaf: Some(super::super::leaf_calls::state_block),
            ..apply_ABI
        },
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
    next: (&Bindings, &buffers::Bindings, Option<Id>),
    mask: &Mask,
) -> Vec<(Option<Id>, Option<Id>)> {
    let (new_shaders, new_buffers, new_texture) = next;
    let mut pairs = Vec::new();
    for (captured, old, new) in [
        (mask.vertex, shaders.vertex, new_shaders.vertex),
        (mask.pixel, shaders.pixel, new_shaders.pixel),
        (
            mask.declaration,
            buffers.declaration,
            new_buffers.declaration,
        ),
        (mask.indices, buffers.indices, new_buffers.indices),
        (mask.texture, texture, new_texture),
    ] {
        if captured && old != new {
            pairs.push((old, new));
        }
    }
    for i in &mask.streams {
        let old = buffers.streams[i].buffer;
        let new = new_buffers.streams[i].buffer;
        if old != new {
            pairs.push((old, new));
        }
    }
    for i in &mask.textures {
        let old = shaders.textures[i];
        let new = new_shaders.textures[i];
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
    for id in block.snapshot.unwrap().identities() {
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
    let Some(Payload::Device(d)) = com.get_identity(&env.sys.objects, device) else {
        return false;
    };
    d.children.values().any(|&id| {
        matches!(
            com.get_identity(&env.sys.objects, id),
            Some(Payload::StateBlock(_))
        )
    })
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
        dirty: 0,
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
            Slots::default()
        },
        indices: all,
        texture: all,
        textures: if all {
            (1..20).collect()
        } else {
            Slots::default()
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
            Slots::default()
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
            Slots::default()
        },
        clip_planes: if vertex {
            (0..6).collect()
        } else {
            Slots::default()
        },
    })
}
fn make(
    env: &mut Env<'_>,
    device: Id,
    snapshot: Snapshot,
    output: GuestPtr<u32>,
    fingerprints: [u128; 15],
) -> u32 {
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
            snapshot: Some(Rc::new(snapshot)),
            fingerprints,
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
    let fingerprints = device(env, this.addr()).unwrap().fingerprints;
    make(env, id, snapshot, output, fingerprints)
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
    make(env, id, snapshot, output, [u128::MAX; 15])
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
    let old = b.snapshot.as_ref().unwrap();
    let Some(Payload::Device(d)) = com(env).get_identity(&env.sys.objects, id) else {
        return INVALID;
    };
    if d.recording.is_some() {
        return INVALID;
    }
    if b.fingerprints == d.fingerprints {
        return 0;
    }
    let mask = b.members(d);
    let fingerprints = d.fingerprints;
    if mask.dirty == 0 {
        let Some(Payload::StateBlock(b)) =
            com(env).get_mut::<Payload>(&mut env.sys.objects, this.addr())
        else {
            unreachable!()
        };
        b.fingerprints = fingerprints;
        return 0;
    }
    let changes = changes(
        &old.shaders,
        &old.buffers,
        old.texture,
        (&d.shaders, &d.buffers, d.texture),
        &mask,
    );
    if !retain_changes(env, &changes) {
        return OUT_OF_MEMORY;
    }
    let Some(Payload::StateBlock(b)) =
        com(env).get_mut::<Payload>(&mut env.sys.objects, this.addr())
    else {
        unreachable!();
    };
    let mut snapshot = b.snapshot.take().unwrap();
    let old = Rc::get_mut(&mut snapshot).expect("capture has exclusive snapshot ownership");
    let Some(Payload::Device(d)) = com(env).get_identity(&env.sys.objects, id) else {
        unreachable!();
    };
    transfer(
        (
            || &mut old.fixed,
            &mut old.shaders,
            &mut old.buffers,
            &mut old.texture,
        ),
        (&d.fixed, &d.shaders, &d.buffers, d.texture),
        &mask,
    );
    let Some(Payload::StateBlock(b)) =
        com(env).get_mut::<Payload>(&mut env.sys.objects, this.addr())
    else {
        unreachable!();
    };
    b.snapshot = Some(snapshot);
    b.fingerprints = fingerprints;
    release_changes(env, &changes);
    0
}
fn copy_constants(target: &mut Constants, source: &Constants, mask: &ConstantMask) {
    for i in &mask.floats {
        target.write_register(i, source.register_bits(i));
    }
    for i in &mask.integers {
        target.write_register(i + 256, source.register_bits(i + 256));
    }
    for i in &mask.booleans {
        target.write_register(i + 272, source.register_bits(i + 272));
    }
}
#[win32(d3d9,name="IDirect3DStateBlock9::Apply",level=state)]
pub(in crate::d3d9) fn apply(env: &mut Env<'_>, this: GuestPtr<()>) -> u32 {
    let Some(b) = block(env, this.addr()) else {
        return INVALID;
    };
    let id = b.device;
    let snapshot = b.snapshot.as_ref().unwrap();
    let fingerprints = b.fingerprints;
    let Some(Payload::Device(d)) = com(env).get_identity(&env.sys.objects, id) else {
        return INVALID;
    };
    if d.recording.is_some() {
        return INVALID;
    }
    if fingerprints == d.fingerprints {
        #[cfg(feature = "d3d9-counters")]
        count_apply(env, 0);
        return 0;
    }
    let mask = b.members(d);
    let fingerprints = d.fingerprints;
    if mask.dirty == 0 {
        let Some(Payload::StateBlock(b)) =
            com(env).get_mut::<Payload>(&mut env.sys.objects, this.addr())
        else {
            unreachable!()
        };
        b.fingerprints = fingerprints;
        #[cfg(feature = "d3d9-counters")]
        count_apply(env, 0);
        return 0;
    }
    let snapshot = Rc::clone(snapshot);
    let changes = changes(
        &d.shaders,
        &d.buffers,
        d.texture,
        (&snapshot.shaders, &snapshot.buffers, snapshot.texture),
        &mask,
    );
    if !retain_changes(env, &changes) {
        return OUT_OF_MEMORY;
    }
    let Some(Payload::Device(d)) = com(env).get_identity_mut(&mut env.sys.objects, id) else {
        return INVALID;
    };
    transfer(
        (
            || &mut *d.fixed,
            &mut d.shaders,
            &mut d.buffers,
            &mut d.texture,
        ),
        (
            &snapshot.fixed,
            &snapshot.shaders,
            &snapshot.buffers,
            snapshot.texture,
        ),
        &mask,
    );
    // Other blocks must observe the restored classes; this block now matches.
    for (class, v) in d.versions.iter_mut().enumerate() {
        if mask.dirty & (1 << class) != 0 {
            *v = v.checked_add(1).expect("state version exhausted");
        }
    }
    d.refresh(u32::from(mask.dirty));
    let fingerprints = d.fingerprints;
    let Some(Payload::StateBlock(b)) =
        com(env).get_mut::<Payload>(&mut env.sys.objects, this.addr())
    else {
        unreachable!();
    };
    b.fingerprints = fingerprints;
    #[cfg(feature = "d3d9-counters")]
    count_apply(env, mask.items());
    release_changes(env, &changes);
    0
}
#[cfg(feature = "d3d9-counters")]
fn count_apply(env: &mut Env<'_>, items: u64) {
    let count = &mut env.ext.get_mut::<State>().frames;
    count.stateblock_applies += 1;
    count.stateblock_items += items;
}
fn transfer<'a>(
    target: (
        impl FnOnce() -> &'a mut fixed::Fixed,
        &mut Bindings,
        &mut buffers::Bindings,
        &mut Option<Id>,
    ),
    source: (&fixed::Fixed, &Bindings, &buffers::Bindings, Option<Id>),
    mask: &Mask,
) {
    let (fixed, shaders, buffers, texture) = target;
    let (source_fixed, source_shaders, source_buffers, source_texture) = source;
    if mask.vertex {
        shaders.vertex = source_shaders.vertex;
    }
    if mask.pixel {
        shaders.pixel = source_shaders.pixel;
    }
    copy_constants(
        &mut shaders.vertex_constants,
        &source_shaders.vertex_constants,
        &mask.vertex_constants,
    );
    copy_constants(
        &mut shaders.pixel_constants,
        &source_shaders.pixel_constants,
        &mask.pixel_constants,
    );
    if mask.declaration {
        buffers.declaration = source_buffers.declaration;
    }
    for i in &mask.streams {
        buffers.streams[i] = source_buffers.streams[i];
    }
    if mask.indices {
        buffers.indices = source_buffers.indices;
    }
    if mask.texture {
        *texture = source_texture;
    }
    for slot in &mask.textures {
        shaders.textures[slot] = source_shaders.textures[slot];
    }
    if mask.fvf
        || mask.viewport
        || mask.scissor
        || !mask.render.is_empty()
        || !mask.stage.is_empty()
        || !mask.stage_slots.is_empty()
        || !mask.sampler.is_empty()
        || !mask.sampler_slots.is_empty()
        || !mask.transforms.is_empty()
        || !mask.clip_planes.is_empty()
    {
        let fixed = fixed();
        if mask.fvf {
            fixed.fvf = source_fixed.fvf;
        }
        for i in &mask.render {
            fixed.render.insert(i, source_fixed.render[&i]);
        }
        for i in &mask.stage {
            fixed.stage.insert(i, source_fixed.stage[&i]);
        }
        for (slot, i) in &mask.stage_slots {
            fixed.stage_slots[slot].insert(i, source_fixed.stage_slots[slot][&i]);
        }
        for i in &mask.sampler {
            fixed.sampler.insert(i, source_fixed.sampler[&i]);
        }
        for (slot, i) in &mask.sampler_slots {
            fixed.sampler_slots[slot].insert(i, source_fixed.sampler_slots[slot][&i]);
        }
        if mask.viewport {
            fixed.viewport = source_fixed.viewport;
        }
        if mask.scissor {
            fixed.scissor = source_fixed.scissor;
        }
        for i in &mask.transforms {
            fixed.transforms[i] = source_fixed.transforms[i];
        }
        for i in &mask.clip_planes {
            fixed.clip_planes[i] = source_fixed.clip_planes[i];
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "d3d9-counters")]
    #[test]
    // spec: GFX-REC-8, GFX-SHADER-4
    fn d3d9_differential_stateblock_work_counts_class_members_and_unchanged_reapplication() {
        let mut m = crate::d3d9::tests::machine();
        m.sys.objects = Objects::default();
        m.ext.get_mut::<State>().com = Com::new(&mut m.sys.objects).unwrap();
        let mut env = Env {
            context: m.sys.current,
            cpu: &mut m.cpu,
            sys: &mut m.sys,
            ext: &mut m.ext,
            thunk: 0,
        };
        let (pointer, id, _, _) = crate::d3d9::tests::setup(&mut env);
        let output = GuestPtr::new(alloc(&mut env, 4).unwrap());
        assert_eq!(create(&mut env, GuestPtr::new(pointer), 1, output), 0);
        let this = GuestPtr::new(output.read(env.cpu).unwrap());
        let render_items = block(&env, this.addr())
            .unwrap()
            .snapshot
            .as_ref()
            .unwrap()
            .mask
            .render
            .len() as u64;
        assert_eq!(capture(&mut env, this), 0);
        let Some(Payload::Device(d)) = com(&env).get_identity_mut(&mut env.sys.objects, id) else {
            panic!()
        };
        let fixed = &mut *d.fixed;
        let z_enable = 1 - fixed.render[&7];
        fixed.render.insert(7, z_enable);
        d.touch(0);
        assert_eq!(apply(&mut env, this), 0);
        assert_eq!(apply(&mut env, this), 0);
        let work = env.ext.get::<State>().frames;
        assert_eq!(work.stateblock_applies, 2);
        assert_eq!(work.stateblock_items, render_items);
    }

    #[test]
    // spec: GFX-SHADER-4
    fn unchanged_subset_refreshes_fingerprints_without_replacing_capture_storage() {
        let mut m = crate::d3d9::tests::machine();
        let mut env = Env {
            context: m.sys.current,
            cpu: &mut m.cpu,
            sys: &mut m.sys,
            ext: &mut m.ext,
            thunk: 0,
        };
        let (pointer, _, _, _) = crate::d3d9::tests::setup(&mut env);
        let this = GuestPtr::new(pointer);
        let output = GuestPtr::new(alloc(&mut env, 4).unwrap());
        assert_eq!(begin(&mut env, this), 0);
        assert_eq!(fixed::set_render(&mut env, this, 22, 3), 0);
        assert_eq!(end(&mut env, this, output), 0);
        let this_block = GuestPtr::new(output.read(env.cpu).unwrap());
        assert_eq!(capture(&mut env, this_block), 0);
        let saved = Rc::as_ptr(
            block(&env, this_block.addr())
                .unwrap()
                .snapshot
                .as_ref()
                .unwrap(),
        );
        assert_eq!(fixed::set_render(&mut env, this, 14, 0), 0);
        assert_eq!(capture(&mut env, this_block), 0);
        let b = block(&env, this_block.addr()).unwrap();
        assert_eq!(Rc::as_ptr(b.snapshot.as_ref().unwrap()), saved);
        assert_eq!(b.fingerprints, device(&env, pointer).unwrap().fingerprints);
        assert_eq!(fixed::set_render(&mut env, this, 14, 1), 0);
        assert_eq!(apply(&mut env, this_block), 0);
        assert_eq!(
            block(&env, this_block.addr()).unwrap().fingerprints,
            device(&env, pointer).unwrap().fingerprints
        );
        assert_eq!(fixed::set_render(&mut env, this, 22, 1), 0);
        assert_eq!(apply(&mut env, this_block), 0);
        assert_eq!(device(&env, pointer).unwrap().fixed.render[&22], 3);
        assert_eq!(device(&env, pointer).unwrap().fixed.render[&14], 1);
    }

    #[test]
    // spec: GFX-REC-11, GFX-SHADER-4
    fn d3d9_reuse_stateblock_members_preserve_capture_apply_and_membership() {
        let mut m = crate::d3d9::tests::machine();
        let mut env = Env {
            context: m.sys.current,
            cpu: &mut m.cpu,
            sys: &mut m.sys,
            ext: &mut m.ext,
            thunk: 0,
        };
        let (pointer, _, _, _) = crate::d3d9::tests::setup(&mut env);
        let this = GuestPtr::new(pointer);
        let output = GuestPtr::new(alloc(&mut env, 4).unwrap());
        assert_eq!(begin(&mut env, this), 0);
        assert_eq!(fixed::set_render(&mut env, this, 22, 3), 0);
        assert_eq!(end(&mut env, this, output), 0);
        let block_pointer = output.read(env.cpu).unwrap();
        let block_this = GuestPtr::new(block_pointer);
        assert_eq!(capture(&mut env, block_this), 0);
        for _ in 0..3 {
            assert_eq!(fixed::set_render(&mut env, this, 14, 0), 0);
            assert!(admits_leaf(&env, block_pointer, false));
            assert_eq!(apply(&mut env, block_this), 0);
            assert_eq!(device(&env, pointer).unwrap().fixed.render[&14], 0);
            assert_eq!(fixed::set_render(&mut env, this, 22, 1), 0);
            assert!(admits_leaf(&env, block_pointer, false));
            assert_eq!(apply(&mut env, block_this), 0);
            assert_eq!(device(&env, pointer).unwrap().fixed.render[&22], 3);
            assert!(admits_leaf(&env, block_pointer, true));
            assert_eq!(capture(&mut env, block_this), 0);
        }
        assert_eq!(fixed::set_render(&mut env, this, 22, 2), 0);
        assert!(admits_leaf(&env, block_pointer, true));
        assert_eq!(capture(&mut env, block_this), 0);
        assert_eq!(fixed::set_render(&mut env, this, 22, 1), 0);
        assert!(admits_leaf(&env, block_pointer, false));
        assert_eq!(apply(&mut env, block_this), 0);
        assert_eq!(device(&env, pointer).unwrap().fixed.render[&22], 2);
    }

    fn fingerprint(d: &super::super::super::Device) -> u128 {
        super::super::super::recording::hash((
            super::super::super::recording::fixed_key(&d.fixed),
            d.shaders
                .vertex_constants
                .floats
                .map(|r| r.map(f32::to_bits)),
            d.shaders.vertex_constants.integers,
            d.shaders.vertex_constants.booleans,
            d.shaders
                .pixel_constants
                .floats
                .map(|r| r.map(f32::to_bits)),
            d.shaders.pixel_constants.integers,
            d.shaders.pixel_constants.booleans,
        ))
    }

    #[test]
    // spec: GFX-SHADER-4
    fn partial_and_complete_members_match_full_apply_and_capture_reuses_storage() {
        let mut m = super::super::super::tests::machine();
        m.sys.objects = Objects::default();
        m.ext.get_mut::<State>().com = Com::new(&mut m.sys.objects).unwrap();
        let mut env = Env {
            context: m.sys.current,
            cpu: &mut m.cpu,
            sys: &mut m.sys,
            ext: &mut m.ext,
            thunk: 0,
        };
        let (pointer, id, _, _) = super::super::super::tests::setup(&mut env);
        let output = GuestPtr::new(alloc(&mut env, 4).unwrap());
        assert_eq!(create(&mut env, GuestPtr::new(pointer), 1, output), 0);
        let this = GuestPtr::new(output.read(env.cpu).unwrap());
        assert_eq!(capture(&mut env, this), 0);
        let saved = Rc::as_ptr(block(&env, this.addr()).unwrap().snapshot.as_ref().unwrap());
        let Some(Payload::Device(d)) = com(&env).get_identity_mut(&mut env.sys.objects, id) else {
            panic!()
        };
        let fixed = &mut *d.fixed;
        let z_enable = 1 - fixed.render[&7];
        fixed.render.insert(7, z_enable);
        d.touch(0);
        let b = block(&env, this.addr()).unwrap();
        let d = device(&env, pointer).unwrap();
        let members = b.members(d);
        assert!(members.render.contains(&7));
        assert_eq!(members.dirty, 1);
        assert_eq!(capture(&mut env, this), 0);
        assert_eq!(
            Rc::as_ptr(block(&env, this.addr()).unwrap().snapshot.as_ref().unwrap()),
            saved
        );
        let expected = fingerprint(device(&env, pointer).unwrap());
        let Some(Payload::Device(d)) = com(&env).get_identity_mut(&mut env.sys.objects, id) else {
            panic!()
        };
        let fixed = &mut *d.fixed;
        for value in fixed.render.values_mut() {
            *value ^= 1;
        }
        for map in std::iter::once(&mut fixed.stage)
            .chain(&mut fixed.stage_slots)
            .chain(std::iter::once(&mut fixed.sampler))
            .chain(fixed.sampler_slots.iter_mut().skip(1))
        {
            for value in map.values_mut() {
                *value ^= 1;
            }
        }
        fixed.fvf ^= 4;
        fixed.viewport.MinZ = -0.;
        fixed.scissor.left = 1;
        fixed.transforms.fill([2.; 16]);
        fixed.clip_planes.fill([3.; 4]);
        for (constants, limit) in [
            (&mut d.shaders.vertex_constants, 256),
            (&mut d.shaders.pixel_constants, 224),
        ] {
            constants.floats[..limit].fill([f32::from_bits(0x7fc0_1234); 4]);
            constants.integers.fill([-7; 4]);
            constants.booleans.fill(-1);
        }
        d.refresh_all();
        assert_eq!(apply(&mut env, this), 0);
        assert_eq!(fingerprint(device(&env, pointer).unwrap()), expected);
        let versions = device(&env, pointer).unwrap().versions;
        assert_eq!(apply(&mut env, this), 0);
        assert_eq!(device(&env, pointer).unwrap().versions, versions);
        // Mutate after Capture, then compare the optimized path with full transfer.
        let Some(Payload::Device(d)) = com(&env).get_identity_mut(&mut env.sys.objects, id) else {
            panic!()
        };
        d.shaders.vertex_constants.floats[3][0] = -0.;
        d.touch(13);
        let snapshot = Rc::clone(block(&env, this.addr()).unwrap().snapshot.as_ref().unwrap());
        let Some(Payload::Device(d)) = com(&env).get_identity_mut(&mut env.sys.objects, id) else {
            panic!()
        };
        transfer(
            (
                || &mut *d.fixed,
                &mut d.shaders,
                &mut d.buffers,
                &mut d.texture,
            ),
            (
                &snapshot.fixed,
                &snapshot.shaders,
                &snapshot.buffers,
                snapshot.texture,
            ),
            &snapshot.mask,
        );
        assert_eq!(fingerprint(d), expected);
        drop(snapshot);
        assert_eq!(apply(&mut env, this), 0);
        assert_eq!(fingerprint(device(&env, pointer).unwrap()), expected);
    }
}
