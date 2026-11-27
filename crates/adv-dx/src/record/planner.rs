//! Ordered admission, observation and retirement of neutral recording payloads.
use super::*;
use adv_device::surface::{Rect, Region};
use key::hash;
use smallvec::SmallVec;
use std::{hash::Hash, marker::PhantomData, sync::OnceLock};

pub(crate) struct Op<'a, P> {
    pub payload: P,
    pub effects: damage::Input<'a>,
    pub bytes: usize,
    pub fresh: bool,
    pub reuse: Option<&'a OnceLock<reuse::Saved>>,
    pub trace: Option<&'a AdmissionTrace<'a>>,
}

type AdmissionTrace<'a> = dyn Fn(usize, bool, &Contents) + 'a;

pub(crate) struct Admitted {
    pub key_hashes: u64,
    #[cfg(feature = "d3d9-counters")]
    reused: bool,
    undo: Option<Undo>,
}
struct Undo {
    bytes: usize,
    contents: Vec<(ResourceKey, Option<u128>, bool)>,
}
impl Admitted {
    #[cfg(feature = "d3d9-counters")]
    pub fn reused(&self) -> bool {
        self.reused
    }

    pub fn at_limit(&self) -> bool {
        self.undo.is_some()
    }
}

#[derive(Clone, Copy)]
pub(crate) enum Reason {
    Present { slot: u128, verify: bool },
    Readback(ResourceKey),
    Query,
    Map,
    Wait,
    CommandLimit,
    Barrier,
    Release,
}

pub(crate) enum Forgotten {
    Subresource(ResourceKey),
    Allocation((u64, u64)),
}
impl From<ResourceKey> for Forgotten {
    fn from(key: ResourceKey) -> Self {
        Self::Subresource(key)
    }
}
impl From<(u64, u64)> for Forgotten {
    fn from(allocation: (u64, u64)) -> Self {
        Self::Allocation(allocation)
    }
}

#[derive(Clone, Copy, Default)]
pub(crate) struct Cursor {
    pub ops: usize,
    pub scissors: usize,
}

pub(crate) struct Plan<P> {
    selected: Option<HashMap<usize, Vec<ResourceKey>>>,
    clipped: bool,
    pub promotions: [u64; 2],
    pub dead_passes: u64,
    payload: PhantomData<fn() -> P>,
}

enum Action<'a> {
    Discard,
    Defer,
    Execute(Option<&'a [ResourceKey]>),
}

pub(crate) struct Planned<'a, P> {
    pub payload: &'a P,
    pub scissors: Option<SmallVec<[Rect; 8]>>,
    pub clear_mask: u8,
    pub node: Option<usize>,
    pub retains_aspects: bool,
}
impl<P> Plan<P> {
    fn action(&self, recording: &Recording<P>, n: usize) -> Action<'_> {
        if recording.passes.skip(n) {
            Action::Discard
        } else if let (Some(selected), Some(node)) = (&self.selected, recording.nodes[n]) {
            match selected.get(&node) {
                Some(contents) => Action::Execute(Some(contents)),
                None => Action::Defer,
            }
        } else {
            Action::Execute(None)
        }
    }

    /// An entry's payload and scissors in API order. The recording's cursor
    /// identifies the first entry and scissor still to be executed.
    pub fn at<'a>(&self, recording: &'a Recording<P>, n: usize) -> Option<Planned<'a, P>> {
        let payload = recording.commands.get(n)?.as_ref()?;
        let Action::Execute(selected) = self.action(recording, n) else {
            return None;
        };
        let node = recording.nodes[n];
        let clear_mask = recording.passes.clear_mask(n)
            & selected.map_or(7, |keys| keys.iter().fold(0, |mask, key| mask | 1 << key.2));
        let scissors = if self.clipped {
            node.and_then(|node| recording.damage.clip(node))
                .map(|clips| {
                    clips
                        .iter()
                        .copied()
                        .filter(|&clip| !recording.passes.outside_damage(n, clip))
                        .collect()
                })
        } else {
            None
        };
        Some(Planned {
            payload,
            scissors,
            clear_mask,
            node,
            retains_aspects: recording.passes.retains_aspects(n, selected),
        })
    }
}

