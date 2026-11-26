//! Version histories and scissor scheduling for recorded resource writes.
use super::{
    Contents, HashMap, Kind, ResourceKey as Content,
    key::{hash, rect},
    trace,
};
use adv_device::surface::{Rect, Region};
use rustc_hash::FxHashSet as HashSet;
use smallvec::SmallVec;
use std::sync::Arc;

mod forward;
pub(crate) mod sampling;
mod schedule;

pub(crate) struct Input<'a> {
    pub kind: Kind,
    pub view: Rect,
    pub attachment: Option<(Rect, u8)>,
    pub color_shape: u8,
    pub slot: u128,
    pub bounds: Rect,
    pub key: u128,
    pub reads: &'a [Content],
    pub samples: &'a [sampling::Read],
    pub writes: &'a [(Content, bool)],
    pub ignores_sampled_color: bool,
    pub clear_keys: [Option<u128>; 3],
}

impl Input<'_> {
    pub fn write_key(&self, key: Content, command: u128) -> u128 {
        if self.kind == Kind::Clear {
            self.clear_keys[key.2 as usize].unwrap()
        } else {
            command
        }
    }

    pub fn attachment_command(&self, command: u128, versions: &[Option<u128>]) -> u128 {
        if !self.ignores_sampled_color
            || !self.writes.iter().any(|(key, _)| key.2 != 0)
            || !self.reads.iter().any(|key| key.2 == 0)
        {
            return command;
        }
        use std::hash::Hash;
        let mut key = xxhash_rust::xxh3::Xxh3::new();
        self.key.hash(&mut key);
        for (&source, &version) in self
            .reads
            .iter()
            .zip(versions)
            .filter(|(key, _)| key.2 != 0)
        {
            (source, version.unwrap_or(0)).hash(&mut key);
        }
        key.digest128()
    }
}

fn take_region(pool: &mut Vec<Region>, extent: (u32, u32), whole: bool) -> Region {
    let mut region = pool.pop().unwrap_or_default();
    region.reset(extent, whole);
    region
}

fn recycle_region(pool: &mut Vec<Region>, region: Region) {
    if pool.len() < 128 {
        pool.push(region);
    }
}

fn size(r: Rect) -> (u32, u32) {
    (r.x + r.w, r.y + r.h)
}
#[derive(Clone, Default)]
struct Samples {
    base: u128,
    reads: SmallVec<[Sample; 4]>,
}
#[derive(Clone)]
struct Sample {
    source: Content,
    mappings: SmallVec<[sampling::Mapping; 2]>,
    version: u128,
}
/// The generation and number of writes visible to a read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Prefix {
    forward: usize,
    len: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Match {
    index: usize,
    inputs: bool,
}

#[derive(Clone, Copy)]
struct Generation {
    index: usize,
    forward: usize,
}

struct Observed {
    samples: Samples,
    prefixes: SmallVec<[Option<Prefix>; 4]>,
}

