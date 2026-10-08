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

/// Units differ by tier: Tier 0/1 retire instruction/REP units; Tier 2 retires blocks.
#[derive(Clone, Copy, Debug, Default)]
pub struct FrameCounters {
    pub tier0_units: u64,
    pub tier1_units: u64,
    pub tier2_blocks: u64,
    pub native_entries: u64,
    pub interpreter_entries: u64,
    pub sync_compile_ns: u64,
    /// Execution in a missing-code lane while compilation is pending, not worker time.
    pub compile_wait_execution_ns: u64,
    pub compile_wait_units: u64,
    pub invalidations: u64,
    pub recompilations: u64,
    pub compilations: u64,
    pub x87_helpers: u64,
    pub softfloat_calls: u64,
    pub reasons: [u64; 6],
}

impl FrameCounters {
    pub fn since(self, before: Self) -> Self {
        macro_rules! delta {
            ($($field:ident),*) => { Self {
                $($field: self.$field.saturating_sub(before.$field),)*
                reasons: std::array::from_fn(|i| self.reasons[i].saturating_sub(before.reasons[i])),
            } };
        }
        delta!(
            tier0_units,
            tier1_units,
            tier2_blocks,
            native_entries,
            interpreter_entries,
            sync_compile_ns,
            compile_wait_execution_ns,
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
        format!(
            concat!(
                "{{\"tier0_units\":{},\"tier1_units\":{},\"tier2_blocks\":{},",
                "\"native_entries\":{},\"interpreter_entries\":{},\"sync_compile_ns\":{},",
                "\"compile_wait_execution_ns\":{},\"compile_wait_units\":{},\"invalidations\":{},",
                "\"recompilations\":{},\"compilations\":{},\"x87_helpers\":{},\"softfloat_calls\":{},\"reasons\":{{{}}}}}"
            ),
            self.tier0_units,
            self.tier1_units,
            self.tier2_blocks,
            self.native_entries,
            self.interpreter_entries,
            self.sync_compile_ns,
            self.compile_wait_execution_ns,
            self.compile_wait_units,
            self.invalidations,
            self.recompilations,
            self.compilations,
            self.x87_helpers,
            self.softfloat_calls,
            reasons
        )
    }
}

impl Executor {
    /// Does not enable the PC profiler or change region linking/execution policy.
    pub fn enable_frame_timing(&mut self, enabled: bool) {
        self.frame_timing = enabled;
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
            native_entries: s.native_entries,
            interpreter_entries: s.interpreter_entries,
            sync_compile_ns: (s.sync_compile_time + t.compile_time).as_nanos() as u64,
            compile_wait_execution_ns: s.compile_wait_execution_ns,
            compile_wait_units: s.compile_wait_units,
            invalidations: s.invalidations + t.invalidations,
            recompilations: s.tier_up_publications,
            compilations: s.compilations + t.compilations,
            x87_helpers: s.cliffs.x87_helper_calls,
            softfloat_calls: s.cliffs.x87_exact.values().sum(),
            reasons,
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
            ..Default::default()
        };
        let now = FrameCounters {
            tier0_units: 25,
            reasons: [3; 6],
            ..Default::default()
        };
        let delta = now.since(previous);
        assert_eq!(delta.tier0_units, 15);
        assert_eq!(delta.reasons, [1; 6]);
        assert_eq!(previous.since(now).tier0_units, 0);
        assert!(delta.json().contains("\"fallback\":1"));
    }
}
