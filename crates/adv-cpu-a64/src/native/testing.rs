use super::{CpuState, Memory, Native};

impl Native {
    pub(crate) fn force_resident(&mut self, memory: &mut Memory, head: u32) -> bool {
        self.force_resident_at(
            memory,
            CpuState {
                eip: head,
                ..CpuState::default()
            },
        )
    }

    pub(crate) fn force_resident_at(&mut self, memory: &mut Memory, at: CpuState) -> bool {
        let head = at.eip;
        self.invalidate(memory);
        if !self.code.units.contains_key(&head) {
            self.translate(memory, head).unwrap();
        }
        // Exercise resident execution independently of its profitability.
        self.code.tier1_words.remove(&head);
        if !self.heads.contains_key(&head) {
            self.discover(memory, head, head, at);
            self.install_optimized(true);
        }
        self.heads
            .get(&head)
            .is_some_and(|head| !head.versions.is_empty())
    }

    pub(crate) fn resident_arrivals(&self, head: u32) -> Option<u32> {
        let head = self.heads.get(&head)?;
        (!head.versions.is_empty()).then(|| super::head::SAMPLE - head.sample.get())
    }
}
