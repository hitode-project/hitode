//! Bounded discovery from mapped image snapshots. Speculative native
//! entries remain private until execution observes a real guest entry.
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::time::{Duration, Instant};

use iced_x86::{Decoder, DecoderOptions, FlowControl, OpKind};

use super::Executor;
use crate::backend::CodeImageSink;
use crate::cliffs::InterpreterDetail;
use crate::compile::{Compiled, Snapshot};
use crate::{AotImage, CompileStrategy, Region};

pub const MAX_IMAGE_BYTES: usize = 8 * 1024 * 1024;
const MAX_SNAPSHOT_BYTES: usize = 32 * 1024 * 1024;
const MAX_REGIONS: usize = 512;
const MAX_INSTRUCTIONS: usize = 32 * 1024;
const MAX_REGION_INSTRUCTIONS: usize = 128;
const MAX_REGION_BYTES: usize = 8 * 1024;
const MAX_READY_BYTES: usize = 32 * 1024 * 1024;
const MAX_READY_REGIONS: usize = 512;
const POLL_INTERVAL: Duration = Duration::from_millis(10);
const CLOCK_CALLS: u32 = 8;

#[derive(Clone, Copy, Debug, Default)]
pub struct AotStats {
    pub images: u64,
    pub image_rejections: u64,
    pub candidates: u64,
    pub decoded_instructions: u64,
    pub budget_exhaustions: u64,
    pub compile_requests: u64,
    pub private_completions: u64,
    pub publications: u64,
    pub stale_candidates: u64,
    pub ready_regions: usize,
    pub ready_bytes: usize,
}

impl AotStats {
    pub fn json(&self) -> String {
        format!(
            "{{\"images\":{},\"image_rejections\":{},\"candidates\":{},\"decoded_instructions\":{},\"budget_exhaustions\":{},\"compile_requests\":{},\"private_completions\":{},\"publications\":{},\"stale_candidates\":{},\"ready_regions\":{},\"ready_bytes\":{}}}",
            self.images,
            self.image_rejections,
            self.candidates,
            self.decoded_instructions,
            self.budget_exhaustions,
            self.compile_requests,
            self.private_completions,
            self.publications,
            self.stale_candidates,
            self.ready_regions,
            self.ready_bytes,
        )
    }
}

struct Candidate {
    region: Region,
    expected: Vec<u8>,
}

struct Batch {
    candidates: Vec<Candidate>,
    instructions: usize,
    exhausted: bool,
}

struct Discovery {
    input: SyncSender<AotImage>,
    output: Receiver<Batch>,
}

impl Discovery {
    fn new(sink: CodeImageSink) -> std::io::Result<Self> {
        let (input, images) = mpsc::sync_channel::<AotImage>(2);
        let (batches, output) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("adv-aot-discovery".into())
            .spawn(move || {
                while let Ok(image) = images.recv() {
                    let batch = discover(&image);
                    sink.release(image.image.len());
                    if batches.send(batch).is_err() {
                        while let Ok(image) = images.try_recv() {
                            sink.release(image.image.len());
                        }
                        break;
                    }
                }
            })?;
        Ok(Self { input, output })
    }
}

struct Ready {
    region: Region,
    input: Snapshot,
    compiled: Compiled,
}

pub(super) struct State {
    pub(super) enabled: bool,
    registration: bool,
    sink: CodeImageSink,
    images: Receiver<AotImage>,
    discovery: Option<Discovery>,
    waiting_images: VecDeque<AotImage>,
    snapshot_bytes: usize,
    candidates: VecDeque<Candidate>,
    speculative: BTreeMap<u32, Region>,
    speculative_entries: BTreeMap<u32, u32>,
    ready: BTreeMap<u32, Ready>,
    ready_entries: BTreeMap<u32, u32>,
    ready_bytes: usize,
    admitting: bool,
    next_poll: Instant,
    /// Calls since the clock was last read for the speculative poll.
    calls: u32,
}