/// One generation of a content: its writes from a proven overwrite (or the
/// frame start) up to the next.
#[derive(Default)]
struct Sequence {
    entries: Vec<(u128, Rect)>,
    observed: Vec<Option<Observed>>,
    forward: usize,
    index: Vec<(u128, usize)>,
    sampled_index: Vec<(u128, usize)>,
    cursor: usize,
    matches: Vec<Option<Match>>,
    different: bool,
    full: Rect,
    view: Rect,
    shape: u128,
    slot: u128,
    independent: bool,
    initial: u128,
}
impl Sequence {
    fn index(&mut self) {
        self.index.extend(
            self.entries
                .iter()
                .enumerate()
                .map(|(n, &(key, _))| (key, n)),
        );
        self.index.sort_unstable();
        self.sampled_index.extend(
            self.observed
                .iter()
                .enumerate()
                .filter_map(|(n, entry)| Some((entry.as_ref()?.samples.base, n))),
        );
        self.sampled_index.sort_unstable();
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.observed.clear();
        self.forward = 0;
        self.index.clear();
        self.sampled_index.clear();
        self.cursor = 0;
        self.matches.clear();
        self.different = false;
        self.full = Rect::default();
        self.view = Rect::default();
        self.shape = 0;
        self.slot = 0;
        self.independent = false;
        self.initial = 0;
    }
}
static EMPTY: std::sync::LazyLock<Sequence> = std::sync::LazyLock::new(Sequence::default);
#[derive(Default)]
struct History {
    /// The previous frame's generations and this frame's.
    previous: Vec<Sequence>,
    next: Vec<Sequence>,
    /// Cleared sequences retained to reuse their buffers across frames.
    spare: Vec<Sequence>,
    /// The previous generation each of this frame's compares with.
    matched: Vec<Option<usize>>,
    /// Per slot, the cells whose pixels differ from this content's current
    /// version. Every change marks them in every slot at once; a slot that
    /// writes the content is current again. A slot stale everywhere is
    /// dropped: no entry means redraw whole.
    stale: HashMap<u128, Region>,
    serial: u64,
    seen: u64,
}
impl History {
    fn current(&self) -> Option<&Sequence> {
        self.next.last()
    }
    fn replace_previous(&mut self) {
        for mut seq in self.previous.drain(..) {
            seq.clear();
            self.spare.push(seq);
        }
        std::mem::swap(&mut self.previous, &mut self.next);
        for sequence in &mut self.previous {
            sequence.index();
        }
    }
    fn recycle_next(&mut self) {
        for mut seq in self.next.drain(..) {
            seq.clear();
            self.spare.push(seq);
        }
    }
    /// Starts a generation at a write keyed `key`. One continuing the frame
    /// start compares with the previous frame's first; one from an overwrite
    /// with the first later previous generation that began with the same
    /// write, or with nothing.
    fn start(&mut self, key: u128, bounds: Rect, fresh: bool) {
        let after = self
            .matched
            .iter()
            .rev()
            .find_map(|m| *m)
            .map_or(0, |m| m + 1);
        let found = if fresh {
            (after..self.previous.len())
                .find(|&j| self.previous[j].entries.first() == Some(&(key, bounds)))
        } else {
            (!self.previous.is_empty()).then_some(0)
        };
        self.matched.push(found);
        self.next.push(self.spare.pop().unwrap_or_default());
    }
    fn record(
        &mut self,
        key: u128,
        bounds: Rect,
        samples: Samples,
        prefixes: SmallVec<[Option<Prefix>; 4]>,
    ) {
        let previous = self
            .matched
            .last()
            .copied()
            .flatten()
            .map_or(&*EMPTY, |m| &self.previous[m]);
        let s = self.next.last_mut().unwrap();
        let candidate = |key, entries: &[(u128, usize)]| {
            let first = entries.partition_point(|&v| v < (key, s.cursor));
            entries[first..]
                .iter()
                .take_while(|&&(k, _)| k == key)
                .find_map(|&(_, n)| (previous.entries[n].1 == bounds).then_some(n))
        };
        let exact = if previous.entries.get(s.cursor) == Some(&(key, bounds)) {
            Some(s.cursor)
        } else {
            candidate(key, &previous.index)
        };
        let matched = exact
            .map(|index| Match {
                index,
                inputs: false,
            })
            .or_else(|| {
                if samples.reads.is_empty() {
                    return None;
                }
                candidate(samples.base, &previous.sampled_index).map(|index| Match {
                    index,
                    inputs: true,
                })
            });
        s.different |= exact != Some(s.cursor);
        if let Some(matched) = matched {
            s.cursor = matched.index + 1;
        }
        s.matches.push(matched);
        if !samples.reads.is_empty() {
            s.observed.push(Some(Observed { samples, prefixes }));
        } else {
            s.observed.push(None);
        }
        s.entries.push((key, bounds));
    }
    /// After every generation's change: what the final version changed since
    /// the previous frame's, and what its slot must redraw.
    #[cfg(test)]
    fn settle(&mut self, force: bool, last: Region) -> (Region, Region) {
        let mut pool = Vec::new();
        self.settle_with_pool(force, last, &mut pool)
    }
    fn settle_with_pool(
        &mut self,
        force: bool,
        last: Region,
        pool: &mut Vec<Region>,
    ) -> (Region, Region) {
        let next = self.next.last().unwrap();
        let size = size(next.full);
        let compared = !force
            && !self.previous.is_empty()
            && self.matched.last() == Some(&Some(self.previous.len() - 1));
        let changed = if compared {
            last
        } else {
            recycle_region(pool, last);
            take_region(pool, size, true)
        };
        self.serial += 1;
        for stale in self.stale.values_mut() {
            stale.union(&changed);
        }
        self.stale.retain(|_, stale| !stale.is_whole());
        let empty = take_region(pool, size, false);
        let redraw = match self.stale.get_mut(&next.slot) {
            Some(existing) => std::mem::replace(existing, empty),
            None => {
                self.stale.insert(next.slot, empty);
                take_region(pool, size, true)
            }
        };
        self.replace_previous();
        self.matched.clear();
        (changed, redraw)
    }

