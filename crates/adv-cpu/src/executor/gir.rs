//! Minimal serial dispatch adapter. Region discovery stays with compile/.
use super::Executor;
use crate::gir::{
    execution::{Engine, Program, Stats},
    hot, lower_cl as lower,
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
            engine.promoted.remove(&owner);
            engine.stats.invalidations += 1;
        }
        if engine.heat.get(&owner).copied().unwrap_or(0) >= hot::HOT
            && engine.promoted.insert(owner)
        {
            // Only immutable, currently valid snapshots participate. Heat is
            // sampled outside generated code and never authorizes an entry.
            let mut candidates: Vec<_> = self
                .code
                .compiled
                .iter()
                .filter_map(|(&entry, native)| {
                    let heat = engine
                        .heat
                        .get(&entry)
                        .copied()
                        .unwrap_or(0)
                        .max(native.profile().map_or(0, |p| p.heat()));
                    (native.valid(&self.memory) && (entry == owner || heat >= hot::HOT))
                        .then_some((entry, heat, native))
                })
                .collect();
            candidates
                .sort_by_key(|&(entry, heat, _)| (entry != owner, std::cmp::Reverse(heat), entry));
            candidates.truncate(hot::MAX_REGIONS);
            let candidates = candidates
                .into_iter()
                .map(|(entry, heat, native)| hot::Candidate {
                    entry,
                    heat,
                    instructions: native.gir_instructions.clone(),
                    pages: native.pages.clone(),
                })
                .collect();
            if let Ok(selection) = hot::select(candidates, owner) {
                let merged = selection.merged;
                // A singleton already uses the same Tail ABI; rebuilding it
                // without new code would only add synchronous compile latency.
                if (merged != 0 || selection.units.len() > 1)
                    && let Ok(program) = Program::compile_family(selection, &mut self.memory)
                {
                    engine.stats.promotions += 1;
                    engine.stats.merged_regions += merged as u64;
                    engine.stats.code_bytes += program.code_bytes as u64;
                    engine.publish(owner, Some(program));
                }
            }
        }
        if !engine.programs.contains_key(&owner) {
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
            engine.stats.code_bytes += program.as_ref().map_or(0, |p| p.code_bytes as u64);
            engine.publish(owner, program);
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
        // Verification retains a bounded oracle prefix. Normal Speed work is
        // not sliced; wall deadlines keep the existing measured allowance.
        let allowance = if trace.is_some() {
            256
        } else {
            self.deadline.map_or(u64::MAX, |deadline| {
                let left = deadline
                    .saturating_duration_since(std::time::Instant::now())
                    .as_micros() as u64;
                (left / 2).saturating_mul(self.native_rate).max(256)
            })
        };
        let budget = self.state.fuel.min(allowance);
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
        let heat = engine.heat.entry(owner).or_default();
        *heat = heat.saturating_add(frame.committed);
        engine.stats.tails += frame.tail_entries;
        engine.stats.boundaries[0] += frame.tail_entries - frame.tail_calls;
        engine.stats.boundaries[2] += frame.tail_calls;
        let boundary = match status {
            lower::COMPLETE => 0,
            lower::CALL => 2,
            lower::INDIRECT => match frame.last_indirect_branch.kind {
                crate::IndirectBranchKind::Call => 2,
                crate::IndirectBranchKind::Return => 3,
                _ => 1,
            },
            lower::BUDGET if budget < self.state.fuel + frame.committed => 4,
            lower::UNSUPPORTED => 5,
            lower::BUDGET => 6,
            lower::READ_CHECK | lower::WRITE_CHECK | lower::GUARD => 7,
            lower::INTERRUPTED | lower::REVOKED => 8,
            _ => 9,
        };
        engine.stats.boundaries[boundary] += 1;
        engine.stats.proven += u64::from(proven);
        engine.stats.checked += u64::from(!proven);
        engine.stats.cold += u64::from(matches!(
            status,
            lower::READ_CHECK | lower::WRITE_CHECK | lower::UNSUPPORTED | lower::GUARD
        ));
        self.stats.native_entries += 1 + frame.tail_entries;
        self.stats.native_blocks += frame.committed;
        if let Some(trace) = trace {
            let check = trace.check_at(frame.committed as usize, &self.state, &self.memory);
            self.record_verification(check)?;
        }
        Ok(Some(status))
    }
}