impl Default for State {
    fn default() -> Self {
        let (sink, images) = CodeImageSink::channel();
        Self {
            enabled: true,
            registration: true,
            sink,
            images,
            discovery: None,
            waiting_images: VecDeque::new(),
            snapshot_bytes: 0,
            candidates: VecDeque::new(),
            speculative: BTreeMap::new(),
            speculative_entries: BTreeMap::new(),
            ready: BTreeMap::new(),
            ready_entries: BTreeMap::new(),
            ready_bytes: 0,
            admitting: false,
            next_poll: Instant::now(),
            calls: 0,
        }
    }
}

impl State {
    pub(super) fn is_admitting(&self) -> bool {
        self.admitting
    }

    pub(super) fn is_speculative(&self, start: u32) -> bool {
        self.speculative.contains_key(&start)
    }

    pub(super) fn has_ready(&self) -> bool {
        !self.ready.is_empty()
    }
}

impl Executor {
    /// Enable bounded image discovery. Runtime-generated code keeps automatic JIT.
    pub fn set_aot_enabled(&mut self, enabled: bool) {
        self.aot.enabled = enabled;
        self.aot.registration = enabled || self.persistent_cache.is_some();
        if !enabled {
            while let Ok(image) = self.aot.images.try_recv() {
                self.aot.sink.release(image.image.len());
            }
            self.aot.candidates.clear();
            self.aot.discovery = None;
            self.aot.ready.clear();
            self.aot.ready_entries.clear();
            self.aot.ready_bytes = 0;
            self.stats.aot.ready_regions = 0;
            self.stats.aot.ready_bytes = 0;
            while let Some(image) = self.aot.waiting_images.pop_front() {
                self.aot.sink.release(image.image.len());
            }
        }
    }

    pub(crate) fn enable_code_image_registration(&mut self) {
        self.aot.registration = true;
    }

    /// The compiler client has already been canceled; its old tickets can no
    /// longer be delivered to a newly created client at the same guest start.
    pub(super) fn reset_aot_compilations(&mut self) {
        self.aot.speculative.clear();
        self.aot.speculative_entries.clear();
        self.aot.ready.clear();
        self.aot.ready_entries.clear();
        self.aot.ready_bytes = 0;
        self.stats.aot.ready_regions = 0;
        self.stats.aot.ready_bytes = 0;
    }

    pub(crate) fn code_image_sink(&self) -> Option<CodeImageSink> {
        self.aot.registration.then(|| self.aot.sink.clone())
    }

    /// Loading never waits for discovery or compilation capacity.
    pub fn register_code_image(&mut self, image: AotImage) {
        if self.aot.registration {
            self.aot.sink.try_send(image);
        }
    }

