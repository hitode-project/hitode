//! Deferred D3D9 commands keep their own data and image leases. Content tracks
//! logical COM subresources, not rotating backend allocations. Complete clears
//! replace dependencies; partial clears and draws retain their previous contents.
//! A flush owns the device until its Present/readback finishes. Busy restores
//! only the unexecuted command, so continuations never replay successful prefixes.
use super::*;
pub use crate::record::Counters;
use crate::record::{self, Cursor, Front as _, ResourceKey as Content};
#[cfg(feature = "d3d9-counters")]
use crate::record::{lookup, lookups};
use rustc_hash::FxHashMap as HashMap;
pub(super) mod damage;
mod encode;
mod fast;
mod key;
mod reuse;
pub mod trace;
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
    Query,
}
impl crate::record::Front for Payload {
    type Payload = Operation;
    type Sink<'a> = Env<'a>;
    type Completion = Completion;
    type Error = Error;
    type Boundary = (Reason, Option<Content>);

    fn encode(env: &mut Env<'_>, operation: &Operation) -> Result<Completion, Error> {
        execute_gpu(env, operation)
    }

    fn reason((reason, target): Self::Boundary) -> record::Reason {
        match reason {
            Reason::Readback => target.map_or(record::Reason::Wait, record::Reason::Readback),
            Reason::Query => record::Reason::Query,
            Reason::Lock => record::Reason::Map,
            Reason::Wait => record::Reason::Wait,
            Reason::Limit => record::Reason::CommandLimit,
            Reason::Dc | Reason::Reset => record::Reason::Barrier,
            Reason::Release => record::Reason::Release,
            Reason::Present => unreachable!("present planning needs the output slot"),
        }
    }
}

struct Recorded {
    operation: Operation,
    vertices: Option<std::ops::Range<usize>>,
}

struct ReadbackTarget {
    source: Content,
    destination: u32,
    storage: u32,
    version: Option<u128>,
    damage: adv_device::surface::Region,
    in_flight: Option<u32>,
}

