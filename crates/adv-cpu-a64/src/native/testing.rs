use super::{CpuState, Memory, Native};

impl Native {
    pub(crate) fn force_resident(&mut self, memory: &mut Memory, head: u32) -> bool {
        if !self.candidates.contains(&head) {
            self.candidates.push(head);
            self.cut(head, head + 1);
        }
        let mut cpu = CpuState {
            eip: head,
            ..CpuState::default()
        };
        for _ in 0..8 {
            if let Some(prepared) = self.computation.get_mut(&head)
                && prepared.inline.is_some()
            {
                // Exercise the Tier-1 entry even if a run starts at the head.
                prepared.code.optimized = None;
                return true;
            }
            // Translation and compilation happen before the fuel check.
            self.run(&mut cpu, memory, u32::MAX, 0, std::ptr::null_mut())
                .unwrap();
            self.install_optimized(false);
        }
        false
    }
}