    /// Called before queuing warm regions, so cache identity sees new modules.
    pub(super) fn apply_aot(&mut self) -> Result<(), String> {
        if !self.aot.registration {
            return Ok(());
        }
        self.stats.aot.image_rejections += self.aot.sink.take_rejections();
        // Registration is lightweight even when static discovery is disabled.
        let images = self.aot.sink.take_sent();
        while images && let Ok(image) = self.aot.images.try_recv() {
            self.register_code_cache_module(image.base, image.size, image.identity, image.primary);
            if !self.aot.enabled || image.image.is_empty() {
                self.aot.sink.release(image.image.len());
                continue;
            }
            if image.image.len() > MAX_IMAGE_BYTES
                || self.aot.snapshot_bytes + image.image.len() > MAX_SNAPSHOT_BYTES
            {
                self.stats.aot.image_rejections += 1;
                self.aot.sink.release(image.image.len());
                continue;
            }
            if self.aot.discovery.is_none() {
                match Discovery::new(self.aot.sink.clone()) {
                    Ok(discovery) => self.aot.discovery = Some(discovery),
                    Err(_) => {
                        self.stats.aot.image_rejections += 1;
                        self.aot.sink.release(image.image.len());
                        continue;
                    }
                }
            }
            self.aot.snapshot_bytes += image.image.len();
            if image.primary {
                self.aot.waiting_images.push_front(image);
            } else {
                self.aot.waiting_images.push_back(image);
            }
        }
        while let Some(image) = self.aot.waiting_images.pop_front() {
            match self.aot.discovery.as_ref().unwrap().input.try_send(image) {
                Ok(()) => self.stats.aot.images += 1,
                Err(mpsc::TrySendError::Full(image)) => {
                    self.aot.waiting_images.push_front(image);
                    break;
                }
                Err(mpsc::TrySendError::Disconnected(image)) => {
                    self.aot.sink.release(image.image.len());
                    self.stats.aot.image_rejections += 1;
                }
            }
        }
        if !self.aot.enabled || !self.auto_jit || !self.async_compilation {
            return Ok(());
        }
        // Every executor run calls this; read the clock on a fraction of them.
        let calls = self.aot.calls;
        self.aot.calls = calls.wrapping_add(1);
        if !calls.is_multiple_of(CLOCK_CALLS) {
            return Ok(());
        }
        let now = Instant::now();
        if now < self.aot.next_poll {
            return Ok(());
        }
        self.aot.next_poll = now + POLL_INTERVAL;
        if let Some(discovery) = &self.aot.discovery {
            while let Ok(batch) = discovery.output.try_recv() {
                self.stats.aot.decoded_instructions += batch.instructions as u64;
                self.stats.aot.budget_exhaustions += u64::from(batch.exhausted);
                self.stats.aot.candidates += batch.candidates.len() as u64;
                let room = MAX_REGIONS.saturating_sub(self.aot.candidates.len());
                self.aot
                    .candidates
                    .extend(batch.candidates.into_iter().take(room));
            }
        }
        // Foreground work owns every already queued job. At most one speculative
        // region borrows an unused compiler lane on each owner poll.
        if !self.deferred_compilations.is_empty()
            || self.pending_regions.len() >= self.compile_threads.max(1)
            || self.aot.ready.len() >= MAX_READY_REGIONS
            || self.aot.ready_bytes >= MAX_READY_BYTES
        {
            return Ok(());
        }
        let Some(candidate) = self.aot.candidates.pop_front() else {
            return Ok(());
        };
        let region = candidate.region;
        let known = self.code.regions.contains_key(&region.start)
            || self.pending_regions.contains_key(&region.start)
            || self.aot.ready.contains_key(&region.start)
            || region.entries.iter().any(|pc| {
                self.code.entry_owner.contains_key(pc) || self.attempted.contains_key(pc)
            });
        if known {
            return Ok(());
        }
        if !self.memory.executable(region.start, region.len)
            || !self
                .memory
                .read(region.start, region.len)
                .is_ok_and(|bytes| bytes == candidate.expected)
        {
            self.stats.aot.stale_candidates += 1;
            return Ok(());
        }
        self.aot.admitting = true;
        let queued = self.queue_region(region.clone());
        self.aot.admitting = false;
        if queued? {
            self.stats.aot.compile_requests += 1;
        } else {
            // Rejected speculative work must not suppress runtime discovery.
            self.attempted.remove(&region.start);
            self.tier_attempted.remove(&region.start);
            for pc in region.entries {
                self.attempted.remove(&pc);
            }
        }
        Ok(())
    }

    /// Called while queue_region adds each responsive compilation fragment.
    pub(super) fn mark_aot_fragment(&mut self, region: &Region) {
        if self.aot.admitting {
            for &entry in region.entries.iter().chain(std::iter::once(&region.start)) {
                self.aot.speculative_entries.insert(entry, region.start);
            }
            self.aot.speculative.insert(region.start, region.clone());
        }
    }

    /// Stale or failed jobs cannot retain a speculative tag for a future job
    /// reusing the same start. This also applies to deferred-work rejection.
    pub(super) fn forget_aot_fragment(&mut self, start: u32) {
        self.aot.speculative.remove(&start);
        self.aot
            .speculative_entries
            .retain(|_, owner| *owner != start);
    }

