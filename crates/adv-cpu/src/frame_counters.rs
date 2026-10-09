//! Fixed-size, address-free snapshots for opt-in frame accounting.
use crate::{Executor, InterpreterReason};

pub const REASONS: [&str; 6] = [
    "uncovered",
    "rejected",
    "pending",
    "interior",
    "fallback",
    "forced",
];

pub(crate) fn reason_index(reason: InterpreterReason) -> usize {
    match reason {
        InterpreterReason::Uncovered => 0,
        InterpreterReason::Rejected => 1,
        InterpreterReason::Pending => 2,
        InterpreterReason::Interior => 3,
        InterpreterReason::Fallback => 4,
        InterpreterReason::Forced => 5,
    }
}

/// Units differ by tier: Tier 0/1 retire instruction/REP units; Tier 2 retires blocks.
#[derive(Clone, Copy, Debug, Default)]
pub struct FrameCounters {
    pub tier0_units: u64,
    pub tier1_units: u64,
    pub tier2_blocks: u64,
    /// Confirmed retired Tier 0 instructions only; never present this as all tiers.
    pub retired_tier0_instructions: u64,
    pub native_entries: u64,
    /// Generated tail links, an inclusive subset of native_entries.
    pub linked_entries: u64,
    pub interpreter_entries: u64,
    pub sync_compile_ns: u64,
    /// Execution in a missing-code lane while compilation is pending, not worker time.
    pub compile_wait_execution_ns: u64,
    /// Chunk-end stage samples, not exact timestamps of worker transitions.
    pub compile_wait_stage_ns: [u64; 4],
    /// Completed worker service, summed across threads; overlaps execution.
    pub compile_worker_ns: u64,
    pub publication_ns: u64,
    pub compile_wait_units: u64,
    pub invalidations: u64,
    pub recompilations: u64,
    pub compilations: u64,
    pub x87_helpers: u64,
    pub softfloat_calls: u64,
    pub reasons: [u64; 6],
    /// Pending includes queued/deferred compilation even without entry ownership.
    /// All six timing lanes are disjoint; Tier 1 fallback/bridge is inclusive.
    pub reason_ns: [u64; 6],
}

impl FrameCounters {
    pub fn since(self, before: Self) -> Self {
        macro_rules! delta {
            ($($field:ident),*) => { Self {
                $($field: self.$field.saturating_sub(before.$field),)*
                reasons: std::array::from_fn(|i| self.reasons[i].saturating_sub(before.reasons[i])),
                reason_ns: std::array::from_fn(|i| self.reason_ns[i].saturating_sub(before.reason_ns[i])),
                compile_wait_stage_ns: std::array::from_fn(|i| self.compile_wait_stage_ns[i].saturating_sub(before.compile_wait_stage_ns[i])),
            } };
        }
        delta!(
            tier0_units,
            tier1_units,
            tier2_blocks,
            retired_tier0_instructions,
            native_entries,
            linked_entries,
            interpreter_entries,
            sync_compile_ns,
            compile_wait_execution_ns,
            compile_worker_ns,
            publication_ns,
            compile_wait_units,
            invalidations,
            recompilations,
            compilations,
            x87_helpers,
            softfloat_calls
        )
    }
    pub fn json(&self) -> String {
        let reasons = REASONS
            .iter()
            .zip(self.reasons)
            .map(|(name, count)| format!("\"{name}\":{count}"))
            .collect::<Vec<_>>()
            .join(",");
        let reason_ns = REASONS
            .iter()
            .zip(self.reason_ns)
            .map(|(name, ns)| format!("\"{name}\":{ns}"))
            .collect::<Vec<_>>()
            .join(",");
        format!(
            concat!(
                "{{\"tier0_units\":{},\"tier1_units\":{},\"tier2_blocks\":{},\"retired_tier0_instructions\":{},",
                "\"native_entries\":{},\"linked_entries\":{},\"interpreter_entries\":{},\"sync_compile_ns\":{},",
                "\"compile_wait_execution_ns\":{},\"compile_wait_stage_ns\":{:?},\"compile_worker_ns\":{},\"publication_ns\":{},\"compile_wait_units\":{},\"invalidations\":{},",
                "\"recompilations\":{},\"compilations\":{},\"x87_helpers\":{},\"softfloat_calls\":{},\"reasons\":{{{}}},\"reason_ns\":{{{}}}}}"
            ),
            self.tier0_units,
            self.tier1_units,
            self.tier2_blocks,
            self.retired_tier0_instructions,
            self.native_entries,
            self.linked_entries,
            self.interpreter_entries,
            self.sync_compile_ns,
            self.compile_wait_execution_ns,
            self.compile_wait_stage_ns,
            self.compile_worker_ns,
            self.publication_ns,
            self.compile_wait_units,
            self.invalidations,
            self.recompilations,
            self.compilations,
            self.x87_helpers,
            self.softfloat_calls,
            reasons,
            reason_ns
        )
    }
}

