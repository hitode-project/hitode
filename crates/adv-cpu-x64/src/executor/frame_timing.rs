//! Frame accounting over the Cranelift executor's counters.
use crate::frame_counters::FrameCounters;
use crate::{Executor, InterpreterReason};

impl Executor {
    /// Does not enable the PC profiler or change region linking/execution policy.
    pub fn enable_frame_timing(&mut self, enabled: bool) {
        self.frame_timing = enabled;
        self.interpreter.frame_retired = enabled.then_some(0);
    }
    pub fn frame_counters(&self) -> FrameCounters {
        let s = &self.stats;
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
            tier2_blocks: s.native_blocks,
            retired_tier0_instructions: s.frame_retired_tier0,
            native_entries: s.native_entries,
            interpreter_entries: s.interpreter_entries,
            sync_compile_ns: s.sync_compile_time.as_nanos() as u64,
            compile_wait_execution_ns: s.compile_wait_execution_ns,
            compile_wait_stage_ns: s.compile_wait_stage_ns,
            compile_worker_ns: s.compile_time.as_nanos() as u64,
            publication_ns: s.publication_ns,
            compile_wait_units: s.compile_wait_units,
            invalidations: s.invalidations,
            recompilations: s.tier_up_publications,
            compilations: s.compilations,
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