    fn retains_start(&self) -> bool {
        let next = &self.next[0];
        self.next.len() == 1
            && !next.independent
            && self
                .previous
                .last()
                .is_some_and(|old| old.shape == next.shape)
            && self.stale.get(&next.slot).is_some_and(Region::is_empty)
    }
}
type Write = (Content, u128, Rect, Rect, u128, bool);
#[derive(Clone)]
pub(crate) struct Record {
    writes: SmallVec<[Write; 3]>,
    /// What the write's pixels read from each source: a proven mapping, or
    /// the whole source.
    demands: SmallVec<[(Content, Option<sampling::Mapping>); 4]>,
    bounds: Rect,
    view: Rect,
    upload: bool,
    clear: bool,
}
pub(crate) struct Saved {
    record: Record,
    samples: SmallVec<[Samples; 3]>,
}

/// Scissors per write. A draw spanning several distant changes is submitted
/// once per scissor it meets; past this many, coarser cells merge them.
const PARTS: usize = 8;
#[derive(Default)]
pub(crate) struct Work {
    pub changed: Region,
    pub executed: Region,
    pub submissions: u64,
    pub vertices: u64,
    pub repeated_vertices: u64,
}
impl Work {
    pub fn areas(&self) -> [u64; 3] {
        let mut common = self.executed.clone();
        common.intersect(&self.changed);
        let executed = self.executed.area();
        [self.changed.area(), executed, executed - common.area()]
    }
}
#[derive(Default)]
pub(crate) struct Analysis {
    histories: HashMap<Content, History>,
    /// This frame's writes in record order.
    nodes: Vec<schedule::Node>,
    /// The write that produced each content's current version this frame.
    last_writer: HashMap<Content, usize>,
    /// Writes, by content, a flush already executed whole.
    done: HashSet<(usize, Content)>,
    /// What each write executes, as disjoint scissors in its view; `None`
    /// executes it whole (GFX-REC-5).
    clips: schedule::Clips,
    /// The cells each slot executes this frame.
    pub execution: HashMap<u128, Region>,
    /// What each slot's color changed.
    pub changed: HashMap<u128, Region>,
    pub work: HashMap<(Content, usize, u128), Work>,
    pub planned: bool,
    epoch: u64,
    forward_len: usize,
    region_pool: Vec<Region>,
    forward_scratch: forward::Scratch,
    stale_scratch: HashMap<Content, Region>,
    schedule_scratch: schedule::Scratch,
}
impl Analysis {
    #[cfg(test)]
    pub fn recorded(&self) -> impl std::fmt::Debug + PartialEq + use<> {
        let nodes: Vec<_> = self
            .nodes
            .iter()
            .map(|n| {
                (
                    n.record.bounds,
                    n.record.writes.clone(),
                    n.record
                        .demands
                        .iter()
                        .map(|&(source, mapping)| (source, mapping.map(sampling::Mapping::bits)))
                        .collect::<Vec<_>>(),
                )
            })
            .collect();
        let mut histories: Vec<_> = self
            .histories
            .iter()
            .map(|(&k, history)| {
                let sequences: Vec<_> = history
                    .next
                    .iter()
                    .map(|s| {
                        let observed: Vec<_> = s
                            .observed
                            .iter()
                            .map(|o| {
                                o.as_ref().map(|o| {
                                    let reads: Vec<_> = o
                                        .samples
                                        .reads
                                        .iter()
                                        .map(|r| {
                                            (
                                                r.source,
                                                r.version,
                                                r.mappings
                                                    .iter()
                                                    .map(|m| m.bits())
                                                    .collect::<Vec<_>>(),
                                            )
                                        })
                                        .collect();
                                    (o.samples.base, reads, o.prefixes.clone())
                                })
                            })
                            .collect();
                        (s.entries.clone(), s.matches.clone(), observed)
                    })
                    .collect();
                (k, sequences)
            })
            .collect();
        histories.sort_by_key(|(k, _)| (k.0, k.1, k.2));
        (nodes, histories)
    }

