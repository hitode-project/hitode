//! Which frame cells each block holds as values (A64-OPT-3). A loop head
//! whose live values outnumber the general registers carries only the cells
//! guest instructions read on; arrivals commit the other dirty cells to
//! memory, and the loop reads them back where it reads them. Elsewhere a
//! block holds every cell execution from it can read, unless the cell waits
//! clean in memory since such a head.
use std::collections::BTreeSet;

use super::live::{Live, Set};
use crate::translate::Unit;
use crate::translate::trace::{Edge, Next};

/// Values a loop head may carry in general registers before its frame
/// cells wait in memory: the allocatable registers less a few for the body.
pub(super) const CARRIED: usize = 20;

pub(super) struct Cells<'a> {
    /// Frame cells, by offset and width.
    pub cells: &'a std::collections::BTreeMap<i32, u8>,
    /// Offsets of the frame bytes, ascending.
    pub frame: &'a [i32],
}

impl Cells<'_> {
    pub fn live(&self, set: &Set, start: i32) -> bool {
        let width = i32::from(self.cells[&start]);
        (start..start + width).any(|p| self.frame.binary_search(&p).is_ok_and(|j| set.byte(j)))
    }
    /// Cells loads and commits move: the others stay values throughout.
    fn movable(&self, start: i32) -> bool {
        matches!(self.cells[&start], 4 | 8)
    }
    fn bytes(&self, start: i32) -> impl Iterator<Item = i32> {
        start..start + i32::from(self.cells[&start])
    }

    /// For each instruction that heads a loop and lacks registers, the cells
    /// its arrivals keep as values.
    pub fn carried(
        &self,
        unit: &Unit,
        arrivals: &[Vec<usize>],
        live: &Live,
    ) -> Vec<Option<BTreeSet<i32>>> {
        (0..unit.trace.len())
            .map(|i| {
                if !arrivals[i].iter().any(|&p| p >= i) {
                    return None;
                }
                let regs = (live.entry[i].regs & 0xff).count_ones() as usize + 1;
                let cells = self
                    .cells
                    .keys()
                    .filter(|&&c| self.live(&live.entry[i], c))
                    .count();
                (regs + cells > CARRIED).then(|| {
                    self.cells
                        .keys()
                        .copied()
                        .filter(|&c| !self.movable(c) || self.live(&live.guest[i], c))
                        .collect()
                })
            })
            .collect()
    }

    /// The frame bytes a carried set keeps dirty.
    pub fn kept(&self, carried: &[Option<BTreeSet<i32>>]) -> Vec<Option<BTreeSet<i32>>> {
        carried
            .iter()
            .map(|k| {
                k.as_ref()
                    .map(|k| k.iter().flat_map(|&c| self.bytes(c)).collect())
            })
            .collect()
    }

    /// The cells each instruction's block holds: those execution from it
    /// can read, less the clean ones left in memory at a loop head and not
    /// accessed since.
    pub fn held(
        &self,
        unit: &Unit,
        live: &Live,
        carried: &[Option<BTreeSet<i32>>],
        models: &[super::model::Model],
    ) -> Vec<BTreeSet<i32>> {
        let n = unit.trace.len();
        let left = |kept: &Option<BTreeSet<i32>>| -> BTreeSet<i32> {
            kept.as_ref().map_or_else(BTreeSet::new, |k| {
                self.cells
                    .keys()
                    .copied()
                    .filter(|c| !k.contains(c))
                    .collect()
            })
        };
        let mut absent: Vec<Option<BTreeSet<i32>>> = vec![None; n];
        absent[0] = Some(left(&carried[0]));
        let mut changed = true;
        while changed {
            changed = false;
            for i in 0..n {
                let Some(mut out) = absent[i].clone() else {
                    continue;
                };
                out.retain(|&c| !self.live(&live.touched[i], c));
                let inst = &unit.trace.insts[i];
                let fall = match inst.next {
                    Next::Fall => Some(Edge::Internal(i + 1)),
                    Next::Edge(e) => Some(e),
                    Next::Dynamic => None,
                };
                for edge in [fall, inst.taken].into_iter().flatten() {
                    let Edge::Internal(to) = edge else {
                        continue;
                    };
                    let mut arriving = out.clone();
                    arriving.extend(left(&carried[to]));
                    match &mut absent[to] {
                        Some(next) => {
                            let before = next.len();
                            next.extend(arriving);
                            changed |= next.len() != before;
                        }
                        none => {
                            *none = Some(arriving);
                            changed = true;
                        }
                    }
                }
            }
        }
        (0..n)
            .map(|i| {
                let absent = absent[i].as_ref();
                self.cells
                    .keys()
                    .copied()
                    .filter(|&c| {
                        self.live(&live.entry[i], c)
                            && (absent.is_none_or(|a| !a.contains(&c))
                                || self.bytes(c).any(|b| models[i].dirty.contains(&b)))
                    })
                    .collect()
            })
            .collect()
    }
}
