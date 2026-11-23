//! Executable units, link records, fault recovery and page watches.
//!
//! One chain holds the checked resident prefix and Tier-1 suffix for each
//! guest entry. The store publishes their code, links and watches together.
//! Detached worker placements retain their own fault sites.

use rustc_hash::{FxHashMap, FxHashSet};
use std::cell::Cell;
use std::collections::BTreeMap;
use std::sync::Arc;

use crate::cache::CodeCache;
use crate::encode::{NOP, b, bl};
use crate::facts::Key;
use adv_cpu_core::{CpuState, Memory};

pub(super) use crate::translate::lower::resident::OfferEntry;

use crate::runtime::{self, Enter, ShadowStack};
use crate::translate::front::{Decodes, Unit, decode::Helper, trace::Trace};
use crate::translate::lower::{
    Link, Site, Stubs, VersionLayout,
    flags::{Operand, Sense},
    resident::{self as ingress, Counter, Slot},
};

#[derive(Clone, Copy)]
pub(super) struct Tier1Unit {
    pub(super) unit: usize,
    pub(super) body: usize,
    pub(super) miss: Option<usize>,
    pub(super) retired: u32,
}

use crate::policy::tier1::CHAIN_LEN;
pub(super) const EMPTY: [u64; 2] = [u64::MAX, 0];

pub(super) struct Chain<R> {
    pub(super) resident: Option<Box<ResidentSite<R>>>,
    offer: Option<(usize, usize)>,
}

impl<R> Default for Chain<R> {
    fn default() -> Self {
        Self {
            resident: None,
            offer: None,
        }
    }
}

pub(super) struct Tier1Chain {
    pub(super) unit: Arc<Unit>,
    pub(super) mask: u64,
    pub(super) helpers: u32,
    pub(super) versions: Vec<Version>,
}

pub(super) struct Tier1Placement {
    entry: Tier1Unit,
    table_target: bool,
}

#[derive(Clone, Copy)]
pub(super) struct Version {
    pub(super) key: Key,
    pub(super) entry: Tier1Unit,
}

#[derive(Debug, Default)]
pub(super) struct VersionStats {
    pub(super) built: u64,
    pub(super) misses: u64,
    pub(super) joins: u64,
    pub(super) resident_misses: [Cell<u64>; 2],
    pub(super) arrivals: [Cell<u64>; CHAIN_LEN],
    pub(super) spans: [Cell<u64>; crate::translate::lower::resident::COUNTERS],
}

#[derive(Clone, Copy)]
pub(super) struct Linked {
    pub(super) point: usize,
    pub(super) original: u32,
    pub(super) next: u32,
    pub(super) back: bool,
    pub(super) direct: Option<usize>,
}

impl Linked {
    pub(super) fn branch(&self, target: usize) -> u32 {
        if self.original & 0xFC00_0000 == 0x9400_0000 {
            bl((target as i64 - self.point as i64) as i32)
        } else {
            b((target as i64 - self.point as i64) as i32)
        }
    }
}

pub(super) struct InternalEdge {
    pub(super) point: usize,
    pub(super) target: usize,
    pub(super) transition: usize,
    pub(super) link: usize,
    pub(super) source: u32,
    pub(super) unit: usize,
    pub(super) source_pc: u32,
    pub(super) back: bool,
    pub(super) sensor: Option<(usize, [u32; runtime::SENSOR_BYTES / 4])>,
}

#[derive(Default)]
pub(super) struct SlowPaths {
    pub(super) interpreter: BTreeMap<(u32, InterpreterCause), u64>,
    pub(super) values: BTreeMap<u32, u64>,
}

pub(super) struct ResidentSite<R> {
    pub unit: Option<Arc<Unit>>,
    pub sample: Box<Cell<u32>>,
    cold_completions: Box<Cell<u32>>,
    pub versions: Vec<ResidentVersion<R>>,
    pub fallback: usize,
    pub suspended: bool,
    pub(super) row_entry: Option<RowEntry>,
    pub row_feedback: Option<Box<ingress::RowFeedback>>,
    pub row_router_words: u32,
    row_retry: Option<(usize, [usize; 2])>,
}

pub(super) struct RowEntry {
    pub(super) slot: (u8, u32),
    address: usize,
    fallback: Option<[usize; 2]>,
}

impl<R> ResidentSite<R> {
    pub fn entry(&self) -> Option<usize> {
        if self.suspended {
            return None;
        }
        self.row_entry.as_ref().map(|row| row.address).or_else(|| {
            self.versions
                .iter()
                .find(|v| v.enabled && v.row_target.is_none())
                .map(|v| v.probe)
        })
    }
}

pub(super) fn place_stubs(capacity: usize, words: &[u32]) -> Result<(CodeCache, usize), String> {
    let mut cache = CodeCache::new(capacity)?;
    let base = cache.append(words).ok_or("stubs do not fit")?;
    Ok((cache, base))
}

pub(super) fn branch(from: usize, to: usize) -> u32 {
    b((to as i64 - from as i64) as i32)
}

pub(super) struct ResidentVersion<R> {
    pub entry: usize,
    pub proved: Option<usize>,
    proved_cold: Option<usize>,
    pub probe: usize,
    probe_word: Option<u32>,
    pub next: usize,
    pub miss: usize,
    miss_counter: Option<ingress::MissCounter>,
    pub cold: usize,
    pub retry: usize,
    pub environment: Vec<usize>,
    pub domain_fallback: Option<usize>,
    pub domain_guards: Vec<u32>,
    pub domain_guard_words: u32,
    pub domain_saved_per_iter: u32,
    pub at: CpuState,
    pub key: Key,
    pub plain_sites: Vec<u32>,
    pub unit: Arc<Unit>,
    pub row_price: Option<crate::policy::proved::RowPrice>,
    pub row_target: Option<crate::translate::lower::resident::RowTarget>,
    row_sample: Option<(usize, u32, u32)>,
    pub row_measuring: bool,
    pub enabled: bool,
    row_failures: Option<Box<[Cell<u64>; 6]>>,
    pub recovery: R,
    pub slot: Box<Slot>,
    pub probation: bool,
    pub probation_sensors: Vec<(usize, u32)>,
    probation_counter: usize,
    pub iteration_retired: u64,
    pub accounting: Vec<(usize, u32)>,
    split: Option<PlacedSplit>,
    offloaded: Cell<(u64, u64, u64)>,
}

struct PlacedSplit {
    sensor: usize,
    active: u32,
    threshold: usize,
    published: Option<u64>,
}

impl<R> ResidentVersion<R> {
    pub fn iterations(&self, retired: u64) -> u64 {
        if self.row_price.is_some() {
            0
        } else {
            retired.checked_div(self.iteration_retired).unwrap_or(0)
        }
    }

    pub fn account_rows(&self, rows: u32, work: u32) {
        if rows != 0 {
            let (entries, completed, paid_work) = self.offloaded.get();
            self.offloaded.set((
                entries + 1,
                completed + u64::from(rows),
                paid_work + u64::from(rows) * u64::from(work),
            ));
        }
    }

    pub fn row_counts(&self) -> (u64, u64, u64) {
        let (entries, rows, work) = self.offloaded.get();
        (
            self.slot.entries.get() + entries,
            self.slot.rows.get() + rows,
            self.slot.row_work.get() + work,
        )
    }

    pub fn row_failures(&self) -> [u64; 6] {
        self.row_failures
            .as_ref()
            .map_or([0; 6], |counts| counts.each_ref().map(Cell::get))
    }

    pub fn account(&mut self, cache: &mut crate::cache::CodeCache, enabled: bool) {
        if let Some((point, settled, active)) = self.row_sample {
            cache.patch(point, if enabled { active } else { settled });
            self.row_measuring = enabled;
        }
        for &(point, call) in &self.accounting {
            cache.patch(point, if enabled { call } else { NOP });
        }
        if !self.probation {
            let skip = if enabled {
                runtime::SENSOR_BYTES
            } else {
                runtime::SENSOR_BYTES + 4
            };
            for &(point, _) in &self.probation_sensors {
                cache.patch(point, b(skip as i32));
            }
        }
    }
}