    /// A completion may contain executable storage, but no public link slot or
    /// entry-owner index is installed until a guest reaches that region.
    pub(super) fn accept_aot_completion(
        &mut self,
        region: Region,
        input: Snapshot,
        compiled: Compiled,
    ) -> Option<(Region, Compiled)> {
        if !self.aot.is_speculative(region.start) {
            return Some((region, compiled));
        }
        self.forget_aot_fragment(region.start);
        self.stats.aot.private_completions += 1;
        if !self.aot.enabled
            || self.aot.ready.len() >= MAX_READY_REGIONS
            || self.aot.ready_bytes.saturating_add(compiled.code_size) > MAX_READY_BYTES
        {
            return None;
        }
        self.aot.ready_bytes += compiled.code_size;
        for &entry in &compiled.entries {
            self.aot.ready_entries.insert(entry, region.start);
        }
        self.aot.ready.insert(
            region.start,
            Ready {
                region,
                input,
                compiled,
            },
        );
        self.stats.aot.ready_regions = self.aot.ready.len();
        self.stats.aot.ready_bytes = self.aot.ready_bytes;
        None
    }

    /// Called at the observed execution PC before ordinary runtime discovery.
    pub(super) fn admit_aot(&mut self, pc: u32, stop: u32) {
        if !self.aot.enabled {
            return;
        }
        let start = self.aot.ready_entries.get(&pc).copied();
        if let Some(start) = start {
            let ready = self.aot.ready.remove(&start).unwrap();
            self.aot.ready_entries.retain(|_, owner| *owner != start);
            self.aot.ready_bytes -= ready.compiled.code_size;
            self.stats.aot.ready_regions = self.aot.ready.len();
            self.stats.aot.ready_bytes = self.aot.ready_bytes;
            let contains_stop = stop.wrapping_sub(ready.region.start) < ready.region.len as u32;
            if !contains_stop && ready.input.valid(&self.memory) {
                self.witness_cache_input(&ready.input);
                self.automatic_regions.insert(start);
                self.publish(ready.region, ready.compiled);
                self.stats.aot.publications += 1;
            } else {
                self.stats.aot.stale_candidates += 1;
            }
        }
        let start = self.aot.speculative_entries.get(&pc).copied();
        if let Some(start) = start {
            if self
                .aot
                .speculative
                .get(&start)
                .is_some_and(|region| stop.wrapping_sub(region.start) < region.len as u32)
            {
                return;
            }
            let region = self.aot.speculative.remove(&start).unwrap();
            self.aot
                .speculative_entries
                .retain(|_, owner| *owner != start);
            if self.pending_regions.contains_key(&start)
                || self.deferred_compilations.contains_key(&start)
            {
                self.automatic_regions.insert(start);
                self.attempted
                    .insert(start, InterpreterDetail::NoPublishedEntry);
                for &entry in &region.entries {
                    self.attempted
                        .insert(entry, InterpreterDetail::NoPublishedEntry);
                }
                self.index_region(region);
            }
        }
    }
}

fn executable(image: &AotImage, rva: u32, len: usize) -> bool {
    let end = u64::from(rva) + len as u64;
    end <= image.image.len() as u64
        && image
            .executable
            .iter()
            .any(|&(start, limit)| rva >= start && end <= u64::from(limit))
}

