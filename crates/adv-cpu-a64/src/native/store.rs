//! Executable units, link records and page watches.
//!
//! The store places artifacts and holds the data for link, repoint and
//! invalidation operations. The arbiter alone publishes those changes.

use super::*;

#[derive(Clone, Copy)]
pub(super) struct Tier1Unit {
    pub(super) unit: usize,
    pub(super) retired: u32,
}

#[derive(Clone, Copy)]
pub(super) struct Linked {
    pub(super) point: usize,
    pub(super) original: u32,
    pub(super) next: u32,
    pub(super) back: bool,
    pub(super) direct: Option<usize>,
}

impl Linked {
    pub(super) fn branch(&self, target: usize) -> u32 {
        if self.original & 0xFC00_0000 == 0x9400_0000 {
            bl((target as i64 - self.point as i64) as i32)
        } else {
            head::branch(self.point, target)
        }
    }
}

pub(super) struct InternalEdge {
    pub(super) point: usize,
    pub(super) target: usize,
    pub(super) transition: usize,
    pub(super) link: usize,
    pub(super) source: u32,
    pub(super) source_pc: u32,
    pub(super) back: bool,
}

pub(super) struct CodeStore {
    /// Switch-table slots by guest target: source entry, host slot and
    /// original shared stub. Dispatch looks targets up on every miss.
    pub(super) rows: FxHashMap<u32, Vec<(u32, usize, usize)>>,
    /// Entries of units that own switch tables.
    pub(super) table_sources: FxHashSet<u32>,
    pub(super) cache: CodeCache,
    pub(super) enter: Enter,
    pub(super) opt_enter: Enter,
    pub(super) stubs: Stubs,
    /// Cache bytes the stubs occupy; clearing keeps them.
    pub(super) stub_len: usize,
    pub(super) units: FxHashMap<u32, Tier1Unit>,
    /// Unit entries by the guest pages their code came from.
    pub(super) pages: FxHashMap<u32, Vec<u32>>,
    /// Patched link points, with the words they held, by the unit they
    /// branch to: the head of a list in `linked`.
    pub(super) links: FxHashMap<u32, u32>,
    /// Link point, the word it held, and the next point to the same unit.
    pub(super) linked: Vec<Linked>,
    /// Link points of the units the current translation placed.
    pub(super) points: Vec<(usize, u32, bool)>,
    pub(super) back_points: FxHashSet<usize>,
    pub(super) direct_points: FxHashMap<usize, usize>,
    pub(super) internal_edges: FxHashMap<u32, Vec<InternalEdge>>,
    /// Emitted instruction lengths by entry and guest EIP for probation.
    pub(super) tier1_words: FxHashMap<u32, FxHashMap<u32, u32>>,
    pub(super) uses_permissions: bool,
    /// Each placed unit's host address and entry, ascending.
    pub(super) placed: Vec<(usize, u32)>,
    /// Buffers of the last translation.
    pub(super) work: translate::Workspace,
    /// What the units' `interpret` calls run, until the cache clears,
    /// and how often each ran.
    pub(super) helpers: Vec<translate::Helper>,
    pub(super) helper_counts: Vec<u64>,
    /// Helper runs by EIP from before the last clear.
    pub(super) helper_eips: FxHashMap<u32, u64>,
    /// Counts cache clears, which invalidate link points.
    pub(super) generation: u64,
    /// The indirect table the runtime points at.
    pub(super) table: Box<[[u64; 2]]>,
    pub(super) shadow: ShadowStack,
    pub(super) faults: Faults,
}

impl CodeStore {
    pub(super) fn link(&mut self, point: usize, eip: u32) -> Linked {
        let link = Linked {
            point,
            original: self.cache.word(point),
            next: self
                .links
                .insert(eip, self.linked.len() as u32)
                .unwrap_or(u32::MAX),
            back: self.back_points.contains(&point),
            direct: self.direct_points.get(&point).copied(),
        };
        self.linked.push(link);
        link
    }

