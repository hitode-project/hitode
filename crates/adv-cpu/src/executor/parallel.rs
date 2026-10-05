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
        self.parallel_stop.take()
    }
    pub(crate) fn cold_parallel_retry(&mut self) {
        self.deferred_fallback = Some(self.state.eip);
        self.admission.cold();
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
    ) -> Result<Option<u64>, String> {
        if self
            .parallel_image
            .as_ref()
            .is_none_or(|(revision, _)| *revision != self.code.cache_revision)
        {
            let started = Instant::now();
            let entries = self.code.parallel_entries.clone();
            let links = if self.region_linking {
                self.code.links.clone()
            } else {
                Arc::default()
            };
            self.parallel_image =
                Some((self.code.cache_revision, Arc::new(Image { entries, links })));
            self.parallel
                .as_mut()
                .unwrap()
                .record_image_build(started.elapsed());
        }
        let image = self.parallel_image.as_ref().unwrap().1.clone();
        if !image.entries.contains_key(&self.state.eip) {
            return Err("parallel entry vanished before dispatch".into());
        }
        // Building a new publication can consume the remaining host allowance.
        // Recheck after setup so an almost-expired job stays on the inline lane.
        if self.host_interrupt_pending()
            || self.deadline.is_some_and(|deadline| {
                deadline.saturating_duration_since(Instant::now()) < Duration::from_micros(100)
            })
        {
            return Ok(None);
        }
        self.state.refresh_mmx();
        let memory = self.memory.worker_layout();
        let pool = self
            .parallel
            .as_mut()
            .ok_or("parallel execution is not enabled")?;
        let token = pool.next_token()?;
        let submitted_at = Instant::now();
        let deadline = submitted_at
            .checked_add(duration)
            .ok_or("native deadline overflow")?;
        let deadline = self.deadline.map_or(deadline, |outer| outer.min(deadline));
        pool.submit(Job {
            token,
            cpu: self.state.clone(),
            memory,
            image,
            stop,
            submitted_at,
            deadline,
            admission: self.admission,
            guest_clock: (guest_us, Instant::now()),
            host_interrupt: if self.host_interrupts {
                self.interrupt.clone()
            } else {
                Arc::new(AtomicBool::new(false))
            },
            permit: pool.fence.enter(),
        })
        .map(Some)
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
                admission: completion.admission,
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
