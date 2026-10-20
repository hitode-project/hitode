//! Native precompiled D3DX Effect objects over the existing D3D9 COM device.
//! Shader and state changes go through the device's guest-visible vtable:
//! entries that still hold the published thunk run on the host directly, and
//! replaced (hooked) entries get a guest call (GFX-EFFECT-3). Saving Begins
//! record one block per technique, as native D3DX does.
//! Every observed `D3DXCreateEffect` input was a precompiled FXO, so Effects run
//! natively and HLSL source compilation fails explicitly (GFX-EFFECT-2). Source
//! input would need a compiler, such as Wine's d3dcompiler built as a guest DLL.
use super::*;
use adv_abi::{AnsiStr, guest::check_range};
use adv_cpu::backend::{GuestExt, Prot};
use adv_machine::Outcome;
use adv_os::{
    Heaps,
    com::{Com, Iid},
};
use std::collections::{BTreeMap, VecDeque};
#[path = "effect_methods.rs"]
mod methods;
const INVALID: u32 = 0x8876086c;
const UNAVAILABLE: u32 = 0x8876086a;
const OOM: u32 = 0x8007000e;
const IID_EFFECT: Iid = [
    0xb3, 0xb4, 0xce, 0xf6, 0x4c, 0x4e, 0xdd, 0x40, 0xb8, 0x83, 0x8d, 0x8d, 0xe5, 0xea, 0x0c, 0xd5,
];
const IID_BASE: Iid = [
    0xac, 0x18, 0x7c, 0x01, 0x3f, 0x10, 0x17, 0x44, 0x8c, 0x51, 0x6b, 0xf6, 0xef, 0x1e, 0x56, 0xbe,
];
struct Store {
    com: Com,
    table: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Handle {
    Parameter(usize),
    Technique(usize),
    Pass(usize, usize),
}
struct Object {
    binary: binary::Effect,
    device: u32,
    device_retained: bool,
    allocations: Vec<u32>,
    handles: BTreeMap<u32, Handle>,
    param_handles: Vec<u32>,
    technique_handles: Vec<u32>,
    pass_handles: Vec<Vec<u32>>,
    names: BTreeMap<String, u32>,
    object_data: Vec<u32>,
    shaders: Vec<u32>,
    current: usize,
    started: bool,
    active_pass: Option<usize>,
    /// The block End applies: the current technique's recorded block.
    saved_state: u32,
    /// One recorded block per technique, holding the states its passes set
    /// (GFX-EFFECT-3); 0 until the first saving Begin of that technique.
    technique_states: Vec<u32>,
    /// Parsed constant tables and preshaders by shader object.
    shader_tables: BTreeMap<usize, std::sync::Arc<ShaderTable>>,
    begin_flags: u32,
}
struct ShaderTable {
    /// Each constant with the parameter it reads, if the Effect has one.
    constants: Vec<(Constant, Option<usize>)>,
    preshaders: Vec<Vec<u32>>,
}
pub(super) fn register(registry: &mut Registry) {
    registry.register(create_ABI);
    registry.register(output_semantics_ABI);
    methods::register(registry);
}
fn alloc(env: &mut Env<'_>, bytes: &[u8]) -> Option<u32> {
    let heaps = env.ext.get_mut::<Heaps>();
    let pointer = heaps.alloc(
        env.cpu,
        &mut env.sys.vm,
        heaps.process_heap,
        bytes.len().max(4) as u32,
        true,
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
fn put(env: &mut Env<'_>, pointer: u32, bytes: &[u8]) -> bool {
    check_range(env.cpu, pointer, bytes.len() as u32, Prot::WRITE).is_ok()
        && env.cpu.write(pointer, bytes).is_ok()
}
fn words(env: &mut Env<'_>, pointer: u32, values: &[u32]) -> bool {
    put(env, pointer, bytemuck::cast_slice(values))
}
fn string(env: &Env<'_>, pointer: u32) -> Option<String> {
    AnsiStr(GuestPtr::new(pointer))
        .read(env.cpu, 4096)
        .ok()
        .map(|b| String::from_utf8_lossy(&b).into_owned())
}
fn ensure_store(env: &mut Env<'_>) -> Option<()> {
    if env.ext.try_get::<Store>().is_some() {
        return Some(());
    }
    let com = Com::new(&mut env.sys.objects)?;
    let table = env.sys.vm.alloc(env.cpu, 79 * 4, Prot::RW).ok()?;
    for signature in adv_abi::signature::com_signatures()
        .iter()
        .filter(|s| s.module == "ID3DXEffect")
    {
        let method = format!("ID3DXEffect::{}", signature.name);
        let target = env
            .sys
            .thunks
            .publish(env.cpu, "d3dx9_43.dll", &method)
            .ok()?;
        env.cpu
            .write_u32(table + u32::from(signature.com_slot?) * 4, target)
            .ok()?;
    }
    env.ext.insert(Store { com, table });
    Some(())
}
fn com(env: &Env<'_>) -> Option<Com> {
    Some(env.ext.try_get::<Store>()?.com)
}
fn object<'a>(env: &'a Env<'_>, pointer: u32) -> Option<&'a Object> {
    com(env)?.get::<Object>(&env.sys.objects, pointer)
}
fn object_mut<'a>(env: &'a mut Env<'_>, pointer: u32) -> Option<&'a mut Object> {
    com(env)?.get_mut::<Object>(&mut env.sys.objects, pointer)
}
fn handle(env: &Env<'_>, this: u32, value: u32) -> Option<Handle> {
    let o = object(env, this)?;
    o.handles.get(&value).copied().or_else(|| {
        let name = string(env, value)?;
        o.binary
            .parameters
            .iter()
            .position(|p| p.full_name == name)
            .map(Handle::Parameter)
            .or_else(|| {
                o.binary
                    .techniques
                    .iter()
                    .position(|t| t.name == name)
                    .map(Handle::Technique)
            })
    })
}
fn param(env: &Env<'_>, this: u32, value: u32) -> Option<usize> {
    if let Handle::Parameter(i) = handle(env, this, value)? {
        Some(i)
    } else {
        None
    }
}
fn target(env: &Env<'_>, pointer: u32, slot: u32) -> Option<u32> {
    let table = GuestPtr::<u32>::new(pointer).read(env.cpu).ok()?;
    GuestPtr::<u32>::new(table.checked_add(slot * 4)?)
        .read(env.cpu)
        .ok()
}
struct Pending {
    pointer: u32,
    slot: u32,
    args: Vec<u32>,
    /// Constant registers; staged in guest memory only for a guest call.
    data: Option<Vec<u32>>,
}
struct Calls {
    pending: VecDeque<Pending>,
    temporary: Vec<u32>,
    /// Recording a technique's block ignores failed state calls, as native
    /// D3DX does; the block calls themselves still stop the chain.
    tolerant: bool,
}
impl Calls {
    fn new() -> Self {
        Self {
            pending: VecDeque::new(),
            temporary: Vec::new(),
            tolerant: false,
        }
    }
    fn push(&mut self, pointer: u32, slot: u32, args: impl Into<Vec<u32>>) {
        self.pending.push_back(Pending {
            pointer,
            slot,
            args: args.into(),
            data: None,
        });
    }
    fn push_data(&mut self, pointer: u32, slot: u32, index: u32, data: Vec<u32>, count: u32) {
        self.pending.push_back(Pending {
            pointer,
            slot,
            args: vec![index, 0, count],
            data: Some(data),
        });
    }
    fn finish(self, env: &mut Env<'_>) {
        for pointer in self.temporary {
            free(env, pointer);
        }
    }
}
type Then = Box<dyn FnOnce(&mut Env<'_>) -> Outcome>;
#[cfg(feature = "gpu")]
type Value = Box<dyn FnOnce(&mut Env<'_>, u32) -> Outcome>;
fn calls(env: &mut Env<'_>, c: Calls, result: u32) -> Outcome {
    run(env, c, Box::new(move |_| Outcome::Return(result)))
}
/// Make the queued COM calls in order, then `then`. Unhooked device methods
/// run on the host without a guest round trip (GFX-EFFECT-3).
#[cfg_attr(not(feature = "gpu"), allow(clippy::never_loop))]
fn run(env: &mut Env<'_>, mut c: Calls, then: Then) -> Outcome {
    // No guest code runs between host calls, so each vtable is read once.
    let mut tables: Vec<(u32, Vec<u32>)> = Vec::new();
    loop {
        let Some(call) = c.pending.pop_front() else {
            c.finish(env);
            return then(env);
        };
        let Some(entry) = cached_target(env, &mut tables, call.pointer, call.slot) else {
            c.finish(env);
            return Outcome::Return(INVALID);
        };
        let slot = call.slot;
        #[cfg(feature = "gpu")]
        if let Some(outcome) = crate::d3d9::direct::effect_call(
            env,
            call.pointer,
            slot,
            entry,
            &call.args,
            call.data.as_deref(),
        ) {
            match outcome {
                Outcome::Return(value) if !stops(&c, slot, value) => continue,
                Outcome::Return(value) => {
                    c.finish(env);
                    return Outcome::Return(value);
                }
                other => {
                    return resume(
                        other,
                        Box::new(move |env, value| step(env, c, slot, value, then)),
                    );
                }
            }
        }
        let mut args = call.args;
        if let Some(data) = call.data {
            let Some(pointer) = alloc(env, bytemuck::cast_slice(&data)) else {
                c.finish(env);
                return Outcome::Return(OOM);
            };
            c.temporary.push(pointer);
            args[1] = pointer;
        }
        args.insert(0, call.pointer);
        return Outcome::call(entry, args, move |env, resume| {
            step(env, c, slot, resume.value(), then)
        });
    }
}
fn cached_target(
    env: &Env<'_>,
    tables: &mut Vec<(u32, Vec<u32>)>,
    pointer: u32,
    slot: u32,
) -> Option<u32> {
    if slot >= 128 {
        return target(env, pointer, slot);
    }
    if let Some((_, table)) = tables.iter().find(|(p, _)| *p == pointer) {
        return table
            .get(slot as usize)
            .copied()
            .or_else(|| target(env, pointer, slot));
    }
    let base = GuestPtr::<u32>::new(pointer).read(env.cpu).ok()?;
    // Read up to the highest slot used so far; a short table falls back.
    let mut words = vec![0u32; 128];
    if base.checked_add(128 * 4).is_none()
        || env
            .cpu
            .read(base, bytemuck::cast_slice_mut(&mut words))
            .is_err()
    {
        return target(env, pointer, slot);
    }
    let entry = words[slot as usize];
    tables.push((pointer, words));
    Some(entry)
}
fn stops(c: &Calls, slot: u32, value: u32) -> bool {
    slot > 2 && value & 0x80000000 != 0 && !(c.tolerant && slot != 60 && slot != 61)
}
fn step(env: &mut Env<'_>, c: Calls, slot: u32, value: u32, then: Then) -> Outcome {
    if stops(&c, slot, value) {
        c.finish(env);
        return Outcome::Return(value);
    }
    run(env, c, then)
}
/// Continue with the returned EAX once a host method that waited or called
/// into the guest finishes.
#[cfg(feature = "gpu")]
fn resume(outcome: Outcome, k: Value) -> Outcome {
    match outcome {
        Outcome::Return(_) => unreachable!("returns are handled by the caller"),
        Outcome::Block {
            wait,
            then,
            allow_time_skip,
        } => Outcome::Block {
            wait,
            allow_time_skip,
            then: Box::new(move |env, r| match then(env, r) {
                Outcome::Return(value) => k(env, value),
                other => resume(other, k),
            }),
        },
        Outcome::Call {
            target,
            args,
            then,
            stack,
            unwindable,
        } => Outcome::Call {
            target,
            args,
            stack,
            unwindable,
            then: Box::new(move |env, r| match then(env, r) {
                Outcome::Return(value) => k(env, value),
                other => resume(other, k),
            }),
        },
        Outcome::Defer { then } => Outcome::Defer {
            then: Box::new(move |env| match then(env) {
                Outcome::Return(value) => k(env, value),
                other => resume(other, k),
            }),
        },
        Outcome::HostRequest { then } => Outcome::HostRequest {
            then: Box::new(move |env| match then(env) {
                Outcome::Return(value) => k(env, value),
                other => resume(other, k),
            }),
        },
        other => other,
    }
}
fn fail(env: &mut Env<'_>, reason: &str) -> Outcome {
    adv_abi::warn_unavailable(env, "d3dx9_43.dll!ID3DXEffect", reason);
    Outcome::Return(UNAVAILABLE)
}

#[win32(d3dx9_43,name="D3DXCreateEffect",level=implement)]
#[allow(
    clippy::too_many_arguments,
    reason = "The pinned SDK ABI has nine arguments, plus the host environment."
)]
fn create(
    env: &mut Env<'_>,
    device: GuestPtr<()>,
    source: GuestPtr<u8>,
    length: u32,
    defines: GuestPtr<()>,
    include: GuestPtr<()>,
    flags: u32,
    pool: GuestPtr<()>,
    output: GuestPtr<u32>,
    errors: GuestPtr<u32>,
) -> Outcome {
    if (!output.is_null() && output.write(env.cpu, 0).is_err())
        || (!errors.is_null() && errors.write(env.cpu, 0).is_err())
    {
        return Outcome::Return(INVALID);
    }
    if device.is_null() || source.is_null() {
        return Outcome::Return(INVALID);
    }
    if length == 0 {
        return Outcome::Return(0x80004005);
    }
    if output.is_null() {
        return Outcome::Return(0);
    }
    if !pool.is_null() || !defines.is_null() || !include.is_null() {
        return fail(env, "Effect pools and source preprocessing are unavailable");
    }
    if length as usize > binary::MAX_BYTES
        || check_range(env.cpu, source.addr(), length, Prot::READ).is_err()
    {
        return Outcome::Return(INVALID);
    }
    let mut bytes = vec![0; length as usize];
    if env.cpu.read(source.addr(), &mut bytes).is_err() {
        return Outcome::Return(INVALID);
    }
    let binary = match binary::Effect::parse(&bytes) {
        Ok(b) => b,
        Err(binary::Error::Invalid(_)) => return Outcome::Return(0x88760b59),
        Err(binary::Error::Unsupported(reason)) => return fail(env, reason),
    };
    if ensure_store(env).is_none() {
        return Outcome::Return(OOM);
    }
    let mut o = Object {
        shaders: vec![0; binary.objects.len()],
        object_data: vec![0; binary.objects.len()],
        binary,
        device: device.addr(),
        device_retained: false,
        allocations: Vec::new(),
        handles: BTreeMap::new(),
        param_handles: Vec::new(),
        technique_handles: Vec::new(),
        pass_handles: Vec::new(),
        names: BTreeMap::new(),
        current: 0,
        started: false,
        active_pass: None,
        saved_state: 0,
        technique_states: Vec::new(),
        shader_tables: BTreeMap::new(),
        begin_flags: flags,
    };
    let result = (|| {
        let mut named = Vec::new();
        for p in &o.binary.parameters {
            named.push(p.name.clone());
            named.push(p.semantic.clone());
        }
        for t in &o.binary.techniques {
            named.push(t.name.clone());
            for p in &t.passes {
                named.push(p.name.clone());
            }
        }
        for name in named {
            if name.is_empty() || o.names.contains_key(&name) {
                continue;
            }
            let mut bytes = name.as_bytes().to_vec();
            bytes.push(0);
            let p = alloc(env, &bytes)?;
            o.allocations.push(p);
            o.names.insert(name, p);
        }
        for i in 0..o.binary.parameters.len() {
            let p = alloc(env, &[0; 4])?;
            o.allocations.push(p);
            o.handles.insert(p, Handle::Parameter(i));
            o.param_handles.push(p);
        }
        for (i, t) in o.binary.techniques.iter().enumerate() {
            let p = alloc(env, &[0; 4])?;
            o.allocations.push(p);
            o.handles.insert(p, Handle::Technique(i));
            o.technique_handles.push(p);
            let mut passes = Vec::new();
            for j in 0..t.passes.len() {
                let p = alloc(env, &[0; 4])?;
                o.allocations.push(p);
                o.handles.insert(p, Handle::Pass(i, j));
                passes.push(p);
            }
            o.pass_handles.push(passes);
        }
        for (i, data) in o.binary.objects.iter().enumerate() {
            if data.is_empty() {
                continue;
            }
            let p = alloc(env, data)?;
            o.allocations.push(p);
            o.object_data[i] = p;
        }
        for parameter in &mut o.binary.parameters {
            if parameter.kind == 4 && parameter.class == 4 {
                let value = parameter
                    .object
                    .and_then(|id| o.object_data.get(id))
                    .copied()
                    .unwrap_or(0);
                parameter.bytes = value.to_le_bytes().to_vec();
            }
        }
        Some(())
    })();
    if result.is_none() {
        for p in o.allocations {
            free(env, p);
        }
        return Outcome::Return(OOM);
    }
    let table = env.ext.get::<Store>().table;
    let Some(pointer) = alloc(env, &table.to_le_bytes()) else {
        for p in o.allocations {
            free(env, p);
        }
        return Outcome::Return(OOM);
    };
    let allocations = o.allocations.clone();
    let Some(id) = com(env)
        .unwrap()
        .create(&mut env.sys.objects, pointer, IID_EFFECT, o)
    else {
        for allocation in allocations {
            free(env, allocation);
        }
        free(env, pointer);
        return Outcome::Return(OOM);
    };
    let _ = id;
    let Some(addref) = target(env, device.addr(), 1) else {
        return release(env, pointer, INVALID);
    };
    Outcome::call(addref, [device.addr()], move |env, _| {
        object_mut(env, pointer).unwrap().device_retained = true;
        create_shaders(env, pointer, 0, output.addr())
    })
}
fn create_shaders(env: &mut Env<'_>, this: u32, start: usize, output: u32) -> Outcome {
    let Some(o) = object(env, this) else {
        return Outcome::Return(INVALID);
    };
    let next = (start..o.binary.objects.len()).find_map(|id| {
        let kind = o
            .binary
            .parameters
            .iter()
            .find(|p| p.object == Some(id) && matches!(p.kind, 15 | 16))?
            .kind;
        (!o.binary.objects[id].is_empty()).then_some((id, kind))
    });
    let Some((id, kind)) = next else {
        if GuestPtr::new(output).write(env.cpu, this).is_err() {
            return release(env, this, INVALID);
        }
        return Outcome::Return(0);
    };
    let device = o.device;
    let data = o.object_data[id];
    let Some(scratch) = alloc(env, &[0; 4]) else {
        return release(env, this, OOM);
    };
    let Some(target) = target(env, device, if kind == 16 { 91 } else { 106 }) else {
        free(env, scratch);
        return release(env, this, INVALID);
    };
    Outcome::call(target, [device, data, scratch], move |env, resume| {
        let shader = env.cpu.read_u32(scratch).unwrap_or(0);
        free(env, scratch);
        if resume.value() & 0x80000000 != 0 || shader == 0 {
            return release(
                env,
                this,
                if resume.value() == 0 {
                    0x80004005
                } else {
                    resume.value()
                },
            );
        }
        let Some(o) = object_mut(env, this) else {
            return Outcome::Return(INVALID);
        };
        o.shaders[id] = shader;
        create_shaders(env, this, id + 1, output)
    })
}
fn release(env: &mut Env<'_>, this: u32, result: u32) -> Outcome {
    let Some(com) = com(env) else {
        return Outcome::Return(0);
    };
    let Some(released) = com.release::<Object>(&mut env.sys.objects, this) else {
        return Outcome::Return(0);
    };
    if let Some(dead) = released.destroyed {
        let mut c = Calls::new();
        for shader in dead.value.shaders.into_iter().filter(|p| *p != 0) {
            c.push(shader, 2, []);
        }
        for p in &dead.value.binary.parameters {
            if p.elements == 0 && (5..=9).contains(&p.kind) && p.bytes.len() == 4 {
                let texture = u32::from_le_bytes(p.bytes[..4].try_into().unwrap());
                if texture != 0 {
                    c.push(texture, 2, []);
                }
            }
        }
        for block in dead.value.technique_states.into_iter().filter(|p| *p != 0) {
            c.push(block, 2, []);
        }
        if dead.value.device_retained {
            c.push(dead.value.device, 2, []);
        }
        for pointer in dead.value.allocations.into_iter().chain(dead.interfaces) {
            free(env, pointer);
        }
        calls(env, c, result)
    } else {
        Outcome::Return(if result == 0 {
            released.references
        } else {
            result
        })
    }
}

fn dispatch(env: &mut Env<'_>, this: u32, slot: u32, args: &[u32]) -> Outcome {
    if object(env, this).is_none() {
        return Outcome::Return(INVALID);
    }
    let a = |i: usize| args.get(i).copied().unwrap_or(0);
    if slot == 0 {
        if !words(env, a(1), &[0]) {
            return Outcome::Return(INVALID);
        }
        let Ok(iid) = GuestPtr::<Iid>::new(a(0)).read(env.cpu) else {
            return Outcome::Return(INVALID);
        };
        if iid != IID_EFFECT && iid != IID_BASE && iid != adv_os::com::IUNKNOWN {
            return Outcome::Return(0x80004002);
        }
        let count = com(env)
            .unwrap()
            .add_ref::<Object>(&mut env.sys.objects, this);
        if count.is_none() {
            return Outcome::Return(OOM);
        }
        if !words(env, a(1), &[this]) {
            return release(env, this, INVALID);
        }
        return Outcome::Return(0);
    }
    if slot == 1 {
        return Outcome::Return(
            com(env)
                .unwrap()
                .add_ref::<Object>(&mut env.sys.objects, this)
                .unwrap_or(0),
        );
    }
    if slot == 2 {
        return release(env, this, 0);
    }
    if matches!(slot, 3..=6) {
        return descriptor(env, this, slot, args);
    }
    if (8..=19).contains(&slot) {
        return get_handle(env, this, slot, args);
    }
    if matches!(slot, 20..=49 | 78) {
        return numeric(env, this, slot, args);
    }
    if slot == 50 {
        let Some(id) = param(env, this, a(0)) else {
            return Outcome::Return(INVALID);
        };
        let Some(value) = string(env, a(1)) else {
            return Outcome::Return(INVALID);
        };
        if object(env, this).unwrap().binary.parameters[id].kind != 4 {
            return Outcome::Return(INVALID);
        }
        let mut data = value.as_bytes().to_vec();
        data.push(0);
        let Some(pointer) = alloc(env, &data) else {
            return Outcome::Return(OOM);
        };
        let o = object_mut(env, this).unwrap();
        o.allocations.push(pointer);
        o.binary.parameters[id].bytes = pointer.to_le_bytes().to_vec();
        return Outcome::Return(0);
    }
    if slot == 51 {
        return get_object_value(env, this, args, slot);
    }
    if slot == 52 {
        return set_texture(env, this, args);
    }
    if (53..=55).contains(&slot) {
        return get_object_value(env, this, args, slot);
    }
    match slot {
        56 => {
            let Some(i) = param(env, this, a(0)) else {
                return Outcome::Return(INVALID);
            };
            let p = &object(env, this).unwrap().binary.parameters[i];
            if a(1) > a(2) || a(2) > p.elements {
                return Outcome::Return(INVALID);
            }
            fail(env, "Effect partial array uploads are unavailable")
        }
        57 | 72 => Outcome::Return(if words(env, a(0), &[0]) { 0 } else { INVALID }),
        58 => {
            let Some(Handle::Technique(i)) = handle(env, this, a(0)) else {
                return Outcome::Return(INVALID);
            };
            let o = object_mut(env, this).unwrap();
            if o.started {
                return Outcome::Return(INVALID);
            }
            o.current = i;
            Outcome::Return(0)
        }
        59 => Outcome::Return(
            object(env, this)
                .unwrap()
                .technique_handles
                .get(object(env, this).unwrap().current)
                .copied()
                .unwrap_or(0),
        ),
        60 => {
            let Some(Handle::Technique(i)) = handle(env, this, a(0)) else {
                return Outcome::Return(INVALID);
            };
            match validate_technique(env, this, i) {
                Ok(()) => Outcome::Return(0),
                Err(reason) => fail(env, reason),
            }
        }
        61 => {
            let o = object(env, this).unwrap();
            let next = if a(0) == 0 {
                Some(0)
            } else {
                match handle(env, this, a(0)) {
                    Some(Handle::Technique(i)) => Some(i + 1),
                    _ => None,
                }
            };
            let candidates: Vec<_> = next
                .map(|i| {
                    o.technique_handles
                        .iter()
                        .copied()
                        .enumerate()
                        .skip(i)
                        .collect()
                })
                .unwrap_or_default();
            let value = candidates
                .into_iter()
                .find_map(|(i, value)| validate_technique(env, this, i).is_ok().then_some(value))
                .unwrap_or(0);
            let valid = words(env, a(1), &[value]);
            Outcome::Return(if !valid {
                INVALID
            } else if value == 0 {
                0x80004005
            } else {
                0
            })
        }
        62 => {
            let (Some(parameter), Some(Handle::Technique(technique))) =
                (param(env, this, a(0)), handle(env, this, a(1)))
            else {
                return Outcome::Return(0);
            };
            let o = object(env, this).unwrap();
            Outcome::Return(u32::from(o.binary.techniques[technique].passes.iter().any(
                |pass| {
                    pass.states
                        .iter()
                        .any(|state| state_uses_parameter(&o.binary, state, parameter, 0))
                },
            )))
        }
        63 => begin(env, this, a(0), a(1)),
        64 => begin_pass(env, this, a(0) as usize),
        65 => {
            let Some(pass) = object(env, this).unwrap().active_pass else {
                // Native CommitChanges is a no-op outside an active pass;
                // parameter storage is applied by the next BeginPass.
                return Outcome::Return(0);
            };
            apply_pass(env, this, pass)
        }
        66 => {
            let o = object_mut(env, this).unwrap();
            if o.active_pass.take().is_none() {
                return Outcome::Return(INVALID);
            }
            Outcome::Return(0)
        }
        67 => end(env, this),
        68 => {
            let device = object(env, this).unwrap().device;
            if !words(env, a(0), &[device]) {
                return Outcome::Return(INVALID);
            }
            let mut c = Calls::new();
            c.push(device, 1, []);
            calls(env, c, 0)
        }
        69 | 70 => {
            let o = object_mut(env, this).unwrap();
            if o.started {
                return Outcome::Return(INVALID);
            }
            // State blocks do not survive Reset; record them again afterwards.
            let mut c = Calls::new();
            if slot == 69 {
                for block in std::mem::take(&mut o.technique_states) {
                    if block != 0 {
                        c.push(block, 2, []);
                    }
                }
            }
            calls(env, c, 0)
        }
        71 if a(0) == 0 => Outcome::Return(0),
        _ => fail(env, "Effect method has no implemented contract"),
    }
}

fn descriptor(env: &mut Env<'_>, this: u32, slot: u32, args: &[u32]) -> Outcome {
    let o = object(env, this).unwrap();
    let name = |s: &String| o.names.get(s).copied().unwrap_or(0);
    let data = match slot {
        3 => vec![
            0,
            o.binary.roots.len() as u32,
            o.binary.techniques.len() as u32,
            0,
        ],
        4 => {
            let Some(Handle::Parameter(i)) = handle(env, this, args[0]) else {
                return Outcome::Return(INVALID);
            };
            let p = &o.binary.parameters[i];
            vec![
                name(&p.name),
                name(&p.semantic),
                p.class,
                p.kind,
                p.rows,
                p.columns,
                p.elements,
                p.annotations.len() as u32,
                if p.class == 5 {
                    p.children.len() as u32
                } else {
                    0
                },
                p.flags,
                p.bytes.len() as u32,
            ]
        }
        5 => {
            let Some(Handle::Technique(i)) = handle(env, this, args[0]) else {
                return Outcome::Return(INVALID);
            };
            let t = &o.binary.techniques[i];
            vec![
                name(&t.name),
                t.passes.len() as u32,
                t.annotations.len() as u32,
            ]
        }
        6 => {
            let Some(Handle::Pass(i, j)) = handle(env, this, args[0]) else {
                return Outcome::Return(INVALID);
            };
            let p = &o.binary.techniques[i].passes[j];
            let shader = |op| {
                p.states
                    .iter()
                    .find(|s| s.operation == op)
                    .and_then(|s| o.binary.parameters[s.reference.unwrap_or(s.parameter)].object)
                    .and_then(|id| o.object_data.get(id))
                    .copied()
                    .unwrap_or(0)
            };
            vec![
                name(&p.name),
                p.annotations.len() as u32,
                shader(146),
                shader(147),
            ]
        }
        _ => return Outcome::Return(INVALID),
    };
    Outcome::Return(if words(env, *args.last().unwrap(), &data) {
        0
    } else {
        INVALID
    })
}
fn get_handle(env: &mut Env<'_>, this: u32, slot: u32, args: &[u32]) -> Outcome {
    let o = object(env, this).unwrap();
    let a = |i: usize| args.get(i).copied().unwrap_or(0);
    let result = match slot {
        8 => {
            let list = if a(0) == 0 {
                Some(&o.binary.roots)
            } else {
                match handle(env, this, a(0)) {
                    Some(Handle::Parameter(i)) => Some(&o.binary.parameters[i].children),
                    _ => None,
                }
            };
            list.and_then(|l| l.get(a(1) as usize))
                .and_then(|i| o.param_handles.get(*i))
                .copied()
        }
        9 | 10 => {
            let text = string(env, a(1));
            let children = if a(0) == 0 {
                o.binary.roots.clone()
            } else {
                param(env, this, a(0))
                    .map(|i| o.binary.parameters[i].children.clone())
                    .unwrap_or_default()
            };
            text.and_then(|n| {
                children.into_iter().find(|i| {
                    if slot == 9 {
                        o.binary.parameters[*i].name == n || o.binary.parameters[*i].full_name == n
                    } else {
                        o.binary.parameters[*i].semantic.eq_ignore_ascii_case(&n)
                    }
                })
            })
            .map(|i| o.param_handles[i])
        }
        11 => param(env, this, a(0))
            .and_then(|i| o.binary.parameters[i].children.get(a(1) as usize))
            .map(|i| o.param_handles[*i]),
        12 => o.technique_handles.get(a(0) as usize).copied(),
        13 => string(env, a(0))
            .and_then(|n| o.binary.techniques.iter().position(|t| t.name == n))
            .map(|i| o.technique_handles[i]),
        14 | 15 => {
            let Some(Handle::Technique(i)) = handle(env, this, a(0)) else {
                return Outcome::Return(0);
            };
            let j = if slot == 14 {
                Some(a(1) as usize)
            } else {
                string(env, a(1)).and_then(|n| {
                    o.binary.techniques[i]
                        .passes
                        .iter()
                        .position(|p| p.name == n)
                })
            };
            j.and_then(|j| o.pass_handles[i].get(j)).copied()
        }
        18 | 19 => {
            let list = match handle(env, this, a(0)) {
                Some(Handle::Parameter(i)) => Some(&o.binary.parameters[i].annotations),
                Some(Handle::Technique(i)) => Some(&o.binary.techniques[i].annotations),
                Some(Handle::Pass(i, j)) => Some(&o.binary.techniques[i].passes[j].annotations),
                _ => None,
            };
            list.and_then(|l| {
                if slot == 18 {
                    l.get(a(1) as usize).copied()
                } else {
                    string(env, a(1)).and_then(|n| {
                        l.iter()
                            .copied()
                            .find(|i| o.binary.parameters[*i].name == n)
                    })
                }
            })
            .map(|i| o.param_handles[i])
        }
        _ => None,
    };
    Outcome::Return(result.unwrap_or(0))
}
fn numeric(env: &mut Env<'_>, this: u32, slot: u32, args: &[u32]) -> Outcome {
    let Some(id) = param(env, this, args[0]) else {
        return Outcome::Return(INVALID);
    };
    let p = object(env, this).unwrap().binary.parameters[id].clone();
    if p.class == 4 {
        return Outcome::Return(INVALID);
    }
    let read = slot != 78 && slot % 2 == 1;
    let raw = matches!(slot, 20 | 21 | 78);
    let offset = if slot == 78 { args[2] as usize } else { 0 };
    let (count, wire_words, stored_words, requested_kind) = if raw {
        (if slot == 78 { args[3] } else { args[2] } as usize, 0, 0, 0)
    } else {
        if !matches!(p.kind, 1..=3) || p.class > 3 {
            return Outcome::Return(INVALID);
        }
        match slot {
            22 | 23 | 26 | 27 | 30 | 31 => {
                if p.elements != 0 || p.rows != 1 || p.columns != 1 {
                    return Outcome::Return(INVALID);
                }
                (
                    1,
                    1,
                    1,
                    if slot < 26 {
                        1
                    } else if slot < 30 {
                        2
                    } else {
                        3
                    },
                )
            }
            24 | 25 | 28 | 29 | 32 | 33 => (
                (args[2] as usize).min(p.bytes.len() / 4),
                1,
                1,
                if slot < 28 {
                    1
                } else if slot < 32 {
                    2
                } else {
                    3
                },
            ),
            34 | 35 => {
                if p.elements != 0 || p.class > 1 {
                    return Outcome::Return(INVALID);
                }
                (1, 4, p.columns as usize, 3)
            }
            36 | 37 => {
                if p.class != 1 || args[2] > p.elements {
                    return Outcome::Return(INVALID);
                }
                (args[2] as usize, 4, p.columns as usize, 3)
            }
            38 | 39 | 44 | 45 => {
                if p.elements != 0 || !matches!(p.class, 2 | 3) {
                    return Outcome::Return(INVALID);
                }
                (1, 16, (p.rows * p.columns) as usize, 3)
            }
            40 | 41 | 46 | 47 => {
                if !matches!(p.class, 2 | 3) || args[2] > p.elements {
                    return Outcome::Return(INVALID);
                }
                (args[2] as usize, 16, (p.rows * p.columns) as usize, 3)
            }
            _ => return fail(env, "Effect matrix pointer arrays are unavailable"),
        }
    };
    let Some(wire_len) = count
        .checked_mul(if raw { 1 } else { wire_words * 4 })
        .filter(|n| *n <= binary::MAX_BYTES)
    else {
        return Outcome::Return(INVALID);
    };
    let stored_len = if raw {
        wire_len
    } else {
        count * stored_words * 4
    };
    if offset
        .checked_add(stored_len)
        .is_none_or(|n| n > p.bytes.len())
    {
        return Outcome::Return(INVALID);
    }
    let transpose = matches!(slot, 44..=47);
    let wire_index = |element: usize, component: usize| {
        let component = if wire_words == 16 {
            let row = component / p.columns as usize;
            let col = component % p.columns as usize;
            if transpose {
                col * 4 + row
            } else {
                row * 4 + col
            }
        } else {
            component
        };
        (element * wire_words + component) * 4
    };
    if read {
        let mut data = vec![0; wire_len];
        if raw {
            data.copy_from_slice(&p.bytes[offset..offset + stored_len]);
        } else if slot == 35 && p.kind == 2 && p.columns == 1 {
            let packed = u32::from_le_bytes(p.bytes[..4].try_into().unwrap());
            for (component, shift) in [16, 8, 0, 24].into_iter().enumerate() {
                data[component * 4..component * 4 + 4].copy_from_slice(
                    &(((packed >> shift) & 255) as f32 / 255.0)
                        .to_bits()
                        .to_le_bytes(),
                );
            }
        } else {
            for element in 0..count {
                for component in 0..stored_words {
                    let at = (element * stored_words + component) * 4;
                    let bits = u32::from_le_bytes(p.bytes[at..at + 4].try_into().unwrap());
                    let converted = convert_number(bits, p.kind, requested_kind);
                    let at = wire_index(element, component);
                    data[at..at + 4].copy_from_slice(&converted.to_le_bytes());
                }
            }
        }
        return Outcome::Return(if put(env, args[1], &data) { 0 } else { INVALID });
    }
    let immediate = matches!(slot, 22 | 26 | 30);
    let input = if immediate {
        args[1].to_le_bytes().to_vec()
    } else {
        if check_range(env.cpu, args[1], wire_len as u32, Prot::READ).is_err() {
            return Outcome::Return(INVALID);
        }
        let mut d = vec![0; wire_len];
        if env.cpu.read(args[1], &mut d).is_err() {
            return Outcome::Return(INVALID);
        }
        d
    };
    let mut data = vec![0; stored_len];
    if raw {
        data.copy_from_slice(&input);
    } else if slot == 34 && p.kind == 2 && p.columns == 1 {
        let mut packed = 0;
        for (component, shift) in [16, 8, 0, 24].into_iter().enumerate() {
            let value = f32::from_bits(u32::from_le_bytes(
                input[component * 4..component * 4 + 4].try_into().unwrap(),
            ));
            packed |= ((value.clamp(0.0, 1.0) * 255.0) as u32) << shift;
        }
        data.copy_from_slice(&packed.to_le_bytes());
    } else {
        // BOOL array setters use integer conversion: native preserves values
        // such as 2 when the destination parameter is an int or float.
        let source_kind = if slot == 24 { 2 } else { requested_kind };
        for element in 0..count {
            for component in 0..stored_words {
                let at = wire_index(element, component);
                let bits = u32::from_le_bytes(input[at..at + 4].try_into().unwrap());
                let converted = convert_number(bits, source_kind, p.kind);
                let at = (element * stored_words + component) * 4;
                data[at..at + 4].copy_from_slice(&converted.to_le_bytes());
            }
        }
    }
    let o = object_mut(env, this).unwrap();
    o.binary.parameters[id].bytes[offset..offset + stored_len].copy_from_slice(&data);
    sync_children(&mut o.binary, id);
    sync_parents(&mut o.binary, id);
    Outcome::Return(0)
}
fn convert_number(bits: u32, from: u32, to: u32) -> u32 {
    if from == to && from != 1 {
        return bits;
    }
    let number = match from {
        1 => f64::from(bits != 0),
        2 => (bits as i32) as f64,
        _ => f32::from_bits(bits) as f64,
    };
    match to {
        1 => u32::from(number != 0.0),
        2 => (number as i32) as u32,
        _ => (number as f32).to_bits(),
    }
}
fn sync_children(b: &mut binary::Effect, id: usize) {
    let children = b.parameters[id].children.clone();
    let bytes = b.parameters[id].bytes.clone();
    let mut at = 0;
    for child in children {
        let len = b.parameters[child].bytes.len();
        if at + len <= bytes.len() {
            b.parameters[child]
                .bytes
                .copy_from_slice(&bytes[at..at + len]);
            sync_children(b, child);
        }
        at += len;
    }
}
fn sync_parents(b: &mut binary::Effect, id: usize) {
    let parent = b.parameters.iter().position(|p| p.children.contains(&id));
    let Some(parent) = parent else {
        return;
    };
    let at: usize = b.parameters[parent]
        .children
        .iter()
        .take_while(|child| **child != id)
        .map(|child| b.parameters[*child].bytes.len())
        .sum();
    let bytes = b.parameters[id].bytes.clone();
    if let Some(target) = b.parameters[parent].bytes.get_mut(at..at + bytes.len()) {
        target.copy_from_slice(&bytes);
    }
    sync_parents(b, parent);
}
fn parameter_contains(b: &binary::Effect, parent: usize, child: usize) -> bool {
    parent == child
        || b.parameters[parent]
            .children
            .iter()
            .any(|id| parameter_contains(b, *id, child))
}
fn state_uses_parameter(
    b: &binary::Effect,
    state: &binary::State,
    parameter: usize,
    depth: u32,
) -> bool {
    if depth > 16 {
        return false;
    }
    let id = state.reference.unwrap_or(state.parameter);
    if parameter_contains(b, parameter, id) {
        return true;
    }
    let p = &b.parameters[id];
    if state.operation == 178 {
        return p
            .sampler
            .iter()
            .any(|state| state_uses_parameter(b, state, parameter, depth + 1));
    }
    if !matches!(state.operation, 146 | 147) {
        return false;
    }
    let Some(bytes) = p.object.and_then(|id| b.objects.get(id)) else {
        return false;
    };
    let mut entries = constants(bytes).unwrap_or_default();
    let words: Vec<_> = bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|v| u32::from_le_bytes(*v))
        .collect();
    for (tag, program) in super::preshader::comments(&words).unwrap_or_default() {
        if tag == 0x53455250 {
            entries.extend(constants(bytemuck::cast_slice(program)).unwrap_or_default());
        }
    }
    let name = &b.parameters[parameter].full_name;
    entries.iter().any(|constant| {
        let used = constant.name == *name
            || constant
                .name
                .strip_prefix(name)
                .is_some_and(|rest| rest.starts_with('.') || rest.starts_with('['))
            || name
                .strip_prefix(&constant.name)
                .is_some_and(|rest| rest.starts_with('.') || rest.starts_with('['));
        used || b
            .parameters
            .iter()
            .find(|p| p.full_name == constant.name)
            .is_some_and(|p| {
                p.sampler
                    .iter()
                    .any(|state| state_uses_parameter(b, state, parameter, depth + 1))
            })
    })
}
fn get_object_value(env: &mut Env<'_>, this: u32, args: &[u32], slot: u32) -> Outcome {
    let Some(id) = param(env, this, args[0]) else {
        return Outcome::Return(INVALID);
    };
    let o = object(env, this).unwrap();
    let p = &o.binary.parameters[id];
    if !(match slot {
        51 => p.kind == 4,
        53 => (5..=9).contains(&p.kind),
        54 => p.kind == 15,
        55 => p.kind == 16,
        _ => false,
    }) {
        return Outcome::Return(INVALID);
    }
    let value = if matches!(p.kind, 15 | 16) {
        p.object
            .and_then(|i| o.shaders.get(i))
            .copied()
            .unwrap_or(0)
    } else if p.bytes.len() == 4 {
        u32::from_le_bytes(p.bytes[..4].try_into().unwrap())
    } else {
        return Outcome::Return(INVALID);
    };
    if !words(env, args[1], &[value]) {
        return Outcome::Return(INVALID);
    }
    if slot != 51 && value != 0 {
        let mut c = Calls::new();
        c.push(value, 1, []);
        calls(env, c, 0)
    } else {
        Outcome::Return(0)
    }
}
fn set_texture(env: &mut Env<'_>, this: u32, args: &[u32]) -> Outcome {
    let Some(id) = param(env, this, args[0]) else {
        return Outcome::Return(INVALID);
    };
    let p = &object(env, this).unwrap().binary.parameters[id];
    if !(5..=9).contains(&p.kind) || p.bytes.len() != 4 {
        return Outcome::Return(INVALID);
    }
    let old = u32::from_le_bytes(p.bytes[..4].try_into().unwrap());
    let new = args[1];
    let mut c = Calls::new();
    if new != 0 {
        c.push(new, 1, []);
    }
    if old != 0 {
        c.push(old, 2, []);
    }
    object_mut(env, this).unwrap().binary.parameters[id].bytes = new.to_le_bytes().to_vec();
    calls(env, c, 0)
}
fn begin(env: &mut Env<'_>, this: u32, output: u32, flags: u32) -> Outcome {
    let o = object(env, this).unwrap();
    if o.started || o.current >= o.binary.techniques.len() {
        return Outcome::Return(INVALID);
    }
    let count = o.binary.techniques[o.current].passes.len() as u32;
    let device = o.device;
    if output != 0 && !words(env, output, &[count]) {
        return Outcome::Return(INVALID);
    }
    if flags & 1 != 0 {
        let o = object_mut(env, this).unwrap();
        o.started = true;
        o.begin_flags = flags;
        return Outcome::Return(0);
    }
    // Native D3DX saves only what the technique sets: it records the passes'
    // states into one block on first use, then captures it on each Begin.
    let o = object(env, this).unwrap();
    let technique = o.current;
    let saved = o.technique_states.get(technique).copied().unwrap_or(0);
    if saved != 0 {
        return capture(env, this, saved, flags);
    }
    let passes = o.binary.techniques[technique].passes.clone();
    let Some(scratch) = alloc(env, &[0; 4]) else {
        return Outcome::Return(OOM);
    };
    let mut c = Calls::new();
    c.tolerant = true;
    c.push(device, 60, []);
    for pass in &passes {
        for state in &pass.states {
            if let Err(reason) = append_state(env, this, device, state, None, &mut c) {
                c.finish(env);
                free(env, scratch);
                return fail(env, reason);
            }
        }
    }
    c.push(device, 61, [scratch]);
    run(
        env,
        c,
        Box::new(move |env| {
            let block = env.cpu.read_u32(scratch).unwrap_or(0);
            free(env, scratch);
            if block == 0 {
                return Outcome::Return(INVALID);
            }
            let o = object_mut(env, this).unwrap();
            if o.technique_states.len() <= technique {
                o.technique_states.resize(technique + 1, 0);
            }
            o.technique_states[technique] = block;
            capture(env, this, block, flags)
        }),
    )
}
fn capture(env: &mut Env<'_>, this: u32, block: u32, flags: u32) -> Outcome {
    let mut c = Calls::new();
    c.push(block, 4, []);
    run(
        env,
        c,
        Box::new(move |env| {
            let o = object_mut(env, this).unwrap();
            o.saved_state = block;
            o.started = true;
            o.begin_flags = flags;
            Outcome::Return(0)
        }),
    )
}
fn begin_pass(env: &mut Env<'_>, this: u32, pass: usize) -> Outcome {
    let o = object_mut(env, this).unwrap();
    if !o.started || o.active_pass.is_some() || pass >= o.binary.techniques[o.current].passes.len()
    {
        return Outcome::Return(INVALID);
    }
    o.active_pass = Some(pass);
    apply_pass(env, this, pass)
}
fn end(env: &mut Env<'_>, this: u32) -> Outcome {
    let o = object_mut(env, this).unwrap();
    if o.active_pass.is_some() {
        return Outcome::Return(INVALID);
    }
    if !o.started {
        // A repeated native End succeeds without restoring/releasing twice.
        // An unexpected retained state block is not silently discarded.
        return Outcome::Return(if o.saved_state == 0 { 0 } else { INVALID });
    }
    o.started = false;
    let saved = std::mem::take(&mut o.saved_state);
    let mut c = Calls::new();
    if saved != 0 {
        c.push(saved, 5, []);
    }
    calls(env, c, 0)
}

fn apply_pass(env: &mut Env<'_>, this: u32, pass: usize) -> Outcome {
    let o = object(env, this).unwrap();
    let states = o.binary.techniques[o.current].passes[pass].states.clone();
    let device = o.device;
    let mut c = Calls::new();
    for state in &states {
        if let Err(reason) = append_state(env, this, device, state, None, &mut c) {
            for p in c.temporary {
                free(env, p);
            }
            return fail(env, reason);
        }
    }
    calls(env, c, 0)
}
fn validate_technique(env: &mut Env<'_>, this: u32, technique: usize) -> Result<(), &'static str> {
    let o = object(env, this).ok_or("Effect object disappeared")?;
    let device = o.device;
    let passes = o.binary.techniques[technique].passes.clone();
    let mut c = Calls::new();
    let result = passes.iter().try_for_each(|pass| {
        pass.states
            .iter()
            .try_for_each(|state| append_state(env, this, device, state, None, &mut c))
    });
    for pointer in c.temporary {
        free(env, pointer);
    }
    result
}
fn append_state(
    env: &mut Env<'_>,
    this: u32,
    device: u32,
    state: &binary::State,
    sampler: Option<u32>,
    c: &mut Calls,
) -> Result<(), &'static str> {
    let o = object(env, this).ok_or("Effect object disappeared")?;
    let p = &o.binary.parameters[state.reference.unwrap_or(state.parameter)];
    let value = if p.bytes.len() >= 4 {
        u32::from_le_bytes(p.bytes[..4].try_into().unwrap())
    } else {
        0
    };
    match state.operation {
        0..=102 => c.push(device, 57, [RENDER_STATES[state.operation as usize], value]),
        103..=120 => c.push(
            device,
            67,
            [
                state.index,
                TEXTURE_STATES[(state.operation - 103) as usize],
                value,
            ],
        ),
        122 => c.push(device, 89, [value]),
        146 | 147 => {
            let shader = p
                .object
                .and_then(|i| o.shaders.get(i))
                .copied()
                .unwrap_or(0);
            let object_id = p.object;
            c.push(
                device,
                if state.operation == 146 { 92 } else { 107 },
                [shader],
            );
            if let Some(id) = object_id {
                append_constants(env, this, device, id, state.operation == 146, c)?;
            }
        }
        164 => c.push(device, 65, [sampler.unwrap_or(state.index), value]),
        165..=177 => c.push(
            device,
            69,
            [sampler.unwrap_or(state.index), state.operation - 164, value],
        ),
        178 => {
            let states = p.sampler.clone();
            let index = state.index;
            for nested in states {
                append_state(env, this, device, &nested, Some(index), c)?;
            }
        }
        _ => return Err("Effect state operation is unavailable"),
    }
    Ok(())
}
fn append_constants(
    env: &mut Env<'_>,
    this: u32,
    device: u32,
    id: usize,
    vertex: bool,
    c: &mut Calls,
) -> Result<(), &'static str> {
    let table = shader_table(env, this, id)?;
    for program in &table.preshaders {
        let result = super::preshader::evaluate(bytemuck::cast_slice(program), |constant| {
            let o = object(env, this)?;
            let p = o
                .binary
                .parameters
                .iter()
                .find(|p| p.full_name == constant.name)?;
            pack_constant(p, constant).ok()
        })?;
        for (set, index, data) in result {
            c.push_data(device, constant_slot(vertex, set)?, index, data, 1);
        }
    }
    for (constant, parameter) in &table.constants {
        let Some(parameter) = *parameter else {
            continue;
        };
        if constant.set == 3 {
            let states = object(env, this).unwrap().binary.parameters[parameter]
                .sampler
                .clone();
            for state in states {
                append_state(env, this, device, &state, Some(constant.index), c)?;
            }
            continue;
        }
        let data = pack_constant(
            &object(env, this).unwrap().binary.parameters[parameter],
            constant,
        )?;
        let slot = constant_slot(vertex, constant.set)?;
        c.push_data(device, slot, constant.index, data, constant.count);
    }
    Ok(())
}
/// Constant table and preshaders of shader object `id`, parsed once.
fn shader_table(
    env: &mut Env<'_>,
    this: u32,
    id: usize,
) -> Result<std::sync::Arc<ShaderTable>, &'static str> {
    let o = object(env, this).unwrap();
    if let Some(table) = o.shader_tables.get(&id) {
        return Ok(table.clone());
    }
    let bytes = &o.binary.objects[id];
    let tokens: Vec<_> = bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|v| u32::from_le_bytes(*v))
        .collect();
    let preshaders = super::preshader::comments(&tokens)?
        .into_iter()
        .filter(|(tag, _)| *tag == 0x53455250)
        .map(|(_, program)| program.to_vec())
        .collect();
    let constants = constants(bytes)?
        .into_iter()
        .map(|constant| {
            let parameter = o
                .binary
                .parameters
                .iter()
                .position(|p| p.full_name == constant.name);
            (constant, parameter)
        })
        .collect();
    let table = std::sync::Arc::new(ShaderTable {
        constants,
        preshaders,
    });
    object_mut(env, this)
        .unwrap()
        .shader_tables
        .insert(id, table.clone());
    Ok(table)
}
fn constant_slot(vertex: bool, set: u32) -> Result<u32, &'static str> {
    match (vertex, set) {
        (true, 0) => Ok(98),
        (true, 1) => Ok(96),
        (true, 2) => Ok(94),
        (false, 0) => Ok(113),
        (false, 1) => Ok(111),
        (false, 2) => Ok(109),
        _ => Err("Effect register set"),
    }
}
fn pack_constant(p: &binary::Parameter, constant: &Constant) -> Result<Vec<u32>, &'static str> {
    if constant.count > 4096 {
        return Err("Effect constant register limit");
    }
    let mut data = vec![0u32; constant.count as usize * if constant.set == 0 { 1 } else { 4 }];
    let components: Vec<_> = p
        .bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| u32::from_le_bytes(*b))
        .collect();
    if constant.class <= 1 {
        let width = constant.columns.max(1) as usize;
        for (i, v) in components.iter().enumerate() {
            let at = if constant.set == 0 {
                i
            } else {
                i / width * 4 + i % width
            };
            if at < data.len() {
                data[at] = *v;
            }
        }
    } else if matches!(constant.class, 2 | 3) {
        let rows = constant.rows as usize;
        let columns = constant.columns as usize;
        if rows == 0 || columns == 0 || rows > 4 || columns > 4 {
            return Err("Effect matrix dimensions");
        }
        let matrices = p.elements.max(1) as usize;
        let stride = if constant.class == 2 { rows } else { columns } * 4;
        for matrix in 0..matrices {
            for row in 0..rows {
                for col in 0..columns {
                    let at = matrix * stride
                        + if constant.class == 2 {
                            row * 4 + col
                        } else {
                            col * 4 + row
                        };
                    if at < data.len() {
                        data[at] = *components
                            .get(matrix * rows * columns + row * columns + col)
                            .unwrap_or(&0);
                    }
                }
            }
        }
    } else {
        return Err("Effect shader structure constant is unavailable");
    }
    Ok(data)
}
#[derive(Debug)]
pub(super) struct Constant {
    pub name: String,
    pub set: u32,
    pub index: u32,
    pub count: u32,
    pub class: u32,
    pub rows: u32,
    pub columns: u32,
}
pub(super) fn constants(bytes: &[u8]) -> Result<Vec<Constant>, &'static str> {
    let words: Vec<_> = bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|v| u32::from_le_bytes(*v))
        .collect();
    let mut at = 1;
    let mut result = Vec::new();
    while at < words.len() {
        let token = words[at];
        if token & 0xffff == 0xffff {
            break;
        }
        if token & 0xffff == 0xfffe {
            let n = ((token >> 16) & 0x7fff) as usize;
            let Some(comment) = words.get(at + 1..at + 1 + n) else {
                return Err("Effect shader comment bounds");
            };
            if comment.first() == Some(&0x42415443) {
                let data = bytemuck::cast_slice::<u32, u8>(&comment[1..]);
                let u32at = |i| {
                    data.get(i..i + 4)
                        .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
                };
                let u16at = |i| {
                    data.get(i..i + 2)
                        .map(|b| u16::from_le_bytes(b.try_into().unwrap()) as u32)
                };
                let count = u32at(12).ok_or("Effect CTAB header")?;
                let table = u32at(16).ok_or("Effect CTAB header")? as usize;
                if count > 4096 {
                    return Err("Effect CTAB limit");
                }
                for i in 0..count {
                    let entry = table + i as usize * 20;
                    let name = u32at(entry).ok_or("Effect CTAB entry")? as usize;
                    let name = data
                        .get(name..)
                        .and_then(|b| b.split(|v| *v == 0).next())
                        .ok_or("Effect CTAB name")?;
                    let ty = u32at(entry + 12).ok_or("Effect CTAB type")? as usize;
                    result.push(Constant {
                        name: String::from_utf8_lossy(name).into_owned(),
                        set: u16at(entry + 4).ok_or("Effect CTAB set")?,
                        index: u16at(entry + 6).ok_or("Effect CTAB index")?,
                        count: u16at(entry + 8).ok_or("Effect CTAB count")?,
                        class: u16at(ty).ok_or("Effect CTAB class")?,
                        rows: u16at(ty + 4).ok_or("Effect CTAB rows")?,
                        columns: u16at(ty + 6).ok_or("Effect CTAB columns")?,
                    });
                }
            }
            at += 1 + n;
        } else {
            let n = ((token >> 24) & 15) as usize;
            at += if n == 0 { 1 } else { 1 + n };
        }
    }
    Ok(result)
}
const TEXTURE_STATES: [u32; 18] = [
    1, 26, 2, 3, 4, 27, 5, 6, 28, 7, 8, 9, 10, 11, 22, 23, 24, 32,
];
// Binary Effect state ordinals are distinct from D3DRENDERSTATETYPE values.
const RENDER_STATES: [u32; 103] = [
    7, 8, 9, 14, 15, 16, 19, 20, 22, 23, 24, 25, 26, 27, 28, 29, 34, 35, 36, 37, 38, 48, 52, 53,
    54, 55, 56, 57, 58, 59, 60, 128, 129, 130, 131, 132, 133, 134, 135, 198, 199, 200, 201, 202,
    203, 204, 205, 136, 137, 139, 140, 141, 142, 145, 146, 147, 148, 151, 152, 154, 155, 166, 156,
    157, 158, 159, 160, 161, 162, 163, 164, 165, 167, 168, 170, 171, 172, 173, 174, 175, 176, 178,
    179, 180, 181, 182, 183, 184, 185, 186, 187, 188, 189, 190, 191, 192, 193, 194, 195, 206, 207,
    208, 209,
];

