//! Deferred D3D9 commands keep their own data and image leases. Content tracks
//! logical COM subresources, not rotating backend allocations. Complete clears
//! replace dependencies; partial clears and draws retain their previous contents.
//! A flush owns the device until its Present/readback finishes. Busy restores
//! only the unexecuted command, so continuations never replay successful prefixes.
use super::*;
pub use crate::record::Counters;
use crate::record::{Contents, ResourceKey as Content, damage::Analysis, passes};
#[cfg(feature = "d3d9-counters")]
use crate::record::{lookup, lookups};
use rustc_hash::FxHashMap as HashMap;
pub(super) mod damage;
mod encode;
mod fast;
mod key;
mod reuse;
pub mod trace;
pub(super) use key::fixed as fixed_key;
pub(super) use key::hash;
pub(super) use key::rect;
pub(super) use reuse::{bound, indexed, up};
pub(super) type DrawStateIdentity = (u128, Option<Id>, Option<Id>, Option<Id>, u128, u128);

pub(super) fn draw_state_identity(env: &Env<'_>, pointer: u32) -> (u128, bool) {
    let d = device(env, pointer).unwrap();
    let hot = d.hot.borrow();
    let identity = (
        hot.fixed.key(),
        hot.shaders.vertex,
        hot.shaders.pixel,
        hot.buffers.declaration,
        constant_key(env, hot.shaders.vertex, &hot.shaders.vertex_constants),
        constant_key(env, hot.shaders.pixel, &hot.shaders.pixel_constants),
    );
    if let Some((last, key)) = hot.draw_key.get()
        && last == identity
    {
        return (key, false);
    }
    let key = hash(identity);
    hot.draw_key.set(Some((identity, key)));
    (key, true)
}

fn constant_key(env: &Env<'_>, id: Option<Id>, constants: &shaders::Constants) -> u128 {
    constants.key(
        id.and_then(|id| shaders::bound(env, id))
            .and_then(|s| s.translation.as_ref().ok())
            .map_or(Default::default(), |t| t.constants),
    )
}

#[cfg(test)]
pub(super) fn draw_key(env: &Env<'_>, pointer: u32) -> u128 {
    draw_state_identity(env, pointer).0
}

#[derive(Clone, Copy, Debug)]
pub(super) enum Reason {
    Present,
    Readback,
    Lock,
    Dc,
    Reset,
    Release,
    Limit,
    Wait,
}
struct ReadbackTarget {
    source: Content,
    destination: u32,
    storage: u32,
    version: Option<u128>,
    damage: adv_device::surface::Region,
    in_flight: Option<u32>,
}

#[cfg(test)]
type TestExecutionPlan = Vec<(bool, u8, Option<Vec<adv_device::surface::Rect>>)>;

#[derive(Default)]
pub(super) struct Recording {
    commands: Vec<Operation>,
    #[cfg(test)]
    command_identities: Vec<u128>,
    /// The damage analysis write behind each command, while it knows it.
    nodes: Vec<Option<usize>>,
    #[cfg(test)]
    separate_draws: bool,
    #[cfg(test)]
    general_draws: bool,
    #[cfg(test)]
    test_plan: Option<TestExecutionPlan>,
    fast: fast::Cache,
    vertices: Vec<u8>,
    vertex_ranges: Vec<Option<std::ops::Range<usize>>>,
    /// The content a readback flush observes, and the writes, by content,
    /// that flush executes; the rest wait for presentation.
    observe: Option<Content>,
    select: Option<HashMap<usize, Vec<Content>>>,
    reuse: reuse::Cache,
    readback_targets: HashMap<adv_device::gpu::ImageId, ReadbackTarget>,
    passes: passes::Plan,
    damage: Analysis,
    verify_region: Option<adv_device::surface::Region>,
    output_slot: Option<u128>,
    executed: HashMap<u128, adv_device::surface::Region>,
    verify_image: Option<adv_device::gpu::Image>,
    baseline: Option<adv_device::gpu::Image>,
    pub(super) rotate: bool,
    held_uploads: Vec<(Id, Option<u32>)>,
    head: usize,
    /// Scissors the operation at `head` finished before it was busy.
    part: usize,
    bytes: usize,
    contents: Contents,
    binding_cache: Option<key::Bindings>,
    upload: u128,
    active: Option<u64>,
    next: u64,
    partial: bool,
    last: Option<u128>,
    last_copy: Option<(u32, presentation::Copy)>,
    last_serial: Option<u64>,
    verdict: Option<bool>,
    presented: Option<(Completion, Option<u64>)>,
    previous: Option<(u32, u32, Vec<u8>)>,
    readback: Option<adv_device::gpu::Readback>,
    job: Option<adv_sync::background::Task>,
}
impl Recording {
    /// Drops the damage analysis; pending commands then run whole.
    fn forget_damage(&mut self) {
        self.damage.invalidate();
        self.readback_targets.clear();
        self.nodes.fill(None);
    }
}
pub(super) fn forget(env: &mut Env<'_>, owner: Id, id: Option<Id>, pointer: u32) {
    if let Some(Payload::Device(d)) = com(env).get_identity_mut(&mut env.sys.objects, owner) {
        let f = &mut d.frame;
        let allocation = id.map(Id::key);
        let changed = f.contents.forget(|key| Some(key.0) == allocation);
        f.binding_cache = None;
        f.fast.clear();
        f.reuse.invalidate();
        f.damage.forget(|key| Some(key.0) == allocation);
        f.readback_targets
            .retain(|_, r| Some(r.source.0) != allocation && r.destination != pointer);
        if changed {
            f.partial = true;
            f.last = None;
        }
    }
}
#[allow(clippy::too_many_arguments)]
pub(super) fn readback_regions(
    env: &mut Env<'_>,
    owner: Id,
    source: Id,
    destination: u32,
    storage: u32,
    event: u32,
    image: &adv_device::gpu::Image,
) -> Vec<adv_device::surface::Rect> {
    use adv_device::surface::{Region, rects};
    let source = key::content(env, source, None, 0);
    let f = frame(env, owner);
    let version = f.contents.get(&source).copied();
    let size = (image.width(), image.height());
    let regions = if let Some(r) = f.readback_targets.get(&image.id()).filter(|r| {
        r.source == source
            && r.destination == destination
            && r.storage == storage
            && r.in_flight.is_none()
    }) {
        if r.version.is_some() && r.version == version {
            Vec::new()
        } else {
            rects(&r.damage)
        }
    } else {
        vec![adv_device::surface::Rect {
            x: 0,
            y: 0,
            w: size.0,
            h: size.1,
        }]
    };
    invalidate_readback_destination(env, owner, storage);
    frame(env, owner).readback_targets.insert(
        image.id(),
        ReadbackTarget {
            source,
            destination,
            storage,
            version,
            damage: Region::empty(size),
            in_flight: Some(event),
        },
    );
    regions
}

