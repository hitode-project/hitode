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

pub(super) struct Node {
    pub slot: u128,
    pub record: Arc<Record>,
    /// For each demand, the write that produced the version it reads.
    pub producers: Vec<Option<usize>>,
    /// For each write, the write before it to the same content.
    pub previous: Vec<Option<usize>>,
    pub generations: Vec<Generation>,
    pub submissions: u64,
}

/// Scissors per write; `None` executes it whole.
pub(super) type Clips = Vec<Option<Vec<Rect>>>;

/// The plan and whole-chain promotions: intermediate external reads, then
/// late demands reaching writes already planned.
pub(super) fn schedule(
    nodes: &[Node],
    done: &HashSet<(usize, Content)>,
    stale: &HashMap<Content, Region>,
) -> (Clips, [u64; 2]) {
    let graph = Graph::new(nodes, done, stale);
    let mut whole = graph.whole.clone();
    let mut promotions = 0;
    loop {
        match graph.pass(&whole) {
            Ok(clips) => return (clips, [graph.intermediate, promotions]),
            Err(g) => {
                debug_assert!(!whole[g]);
                whole[g] = true;
                promotions += 1;
            }
        }
    }
}

fn overwrites(w: &Write) -> bool {
    w.5 && w.2 == w.3
}
fn rect_cells(size: (u32, u32), rects: &[Rect]) -> Region {
    let mut r = Region::empty(size);
    for &rect in rects {
        r.add(rect.into());
    }
    r
}
fn clipped(parts: &[Rect], bounds: Rect) -> Vec<Rect> {
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
    ) -> Self {
        let mut writers: HashMap<Content, Vec<usize>> = HashMap::default();
        for (i, node) in nodes.iter().enumerate() {
            for w in &node.record.writes {
                writers.entry(w.0).or_default().push(i);
            }
        }
        let start: HashMap<Content, usize> = writers
            .iter()
            .map(|(k, list)| {
                let overwritten = |i: &&usize| {
                    nodes[**i]
                        .record
                        .writes
                        .iter()
                        .any(|w| w.0 == *k && overwrites(w))
                };
                (
                    *k,
                    list.iter().rev().find(overwritten).map_or(list[0], |i| *i),
                )
            })
            .collect();
        let keys: Vec<Content> = writers.keys().copied().collect();
        let index: HashMap<Content, usize> =
            keys.iter().enumerate().map(|(n, k)| (*k, n)).collect();
        let mut parent: Vec<usize> = (0..keys.len()).collect();
        fn find(parent: &mut [usize], mut n: usize) -> usize {
            while parent[n] != n {
                parent[n] = parent[parent[n]];
                n = parent[n];
            }
            n
        }
        let (start, index) = (&start, &index);
        let chains = |i: usize| {
            nodes[i]
                .record
                .writes
                .iter()
                .filter(move |w| i >= start[&w.0])
                .map(|w| index[&w.0])
        };
        for i in 0..nodes.len() {
            let mut members = chains(i);
            if let Some(a) = members.next() {
                for b in members {
                    let (a, b) = (find(&mut parent, a), find(&mut parent, b));
                    parent[b] = a;
                }
            }
        }
        let mut ids: HashMap<usize, usize> = HashMap::default();
        let mut of = HashMap::default();
        for (n, k) in keys.iter().enumerate() {
            let root = find(&mut parent, n);
            let next = ids.len();
            of.insert(*k, *ids.entry(root).or_insert(next));
        }
        let group: Vec<_> = (0..nodes.len())
            .map(|i| chains(i).next().map(|n| of[&keys[n]]))
            .collect();
        let mut members = vec![Vec::new(); ids.len()];
        for (i, g) in group.iter().enumerate() {
            if let Some(g) = g {
                members[*g].push(i);
            }
        }
        let mut extent = vec![(0, 0); ids.len()];
        for node in nodes {
            for w in &node.record.writes {
                let e = &mut extent[of[&w.0]];
                let s = size(w.3);
                *e = (e.0.max(s.0), e.1.max(s.1));
            }
        }
        let mut initial: Vec<Region> = extent.iter().map(|e| Region::empty(*e)).collect();
        for (k, r) in stale {
            if let Some(&g) = of.get(k) {
                for rect in rects(r) {
                    initial[g].add(rect.into());
                }
            }
        }
        let mut whole = vec![false; ids.len()];
        for (k, list) in &writers {
            if list.iter().any(|&i| done.contains(&(i, *k))) {
                whole[of[k]] = true;
            }
        }
        let last: HashMap<Content, usize> = writers
            .iter()
            .map(|(k, list)| (*k, *list.last().unwrap()))
            .collect();
        let mut intermediate = HashSet::default();
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
        for &g in &intermediate {
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
    fn pass(&self, whole: &[bool]) -> Result<Clips, usize> {
        let mut p = Pass {
            graph: self,
            need: HashMap::default(),
            region: self
                .initial
                .iter()
                .zip(whole)
                .map(|(r, &w)| {
                    if w {
                        Region::whole(r.size())
                    } else {
                        r.clone()
                    }
                })
                .collect(),
            parts: vec![None; self.initial.len()],
            dirty: vec![false; self.initial.len()],
            clips: vec![None; self.nodes.len()],
        };
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
                    if let Some(r) = p.need.get(&(i, w.0)) {
                        p.dirty[g] |= p.region[g].union_clipped(r, w.3.into());
                    }
                }
                if p.parts[g].is_none() || p.dirty[g] {
                    p.grow(g, i)?;
                }
                let e = p.cut(g, i)?;
                p.clips[i] = e.clone();
                e
            } else {
                let mut r = Region::empty(size(node.record.view));
                let mut outside = false;
                for w in &node.record.writes {
                    if let Some(need) = p.need.get(&(i, w.0)) {
                        for rect in rects(need) {
                            let rect = rect.clipped(w.2);
                            outside |= rect.clipped(node.record.view) != rect;
                            r.add(rect.into());
                        }
                    }
                }
                let e = if outside || (node.record.upload && !r.is_empty()) {
                    None
                } else {
                    let parts: Vec<Rect> = r.parts(PARTS).into_iter().map(Into::into).collect();
                    Some(clipped(&parts, node.record.bounds))
                };
                p.clips[i] = e.clone();
                e
            };
            p.propagate(i, &e, i, None)?;
            p.contract(i, &e)?;
        }
        Ok(p.clips)
    }
}