#[cfg(test)]
type TestExecutionPlan = Vec<(bool, u8, Option<Vec<Rect>>)>;

pub(crate) struct Recording<P> {
    pub commands: Vec<Option<P>>,
    #[cfg(test)]
    pub command_identities: Vec<u128>,
    #[cfg(test)]
    pub test_plan: Option<TestExecutionPlan>,
    nodes: Vec<Option<usize>>,
    passes: passes::Plan,
    pub damage: damage::Analysis,
    pub contents: Contents,
    pub cursor: Cursor,
    pub partial: bool,
    bytes: usize,
    upload: u128,
    last: Option<u128>,
    last_serial: Option<u64>,
    verdict: Option<bool>,
    output_slot: Option<u128>,
    executed: HashMap<u128, Region>,
    pub verify_region: Option<Region>,
    pub previous: Option<(u32, u32, Vec<u8>)>,
}

impl<P> Default for Recording<P> {
    fn default() -> Self {
        Self {
            commands: Vec::new(),
            #[cfg(test)]
            command_identities: Vec::new(),
            #[cfg(test)]
            test_plan: None,
            nodes: Vec::new(),
            passes: passes::Plan::default(),
            damage: damage::Analysis::default(),
            contents: Contents::default(),
            cursor: Cursor::default(),
            partial: false,
            bytes: 0,
            upload: 0,
            last: None,
            last_serial: None,
            verdict: None,
            output_slot: None,
            executed: HashMap::default(),
            verify_region: None,
            previous: None,
        }
    }
}

impl<P> Recording<P> {
    #[cfg(feature = "d3d9-counters")]
    pub fn effect_bytes(reads: usize) -> usize {
        passes::Plan::bytes(reads)
    }

    fn would_flush(&self, bytes: usize) -> bool {
        self.commands.len() + 1 >= 4096 || self.bytes + bytes >= 64 * 1024 * 1024
    }

    pub fn admit(&mut self, op: Op<'_, P>) -> Admitted {
        let p = &op.effects;
        let retained = op.reuse.and_then(OnceLock::get);
        let shared = retained.filter(|saved| {
            p.writes
                .iter()
                .zip(&saved.destinations)
                .all(|((key, _), old)| {
                    self.damage.independent(key) || self.contents.get(key).copied() == *old
                })
        });
        let bytes = if retained.is_some() { 0 } else { op.bytes };
        if let Some(trace) = op.trace {
            trace(self.commands.len(), shared.is_some(), &self.contents);
        }
        let undo = self.would_flush(bytes).then(|| Undo {
            bytes,
            contents: p
                .writes
                .iter()
                .map(|(key, _)| {
                    (
                        *key,
                        self.contents.get(key).copied(),
                        self.contents.had_base(key),
                    )
                })
                .collect(),
        });
        let destinations = op.reuse.filter(|entry| entry.get().is_none()).map(|_| {
            p.writes
                .iter()
                .map(|(key, _)| self.contents.get(key).copied())
                .collect()
        });
        let read_versions: SmallVec<[Option<u128>; 4]> = if shared.is_none() {
            p.reads
                .iter()
                .map(|key| self.contents.get(key).copied())
                .collect()
        } else {
            SmallVec::new()
        };
        let mut key_hashes = 0;
        let (command, attachment, damage) = if let Some(saved) = shared {
            self.damage.replay(&saved.damage, p.slot, &self.contents);
            (saved.command, saved.attachment, None)
        } else {
            key_hashes += 1;
            let mut command = xxhash_rust::xxh3::Xxh3::new();
            p.key.hash(&mut command);
            p.reads.len().hash(&mut command);
            for (&key, &version) in p.reads.iter().zip(&read_versions) {
                (key, version.unwrap_or(0)).hash(&mut command);
            }
            let command = command.digest128();
            let attachment = p.attachment_command(command, &read_versions);
            let damage = self.damage.record(
                p,
                [command, attachment],
                &self.contents,
                &read_versions,
                destinations.is_some(),
            );
            (command, attachment, damage)
        };
        let mut outputs = Vec::new();
        for (n, &(key, overwrite)) in p.writes.iter().enumerate() {
            let command = p.write_key(key, if key.2 == 0 { command } else { attachment });
            let slot = self.contents.resolve(key);
            let old = self.contents.slots[slot].1.unwrap_or(0);
            if self.contents.slots[slot].2.is_none() {
                self.contents.slots[slot].2 = Some(old);
                self.contents.touched.push(slot);
            }
            let next = if op.fresh {
                self.upload = self
                    .upload
                    .checked_add(1)
                    .expect("upload content exhausted");
                self.upload
            } else if let Some(saved) =
                shared.filter(|saved| saved.destinations[n].unwrap_or(0) == old)
            {
                saved.outputs[n]
            } else {
                key_hashes += 1;
                if overwrite {
                    hash((key, command))
                } else {
                    hash((old, command))
                }
            };
            self.contents.slots[slot].1 = Some(next);
            if destinations.is_some() {
                outputs.push(next);
            }
        }
        if let Some(saved) = shared {
            self.passes.replay(saved.pass.clone());
        } else {
            let pass = self.passes.record(
                p.kind,
                SmallVec::from_slice(p.reads),
                read_versions,
                SmallVec::from_slice(p.writes),
                p.bounds,
                destinations.is_some(),
            );
            if let (Some(entry), Some(destinations)) = (op.reuse, destinations) {
                let _ = entry.set(reuse::Saved {
                    command,
                    attachment,
                    damage: damage.unwrap(),
                    pass: pass.unwrap(),
                    destinations,
                    outputs,
                });
            }
        }
        self.bytes += bytes;
        self.commands.push(Some(op.payload));
        self.nodes.push(self.damage.len().checked_sub(1));
        #[cfg(test)]
        self.command_identities.push(command);
        Admitted {
            key_hashes,
            #[cfg(feature = "d3d9-counters")]
            reused: shared.is_some(),
            undo,
        }
    }