/// Bitmask of the 64-byte lines within `page` covered by `start..start+len`.
pub(super) fn page_line_mask(page: u32, start: u32, len: u64) -> u64 {
    let page_start = u64::from(page);
    let from = u64::from(start).max(page_start) - page_start;
    let to = (u64::from(start) + len - 1).min(page_start + 4095) - page_start;
    let first = (from >> 6) as u32;
    let last = (to >> 6) as u32;
    let count = last - first + 1;
    if count == 64 {
        u64::MAX
    } else {
        ((1u64 << count) - 1) << first
    }
}

pub(super) struct CodeStore<R> {
    pub(super) resident_exit: usize,
    /// Switch-table slots by guest target: source entry, host slot and
    /// original shared stub. Dispatch looks targets up on every miss.
    pub(super) rows: FxHashMap<u32, Vec<(u32, usize, usize)>>,
    /// Entries of units that own switch tables.
    pub(super) table_sources: FxHashSet<u32>,
    pub(super) cache: CodeCache,
    pub(super) enter: Enter,
    pub(super) opt_enter: Enter,
    pub(super) stubs: Stubs,
    /// Cache bytes the stubs occupy; clearing keeps them.
    pub(super) stub_len: usize,
    /// Checked Tier-1 entries are the common dispatch index.
    pub(super) tier1: FxHashMap<u32, Tier1Placement>,
    /// Only keyed entries own a trace and a version list.
    pub(super) tier1_chains: FxHashMap<u32, Tier1Chain>,
    pub(super) chains: FxHashMap<u32, Chain<R>>,
    pub(super) version_stats: Option<FxHashMap<u32, Box<VersionStats>>>,
    pub(super) slow_paths: Option<SlowPaths>,
    #[cfg(test)]
    pub(super) value_counts: FxHashMap<u32, u64>,
    /// Unit entries and their watched 64-byte line masks by guest page.
    pub(super) pages: FxHashMap<u32, Vec<(u32, u64)>>,
    pub(super) pages_by_unit: FxHashMap<u32, Vec<u32>>,
    /// Patched link points, with the words they held, by the unit they
    /// branch to: the head of a list in `linked`.
    pub(super) links: FxHashMap<u32, u32>,
    /// Link point, the word it held, and the next point to the same unit.
    pub(super) linked: Vec<Linked>,
    /// Link points of the units the current translation placed.
    pub(super) points: Vec<(usize, u32, bool)>,
    pub(super) back_points: FxHashSet<usize>,
    pub(super) direct_points: FxHashMap<usize, usize>,
    pub(super) point_facts: FxHashMap<usize, Key>,
    pub(super) internal_edges: FxHashMap<u32, Vec<InternalEdge>>,
    /// Emitted instruction lengths by entry and guest EIP for probation.
    pub(super) tier1_words: FxHashMap<u32, FxHashMap<u32, u32>>,
    pub(super) uses_permissions: bool,
    /// Each placed unit's host address and entry, ascending.
    pub(super) placed: Vec<(usize, u32)>,
    /// Prepared operand operations, indexed by generated value calls.
    pub(super) helpers: Vec<Helper>,
    /// Counts cache clears, which invalidate link points.
    pub(super) generation: u64,
    /// The indirect table the runtime points at.
    pub(super) table: Box<[[u64; 2]]>,
    pub(super) shadow: Arc<ShadowStack>,
    pub(super) faults: Faults,
}

impl<R> CodeStore<R> {
    pub(super) fn used(&self) -> usize {
        self.cache.used()
    }

    pub(super) fn next(&self) -> usize {
        self.cache.next()
    }

    pub(super) fn install_tier1_version(
        &mut self,
        eip: u32,
        mut chain: Tier1Chain,
        position: usize,
        version: Version,
    ) {
        let previous = chain.versions[position - 1].entry.miss.unwrap();
        self.repoint(previous, version.entry.unit);
        chain.versions.truncate(position);
        chain.versions.push(version);
        self.tier1_chains.insert(eip, chain);
    }

    pub(super) fn tier1(&self, eip: u32) -> Option<Tier1Unit> {
        Some(self.tier1.get(&eip)?.entry)
    }

    pub(super) fn tier1_chain(&self, eip: u32) -> Option<&Tier1Chain> {
        self.tier1_chains.get(&eip)
    }

    pub(super) fn set_tier1(
        &mut self,
        eip: u32,
        entry: Tier1Unit,
        table_target: bool,
        chain: Option<Tier1Chain>,
    ) {
        let previous = self.tier1.insert(
            eip,
            Tier1Placement {
                entry,
                table_target,
            },
        );
        if let Some(chain) = chain {
            self.tier1_chains.insert(eip, chain);
        } else if previous.is_some() {
            self.tier1_chains.remove(&eip);
        }
    }

    pub(super) fn take_tier1_chain(&mut self, eip: u32) -> Option<Tier1Chain> {
        self.tier1_chains.remove(&eip)
    }

    pub(super) fn table_target(&self, eip: u32) -> bool {
        self.tier1.get(&eip).is_some_and(|unit| unit.table_target)
    }

    pub(super) fn resident(&self, eip: u32) -> Option<&ResidentSite<R>> {
        self.chains.get(&eip)?.resident.as_deref()
    }

    pub(super) fn resident_mut(&mut self, eip: u32) -> Option<&mut ResidentSite<R>> {
        self.chains.get_mut(&eip)?.resident.as_deref_mut()
    }

    pub(super) fn prepare_resident(&mut self, eip: u32, unit: Arc<Unit>, sample: u32) {
        self.chains.entry(eip).or_default().resident = Some(Box::new(ResidentSite {
            unit: Some(unit),
            sample: Box::new(sample.into()),
            cold_completions: Box::default(),
            versions: Vec::new(),
            fallback: 0,
            suspended: false,
            row_entry: None,
            row_feedback: None,
            row_router_words: 0,
            row_retry: None,
        }));
    }

    pub(super) fn publish_split(&mut self, eip: u32, index: u8, threshold: Option<u64>) {
        let Some(split) = self
            .chains
            .get_mut(&eip)
            .and_then(|chain| chain.resident.as_mut())
            .and_then(|site| site.versions.get_mut(usize::from(index)))
            .and_then(|version| version.split.as_mut())
        else {
            return;
        };
        if split.published == threshold {
            return;
        }
        split.published = threshold;
        let sensor = if let Some(threshold) = threshold {
            self.cache.patch(split.threshold, threshold as u32);
            self.cache
                .patch(split.threshold + 4, (threshold >> 32) as u32);
            split.active
        } else {
            b(12)
        };
        self.cache.patch(split.sensor, sensor);
    }

    pub(super) fn offers_split(&self, eip: u32, version: usize) -> bool {
        self.resident(eip)
            .filter(|site| !site.suspended)
            .and_then(|site| site.versions.get(version))
            .and_then(|version| version.split.as_ref())
            .is_some_and(|split| split.published.is_some())
    }

