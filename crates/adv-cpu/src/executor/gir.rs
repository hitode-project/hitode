//! Minimal serial dispatch adapter. Region discovery stays with compile/.
use super::Executor;
use crate::gir::{
    execution::{Engine, Program, Stats},
    lower_cl as lower,
};

impl Executor {
    pub fn set_gir_state_ssa(&mut self, enabled: bool) -> Result<(), String> {
        if self.parallel.is_some()
            || self.stats.native_entries != 0
            || self.stats.interpreter_entries != 0
        {
            return Err("GIR must be selected before execution on a serial owner".into());
        }
        self.gir = if enabled { Some(Engine::new()?) } else { None };
        Ok(())
    }

    pub fn gir_stats(&self) -> Stats {
        self.gir
            .as_ref()
            .map_or_else(Stats::default, |g| g.stats.clone())
    }

    pub(super) fn run_gir(&mut self, owner: u32, stop: u32) -> Result<Option<u32>, String> {
        let Some(engine) = &mut self.gir else {
            return Ok(None);
        };
        if self.parallel.is_some() || self.state.eip != owner || !Engine::mode_allowed(&self.state)
        {
            engine.stats.mode_rejections += 1;
            return Ok(None);
        }
        // Retire on any changed code stamp, even if compile/ later determines
        // the original bytes still match. Data leases are re-acquired separately.
        if engine
            .programs
            .get(&owner)
            .and_then(Option::as_ref)
            .is_some_and(|p| !p.valid(&self.memory))
        {
            engine.programs.remove(&owner);
            engine.stats.invalidations += 1;
        }
        if let std::collections::btree_map::Entry::Vacant(slot) = engine.programs.entry(owner) {
            let native = &self.code.compiled[&owner];
            let program = Program::compile(
                &native.gir_instructions,
                owner,
                &mut self.memory,
                native.pages.clone(),
            )
            .ok();
            engine.stats.admitted += u64::from(program.is_some());
            engine.stats.rejected += u64::from(program.is_none());
            slot.insert(program);
        }
        let Some(program) = engine.programs[&owner].as_ref() else {
            return Ok(None);
        };
        // The oracle records each retirement; comparison uses the exact count,
        // including repeated PCs in loops, and retains complete EFLAGS.
        let trace = self
            .verifier
            .as_deref_mut()
            .map(|v| {
                v.record_gir(
                    owner,
                    stop,
                    &mut self.interpreter,
                    &mut self.state,
                    &mut self.memory,
                )
            })
            .transpose()?;
        let budget = self.state.fuel.min(256);
        let mut frame = lower::Frame::from_cpu(&self.state);
        let (status, proven) = program.run(
            &mut frame,
            &mut self.memory,
            &mut engine.lease,
            budget,
            if self.host_interrupts {
                &self.interrupt
            } else {
                &self.inactive_interrupt
            },
            &engine.revoke,
            stop,
        )?;
        frame.restore(&mut self.state);
        self.state.fuel -= frame.committed;
        engine.stats.entries += 1;
        engine.stats.units += frame.committed;
        engine.stats.proven += u64::from(proven);
        engine.stats.checked += u64::from(!proven);
        engine.stats.cold += u64::from(matches!(
            status,
            lower::READ_CHECK | lower::WRITE_CHECK | lower::UNSUPPORTED | lower::GUARD
        ));
        self.stats.native_entries += 1;
        self.stats.native_blocks += frame.committed;
        if let Some(trace) = trace {
            let check = trace.check_at(frame.committed as usize, &self.state, &self.memory);
            self.record_verification(check)?;
        }
        Ok(Some(status))
    }
}
