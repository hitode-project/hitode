//! Controller-side ownership of native leases. Workers never own API state,
//! compiler state or the mutable region index.
use super::*;
use crate::parallel::{Image, Job, ParallelStats, Pool, Stop};
use std::sync::atomic::AtomicU64;

impl Executor {
    pub fn set_guest_workers(&mut self, count: usize) -> Result<(), String> {
        if count == self.guest_workers() {
            return Ok(());
        }
        if !matches!(count, 1 | 2) {
            return Err("this release supports 1 or 2 guest workers".into());
        }
        if !cfg!(all(windows, target_arch = "x86_64", target_env = "msvc")) {
            return Err("parallel guest execution currently requires Windows x64 MSVC".into());
        }
        if self.stats.native_entries != 0
            || self.stats.interpreter_entries != 0
            || !self.code.compiled.is_empty()
            || self.parallel.is_some()
        {
            return Err(
                "guest worker count must be selected before execution and compilation".into(),
            );
        }
        if self.shadow_returns.is_some() {
            return Err("parallel return-stack hints are not enabled in this release".into());
        }
        let tsc = Arc::new(AtomicU64::new(0));
        let pool = Pool::new(count - 1, tsc.clone())?;
        self.memory.parallel = Some(pool.fence.clone());
        self.timestamp.share(tsc);
        self.parallel = Some(pool);
        // Parallel code has an explicit unoptimized memory-order contract until
        // Speed's memory transformations are independently audited.
        self.tiered_compilation = false;
        self.auto_compile_strategy = CompileStrategy::Fast;
        Ok(())
    }

    pub fn guest_workers(&self) -> usize {
        self.parallel.as_ref().map_or(1, |p| p.capacity() + 1)
    }
    pub fn parallel_stats(&self) -> ParallelStats {
        self.parallel.as_ref().map_or_else(
            || ParallelStats {
                workers: 1,
                ..Default::default()
            },
            Pool::stats,
        )
    }
    pub(crate) fn take_parallel_stop(&mut self) -> Option<Stop> {
        let stop = self.parallel_stop.take();
        if matches!(stop, Some(Stop::Trap(_))) {
            self.offload = false;
        }
        stop
    }
    pub(crate) fn cold_parallel_retry(&mut self) {
        self.deferred_fallback = Some(self.state.eip);
        self.offload = false;
    }
    pub(crate) fn inline_activity(&self) -> Option<crate::parallel::InlineActivity> {
        self.parallel.as_ref().map(Pool::inline_activity)
    }
    pub(crate) fn has_parallel_capacity(&self) -> bool {
        self.parallel.as_ref().is_some_and(Pool::has_capacity)
    }

    pub(crate) fn dispatch_parallel(
        &mut self,
        stop: u32,
        duration: Duration,
        guest_us: u64,
    ) -> Result<u64, String> {
        if self
            .parallel_image
            .as_ref()
            .is_none_or(|(revision, _)| *revision != self.code.cache_revision)
        {
            let mut entries = HashMap::new();
            for (&pc, &owner) in &self.code.entry_owner {
                if let Some(compiled) = self.code.compiled.get(&owner) {
                    entries.insert(pc, compiled.parallel_entry());
                }
            }
            let mut links = crate::linking::Table::default();
            if self.region_linking {
                links.rebuild(self.code.entry_owner.iter().filter_map(|(&pc, owner)| {
                    self.code.compiled.get(owner).map(|c| (pc, c.tail_entry))
                }));
            }
            self.parallel_image =
                Some((self.code.cache_revision, Arc::new(Image { entries, links })));
        }
        let image = self.parallel_image.as_ref().unwrap().1.clone();
        if !image.entries.contains_key(&self.state.eip) {
            return Err("parallel entry vanished before dispatch".into());
        }
        self.state.refresh_mmx();
        let memory = self.memory.worker_layout();
        let pool = self
            .parallel
            .as_mut()
            .ok_or("parallel execution is not enabled")?;
        let token = pool.next_token()?;
        pool.submit(Job {
            token,
            cpu: self.state.clone(),
            memory,
            image,
            stop,
            duration,
            guest_clock: (guest_us, Instant::now()),
            host_interrupt: if self.host_interrupts {
                self.interrupt.clone()
            } else {
                Arc::new(AtomicBool::new(false))
            },
            permit: pool.fence.enter(),
        })
    }

    pub(crate) fn poll_parallel(&mut self) -> Option<(u64, ExecutionContext)> {
        let completion = self.parallel.as_mut()?.poll()?;
        self.stats.native_entries += completion.entries;
        self.stats.native_blocks += completion.blocks;
        self.stats.bulk_calls += completion.bulk_calls;
        self.stats.bulk_bytes += completion.bulk_bytes;
        Some((
            completion.token,
            ExecutionContext {
                cpu: completion.cpu,
                deferred_fallback: None,
                offload: true,
                parallel_stop: Some(completion.stop),
            },
        ))
    }

    pub(crate) fn wait_parallel(&self, duration: Duration) -> Result<(), String> {
        self.parallel
            .as_ref()
            .ok_or("no parallel workers")?
            .wait(duration)
    }
}

impl Drop for Executor {
    fn drop(&mut self) {
        // Drop bodies run before fields. No code allocation or guest mapping
        // can be released while a worker holds a raw native pointer.
        self.memory.quiesce_workers();
        drop(self.parallel.take());
    }
}