#[win32(d3dx9_43,name="D3DXGetShaderOutputSemantics",level=implement)]
fn output_semantics(
    env: &mut Env<'_>,
    function: GuestPtr<u32>,
    output: GuestPtr<u32>,
    count: GuestPtr<u32>,
) -> u32 {
    let Ok(words) =
        crate::shader::read_bytecode(|index| function.offset(index as u32)?.read(env.cpu))
    else {
        return INVALID;
    };
    let Ok(shader) = crate::shader::Shader::parse(&words) else {
        return INVALID;
    };
    let semantics = shader_semantics(&shader);
    if count.write(env.cpu, (semantics.len() / 2) as u32).is_err() {
        return INVALID;
    }
    if !output.is_null() && !put(env, output.addr(), bytemuck::cast_slice(&semantics)) {
        return INVALID;
    }
    0
}

fn shader_semantics(shader: &crate::shader::Shader) -> Vec<u32> {
    use crate::shader::{RegisterKind, Stage};
    if shader.stage == Stage::Vertex && shader.version.major >= 3 {
        return shader
            .outputs
            .iter()
            .flat_map(|d| [d.usage as u32, d.index as u32])
            .collect();
    }
    let mut registers = std::collections::BTreeSet::new();
    for instruction in &shader.instructions {
        let Some(destination) = &instruction.destination else {
            continue;
        };
        let r = destination.register;
        let key = match (shader.stage, r.kind, r.index) {
            (Stage::Vertex, RegisterKind::Output, i) => Some((0, i, 5, i)),
            (Stage::Vertex, RegisterKind::ColorOutputLegacy, i) => Some((1, i, 10, i)),
            (Stage::Vertex, RegisterKind::RasterOutput, 0) => Some((2, 0, 0, 0)),
            (Stage::Vertex, RegisterKind::RasterOutput, 1) => Some((2, 1, 11, 0)),
            (Stage::Vertex, RegisterKind::RasterOutput, 2) => Some((2, 2, 4, 0)),
            (Stage::Pixel, RegisterKind::ColorOutput, i) => Some((0, i, 10, i)),
            (Stage::Pixel, RegisterKind::DepthOutput, _) => Some((1, 0, 12, 0)),
            _ => None,
        };
        if let Some(key) = key {
            registers.insert(key);
        }
    }
    if shader.stage == Stage::Pixel && shader.version.major == 1 {
        registers.insert((0, 0, 10, 0));
    }
    registers
        .into_iter()
        .flat_map(|(_, _, usage, index)| [usage as u32, u32::from(index)])
        .collect()
}
