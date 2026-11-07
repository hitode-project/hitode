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
}

/// Scissors per write; `None` executes it whole.
pub(super) type Clips = Vec<Option<Vec<Rect>>>;

/// The plan, and how many chains it had to execute whole because a late
/// demand reached writes already planned.
pub(super) fn schedule(
    nodes: &[Node],
    done: &HashSet<(usize, Content)>,
    stale: &HashMap<Content, Region>,
) -> (Clips, u64) {
    let graph = Graph::new(nodes, done, stale);
    let mut whole = graph.whole.clone();
    let mut promotions = 0;
    loop {
        match graph.pass(&whole) {
            Ok(clips) => return (clips, promotions),
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
/// Whether `outer` marks every cell of `inner`; both share a size.
fn covers(outer: &Region, inner: &Region) -> bool {
    let mut union = outer.clone();
    union.union(inner);
    union == *outer
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
    /// The group holding each content's final chain.
    of: HashMap<Content, usize>,
    /// Each content's last write, which produces its final version.
    last: HashMap<Content, usize>,
    /// Each group's cells before demand: the stale cells of its contents.
    initial: Vec<Region>,
    /// Groups that execute whole: a flush already ran some of their writes.
    whole: Vec<bool>,
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
        let group = (0..nodes.len())
            .map(|i| chains(i).next().map(|n| of[&keys[n]]))
            .collect();
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
        let last = writers
            .iter()
            .map(|(k, list)| (*k, *list.last().unwrap()))
            .collect();
        Self {
            nodes,
            done,
            group,
            of,
            last,
            initial,
            whole,
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
                        for rect in rects(r) {
                            p.region[g].add(rect.into());
                        }
                    }
                }
                let frozen = p.parts[g].as_ref().is_some_and(|parts| {
                    covers(&rect_cells(p.region[g].size(), parts), &p.region[g])
                });
                if !frozen {
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
        self.parts[g] = Some(parts.clone());
        let members: Vec<usize> = (current + 1..self.graph.nodes.len())
            .filter(|&c| self.graph.group[c] == Some(g))
            .collect();
        for &c in &members {
            self.clips[c] = self.cut(g, c)?;
        }
        for &c in &members {
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
        // Uploads replace the whole image; only a whole chain may run one.
        if record.upload && !e.is_empty() {
            return if self.region[g].is_whole() {
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
        let executed = e.clone().unwrap_or_else(|| vec![node.record.bounds]);
        for (j, &(source, mapping)) in node.record.demands.iter().enumerate() {
            let Some(p) = node.producers[j] else {
                continue;
            };
            for r in &executed {
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
        let executed = e.clone().unwrap_or_else(|| vec![node.record.bounds]);
        for w in &node.record.writes {
            let g = self.graph.of[&w.0];
            if self.graph.group[c] == Some(g) {
                continue;
            }
            for r in &executed {
                self.region[g].add(r.clipped(w.2).into());
            }
            let covered = self.parts[g].as_ref().is_none_or(|parts| {
                covers(&rect_cells(self.region[g].size(), parts), &self.region[g])
            });
            if !covered {
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
        let mut asked = Region::empty(size(full));
        match rect {
            Some(r) => asked.add(r.into()),
            None => asked = Region::whole(size(full)),
        }
        need.union(&asked);
        if p > current {
            let executed = match &self.clips[p] {
                Some(parts) => rect_cells(size(full), parts),
                None => Region::whole(size(full)),
            };
            if !covers(&executed, &asked) {
                return Err(growing.or(self.graph.group[p]).unwrap());
            }
        }
        Ok(())
    }
}