    pub(super) fn resident_fallback(&self, eip: u32) -> Option<usize> {
        self.tier1(eip).map(|entry| entry.unit).or_else(|| {
            self.resident(eip)
                .map(|site| site.fallback)
                .filter(|&at| at != 0)
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn place_resident(
        &mut self,
        eip: u32,
        index: usize,
        at: CpuState,
        key: Key,
        plain_sites: Vec<u32>,
        domain_guards: Vec<u32>,
        domain_saved_per_iter: u32,
        recovery: R,
        iteration_retired: u32,
        gate: Option<OfferEntry>,
        probation: Option<u32>,
        misses: u32,
        layout: ingress::Layout<'_>,
        unit: Arc<Unit>,
        row_target: Option<crate::translate::lower::resident::RowTarget>,
    ) -> bool {
        let Some(fallback) = self.ensure_resident_fallback(eip) else {
            return false;
        };
        if layout.row_price.is_some() {
            self.resident_mut(eip)
                .unwrap()
                .row_feedback
                .get_or_insert_with(Box::default);
        }
        let site = self.chains[&eip].resident.as_ref().unwrap();
        let slot = Box::<Slot>::default();
        slot.probation.set(probation.unwrap_or_default());
        for counter in &slot.env {
            counter.set(misses);
        }
        for counter in &slot.domain_sensor {
            counter.set(crate::policy::resident::DOMAIN_MISS_SAMPLE);
        }
        let base = self.cache.next();
        let mut entry = ingress::emit(
            (eip, index),
            &site.sample,
            &slot,
            gate,
            layout.spill_cells,
            self.version_stats.is_some(),
            ingress::Targets {
                at: base,
                proved_body: (row_target.is_none() && !key.uses_resident_context())
                    .then_some(layout.body),
                tier1: fallback,
                exit: self.stubs.exit,
                resident_exit: self.resident_exit,
                split: layout.split.is_some(),
                domain: layout.domain_miss.is_some(),
            },
        );
        let probe_word = (index == 0).then(|| entry.words[(entry.probe - base) / 4]);
        let words = layout.words;
        let row_sample = layout.row_sample.map(|(word, target)| {
            let point = entry.form + word * 4;
            let active = branch(point, entry.form + target * 4);
            (point, std::mem::replace(&mut words[word], active), active)
        });
        let literal = |words: &mut [u32], at: usize, pointer: u64| {
            words[at] = pointer as u32;
            words[at + 1] = (pointer >> 32) as u32;
        };
        if !layout.counters.is_empty() {
            let stats = self.version_stats.get_or_insert_with(FxHashMap::default);
            for &(head, kind, offset) in layout.counters {
                let stats = stats.entry(head).or_default();
                literal(words, offset, stats.spans[kind.index()].as_ptr() as u64);
            }
        }
        literal(words, layout.probation_slot, slot.countdown.as_ptr() as u64);
        let (sensors, counter, leave) = layout.probation_sensor;
        literal(words, *counter, slot.probation.as_ptr() as u64);
        words[*leave] = branch(entry.form + leave * 4, entry.probation);
        for (ordinal, &(_, offset)) in layout.environment.iter().enumerate() {
            literal(words, offset, slot.env[ordinal].as_ptr() as u64);
        }
        for &(ordinal, _, offset) in layout.domain_sensors {
            literal(
                words,
                offset,
                slot.domain_sensor[usize::from(ordinal)].as_ptr() as u64,
            );
        }
        if let Some(offset) = layout.domain_misses_slot {
            literal(words, offset, slot.domain_misses.as_ptr() as u64);
        }
        for (offset, target) in [
            (layout.exit, entry.tail),
            (layout.miss, entry.retry),
            (layout.env_miss, entry.environment),
            (layout.cold, fallback),
        ] {
            words[offset] = branch(entry.form + offset * 4, target);
        }
        if let (Some(offset), Some(target)) = (layout.domain_miss, entry.domain) {
            words[offset] = branch(entry.form + offset * 4, target);
        }
        if let Some(offset) = layout.domain_fallback {
            words[offset] = branch(entry.form + offset * 4, fallback);
        }
        let split = layout.split.zip(entry.split).map(|(split, target)| {
            let leave = entry.form + split.leave * 4;
            words[split.leave] = branch(leave, target);
            let active = words[split.sensor];
            words[split.sensor] = b(12);
            PlacedSplit {
                sensor: entry.form + split.sensor * 4,
                active,
                threshold: entry.form + split.threshold * 4,
                published: None,
            }
        });
        for &(offset, _) in layout.links {
            let word = offset / 4 + 3;
            words[word] = branch(entry.form + word * 4, self.stubs.dispatch);
        }
        let mut feedback_words = Vec::new();
        let row_failures = layout.row_price.map(|_| Box::<[Cell<u64>; 6]>::default());
        if let Some(failures) = &row_failures {
            let next = &site.row_feedback.as_ref().unwrap().next;
            let cold = entry.form + layout.cold * 4;
            for &(reason, offset) in layout.row_failures {
                let at = entry.form + (words.len() + feedback_words.len()) * 4;
                let from = entry.form + offset * 4;
                words[offset] = branch(from, at);
                feedback_words.extend(ingress::row_failure(
                    eip,
                    (at, cold, self.stubs.exit),
                    (&failures[reason as usize], next),
                    reason,
                ));
            }
        }
        entry.words.extend_from_slice(words);
        entry.words.extend(feedback_words);
        let placed = &mut entry.words[(entry.form - base) / 4..];
        let accounting = layout
            .accounting
            .iter()
            .map(|&word| {
                let call = placed[word];
                if probation.is_none() {
                    placed[word] = NOP;
                }
                (entry.form + word * 4, call)
            })
            .collect();
        let probation_sensors = sensors
            .iter()
            .map(|&word| {
                let active = placed[word];
                if probation.is_none() {
                    placed[word] = b((runtime::SENSOR_BYTES + 4) as i32);
                }
                (entry.form + word * 4, active)
            })
            .collect();
        if self.cache.append(&entry.words).is_none() {
            return false;
        }
        self.direct_points.extend(
            layout
                .transitions
                .iter()
                .map(|&(point, transition)| (entry.form + point, entry.form + transition)),
        );
        self.faults.append(entry.form, layout.sites);
        crate::jit_map::optimized(eip, format_args!("installed\t-\t-"));
        crate::jit_map::record(
            entry.form,
            words,
            eip,
            site.unit.as_ref().unwrap().trace.prepaid(0),
            layout.marks,
            "a64-resident",
        );
        let version = ResidentVersion {
            entry: entry.entry,
            proved: entry.proved,
            proved_cold: entry.proved_cold,
            probe: entry.probe,
            probe_word,
            next: entry.next,
            miss: entry.form + layout.miss * 4,
            miss_counter: entry.miss_counter,
            cold: entry.form + layout.cold * 4,
            retry: entry.retry,
            environment: layout
                .environment
                .iter()
                .map(|&(sensor, _)| entry.form + sensor)
                .collect(),
            domain_fallback: layout.domain_fallback.map(|offset| entry.form + offset * 4),
            domain_guards,
            domain_guard_words: layout.domain_guard_words,
            domain_saved_per_iter,
            at,
            key,
            plain_sites,
            unit,
            row_target,
            row_price: layout.row_price,
            row_sample,
            row_measuring: row_sample.is_some(),
            row_failures,
            enabled: true,
            recovery,
            slot,
            probation: probation.is_some(),
            probation_sensors,
            probation_counter: entry.form + *counter * 4,
            accounting,
            iteration_retired: u64::from(iteration_retired),
            split,
            offloaded: Cell::new((0, 0, 0)),
        };
        let versions = &mut self.resident_mut(eip).unwrap().versions;
        if index == versions.len() {
            versions.push(version);
        } else {
            versions[index] = version;
        }
        self.uses_permissions = true;
        self.faults.end.set(self.cache.next());
        true
    }

    fn ensure_resident_fallback(&mut self, eip: u32) -> Option<usize> {
        if let Some(tier1) = self.resident_fallback(eip) {
            return Some(tier1);
        }
        let at = self.cache.next();
        let words = ingress::version_miss(eip, at, self.stubs.dispatch);
        self.cache.append(&words)?;
        self.resident_mut(eip).unwrap().fallback = at;
        Some(at)
    }

    pub(super) fn publish_row_targets(
        &mut self,
        eip: u32,
        slot: (u8, u32),
        observed: &[u32],
    ) -> bool {
        let Some(tier1) = self.ensure_resident_fallback(eip) else {
            return false;
        };
        self.resident_mut(eip)
            .unwrap()
            .row_feedback
            .get_or_insert_with(Box::default);
        let counters = self.version_stats.as_ref().and_then(|stats| {
            stats.get(&eip).map(|stats| {
                [Counter::RowArrival, Counter::RowShort]
                    .map(|kind| stats.spans[kind.index()].as_ptr() as u64)
            })
        });
        let site = self.chains[&eip].resident.as_ref().unwrap();
        let mut cases: Vec<_> = site
            .versions
            .iter()
            .map(|v| {
                let target = v.row_target.expect("each case has the same slot");
                debug_assert_eq!(slot, (target.base, target.offset));
                (target.value, v.enabled.then_some(v.entry))
            })
            .collect();
        for &target in observed {
            if cases.iter().all(|&(installed, _)| installed != target) {
                cases.push((target, None));
            }
        }
        let first = site.versions.first();
        let row_counter = first
            .and_then(|version| version.row_price?.count_register)
            .filter(|&reg| {
                site.versions.iter().all(|version| {
                    version.row_price.and_then(|price| price.count_register) == Some(reg)
                })
            });
        let fallback = first.map_or(tier1, |version| version.cold);
        let at = self.cache.next();
        let (words, points) = ingress::row_entry(
            eip,
            slot,
            &cases,
            (at, fallback, self.stubs.exit),
            site.row_feedback.as_deref().unwrap(),
            row_counter,
            counters,
        );
        let Some(address) = self.cache.append(&words) else {
            return false;
        };
        let fallback = first.is_none().then_some(points);
        let site = self.resident_mut(eip).unwrap();
        site.row_router_words = words.len() as u32;
        site.row_entry = Some(RowEntry {
            slot,
            address,
            fallback,
        });
        true
    }

    pub(super) fn install_row_retry(&mut self, eip: u32) -> bool {
        let site = self.resident(eip).unwrap();
        if site.row_retry.is_some() || site.entry().is_none() {
            return false;
        }
        let at = self.cache.next();
        let fallback = self.resident_fallback(eip).unwrap();
        let (words, points) = ingress::row_retry(eip, at, fallback);
        let Some(entry) = self.cache.append(&words) else {
            return false;
        };
        self.resident_mut(eip).unwrap().row_retry = Some((entry, points));
        true
    }

    pub(super) fn start_link_sampling(&mut self, eip: u32, period: u32) {
        let site = self.chains[&eip].resident.as_ref().unwrap();
        site.sample.set(period);
        let first = &site.versions[0];
        self.cache.patch(first.probe, first.probe_word.unwrap());
    }

    pub(super) fn publish_resident(&mut self, eip: u32, period: Option<u32>) {
        let fallback = self.resident_fallback(eip).unwrap();
        let site = self
            .chains
            .get_mut(&eip)
            .unwrap()
            .resident
            .as_mut()
            .unwrap();
        let counters = self.version_stats.as_mut().map(|stats| {
            stats
                .entry(eip)
                .or_default()
                .resident_misses
                .each_ref()
                .map(|c| c.as_ptr() as u64)
        });
        for (index, version) in site.versions.iter().enumerate() {
            let next = site.versions[index + 1..]
                .iter()
                .find(|candidate| candidate.enabled && version.row_target.is_none());
            let tail = next.is_none();
            let next = next.map_or(fallback, |candidate| candidate.entry);
            self.cache.patch(version.next, branch(version.next, next));
            self.cache
                .patch(version.cold, branch(version.cold, fallback));
            if let Some(point) = version.domain_fallback {
                let general = site.versions.iter().enumerate().find_map(|(j, candidate)| {
                    (j != index
                        && candidate.enabled
                        && candidate.domain_guards.is_empty()
                        && candidate.row_target.is_none())
                    .then_some(candidate.entry)
                });
                self.cache
                    .patch(point, branch(point, general.unwrap_or(fallback)));
            }
            let target =
                if let Some(period) = period.filter(|_| tail && version.row_target.is_none()) {
                    version.slot.retry.set(period);
                    version.retry
                } else {
                    next
                };
            if let Some(counter) = &version.miss_counter {
                let address = counters.unwrap()[usize::from(tail)];
                self.cache.patch(counter.literal, address as u32);
                self.cache
                    .patch(counter.literal + 4, (address >> 32) as u32);
                self.cache.patch(counter.next, branch(counter.next, target));
                self.cache
                    .patch(version.miss, branch(version.miss, counter.entry));
            } else {
                self.cache.patch(version.miss, branch(version.miss, target));
            }
        }
    }

    pub(super) fn repoint_resident_fallback(&mut self, eip: u32, target: usize) {
        if let Some((_, fallback)) = self.offer(eip) {
            self.repoint(fallback, target);
        }
        let Some(site) = self
            .chains
            .get(&eip)
            .and_then(|chain| chain.resident.as_ref())
        else {
            return;
        };
        if let Some(points) = site.row_entry.as_ref().and_then(|row| row.fallback) {
            for point in points {
                self.cache.patch(point, branch(point, target));
            }
        }
        if let Some((_, [_, point])) = site.row_retry {
            self.cache.patch(point, branch(point, target));
        }
        for (index, version) in site.versions.iter().enumerate() {
            self.cache.patch(version.cold, branch(version.cold, target));
            if let Some(point) = version.domain_fallback {
                let has_general = site.versions.iter().enumerate().any(|(j, candidate)| {
                    j != index
                        && candidate.enabled
                        && candidate.domain_guards.is_empty()
                        && candidate.row_target.is_none()
                });
                if !has_general {
                    self.cache.patch(point, branch(point, target));
                }
            }
            if let Some(point) = version.proved_cold {
                self.cache.patch(point, branch(point, target));
            }
        }
        if let Some(tail) = site.versions.iter().rfind(|v| v.enabled) {
            self.cache.patch(tail.next, branch(tail.next, target));
            let point = tail
                .miss_counter
                .as_ref()
                .map_or(tail.miss, |counter| counter.next);
            if self.cache.word(point) != branch(point, tail.retry) {
                self.cache.patch(point, branch(point, target));
            }
        }
    }

    pub(super) fn offer(&self, eip: u32) -> Option<(usize, usize)> {
        self.chains.get(&eip)?.offer
    }

    pub(super) fn install_offer(&mut self, eip: u32, gate: OfferEntry) {
        let at = self.cache.next();
        let unit = self.tier1(eip).map(|entry| entry.unit);
        let (words, fallback) = ingress::offer_link(eip, at, self.stubs.exit, unit, gate);
        if self.cache.append(&words).is_some() {
            self.chains.entry(eip).or_default().offer = Some((at, fallback));
        }
    }

    pub(super) fn accounted(&self, eip: u32) -> (u64, u64) {
        self.resident(eip).unwrap().versions.iter().fold(
            (0, 0),
            |(entries, iterations), version| {
                let (offloaded_entries, offloaded_rows, _) = version.offloaded.get();
                (
                    entries + version.slot.entries.get() + offloaded_entries,
                    iterations
                        + version.iterations(version.slot.countdown[1].get())
                        + version.slot.rows.get()
                        + offloaded_rows,
                )
            },
        )
    }

    pub(super) fn contains_unit(&self, entry: u32, address: usize) -> bool {
        self.tier1(entry).is_some_and(|unit| unit.unit == address)
            || self.tier1_chain(entry).is_some_and(|chain| {
                chain.versions[1..]
                    .iter()
                    .any(|version| version.entry.unit == address)
            })
    }

    fn proved_resident(site: &ResidentSite<R>, proof: Option<&Key>) -> Option<usize> {
        let proof = proof?;
        site.versions
            .iter()
            .find_map(|v| v.proved.filter(|_| v.enabled && proof.proves(v.key)))
    }

    fn resident_target(
        &self,
        eip: u32,
        point: usize,
        back: bool,
        checked: Option<usize>,
    ) -> Option<usize> {
        checked.map(|entry| {
            let site = self.resident(eip).unwrap();
            Self::resident_link_target(site, self.point_facts.get(&point), back, entry)
        })
    }

    fn resident_link_target(
        site: &ResidentSite<R>,
        proof: Option<&Key>,
        back: bool,
        entry: usize,
    ) -> usize {
        if back && let Some((retry, _)) = site.row_retry {
            return retry;
        }
        Self::proved_resident(site, proof).unwrap_or(entry)
    }

    pub(super) fn patch_link(
        &mut self,
        eip: u32,
        point: usize,
        checked: usize,
        counting: bool,
        offer: bool,
    ) {
        let link = self.link(point, eip);
        let target = self.linked_target(point, eip, checked);
        if link.back {
            let fallback = point + runtime::BACK_UNIT;
            self.repoint(fallback, target);
            if !counting {
                self.repoint(point + runtime::BACK_COUNT, fallback);
            }
        }
        let checked = self.resident(eip).and_then(ResidentSite::entry);
        let resident = self.resident_target(eip, point, link.back, checked);
        let gate = self.offer(eip).filter(|_| offer).map(|(entry, _)| entry);
        Self::route_link(&mut self.cache, &link, resident, counting, gate, target);
    }

    fn route_link(
        cache: &mut CodeCache,
        link: &Linked,
        resident: Option<usize>,
        counting: bool,
        offer: Option<usize>,
        tier1: usize,
    ) {
        let point = link.point;
        let target = match (resident, link.back, link.direct, offer, counting) {
            (Some(entry), true, _, _, _) => entry,
            (Some(entry), false, Some(transition), _, _) => {
                cache.patch(transition + 4, branch(transition + 4, entry));
                transition
            }
            (_, _, _, Some(gate), _) => gate,
            (_, true, _, _, true) => point + runtime::BACK_COUNT,
            _ => tier1,
        };
        cache.patch(point, link.branch(target));
    }

    pub(super) fn repoint_tier1(&mut self, eip: u32, checked: usize, counting: bool, offer: bool) {
        let slot = &mut self.table[runtime::table_slot(eip)];
        if slot[0] == u64::from(eip) {
            slot[1] = checked as u64;
        }
        self.repoint_resident_fallback(eip, checked);
        let resident = self.resident(eip).and_then(ResidentSite::entry);
        let gate = self.offer(eip).filter(|_| offer).map(|(entry, _)| entry);
        let mut next = self.links.get(&eip).copied().unwrap_or(u32::MAX);
        while let Some(link) = self.linked.get(next as usize).copied() {
            next = link.next;
            let target = self.linked_target(link.point, eip, checked);
            if link.back {
                self.repoint(link.point + runtime::BACK_UNIT, target);
            }
            if resident.is_some() && (link.back || link.direct.is_some()) {
                continue;
            }
            Self::route_link(&mut self.cache, &link, resident, counting, gate, target);
        }
    }

    pub(super) fn repoint_resident_edges(
        &mut self,
        eip: u32,
        resident: Option<usize>,
        counting: bool,
        offer: bool,
    ) {
        if let Some((_, [point, _])) = self.resident(eip).and_then(|site| site.row_retry)
            && let Some(target) = resident
        {
            self.repoint(point, target);
        }
        let gate = self.offer(eip);
        let offer = gate.filter(|_| offer).map(|(entry, _)| entry);
        self.repoint_internal_edges(eip, resident, offer);
        let mut next = self.links.get(&eip).copied().unwrap_or(u32::MAX);
        let Some(tier1) = self
            .resident_fallback(eip)
            .or_else(|| gate.map(|(_, fallback)| fallback))
        else {
            return;
        };
        while let Some(edge) = self.linked.get(next as usize).copied() {
            next = edge.next;
            if edge.back || edge.direct.is_some() || gate.is_some() {
                let target = self.linked_target(edge.point, eip, tier1);
                let resident = self.resident_target(eip, edge.point, edge.back, resident);
                Self::route_link(&mut self.cache, &edge, resident, counting, offer, target);
            }
        }
    }

    fn repoint_internal_edges(&mut self, eip: u32, resident: Option<usize>, offer: Option<usize>) {
        let site = self
            .chains
            .get(&eip)
            .and_then(|chain| chain.resident.as_deref());
        for edge in self.internal_edges.get_mut(&eip).into_iter().flatten() {
            Self::route_internal(
                &mut self.cache,
                edge,
                site.zip(resident),
                self.point_facts.get(&edge.point),
                offer,
            );
        }
    }

    fn route_internal(
        cache: &mut CodeCache,
        edge: &InternalEdge,
        resident: Option<(&ResidentSite<R>, usize)>,
        facts: Option<&Key>,
        offer: Option<usize>,
    ) {
        let target = if let Some((site, entry)) = resident.filter(|(site, _)| {
            edge.back
                || !site
                    .unit
                    .as_ref()
                    .unwrap()
                    .trace
                    .insts
                    .iter()
                    .any(|inst| inst.pc == edge.source_pc)
        }) {
            let entry = Self::resident_link_target(site, facts, edge.back, entry);
            cache.patch(edge.link, branch(edge.link, entry));
            edge.transition
        } else if let Some(((sensor, _), offer)) = edge.sensor.zip(offer) {
            let words = ingress::internal_offer(sensor, edge.transition, edge.target);
            for (i, word) in words.into_iter().enumerate() {
                cache.patch(sensor + i * 4, word);
            }
            cache.patch(edge.link, branch(edge.link, offer));
            sensor
        } else {
            edge.target
        };
        cache.patch(edge.point, branch(edge.point, target));
    }

    pub(super) fn publish_switch(&mut self, eip: u32, target: usize) {
        for &(_, point, _) in self.rows.get(&eip).into_iter().flatten() {
            self.cache.patch(point, target as u32);
            self.cache.patch(point + 4, ((target as u64) >> 32) as u32);
        }
    }

    pub(super) fn publish_indirect(&mut self, eip: u32, target: usize) {
        self.table[runtime::table_slot(eip)] = [u64::from(eip), target as u64];
    }

    pub(super) fn sync(&mut self) {
        self.cache.sync();
        self.faults.end.set(self.cache.next());
    }

    pub(super) fn bind_counter(&mut self, point: usize, address: u64) {
        self.cache.patch(point, address as u32);
        self.cache.patch(point + 4, (address >> 32) as u32);
    }

    pub(super) fn stop_counting(&mut self, eip: u32) {
        let mut next = self.links.get(&eip).copied().unwrap_or(u32::MAX);
        while let Some(link) = self.linked.get(next as usize).copied() {
            next = link.next;
            if link.back {
                self.repoint(
                    link.point + runtime::BACK_COUNT,
                    link.point + runtime::BACK_UNIT,
                );
            }
        }
        for edge in self.internal_edges.get(&eip).into_iter().flatten() {
            if let Some((sensor, _)) = edge.sensor {
                self.cache.patch(sensor, branch(sensor, edge.target));
            }
        }
    }

    pub(super) fn restore_internal_counting(&mut self, eip: u32) {
        for edge in self.internal_edges.get(&eip).into_iter().flatten() {
            if let Some((sensor, words)) = edge.sensor
                && self.contains_unit(edge.source, edge.unit)
            {
                for (i, word) in words.into_iter().enumerate() {
                    self.cache.patch(sensor + i * 4, word);
                }
                self.cache.patch(edge.point, branch(edge.point, sensor));
            }
        }
    }

    pub(super) fn settle_sample(&mut self, eip: u32) {
        let version = &self.chains[&eip].resident.as_ref().unwrap().versions[0];
        self.cache
            .patch(version.probe, branch(version.probe, version.entry));
    }

    pub(super) fn specialize_link(&mut self, eip: u32, point: usize, version: usize) {
        let patch = if let Some(edge) = self
            .internal_edges
            .get(&eip)
            .and_then(|edges| edges.iter().find(|edge| edge.point == point))
        {
            edge.link
        } else {
            let mut next = self.links[&eip];
            while self.linked[next as usize].point != point {
                next = self.linked[next as usize].next;
            }
            self.linked[next as usize]
                .direct
                .map_or(point, |transition| transition + 4)
        };
        let entry = self.resident(eip).unwrap().versions[version].entry;
        self.repoint(patch, entry);
    }

    pub(super) fn environment_failed(&mut self, eip: u32, version: usize, fact: usize) -> u32 {
        let version = &self.chains[&eip].resident.as_ref().unwrap().versions[version];
        self.cache
            .patch(version.environment[fact], b(runtime::SENSOR_BYTES as i32));
        version.plain_sites[fact]
    }

    pub(super) fn domain_guard_totals(
        &self,
        eip: u32,
        version: usize,
        ordinal: usize,
        history: &[(u64, u64)],
    ) -> Option<(u32, u64, u64, u64, u32, u32)> {
        let site = self.chains.get(&eip)?.resident.as_ref()?;
        let current = site.versions.get(version)?;
        let &guard_id = current.domain_guards.get(ordinal)?;
        let mut total_entries = 0u64;
        let mut total_iters = 0u64;
        let mut total_misses = 0u64;
        for (j, v) in site.versions.iter().enumerate() {
            if !v.domain_guards.contains(&guard_id) {
                continue;
            }
            let (hist_entries, hist_iters) = history.get(j).copied().unwrap_or((0, 0));
            total_entries = total_entries.saturating_add(hist_entries.max(v.slot.entries.get()));
            total_iters =
                total_iters.saturating_add(hist_iters.max(v.iterations(v.slot.countdown[1].get())));
            total_misses = total_misses.saturating_add(v.slot.domain_misses.get());
        }
        Some((
            guard_id,
            total_entries,
            total_iters,
            total_misses,
            current.domain_guard_words,
            current.domain_saved_per_iter,
        ))
    }

    pub(super) fn reset_domain_sensor(
        &self,
        eip: u32,
        version: usize,
        ordinal: usize,
        period: u32,
    ) {
        if let Some(version) = self
            .chains
            .get(&eip)
            .and_then(|chain| chain.resident.as_ref())
            .and_then(|site| site.versions.get(version))
            && let Some(cell) = version.slot.domain_sensor.get(ordinal)
        {
            cell.set(period);
        }
    }

    pub(super) fn measure_cold(&mut self, eip: u32, period: Option<u32>) {
        let site = self
            .chains
            .get_mut(&eip)
            .unwrap()
            .resident
            .as_mut()
            .unwrap();
        if let Some(period) = period {
            site.cold_completions.set(period);
        }
        let enabled = period.is_some();
        let counter = site.cold_completions.as_ptr() as u64;
        for version in &mut site.versions {
            if !version.probation {
                if enabled && version.row_price.is_none() {
                    version.slot.entries.set(0);
                    version.slot.countdown[1].set(0);
                }
                version.account(&mut self.cache, enabled);
                if enabled {
                    self.cache.patch(version.probation_counter, counter as u32);
                    self.cache
                        .patch(version.probation_counter + 4, (counter >> 32) as u32);
                    for &(point, active) in &version.probation_sensors {
                        self.cache.patch(point, active);
                    }
                }
            }
        }
        self.sync();
    }

    pub(super) fn measure_rows(&mut self, eip: u32, index: usize, enabled: bool) {
        let site = self
            .chains
            .get_mut(&eip)
            .unwrap()
            .resident
            .as_mut()
            .unwrap();
        site.versions[index].account(&mut self.cache, enabled);
    }

    pub(super) fn finish_probation(&mut self, eip: u32, index: usize) -> (u64, u64) {
        let version = &mut self
            .chains
            .get_mut(&eip)
            .unwrap()
            .resident
            .as_mut()
            .unwrap()
            .versions[index];
        version.probation = false;
        version.account(&mut self.cache, false);
        (version.slot.entries.get(), version.slot.countdown[1].get())
    }

    pub(super) fn forget_dynamic(&mut self) -> u64 {
        self.table.fill(EMPTY);
        self.shadow.top()
    }

    pub(super) fn clear(&mut self) {
        self.cache.truncate(self.stub_len);
        self.tier1.clear();
        self.tier1_chains.clear();
        self.chains.clear();
        self.pages_by_unit.clear();
        self.pages.clear();
        self.rows.clear();
        self.table_sources.clear();
        self.uses_permissions = false;
        self.links.clear();
        self.linked.clear();
        self.back_points.clear();
        self.direct_points.clear();
        self.point_facts.clear();
        self.internal_edges.clear();
        self.tier1_words.clear();
        self.placed.clear();
        self.helpers.clear();
        self.generation += 1;
        self.faults.sites.clear();
        self.faults.end.set(self.cache.next());
    }

    pub(super) fn link(&mut self, point: usize, eip: u32) -> Linked {
        let link = Linked {
            point,
            original: self.cache.word(point),
            next: self
                .links
                .insert(eip, self.linked.len() as u32)
                .unwrap_or(u32::MAX),
            back: self.back_points.contains(&point),
            direct: self.direct_points.get(&point).copied(),
        };
        self.linked.push(link);
        link
    }

    pub(super) fn repoint(&mut self, point: usize, target: usize) {
        self.cache
            .patch(point, b((target as i64 - point as i64) as i32));
    }

    pub(super) fn linked_target(&self, point: usize, eip: u32, checked: usize) -> usize {
        let Some(proved) = self.point_facts.get(&point) else {
            return checked;
        };
        if self.tier1(eip).is_some_and(|entry| entry.unit == checked)
            && let Some(chain) = self.tier1_chain(eip)
            && let Some(version) = chain
                .versions
                .iter()
                .find(|version| proved.proves(version.key))
        {
            return version.entry.body;
        }
        checked
    }

    pub(super) fn counter(&mut self, eip: u32, position: usize) -> usize {
        self.version_stats.as_mut().map_or(0, |stats| {
            stats.entry(eip).or_default().arrivals[position].as_ptr() as usize
        })
    }

    pub(super) fn invalidate_unit(&mut self, eip: u32) -> bool {
        if let Some(pages) = self.pages_by_unit.remove(&eip) {
            for page in pages {
                if let std::collections::hash_map::Entry::Occupied(mut entry) =
                    self.pages.entry(page)
                {
                    let units = entry.get_mut();
                    if let Some(index) = units.iter().position(|&(unit, _)| unit == eip) {
                        units.swap_remove(index);
                    }
                    if units.is_empty() {
                        entry.remove();
                    }
                }
            }
        }
        self.repoint_internal_edges(eip, None, None);
        let dropped = self.tier1.remove(&eip).is_some() | self.chains.remove(&eip).is_some();
        self.tier1_chains.remove(&eip);
        if self.table_sources.remove(&eip) {
            self.rows.retain(|_, slots| {
                slots.retain(|&(source, _, _)| source != eip);
                !slots.is_empty()
            });
        }
        for &(_, point, stub) in self.rows.get(&eip).into_iter().flatten() {
            self.cache.patch(point, stub as u32);
            self.cache.patch(point + 4, ((stub as u64) >> 32) as u32);
        }
        let mut next = self.links.remove(&eip).unwrap_or(u32::MAX);
        while let Some(link) = self.linked.get(next as usize) {
            self.cache.patch(link.point, link.original);
            next = link.next;
        }
        self.tier1_words.remove(&eip);
        dropped
    }

    pub(super) fn watch_pages(
        &mut self,
        memory: &mut Memory,
        eip: u32,
        spans: &[(u32, u32)],
        decodes: &mut Decodes,
    ) {
        // The front end returns sorted dependency spans, including failed
        // fetches and the bytes used to admit a redirect.
        let mut merged: Option<(u32, u64)> = None;
        for (start, len) in decodes.watch_spans(spans) {
            let stop = u64::from(start) + u64::from(len);
            match &mut merged {
                Some((_, end)) if u64::from(start) <= *end => *end = (*end).max(stop),
                _ => {
                    if let Some((from, to)) = merged.replace((start, stop)) {
                        memory.watch_code(from, (to - u64::from(from)) as usize);
                    }
                }
            }
        }
        if let Some((from, to)) = merged {
            memory.watch_code(from, (to - u64::from(from)) as usize);
        }
        let pages = self.pages_by_unit.entry(eip).or_default();
        for &(start, len) in spans.iter().filter(|&&(_, len)| len != 0) {
            let end = (u64::from(start) + u64::from(len) - 1).min(u64::from(u32::MAX));
            let mut page = start & !0xfff;
            let last_page = end as u32 & !0xfff;
            loop {
                let mask = page_line_mask(page, start, u64::from(len));
                let units = self.pages.entry(page).or_default();
                match pages.binary_search(&page) {
                    Ok(_) => {
                        if let Some(entry) = units.iter_mut().find(|(unit, _)| *unit == eip) {
                            entry.1 |= mask;
                        } else {
                            units.push((eip, mask));
                        }
                    }
                    Err(index) => {
                        pages.insert(index, page);
                        units.push((eip, mask));
                    }
                }
                if page == last_page {
                    break;
                }
                page = page.wrapping_add(4096);
            }
        }
    }

    /// Appends a unit for `eip` whose `sites` are at byte offsets.
    pub(super) fn place_worker_form(
        &mut self,
        words: &mut [u32],
        exit: usize,
        sites: &[(usize, Site)],
    ) -> Option<WorkerCode> {
        words[exit] = branch(self.cache.next() + exit * 4, self.stubs.opt_exit);
        self.place_worker(words, sites)
    }

    pub(super) fn place_worker(
        &mut self,
        words: &[u32],
        sites: &[(usize, Site)],
    ) -> Option<WorkerCode> {
        let entry = self.cache.append(words)?;
        let mut sites: Vec<_> = sites
            .iter()
            .map(|&(offset, site)| (entry + offset, site))
            .collect();
        sites.sort_unstable_by_key(|&(pc, _)| pc);
        Some(WorkerCode {
            entry,
            end: entry + words.len() * 4,
            sites: sites.into(),
        })
    }

    pub(super) fn place_tier1(
        &mut self,
        eip: u32,
        work: &crate::translate::lower::Tier1Placement<'_>,
        unit: &Unit,
        key: Key,
        record_cost: bool,
        mut counter: impl FnMut(u32) -> (u64, u32),
    ) -> Option<Tier1Unit> {
        let entry = self.install_unit(
            eip,
            work.words,
            work.sites,
            work.retired,
            work.marks,
            work.version,
        )?;
        let at = entry.unit;
        if !unit.tables.is_empty() {
            self.table_sources.insert(eip);
        }
        for (table, &(offset, stub)) in unit.tables.iter().zip(work.table_positions) {
            for (index, &target) in table.targets.iter().enumerate() {
                let slot = (eip, at + offset + index * 8, at + stub);
                self.rows.entry(target).or_default().push(slot);
            }
        }
        let proofs = if key.mask() == 0 {
            key
        } else {
            key.project(unit.preserved_facts())
        };
        let mut has_backedge = false;
        for link in work.links {
            let point = at + link.offset;
            if proofs.mask() != 0 {
                self.point_facts.insert(point, proofs);
            }
            self.points.push((point, link.eip, link.main));
            if let Some(transition) = link.direct {
                self.direct_points.insert(point, at + transition);
            }
            if link.back {
                has_backedge = true;
                self.back_points.insert(point);
                let (address, count) = counter(link.eip);
                self.bind_counter(point + runtime::BACK_COUNTER, address);
                if count == u32::MAX {
                    self.cache
                        .patch(point + runtime::BACK_COUNT, b(runtime::SENSOR_BYTES as i32));
                }
            }
        }
        for link in work.internal_links {
            let edge = InternalEdge {
                point: at + link.point,
                target: at + link.target,
                transition: at + link.transition,
                link: at + link.link,
                source: eip,
                unit: at,
                source_pc: link.source_pc,
                back: link.counter.is_some(),
                sensor: link.sensor.map(|offset| {
                    let words = &work.words[offset / 4..];
                    (at + offset, std::array::from_fn(|i| words[i]))
                }),
            };
            if proofs.mask() != 0 {
                self.point_facts.insert(edge.point, proofs);
            }
            let value = if let Some(address_slot) = link.counter {
                has_backedge = true;
                let (address, value) = counter(link.eip);
                self.bind_counter(at + address_slot, address);
                value
            } else {
                u32::MAX
            };
            if value == u32::MAX
                && let Some((sensor, _)) = edge.sensor
            {
                self.cache.patch(sensor, branch(sensor, edge.target));
            }
            let chain = self.chains.get(&link.eip);
            let resident = chain.and_then(|chain| chain.resident.as_deref());
            if resident.is_some() || value == u32::MAX {
                let offer = chain
                    .filter(|chain| {
                        chain
                            .resident
                            .as_ref()
                            .is_some_and(|site| site.versions.is_empty())
                    })
                    .and_then(|chain| chain.offer)
                    .map(|(entry, _)| entry);
                Self::route_internal(
                    &mut self.cache,
                    &edge,
                    resident.and_then(|site| Some((site, site.entry()?))),
                    self.point_facts.get(&edge.point),
                    offer,
                );
            }
            self.internal_edges.entry(link.eip).or_default().push(edge);
        }
        if record_cost {
            if has_backedge {
                self.tier1_words.insert(
                    eip,
                    tier1_words(work.marks, work.links, work.words.len(), &unit.trace),
                );
            } else {
                self.tier1_words.remove(&eip);
            }
        }
        self.uses_permissions |= work.uses_permissions;
        Some(entry)
    }

    pub(super) fn install_unit(
        &mut self,
        eip: u32,
        words: &[u32],
        sites: &[(usize, Site)],
        retired: u32,
        marks: &[crate::jit_map::Mark],
        layout: VersionLayout,
    ) -> Option<Tier1Unit> {
        let unit = self.cache.append(words)?;
        self.faults.append(unit, sites);
        let entry = Tier1Unit {
            unit,
            body: unit + layout.body,
            miss: layout.miss.map(|offset| unit + offset),
            retired,
        };
        crate::jit_map::record(unit, words, eip, retired, marks, "a64");
        self.placed.push((unit, eip));
        Some(entry)
    }
}

/// Counts the Tier-1 body words for each guest instruction. A taken
/// back edge adds its canonical publication and patch word, excluding
/// the unlinked dispatch and discovery counter.
pub(super) fn tier1_words(
    marks: &[crate::jit_map::Mark],
    links: &[Link],
    code_words: usize,
    trace: &Trace,
) -> FxHashMap<u32, u32> {
    use crate::jit_map::Part;

    let mut words: FxHashMap<_, _> = trace.insts.iter().map(|inst| (inst.pc, 0)).collect();
    let cold = marks
        .iter()
        .position(|mark| mark.part == Part::Cold)
        .unwrap_or(marks.len());
    for (i, mark) in marks[..cold].iter().enumerate() {
        let end = marks
            .get(i + 1)
            .map_or(code_words * 4, |mark| mark.offset as usize);
        *words.entry(mark.eip).or_default() += ((end - mark.offset as usize) / 4) as u32;
    }
    for link in links.iter().filter(|link| link.back) {
        let at = marks.partition_point(|mark| mark.offset as usize <= link.offset);
        if at > cold {
            let start = marks[..at]
                .iter()
                .rfind(|mark| mark.part == Part::Cold)
                .unwrap();
            *words.entry(start.eip).or_default() +=
                ((link.offset + 4 - start.offset as usize) / 4) as u32;
        } else if let Some(mark) = marks.get(at - 1) {
            *words.entry(mark.eip).or_default() -= (runtime::BACK_END / 4) as u32;
        }
    }
    words
}

// Workers share immutable code and fault sites while a job is active.
#[derive(Clone)]
pub(super) struct WorkerCode {
    pub entry: usize,
    pub end: usize,
    pub sites: Arc<[(usize, Site)]>,
}

pub(super) fn prepare_execution_thread() {
    crate::cache::execution_thread();
}

/// Observed results of the existing interpreter event path.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum InterpreterCause {
    HostFault,
    DebugFlags,
    System,
    Fetch,
    InvalidEncoding,
    OperandFault,
    Trap(u8),
}

/// What the signal handler may read while translated code runs.
pub(super) struct Faults {
    /// Executable range of translated code.
    pub(super) start: usize,
    pub(super) end: Cell<usize>,
    /// Cache-relative offset of each site, ascending. The owner cache fits in u32.
    pub(super) sites: Vec<(u32, Site)>,
    pub(super) stub: usize,
    pub(super) capture: FaultCapture,
    /// The shadow stack's guard pages.
    pub(super) guard: (usize, usize),
    /// Where an overflow resumes, and the runtime field it reads the
    /// faulting PC from.
    pub(super) reset: usize,
    pub(super) resume: *mut u64,
}

#[derive(Default)]
pub(super) struct FaultCapture {
    /// The site that faulted, written in signal context.
    pub(super) pc: Cell<usize>,
    /// v8-v23 captured before the fault stub restores host SIMD.
    pub(super) registers: Cell<runtime::FaultRegisters>,
}

pub(super) fn site_at(sites: &[(usize, Site)], pc: usize) -> Option<Site> {
    let index = sites.binary_search_by_key(&pc, |&(at, _)| at).ok()?;
    Some(sites[index].1)
}

impl Faults {
    fn append(&mut self, base: usize, sites: &[(usize, Site)]) {
        let from = self.sites.len().saturating_sub(1);
        let base = base - self.start;
        self.sites.extend(
            sites
                .iter()
                .map(|&(offset, site)| ((base + offset) as u32, site)),
        );
        if !self.sites[from..].is_sorted_by_key(|&(at, _)| at) {
            self.sites.sort_unstable_by_key(|&(at, _)| at);
        }
    }

    pub(super) fn site(&self, pc: usize) -> Option<Site> {
        let offset = u32::try_from(pc.checked_sub(self.start)?).ok()?;
        let index = self
            .sites
            .binary_search_by_key(&offset, |&(at, _)| at)
            .ok()?;
        Some(self.sites[index].1)
    }
}

#[derive(Clone, Copy)]
pub(super) enum Active {
    None,
    Owner(*const Faults),
    Worker(*const WorkerFaults<'static>),
}

thread_local! {
    /// The `Faults` of the code this thread is running, or null.
    pub(super) static ACTIVE: Cell<Active> = const { Cell::new(Active::None) };
}

impl FaultCapture {
    pub(super) fn record(
        &self,
        site: Site,
        pc: usize,
        stub: usize,
        snapshot: &adv_cpu_core::execution_abi::HostContextSnapshot,
    ) -> usize {
        self.registers.set(runtime::FaultRegisters {
            simd: std::array::from_fn(|n| snapshot.simd[8 + n]),
            temps: [snapshot.gpr[7], snapshot.gpr[8]],
        });
        self.pc.set(pc);
        if site.resume != 0 {
            // The unit publishes its cached x87 state, then faults.
            pc.wrapping_add_signed(site.resume as isize)
        } else {
            stub
        }
    }

    pub(super) fn recover(&self, cpu: &mut CpuState, site: Site) {
        let registers = self.registers.get();
        recover(cpu, site, registers.temps);
        for (n, bytes) in registers.simd.into_iter().enumerate() {
            if site.simd_dirty & (1 << n) == 0 {
                continue;
            }
            if n < 8 {
                let value = u64::from_le_bytes(bytes[..8].try_into().unwrap());
                cpu.mmx[n] = value;
                cpu.x87.regs[n] = adv_cpu_core::X87Register::from_parts(value, 0xffff);
            } else {
                cpu.xmm[n - 8] = bytes;
            }
        }
    }
}

/// Runs in signal context: no allocation, no locks.
pub(super) unsafe fn on_fault(
    pc: usize,
    address: usize,
    snapshot: &adv_cpu_core::execution_abi::HostContextSnapshot,
) -> usize {
    ACTIVE
        .try_with(|slot| {
            let faults = match slot.get() {
                Active::None => return 0,
                Active::Worker(faults) => {
                    // SAFETY: the worker keeps its private capture and unit
                    // side table alive while its entry is active on this thread.
                    let faults = unsafe { &*faults };
                    return faults.record(pc, snapshot);
                }
                Active::Owner(faults) => faults,
            };
            // SAFETY: `run` keeps the pointee alive and unmodified, apart
            // from the fault capture cells, until it clears the slot.
            let faults = unsafe { &*faults };
            if !(faults.start..faults.end.get()).contains(&pc) {
                return 0;
            }
            if (faults.guard.0..faults.guard.1).contains(&address) {
                // A shadow push overflowed: empty the stack and retry it.
                // SAFETY: the runtime outlives the run, and translated code
                // is stopped here.
                unsafe { faults.resume.write(pc as u64) };
                return faults.reset;
            }
            let Some(site) = faults.site(pc) else {
                return 0;
            };
            faults.capture.record(site, pc, faults.stub, snapshot)
        })
        .unwrap_or(0)
}

pub(super) struct WorkerFaults<'a> {
    pub(super) code: &'a WorkerCode,
    pub(super) stub: usize,
    pub(super) capture: &'a FaultCapture,
}

impl WorkerFaults<'_> {
    pub(super) fn record(
        &self,
        pc: usize,
        snapshot: &adv_cpu_core::execution_abi::HostContextSnapshot,
    ) -> usize {
        if !(self.code.entry..self.code.end).contains(&pc) {
            return 0;
        }
        site_at(&self.code.sites, pc)
            .map_or(0, |site| self.capture.record(site, pc, self.stub, snapshot))
    }
}

/// Rebuilds the x86 flags at a fault site, where the exit stub assumed
/// subtraction sense and canonical PF/AF sources.
pub(super) fn recover(cpu: &mut CpuState, site: Site, temps: [u64; 2]) {
    cpu.eip = site.eip;
    let regs = cpu.regs;
    let value = |operand| match operand {
        Operand::Guest(index) => regs[usize::from(index)],
        Operand::Temp(index) => temps[usize::from(index)] as u32,
        Operand::Imm(value) => value,
    };
    if let Some(result) = site.nz {
        let result = value(result);
        cpu.eflags = cpu.eflags & !0xc0 | u32::from(result == 0) << 6 | (result >> 24 & 0x80);
    }
    if let Some(overflow) = site.overflow.evaluate(value) {
        cpu.eflags = cpu.eflags & !0x800 | u32::from(overflow) << 11;
    }
    match site.carry.evaluate(value) {
        Some(cf) => cpu.eflags = cpu.eflags & !1 | u32::from(cf),
        None if site.sense == Sense::Addition => cpu.eflags ^= 1,
        None => {}
    }
    if let Some((parity, adjust)) = site.recipe.evaluate(value) {
        let pf = u32::from((parity as u8).count_ones().is_multiple_of(2));
        cpu.eflags = cpu.eflags & !0x04 | pf << 2;
        if let Some(adjust) = adjust {
            cpu.eflags = cpu.eflags & !0x10 | adjust & 0x10;
        }
    }
}
