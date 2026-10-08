//! Thin selection and accounting adapter; templates and ownership live in tier1.
use super::Executor;
use crate::Tier1Mode;

impl Executor {
    /// Explicitly select the experimental Linux/Android ARM64 fixed-register lane.
    /// Select before compilation/execution; the default remains unchanged.
    pub fn set_arm64_tier1(&mut self, enabled: bool) -> Result<(), String> {
        self.set_tier1_mode(if enabled {
            Tier1Mode::Exclusive
        } else {
            Tier1Mode::Off
        })
    }

    /// Bridge is a serial fallback lane, never a second execution owner. The
    /// dispatcher refreshes code and collects Cranelift publications first. Only
    /// missing native entries may use Tier 1; forced and Cranelift fallback exits
    /// retain Tier 0. Each entry returns within 256 instruction/REP units, so
    /// publication, interrupts, deadlines and fuel are checked at that boundary.
    /// Both caches watch Memory code pages and independently validate their code;
    /// consuming Cranelift's dirty-page list cannot invalidate Tier 1's epoch.
    /// Bridge CFGs end before published Cranelift entries. A publication revision
    /// retires conflicting Tier 1 CFGs before they can cross a new entry;
    /// unrelated publications keep existing CFGs and their pinned mappings.
    pub fn set_tier1_mode(&mut self, mode: Tier1Mode) -> Result<(), String> {
        if mode == self.tier1_mode {
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
        let engine = if mode != Tier1Mode::Off {
            Some(crate::tier1::Engine::new()?)
        } else {
            None
        };
        if mode == Tier1Mode::Exclusive
            || (mode == Tier1Mode::Off && self.tier1_mode == Tier1Mode::Exclusive)
        {
            self.set_async_compilation(false);
            self.auto_jit = false;
        }
        self.tier1 = engine;
        self.tier1_mode = mode;
        Ok(())
    }
    pub fn tier1_mode(&self) -> Tier1Mode {
        self.tier1_mode
    }
    /// Bound Tier 1 entries and page-rounded RX bytes independently of Cranelift.
    /// Reducing limits drops this serial cache; zero disables native admission.
    pub fn set_tier1_cache_limits(&mut self, entries: usize, bytes: usize) {
        if let Some(engine) = &mut self.tier1 {
            engine.set_cache_limits(entries, bytes);
        }
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
        // Tier 1 owns the CPU serially, so the interpreted trace can be undone
        // privately before the same entry runs natively.
        let mut trace = None;
        if let Some(verifier) = self.verifier.as_deref_mut()
            && verifier.take_tier1(self.state.eip, before.compilations)
        {
            trace = Some(verifier.record_tier1(
                self.state.eip,
                stop,
                &mut self.interpreter,
                &mut self.state,
                &mut self.memory,
            )?);
        }
        let execution = engine.run_block(
            &mut self.state,
            &mut self.memory,
            &mut self.interpreter,
            units,
            stop,
            (self.tier1_mode == Tier1Mode::Bridge)
                .then_some((&self.code.entry_owner, self.code.cache_revision)),
        )?;
        if self.tier1_mode == Tier1Mode::Bridge {
            engine.record_bridge(execution.execution.units - execution.interpreted_units);
        }
        let after = engine.stats();
        self.stats.native_entries += after.native_entries - before.native_entries;
        self.stats.linked_entries += after.direct_transfers - before.direct_transfers;
        if let Some(trace) = trace {
            let steps = execution.execution.units as usize;
            let check = trace.check_at(steps, &self.state, &self.memory);
            self.record_verification(check)?;
        }
        Ok(Some(execution))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_is_atomic_and_keeps_the_requested_compiler_policy() {
        let mut m = Executor::new(0, 4096).unwrap();
        m.enable_auto_jit(true);
        m.set_async_compilation(true);
        if cfg!(all(
            target_arch = "aarch64",
            any(target_os = "linux", target_os = "android")
        )) {
            m.set_tier1_mode(Tier1Mode::Bridge).unwrap();
            assert!(m.auto_jit && m.async_compilation);
            assert!(m.set_guest_workers(2).is_err());
            m.set_tier1_mode(Tier1Mode::Off).unwrap();
            assert!(m.auto_jit && m.async_compilation);
            m.set_arm64_tier1(true).unwrap();
            assert_eq!(m.tier1_mode(), Tier1Mode::Exclusive);
            assert!(!m.auto_jit && !m.async_compilation);
            m.enable_auto_jit(true);
            m.set_async_compilation(true);
            m.set_tier1_mode(Tier1Mode::Bridge).unwrap();
            assert!(m.auto_jit && m.async_compilation);
            m.set_arm64_tier1(true).unwrap();
            m.set_arm64_tier1(false).unwrap();
            assert_eq!(m.tier1_mode(), Tier1Mode::Off);
        } else {
            assert!(m.set_tier1_mode(Tier1Mode::Bridge).is_err());
            assert!(m.auto_jit && m.async_compilation);
            assert_eq!(m.tier1_mode(), Tier1Mode::Off);
        }
    }
}