    pub fn written(&self, content: &Content) -> bool {
        self.histories
            .get(content)
            .is_some_and(|h| !h.next.is_empty())
    }
    pub fn record(
        &mut self,
        p: &Input<'_>,
        [command, attachment]: [u128; 2],
        contents: &Contents,
        read_versions: &[Option<u128>],
        retain: bool,
    ) -> Option<Arc<Saved>> {
        let mut demands = SmallVec::new();
        for &k in p.reads {
            let mut proven = p.samples.iter().filter(|r| r.source == k).peekable();
            if proven.peek().is_none() {
                let attachment = (k.2 != 0 && p.kind == Kind::Draw)
                    .then_some(p.attachment)
                    .flatten();
                demands.push((k, attachment.map(|(r, _)| sampling::Mapping::identity(r))));
            }
            demands.extend(proven.map(|r| (k, Some(r.mapping))));
        }
        let mut record = Record {
            writes: SmallVec::new(),
            demands,
            bounds: p.bounds.clipped(p.view),
            view: p.view,
            upload: p.kind == Kind::Upload,
            clear: p.kind == Kind::Clear,
        };
        let mut observations = SmallVec::new();
        for &(k, overwrite) in p.writes {
            let current = self.histories.get(&k).and_then(History::current);
            let (full, format) = if k.2 == 0 {
                (p.view, p.color_shape)
            } else {
                p.attachment.unwrap()
            };
            let shape = hash((rect(full), format));
            let bounds = p.bounds.clipped(full);
            let independent =
                (overwrite && bounds == full) || current.is_some_and(|s| s.independent);
            // Only drawing can depend on the old values inside its write bounds.
            // Clears and copies overwrite that region even when the attachment is larger.
            let retained = (p.kind == Kind::Draw && !independent)
                .then(|| contents.get(&k).copied().unwrap_or(0));
            let key = hash((
                p.write_key(k, if k.2 == 0 { command } else { attachment }),
                retained,
            ));
            let ignores_color = k.2 != 0 && p.ignores_sampled_color;
            let samples = Samples {
                base: hash((p.key, retained)),
                reads: p
                    .reads
                    .iter()
                    .zip(read_versions)
                    .filter(|(source, _)| source.2 != 0 || !ignores_color)
                    .map(|(&source, &version)| Sample {
                        source,
                        mappings: p
                            .samples
                            .iter()
                            .filter(|r| r.source == source)
                            .map(|r| r.mapping)
                            .collect(),
                        version: version.unwrap_or(0),
                    })
                    .collect(),
            };
            observations.push(samples);
            record.writes.push((k, key, bounds, full, shape, overwrite));
        }
        let saved = retain.then(|| {
            Arc::new(Saved {
                record: record.clone(),
                samples: observations.clone(),
            })
        });
        self.install(record, observations, p.slot, contents);
        saved
    }
    pub fn independent(&self, k: &Content) -> bool {
        self.histories
            .get(k)
            .and_then(History::current)
            .is_some_and(|s| s.independent)
    }
    pub fn replay(&mut self, saved: &Saved, slot: u128, contents: &Contents) {
        self.install(saved.record.clone(), saved.samples.clone(), slot, contents);
    }

