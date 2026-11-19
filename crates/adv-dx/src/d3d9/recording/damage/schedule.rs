//! What each recorded write executes (GFX-REC-5). Every write reads the
//! version its sources had when it was recorded, so demand runs backward over
//! record order once: a write is planned after every later read of what it
//! wrote, and its scissors are final before its inputs are asked for.
//!
//! A content's last overwrite starts its final chain, which the slot retains:
//! the chain executes the stale cells and every cell a read of an earlier
//! chain version needs, all with one set of scissors so the retained pixels
//! end as a full replay leaves them. Reads of the final version need nothing:
//! outside the chain's scissors the slot already holds it. Earlier versions are transient and execute only what their
//! readers need; the chain then also covers the cells they wrote in place.
//! Writes that put several final chains in one command join them.
use super::*;
use adv_device::surface::{PartsScratch, for_each_part, for_each_rect};

pub(super) struct Node {
    pub slot: u128,
    pub record: Record,
    /// For each demand, the write that produced the version it reads.
    pub producers: SmallVec<[Option<usize>; 4]>,
    /// For each write, the write before it to the same content.
    pub previous: SmallVec<[Option<usize>; 3]>,
    pub generations: SmallVec<[Generation; 3]>,
    pub submissions: u64,
}

pub(super) type ClipRects = SmallVec<[Rect; PARTS]>;
/// Scissors per write; `None` executes it whole.
pub(super) type Clips = Vec<Option<ClipRects>>;

#[derive(Default)]
pub(super) struct Scratch {
    graph: GraphScratch,
    pass: PassScratch,
}

#[derive(Default)]
struct GraphScratch {
    writers: HashMap<Content, Vec<usize>>,
    writer_pool: Vec<Vec<usize>>,
    start: HashMap<Content, usize>,
    keys: Vec<Content>,
    index: HashMap<Content, usize>,
    parent: Vec<usize>,
    ids: HashMap<usize, usize>,
    of: HashMap<Content, usize>,
    members: Vec<Vec<usize>>,
    member_pool: Vec<Vec<usize>>,
    extent: Vec<(u32, u32)>,
    initial: Vec<Region>,
    whole: Vec<bool>,
    pass_whole: Vec<bool>,
    last: HashMap<Content, usize>,
    intermediate: HashSet<usize>,
    group: Vec<Option<usize>>,
}

#[derive(Default)]
struct PassScratch {
    need: HashMap<(usize, Content), Region>,
    region: Vec<Region>,
    parts: Vec<Option<ClipRects>>,
    dirty: Vec<bool>,
    parts_scratch: PartsScratch,
    through_scratch: Vec<Rect>,
    check_region: Region,
}

impl PassScratch {
    fn clear(&mut self, pool: &mut Vec<Region>) {
        for (_, region) in self.need.drain() {
            recycle_region(pool, region);
        }
        for region in self.region.drain(..) {
            recycle_region(pool, region);
        }
        self.parts.clear();
        self.dirty.clear();
    }
}

/// The plan and whole-chain promotions: intermediate external reads, then
/// late demands reaching writes already planned.
pub(super) fn schedule(
    nodes: &[Node],
    done: &HashSet<(usize, Content)>,
    stale: &HashMap<Content, Region>,
    clips: &mut Clips,
    scratch: &mut Scratch,
    pool: &mut Vec<Region>,
) -> [u64; 2] {
    let graph = Graph::new(nodes, done, stale, &mut scratch.graph, pool);
    scratch.graph.pass_whole.clear();
    scratch.graph.pass_whole.extend_from_slice(&graph.whole);
    let mut promotions = 0;
    loop {
        let result = graph.pass(&scratch.graph.pass_whole, clips, &mut scratch.pass, pool);
        match result {
            Ok(()) => {
                let intermediate = graph.intermediate;
                graph.recycle(&mut scratch.graph, pool);
                return [intermediate, promotions];
            }
            Err(g) => {
                debug_assert!(!scratch.graph.pass_whole[g]);
                scratch.graph.pass_whole[g] = true;
                promotions += 1;
            }
        }
    }
}

fn overwrites(w: &Write) -> bool {
    w.5 && w.2 == w.3
}
fn clipped(parts: &[Rect], bounds: Rect) -> ClipRects {
    parts
        .iter()
        .map(|r| r.clipped(bounds))
        .filter(|r| !r.empty())
        .collect()
}