fn discover(image: &AotImage) -> Batch {
    let mut pending: VecDeque<_> = image.seeds.iter().copied().collect();
    let mut seen = BTreeSet::new();
    let mut candidates = Vec::new();
    let mut instructions = 0;
    while let Some(entry) = pending.pop_front() {
        if candidates.len() >= MAX_REGIONS || instructions >= MAX_INSTRUCTIONS {
            return Batch {
                candidates,
                instructions,
                exhausted: true,
            };
        }
        if !seen.insert(entry) || !executable(image, entry, 1) {
            continue;
        }
        let mut paths = vec![entry];
        let mut decoded = BTreeSet::new();
        let mut frontier = BTreeSet::new();
        let mut start = entry;
        let mut end = u64::from(entry);
        while let Some(mut rva) = paths.pop() {
            loop {
                if decoded.contains(&rva) {
                    break;
                }
                if decoded.len() >= MAX_REGION_INSTRUCTIONS || instructions >= MAX_INSTRUCTIONS {
                    frontier.insert(rva);
                    break;
                }
                let Some(pc) = image.base.checked_add(rva) else {
                    break;
                };
                let Some((_, limit)) = image
                    .executable
                    .iter()
                    .find(|&&(lo, hi)| rva >= lo && rva < hi)
                else {
                    break;
                };
                let available = (usize::try_from(*limit - rva).unwrap_or(0)).min(15);
                let Some(bytes) = image.image.get(rva as usize..rva as usize + available) else {
                    break;
                };
                let instruction =
                    Decoder::with_ip(32, bytes, u64::from(pc), DecoderOptions::NONE).decode();
                if instruction.is_invalid() || !executable(image, rva, instruction.len()) {
                    break;
                }
                let next_start = start.min(rva);
                let next_end = end.max(u64::from(rva) + instruction.len() as u64);
                if next_end - u64::from(next_start) > MAX_REGION_BYTES as u64
                    || !executable(
                        image,
                        next_start,
                        (next_end - u64::from(next_start)) as usize,
                    )
                {
                    frontier.insert(rva);
                    break;
                }
                start = next_start;
                end = next_end;
                decoded.insert(rva);
                instructions += 1;
                let target = (instruction.op0_kind() == OpKind::NearBranch32)
                    .then(|| instruction.near_branch32().checked_sub(image.base))
                    .flatten()
                    .filter(|&target| executable(image, target, 1));
                match instruction.flow_control() {
                    FlowControl::ConditionalBranch => {
                        if let Some(target) = target {
                            paths.push(target);
                        }
                    }
                    FlowControl::UnconditionalBranch => {
                        if let Some(target) = target {
                            paths.push(target);
                        }
                        break;
                    }
                    FlowControl::Call => {
                        if let Some(target) = target {
                            frontier.insert(target);
                        }
                    }
                    FlowControl::IndirectBranch | FlowControl::Return | FlowControl::Exception => {
                        break;
                    }
                    _ => {}
                }
                let Some(next) = instruction.next_ip32().checked_sub(image.base) else {
                    break;
                };
                rva = next;
            }
        }
        if decoded.is_empty() {
            continue;
        }
        frontier.retain(|rva| !decoded.contains(rva));
        // Calls reached from the startup entry outrank unobserved exports and
        // relocated pointers that were supplied as initial seeds.
        for &target in frontier.iter().rev() {
            pending.push_front(target);
        }
        let Some(base) = image.base.checked_add(start) else {
            continue;
        };
        let Some(entry) = image.base.checked_add(entry) else {
            continue;
        };
        let len = (end - u64::from(start)) as usize;
        let mut region = Region::new(base, len);
        region.entries.push(entry);
        region.frontier = frontier
            .into_iter()
            .filter_map(|rva| image.base.checked_add(rva))
            .collect();
        region.reachable = true;
        region.compile_strategy = CompileStrategy::Fast;
        candidates.push(Candidate {
            region,
            expected: image.image[start as usize..start as usize + len].to_vec(),
        });
    }
    Batch {
        candidates,
        instructions,
        exhausted: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EXEC, Memory, READ, WRITE};
    use iced_x86::code_asm::{CodeAssembler, eax};

    fn image() -> AotImage {
        let mut code = CodeAssembler::new(32).unwrap();
        let mut callee = code.create_label();
        code.call(callee).unwrap();
        code.ret().unwrap();
        code.set_label(&mut callee).unwrap();
        code.mov(eax, 7).unwrap();
        code.ret().unwrap();
        let bytes = code.assemble(0).unwrap();
        AotImage {
            identity: AotImage::identity(&bytes),
            base: 0,
            size: bytes.len() as u32,
            primary: true,
            executable: vec![(0, bytes.len() as u32)],
            image: bytes,
            seeds: vec![0],
        }
    }

    #[test]
    fn direct_calls_discover_independent_callees() {
        let batch = discover(&image());
        assert_eq!(batch.candidates.len(), 2);
        assert_eq!(batch.instructions, 4);
        assert!(!batch.exhausted);
        assert!(!batch.candidates[0].region.frontier.is_empty());
    }

    #[test]
    fn data_seeds_and_cross_section_decoding_are_rejected() {
        let mut image = image();
        image.executable = vec![(0, 1)];
        image.seeds.push(1);
        assert!(discover(&image).candidates.is_empty());
    }

    #[test]
    fn discovery_has_a_finite_instruction_budget() {
        let image = AotImage {
            identity: [0; 32],
            base: 0,
            size: MAX_INSTRUCTIONS as u32 + 1,
            primary: true,
            image: vec![0x90; MAX_INSTRUCTIONS + 1],
            executable: vec![(0, MAX_INSTRUCTIONS as u32 + 1)],
            seeds: vec![0],
        };
        let batch = discover(&image);
        assert_eq!(batch.instructions, MAX_INSTRUCTIONS);
        assert!(batch.exhausted);
        assert!(batch.candidates.len() <= MAX_REGIONS);
    }

    #[test]
    fn snapshot_capacity_keeps_module_metadata_and_reserves_the_primary_image() {
        let (sink, images) = CodeImageSink::channel();
        for primary in [false, false, false, false, true] {
            sink.try_send(AotImage {
                identity: [0; 32],
                base: 0,
                size: MAX_IMAGE_BYTES as u32,
                primary,
                image: vec![0; MAX_IMAGE_BYTES],
                executable: Vec::new(),
                seeds: Vec::new(),
            });
        }
        let images: Vec<_> = images.try_iter().collect();
        assert_eq!(images.len(), 5);
        assert!(images[3].image.is_empty());
        assert_eq!(images[4].image.len(), MAX_IMAGE_BYTES);
        assert_eq!(sink.take_rejections(), 1);
        assert_eq!(
            images.iter().map(|image| image.image.len()).sum::<usize>(),
            MAX_SNAPSHOT_BYTES
        );
    }

    #[test]
    fn speculative_entries_publish_only_after_observation_and_reject_smc() {
        for mutate in [false, true] {
            let image = image();
            let mut executor = Executor::create(Memory::reserve(0, 0x4000).unwrap()).unwrap();
            executor.set_aot_enabled(true);
            executor.memory.map(0, 4096, READ | WRITE | EXEC).unwrap();
            executor.memory.write(0, &image.image).unwrap();
            let region = discover(&image).candidates.pop().unwrap().region;
            let input = Snapshot::capture(&region, &mut executor.memory).unwrap();
            let compiled = crate::compile::compile_snapshot(&input).unwrap();
            executor
                .aot
                .speculative
                .insert(region.start, region.clone());
            assert!(
                executor
                    .accept_aot_completion(region.clone(), input, compiled)
                    .is_none()
            );
            assert!(executor.code.compiled.is_empty());
            if mutate {
                executor.memory.write(region.start, &[0x90]).unwrap();
            }
            executor.admit_aot(region.entries[0], u32::MAX);
            assert_eq!(executor.code.compiled.is_empty(), mutate);
            assert!(executor.aot.ready.is_empty());
        }
    }

    #[test]
    fn an_authored_stop_inside_a_candidate_keeps_interpreter_execution() {
        let image = image();
        let mut executor = Executor::create(Memory::reserve(0, 0x4000).unwrap()).unwrap();
        executor.set_aot_enabled(true);
        executor.memory.map(0, 4096, READ | WRITE | EXEC).unwrap();
        executor.memory.write(0, &image.image).unwrap();
        let region = discover(&image).candidates.pop().unwrap().region;
        let input = Snapshot::capture(&region, &mut executor.memory).unwrap();
        let compiled = crate::compile::compile_snapshot(&input).unwrap();
        executor
            .aot
            .speculative
            .insert(region.start, region.clone());
        assert!(
            executor
                .accept_aot_completion(region.clone(), input, compiled)
                .is_none()
        );
        executor.admit_aot(region.entries[0], region.start + 1);
        assert!(executor.code.compiled.is_empty());
    }
}