    fn install(
        &mut self,
        record: Record,
        samples: SmallVec<[Samples; 3]>,
        slot: u128,
        contents: &Contents,
    ) {
        let at = self.nodes.len();
        let producers = record
            .demands
            .iter()
            .map(|(k, _)| self.last_writer.get(k).copied())
            .collect();
        let previous = record
            .writes
            .iter()
            .map(|w| self.last_writer.insert(w.0, at))
            .collect();
        let mut generations = SmallVec::new();
        // Reads of all aspects precede this command's writes, including a
        // color write that also updates the depth or stencil it tests.
        let mut prefixes: [SmallVec<[Option<Prefix>; 4]>; 3] =
            [SmallVec::new(), SmallVec::new(), SmallVec::new()];
        for (samples, prefixes) in samples.iter().zip(&mut prefixes) {
            *prefixes = samples
                .reads
                .iter()
                .map(|r| {
                    let h = self.histories.get(&r.source)?;
                    Some(Prefix {
                        forward: h.next.last()?.forward,
                        len: h.next.last()?.entries.len(),
                    })
                })
                .collect();
        }
        for ((&(k, key, bounds, full, shape, overwrite), prefixes), samples) in
            record.writes.iter().zip(prefixes).zip(samples)
        {
            let history = self.histories.entry(k).or_default();
            let fresh = overwrite && bounds == full;
            if fresh || history.next.is_empty() {
                history.start(key, bounds, fresh);
            }
            let s = history.next.last_mut().unwrap();
            s.full = full;
            s.view = record.view;
            s.shape = shape;
            s.slot = slot;
            if s.entries.is_empty() {
                s.forward = self.forward_len;
                self.forward_len += 1;
                s.independent = fresh;
                if !fresh {
                    s.initial = contents.get(&k).copied().unwrap_or(0);
                }
            }
            let forward = s.forward;
            history.record(key, bounds, samples, prefixes);
            let generation = history.next.len() - 1;
            generations.push(Generation {
                index: generation,
                forward,
            });
            if trace::capturing() {
                self.work
                    .entry((k, generation, slot))
                    .or_insert_with(|| Work {
                        changed: Region::whole(size(full)),
                        executed: Region::empty(size(full)),
                        ..Default::default()
                    });
            }
        }
        self.nodes.push(schedule::Node {
            slot,
            record,
            producers,
            previous,
            generations,
            submissions: 0,
        });
    }
    /// Plans the frame; returns whole-chain promotions for intermediate
    /// external reads and late demands, respectively.
    pub fn plan(&mut self, force: bool, output: u128) -> [u64; 2] {
        if self.planned {
            return [0; 2];
        }
        self.epoch += 1;
        forward::changes(
            &self.histories,
            &self.nodes,
            self.forward_len,
            self.epoch,
            force,
            &mut self.forward_scratch,
            &mut self.region_pool,
        );
        for ((content, generation, _), work) in &mut self.work {
            if let Some(s) = self
                .histories
                .get(content)
                .and_then(|h| h.next.get(*generation))
            {
                work.changed = self.forward_scratch.results[s.forward]
                    .as_ref()
                    .unwrap()
                    .clone();
            }
        }
        self.stale_scratch.clear();
        for (content, h) in &mut self.histories {
            if h.next.is_empty() {
                continue;
            }
            let last = h.next.len() - 1;
            let force = force || (h.seen + 1 != self.epoch && !h.retains_start());
            let slot = h.next[last].slot;
            let last_change = self.forward_scratch.results[h.next[last].forward]
                .take()
                .unwrap();
            let (changed, redraw) = h.settle_with_pool(force, last_change, &mut self.region_pool);
            h.seen = self.epoch;
            if content.2 == 0 {
                match self.changed.entry(slot) {
                    std::collections::hash_map::Entry::Occupied(mut entry) => {
                        entry.get_mut().union(&changed);
                        recycle_region(&mut self.region_pool, changed);
                    }
                    std::collections::hash_map::Entry::Vacant(entry) => {
                        entry.insert(changed);
                    }
                }
            } else {
                recycle_region(&mut self.region_pool, changed);
            }
            self.stale_scratch.insert(*content, redraw);
        }
        for region in self.forward_scratch.results.drain(..).flatten() {
            recycle_region(&mut self.region_pool, region);
        }
        let promotions = schedule::schedule(
            &self.nodes,
            &self.done,
            &self.stale_scratch,
            &mut self.clips,
            &mut self.schedule_scratch,
            &mut self.region_pool,
        );
        for (_, region) in self.stale_scratch.drain() {
            recycle_region(&mut self.region_pool, region);
        }
        let diagnose = trace::capturing();
        for (i, node) in self.nodes.iter().enumerate() {
            if node.slot != output && !diagnose {
                continue;
            }
            let view = node.record.view;
            let r = self
                .execution
                .entry(node.slot)
                .or_insert_with(|| take_region(&mut self.region_pool, size(view), false));
            let early = node
                .record
                .writes
                .iter()
                .any(|w| self.done.contains(&(i, w.0)));
            match self.clips.get(i).filter(|_| !early) {
                Some(Some(parts)) => {
                    for &p in parts {
                        r.add(p.into());
                    }
                }
                _ if node.record.upload => r.add(node.record.bounds.into()),
                _ => r.reset(r.size(), true),
            }
        }
        self.planned = true;
        promotions
    }
    pub fn write_bounds(&self, n: usize) -> Option<Rect> {
        self.nodes.get(n).map(|node| node.record.bounds)
    }