    pub(super) fn repoint(&mut self, point: usize, target: usize) {
        self.cache.patch(point, head::branch(point, target));
    }

    pub(super) fn invalidate_unit(&mut self, eip: u32) -> bool {
        if self.table_sources.remove(&eip) {
            self.rows.retain(|_, slots| {
                slots.retain(|&(source, _, _)| source != eip);
                !slots.is_empty()
            });
        }
        for &(_, point, stub) in self.rows.get(&eip).into_iter().flatten() {
            self.cache.patch(point, stub as u32);
            self.cache.patch(point + 4, ((stub as u64) >> 32) as u32);
        }
        let mut next = self.links.remove(&eip).unwrap_or(u32::MAX);
        while let Some(link) = self.linked.get(next as usize) {
            self.cache.patch(link.point, link.original);
            next = link.next;
        }
        self.tier1_words.remove(&eip);
        self.units.remove(&eip).is_some()
    }

    pub(super) fn watch_pages(&mut self, eip: u32, spans: &[(u32, u32)]) {
        let mut last = None;
        for &(start, len) in spans.iter().filter(|&&(_, len)| len != 0) {
            let end = (u64::from(start) + u64::from(len) - 1).min(u64::from(u32::MAX));
            for page in (start & !0xfff..=end as u32 & !0xfff).step_by(4096) {
                if last.is_none_or(|last| last < page) {
                    last = Some(page);
                    self.pages.entry(page).or_default().push(eip);
                }
            }
        }
    }

    /// Appends a unit for `eip` whose `sites` are at byte offsets.
    pub(super) fn install_unit(
        &mut self,
        eip: u32,
        words: &[u32],
        sites: &[(usize, Site)],
        retired: u32,
        marks: &[crate::jit_map::Mark],
    ) -> Option<Tier1Unit> {
        let unit = self.cache.append(words)?;
        let from = self.faults.sites.len().saturating_sub(1);
        self.faults
            .sites
            .extend(sites.iter().map(|&(offset, site)| (unit + offset, site)));
        if !self.faults.sites[from..].is_sorted_by_key(|&(at, _)| at) {
            self.faults.sites.sort_unstable_by_key(|&(at, _)| at);
        }
        let entry = Tier1Unit { unit, retired };
        crate::jit_map::record(unit, words, eip, retired, marks, "a64");
        self.units.insert(eip, entry);
        self.placed.push((unit, eip));
        Some(entry)
    }
}

/// Counts the Tier-1 body words for each guest instruction. A taken
/// back edge adds its canonical publication and patch word, excluding
/// the unlinked dispatch and discovery counter.
pub(super) fn tier1_words(
    work: &translate::Workspace,
    trace: &translate::Trace,
) -> FxHashMap<u32, u32> {
    use crate::jit_map::Part;

    let mut words: FxHashMap<_, _> = trace.insts.iter().map(|inst| (inst.pc, 0)).collect();
    let marks = work.marks();
    let cold = marks
        .iter()
        .position(|mark| mark.part == Part::Cold)
        .unwrap_or(marks.len());
    for (i, mark) in marks[..cold].iter().enumerate() {
        let end = marks
            .get(i + 1)
            .map_or(work.words().len() * 4, |mark| mark.offset as usize);
        *words.entry(mark.eip).or_default() += ((end - mark.offset as usize) / 4) as u32;
    }
    for link in work.links().iter().filter(|link| link.back) {
        let at = marks.partition_point(|mark| mark.offset as usize <= link.offset);
        if at > cold {
            let start = marks[..at]
                .iter()
                .rfind(|mark| mark.part == Part::Cold)
                .unwrap();
            *words.entry(start.eip).or_default() +=
                ((link.offset + 4 - start.offset as usize) / 4) as u32;
        } else if let Some(mark) = marks.get(at - 1) {
            *words.entry(mark.eip).or_default() -= ((runtime::BACK_COUNTER + 4) / 4) as u32;
        }
    }
    words
}