struct Graph<'a> {
    nodes: &'a [Node],
    /// Writes of contents a flush already executed whole.
    done: &'a HashSet<(usize, Content)>,
    /// The final chain group each write belongs to.
    group: Vec<Option<usize>>,
    members: Vec<Vec<usize>>,
    /// The group holding each content's final chain.
    of: HashMap<Content, usize>,
    /// Each content's last write, which produces its final version.
    last: HashMap<Content, usize>,
    /// Each group's cells before demand: the stale cells of its contents.
    initial: Vec<Region>,
    /// Groups that execute whole after an early flush or intermediate read.
    whole: Vec<bool>,
    intermediate: u64,
}
impl<'a> Graph<'a> {
    fn new(
        nodes: &'a [Node],
        done: &'a HashSet<(usize, Content)>,
        stale: &HashMap<Content, Region>,
        scratch: &mut GraphScratch,
        pool: &mut Vec<Region>,
    ) -> Self {
        let writers = &mut scratch.writers;
        writers.clear();
        for (i, node) in nodes.iter().enumerate() {
            for w in &node.record.writes {
                writers
                    .entry(w.0)
                    .or_insert_with(|| {
                        let mut v = scratch.writer_pool.pop().unwrap_or_default();
                        v.clear();
                        v
                    })
                    .push(i);
            }
        }
        let start = &mut scratch.start;
        start.clear();
        for (k, list) in writers.iter() {
            let overwritten = |i: &&usize| {
                nodes[**i]
                    .record
                    .writes
                    .iter()
                    .any(|w| w.0 == *k && overwrites(w))
            };
            start.insert(
                *k,
                list.iter().rev().find(overwritten).map_or(list[0], |i| *i),
            );
        }
        let keys = &mut scratch.keys;
        keys.clear();
        keys.extend(writers.keys().copied());
        let index = &mut scratch.index;
        index.clear();
        for (n, k) in keys.iter().enumerate() {
            index.insert(*k, n);
        }
        let parent = &mut scratch.parent;
        parent.clear();
        parent.extend(0..keys.len());
        fn find(parent: &mut [usize], mut n: usize) -> usize {
            while parent[n] != n {
                parent[n] = parent[parent[n]];
                n = parent[n];
            }
            n
        }
        let start_ref = &*start;
        let index_ref = &*index;
        let chains = |i: usize| {
            nodes[i]
                .record
                .writes
                .iter()
                .filter(move |w| i >= start_ref[&w.0])
                .map(move |w| index_ref[&w.0])
        };
        for i in 0..nodes.len() {
            let mut members = chains(i);
            if let Some(a) = members.next() {
                for b in members {
                    let (a, b) = (find(parent, a), find(parent, b));
                    parent[b] = a;
                }
            }
        }
        let ids = &mut scratch.ids;
        ids.clear();
        let mut of = std::mem::take(&mut scratch.of);
        of.clear();
        for (n, k) in keys.iter().enumerate() {
            let root = find(parent, n);
            let next = ids.len();
            of.insert(*k, *ids.entry(root).or_insert(next));
        }
        let mut group = std::mem::take(&mut scratch.group);
        group.clear();
        group.extend((0..nodes.len()).map(|i| chains(i).next().map(|n| of[&keys[n]])));
        let mut members = std::mem::take(&mut scratch.members);
        for mut v in members.drain(..) {
            v.clear();
            scratch.member_pool.push(v);
        }
        for _ in 0..ids.len() {
            let mut v = scratch.member_pool.pop().unwrap_or_default();
            v.clear();
            members.push(v);
        }
        for (i, g) in group.iter().enumerate() {
            if let Some(g) = g {
                members[*g].push(i);
            }
        }
        let extent = &mut scratch.extent;
        extent.clear();
        extent.resize(ids.len(), (0, 0));
        for node in nodes {
            for w in &node.record.writes {
                let e = &mut extent[of[&w.0]];
                let s = size(w.3);
                *e = (e.0.max(s.0), e.1.max(s.1));
            }
        }
        let mut initial = std::mem::take(&mut scratch.initial);
        for region in initial.drain(..) {
            recycle_region(pool, region);
        }
        for &e in extent.iter() {
            initial.push(take_region(pool, e, false));
        }
        for (k, r) in stale {
            if let Some(&g) = of.get(k) {
                for_each_rect(r, |rect| initial[g].add(rect.into()));
            }
        }
        let mut whole = std::mem::take(&mut scratch.whole);
        whole.clear();
        whole.resize(ids.len(), false);
        for (k, list) in writers.iter() {
            if list.iter().any(|&i| done.contains(&(i, *k))) {
                whole[of[k]] = true;
            }
        }
        let mut last = std::mem::take(&mut scratch.last);
        last.clear();
        for (k, list) in writers.iter() {
            last.insert(*k, *list.last().unwrap());
        }
        for (_, mut list) in writers.drain() {
            list.clear();
            scratch.writer_pool.push(list);
        }
        let intermediate = &mut scratch.intermediate;
        intermediate.clear();
        for node in nodes {
            for ((source, _), producer) in node.record.demands.iter().zip(&node.producers) {
                if let Some(p) = producer
                    && *p >= start[source]
                    && *p != last[source]
                    && !node.record.writes.iter().any(|w| w.0 == *source)
                {
                    intermediate.insert(of[source]);
                }
            }
        }
        let intermediate_count = intermediate
            .iter()
            .filter(|&&g| !whole[g] && !initial[g].is_whole())
            .count() as u64;
        for &g in intermediate.iter() {
            whole[g] = true;
        }
        Self {
            nodes,
            done,
            group,
            members,
            of,
            last,
            initial,
            whole,
            intermediate: intermediate_count,
        }
    }
    fn recycle(mut self, scratch: &mut GraphScratch, pool: &mut Vec<Region>) {
        self.group.clear();
        scratch.group = self.group;
        for mut v in self.members.drain(..) {
            v.clear();
            scratch.member_pool.push(v);
        }
        scratch.members = self.members;
        self.of.clear();
        scratch.of = self.of;
        self.last.clear();
        scratch.last = self.last;
        for region in self.initial.drain(..) {
            recycle_region(pool, region);
        }
        scratch.initial = self.initial;
        self.whole.clear();
        scratch.whole = self.whole;
    }
    fn pass(
        &self,
        whole: &[bool],
        clips: &mut Clips,
        scratch: &mut PassScratch,
        pool: &mut Vec<Region>,
    ) -> Result<(), usize> {
        scratch.clear(pool);
        for (r, &w) in self.initial.iter().zip(whole) {
            let mut region = take_region(pool, r.size(), w);
            if !w {
                region.union(r);
            }
            scratch.region.push(region);
        }
        scratch.parts.resize_with(self.initial.len(), || None);
        scratch.dirty.resize(self.initial.len(), false);
        clips.clear();
        clips.resize_with(self.nodes.len(), || None);
        let mut p = Pass {
            graph: self,
            scratch,
            clips,
            pool,
        };
        let result = (|| {
            for i in (0..self.nodes.len()).rev() {
                let node = &self.nodes[i];
                if node
                    .record
                    .writes
                    .iter()
                    .all(|w| self.done.contains(&(i, w.0)))
                {
                    continue;
                }
                let e = if let Some(g) = self.group[i] {
                    for w in &node.record.writes {
                        if let Some(r) = p.scratch.need.get(&(i, w.0)) {
                            p.scratch.dirty[g] |= p.scratch.region[g].union_clipped(r, w.3.into());
                        }
                    }
                    if p.scratch.parts[g].is_none() || p.scratch.dirty[g] {
                        p.grow(g, i)?;
                    }
                    let e = p.cut(g, i)?;
                    p.clips[i] = e.clone();
                    e
                } else {
                    let mut r = take_region(p.pool, size(node.record.view), false);
                    let mut outside = false;
                    for w in &node.record.writes {
                        if let Some(need) = p.scratch.need.get(&(i, w.0)) {
                            for_each_rect(need, |rect| {
                                let rect = rect.clipped(w.2);
                                outside |= rect.clipped(node.record.view) != rect;
                                r.add(rect.into());
                            });
                        }
                    }
                    let e = if outside || (node.record.upload && !r.is_empty()) {
                        None
                    } else {
                        let mut parts = ClipRects::new();
                        for_each_part(&r, PARTS, &mut p.scratch.parts_scratch, |rect| {
                            parts.push(rect);
                        });
                        Some(clipped(&parts, node.record.bounds))
                    };
                    recycle_region(p.pool, r);
                    p.clips[i] = e.clone();
                    e
                };
                p.propagate(i, &e, i, None)?;
                p.contract(i, &e)?;
            }
            Ok(())
        })();
        p.scratch.clear(p.pool);
        result
    }
}