    /// A continuation that cannot be parked rejects only the new admission.
    pub fn reject(&mut self, admitted: Admitted) -> P {
        let Undo { bytes, contents } = admitted.undo.unwrap();
        let rejected = self.commands.pop().unwrap().unwrap();
        #[cfg(test)]
        self.command_identities.pop();
        self.nodes.pop();
        self.passes.truncate(self.commands.len());
        self.invalidate_damage();
        self.bytes -= bytes;
        for (key, old, had_base) in contents {
            self.contents.restore(key, old, had_base);
        }
        rejected
    }

    pub fn observes(&self, key: ResourceKey, verify: bool) -> bool {
        if !verify && self.damage.settled(&key) {
            return false;
        }
        self.commands[self.cursor.ops..]
            .iter()
            .zip(&self.nodes[self.cursor.ops..])
            .any(|(op, node)| op.is_some() && node.is_none_or(|n| self.damage.pending(n, &key)))
    }

    pub fn plan(&mut self, reason: Reason) -> Plan<P> {
        let mut plan = self.unclipped_plan();
        match reason {
            Reason::Present { slot, verify } => {
                plan.clipped = !verify;
                if !verify && !self.partial && self.cursor.ops == 0 {
                    plan.dead_passes = self.passes.plan();
                }
                plan.promotions = self.damage.plan(self.last.is_none(), slot);
                self.output_slot = Some(slot);
                #[cfg(test)]
                if let Some(plan) = &mut self.test_plan {
                    plan.extend(self.nodes.iter().enumerate().map(|(n, node)| {
                        (
                            self.passes.skip(n),
                            self.passes.clear_mask(n),
                            node.and_then(|node| self.damage.clip(node).map(<[_]>::to_vec)),
                        )
                    }));
                }
            }
            Reason::Readback(target) => plan.selected = Some(self.damage.closure(target)),
            Reason::Map | Reason::Barrier | Reason::Release => self.invalidate_damage(),
            Reason::Query | Reason::Wait | Reason::CommandLimit => (),
        }
        plan
    }

