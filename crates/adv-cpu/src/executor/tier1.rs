//! Thin selection and accounting adapter; templates and ownership live in tier1.
use super::Executor;

impl Executor {
    /// Explicitly select the experimental Linux ARM64 fixed-register lane.
    /// Select before compilation/execution; the default remains unchanged.
    pub fn set_arm64_tier1(&mut self, enabled: bool) -> Result<(), String> {
        if enabled == self.tier1.is_some() {
            return Ok(());
        }
        if self.parallel.is_some()
            || !self.code.compiled.is_empty()
            || self.stats.native_entries != 0
            || self.stats.interpreter_entries != 0
            || !self.pending_regions.is_empty()
            || !self.deferred_compilations.is_empty()
        {
            return Err(
                "Tier 1 must be selected before execution/compilation on a serial owner".into(),
            );
        }
        let engine = if enabled {
            Some(crate::tier1::Engine::new()?)
        } else {
            None
        };
        self.set_async_compilation(false);
        self.auto_jit = false;
        self.tier1 = engine;
        Ok(())
    }
    pub fn tier1_stats(&self) -> crate::Tier1Stats {
        self.tier1
            .as_ref()
            .map_or_else(Default::default, |engine| engine.stats())
    }
    pub(super) fn run_tier1(
        &mut self,
        stop: u32,
        units: u32,
        forced: bool,
    ) -> Result<Option<crate::tier1::Execution>, String> {
        if forced {
            return Ok(None);
        }
        let Some(engine) = self.tier1.as_mut() else {
            return Ok(None);
        };
        let before = engine.stats();
        let execution = engine.run_block(
            &mut self.state,
            &mut self.memory,
            &mut self.interpreter,
            units,
            stop,
        )?;
        let after = engine.stats();
        self.stats.native_entries += after.native_entries - before.native_entries;
        self.stats.linked_entries += after.direct_transfers - before.direct_transfers;
        Ok(Some(execution))
    }
}
