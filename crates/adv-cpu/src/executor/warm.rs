//! Warm-start profile: the automatic regions of a previous run, queued for
//! background compilation as soon as identical guest code is mapped again.
//! Entries only name guest ranges; every compilation still snapshots and
//! validates the live code, so a stale or foreign profile costs work, never
//! correctness.
use std::time::{Duration, Instant};

use super::Executor;
use crate::{CompileStrategy, Region};

const MAGIC: &[u8; 8] = b"ADVWARM1";
/// Bounded by the deferred queue so one import cannot crowd out discovery.
const LIMIT: usize = 2048;
/// Modules mapped later than this after import are left to normal discovery.
const PATIENCE: Duration = Duration::from_secs(60);
const RETRY: Duration = Duration::from_millis(250);

pub(super) struct Pending {
    regions: Vec<(Region, u64)>,
    loaded: Instant,
    next_try: Instant,
}

fn hash(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

struct Reader<'a>(&'a [u8]);
impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> Option<[u8; N]> {
        let (head, rest) = self.0.split_first_chunk::<N>()?;
        self.0 = rest;
        Some(*head)
    }
    fn u32(&mut self) -> Option<u32> {
        self.take().map(u32::from_le_bytes)
    }
    fn list(&mut self) -> Option<Vec<u32>> {
        let count = self.u32()? as usize;
        if count > self.0.len() / 4 {
            return None;
        }
        (0..count).map(|_| self.u32()).collect()
    }
}

impl Executor {
    /// Serialize resident automatic regions, most recently used last.
    pub fn warm_profile(&self) -> Vec<u8> {
        let mut regions: Vec<_> = self
            .compiled
            .iter()
            .filter(|(start, _)| self.automatic_regions.contains(start))
            .filter_map(|(start, compiled)| Some((compiled.cache_stamp, self.regions.get(start)?)))
            .collect();
        regions.sort_by_key(|(stamp, _)| *stamp);
        let skip = regions.len().saturating_sub(LIMIT);
        let mut out = MAGIC.to_vec();
        let mut count = 0u32;
        let mut body = Vec::new();
        for (_, region) in regions.into_iter().skip(skip) {
            let Ok(bytes) = self.memory.read(region.start, region.len) else {
                continue;
            };
            body.extend(region.start.to_le_bytes());
            body.extend((region.len as u32).to_le_bytes());
            body.push(u8::from(region.reachable) | u8::from(region.complete_tail) << 1);
            body.extend(hash(bytes).to_le_bytes());
            for list in [&region.entries, &region.frontier] {
                body.extend((list.len() as u32).to_le_bytes());
                for pc in list {
                    body.extend(pc.to_le_bytes());
                }
            }
            count += 1;
        }
        out.extend(count.to_le_bytes());
        out.extend(body);
        out
    }

    /// Accept a profile from `warm_profile`. Malformed input is ignored whole.
    pub fn load_warm_profile(&mut self, bytes: &[u8]) -> usize {
        let Some(regions) = Self::parse_warm(bytes) else {
            return 0;
        };
        let count = regions.len();
        let now = Instant::now();
        self.warm = (count != 0).then_some(Pending {
            regions,
            loaded: now,
            next_try: now,
        });
        count
    }

    fn parse_warm(bytes: &[u8]) -> Option<Vec<(Region, u64)>> {
        let mut reader = Reader(bytes.strip_prefix(MAGIC)?);
        let count = reader.u32()? as usize;
        let mut regions = Vec::with_capacity(count.min(LIMIT));
        for _ in 0..count.min(LIMIT) {
            let start = reader.u32()?;
            let len = reader.u32()? as usize;
            let [flags] = reader.take()?;
            let hash = u64::from_le_bytes(reader.take()?);
            let entries = reader.list()?;
            let frontier = reader.list()?;
            if len == 0 || u64::from(start) + len as u64 > 1 << 32 {
                return None;
            }
            let mut region = Region::new(start, len);
            region.entries = entries;
            region.frontier = frontier;
            region.reachable = flags & 1 != 0;
            region.complete_tail = flags & 2 != 0;
            region.compile_strategy = CompileStrategy::Fast;
            regions.push((region, hash));
        }
        Some(regions)
    }

    /// Queue profile regions whose code is mapped and unchanged. Called from the
    /// execution owner between native runs; bounded by the retry interval.
    pub(super) fn apply_warm(&mut self) -> Result<(), String> {
        let now = Instant::now();
        let Some(pending) = self.warm.as_mut() else {
            return Ok(());
        };
        if now < pending.next_try {
            return Ok(());
        }
        pending.next_try = now + RETRY;
        let expired = now.duration_since(pending.loaded) > PATIENCE;
        let mut regions = std::mem::take(&mut pending.regions);
        let mut kept = Vec::new();
        // Queue the most recently used last so they are compiled first.
        for (region, expected) in regions.drain(..) {
            if !self.auto_jit || !self.async_compilation {
                kept.push((region, expected));
                continue;
            }
            if !self.memory.executable(region.start, region.len) {
                if !expired {
                    kept.push((region, expected));
                }
                continue;
            }
            let matches = self
                .memory
                .read(region.start, region.len)
                .is_ok_and(|bytes| hash(bytes) == expected);
            let known = self.regions.contains_key(&region.start)
                || std::iter::once(&region.start)
                    .chain(&region.entries)
                    .any(|pc| self.entry_owner.contains_key(pc) || self.attempted.contains(pc));
            if !matches || known {
                continue;
            }
            let entries = region.entries.clone();
            let start = region.start;
            if self.queue_region(region.clone())? {
                self.stats.warm_requests += 1;
                self.attempted.insert(start);
                self.attempted.extend(entries);
            } else if !expired {
                kept.push((region, expected));
            }
        }
        if kept.is_empty() {
            self.warm = None;
        } else if let Some(pending) = self.warm.as_mut() {
            pending.regions = kept;
        }
        Ok(())
    }
}
