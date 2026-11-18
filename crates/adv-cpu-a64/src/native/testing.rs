use super::{CpuState, Memory, Native};

impl Native {
    pub(crate) fn force_resident(&mut self, memory: &mut Memory, head: u32) -> bool {
        self.invalidate(memory);
        if !self.units.contains_key(&head) {
            self.translate(memory, head).unwrap();
        }
        // Exercise resident execution independently of its profitability.
        self.tier1_words.remove(&head);
        if !self.heads.contains_key(&head) {
            let at = CpuState {
                eip: head,
                ..CpuState::default()
            };
            self.discover(memory, head, head, at);
            self.install_optimized(true);
        }
        if let Some(job) = self.computation.get_mut(&head) {
            job.code.optimized = None;
        }
        self.heads
            .get(&head)
            .is_some_and(|head| !head.versions.is_empty())
    }
}