    /// The scissors write `n` of the frame executes in; `None` runs it whole.
    pub fn clip(&self, n: usize) -> Option<&[Rect]> {
        self.clips.get(n)?.as_deref()
    }
    /// Counts only successful submissions, including writes before Present.
    pub fn submitted(&mut self, n: usize, rect: Rect, aspects: u8, vertices: u64) -> bool {
        let node = &mut self.nodes[n];
        let repeated = node.submissions != 0;
        node.submissions += 1;
        if self.work.is_empty() {
            return repeated;
        }
        for (w, &generation) in node.record.writes.iter().zip(&node.generations) {
            if aspects & (1 << w.0.2) == 0 {
                continue;
            }
            let Some(work) = self.work.get_mut(&(w.0, generation.index, node.slot)) else {
                continue;
            };
            work.executed.add(rect.into());
            work.submissions += 1;
            work.vertices += vertices;
            if repeated {
                work.repeated_vertices += vertices;
            }
        }
        repeated
    }
    /// How many writes the frame recorded.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }
    /// Write `n` executed whole for `contents`, or for all it writes.
    pub fn executed(&mut self, n: usize, contents: Option<&[Content]>) {
        let Some(node) = self.nodes.get(n) else {
            return;
        };
        for w in &node.record.writes {
            if contents.is_none_or(|c| c.contains(&w.0)) {
                self.done.insert((n, w.0));
            }
        }
    }
    /// Whether `content`'s version so far is the one its slot already holds:
    /// it repeats the previous frame's final version write for write, and no
    /// write of it has executed this frame.
    pub fn settled(&self, content: &Content) -> bool {
        let Some(h) = self.histories.get(content) else {
            return false;
        };
        let (Some(s), Some(&Some(m))) = (h.next.last(), h.matched.last()) else {
            return false;
        };
        m + 1 == h.previous.len()
            && h.serial != 0
            && h.seen == self.epoch
            && h.previous[m].shape == s.shape
            && s.cursor == h.previous[m].entries.len()
            && !s.different
            && h.stale.get(&s.slot).is_some_and(Region::is_empty)
            && !self.done.iter().any(|(_, k)| k == content)
    }
    /// Whether a write not yet executed writes `content`.
    pub fn pending(&self, n: usize, content: &Content) -> bool {
        self.nodes.get(n).is_none_or(|node| {
            node.record
                .writes
                .iter()
                .any(|w| w.0 == *content && !self.done.contains(&(n, w.0)))
        })
    }
    /// The writes, and which of their contents, that must execute before
    /// `content` is observed: the writes its current version depends on, and
    /// the writes that must keep their order with those. Only clears split
    /// by content; other writes need all they write.
    pub fn closure(&self, content: Content) -> HashMap<usize, Vec<Content>> {
        let mut readers: HashMap<Content, Vec<usize>> = HashMap::default();
        for (i, node) in self.nodes.iter().enumerate() {
            for (k, _) in &node.record.demands {
                readers.entry(*k).or_default().push(i);
            }
        }
        let mut needed: HashMap<usize, Vec<Content>> = HashMap::default();
        let mut stack: Vec<(usize, Content)> = self
            .last_writer
            .get(&content)
            .map(|&n| (n, content))
            .into_iter()
            .collect();
        while let Some((c, k)) = stack.pop() {
            if self.done.contains(&(c, k)) || needed.get(&c).is_some_and(|v| v.contains(&k)) {
                continue;
            }
            let node = &self.nodes[c];
            let first = !needed.contains_key(&c);
            needed.entry(c).or_default().push(k);
            if !node.record.clear {
                stack.extend(node.record.writes.iter().map(|w| (c, w.0)));
            }
            if first {
                for (j, (source, _)) in node.record.demands.iter().enumerate() {
                    if let Some(p) = node.producers[j] {
                        stack.push((p, *source));
                    }
                }
            }
            let j = node.record.writes.iter().position(|w| w.0 == k).unwrap();
            if let Some(p) = node.previous[j] {
                stack.push((p, k));
            }
            for &r in readers.get(&k).into_iter().flatten().filter(|&&r| r < c) {
                if let Some(w) = self.nodes[r].record.writes.first() {
                    stack.push((r, w.0));
                }
            }
        }
        needed
    }
    pub fn rotatable(&self, slot: u128) -> bool {
        self.histories.iter().any(|(k, h)| {
            k.2 == 0
                && h.previous.last().is_some_and(|s| s.slot == slot)
                && h.previous.first().is_some_and(|s| s.independent)
                && h.seen == self.epoch
        })
    }
    pub fn finish(&mut self) {
        for h in self.histories.values_mut() {
            if !self.planned && !h.next.is_empty() {
                h.replace_previous();
            } else {
                h.recycle_next();
            }
            h.matched.clear();
        }
        self.forward_len = 0;
        self.clips.clear();
        for (_, region) in self.changed.drain() {
            recycle_region(&mut self.region_pool, region);
        }
        self.nodes.clear();
        self.last_writer.clear();
        self.done.clear();
        for (_, region) in self.execution.drain() {
            recycle_region(&mut self.region_pool, region);
        }
        self.work.clear();
        self.planned = false;
    }
    pub fn invalidate(&mut self) {
        *self = Self::default();
    }
    pub fn forget(&mut self, matches: impl Fn(&Content) -> bool) {
        self.histories.retain(|k, _| !matches(k));
    }
}

#[cfg(test)]
#[path = "damage_unit_tests.rs"]
mod tests;
