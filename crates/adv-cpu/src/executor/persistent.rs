//! Optional on-disk native code, restored through the ordinary owner publisher.
use super::Executor;
use crate::{CompileStrategy, compile};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub(super) struct Pending {
    regions: Vec<crate::Region>,
    loaded: Instant,
    next_try: Instant,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EXEC, Memory, READ, Region, RunOutcome, WRITE};
    use iced_x86::code_asm::{CodeAssembler, eax};
    use std::sync::atomic::{AtomicU64, Ordering};

    fn fixture(value: u32) -> (Executor, Region) {
        let mut code = CodeAssembler::new(32).unwrap();
        code.mov(eax, value).unwrap();
        code.jmp(0x3000u64).unwrap();
        let bytes = code.assemble(0x1000).unwrap();
        let mut memory = Memory::reserve(0, 0x4000).unwrap();
        memory.map(0x1000, 4096, READ | WRITE | EXEC).unwrap();
        memory.write(0x1000, &bytes).unwrap();
        let mut executor = Executor::create(memory).unwrap();
        executor.enable_auto_jit(true);
        executor.set_async_compilation(true);
        executor.set_tiered_compilation(false);
        executor.register_code_cache_module(0x1000, 4096, crate::AotImage::identity(&bytes), true);
        executor.state.eip = 0x1000;
        let mut region = Region::new(0x1000, bytes.len());
        region.compile_strategy = CompileStrategy::Fast;
        (executor, region)
    }

    #[test]
    fn persistent_hit_avoids_compilation_and_smc_invalidates_the_restored_entry() {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let directory = std::env::temp_dir().join(format!(
            "adv-native-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("jit-code.bin");
        let (mut first, region) = fixture(7);
        let mut input = compile::Snapshot::capture(&region, &mut first.memory).unwrap();
        first.locate_cache_input(&mut input);
        let compiled = compile::compile_snapshot(&input).unwrap();
        let cache = crate::code_cache::Cache::open(&path);
        cache.store(&input, &compiled);
        assert!(
            cache.regions().is_empty(),
            "unobserved AOT must remain private after restart"
        );
        cache.witness(&input);
        cache.save().unwrap();
        cache.save().unwrap(); // Atomic replacement also works with an existing file.
        drop(compiled);
        drop(first);

        let (mut second, _) = fixture(7);
        second.set_persistent_code_cache(&path).unwrap();
        assert_eq!(second.run(0x3000, 100).unwrap(), RunOutcome::ReachedStop);
        assert_eq!(second.state.regs[0], 7);
        assert_eq!(second.stats.compilations, 0);
        assert_eq!(second.stats.compile_requests, 0);
        assert!(second.persistent_code_cache_json().contains("\"hits\":1"));
        assert_eq!(second.stats.native_entries, 1);
        assert!(second.set_shadow_returns(true).is_err());

        let (mut full, cached_region) = fixture(7);
        full.set_persistent_code_cache(&path).unwrap();
        for index in 0..crate::compiler::CAPACITY {
            full.pending_regions
                .insert(0x2000 + index as u32, index as u64);
        }
        assert!(full.queue_region(cached_region).unwrap());
        assert_eq!(full.stats.compile_requests, 0);
        assert_eq!(full.pending_regions.len(), crate::compiler::CAPACITY);
        assert!(full.code.compiled.contains_key(&region.start));
        drop(full);

        let (changed, _) = fixture(9);
        let bytes = changed.memory.read(0x1000, region.len).unwrap().to_vec();
        second.write(0x1000, &bytes).unwrap();
        second.state.eip = 0x1000;
        assert_eq!(second.run(0x3000, 100).unwrap(), RunOutcome::ReachedStop);
        assert_eq!(second.state.regs[0], 9);
        assert_eq!(second.stats.invalidations, 1);

        let (mut mismatch, _) = fixture(9);
        mismatch.set_persistent_code_cache(&path).unwrap();
        assert_eq!(mismatch.run(0x3000, 100).unwrap(), RunOutcome::ReachedStop);
        assert_eq!(mismatch.stats.native_entries, 0);
        assert_eq!(mismatch.state.regs[0], 9);
        std::fs::write(&path, b"interrupted archive").unwrap();
        let (mut corrupted, _) = fixture(7);
        corrupted.set_persistent_code_cache(&path).unwrap();
        assert_eq!(corrupted.run(0x3000, 100).unwrap(), RunOutcome::ReachedStop);
        assert!(
            corrupted
                .persistent_code_cache_json()
                .contains("\"corrupt\":1")
        );
        drop((cache, second, mismatch, corrupted));
        std::fs::remove_dir_all(directory).unwrap();
    }
}

impl Executor {
    /// Enable a local cache before compilation starts. No environment selector
    /// is implicit in the CPU API; the frontend supplies its writable data root.
    pub fn set_persistent_code_cache(&mut self, path: &Path) -> Result<(), String> {
        if self.compiler.is_some()
            || self.stats.compilations != 0
            || !self.code.compiled.is_empty()
            || self.aot.has_ready()
        {
            return Err("code cache must be configured before compilation".into());
        }
        let cache = crate::code_cache::Cache::open(path);
        let now = Instant::now();
        self.cached_pending = Some(Pending {
            regions: cache.regions(),
            loaded: now,
            next_try: now,
        });
        self.persistent_cache = Some(cache);
        self.enable_code_image_registration();
        Ok(())
    }

    pub fn save_persistent_code_cache(&self) -> Result<(), String> {
        self.persistent_cache.as_ref().map_or(Ok(()), |cache| {
            cache.save().map_err(|error| error.to_string())
        })
    }

    pub fn persistent_code_cache_json(&self) -> String {
        self.persistent_cache
            .as_ref()
            .map_or_else(|| "{\"enabled\":false}".into(), |cache| cache.json())
    }

    pub(crate) fn register_code_cache_module(
        &mut self,
        base: u32,
        size: u32,
        identity: [u8; 32],
        primary: bool,
    ) {
        if primary {
            self.cache_guest = Some(identity);
        }
        if size != 0 && u64::from(base) + u64::from(size) <= 1 << 32 {
            self.cache_images.retain(|&(old, _, _)| old != base);
            if self.cache_images.len() < 1024 {
                self.cache_images.push((base, size, identity));
            }
        }
    }

    pub(super) fn witness_cache_input(&self, input: &compile::Snapshot) {
        if let Some(cache) = &self.persistent_cache {
            cache.witness(input);
        }
    }

    pub(super) fn locate_cache_input(&self, input: &mut compile::Snapshot) {
        input.cache_capture = self.persistent_cache.is_some() && self.cache_guest.is_some();
        input.cache_guest = self.cache_guest;
        input.cache_location = self
            .cache_images
            .iter()
            .find_map(|&(base, size, identity)| {
                let relative = input.region.start.checked_sub(base)?;
                (relative < size).then_some((base, relative, identity))
            });
    }

    /// Previously executed regions may be restored as soon as mapped. Unlike a
    /// speculative AOT result these entries have a prior execution witness.
    /// An incompatible archive never submits compile work just by being loaded.
    pub(super) fn apply_code_cache(&mut self, stop: u32) {
        if !self.auto_jit || !self.async_compilation {
            return;
        }
        let Some(cache) = self.persistent_cache.clone() else {
            return;
        };
        let Some(mut pending) = self.cached_pending.take() else {
            return;
        };
        let now = Instant::now();
        if now < pending.next_try {
            self.cached_pending = Some(pending);
            return;
        }
        let mut kept = Vec::new();
        pending.regions.sort_by_key(|region| {
            !(region.start == self.state.eip || region.entries.contains(&self.state.eip))
        });
        let mut attempted = 0;
        for region in pending.regions.drain(..) {
            if stop.wrapping_sub(region.start) < region.len as u32 {
                continue;
            }
            if self.code.compiled.contains_key(&region.start) {
                continue;
            }
            if attempted >= 8 || !self.memory.executable(region.start, region.len) {
                if now.duration_since(pending.loaded) < Duration::from_secs(60) {
                    kept.push(region);
                }
                continue;
            }
            attempted += 1;
            let Ok(mut input) = compile::Snapshot::capture(&region, &mut self.memory) else {
                continue;
            };
            input.shadow_returns = self.shadow_returns.is_some();
            if self.tiered_compilation
                && self.parallel.is_none()
                && region.compile_strategy == CompileStrategy::Fast
            {
                input.profile = Some(Arc::default());
            }
            self.locate_cache_input(&mut input);
            if let Some(compiled) = cache.restore(&input) {
                self.automatic_regions.insert(region.start);
                self.publish(region, compiled);
            }
        }
        if !kept.is_empty() {
            pending.regions = kept;
            // Drain available entries promptly; only mapping retries wait.
            pending.next_try = if attempted == 8 {
                now
            } else {
                now + Duration::from_millis(250)
            };
            self.cached_pending = Some(pending);
        }
    }
}