struct Pass<'a, 'g, 's> {
    graph: &'g Graph<'a>,
    scratch: &'s mut PassScratch,
    clips: &'s mut Clips,
    pool: &'s mut Vec<Region>,
}
impl Pass<'_, '_, '_> {
    /// Recuts group `g`'s scissors from its region and applies them to its
    /// writes already planned after `current`, asking their inputs for the
    /// cells they gained.
    fn grow(&mut self, g: usize, current: usize) -> Result<(), usize> {
        let size = self.scratch.region[g].size();
        let mut parts = ClipRects::new();
        for_each_part(
            &self.scratch.region[g],
            PARTS,
            &mut self.scratch.parts_scratch,
            |r| parts.push(r),
        );
        self.scratch.region[g].reset(size, false);
        for &rect in &parts {
            self.scratch.region[g].add(rect.into());
        }
        self.scratch.parts[g] = Some(parts);
        self.scratch.dirty[g] = false;
        let members = &self.graph.members[g];
        let members = &members[members.partition_point(|&c| c <= current)..];
        for &c in members {
            self.clips[c] = self.cut(g, c)?;
        }
        for &c in members {
            let e = self.clips[c].clone();
            self.propagate(c, &e, current, Some(g))?;
            self.contract(c, &e)?;
        }
        Ok(())
    }
    /// Write `c`'s scissors from its group's.
    fn cut(&self, g: usize, c: usize) -> Result<Option<ClipRects>, usize> {
        let record = &self.graph.nodes[c].record;
        let e = clipped(self.scratch.parts[g].as_ref().unwrap(), record.bounds);
        // Uploads cannot be scissored. Every cell they write must be in the chain's plan.
        if record.upload && !e.is_empty() {
            return if self.scratch.region[g].covers_rect(record.bounds.into()) {
                Ok(None)
            } else {
                Err(g)
            };
        }
        Ok(Some(e))
    }
    /// Asks the versions write `c` reads for the cells it executes.
    fn propagate(
        &mut self,
        c: usize,
        e: &Option<ClipRects>,
        current: usize,
        growing: Option<usize>,
    ) -> Result<(), usize> {
        let node = &self.graph.nodes[c];
        let executed = e
            .as_deref()
            .unwrap_or_else(|| std::slice::from_ref(&node.record.bounds));
        for (j, &(source, mapping)) in node.record.demands.iter().enumerate() {
            let Some(p) = node.producers[j] else {
                continue;
            };
            for r in executed {
                let r = r.clipped(node.record.bounds);
                if !r.empty() {
                    self.ask(p, source, mapping.map(|m| m.unmap(r)), current, growing)?;
                }
            }
        }
        let mut through = std::mem::take(&mut self.scratch.through_scratch);
        let result = (|| {
            for (j, w) in node.record.writes.iter().enumerate() {
                let Some(p) = node.previous[j].filter(|_| !overwrites(w)) else {
                    continue;
                };
                // Pixels the write leaves keep the earlier version.
                through.clear();
                if let Some(need) = self.scratch.need.get(&(c, w.0)) {
                    for_each_rect(need, |r| through.push(r));
                }
                for &r in &through {
                    if !r.empty() {
                        self.ask(p, w.0, Some(r), current, growing)?;
                    }
                }
                for r in executed.iter().map(|r| r.clipped(w.2)) {
                    if !r.empty() {
                        self.ask(p, w.0, Some(r), current, growing)?;
                    }
                }
            }
            Ok(())
        })();
        self.scratch.through_scratch = through;
        result
    }
    /// A transient version written in place costs its content's final chain
    /// those cells.
    fn contract(&mut self, c: usize, e: &Option<ClipRects>) -> Result<(), usize> {
        let node = &self.graph.nodes[c];
        let executed = e
            .as_deref()
            .unwrap_or_else(|| std::slice::from_ref(&node.record.bounds));
        for w in &node.record.writes {
            let g = self.graph.of[&w.0];
            if self.graph.group[c] == Some(g) {
                continue;
            }
            for r in executed {
                self.scratch.dirty[g] |= self.scratch.region[g].add_changed(r.clipped(w.2).into());
            }
            if self.scratch.parts[g].is_some() && self.scratch.dirty[g] {
                self.grow(g, c)?;
            }
        }
        Ok(())
    }
    /// Records that version `p` of `k` must be correct in `rect` (all of it
    /// when `None`). A write already planned must already execute it.
    fn ask(
        &mut self,
        p: usize,
        k: Content,
        rect: Option<Rect>,
        current: usize,
        growing: Option<usize>,
    ) -> Result<(), usize> {
        if self.graph.done.contains(&(p, k)) || self.graph.last[&k] == p {
            return Ok(());
        }
        let full = self.graph.nodes[p]
            .record
            .writes
            .iter()
            .find(|w| w.0 == k)
            .unwrap()
            .3;
        let need = self
            .scratch
            .need
            .entry((p, k))
            .or_insert_with(|| take_region(self.pool, size(full), false));
        let asked = rect.unwrap_or(full).into();
        need.add(asked);
        if p > current
            && let Some(parts) = &self.clips[p]
        {
            self.scratch.check_region.reset(size(full), false);
            for &part in parts {
                self.scratch.check_region.add(part.into());
            }
            if !self.scratch.check_region.covers_rect(asked) {
                return Err(growing.or(self.graph.group[p]).unwrap());
            }
        }
        Ok(())
    }
}
