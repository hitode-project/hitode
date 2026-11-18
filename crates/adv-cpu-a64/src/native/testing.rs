use super::{CpuState, Diagnostics, Memory, Native};

impl Native {
    pub(crate) fn set_baseline_only(&mut self, baseline_only: bool) {
        self.plans.baseline_only = baseline_only;
    }

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
        if self.plans.baseline_only {
            return false;
        }
        let head = at.eip;
        self.invalidate(memory);
        if !self.code.units.contains_key(&head) {
            self.translate(memory, head).unwrap();
        }
        // Exercise resident execution independently of its profitability.
        self.code.tier1_words.remove(&head);
        if self.plans.resident(head).is_none() {
            self.discover(memory, head, head, at);
            self.install_optimized(true);
        }
        self.plans
            .resident(head)
            .is_some_and(|head| !head.versions.is_empty())
    }

    pub(crate) fn resident_arrivals(&self, head: u32) -> Option<u32> {
        let head = self.plans.resident(head)?;
        (!head.versions.is_empty()).then(|| super::plan::SAMPLE - head.sample.get())
    }
}

impl Diagnostics {
    pub(crate) fn tier1_counts(&self) -> [u64; 19] {
        [
            self.first_visits,
            self.first_declined,
            self.empty,
            self.over_fuel,
            self.interprets,
            self.faults,
            self.dispatches,
            self.dropped_units,
            self.clears,
            self.promotions,
            self.empty_units[0],
            self.empty_units[1],
            self.empty_units[2],
            self.empty_units[3],
            self.accesses,
            self.refused,
            self.checked_faults,
            self.helpers,
            self.helper_exits,
        ]
    }
}