#[derive(Default)]
pub(super) struct Recording {
    core: record::Recording<Recorded>,
    plan: Option<record::Plan<Recorded>>,
    #[cfg(test)]
    separate_draws: bool,
    #[cfg(test)]
    general_draws: bool,
    fast: fast::Cache,
    vertices: Vec<u8>,
    /// The resource version the next readback observes.
    observe: Option<Content>,
    reuse: reuse::Cache,
    readback_targets: HashMap<adv_device::gpu::ImageId, ReadbackTarget>,
    verify_image: Option<adv_device::gpu::Image>,
    baseline: Option<adv_device::gpu::Image>,
    pub(super) rotate: bool,
    held_uploads: Vec<(Id, Option<u32>)>,
    binding_cache: Option<key::Bindings>,
    active: Option<u64>,
    next: u64,
    last_copy: Option<(u32, presentation::Copy)>,
    presented: Option<(Completion, Option<u64>)>,
    readback: Option<adv_device::gpu::Readback>,
    job: Option<adv_sync::background::Task>,
}
pub(super) fn forget(env: &mut Env<'_>, owner: Id, id: Option<Id>, pointer: u32) {
    if let Some(Payload::Device(d)) = com(env).get_identity_mut(&mut env.sys.objects, owner) {
        let f = &mut d.frame;
        let allocation = id.map(Id::key);
        if let Some(allocation) = allocation {
            f.core.forget(allocation);
        }
        f.binding_cache = None;
        f.fast.clear();
        f.reuse.invalidate();
        f.readback_targets
            .retain(|_, r| Some(r.source.0) != allocation && r.destination != pointer);
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
    let version = f.core.contents.get(&source).copied();
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
    f.core.verify_region.clone()
}
fn frame<'a>(env: &'a mut Env<'_>, owner: Id) -> &'a mut Recording {
    let Some(Payload::Device(d)) = com(env).get_identity_mut(&mut env.sys.objects, owner) else {
        unreachable!("retained recording device")
    };
    &mut d.frame
}
fn frame_and_counters<'a>(
    env: &'a mut Env<'_>,
    owner: Id,
) -> (&'a mut Recording, &'a mut Counters) {
    let Some(Payload::Device(d)) = com(env).get_identity_mut(&mut env.sys.objects, owner) else {
        unreachable!("retained recording device")
    };
    (&mut d.frame, &mut env.ext.get_mut::<State>().frames)
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
    for command in f.core.commands[queries.scanned..].iter().flatten() {
        if let Operation::Draw(draw) = &command.operation {
            let area = u64::from(draw.target.image.width()) * u64::from(draw.target.image.height());
            queries.pixels = queries.pixels.wrapping_add(area);
        }
    }
    queries.scanned = f.core.commands.len();
    queries.pixels
}
pub(super) fn query_pending(f: &Recording) -> bool {
    !f.core.commands.is_empty() || f.active.is_some()
}
pub(super) fn query_flush(env: &mut Env<'_>, owner: Id) -> Result<(), Error> {
    if frame(env, owner).active.is_some() {
        return Err(Error::Busy);
    }
    if frame(env, owner).core.commands.is_empty() {
        return Ok(());
    }
    let token = token(env, owner);
    let result = flush(env, owner, token, Reason::Query);
    if matches!(result, Err(Error::Busy)) {
        cancel(
            env,
            &Operation::Flush {
                owner,
                token,
                reason: Reason::Query,
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
    ) && (!frame(env, owner).core.commands.is_empty()
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
    if f.active.is_some() || f.core.commands.is_empty() {
        return true;
    }
    let verify = env.ext.get::<State>().verify_frames;
    let f = frame(env, owner);
    let pending = f.core.observes(content, verify);
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
    if frame(env, owner).core.commands.is_empty() && frame(env, owner).active.is_none() {
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
    let upload = match &p.operation {
        Operation::Upload {
            resource, level, ..
        } => Some((*resource, *level)),
        _ => None,
    };
    if d.gpu_lost {
        if let Some((resource, level)) = upload {
            clear_upload_lock(env, resource, level, false);
        }
        return Outcome::Return(0);
    }
    let f = frame(env, owner);
    let effects = p.damage(target.as_ref());
    let mut operation = p.operation.clone();
    if let (Some(target), Operation::Draw(draw)) = (target, &mut operation) {
        draw.target = target;
    }
    let is_draw = matches!(&operation, Operation::Draw(_));
    let fixed = matches!(&operation, Operation::Draw(d) if d.fixed_vertex.is_some() || d.programmable.is_none());
    let captured = vertices.is_some();
    let detail = |index, reused, contents: &crate::record::Contents| {
        trace::command(index, p, reused, contents)
    };
    let admitted = f.core.admit(record::Op {
        payload: Recorded { operation, vertices },
        effects,
        bytes: p.bytes,
        fresh: matches!(&p.operation, Operation::Upload { pixels, .. } if matches!(pixels.key(), UploadKey::Admission(_))),
        reuse: entry.map(|entry| &entry.saved),
        trace: trace::capturing().then_some(&detail as &dyn Fn(_, _, &crate::record::Contents)),
    });
    if let Some((resource, level)) = upload {
        // A limit flush keeps the upload reserved while it can suspend.
        if admitted.at_limit() {
            f.held_uploads.push((resource, level));
        } else {
            clear_upload_lock(env, resource, level, false);
        }
    }
    #[cfg(feature = "d3d9-counters")]
    let work = (!admitted.reused() && is_draw).then_some((
        (record::Recording::<Recorded>::effect_bytes(p.reads.len())
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
    let counters = &mut env.ext.get_mut::<State>().frames;
    counters.recorded_commands += 1;
    counters.fast_path_draws += u64::from(captured);
    counters.general_path_draws += u64::from(is_draw && !captured);
    counters.fixed_draws += u64::from(fixed);
    if is_draw {
        counters.draw_key_hashes += admitted.key_hashes + u64::from(captured);
    }
    if admitted.at_limit() {
        let outcome = barrier(env, owner, Reason::Limit, |_| Outcome::Return(0));
        // Parking allocation failure rejects the incoming command. Successful
        // prefixes stay committed; older accepted commands retain their order.
        if matches!(outcome, Outcome::Return(OUT_OF_MEMORY)) {
            let f = frame(env, owner);
            let recorded = f.core.reject(admitted);
            if let Some(vertices) = recorded.vertices {
                f.vertices.truncate(vertices.start);
            }
            f.readback_targets.clear();
            let len = f.core.commands.len();
            if let Some(Payload::Device(d)) = com(env).get_identity_mut(&mut env.sys.objects, owner)
                && d.queries.scanned > len
            {
                if let Operation::Draw(draw) = recorded.operation {
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
            let (f, counters) = frame_and_counters(env, owner);
            f.active = Some(token);
            let counter_reason = if matches!(reason, Reason::Query) {
                Reason::Wait
            } else {
                reason
            };
            trace::flush(counter_reason, f.core.commands.len());
            if !matches!(reason, Reason::Present) {
                let reason = Payload::reason((reason, f.observe.take()));
                if matches!(
                    reason,
                    record::Reason::Map | record::Reason::Barrier | record::Reason::Release
                ) {
                    f.readback_targets.clear();
                }
                f.plan = Some(f.core.plan(reason));
                f.reuse.invalidate();
            }
            counters.flushes[counter_reason as usize] += 1;
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
        Payload::encode(env, &operation)?;
        let image = damage::image(&operation).unwrap();
        let f = frame(env, owner);
        f.baseline = Some(image.clone());
        f.readback_targets.remove(&image.id());
    }
    loop {
        let f = frame(env, owner);
        if f.core.cursor.ops == f.core.commands.len() {
            let surface = match com(env).get_identity(&env.sys.objects, owner) {
                Some(Payload::Device(d)) => d.surface,
                _ => unreachable!(),
            };
            frame(env, owner).baseline =
                (env.ext.get::<State>().provider.target)(env.ext, surface).map(|t| t.image);
            return Ok(Completion::Command);
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
        let head = f.core.cursor.ops;
        let Some(planned) = f.plan.as_ref().unwrap().at(&f.core, head) else {
            f.core.retire(
                f.plan.as_ref().unwrap(),
                Cursor {
                    ops: head + 1,
                    scissors: 0,
                },
            );
            continue;
        };
        let (node, scissors, mask, retains_aspects) = (
            planned.node,
            planned.scissors,
            planned.clear_mask,
            planned.retains_aspects,
        );
        let mut command = f.core.commands[head].take().unwrap();
        let original = if let Operation::Clear {
            target,
            rectangle,
            color,
            depth,
            stencil,
        } = &mut command.operation
        {
            let rest = (
                color.filter(|_| mask & 1 == 0),
                depth.filter(|_| mask & 2 == 0),
                stencil.filter(|_| mask & 4 == 0),
            );
            let original =
                (retains_aspects && rest != (None, None, None)).then(|| Operation::Clear {
                    target: target.clone(),
                    rectangle: *rectangle,
                    color: *color,
                    depth: *depth,
                    stencil: *stencil,
                });
            if original.is_some() {
                f.core.commands[head] = Some(Recorded {
                    operation: Operation::Clear {
                        target: target.clone(),
                        rectangle: *rectangle,
                        color: rest.0,
                        depth: rest.1,
                        stencil: rest.2,
                    },
                    vertices: None,
                });
            }
            *color = color.filter(|_| mask & 1 != 0);
            *depth = depth.filter(|_| mask & 2 != 0);
            *stencil = stencil.filter(|_| mask & 4 != 0);
            original
        } else {
            None
        };
        // The retained template survives a busy draw. Only the encoder
        // materializes the captured vertices.
        let captured =
            if let (Some(range), Operation::Draw(draw)) = (&command.vertices, &command.operation) {
                let mut draw = draw.clone();
                fast::materialize(&mut draw, &f.vertices[range.clone()]);
                Some(Operation::Draw(draw))
            } else {
                None
            };
        let operation = captured.as_ref().unwrap_or(&command.operation);
        let mut cursor = f.core.cursor;
        let result = if let Some(scissors) = &scissors {
            let remaining = &scissors[cursor.scissors..];
            if let Operation::Draw(draw) = operation {
                if remaining.is_empty() {
                    Ok(Completion::Command)
                } else {
                    let result = execute_draw_clips(env, draw, remaining);
                    if result.is_ok() {
                        for &clip in remaining {
                            submitted(env, owner, node, &command.operation, Some(clip));
                        }
                    }
                    result
                }
            } else {
                let mut result = Ok(Completion::Command);
                for &clip in remaining {
                    env.ext
                        .get_mut::<State>()
                        .gpu
                        .as_mut()
                        .unwrap()
                        .set_clip(Some(clip));
                    result = Payload::encode(env, operation);
                    env.ext
                        .get_mut::<State>()
                        .gpu
                        .as_mut()
                        .unwrap()
                        .set_clip(None);
                    if result.is_err() {
                        break;
                    }
                    submitted(env, owner, node, &command.operation, Some(clip));
                    cursor.scissors += 1;
                }
                result
            }
        } else {
            let result = Payload::encode(env, operation);
            if result.is_ok() {
                submitted(env, owner, node, &command.operation, None);
            }
            result
        };
        match result {
            Err(Error::Busy) => {
                let f = frame(env, owner);
                if let Some(original) = original {
                    command.operation = original;
                }
                f.core.commands[head] = Some(command);
                f.core.retire(f.plan.as_ref().unwrap(), cursor);
                return Err(Error::Busy);
            }
            Err(Error::Device(_)) => return Ok(lose(env, owner)),
            result => {
                if let Operation::Upload {
                    resource, level, ..
                } = &command.operation
                {
                    release_upload(env, owner, *resource, *level, false);
                }
                let f = frame(env, owner);
                f.core.retire(
                    f.plan.as_ref().unwrap(),
                    Cursor {
                        ops: head + 1,
                        scissors: 0,
                    },
                );
                result?;
            }
        }
    }
}
fn lose(env: &mut Env<'_>, owner: Id) -> Completion {
    if let Some(Payload::Device(d)) = com(env).get_identity_mut(&mut env.sys.objects, owner) {
        d.gpu_lost = true;
        d.frame.core.lose();
        d.frame.vertices.clear();
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
                .and_then(|n| f.core.damage.write_bounds(n))
                .map_or(area, |r| area.clipped(r));
            readback.damage.add(area.into());
        }
    }
    let (f, counters) = frame_and_counters(env, owner);
    f.core.submitted(
        node,
        (hash(image.id()), (image.width(), image.height())),
        area,
        aspects,
        vertices,
        counters,
    );
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
        f.core.end(f.plan.take());
        d.queries.scanned = f.core.commands.len();
        if f.core.commands.is_empty() {
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
            f.core.cancel();
            f.last_copy = None;
            f.readback_targets.clear();
            f.plan = None;
            f.rotate = false;
            f.verify_image = None;
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
    let repeat = f.core.repeated(presentation, serial);
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
    if frame(env, *owner).plan.is_none() {
        let provider = env.ext.get::<State>().provider;
        let image = (provider.target)(env.ext, *surface).unwrap().image;
        let slot = hash(image.id());
        let size = (image.width(), image.height());
        let (f, counters) = frame_and_counters(env, *owner);
        let plan = f.core.plan(record::Reason::Present { slot, verify });
        f.rotate = f.core.rotatable(slot);
        if verify && copy.is_none() {
            f.verify_image = Some(image);
        }
        let force = f.core.force_present();
        let last_copy = f.last_copy;
        f.core.present_damage(
            &plan,
            size,
            copy.is_some(),
            |delta, current| {
                if let Some((_, copy)) = copy {
                    // The private front survives intervening publications. The
                    // surface provider handles a change of published source.
                    copy.damage(
                        last_copy.map(|(_, copy)| copy),
                        delta,
                        current
                            && delta.is_some()
                            && last_copy.is_some_and(|(old, _)| old == *surface),
                    )
                } else if force {
                    adv_device::surface::Region::whole(size)
                } else {
                    delta
                        .cloned()
                        .unwrap_or_else(|| adv_device::surface::Region::empty(size))
                }
            },
            counters,
        );
        f.plan = Some(plan);
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
        let completion = Payload::encode(env, operation)?;
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
    let (f, counters) = frame_and_counters(env, owner);
    f.core.finish_metrics(counters);
    unlock(env, owner, token);
    let f = frame(env, owner);
    f.core.finish(presentation, serial);
    f.rotate = false;
    f.verify_image = None;
    f.last_copy = copy;
    f.reuse.finish();
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
    let (f, counters) = frame_and_counters(env, owner);
    let ticket = f.readback.as_mut().unwrap();
    let Some(pixels) = ticket.poll() else {
        return Err(Error::Busy);
    };
    let pixels = (ticket.width, ticket.height, pixels?);
    f.readback = None;
    if f.core.verify(pixels, repeat, counters) {
        log::warn!("D3D9 frame pixel verification mismatch outside damage");
    }
    Ok(())
}

#[cfg(test)]
mod tests;

#[cfg(test)]
impl Recording {
    fn commands(&self) -> impl DoubleEndedIterator<Item = &Operation> {
        self.core
            .commands
            .iter()
            .flatten()
            .map(|command| &command.operation)
    }

    pub(super) fn force_limit(&mut self) {
        self.core.set_bytes(64 * 1024 * 1024);
    }
}

#[cfg(test)]
pub(super) fn command_keys(env: &mut Env<'_>, pointer: u32) -> Vec<u128> {
    let owner = com(env)
        .identity::<Payload>(&env.sys.objects, pointer)
        .unwrap();
    let commands = std::mem::take(&mut frame(env, owner).core.commands);
    let mut keys = Vec::new();
    for mut command in commands.into_iter().flatten() {
        let prepared = key::prepare(env, owner, command.operation).ok().unwrap();
        keys.push(prepared.key);
        command.operation = prepared.operation;
        frame(env, owner).core.commands.push(Some(command));
    }
    keys
}
