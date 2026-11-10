//! Deferred D3D9 commands keep their own data and image leases. Content tracks
//! logical COM subresources, not rotating backend allocations. Complete clears
//! replace dependencies; partial clears and draws retain their previous contents.
//! A flush owns the device until its Present/readback finishes. Busy restores
//! only the unexecuted command, so continuations never replay successful prefixes.
use super::*;
use rustc_hash::FxHashMap as HashMap;
mod damage;
mod key;
mod passes;
mod reuse;
pub mod trace;
pub(super) use key::fixed as fixed_key;
pub(super) use key::hash;
pub(super) use key::rect;
pub(super) use reuse::indexed;
pub(super) fn draw_key(env: &Env<'_>, pointer: u32) -> u128 {
    let d = device(env, pointer).unwrap();
    let constants = |id: Option<Id>, c: &shaders::Constants| {
        c.key(
            id.and_then(|id| shaders::bound(env, id))
                .and_then(|s| s.translation.as_ref().ok())
                .map_or(Default::default(), |t| t.constants),
        )
    };
    hash((
        d.fixed.key(),
        d.shaders.vertex,
        d.shaders.pixel,
        d.buffers.declaration,
        constants(d.shaders.vertex, &d.shaders.vertex_constants),
        constants(d.shaders.pixel, &d.shaders.pixel_constants),
    ))
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Content(Id, u32, u8, u32);
#[derive(Default)]
struct Contents {
    indices: HashMap<Content, usize>,
    slots: Vec<(Content, Option<u128>, Option<u128>)>,
    touched: Vec<usize>,
    vacant: Vec<usize>,
}
impl Contents {
    fn resolve(&mut self, key: Content) -> usize {
        #[cfg(feature = "d3d9-counters")]
        lookup(1);
        *self.indices.entry(key).or_insert_with(|| {
            if let Some(n) = self.vacant.pop() {
                self.slots[n] = (key, None, None);
                n
            } else {
                self.slots.push((key, None, None));
                self.slots.len() - 1
            }
        })
    }
    fn get(&self, key: &Content) -> Option<&u128> {
        #[cfg(feature = "d3d9-counters")]
        lookup(1);
        self.slots.get(*self.indices.get(key)?)?.1.as_ref()
    }
    fn had_base(&self, key: &Content) -> bool {
        #[cfg(feature = "d3d9-counters")]
        lookup(1);
        self.indices
            .get(key)
            .is_some_and(|&n| self.slots[n].2.is_some())
    }
    fn restore(&mut self, key: Content, value: Option<u128>, had_base: bool) {
        let n = self.resolve(key);
        self.slots[n].1 = value;
        if !had_base {
            self.slots[n].2 = None;
        }
    }
    fn unchanged(&self) -> bool {
        self.touched.iter().all(|&n| {
            self.slots[n]
                .2
                .is_none_or(|base| self.slots[n].1 == Some(base))
        })
    }
    fn reset(&mut self) {
        for n in self.touched.drain(..) {
            self.slots[n].2 = None;
        }
    }
    fn forget(&mut self, pointer: u32) -> bool {
        let before = self.indices.len();
        self.indices.retain(|k, &mut n| {
            if k.3 != pointer {
                return true;
            }
            self.slots[n].1 = None;
            self.slots[n].2 = None;
            self.vacant.push(n);
            false
        });
        self.indices.len() != before
    }
    #[cfg(test)]
    fn keys(&self) -> impl Iterator<Item = &Content> {
        self.slots.iter().filter(|s| s.1.is_some()).map(|s| &s.0)
    }
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
#[derive(Clone, Copy, Debug, Default)]
pub struct Counters {
    pub recorded_draws: u64,
    pub recorded_draw_bytes: u64,
    pub recording_lookups: u64,
    pub stateblock_applies: u64,
    pub stateblock_items: u64,
    pub eliminated_passes: u64,
    pub recorded_commands: u64,
    /// Present, readback, lock, DC, Reset, Release, limit, existing GPU wait.
    pub flushes: [u64; 8],
    pub eliminated_frames: u64,
    pub verification_mismatches: u64,
    pub full_executions: u64,
    pub clipped_executions: u64,
    pub clipped_area_sum: f64,
    /// Presents that changed the whole surface, by cause: a flush that
    /// changed contents outside the analysis, a new presentation, a whole
    /// COPY publication, and the analysis itself (a whole change).
    pub whole: [u64; 4],
    /// Final chains planned whole because a late demand reached writes
    /// already planned.
    pub promotions: u64,
    pub intermediate_promotions: u64,
    /// Recorded writes submitted, once per scissor, and the pixels those
    /// submissions covered (a whole write counts its image).
    pub submissions: u64,
    pub submitted_pixels: u64,
    /// Input vertices submitted, including instances and repeated scissors.
    pub submitted_vertices: u64,
    pub repeated_vertices: u64,
    /// Backbuffer cell areas accumulated once per completed present.
    pub changed_pixels: u64,
    pub executed_pixels: u64,
    pub extra_pixels: u64,
}
impl Counters {
    pub fn work_report(&self) -> String {
        if !cfg!(feature = "d3d9-counters") {
            return String::new();
        }
        format!(
            "\nD3D9 work: draws {}; recorded bytes {} ({:.2}/draw); recording lookups {} ({:.2}/draw); StateBlock applies {}; applied items {} ({:.2}/apply); dead frames {}; dead passes {}",
            self.recorded_draws,
            self.recorded_draw_bytes,
            self.recorded_draw_bytes as f64 / self.recorded_draws.max(1) as f64,
            self.recording_lookups,
            self.recording_lookups as f64 / self.recorded_draws.max(1) as f64,
            self.stateblock_applies,
            self.stateblock_items,
            self.stateblock_items as f64 / self.stateblock_applies.max(1) as f64,
            self.eliminated_frames,
            self.eliminated_passes
        )
    }
}
#[cfg(feature = "d3d9-counters")]
thread_local! { static LOOKUPS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) }; }
#[cfg(feature = "d3d9-counters")]
fn lookup(count: u64) {
    LOOKUPS.set(LOOKUPS.get() + count);
}
#[cfg(feature = "d3d9-counters")]
fn lookups() -> u64 {
    LOOKUPS.get()
}
#[derive(Default)]
pub(super) struct Recording {
    commands: Vec<Operation>,
    /// The damage analysis write behind each command, while it knows it.
    nodes: Vec<Option<usize>>,
    /// The content a readback flush observes, and the writes, by content,
    /// that flush executes; the rest wait for presentation.
    observe: Option<Content>,
    select: Option<HashMap<usize, Vec<Content>>>,
    reuse: reuse::Cache,
    passes: passes::Plan,
    damage: damage::Analysis,
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
    key: u128,
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
    pub(super) fn invalidate_streams(&mut self) {
        self.reuse.invalidate_streams();
    }
    /// Drops the damage analysis; pending commands then run whole.
    fn forget_damage(&mut self) {
        self.damage.invalidate();
        self.nodes.fill(None);
    }
}
pub(super) fn forget(env: &mut Env<'_>, owner: Id, pointer: u32) {
    if let Some(Payload::Device(d)) = com(env).get_identity_mut(&mut env.sys.objects, owner) {
        let f = &mut d.frame;
        let changed = f.contents.forget(pointer);
        f.binding_cache = None;
        f.reuse.invalidate();
        f.damage.forget(pointer);
        if changed {
            f.partial = true;
            f.last = None;
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
    append(env, owner, &p, None, None)
}
fn append(
    env: &mut Env<'_>,
    owner: Id,
    p: &key::Prepared,
    target: Option<Target>,
    entry: Option<&reuse::Entry>,
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
            f.key,
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
    let (command, attachment, damage) = if let Some(saved) = shared {
        let slot = hash(target.as_ref().unwrap().image.id());
        f.damage.replay(&saved.damage, slot, &f.contents);
        (saved.command, saved.attachment, saved.damage.clone())
    } else {
        use std::hash::Hash;
        let mut command = xxhash_rust::xxh3::Xxh3::new();
        p.key.hash(&mut command);
        p.reads.len().hash(&mut command);
        for k in &p.reads {
            (*k, f.contents.get(k).copied().unwrap_or(0)).hash(&mut command);
        }
        let command = command.digest128();
        let attachment = p.attachment_command(command, &f.contents);
        (
            command,
            attachment,
            f.damage.record(
                p,
                command,
                attachment,
                &f.contents,
                target.as_ref().map(|t| hash(t.image.id())),
            ),
        )
    };
    for &(k, overwrite) in &p.writes {
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
        } else if overwrite {
            hash((k, command))
        } else {
            hash((old, command))
        };
        f.contents.slots[slot].1 = Some(next);
    }
    if limit
        && let Operation::Upload {
            resource, level, ..
        } = &p.operation
    {
        f.held_uploads.push((*resource, *level));
    }
    f.key = hash((f.key, command));
    #[cfg(feature = "d3d9-counters")]
    let work = (shared.is_none() && matches!(p.operation, Operation::Draw(_))).then_some((
        (passes::Plan::bytes(p.reads.len(), p.writes.len())
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
        let pass = f
            .passes
            .record(&p.operation, p.reads.clone(), p.writes.clone(), p.bounds);
        if let (Some(entry), Some(destinations)) = (entry, destinations) {
            let _ = entry.saved.set(reuse::Saved {
                command,
                attachment,
                damage,
                pass,
                destinations,
            });
        }
    }
    let mut operation = p.operation.clone();
    if let (Some(target), Operation::Draw(draw)) = (target, &mut operation) {
        draw.target = target;
    }
    f.commands.push(operation);
    f.nodes.push(f.damage.len().checked_sub(1));
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
            let rejected = f.commands.pop();
            f.nodes.pop();
            let len = f.commands.len();
            f.passes.truncate(f.commands.len());
            f.forget_damage();
            f.bytes -= bytes;
            f.key = key;
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
            env.ext.get_mut::<State>().frames.recorded_commands -= 1;
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
        if f.passes.skip(f.head) {
            f.commands[f.head] = Operation::Consumed;
            f.head += 1;
            continue;
        }
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
            let mut result = Ok(Completion::Command);
            // A write meeting several scissors resumes after the ones it
            // finished when it was busy, so no blend applies twice.
            while result.is_ok() && frame(env, owner).part < parts {
                let f = frame(env, owner);
                let clip = f.damage.clip(node.unwrap()).unwrap()[f.part];
                if !f.passes.outside_damage(f.head, clip) {
                    let gpu = env.ext.get_mut::<State>().gpu.as_mut().unwrap();
                    gpu.set_clip(Some(clip));
                    result = execute_gpu(env, &operation);
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
        } else {
            let result = execute_gpu(env, &operation);
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
            Err(Error::Device(_)) => {
                if let Some(Payload::Device(d)) =
                    com(env).get_identity_mut(&mut env.sys.objects, owner)
                {
                    d.gpu_lost = true;
                    d.frame.damage.invalidate();
                    d.frame.commands.clear();
                    d.frame.nodes.clear();
                    d.frame.passes.truncate(0);
                    d.frame.head = 0;
                    d.frame.part = 0;
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
        f.passes.retain(&keep);
        d.queries.scanned = f.commands.len();
        f.head = 0;
        f.part = 0;
        if f.commands.is_empty() {
            f.bytes = 0;
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
        let area = executed.map_or(u64::from(full.w) * u64::from(full.h), |r| {
            adv_device::surface::rects(r)
                .iter()
                .map(|r| u64::from(r.w) * u64::from(r.h))
                .sum()
        });
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
    let changed = delta.map_or(0, damage::area);
    let executed = f.output_slot.and_then(|s| f.executed.get(&s)).cloned();
    let execution_area = executed.as_ref().map_or(0, damage::area);
    let extra = match (executed, delta) {
        (Some(mut r), Some(delta)) => {
            r.intersect(delta);
            execution_area - damage::area(&r)
        }
        _ => execution_area,
    };
    trace::work(&f.damage.work, f.damage.planned);
    let counters = &mut env.ext.get_mut::<State>().frames;
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
    f.key = 0;
    f.reuse.finish();
    f.partial = false;
    f.verdict = None;
    if let Some(Payload::Device(d)) = com(env).get_identity(&env.sys.objects, owner) {
        d.fixed.finish_inputs();
    }
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