    pub fn unclipped_plan(&self) -> Plan<P> {
        Plan {
            selected: None,
            clipped: false,
            promotions: [0; 2],
            dead_passes: 0,
            payload: PhantomData,
        }
    }

    /// The cursor names the completed prefix and the completed scissors of
    /// the next operation. A selective readback leaves other writes queued.
    pub fn retire(&mut self, plan: &Plan<P>, executed: Cursor) {
        for n in self.cursor.ops..executed.ops {
            match plan.action(self, n) {
                Action::Discard => self.commands[n] = None,
                Action::Defer => (),
                Action::Execute(selected) => {
                    if let Some(node) = self.nodes[n] {
                        self.damage.executed(node, selected);
                    }
                    if !self.passes.retains_aspects(n, selected) {
                        self.commands[n] = None;
                    }
                    self.partial = true;
                }
            }
        }
        self.cursor = executed;
    }

    pub fn end(&mut self, plan: Option<Plan<P>>) {
        if plan.is_some_and(|p| p.selected.is_some()) {
            let keep: Vec<_> = self.commands.iter().map(Option::is_some).collect();
            let mut iter = keep.iter().copied();
            self.commands.retain(|_| iter.next().unwrap());
            let mut iter = keep.iter().copied();
            self.nodes.retain(|_| iter.next().unwrap());
            self.passes.retain(&keep);
        } else {
            self.commands.clear();
            self.nodes.clear();
            self.passes.truncate(0);
        }
        self.cursor = Cursor::default();
        if self.commands.is_empty() {
            self.bytes = 0;
        }
    }

    pub fn node(&self, index: usize) -> Option<usize> {
        self.nodes[index]
    }

    pub fn can_merge(&self, first: usize, next: usize) -> bool {
        self.passes.can_merge(first, next)
    }

    pub fn forget(&mut self, resource: impl Into<Forgotten>) {
        let resource = resource.into();
        let matches = |key: &ResourceKey| match resource {
            Forgotten::Subresource(candidate) => candidate == *key,
            Forgotten::Allocation(allocation) => allocation == key.0,
        };
        if self.contents.forget(matches) {
            self.partial = true;
            self.last = None;
        }
        self.damage.forget(matches);
    }

    pub fn invalidate_damage(&mut self) {
        self.damage.invalidate();
        self.nodes.fill(None);
    }

    pub fn cancel(&mut self) {
        self.partial = true;
        self.passes.truncate(self.commands.len());
        self.last = None;
        self.invalidate_damage();
        self.verdict = None;
    }

    pub fn lose(&mut self) {
        self.damage.invalidate();
        self.commands.clear();
        self.nodes.clear();
        self.passes.truncate(0);
        self.cursor = Cursor::default();
    }

    pub fn repeated(&mut self, identity: u128, serial: Option<u64>) -> bool {
        *self.verdict.get_or_insert_with(|| {
            !self.partial
                && self.last == Some(identity)
                && self.last_serial.is_some()
                && self.last_serial == serial
                && self.contents.unchanged()
        })
    }

    pub fn force_present(&self) -> bool {
        self.last.is_none()
    }

    pub fn rotatable(&self, slot: u128) -> bool {
        !self.force_present() && !self.partial && self.damage.rotatable(slot)
    }

    pub fn present_damage(
        &mut self,
        plan: &Plan<P>,
        size: (u32, u32),
        copy: bool,
        published: impl FnOnce(Option<&Region>, bool) -> Region,
        counters: &mut Counters,
    ) {
        let force = self.force_present();
        let slot = self.output_slot.unwrap();
        let delta = self.damage.changed.get(&slot);
        self.verify_region = Some(published(delta, !force && !self.partial));
        trace::present(
            self.partial,
            force,
            copy,
            plan.promotions,
            &self.damage.execution,
        );
        let whole = if force {
            Some(1)
        } else if self.verify_region.as_ref().is_some_and(Region::is_whole) {
            Some(if self.partial {
                0
            } else if copy {
                2
            } else {
                3
            })
        } else {
            None
        };
        #[cfg(feature = "d3d9-counters")]
        {
            counters.eliminated_passes += plan.dead_passes;
        }
        if let Some(cause) = whole {
            counters.whole[cause] += 1;
        }
        counters.intermediate_promotions += plan.promotions[0];
        counters.promotions += plan.promotions[1];
        let executed = self
            .damage
            .execution
            .get(&slot)
            .filter(|region| !region.is_whole());
        if !plan.clipped || executed.is_none() {
            counters.full_executions += 1;
        } else {
            counters.clipped_executions += 1;
        }
        let area = executed.map_or(u64::from(size.0) * u64::from(size.1), Region::area);
        counters.clipped_area_sum += area as f64 / (f64::from(size.0) * f64::from(size.1));
    }