impl Executor {
    /// Does not enable the PC profiler or change region linking/execution policy.
    pub fn enable_frame_timing(&mut self, enabled: bool) {
        self.frame_timing = enabled;
        self.interpreter.frame_retired = enabled.then_some(0);
    }
    pub fn frame_counters(&self) -> FrameCounters {
        let s = &self.stats;
        let t = self.tier1_stats();
        let reasons = [
            InterpreterReason::Uncovered,
            InterpreterReason::Rejected,
            InterpreterReason::Pending,
            InterpreterReason::Interior,
            InterpreterReason::Fallback,
            InterpreterReason::Forced,
        ]
        .map(|r| s.cliffs.interpreter.get(&r).map_or(0, |n| n.entries));
        FrameCounters {
            tier0_units: s.interpreter_instructions,
            tier1_units: t.native_instructions,
            tier2_blocks: s.native_blocks,
            retired_tier0_instructions: s.frame_retired_tier0,
            native_entries: s.native_entries,
            linked_entries: s.linked_entries,
            interpreter_entries: s.interpreter_entries,
            sync_compile_ns: (s.sync_compile_time + t.compile_time).as_nanos() as u64,
            compile_wait_execution_ns: s.compile_wait_execution_ns,
            compile_wait_stage_ns: s.compile_wait_stage_ns,
            compile_worker_ns: s.compile_time.as_nanos() as u64,
            publication_ns: s.publication_ns,
            compile_wait_units: s.compile_wait_units,
            invalidations: s.invalidations + t.invalidations,
            recompilations: s.tier_up_publications,
            compilations: s.compilations + t.compilations,
            x87_helpers: s.cliffs.x87_helper_calls,
            softfloat_calls: s.cliffs.x87_exact.values().sum(),
            reasons,
            reason_ns: s.frame_reason_ns,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn delta_is_bounded_and_address_free() {
        let previous = FrameCounters {
            tier0_units: 10,
            reasons: [2; 6],
            reason_ns: [10; 6],
            compile_wait_stage_ns: [10; 4],
            ..Default::default()
        };
        let now = FrameCounters {
            tier0_units: 25,
            reasons: [3; 6],
            reason_ns: [25; 6],
            compile_wait_stage_ns: [25; 4],
            ..Default::default()
        };
        let delta = now.since(previous);
        assert_eq!(delta.tier0_units, 15);
        assert_eq!(delta.reasons, [1; 6]);
        assert_eq!(delta.reason_ns, [15; 6]);
        assert_eq!(delta.compile_wait_stage_ns, [15; 4]);
        assert_eq!(previous.since(now).tier0_units, 0);
        assert!(delta.json().contains("\"fallback\":1"));
    }

    #[test]
    fn fallback_timing_is_opt_in_and_does_not_enable_pc_profiling() {
        let mut executor = Executor::new(0x10000, 0x2000).unwrap();
        executor
            .protect(0x10000, 4096, crate::READ | crate::WRITE | crate::EXEC)
            .unwrap();
        executor.write(0x10000, &[0x40, 0xeb, 0xfd]).unwrap();
        executor.state.eip = 0x10000;
        executor.run(0x12000, 100).unwrap();
        assert_eq!(executor.frame_counters().reason_ns, [0; 6]);
        executor.enable_frame_timing(true);
        executor.run(0x12000, 100).unwrap();
        assert!(executor.frame_counters().reason_ns[0] > 0);
        assert!(executor.profile.is_empty());
        let before = executor.frame_counters();
        executor.enable_frame_timing(false);
        executor.run(0x12000, 100).unwrap();
        assert_eq!(executor.frame_counters().reason_ns, before.reason_ns);
    }
}
