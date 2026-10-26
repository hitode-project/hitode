//! Backward liveness for complete, contiguous target passes. Uploads remain
//! barriers: their fresh content keys do not prove a full pixel overwrite.
use super::*;
use std::collections::HashSet;

pub(super) struct Effects {
    pass: u128,
    barrier: bool,
    clear: bool,
    draw_bounds: Option<adv_device::surface::Rect>,
    reads: Vec<Content>,
    writes: Vec<(Content, bool)>,
}
#[derive(Default)]
pub(super) struct Plan {
    effects: Vec<Arc<Effects>>,
    skipped: Vec<bool>,
    clear_masks: Vec<u8>,
    covered: HashSet<Content>,
}
impl Plan {
    pub fn record(
        &mut self,
        operation: &Operation,
        reads: Vec<Content>,
        writes: Vec<(Content, bool)>,
        bounds: adv_device::surface::Rect,
    ) -> Arc<Effects> {
        let effect = Arc::new(Effects {
            pass: hash(writes.first().map(|(k, _)| (k.0, k.1))),
            barrier: matches!(operation, Operation::Upload { .. }),
            clear: matches!(operation, Operation::Clear { .. }),
            draw_bounds: matches!(operation, Operation::Draw(_)).then_some(bounds),
            reads,
            writes,
        });
        self.effects.push(effect.clone());
        effect
    }
    pub fn replay(&mut self, effect: Arc<Effects>) {
        self.effects.push(effect);
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
        self.clear_masks.clear();
    }
    pub fn skip(&self, n: usize) -> bool {
        self.skipped.get(n).copied().unwrap_or(false)
    }
    pub fn outside_damage(&self, n: usize, clip: adv_device::surface::Rect) -> bool {
        self.effects[n]
            .draw_bounds
            .is_some_and(|r| r.clipped(clip).empty())
    }
    pub fn clear_mask(&self, n: usize) -> u8 {
        self.clear_masks.get(n).copied().unwrap_or(7)
    }
    pub fn plan(&mut self) -> u64 {
        self.skipped.clear();
        self.skipped.resize(self.effects.len(), false);
        self.clear_masks.clear();
        self.clear_masks.resize(self.effects.len(), 7);
        let covered = &mut self.covered;
        covered.clear();
        let mut end = self.effects.len();
        #[cfg(feature = "d3d9-counters")]
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
            let mut dead_group = true;
            for (index, e) in group.iter().enumerate().rev() {
                let dead = !e.barrier
                    && !e.writes.is_empty()
                    && e.writes.iter().all(|(k, _)| covered.contains(k));
                if dead {
                    self.skipped[start + index] = true;
                    continue;
                }
                dead_group = false;
                let mut mask = 7;
                for &(k, overwrite) in &e.writes {
                    if e.clear && covered.contains(&k) {
                        mask &= !(1 << k.2);
                        continue;
                    }
                    if overwrite && !e.barrier {
                        covered.insert(k);
                    } else {
                        covered.remove(&k);
                    }
                }
                self.clear_masks[start + index] = mask;
                for k in &e.reads {
                    covered.remove(k);
                }
            }
            #[cfg(feature = "d3d9-counters")]
            {
                count += u64::from(dead_group);
            }
            let _ = dead_group;
            end = start;
        }
        #[cfg(feature = "d3d9-counters")]
        {
            count
        }
        #[cfg(not(feature = "d3d9-counters"))]
        {
            0
        }
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
            clear: false,
            draw_bounds: None,
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
                effects: effects.into_iter().map(Arc::new).collect(),
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
        // A clear keeps only the components no later write fully replaces.
        let clear = |writes| Effects {
            clear: true,
            ..effect(1, vec![], writes, false)
        };
        let mut plan = Plan {
            effects: vec![
                Arc::new(clear(vec![(a, true), (depth, true)])),
                Arc::new(effect(1, vec![], vec![(a, false)], false)),
                Arc::new(clear(vec![(depth, true)])),
            ],
            ..Default::default()
        };
        plan.plan();
        assert_eq!(plan.skipped, [false, false, false]);
        assert_eq!([0, 1, 2].map(|n| plan.clear_mask(n)), [7 & !(1 << 1), 7, 7]);
        plan.truncate(0);
        assert_eq!(plan.clear_mask(0), 7);
    }
}
