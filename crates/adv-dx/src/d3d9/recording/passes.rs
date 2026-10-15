//! Backward liveness for complete, contiguous target passes. Uploads remain
//! barriers: their fresh content keys do not prove a full pixel overwrite.
use super::*;
use std::collections::HashSet;

struct Effects {
    pass: u128,
    barrier: bool,
    reads: Vec<Content>,
    writes: Vec<(Content, bool)>,
}
#[derive(Default)]
pub(super) struct Plan {
    effects: Vec<Effects>,
    skipped: Vec<bool>,
    covered: HashSet<Content>,
}
impl Plan {
    pub fn record(
        &mut self,
        operation: &Operation,
        reads: Vec<Content>,
        writes: Vec<(Content, bool)>,
    ) {
        self.effects.push(Effects {
            pass: damage::image(operation).map_or(0, |i| hash(i.view())),
            barrier: matches!(operation, Operation::Upload { .. }),
            reads,
            writes,
        });
    }
    #[cfg(feature = "d3d9-counters")]
    pub fn bytes(reads: usize, writes: usize) -> usize {
        std::mem::size_of::<Effects>()
            + reads * std::mem::size_of::<Content>()
            + writes * std::mem::size_of::<(Content, bool)>()
            + 1
    }
    pub fn truncate(&mut self, len: usize) {
        self.effects.truncate(len);
        self.skipped.clear();
    }
    pub fn skip(&self, n: usize) -> bool {
        self.skipped.get(n).copied().unwrap_or(false)
    }
    pub fn plan(&mut self) -> u64 {
        self.skipped.clear();
        self.skipped.resize(self.effects.len(), false);
        let covered = &mut self.covered;
        covered.clear();
        let mut end = self.effects.len();
        let mut count = 0;
        while end != 0 {
            let last = &self.effects[end - 1];
            let mut start = end - 1;
            if !last.barrier {
                while start != 0
                    && !self.effects[start - 1].barrier
                    && self.effects[start - 1].pass == last.pass
                {
                    start -= 1;
                }
            }
            let group = &self.effects[start..end];
            let dead = !last.barrier
                && group.iter().any(|e| !e.writes.is_empty())
                && group
                    .iter()
                    .flat_map(|e| &e.writes)
                    .all(|(k, _)| covered.contains(k));
            if dead {
                self.skipped[start..end].fill(true);
                count += 1;
            } else {
                for e in group.iter().rev() {
                    for &(k, overwrite) in &e.writes {
                        if overwrite && !e.barrier {
                            covered.insert(k);
                        } else {
                            covered.remove(&k);
                        }
                    }
                    for k in &e.reads {
                        covered.remove(k);
                    }
                }
            }
            end = start;
        }
        count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    // spec: GFX-REC-9
    fn complete_overwrites_protect_reads_partial_writes_mips_and_aspects() {
        let mut m = crate::d3d9::tests::machine();
        let mut env = Env {
            context: m.sys.current,
            cpu: &mut m.cpu,
            sys: &mut m.sys,
            ext: &mut m.ext,
            thunk: 0,
        };
        let (_, _, _, id) = crate::d3d9::tests::setup(&mut env);
        let a = Content(id, 0, 0, 1);
        let b = Content(id, 1, 0, 1);
        let depth = Content(id, 0, 1, 1);
        let effect = |pass, reads, writes, barrier| Effects {
            pass,
            reads,
            writes,
            barrier,
        };
        for (effects, skipped) in [
            (
                vec![
                    effect(1, vec![], vec![(a, false)], false),
                    effect(2, vec![], vec![(b, false)], false),
                    effect(1, vec![], vec![(a, true)], false),
                ],
                vec![true, false, false],
            ),
            (
                vec![
                    effect(1, vec![], vec![(a, false)], false),
                    effect(2, vec![a], vec![(b, false)], false),
                    effect(1, vec![], vec![(a, true)], false),
                ],
                vec![false, false, false],
            ),
            (
                vec![
                    effect(1, vec![], vec![(a, false)], false),
                    effect(2, vec![], vec![(b, false)], false),
                    effect(1, vec![], vec![(a, false)], false),
                ],
                vec![false, false, false],
            ),
            (
                vec![
                    effect(1, vec![], vec![(a, false), (depth, false)], false),
                    effect(2, vec![], vec![(b, false)], false),
                    effect(1, vec![], vec![(a, true)], false),
                ],
                vec![false, false, false],
            ),
            (
                vec![
                    effect(1, vec![], vec![(a, false)], false),
                    effect(2, vec![], vec![(b, true)], false),
                ],
                vec![false, false],
            ),
            (
                vec![
                    effect(1, vec![], vec![(a, false)], false),
                    effect(2, vec![], vec![(a, true)], true),
                ],
                vec![false, false],
            ),
            (
                vec![
                    effect(1, vec![], vec![(a, false)], false),
                    effect(2, vec![a], vec![(b, true)], false),
                    effect(1, vec![], vec![(a, true)], false),
                    effect(2, vec![], vec![(b, true)], false),
                ],
                vec![true, true, false, false],
            ),
        ] {
            let mut plan = Plan {
                effects,
                ..Default::default()
            };
            plan.plan();
            assert_eq!(plan.skipped, skipped);
            // Replanning and truncation never retain a stale skip verdict.
            plan.plan();
            assert_eq!(plan.skipped, skipped);
            plan.truncate(0);
            assert!(!plan.skip(0));
        }
    }
}
