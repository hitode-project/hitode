//! Entry-state attribution for the two historical unowned interpreter buckets.
use super::Executor;
use crate::{Region, cliffs::InterpreterDetail as Detail};

impl Executor {
    pub(super) fn record_rejection(&mut self, region: &Region, reason: Detail) {
        // Preserve the historical bucket membership; these details are diagnostic.
        for pc in region.entries.iter().chain(std::iter::once(&region.start)) {
            if let Some(detail) = self.attempted.get_mut(pc) {
                *detail = reason;
            }
        }
    }

    pub(super) fn interpreter_detail(&self, pc: u32) -> Detail {
        if !self.auto_jit {
            return Detail::AutoJitDisabled;
        }
        if !self.memory.executable(pc, 1) {
            return Detail::NonExecutable;
        }
        if self.rejected_entries.contains_key(&pc) {
            return Detail::CachePolicy;
        }
        let covers = |region: &Region| pc.wrapping_sub(region.start) < region.len as u32;
        if self
            .pending_regions
            .keys()
            .filter_map(|owner| self.code.regions.get(owner))
            .any(covers)
        {
            return Detail::PendingCompilation;
        }
        if self
            .deferred_compilations
            .values()
            .any(|input| covers(&input.region))
        {
            return Detail::DeferredCompilation;
        }
        let mut location = None;
        // Only inspect this code page's owners, never scan the entire code cache.
        if let Some(owners) = self.code.page_regions.get(&(pc & !4095)) {
            for owner in owners {
                if let Some(compiled) = self.code.compiled.get(owner)
                    && let Some(&code) = compiled.excluded_entries.get(&pc)
                {
                    return Detail::Unsupported(code);
                }
                if let Some(region) = self.code.regions.get(owner) {
                    if region.frontier.contains(&pc) {
                        location = Some(Detail::RegionBoundary);
                    } else if covers(region) && location.is_none() {
                        // A byte range is not a decoded CFG. This may be an indirect
                        // target, a continuation, or an unreachable gap; do not guess.
                        location = Some(Detail::UnregisteredEntry);
                    }
                }
            }
        }
        if let Some(&reason) = self.attempted.get(&pc) {
            return reason;
        }
        if let Some(location) = location {
            return location;
        }
        match self.hits.get(&pc) {
            Some(&hits) if hits < 32 => Detail::BelowThreshold,
            Some(_) => Detail::QueueCapacity,
            None => Detail::NotDiscovered,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CompileStrategy, EXEC, InterpreterReason, READ, RunOutcome, WRITE};
    use iced_x86::code_asm::*;

    const CODE: u32 = 0x10000;
    const STOP: u32 = 0x20000;

    fn machine() -> Executor {
        let mut m = Executor::new(CODE, 0x20000).unwrap();
        m.protect(CODE, 4096, READ | WRITE | EXEC).unwrap();
        m.enable_auto_jit(true);
        m.set_async_compilation(false);
        m.state.eip = CODE;
        m
    }

    #[test]
    fn unsupported_entry_forms_are_measured_without_disabling_the_region() {
        let mut a = CodeAssembler::new(32).unwrap();
        let mut top = a.create_label();
        a.set_label(&mut top).unwrap();
        a.scasb().unwrap();
        a.inc(ebx).unwrap();
        a.jmp(top).unwrap();
        let mut m = machine();
        m.write(CODE, &a.assemble(CODE.into()).unwrap()).unwrap();
        m.state.regs[7] = CODE;
        assert_eq!(m.run(STOP, 500).unwrap(), RunOutcome::Budget);
        assert!(m.stats.compilations > 0);
        assert_eq!(
            m.interpreter_detail(CODE),
            Detail::Unsupported(iced_x86::Code::Scasb_AL_m8)
        );
        assert!(
            m.stats
                .cliffs
                .interpreter_detail_rows()
                .iter()
                .any(|(label, n)| {
                    label == "Rejected/Unsupported/Scasb/Scasb_AL_m8" && n.instructions > 0
                })
        );
        for reason in [InterpreterReason::Uncovered, InterpreterReason::Rejected] {
            let totals = m
                .stats
                .cliffs
                .interpreter
                .get(&reason)
                .copied()
                .unwrap_or_default();
            let rows: Vec<_> = m
                .stats
                .cliffs
                .interpreter_details
                .iter()
                .filter(|((r, _), _)| *r == reason)
                .collect();
            assert_eq!(
                rows.iter().map(|(_, n)| n.entries).sum::<u64>(),
                totals.entries
            );
            assert_eq!(
                rows.iter().map(|(_, n)| n.instructions).sum::<u64>(),
                totals.instructions
            );
        }
    }

    #[test]
    fn queued_and_deferred_entries_are_not_misreported_as_instruction_rejections() {
        let mut m = machine();
        let mut a = CodeAssembler::new(32).unwrap();
        a.nop().unwrap();
        a.jmp(STOP as u64).unwrap();
        let bytes = a.assemble(CODE.into()).unwrap();
        m.write(CODE, &bytes).unwrap();
        let mut region = Region::new(CODE, bytes.len());
        region.entries.push(CODE);
        region.compile_strategy = CompileStrategy::Fast;
        let snapshot = crate::compile::Snapshot::capture(&region, &mut m.memory).unwrap();
        m.attempted.insert(CODE, Detail::NoPublishedEntry);
        m.deferred_compilations.insert(CODE, snapshot);
        assert_eq!(m.interpreter_detail(CODE), Detail::DeferredCompilation);
        assert_eq!(m.interpreter_detail(CODE + 1), Detail::DeferredCompilation);
        m.deferred_compilations.clear();
        m.index_region(region.clone());
        m.pending_regions.insert(CODE, 1);
        assert_eq!(m.interpreter_detail(CODE), Detail::PendingCompilation);
        assert_eq!(m.interpreter_detail(CODE + 1), Detail::PendingCompilation);
        m.pending_regions.clear();
        m.record_rejection(&region, Detail::CompileFailure);
        assert_eq!(m.interpreter_detail(CODE), Detail::CompileFailure);
        m.attempted.clear();
        m.code.clear_regions();
        m.hits.insert(CODE, 1);
        assert_eq!(m.interpreter_detail(CODE), Detail::BelowThreshold);
        m.set_code_cache_limits(0, 0);
        m.add_region(region).unwrap();
        assert_eq!(m.interpreter_detail(CODE), Detail::CachePolicy);
    }

    #[test]
    fn discovery_reports_the_actual_empty_frontier() {
        let mut m = machine();
        assert!(matches!(
            crate::discovery::discover(&mut m.memory, CODE, CODE, CompileStrategy::Fast, &|_| {
                false
            }),
            Err(Detail::DiscoveryBoundary)
        ));
        m.protect(CODE, 4096, READ).unwrap();
        assert!(matches!(
            crate::discovery::discover(&mut m.memory, CODE, STOP, CompileStrategy::Fast, &|_| {
                false
            }),
            Err(Detail::DiscoveryMemory)
        ));
        assert_eq!(m.interpreter_detail(CODE), Detail::NonExecutable);
    }
}
