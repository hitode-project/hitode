//! Recording-time sequence realignment and conservative eight-frame slot damage.
use super::*;
use adv_device::{
    gpu::{AttributeFormat, Image},
    surface::{Rect, Region, rects},
};
use rustc_hash::FxHashSet as HashSet;
pub(super) mod sampling;
mod schedule;

pub(super) fn image(operation: &Operation) -> Option<&Image> {
    match operation {
        Operation::Draw(d) => Some(&d.target.image),
        Operation::Clear { target, .. } | Operation::Upload { target, .. } => Some(&target.image),
        Operation::Copy { destination, .. } => Some(&destination.image),
        _ => None,
    }
}
fn full(image: &Image) -> Rect {
    Rect {
        x: 0,
        y: 0,
        w: image.width(),
        h: image.height(),
    }
}
fn size(r: Rect) -> (u32, u32) {
    (r.x + r.w, r.y + r.h)
}
pub(super) fn bounds(env: &Env<'_>, device: &Device, operation: &Operation) -> Rect {
    match operation {
        Operation::Clear { rectangle, .. } => *rectangle,
        Operation::Copy { region, .. } => *region,
        Operation::Upload { target, .. } => full(&target.image),
        Operation::Draw(d) => draw_bounds(env, device, d).unwrap_or_else(|| full(&d.target.image)),
        _ => Rect::default(),
    }
}
fn draw_bounds(env: &Env<'_>, device: &Device, d: &fixed::Draw) -> Option<Rect> {
    use crate::shader::interpreter::{self, NoTextures};
    if d.programmable.as_ref().is_some_and(|p| {
        p.instance_count != 1 || p.parameters[15][0] == 2 || p.streams.iter().any(|s| s.instance)
    }) {
        return None;
    }
    let mut positions = [[0.; 4]; 64];
    let count;
    let point = d.programmable.as_ref().filter(|p| p.parameters[2][3] != 0);
    let point_size = |value: f32| -> Option<f32> {
        let p = point?;
        let lo = f32::from_bits(p.parameters[12][0]);
        let hi = f32::from_bits(p.parameters[12][1]);
        (value.is_finite() && lo.is_finite() && hi.is_finite() && lo >= 0. && lo <= hi)
            .then(|| value.clamp(lo, hi))
    };
    let mut margin = if let Some(p) = point {
        1. + point_size(f32::from_bits(p.parameters[11][3]))? * 0.5
    } else {
        1.
    };
    if !d.vertices.is_empty() {
        if d.vertices.len() > 64 {
            return None;
        }
        count = d.vertices.len();
        for (out, v) in positions.iter_mut().zip(d.vertices.iter()) {
            *out = v.position;
        }
    } else {
        let p = d.programmable.as_ref()?;
        if p.vertex_count > 64 {
            return None;
        }
        count = p.vertex_count as usize;
        if p.parameters[15][0] != 0 {
            for n in 0..p.vertex_count {
                let input = vertex_input(p, n)?;
                positions[n as usize] =
                    programmable::transformed::position(input.v[0], d.state.viewport)?;
            }
        } else {
            let shader = shaders::bound(env, device.shaders.vertex?)?;
            let translation = shader.translation.as_ref().ok()?;
            for n in 0..p.vertex_count {
                let input = vertex_input(p, n)?;
                let output =
                    interpreter::run(&shader.parsed, &input, &p.vertex_constants, &mut NoTextures)
                        .ok()?;
                if point.is_some() && translation.writes_point_size {
                    margin = margin.max(1. + point_size(output.point_size)? * 0.5);
                }
                let mut position = output.position;
                position[0] += position[3] / d.state.viewport.width;
                position[1] -= position[3] / d.state.viewport.height;
                positions[n as usize] = position;
            }
        }
    }
    let v = d.state.viewport;
    let mut lo = [f32::INFINITY; 2];
    let mut hi = [f32::NEG_INFINITY; 2];
    for p in &positions[..count] {
        if p.iter().any(|x| !x.is_finite()) || p[3] <= 0. {
            return None;
        }
        let screen = [
            v.x + (p[0] / p[3] + 1.) * v.width * 0.5,
            v.y + (1. - p[1] / p[3]) * v.height * 0.5,
        ];
        for n in 0..2 {
            if !screen[n].is_finite() {
                return None;
            }
            lo[n] = lo[n].min(screen[n]);
            hi[n] = hi[n].max(screen[n]);
        }
    }
    if !margin.is_finite() {
        return None;
    }
    let x = (lo[0] - margin).floor().max(v.x).max(0.) as u32;
    let y = (lo[1] - margin).floor().max(v.y).max(0.) as u32;
    let r = Rect {
        x,
        y,
        w: ((hi[0] + margin).ceil().min(v.x + v.width) as u32).saturating_sub(x),
        h: ((hi[1] + margin).ceil().min(v.y + v.height) as u32).saturating_sub(y),
    }
    .clipped(full(&d.target.image));
    Some(d.state.scissor.map_or(r, |s| r.clipped(s)))
}
pub(in crate::d3d9) fn vertex_input(
    p: &adv_device::gpu::ProgrammableDraw,
    n: u32,
) -> Option<crate::shader::interpreter::Inputs> {
    let mut input = crate::shader::interpreter::Inputs::default();
    for stream in &p.streams {
        for a in &stream.attributes {
            if a.format == AttributeFormat::Bgra8Unorm {
                let mut bytes = [0; 4];
                stream.bytes.read_vertex(
                    n as usize,
                    stream.stride as usize,
                    a.offset as usize,
                    &mut bytes,
                )?;
                input.v[a.location as usize] =
                    [bytes[2], bytes[1], bytes[0], bytes[3]].map(|v| f32::from(v) / 255.);
                continue;
            }
            if !matches!(
                a.format,
                AttributeFormat::Float1
                    | AttributeFormat::Float2
                    | AttributeFormat::Float3
                    | AttributeFormat::Float4
            ) {
                return None;
            }
            let mut v = [0., 0., 0., 1.];
            let mut attribute = [0; 16];
            let bytes = &mut attribute[..a.format.size() as usize];
            stream.bytes.read_vertex(
                n as usize,
                stream.stride as usize,
                a.offset as usize,
                bytes,
            )?;
            for (component, bytes) in bytes.as_chunks::<4>().0.iter().enumerate() {
                v[component] = f32::from_le_bytes(*bytes);
            }
            *input.v.get_mut(a.location as usize)? = v;
        }
    }
    Some(input)
}
#[derive(Default)]
struct Samples {
    base: u128,
    reads: Vec<(sampling::Read, u128)>,
}
/// For each of an entry's sampled reads, the generation of its source this
/// frame had written when the read was recorded.
type Generations = Vec<Option<usize>>;
/// Each generation's change this frame, and the previous frame's generation
/// it was compared with.
type Changes = HashMap<(Content, usize), (Region, Option<usize>)>;
impl Samples {
    /// Each changed source region maps through the proven sampling, rectangle
    /// by rectangle, into `damage`. A read of another generation than the
    /// one its previous frame's read compares with is unknown.
    fn changed(
        &self,
        gens: &Generations,
        old: &Self,
        old_gens: &Generations,
        changes: &Changes,
        bounds: Rect,
        damage: &mut Region,
    ) -> Option<()> {
        if self.base != old.base || self.reads.len() != old.reads.len() {
            return None;
        }
        let mut local = Region::empty(damage.size());
        for (n, ((read, version), (previous, before))) in
            self.reads.iter().zip(&old.reads).enumerate()
        {
            if read != previous {
                return None;
            }
            if version != before {
                let (Some(generation), Some(old)) =
                    (gens.get(n).copied()?, old_gens.get(n).copied()?)
                else {
                    return None;
                };
                let (region, matched) = changes.get(&(read.source, generation))?;
                if *matched != Some(old) {
                    return None;
                }
                for r in rects(region) {
                    local.add(read.mapping.map(r, bounds).into());
                }
            }
        }
        damage.union(&local);
        Some(())
    }
}
/// One generation of a content: its writes from a proven overwrite (or the
/// frame start) up to the next.
#[derive(Default)]
struct Sequence {
    entries: Vec<(u128, Rect)>,
    samples: HashMap<usize, Arc<Samples>>,
    gens: HashMap<usize, Generations>,
    index: HashMap<u128, Vec<usize>>,
    cursor: usize,
    damage: Region,
    pending: Vec<(usize, usize)>,
    last_write: u64,
    full: Rect,
    view: Rect,
    shape: u128,
    slot: u128,
    independent: bool,
}
static EMPTY: std::sync::LazyLock<Sequence> = std::sync::LazyLock::new(Sequence::default);
#[derive(Default)]
struct History {
    /// The previous frame's generations and this frame's.
    previous: Vec<Sequence>,
    next: Vec<Sequence>,
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
        self.next.push(Sequence::default());
    }
    fn record(&mut self, key: u128, bounds: Rect, samples: Arc<Samples>, gens: Generations) {
        let previous = self
            .matched
            .last()
            .copied()
            .flatten()
            .map_or(&*EMPTY, |m| &self.previous[m]);
        let s = self.next.last_mut().unwrap();
        let at = s.entries.len();
        if at == 0 {
            s.damage = Region::empty(size(s.full));
        }
        let old = previous.index.get(&key).and_then(|positions| {
            let first = positions.partition_point(|&n| n < s.cursor);
            positions[first..]
                .iter()
                .copied()
                .find(|&n| previous.entries[n].1 == bounds)
        });
        if let Some(old) = old {
            for &(_, r) in &previous.entries[s.cursor..old] {
                s.damage.add(r.into());
            }
            s.cursor = old + 1;
            if let Some(samples) = previous.samples.get(&old) {
                s.samples.insert(at, samples.clone());
                s.gens.insert(at, previous.gens[&old].clone());
            }
        } else {
            if previous
                .entries
                .get(s.cursor)
                .is_some_and(|e| e.1 == bounds)
                && previous
                    .samples
                    .get(&s.cursor)
                    .is_some_and(|old| old.base == samples.base)
                && !samples.reads.is_empty()
            {
                s.pending.push((s.cursor, at));
                s.cursor += 1;
            } else {
                s.damage.add(bounds.into());
            }
            if !samples.reads.is_empty() {
                s.samples.insert(at, samples);
                s.gens.insert(at, gens);
            }
        }
        s.index.entry(key).or_default().push(at);
        s.entries.push((key, bounds));
    }
    /// What generation `g` changed against the previous generation it
    /// compares with, or everything when it compares with none.
    fn changed(&self, g: usize, force: bool, changes: &Changes) -> Region {
        let next = &self.next[g];
        let size = size(next.full);
        let previous = match self.matched[g] {
            Some(m) if !force && self.serial != 0 => &self.previous[m],
            _ => return Region::whole(size),
        };
        if previous.shape != next.shape {
            return Region::whole(size);
        }
        let mut changed = next.damage.clone();
        for &(_, bounds) in &previous.entries[next.cursor..] {
            changed.add(bounds.into());
        }
        for &(old, new) in &next.pending {
            let (_, bounds) = next.entries[new];
            if next.samples[&new]
                .changed(
                    &next.gens[&new],
                    &previous.samples[&old],
                    &previous.gens[&old],
                    changes,
                    bounds,
                    &mut changed,
                )
                .is_none()
            {
                changed.add(bounds.into());
            }
        }
        // Appending to the previous frame's only, hence final, version
        // changes no more than this frame's writes either.
        if !next.independent && self.previous.len() == 1 {
            let mut written = Region::empty(size);
            for &(_, bounds) in &next.entries {
                written.add(bounds.into());
            }
            changed.intersect(&written);
        }
        changed
    }
    /// After every generation's change: what the final version changed since
    /// the previous frame's, and what its slot must redraw.
    fn settle(&mut self, force: bool, last: Region) -> (Region, Region) {
        let next = self.next.last().unwrap();
        let size = size(next.full);
        let compared = !force
            && !self.previous.is_empty()
            && self.matched.last() == Some(&Some(self.previous.len() - 1));
        let changed = if compared { last } else { Region::whole(size) };
        self.serial += 1;
        for stale in self.stale.values_mut() {
            stale.union(&changed);
        }
        self.stale.retain(|_, stale| !stale.is_whole());
        let redraw = self
            .stale
            .insert(next.slot, Region::empty(size))
            .unwrap_or_else(|| Region::whole(size));
        self.previous = std::mem::take(&mut self.next);
        self.matched.clear();
        (changed, redraw)
    }
}
type Write = (Content, u128, Rect, Rect, u128, bool, Arc<Samples>);
pub(super) struct Record {
    writes: Vec<Write>,
    /// What the write's pixels read from each source: a proven mapping, or
    /// the whole source.
    demands: Vec<(Content, Option<sampling::Mapping>)>,
    bounds: Rect,
    view: Rect,
    upload: bool,
    clear: bool,
}
/// Scissors per write. A draw spanning several distant changes is submitted
/// once per scissor it meets; past this many, coarser cells merge them.
const PARTS: usize = 8;
#[derive(Default)]
pub(super) struct Analysis {
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
    pub planned: bool,
    epoch: u64,
    ordinal: u64,
}
impl Analysis {
    pub fn written(&self, content: &Content) -> bool {
        self.histories
            .get(content)
            .is_some_and(|h| !h.next.is_empty())
    }
    pub fn record(
        &mut self,
        p: &key::Prepared,
        command: u128,
        contents: &Contents,
        slot: Option<u128>,
    ) -> Arc<Record> {
        let image = image(&p.operation).unwrap();
        let slot = slot.unwrap_or_else(|| hash(image.id()));
        let mut demands = Vec::new();
        for &k in &p.reads {
            let mut proven = p.samples.iter().filter(|r| r.source == k).peekable();
            if proven.peek().is_none() {
                let depth = (k.2 != 0)
                    .then(|| match &p.operation {
                        Operation::Draw(d) => d.target.depth(),
                        _ => None,
                    })
                    .flatten();
                demands.push((
                    k,
                    depth.map(|d| {
                        sampling::Mapping::identity(Rect {
                            x: 0,
                            y: 0,
                            w: d.width(),
                            h: d.height(),
                        })
                    }),
                ));
            }
            demands.extend(proven.map(|r| (k, Some(r.mapping))));
        }
        let mut record = Record {
            writes: Vec::new(),
            demands,
            bounds: p.bounds.clipped(full(image)),
            view: full(image),
            upload: matches!(p.operation, Operation::Upload { .. }),
            clear: matches!(p.operation, Operation::Clear { .. }),
        };
        for &(k, overwrite) in &p.writes {
            let current = self.histories.get(&k).and_then(History::current);
            let depth = match &p.operation {
                Operation::Draw(d) => d.target.depth(),
                Operation::Clear { target, .. } => target.depth(),
                _ => None,
            };
            let bounds = if k.2 == 0 {
                full(image)
            } else {
                let depth = depth.unwrap();
                Rect {
                    x: 0,
                    y: 0,
                    w: depth.width(),
                    h: depth.height(),
                }
            };
            let shape = hash((
                rect(bounds),
                if k.2 == 0 {
                    image.alpha_mode() as u8
                } else {
                    depth.unwrap().format() as u8
                },
            ));
            let full = bounds;
            let bounds = p.bounds.clipped(full);
            let independent =
                (overwrite && bounds == full) || current.is_some_and(|s| s.independent);
            // Only drawing can depend on the old values inside its write bounds.
            // Clears and copies overwrite that region even when the attachment is larger.
            let key = hash((
                p.write_key(k, command),
                (matches!(p.operation, Operation::Draw(_)) && !independent)
                    .then(|| contents.get(&k).copied().unwrap_or(0)),
            ));
            let samples = if p.samples.is_empty() {
                Samples::default()
            } else {
                let external: Vec<_> = p
                    .reads
                    .iter()
                    .filter(|k| !p.samples.iter().any(|r| r.source == **k))
                    .map(|k| (*k, contents.get(k).copied().unwrap_or(0)))
                    .collect();
                Samples {
                    base: hash((
                        p.key,
                        external,
                        (matches!(p.operation, Operation::Draw(_)) && !independent)
                            .then(|| contents.get(&k).copied().unwrap_or(0)),
                    )),
                    reads: p
                        .samples
                        .iter()
                        .map(|r| (*r, contents.get(&r.source).copied().unwrap_or(0)))
                        .collect(),
                }
            };
            record
                .writes
                .push((k, key, bounds, full, shape, overwrite, Arc::new(samples)));
        }
        let record = Arc::new(record);
        self.replay(&record, slot);
        record
    }
    pub fn independent(&self, k: &Content) -> bool {
        self.histories
            .get(k)
            .and_then(History::current)
            .is_some_and(|s| s.independent)
    }
    pub fn replay(&mut self, record: &Arc<Record>, slot: u128) {
        self.ordinal += 1;
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
        self.nodes.push(schedule::Node {
            slot,
            record: record.clone(),
            producers,
            previous,
        });
        for &(k, key, bounds, full, shape, overwrite, ref samples) in &record.writes {
            // Which generation of each sampled source the read meets.
            let gens = samples
                .reads
                .iter()
                .map(|(r, _)| {
                    self.histories
                        .get(&r.source)
                        .and_then(|h| h.next.len().checked_sub(1))
                })
                .collect();
            let history = self.histories.entry(k).or_default();
            let fresh = overwrite && bounds == full;
            if fresh || history.next.is_empty() {
                history.start(key, bounds, fresh);
            }
            let s = history.next.last_mut().unwrap();
            s.full = full;
            s.last_write = self.ordinal;
            s.view = record.view;
            s.shape = shape;
            s.slot = slot;
            if s.entries.is_empty() {
                s.independent = fresh;
            }
            history.record(key, bounds, samples.clone(), gens);
        }
    }
    /// Plans the frame; returns how many final chains a late demand made
    /// execute whole.
    pub fn plan(&mut self, force: bool) -> u64 {
        if self.planned {
            return 0;
        }
        self.epoch += 1;
        // Generations in the order their last writes ran, so each read meets
        // its source's change already known.
        let mut order: Vec<_> = self
            .histories
            .iter()
            .flat_map(|(k, h)| {
                h.next
                    .iter()
                    .enumerate()
                    .map(|(g, s)| (*k, g, s.last_write))
            })
            .collect();
        order.sort_unstable_by_key(|&(_, _, last)| last);
        let mut changes = Changes::default();
        for (content, g, _) in order {
            let h = &self.histories[&content];
            let changed = h.changed(g, force || h.seen + 1 != self.epoch, &changes);
            changes.insert((content, g), (changed, h.matched[g]));
        }
        let mut stale = HashMap::default();
        for (content, h) in &mut self.histories {
            if h.next.is_empty() {
                continue;
            }
            let last = h.next.len() - 1;
            let force = force || h.seen + 1 != self.epoch;
            let slot = h.next[last].slot;
            let (changed, redraw) = h.settle(force, changes[&(*content, last)].0.clone());
            h.seen = self.epoch;
            if content.2 == 0 {
                self.changed
                    .entry(slot)
                    .and_modify(|r| r.union(&changed))
                    .or_insert(changed);
            }
            stale.insert(*content, redraw);
        }
        let (clips, promotions) = schedule::schedule(&self.nodes, &self.done, &stale);
        self.clips = clips;
        for (i, node) in self.nodes.iter().enumerate() {
            let view = node.record.view;
            let r = self
                .execution
                .entry(node.slot)
                .or_insert_with(|| Region::empty(size(view)));
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
                _ => *r = Region::whole(r.size()),
            }
        }
        self.planned = true;
        promotions
    }
    /// The scissors write `n` of the frame executes in; `None` runs it whole.
    pub fn clip(&self, n: usize) -> Option<&[Rect]> {
        self.clips.get(n)?.as_deref()
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
            && s.pending.is_empty()
            && s.damage.is_empty()
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
                h.previous = std::mem::take(&mut h.next);
            }
            h.next.clear();
            h.matched.clear();
        }
        self.ordinal = 0;
        self.clips.clear();
        self.changed.clear();
        self.nodes.clear();
        self.last_writer.clear();
        self.done.clear();
        self.execution.clear();
        self.planned = false;
    }
    pub fn invalidate(&mut self) {
        *self = Self::default();
    }
    pub fn forget(&mut self, pointer: u32) {
        self.histories.retain(|k, _| k.3 != pointer);
    }
}

#[cfg(test)]
#[path = "damage_unit_tests.rs"]
mod tests;