pub(super) fn invalidate_readback_destination(env: &mut Env<'_>, owner: Id, storage: u32) {
    frame(env, owner)
        .readback_targets
        .retain(|_, r| r.storage != storage);
}

pub(super) fn complete_readback(
    env: &mut Env<'_>,
    owner: Id,
    slot: adv_device::gpu::ImageId,
    event: u32,
    success: bool,
) {
    let targets = &mut frame(env, owner).readback_targets;
    if let Some(r) = targets
        .get_mut(&slot)
        .filter(|r| r.in_flight == Some(event))
    {
        if success {
            r.in_flight = None;
        } else {
            targets.remove(&slot);
        }
    }
}

/// What the frame being presented changed since the previous presentation, when known.
pub(super) fn changed(f: &Recording) -> Option<adv_device::surface::Region> {
    f.verify_region.clone()
}
fn frame<'a>(env: &'a mut Env<'_>, owner: Id) -> &'a mut Recording {
    let Some(Payload::Device(d)) = com(env).get_identity_mut(&mut env.sys.objects, owner) else {
        unreachable!("retained recording device")
    };
    &mut d.frame
}
pub(super) fn owner(env: &Env<'_>, id: Id) -> Id {
    match com(env).get_identity(&env.sys.objects, id) {
        Some(Payload::Resource(r)) => r.device,
        Some(Payload::Buffer(b)) => b.device,
        Some(Payload::Declaration(d)) => d.device,
        Some(Payload::Shader(s)) => s.device,
        Some(Payload::StateBlock(b)) => b.device,
        Some(Payload::SwapChain(owner)) => *owner,
        Some(Payload::Query(q)) => q.device,
        _ => id,
    }
}
fn token(env: &mut Env<'_>, owner: Id) -> u64 {
    let f = frame(env, owner);
    f.next = f.next.checked_add(1).expect("flush token exhausted");
    f.next
}
pub(super) fn query_pixels(f: &Recording, queries: &mut queries::DeviceState) -> u64 {
    for operation in &f.commands[queries.scanned..] {
        if let Operation::Draw(draw) = operation {
            let area = u64::from(draw.target.image.width()) * u64::from(draw.target.image.height());
            queries.pixels = queries.pixels.wrapping_add(area);
        }
    }
    queries.scanned = f.commands.len();
    queries.pixels
}
pub(super) fn query_pending(f: &Recording) -> bool {
    !f.commands.is_empty() || f.active.is_some()
}
pub(super) fn query_flush(env: &mut Env<'_>, owner: Id) -> Result<(), Error> {
    if frame(env, owner).active.is_some() {
        return Err(Error::Busy);
    }
    if frame(env, owner).commands.is_empty() {
        return Ok(());
    }
    let token = token(env, owner);
    let result = flush(env, owner, token, Reason::Wait);
    if matches!(result, Err(Error::Busy)) {
        cancel(
            env,
            &Operation::Flush {
                owner,
                token,
                reason: Reason::Wait,
            },
        );
    } else {
        unlock(env, owner, token);
    }
    result.map(|_| ())
}
pub(super) fn has_work(env: &mut Env<'_>, owner: Id) -> bool {
    matches!(
        com(env).get_identity(&env.sys.objects, owner),
        Some(Payload::Device(_))
    ) && (!frame(env, owner).commands.is_empty()
        || frame(env, owner).active.is_some()
        || restore_operation(env, owner).is_some())
}
/// Whether reading `surface` must first flush recorded work, which then
/// executes only what that read depends on (GFX-REC-5).
pub(super) fn observes(env: &mut Env<'_>, owner: Id, surface: Id) -> bool {
    if !has_work(env, owner) {
        return false;
    }
    let content = key::content(env, surface, None, 0);
    let f = frame(env, owner);
    if f.active.is_some() || f.commands.is_empty() {
        return true;
    }
    // A version the slot already holds needs no flush.
    if !env.ext.get::<State>().verify_frames && frame(env, owner).damage.settled(&content) {
        return false;
    }
    let f = frame(env, owner);
    let pending = f.commands[f.head..]
        .iter()
        .zip(&f.nodes[f.head..])
        .any(|(op, node)| {
            !matches!(op, Operation::Consumed) && node.is_none_or(|n| f.damage.pending(n, &content))
        });
    if pending {
        f.observe = Some(content);
    }
    pending
}
// A frame may stop rebuilding the whole DISCARD target after a rotating frame.
// Seed such a frame (or an early read) from the last logical backbuffer first.
fn restore_operation(env: &Env<'_>, owner: Id) -> Option<Operation> {
    let Some(Payload::Device(d)) = com(env).get_identity(&env.sys.objects, owner) else {
        return None;
    };
    let source = d.frame.baseline.as_ref()?;
    env.ext.get::<State>().gpu.as_ref()?.check().ok()?;
    let destination = (env.ext.get::<State>().provider.target)(env.ext, d.surface)?;
    if source.shares_storage(&destination.image) {
        return None;
    }
    let rectangle = adv_device::surface::Rect {
        x: 0,
        y: 0,
        w: source.width(),
        h: source.height(),
    };
    Some(Operation::Copy {
        source: source.clone(),
        rectangle,
        destination,
        region: rectangle,
        filter: adv_device::gpu::Filter::Nearest,
        resources: [d.back_buffer?; 2],
    })
}
fn follow(
    env: &mut Env<'_>,
    outcome: Outcome,
    then: impl FnOnce(&mut Env<'_>) -> Outcome + 'static,
) -> Outcome {
    match outcome {
        Outcome::Return(0) => then(env),
        Outcome::Block {
            wait,
            then: resume,
            allow_time_skip,
        } => Outcome::Block {
            wait,
            allow_time_skip,
            then: Box::new(move |env, r| {
                let outcome = resume(env, r);
                follow(env, outcome, then)
            }),
        },
        other => other,
    }
}
pub(super) fn barrier(
    env: &mut Env<'_>,
    owner: Id,
    reason: Reason,
    then: impl FnOnce(&mut Env<'_>) -> Outcome + 'static,
) -> Outcome {
    if !matches!(
        com(env).get_identity(&env.sys.objects, owner),
        Some(Payload::Device(_))
    ) {
        return then(env);
    }
    if frame(env, owner).commands.is_empty() && frame(env, owner).active.is_none() {
        if let Some(operation) = restore_operation(env, owner) {
            let image = damage::image(&operation).unwrap().clone();
            let result = dispatch_now(env, owner, operation);
            return follow(env, result, move |env| {
                frame(env, owner).baseline = Some(image);
                then(env)
            });
        }
        return then(env);
    }
    let token = token(env, owner);
    let result = dispatch_now(
        env,
        owner,
        Operation::Flush {
            owner,
            token,
            reason,
        },
    );
    follow(env, result, then)
}
pub(super) fn record(env: &mut Env<'_>, id: Id, operation: Operation) -> Outcome {
    let owner = owner(env, id);
    if env.ext.get::<State>().gpu.is_none() {
        return Outcome::Return(UNAVAILABLE);
    }
    let prepared = match key::prepare(env, owner, operation) {
        Ok(p) => p,
        Err(e) => return Outcome::Return(e),
    };
    barrier_if_busy(env, owner, move |env| accept(env, owner, prepared))
}
fn barrier_if_busy(
    env: &mut Env<'_>,
    owner: Id,
    then: impl FnOnce(&mut Env<'_>) -> Outcome + 'static,
) -> Outcome {
    if frame(env, owner).active.is_some() {
        barrier(env, owner, Reason::Wait, then)
    } else {
        then(env)
    }
}
fn accept(env: &mut Env<'_>, owner: Id, p: key::Prepared) -> Outcome {
    append(env, owner, &p, None, None, None)
}
fn append(
    env: &mut Env<'_>,
    owner: Id,
    p: &key::Prepared,
    target: Option<Target>,
    entry: Option<&reuse::Entry>,
    vertices: Option<std::ops::Range<usize>>,
) -> Outcome {
    #[cfg(feature = "d3d9-counters")]
    let before = lookups();
    #[cfg(feature = "d3d9-counters")]
    lookup(1);
    let Some(Payload::Device(d)) = com(env).get_identity(&env.sys.objects, owner) else {
        return Outcome::Return(INVALID);
    };
    let retained = entry.and_then(|e| e.saved.get());
    let shared = retained.filter(|saved| {
        p.writes
            .iter()
            .zip(&saved.destinations)
            .all(|((k, _), old)| {
                d.frame.damage.independent(k) || d.frame.contents.get(k).copied() == *old
            })
    });
    let bytes = if retained.is_some() { 0 } else { p.bytes };
    let lost = d.gpu_lost;
    let index = d.frame.commands.len();
    let limit = d.frame.commands.len() + 1 >= 4096 || d.frame.bytes + bytes >= 64 * 1024 * 1024;
    // Ordinary admission unlocks immediately. A limit flush retains its lock
    // until execution, preserving the existing Busy API reservation.
    if (!limit || lost)
        && let Operation::Upload {
            resource, level, ..
        } = &p.operation
        && let Some(Payload::Resource(r)) =
            com(env).get_identity_mut(&mut env.sys.objects, *resource)
    {
        if let Some(n) = level {
            r.levels[*n as usize].lock = None
        } else {
            r.lock = None
        }
    }
    if lost {
        return Outcome::Return(0);
    }
    let f = frame(env, owner);
    trace::command(index, p, shared.is_some(), &f.contents);
    let previous = limit.then(|| {
        (
            bytes,
            p.writes
                .iter()
                .map(|(k, _)| (*k, f.contents.get(k).copied(), f.contents.had_base(k)))
                .collect::<Vec<_>>(),
        )
    });
    let destinations = entry.filter(|e| e.saved.get().is_none()).map(|_| {
        p.writes
            .iter()
            .map(|(k, _)| f.contents.get(k).copied())
            .collect()
    });
    let read_versions: smallvec::SmallVec<[Option<u128>; 4]> = if shared.is_none() {
        p.reads.iter().map(|k| f.contents.get(k).copied()).collect()
    } else {
        smallvec::SmallVec::new()
    };
    let mut draw_key_hashes = 0;
    let (command, attachment, damage) = if let Some(saved) = shared {
        let slot = hash(target.as_ref().unwrap().image.id());
        f.damage.replay(&saved.damage, slot, &f.contents);
        (saved.command, saved.attachment, None)
    } else {
        draw_key_hashes += 1;
        use std::hash::Hash;
        let mut command = xxhash_rust::xxh3::Xxh3::new();
        p.key.hash(&mut command);
        p.reads.len().hash(&mut command);
        for (&k, &version) in p.reads.iter().zip(&read_versions) {
            (k, version.unwrap_or(0)).hash(&mut command);
        }
        let command = command.digest128();
        let attachment = p.attachment_command(command, &read_versions);
        (
            command,
            attachment,
            f.damage.record(
                p.damage(target.as_ref()),
                [command, attachment],
                &f.contents,
                &read_versions,
                destinations.is_some(),
            ),
        )
    };
    let mut outputs = Vec::new();
    for (n, &(k, overwrite)) in p.writes.iter().enumerate() {
        let command = p.write_key(k, if k.2 == 0 { command } else { attachment });
        let slot = f.contents.resolve(k);
        let old = f.contents.slots[slot].1.unwrap_or(0);
        if f.contents.slots[slot].2.is_none() {
            f.contents.slots[slot].2 = Some(old);
            f.contents.touched.push(slot);
        }
        let next = if matches!(&p.operation, Operation::Upload { pixels, .. } if matches!(pixels.key(), UploadKey::Admission(_)))
        {
            f.upload = f.upload.checked_add(1).expect("upload content exhausted");
            f.upload
        } else if let Some(saved) = shared.filter(|saved| saved.destinations[n].unwrap_or(0) == old)
        {
            saved.outputs[n]
        } else {
            draw_key_hashes += 1;
            if overwrite {
                hash((k, command))
            } else {
                hash((old, command))
            }
        };
        f.contents.slots[slot].1 = Some(next);
        if destinations.is_some() {
            outputs.push(next);
        }
    }
    if limit
        && let Operation::Upload {
            resource, level, ..
        } = &p.operation
    {
        f.held_uploads.push((*resource, *level));
    }
    #[cfg(feature = "d3d9-counters")]
    let work = (shared.is_none() && matches!(p.operation, Operation::Draw(_))).then_some((
        (passes::Plan::bytes(p.reads.len())
            + std::mem::size_of::<Operation>()
            + match &p.operation {
                Operation::Draw(d) => d.programmable.as_ref().map_or(p.bytes, |draw| {
                    p.bytes - draw.streams.iter().map(|s| s.bytes.len()).sum::<usize>()
                        + std::mem::size_of_val(draw.as_ref())
                        + draw.vertex_constants.retained_bytes()
                        + draw.pixel_constants.retained_bytes()
                        + draw
                            .streams
                            .iter()
                            .map(|s| s.bytes.retained_bytes())
                            .sum::<usize>()
                }),
                _ => p.bytes,
            }) as u64,
        p.lookups + lookups() - before,
    ));
    #[cfg(feature = "d3d9-counters")]
    if let Some((bytes, lookups)) = work {
        let count = &mut env.ext.get_mut::<State>().frames;
        count.recorded_draws += 1;
        count.recorded_draw_bytes += bytes;
        count.recording_lookups += lookups;
    }
    #[cfg(feature = "d3d9-counters")]
    let f = frame(env, owner);
    f.bytes += bytes;
    if let Some(saved) = shared {
        f.passes.replay(saved.pass.clone());
    } else {
        let pass = f.passes.record(
            p.kind(),
            p.reads.clone(),
            read_versions,
            p.writes.clone(),
            p.bounds,
            destinations.is_some(),
        );
        if let (Some(entry), Some(destinations)) = (entry, destinations) {
            let _ = entry.saved.set(reuse::Saved {
                command,
                attachment,
                damage: damage.unwrap(),
                pass: pass.unwrap(),
                destinations,
                outputs,
            });
        }
    }
    let mut operation = p.operation.clone();
    if let (Some(target), Operation::Draw(draw)) = (target, &mut operation) {
        draw.target = target;
    }
    let is_draw = matches!(&operation, Operation::Draw(_));
    let fixed = matches!(&operation, Operation::Draw(d) if d.fixed_vertex.is_some() || d.programmable.is_none());
    f.commands.push(operation);
    let captured = vertices.is_some();
    f.vertex_ranges.push(vertices);
    #[cfg(test)]
    f.command_identities.push(command);
    f.nodes.push(f.damage.len().checked_sub(1));
    let limit = f.commands.len() >= 4096 || f.bytes >= 64 * 1024 * 1024;
    let counters = &mut env.ext.get_mut::<State>().frames;
    counters.recorded_commands += 1;
    counters.fast_path_draws += u64::from(captured);
    counters.general_path_draws += u64::from(is_draw && !captured);
    counters.fixed_draws += u64::from(fixed);
    if is_draw {
        counters.draw_key_hashes += draw_key_hashes + u64::from(captured);
    }
    if limit {
        let outcome = barrier(env, owner, Reason::Limit, |_| Outcome::Return(0));
        // Parking allocation failure rejects the incoming command. Successful
        // prefixes stay committed; older accepted commands retain their order.
        if matches!(outcome, Outcome::Return(OUT_OF_MEMORY)) {
            let (bytes, contents) = previous.unwrap();
            let f = frame(env, owner);
            assert!(f.head < f.commands.len());
            let rejected = f.commands.pop();
            if let Some(vertices) = f.vertex_ranges.pop().flatten() {
                f.vertices.truncate(vertices.start);
            }
            #[cfg(test)]
            f.command_identities.pop();
            f.nodes.pop();
            let len = f.commands.len();
            f.passes.truncate(f.commands.len());
            f.forget_damage();
            f.bytes -= bytes;
            for (k, old, had_base) in contents {
                f.contents.restore(k, old, had_base);
            }
            if let Some(Payload::Device(d)) = com(env).get_identity_mut(&mut env.sys.objects, owner)
                && d.queries.scanned > len
            {
                if let Some(Operation::Draw(draw)) = rejected {
                    let area = u64::from(draw.target.image.width())
                        * u64::from(draw.target.image.height());
                    d.queries.pixels = d.queries.pixels.wrapping_sub(area);
                }
                d.queries.scanned = len;
            }
            let counters = &mut env.ext.get_mut::<State>().frames;
            counters.recorded_commands -= 1;
            counters.fast_path_draws -= u64::from(captured);
            counters.general_path_draws -= u64::from(is_draw && !captured);
            counters.fixed_draws -= u64::from(fixed);
            #[cfg(feature = "d3d9-counters")]
            if let Some((bytes, lookups)) = work {
                let count = &mut env.ext.get_mut::<State>().frames;
                count.recorded_draws -= 1;
                count.recorded_draw_bytes -= bytes;
                count.recording_lookups -= lookups;
            }
        }
        outcome
    } else {
        Outcome::Return(0)
    }
}
fn enter(env: &mut Env<'_>, owner: Id, token: u64, reason: Reason) -> Result<(), Error> {
    match frame(env, owner).active {
        Some(n) if n != token => Err(Error::Busy),
        Some(_) => Ok(()),
        None => {
            frame(env, owner).active = Some(token);
            trace::flush(reason, frame(env, owner).commands.len());
            if !matches!(reason, Reason::Present) {
                // Flushes that only execute keep the damage analysis: it follows
                // the recorded commands, not when they run. CPU access, Reset
                // and Release change contents outside it.
                if !matches!(reason, Reason::Readback | Reason::Wait | Reason::Limit) {
                    frame(env, owner).forget_damage();
                }
                let f = frame(env, owner);
                if let Some(content) = f
                    .observe
                    .take()
                    .filter(|_| matches!(reason, Reason::Readback))
                {
                    f.select = Some(f.damage.closure(content));
                }
                f.reuse.invalidate();
            }
            env.ext.get_mut::<State>().frames.flushes[reason as usize] += 1;
            Ok(())
        }
    }
}
pub(super) fn flush(
    env: &mut Env<'_>,
    owner: Id,
    token: u64,
    reason: Reason,
) -> Result<Completion, Error> {
    enter(env, owner, token, reason)?;
    if let Some(Payload::Device(d)) = com(env).get_identity_mut(&mut env.sys.objects, owner)
        && d.queries.active != 0
    {
        query_pixels(&d.frame, &mut d.queries);
    }
    if !frame(env, owner).rotate
        && let Some(operation) = restore_operation(env, owner)
    {
        execute_gpu(env, &operation)?;
        let image = damage::image(&operation).unwrap();
        let f = frame(env, owner);
        f.baseline = Some(image.clone());
        f.readback_targets.remove(&image.id());
    }
    loop {
        let f = frame(env, owner);
        if f.head == f.commands.len() {
            let surface = match com(env).get_identity(&env.sys.objects, owner) {
                Some(Payload::Device(d)) => d.surface,
                _ => unreachable!(),
            };
            frame(env, owner).baseline =
                (env.ext.get::<State>().provider.target)(env.ext, surface).map(|t| t.image);
            return Ok(Completion::Command);
        }
        if f.passes.skip(f.head) {
            f.commands[f.head] = Operation::Consumed;
            f.head += 1;
            continue;
        }
        #[cfg(test)]
        let separate_draws = f.separate_draws;
        #[cfg(not(test))]
        let separate_draws = false;
        if !separate_draws && !env.ext.get::<State>().verify_frames {
            match encode::run(env, owner) {
                Ok(true) => continue,
                Ok(false) => (),
                Err(Error::Device(_)) => return Ok(lose(env, owner)),
                Err(error) => return Err(error),
            }
        }
        let f = frame(env, owner);
        let node = f.nodes[f.head];
        let selected = match &f.select {
            Some(select) => match node.map(|n| select.get(&n)) {
                Some(None) => {
                    // Not needed by the observation: it waits for presentation.
                    f.head += 1;
                    continue;
                }
                Some(Some(contents)) => Some(contents.clone()),
                None => None,
            },
            None => None,
        };
        let mut operation = std::mem::replace(&mut f.commands[f.head], Operation::Consumed);
        // Keep the original template across Busy; the captured bytes still
        // belong to the frame and may form part of a run on the next attempt.
        let mut captured = None;
        if let Some(range) = &f.vertex_ranges[f.head]
            && let Operation::Draw(draw) = &operation
        {
            let mut draw = draw.clone();
            fast::materialize(&mut draw, &f.vertices[range.clone()]);
            captured = Some(Operation::Draw(draw));
        }
        let execution = captured.as_ref();
        // A clear the observation needs only part of runs that part now and
        // keeps the rest in order.
        let mut original = None;
        if let (
            Some(contents),
            Operation::Clear {
                target,
                rectangle,
                color,
                depth,
                stencil,
            },
        ) = (&selected, &mut operation)
        {
            let aspect = |a: u8| contents.iter().any(|k| k.2 == a);
            let rest = (
                color.filter(|_| !aspect(0)),
                depth.filter(|_| !aspect(1)),
                stencil.filter(|_| !aspect(2)),
            );
            if rest != (None, None, None) {
                let whole = Operation::Clear {
                    target: target.clone(),
                    rectangle: *rectangle,
                    color: *color,
                    depth: *depth,
                    stencil: *stencil,
                };
                let rest = Operation::Clear {
                    target: target.clone(),
                    rectangle: *rectangle,
                    color: rest.0,
                    depth: rest.1,
                    stencil: rest.2,
                };
                (*color, *depth, *stencil) = (
                    color.filter(|_| aspect(0)),
                    depth.filter(|_| aspect(1)),
                    stencil.filter(|_| aspect(2)),
                );
                original = Some(whole);
                let f = frame(env, owner);
                let head = f.head;
                f.commands[head] = rest;
            }
        }
        let f = frame(env, owner);
        if let Operation::Clear {
            color,
            depth,
            stencil,
            ..
        } = &mut operation
        {
            let mask = f.passes.clear_mask(f.head);
            if mask & 1 == 0 {
                *color = None;
            }
            if mask & 2 == 0 {
                *depth = None;
            }
            if mask & 4 == 0 {
                *stencil = None;
            }
        }
        // The write's scissors, unless it runs whole (GFX-REC-5).
        let parts = (!env.ext.get::<State>().verify_frames)
            .then(|| node.and_then(|n| frame(env, owner).damage.clip(n).map(<[_]>::len)))
            .flatten();
        let result = if let Some(parts) = parts {
            let effective = execution.unwrap_or(&operation);
            if let Operation::Draw(draw) = effective {
                let mut clips = smallvec::SmallVec::<[adv_device::surface::Rect; 8]>::new();
                {
                    let f = frame(env, owner);
                    let all_clips = f.damage.clip(node.unwrap()).unwrap();
                    for &clip in &all_clips[f.part..parts] {
                        if !f.passes.outside_damage(f.head, clip) {
                            clips.push(clip);
                        }
                    }
                }
                if clips.is_empty() {
                    frame(env, owner).part = parts;
                    Ok(Completion::Command)
                } else {
                    let result = execute_draw_clips(env, draw, &clips);
                    if result.is_ok() {
                        for clip in clips {
                            submitted(env, owner, node, &operation, Some(clip));
                        }
                        frame(env, owner).part = parts;
                    }
                    result
                }
            } else {
                let mut result = Ok(Completion::Command);
                // A write meeting several scissors resumes after the ones it
                // finished when it was busy, so no blend applies twice.
                while result.is_ok() && frame(env, owner).part < parts {
                    let f = frame(env, owner);
                    let clip = f.damage.clip(node.unwrap()).unwrap()[f.part];
                    if !f.passes.outside_damage(f.head, clip) {
                        let gpu = env.ext.get_mut::<State>().gpu.as_mut().unwrap();
                        gpu.set_clip(Some(clip));
                        result = execute_gpu(env, effective);
                        env.ext
                            .get_mut::<State>()
                            .gpu
                            .as_mut()
                            .unwrap()
                            .set_clip(None);
                        if result.is_err() {
                            break;
                        }
                        submitted(env, owner, node, &operation, Some(clip));
                    }
                    frame(env, owner).part += 1;
                }
                result
            }
        } else {
            let result = execute_gpu(env, execution.unwrap_or(&operation));
            if result.is_ok() {
                submitted(env, owner, node, &operation, None);
            }
            result
        };
        match result {
            Err(Error::Busy) => {
                let f = frame(env, owner);
                f.commands[f.head] = original.unwrap_or(operation);
                return Err(Error::Busy);
            }
            Err(Error::Device(_)) => return Ok(lose(env, owner)),
            result => {
                if let Operation::Upload {
                    resource, level, ..
                } = &operation
                {
                    release_upload(env, owner, *resource, *level, false);
                }
                let f = frame(env, owner);
                if let Some(n) = node {
                    f.damage.executed(n, selected.as_deref());
                }
                f.head += 1;
                f.part = 0;
                f.partial = true;
                result?;
            }
        }
    }
}
fn lose(env: &mut Env<'_>, owner: Id) -> Completion {
    if let Some(Payload::Device(d)) = com(env).get_identity_mut(&mut env.sys.objects, owner) {
        d.gpu_lost = true;
        d.frame.damage.invalidate();
        d.frame.commands.clear();
        d.frame.vertex_ranges.clear();
        d.frame.vertices.clear();
        d.frame.nodes.clear();
        d.frame.passes.truncate(0);
        d.frame.head = 0;
        d.frame.part = 0;
    }
    Completion::Command
}

fn submitted(
    env: &mut Env<'_>,
    owner: Id,
    node: Option<usize>,
    operation: &Operation,
    clip: Option<adv_device::surface::Rect>,
) {
    let Some(image) = damage::image(operation) else {
        return;
    };
    let area = clip.unwrap_or(adv_device::surface::Rect {
        x: 0,
        y: 0,
        w: image.width(),
        h: image.height(),
    });
    let area = match operation {
        Operation::Clear { rectangle, .. } | Operation::Upload { rectangle, .. } => {
            area.clipped(*rectangle)
        }
        Operation::Copy { region, .. } => area.clipped(*region),
        _ => area,
    };
    frame(env, owner)
        .executed
        .entry(hash(image.id()))
        .or_insert_with(|| adv_device::surface::Region::empty((image.width(), image.height())))
        .add(area.into());
    let vertices = match operation {
        Operation::Draw(d) => d
            .programmable
            .as_ref()
            .map_or(d.vertices.len() as u64, |p| {
                u64::from(p.vertex_count) * u64::from(p.instance_count)
            }),
        _ => 0,
    };
    let aspects = match operation {
        Operation::Clear {
            color,
            depth,
            stencil,
            ..
        } => {
            u8::from(color.is_some())
                | u8::from(depth.is_some()) << 1
                | u8::from(stencil.is_some()) << 2
        }
        _ => 7,
    };
    let color = match operation {
        Operation::Draw(d) => d.state.color_write != 0,
        Operation::Clear { color, .. } => color.is_some(),
        _ => true,
    };
    if color {
        let f = frame(env, owner);
        if let Some(readback) = f.readback_targets.get_mut(&image.id()) {
            let area = node
                .and_then(|n| f.damage.write_bounds(n))
                .map_or(area, |r| area.clipped(r));
            readback.damage.add(area.into());
        }
    }
    let repeated = node.is_some_and(|n| {
        frame(env, owner)
            .damage
            .submitted(n, area, aspects, vertices)
    });
    let counters = &mut env.ext.get_mut::<State>().frames;
    counters.submissions += 1;
    counters.submitted_pixels += u64::from(area.w) * u64::from(area.h);
    counters.submitted_vertices += vertices;
    if repeated {
        counters.repeated_vertices += vertices;
    }
}
fn clear_upload_lock(env: &mut Env<'_>, resource: Id, level: Option<u32>, dirty: bool) {
    if let Some(Payload::Resource(r)) = com(env).get_identity_mut(&mut env.sys.objects, resource) {
        if let Some(n) = level {
            if let Some(l) = r.levels.get_mut(n as usize) {
                l.lock = None;
                if dirty {
                    l.mark_all();
                }
            }
        } else {
            r.lock = None;
        }
    }
    if dirty {
        resources::invalidate_hot_texture(env, resource);
    }
}
fn release_upload(env: &mut Env<'_>, owner: Id, resource: Id, level: Option<u32>, dirty: bool) {
    let held = &mut frame(env, owner).held_uploads;
    if let Some(n) = held.iter().position(|v| *v == (resource, level)) {
        held.swap_remove(n);
        clear_upload_lock(env, resource, level, dirty);
    }
}
pub(super) fn unlock(env: &mut Env<'_>, owner: Id, token: u64) {
    let Some(Payload::Device(d)) = com(env).get_identity_mut(&mut env.sys.objects, owner) else {
        unreachable!()
    };
    if d.frame.active == Some(token) {
        if d.queries.active != 0 {
            query_pixels(&d.frame, &mut d.queries);
        }
        d.queries.submissions += 1;
        let f = &mut d.frame;
        let held = std::mem::take(&mut f.held_uploads);
        f.active = None;
        // Commands a readback did not need stay recorded, in order; other
        // flushes end with nothing pending.
        let selective = f.select.take().is_some();
        let keep: Vec<bool> = f
            .commands
            .iter()
            .map(|op| selective && !matches!(op, Operation::Consumed))
            .collect();
        let mut n = 0;
        f.commands.retain(|_| {
            n += 1;
            keep[n - 1]
        });
        let mut n = 0;
        f.nodes.retain(|_| {
            n += 1;
            keep[n - 1]
        });
        let mut n = 0;
        f.vertex_ranges.retain(|_| {
            n += 1;
            keep[n - 1]
        });
        f.passes.retain(&keep);
        d.queries.scanned = f.commands.len();
        f.head = 0;
        f.part = 0;
        if f.commands.is_empty() {
            f.bytes = 0;
            f.vertices.clear();
        }
        for (resource, level) in held {
            clear_upload_lock(env, resource, level, true);
        }
    }
}
pub(super) fn cancel(env: &mut Env<'_>, operation: &Operation) {
    if let Operation::Flush { owner, token, .. } | Operation::Present { owner, token, .. } =
        operation
        && matches!(
            com(env).get_identity(&env.sys.objects, *owner),
            Some(Payload::Device(_))
        )
    {
        let f = frame(env, *owner);
        if f.active == Some(*token) {
            f.active = None;
            f.partial = true;
            f.passes.truncate(f.commands.len());
            f.last = None;
            f.last_copy = None;
            f.forget_damage();
            f.select = None;
            f.rotate = false;
            f.verify_image = None;
            f.verdict = None;
            f.presented = None;
            if let Some(ticket) = f.readback.take() {
                ticket.cancel();
            }
            f.job = None;
            let held = std::mem::take(&mut f.held_uploads);
            for (resource, level) in held {
                clear_upload_lock(env, resource, level, true);
            }
        }
    }
}
pub(super) fn present_operation(
    env: &mut Env<'_>,
    owner: Id,
    surface: u32,
    deadline: Option<u64>,
    copy: Option<(Id, presentation::Copy)>,
) -> Outcome {
    let token = token(env, owner);
    dispatch_now(
        env,
        owner,
        Operation::Present {
            owner,
            token,
            surface,
            deadline,
            copy,
        },
    )
}

fn window_presented(env: &Env<'_>, owner: Id) {
    if let Some(Payload::Device(device)) = com(env).get_identity(&env.sys.objects, owner) {
        let provider = env.ext.get::<State>().provider;
        (provider.window_presented)(env.ext, device.parameters.hDeviceWindow);
    }
}

pub(super) fn present(env: &mut Env<'_>, operation: &Operation) -> Result<Completion, Error> {
    let Operation::Present {
        owner,
        token,
        surface,
        deadline,
        copy,
    } = operation
    else {
        unreachable!()
    };
    env.ext
        .get::<State>()
        .gpu
        .as_ref()
        .ok_or_else(|| Error::Unavailable("D3D9 GPU is absent".into()))?
        .check()?;
    enter(env, *owner, *token, Reason::Present)?;
    let presentation = hash((surface, copy.map(|(_, c)| c.key())));
    let copy_baseline = copy.map(|(_, copy)| (*surface, copy));
    let verify = env.ext.get::<State>().verify_frames;
    let serial = (env.ext.get::<State>().provider.presentation_serial)(env.ext);
    let f = frame(env, *owner);
    let repeat = *f.verdict.get_or_insert_with(|| {
        !f.partial
            && f.last == Some(presentation)
            && f.last_serial.is_some()
            && f.last_serial == serial
            && f.contents.unchanged()
    });
    if repeat && !verify {
        let provider = env.ext.get::<State>().provider;
        let serial = (provider.repeat)(env.ext, *surface)?;
        window_presented(env, *owner);
        env.ext.get_mut::<State>().frames.eliminated_frames += 1;
        finish(
            env,
            *owner,
            *token,
            presentation,
            copy_baseline,
            Some(serial),
        );
        return Ok(Completion::Presented(serial, *deadline));
    }
    if !frame(env, *owner).damage.planned {
        let provider = env.ext.get::<State>().provider;
        let image = (provider.target)(env.ext, *surface).unwrap().image;
        let f = frame(env, *owner);
        let force = f.last.is_none();
        let _dead_passes = if !verify && !f.partial && f.head == 0 {
            f.passes.plan()
        } else {
            0
        };
        let slot = hash(image.id());
        let promotions = f.damage.plan(force, slot);
        #[cfg(test)]
        if let Some(plan) = &mut f.test_plan {
            plan.extend(f.nodes.iter().enumerate().map(|(n, node)| {
                (
                    f.passes.skip(n),
                    f.passes.clear_mask(n),
                    node.and_then(|node| f.damage.clip(node).map(<[_]>::to_vec)),
                )
            }));
        }
        f.rotate = !force && !f.partial && f.damage.rotatable(slot);
        if verify && copy.is_none() {
            f.verify_image = Some(image.clone());
        }
        let full = adv_device::surface::Rect {
            x: 0,
            y: 0,
            w: image.width(),
            h: image.height(),
        };
        f.output_slot = Some(slot);
        let size = (full.w, full.h);
        let delta = f.damage.changed.get(&slot);
        f.verify_region = Some(if let Some((_, copy)) = copy {
            // The private front survives intervening publications. The surface
            // provider accounts separately for a change of published source.
            copy.damage(
                f.last_copy.map(|(_, copy)| copy),
                delta,
                !force
                    && !f.partial
                    && delta.is_some()
                    && f.last_copy.is_some_and(|(old, _)| old == *surface),
            )
        } else if force {
            adv_device::surface::Region::whole(size)
        } else {
            delta
                .cloned()
                .unwrap_or_else(|| adv_device::surface::Region::empty(size))
        });
        trace::present(
            f.partial,
            force,
            copy.is_some(),
            promotions,
            &f.damage.execution,
        );
        let whole = if force {
            Some(1)
        } else if f.verify_region.as_ref().is_some_and(|r| r.is_whole()) {
            Some(if f.partial {
                0
            } else if copy.is_some() {
                2
            } else {
                3
            })
        } else {
            None
        };
        let executed = f.damage.execution.get(&slot).filter(|r| !r.is_whole());
        let clipped = executed.is_some();
        let area = executed.map_or(u64::from(full.w) * u64::from(full.h), |r| r.area());
        let fraction = area as f64 / (f64::from(full.w) * f64::from(full.h));
        let counters = &mut env.ext.get_mut::<State>().frames;
        #[cfg(feature = "d3d9-counters")]
        {
            counters.eliminated_passes += _dead_passes;
        }
        if let Some(cause) = whole {
            counters.whole[cause] += 1;
        }
        counters.intermediate_promotions += promotions[0];
        counters.promotions += promotions[1];
        if verify || !clipped {
            counters.full_executions += 1;
        } else {
            counters.clipped_executions += 1;
        }
        counters.clipped_area_sum += fraction;
    }
    flush(env, *owner, *token, Reason::Present)?;
    if matches!(com(env).get_identity(&env.sys.objects,*owner),Some(Payload::Device(d)) if d.gpu_lost)
    {
        unlock(env, *owner, *token);
        return Err(Error::Device("recorded D3D9 device lost".into()));
    }
    if frame(env, *owner).presented.is_none() {
        env.ext
            .get::<State>()
            .gpu
            .as_ref()
            .ok_or_else(|| Error::Unavailable("D3D9 GPU is absent".into()))?
            .admit_present()?;
        let completion = execute_gpu(env, operation)?;
        window_presented(env, *owner);
        let Completion::Presented(serial, _) = completion else {
            unreachable!()
        };
        let visible =
            (env.ext.get::<State>().provider.presentation_serial)(env.ext).map(|_| serial);
        frame(env, *owner).presented = Some((completion, visible));
    }
    if verify {
        verify_pixels(env, *owner, *surface, copy.is_some(), repeat)?;
    }
    let (completion, serial) = frame(env, *owner).presented.take().unwrap();
    finish(env, *owner, *token, presentation, copy_baseline, serial);
    Ok(completion)
}
fn finish(
    env: &mut Env<'_>,
    owner: Id,
    token: u64,
    presentation: u128,
    copy: Option<(u32, presentation::Copy)>,
    serial: Option<u64>,
) {
    let f = frame(env, owner);
    let delta = f.output_slot.and_then(|s| f.damage.changed.get(&s));
    let changed = delta.map_or(0, crate::record::damage::area);
    let executed = f.output_slot.and_then(|s| f.executed.get(&s)).cloned();
    let execution_area = executed.as_ref().map_or(0, crate::record::damage::area);
    let extra = match (executed, delta) {
        (Some(mut r), Some(delta)) => {
            r.intersect(delta);
            execution_area - crate::record::damage::area(&r)
        }
        _ => execution_area,
    };
    trace::work(&f.damage.work, f.damage.planned);
    let counters = &mut env.ext.get_mut::<State>().frames;
    counters.presents += 1;
    counters.changed_pixels += changed;
    counters.executed_pixels += execution_area;
    counters.extra_pixels += extra;
    trace::frame_end();
    unlock(env, owner, token);
    let f = frame(env, owner);
    f.damage.finish();
    f.rotate = false;
    f.verify_image = None;
    f.verify_region = None;
    f.output_slot = None;
    f.executed.clear();
    f.last = Some(presentation);
    f.last_copy = copy;
    f.last_serial = serial;
    f.contents.reset();
    #[cfg(test)]
    f.command_identities.clear();
    f.reuse.finish();
    f.partial = false;
    f.verdict = None;
}
fn verify_pixels(
    env: &mut Env<'_>,
    owner: Id,
    surface: u32,
    copy: bool,
    repeat: bool,
) -> Result<(), Error> {
    use adv_sync::background::{Priority, Task};
    let state = env.ext.get::<State>();
    let sender = state.sender.clone().ok_or_else(|| {
        Error::Unavailable("frame verification requires the background service".into())
    })?;
    let wake = state.wake.clone();
    let provider = state.provider;
    if frame(env, owner).readback.is_none() {
        let surface = if copy {
            match com(env).get_identity(&env.sys.objects, owner) {
                Some(Payload::Device(d)) => d.front.unwrap(),
                _ => unreachable!(),
            }
        } else {
            surface
        };
        let image = if let Some(image) = frame(env, owner).verify_image.clone() {
            image
        } else {
            (provider.target)(env.ext, surface)
                .ok_or_else(|| Error::Invalid("verification image missing".into()))?
                .image
        };
        let mut ticket = env
            .ext
            .get_mut::<State>()
            .gpu
            .as_mut()
            .unwrap()
            .readback(&image, wake)?;
        let job = Task::new(ticket.take_completion_job().unwrap())
            .finish_on_shutdown()
            .without_wake();
        let f = frame(env, owner);
        f.readback = Some(ticket);
        f.job = Some(job);
    }
    if let Some(job) = frame(env, owner).job.take() {
        match sender.borrow_mut().try_submit(Priority::Urgent, job) {
            Ok(_) => {}
            Err(std::sync::mpsc::TrySendError::Full(job)) => {
                frame(env, owner).job = Some(job);
                return Err(Error::Busy);
            }
            Err(_) => return Err(Error::Device("frame verification service stopped".into())),
        }
    }
    let f = frame(env, owner);
    let ticket = f.readback.as_mut().unwrap();
    let Some(pixels) = ticket.poll() else {
        return Err(Error::Busy);
    };
    let pixels = (ticket.width, ticket.height, pixels?);
    let region = if repeat {
        adv_device::surface::Region::empty((pixels.0, pixels.1))
    } else {
        f.verify_region.clone().unwrap()
    };
    let mismatch = f.previous.as_ref().is_some_and(|previous| {
        previous.0 == pixels.0
            && previous.1 == pixels.1
            && pixels
                .2
                .as_chunks::<4>()
                .0
                .iter()
                .zip(previous.2.as_chunks::<4>().0.iter())
                .enumerate()
                .any(|(n, (a, b))| {
                    let (x, y) = (n as u32 % pixels.0, n as u32 / pixels.0);
                    a != b
                        && !region.intersects(adv_device::surface::Rect { x, y, w: 1, h: 1 }.into())
                })
    });
    f.previous = Some(pixels);
    f.readback = None;
    if mismatch {
        env.ext.get_mut::<State>().frames.verification_mismatches += 1;
        log::warn!("D3D9 frame pixel verification mismatch outside damage");
    }
    Ok(())
}

#[cfg(test)]
mod tests;

#[cfg(test)]
impl Recording {
    pub(super) fn force_limit(&mut self) {
        self.bytes = 64 * 1024 * 1024;
    }
}

#[cfg(test)]
pub(super) fn command_keys(env: &mut Env<'_>, pointer: u32) -> Vec<u128> {
    let owner = com(env)
        .identity::<Payload>(&env.sys.objects, pointer)
        .unwrap();
    let commands = std::mem::take(&mut frame(env, owner).commands);
    let mut keys = Vec::new();
    for operation in commands {
        let prepared = key::prepare(env, owner, operation).ok().unwrap();
        keys.push(prepared.key);
        frame(env, owner).commands.push(prepared.operation);
    }
    keys
}