    pub fn submitted(
        &mut self,
        node: Option<usize>,
        (slot, size): (u128, (u32, u32)),
        area: Rect,
        aspects: u8,
        vertices: u64,
        counters: &mut Counters,
    ) {
        self.executed
            .entry(slot)
            .or_insert_with(|| Region::empty(size))
            .add(area.into());
        let repeated = node.is_some_and(|n| self.damage.submitted(n, area, aspects, vertices));
        counters.submissions += 1;
        counters.submitted_pixels += u64::from(area.w) * u64::from(area.h);
        counters.submitted_vertices += vertices;
        if repeated {
            counters.repeated_vertices += vertices;
        }
    }

    pub fn finish_metrics(&self, counters: &mut Counters) {
        let delta = self.output_slot.and_then(|s| self.damage.changed.get(&s));
        let executed = self
            .output_slot
            .and_then(|s| self.executed.get(&s))
            .cloned();
        let execution_area = executed.as_ref().map_or(0, Region::area);
        let extra = match (executed, delta) {
            (Some(mut r), Some(delta)) => {
                r.intersect(delta);
                execution_area - r.area()
            }
            _ => execution_area,
        };
        counters.presents += 1;
        counters.changed_pixels += delta.map_or(0, Region::area);
        counters.executed_pixels += execution_area;
        counters.extra_pixels += extra;
        trace::work(&self.damage.work, self.damage.planned);
        trace::frame_end();
    }

    pub fn finish(&mut self, identity: u128, serial: Option<u64>) {
        self.damage.finish();
        self.verify_region = None;
        self.output_slot = None;
        self.executed.clear();
        self.last = Some(identity);
        self.last_serial = serial;
        self.contents.reset();
        #[cfg(test)]
        self.command_identities.clear();
        self.partial = false;
        self.verdict = None;
    }

    pub fn verify(
        &mut self,
        pixels: (u32, u32, Vec<u8>),
        repeat: bool,
        counters: &mut Counters,
    ) -> bool {
        let mismatch = Self::verify_pixels(
            &mut self.previous,
            pixels,
            self.verify_region.as_ref().unwrap(),
            repeat,
        );
        counters.verification_mismatches += u64::from(mismatch);
        mismatch
    }

    pub fn verify_pixels(
        previous: &mut Option<(u32, u32, Vec<u8>)>,
        pixels: (u32, u32, Vec<u8>),
        damage: &Region,
        repeat: bool,
    ) -> bool {
        let mismatch = previous.as_ref().is_some_and(|previous| {
            previous.0 == pixels.0
                && previous.1 == pixels.1
                && pixels
                    .2
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .zip(previous.2.as_chunks::<4>().0)
                    .enumerate()
                    .any(|(n, (a, b))| {
                        let (x, y) = (n as u32 % pixels.0, n as u32 / pixels.0);
                        a != b && (repeat || !damage.intersects(Rect { x, y, w: 1, h: 1 }.into()))
                    })
        });
        *previous = Some(pixels);
        mismatch
    }

    #[cfg(test)]
    pub fn set_bytes(&mut self, bytes: usize) {
        self.bytes = bytes;
    }

    #[cfg(test)]
    pub fn last_serial(&self) -> Option<u64> {
        self.last_serial
    }
}

#[cfg(test)]
#[path = "planner_tests.rs"]
mod tests;
