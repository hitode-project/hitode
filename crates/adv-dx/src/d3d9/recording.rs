//! Deferred D3D9 commands keep their own data and image leases. Content tracks
//! logical COM subresources, not rotating backend allocations. Complete clears
//! replace dependencies; partial clears and draws retain their previous contents.
//! A flush owns the device until its Present/readback finishes. Busy restores
//! only the unexecuted command, so continuations never replay successful prefixes.
use super::*;
use std::collections::HashMap;
mod damage;
mod key;
pub(super) use key::fixed as fixed_key;
pub(super) use key::hash;
pub(super) use key::rect;
pub(super) fn draw_key(env: &Env<'_>, pointer: u32) -> u128 {
    let d = device(env, pointer).unwrap();
    let constants = |id: Option<Id>, c: &shaders::Constants| {
        use std::hash::Hash;
        let mut h = xxhash_rust::xxh3::Xxh3::new();
        if let Some(usage) = id
            .and_then(|id| shaders::bound(env, id))
            .and_then(|s| s.translation.as_ref().ok())
            .map(|t| t.constants)
        {
            for n in usage.ranges().flat_map(|r| r.start / 16..r.end / 16) {
                match n {
                    0..256 => c.floats[n].map(f32::to_bits).hash(&mut h),
                    256..272 => c.integers[n - 256].hash(&mut h),
                    272..288 => c.booleans[n - 272].hash(&mut h),
                    _ => {}
                }
            }
        }
        h.digest128()
    };
    hash((
        fixed_key(&d.fixed),
        d.shaders.vertex,
        d.shaders.pixel,
        d.buffers.declaration,
        constants(d.shaders.vertex, &d.shaders.vertex_constants),
        constants(d.shaders.pixel, &d.shaders.pixel_constants),
    ))
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Content(Id, u32, u8, u32);
#[derive(Clone, Copy)]
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
#[derive(Clone, Copy, Debug, Default)]
pub struct Counters {
    pub recorded_commands: u64,
    /// Present, readback, lock, DC, Reset, Release, limit, existing GPU wait.
    pub flushes: [u64; 8],
    pub eliminated_frames: u64,
    pub verification_mismatches: u64,
    pub full_executions: u64,
    pub clipped_executions: u64,
    pub clipped_area_sum: f64,
}
#[derive(Default)]
pub(super) struct Recording {
    commands: Vec<Operation>,
    damage: damage::Analysis,
    verify_region: Option<adv_device::surface::Rect>,
    verify_image: Option<adv_device::gpu::Image>,
    baseline: Option<adv_device::gpu::Image>,
    pub(super) rotate: bool,
    held_uploads: Vec<(Id, Option<u32>)>,
    head: usize,
    bytes: usize,
    key: u128,
    contents: HashMap<Content, u128>,
    base: HashMap<Content, u128>,
    upload: u128,
    active: Option<u64>,
    next: u64,
    partial: bool,
    last: Option<u128>,
    last_serial: Option<u64>,
    verdict: Option<bool>,
    presented: Option<(Completion, Option<u64>)>,
    previous: Option<(u32, u32, Vec<u8>)>,
    readback: Option<adv_device::gpu::Readback>,
    job: Option<adv_sync::background::Task>,
}
pub(super) fn admits_leaf(env: &Env<'_>, pointer: u32, bytes: usize) -> bool {
    device(env, pointer).is_some_and(|d| {
        d.frame.active.is_none()
            && d.frame.commands.len() + 1 < 4096
            && d.frame.bytes + bytes < 64 * 1024 * 1024
    })
}
pub(super) fn forget(env: &mut Env<'_>, owner: Id, pointer: u32) {
    if let Some(Payload::Device(d)) = com(env).get_identity_mut(&mut env.sys.objects, owner) {
        let f = &mut d.frame;
        let before = f.contents.len();
        f.contents.retain(|k, _| k.3 != pointer);
        f.damage.forget(pointer);
        if f.contents.len() != before {
            f.partial = true;
            f.last = None;
        }
    }
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
        _ => id,
    }
}
fn token(env: &mut Env<'_>, owner: Id) -> u64 {
    let f = frame(env, owner);
    f.next = f.next.checked_add(1).expect("flush token exhausted");
    f.next
}
pub(super) fn has_work(env: &mut Env<'_>, owner: Id) -> bool {
    matches!(
        com(env).get_identity(&env.sys.objects, owner),
        Some(Payload::Device(_))
    ) && (!frame(env, owner).commands.is_empty()
        || frame(env, owner).active.is_some()
        || restore_operation(env, owner).is_some())
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
    if !matches!(
        com(env).get_identity(&env.sys.objects, owner),
        Some(Payload::Device(_))
    ) {
        return Outcome::Return(INVALID);
    }
    let lost = matches!(com(env).get_identity(&env.sys.objects,owner),Some(Payload::Device(d)) if d.gpu_lost);
    let limit = {
        let f = frame(env, owner);
        f.commands.len() + 1 >= 4096 || f.bytes + p.bytes >= 64 * 1024 * 1024
    };
    // Ordinary admission unlocks immediately. A limit flush retains its lock
    // until execution, preserving the existing Busy API reservation.
    if (!limit || lost)
        && let Operation::Upload {
            resource, level, ..
        } = &p.operation
    {
        if let Some(Payload::Resource(r)) =
            com(env).get_identity_mut(&mut env.sys.objects, *resource)
        {
            if let Some(n) = level {
                r.levels[*n as usize].lock = None
            } else {
                r.lock = None
            }
        }
    }
    if lost {
        return Outcome::Return(0);
    }
    let f = frame(env, owner);
    let previous = limit.then(|| {
        (
            f.key,
            p.bytes,
            p.writes
                .iter()
                .map(|(k, _)| (*k, f.contents.get(k).copied(), f.base.contains_key(k)))
                .collect::<Vec<_>>(),
        )
    });
    let command = hash((
        p.key,
        p.reads
            .iter()
            .map(|k| (*k, f.contents.get(k).copied().unwrap_or(0)))
            .collect::<Vec<_>>(),
    ));
    f.damage.record(&p, command, &f.contents);
    for (k, overwrite) in p.writes {
        let old = f.contents.get(&k).copied().unwrap_or(0);
        f.base.entry(k).or_insert(old);
        let next = if matches!(p.operation, Operation::Upload { .. }) {
            f.upload = f.upload.checked_add(1).expect("upload content exhausted");
            f.upload
        } else if overwrite {
            hash((k, command))
        } else {
            hash((old, command))
        };
        f.contents.insert(k, next);
    }
    if limit
        && let Operation::Upload {
            resource, level, ..
        } = &p.operation
    {
        f.held_uploads.push((*resource, *level));
    }
    f.key = hash((f.key, command));
    f.bytes += p.bytes;
    f.commands.push(p.operation);
    let limit = f.commands.len() >= 4096 || f.bytes >= 64 * 1024 * 1024;
    env.ext.get_mut::<State>().frames.recorded_commands += 1;
    if limit {
        let outcome = barrier(env, owner, Reason::Limit, |_| Outcome::Return(0));
        // Parking allocation failure rejects the incoming command. Successful
        // prefixes stay committed; older accepted commands retain their order.
        if matches!(outcome, Outcome::Return(OUT_OF_MEMORY)) {
            let (key, bytes, contents) = previous.unwrap();
            let f = frame(env, owner);
            assert!(f.head < f.commands.len());
            f.commands.pop();
            f.damage.invalidate();
            f.bytes -= bytes;
            f.key = key;
            for (k, old, had_base) in contents {
                if let Some(old) = old {
                    f.contents.insert(k, old);
                } else {
                    f.contents.remove(&k);
                }
                if !had_base {
                    f.base.remove(&k);
                }
            }
            env.ext.get_mut::<State>().frames.recorded_commands -= 1;
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
            if !matches!(reason, Reason::Present) {
                frame(env, owner).damage.invalidate();
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
    if !frame(env, owner).rotate
        && let Some(operation) = restore_operation(env, owner)
    {
        execute_gpu(env, &operation)?;
        frame(env, owner).baseline = Some(damage::image(&operation).unwrap().clone());
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
        let operation = std::mem::replace(&mut f.commands[f.head], Operation::Consumed);
        let clip = if env.ext.get::<State>().verify_frames {
            None
        } else {
            damage::image(&operation).and_then(|i| {
                frame(env, owner)
                    .damage
                    .clips
                    .get(&hash(i.view()))
                    .copied()
                    .filter(|r| r.w < i.width() || r.h < i.height())
            })
        };
        env.ext
            .get_mut::<State>()
            .gpu
            .as_mut()
            .unwrap()
            .set_clip(clip);
        let result = execute_gpu(env, &operation);
        env.ext
            .get_mut::<State>()
            .gpu
            .as_mut()
            .unwrap()
            .set_clip(None);
        match result {
            Err(Error::Busy) => {
                let f = frame(env, owner);
                f.commands[f.head] = operation;
                return Err(Error::Busy);
            }
            Err(Error::Device(_)) => {
                if let Some(Payload::Device(d)) =
                    com(env).get_identity_mut(&mut env.sys.objects, owner)
                {
                    d.gpu_lost = true;
                    d.frame.damage.invalidate();
                    d.frame.commands.clear();
                    d.frame.head = 0;
                }
                return Ok(Completion::Command);
            }
            result => {
                if let Operation::Upload {
                    resource, level, ..
                } = &operation
                {
                    release_upload(env, owner, *resource, *level, false);
                }
                let f = frame(env, owner);
                f.head += 1;
                f.partial = true;
                result?;
            }
        }
    }
}
fn clear_upload_lock(env: &mut Env<'_>, resource: Id, level: Option<u32>, dirty: bool) {
    if let Some(Payload::Resource(r)) = com(env).get_identity_mut(&mut env.sys.objects, resource) {
        if let Some(n) = level {
            if let Some(l) = r.levels.get_mut(n as usize) {
                l.lock = None;
                l.dirty |= dirty;
            }
        } else {
            r.lock = None;
        }
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
    let f = frame(env, owner);
    if f.active == Some(token) {
        let held = std::mem::take(&mut f.held_uploads);
        f.active = None;
        f.commands.clear();
        f.head = 0;
        f.bytes = 0;
        for (resource, level) in held {
            clear_upload_lock(env, resource, level, true);
        }
    }
}
pub(super) fn cancel(env: &mut Env<'_>, operation: &Operation) {
    if let Operation::Flush { owner, token, .. } | Operation::Present { owner, token, .. } =
        operation
    {
        if matches!(
            com(env).get_identity(&env.sys.objects, *owner),
            Some(Payload::Device(_))
        ) {
            let f = frame(env, *owner);
            if f.active == Some(*token) {
                f.active = None;
                f.partial = true;
                f.last = None;
                f.damage.invalidate();
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
    let verify = env.ext.get::<State>().verify_frames;
    let serial = (env.ext.get::<State>().provider.presentation_serial)(env.ext);
    let f = frame(env, *owner);
    let repeat = *f.verdict.get_or_insert_with(|| {
        !f.partial
            && f.last == Some(presentation)
            && f.last_serial.is_some()
            && f.last_serial == serial
            && f.base.iter().all(|(k, v)| f.contents.get(k) == Some(v))
    });
    if repeat && !verify {
        let provider = env.ext.get::<State>().provider;
        let serial = (provider.repeat)(env.ext, *surface)?;
        env.ext.get_mut::<State>().frames.eliminated_frames += 1;
        finish(env, *owner, *token, presentation, Some(serial));
        return Ok(Completion::Presented(serial, *deadline));
    }
    if !frame(env, *owner).damage.planned {
        let provider = env.ext.get::<State>().provider;
        let image = (provider.target)(env.ext, *surface).unwrap().image;
        let f = frame(env, *owner);
        let force = f.partial || f.last != Some(presentation);
        f.damage.plan(force);
        f.rotate = !force && f.damage.rotatable(hash(image.view()));
        if verify && copy.is_none() {
            f.verify_image = Some(image.clone());
        }
        let full = adv_device::surface::Rect {
            x: 0,
            y: 0,
            w: image.width(),
            h: image.height(),
        };
        let slot = hash(image.view());
        f.verify_region = Some(if force || copy.is_some() {
            full
        } else {
            f.damage.changed.get(&slot).copied().unwrap_or_default()
        });
        let clip = f.damage.clips.get(&slot).copied().unwrap_or(full);
        let fraction =
            f64::from(clip.w) * f64::from(clip.h) / (f64::from(full.w) * f64::from(full.h));
        let counters = &mut env.ext.get_mut::<State>().frames;
        if verify || clip == full {
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
        let completion = execute_gpu(env, operation)?;
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
    finish(env, *owner, *token, presentation, serial);
    Ok(completion)
}
fn finish(env: &mut Env<'_>, owner: Id, token: u64, presentation: u128, serial: Option<u64>) {
    unlock(env, owner, token);
    let f = frame(env, owner);
    f.damage.finish();
    f.rotate = false;
    f.verify_image = None;
    f.verify_region = None;
    f.last = Some(presentation);
    f.last_serial = serial;
    f.base.clear();
    f.key = 0;
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
        adv_device::surface::Rect::default()
    } else {
        f.verify_region.unwrap()
    };
    let mismatch = f.previous.as_ref().is_some_and(|previous| {
        previous.0 == pixels.0
            && previous.1 == pixels.1
            && pixels
                .2
                .chunks_exact(4)
                .zip(previous.2.chunks_exact(4))
                .enumerate()
                .any(|(n, (a, b))| {
                    let (x, y) = (n as u32 % pixels.0, n as u32 / pixels.0);
                    (x < region.x
                        || x >= region.x + region.w
                        || y < region.y
                        || y >= region.y + region.h)
                        && a != b
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