struct Pass<'a, 'g> {
    graph: &'g Graph<'a>,
    /// Cells each write's version must hold correctly, in its content's extent.
    need: HashMap<(usize, Content), Region>,
    region: Vec<Region>,
    parts: Vec<Option<Vec<Rect>>>,
    /// New cells added since the group's scissors were coarsened.
    dirty: Vec<bool>,
    clips: Clips,
}
impl Pass<'_, '_> {
    /// Recuts group `g`'s scissors from its region and applies them to its
    /// writes already planned after `current`, asking their inputs for the
    /// cells they gained.
    fn grow(&mut self, g: usize, current: usize) -> Result<(), usize> {
        let size = self.region[g].size();
        let parts: Vec<Rect> = self.region[g]
            .parts(PARTS)
            .into_iter()
            .map(Into::into)
            .collect();
        self.region[g] = rect_cells(size, &parts);
        self.parts[g] = Some(parts);
        self.dirty[g] = false;
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
    fn cut(&self, g: usize, c: usize) -> Result<Option<Vec<Rect>>, usize> {
        let record = &self.graph.nodes[c].record;
        let e = clipped(self.parts[g].as_ref().unwrap(), record.bounds);
        // Uploads cannot be scissored. Every cell they write must be in the chain's plan.
        if record.upload && !e.is_empty() {
            return if self.region[g].covers_rect(record.bounds.into()) {
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
        e: &Option<Vec<Rect>>,
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
        for (j, w) in node.record.writes.iter().enumerate() {
            let Some(p) = node.previous[j].filter(|_| !overwrites(w)) else {
                continue;
            };
            // Pixels the write leaves keep the earlier version.
            let through: Vec<Rect> = self.need.get(&(c, w.0)).map(rects).unwrap_or_default();
            for r in through
                .into_iter()
                .chain(executed.iter().map(|r| r.clipped(w.2)))
            {
                if !r.empty() {
                    self.ask(p, w.0, Some(r), current, growing)?;
                }
            }
        }
        Ok(())
    }
    /// A transient version written in place costs its content's final chain
    /// those cells.
    fn contract(&mut self, c: usize, e: &Option<Vec<Rect>>) -> Result<(), usize> {
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
                self.dirty[g] |= self.region[g].add_changed(r.clipped(w.2).into());
            }
            if self.parts[g].is_some() && self.dirty[g] {
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
            .need
            .entry((p, k))
            .or_insert_with(|| Region::empty(size(full)));
        let asked = rect.unwrap_or(full).into();
        need.add(asked);
        if p > current
            && let Some(parts) = &self.clips[p]
            && !rect_cells(size(full), parts).covers_rect(asked)
        {
            return Err(growing.or(self.graph.group[p]).unwrap());
        }
        Ok(())
    }
}
