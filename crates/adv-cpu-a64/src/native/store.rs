//! Tier-1 code placement, watches, and link storage.

use super::*;

pub(super) struct CodeStore {
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
    pub(super) units: FxHashMap<u32, Entry>,
    /// Unit entries by the guest pages their code came from.
    pub(super) pages: FxHashMap<u32, Vec<u32>>,
    /// Patched link points, with the words they held, by the unit they
    /// branch to: the head of a list in `linked`.
    pub(super) links: FxHashMap<u32, u32>,
    /// Link point, the word it held, and the next point to the same unit.
    pub(super) linked: Vec<Linked>,
    /// Link points of the units the current translation placed.
    pub(super) points: Vec<(usize, u32, bool)>,
    pub(super) back_points: FxHashSet<usize>,
    pub(super) direct_points: FxHashMap<usize, usize>,
    pub(super) internal_edges: FxHashMap<u32, Vec<InternalEdge>>,
    /// Emitted instruction lengths by entry and guest EIP for probation.
    pub(super) tier1_words: FxHashMap<u32, FxHashMap<u32, u32>>,
    pub(super) uses_permissions: bool,
    /// Each placed unit's host address and entry, ascending.
    pub(super) placed: Vec<(usize, u32)>,
    /// Buffers of the last translation.
    pub(super) work: translate::Workspace,
    /// What the units' `interpret` calls run, until the cache clears,
    /// and how often each ran.
    pub(super) helpers: Vec<translate::Helper>,
    pub(super) helper_counts: Vec<u64>,
    /// Helper runs by EIP from before the last clear.
    pub(super) helper_eips: FxHashMap<u32, u64>,
    /// Counts cache clears, which invalidate link points.
    pub(super) generation: u64,
    /// The indirect table the runtime points at.
    pub(super) table: Box<[[u64; 2]]>,
    pub(super) shadow: ShadowStack,
    pub(super) faults: Faults,
}

impl Native {
    /// The unit at `eip`, translated if needed; a nonzero `point` is
    /// patched into a branch to it. The caller keeps stops unlinked: the
    /// owner must see them.
    pub(super) fn link(
        &mut self,
        memory: &mut Memory,
        point: usize,
        eip: u32,
        entry: Option<Entry>,
    ) -> Result<usize, String> {
        let generation = self.code.generation;
        let entry = match entry {
            Some(entry) => entry,
            None => self.translate(memory, eip)?,
        };
        // Translating may have cleared the cache and the link point with it.
        if point != 0 && self.code.generation == generation {
            self.patch(point, eip, entry.unit);
            if self.code.back_points.contains(&point) {
                // Reload the back edge's identity after the dispatch stub.
                return Ok(point - 4);
            }
            if self.code.direct_points.contains_key(&point)
                && self
                    .heads
                    .get(&eip)
                    .is_some_and(|head| head.trampoline != 0 && !head.unprofitable)
            {
                return Ok(point);
            }
        }
        Ok(entry.unit)
    }

    /// Makes the link point at `point` branch to the unit at `eip`.
    pub(super) fn patch(&mut self, point: usize, eip: u32, unit: usize) {
        let original = self.code.cache.word(point);
        let back = self.code.back_points.contains(&point);
        let next = self.code.links.insert(eip, self.code.linked.len() as u32);
        let link = Linked {
            point,
            original,
            next: next.unwrap_or(u32::MAX),
            back,
            direct: self.code.direct_points.get(&point).copied(),
            sample: None,
        };
        if back {
            let fallback = point + runtime::BACK_UNIT;
            self.code
                .cache
                .patch(fallback, head::branch(fallback, unit));
        }
        Self::route_link(&mut self.code.cache, &link, self.heads.get(&eip), unit);
        self.code.linked.push(link);
    }

    pub(super) fn enters_resident(link: &Linked, head: &head::Head) -> bool {
        head.trampoline != 0 && !head.unprofitable && (link.back || link.direct.is_some())
    }

    pub(super) fn route_link(
        cache: &mut CodeCache,
        link: &Linked,
        head: Option<&head::Head>,
        unit: usize,
    ) {
        let point = link.point;
        let target = if let Some(head) = head.filter(|head| Self::enters_resident(link, head)) {
            if link.back {
                head.trampoline
            } else if let Some(transition) = link.direct {
                cache.patch(
                    transition + 4,
                    head::branch(transition + 4, head.trampoline),
                );
                transition
            } else {
                unit
            }
        } else if link.back && head.is_none() {
            point + runtime::BACK_COUNT
        } else {
            unit
        };
        cache.patch(point, link.branch(target));
    }

    /// Tier-1 replacement keeps old continuations and all their watches.
    pub(super) fn replace_unit(&mut self, memory: &mut Memory, eip: u32) {
        let Some(new) = self.emit(memory, eip) else {
            return;
        };
        let slot = &mut self.code.table[runtime::table_slot(eip)];
        if slot[0] == u64::from(eip) {
            slot[1] = new.unit as u64;
        }
        self.resolve_tables(memory, eip, new.unit);
        let resident = self.heads.get(&eip);
        let mut next = self.code.links.get(&eip).copied().unwrap_or(u32::MAX);
        while let Some(link) = self.code.linked.get(next as usize) {
            next = link.next;
            if link.back {
                let fallback = link.point + runtime::BACK_UNIT;
                self.code
                    .cache
                    .patch(fallback, head::branch(fallback, new.unit));
            }
            if resident.is_some_and(|head| Self::enters_resident(link, head)) {
                continue;
            }
            Self::route_link(&mut self.code.cache, link, resident, new.unit);
        }
        self.code.cache.sync();
    }

    pub(super) fn resolve_tables(&mut self, memory: &Memory, eip: u32, unit: usize) {
        if !self.code.rows.contains_key(&eip)
            || self.is_stop(eip)
            || !translate::tables::image(&self.images.borrow(), eip, 1)
        {
            return;
        }
        // Import trampolines and transport thunks remain patch-sensitive.
        let Ok(bytes) = memory.read(eip, 15) else {
            return;
        };
        let mut decoder =
            iced_x86::Decoder::with_ip(32, bytes, u64::from(eip), iced_x86::DecoderOptions::NONE);
        let instruction = decoder.decode();
        if matches!(
            instruction.flow_control(),
            iced_x86::FlowControl::UnconditionalBranch
                | iced_x86::FlowControl::IndirectBranch
                | iced_x86::FlowControl::Interrupt
        ) || matches!(instruction.code(), Code::Pushd_imm32 | Code::Pushd_imm8)
        {
            return;
        }
        for &(_, point, _) in &self.code.rows[&eip] {
            self.code.cache.patch(point, unit as u32);
            self.code
                .cache
                .patch(point + 4, ((unit as u64) >> 32) as u32);
        }
    }

    /// Drops units whose guest code changed, and restores the link points
    /// that branch to them.
    pub(super) fn invalidate(&mut self, memory: &mut Memory) {
        if memory.dirty_pages.is_empty() {
            return;
        }
        let mut dropped = false;
        for page in std::mem::take(&mut memory.dirty_pages).into_keys() {
            for eip in self.code.pages.remove(&page).unwrap_or_default() {
                self.promoted.remove(&eip);
                let unit = self.drop_unit(eip);
                self.diagnostics.dropped_units += u64::from(unit);
                dropped |= unit;
            }
        }
        if dropped {
            self.samples.clear();
            self.forget_dynamic();
        }
    }

    /// Drops the unit at `eip`, if any, and restores the link points that
    /// branch to it; the caller forgets dynamic targets.
    pub(super) fn drop_unit(&mut self, eip: u32) -> bool {
        if self.code.table_sources.remove(&eip) {
            self.code.rows.retain(|_, slots| {
                slots.retain(|&(source, _, _)| source != eip);
                !slots.is_empty()
            });
        }
        for &(_, point, stub) in self.code.rows.get(&eip).into_iter().flatten() {
            self.code.cache.patch(point, stub as u32);
            self.code
                .cache
                .patch(point + 4, ((stub as u64) >> 32) as u32);
        }
        let mut next = self.code.links.remove(&eip).unwrap_or(u32::MAX);
        while let Some(link) = self.code.linked.get(next as usize) {
            self.code.cache.patch(link.point, link.original);
            next = link.next;
        }
        if let Some(count) = self.counters.get(&eip) {
            count.set(head::HOT);
        }
        self.repoint_internal_edges(eip, None);
        self.heads.remove(&eip);
        self.code.tier1_words.remove(&eip);
        self.offload.invalidate(eip);
        self.code.units.remove(&eip).is_some()
    }

    /// Empties the indirect table and the shadow stack, which may lead to
    /// dropped units.
    pub(super) fn forget_dynamic(&mut self) {
        self.diagnostics.global_invalidations += 1;
        self.code.table.fill(EMPTY);
        self.runtime.shadow = self.code.shadow.top();
    }

    /// Units must not run past any stop; a new one cuts those that could.
    pub(super) fn observe_stop(&mut self, stop: u32) {
        if self.stops.contains(&stop) {
            return;
        }
        self.stops.push(stop);
        if let Some(word) = &self.interrupt {
            word.fetch_or(
                adv_sync::pending::STOP,
                std::sync::atomic::Ordering::Release,
            );
        }
        self.cut(stop, stop.saturating_add(1));
    }

    /// Drops the units decoded from the pages of `start..end`, which hold
    /// every unit whose trace runs through a new boundary there, and the
    /// unit starting at it, whose dropping restores the links to it.
    pub(super) fn cut(&mut self, start: u32, end: u32) {
        let end = u64::from(end).max(u64::from(start) + 1);
        let mut dropped = false;
        for page in (u64::from(start & !0xfff)..end).step_by(4096) {
            for eip in self.code.pages.remove(&(page as u32)).unwrap_or_default() {
                dropped |= self.drop_unit(eip);
            }
        }
        if dropped {
            self.forget_dynamic();
        }
    }

    /// Clears the cache as a whole (A64-CTL-1).
    pub(crate) fn clear(&mut self) {
        self.diagnostics.clears += 1;
        self.code.cache.truncate(self.code.stub_len);
        self.code.units.clear();
        self.offload.clear();
        self.code.rows.clear();
        self.code.table_sources.clear();
        self.code.uses_permissions = false;
        self.code.pages.clear();
        self.code.links.clear();
        self.code.linked.clear();
        self.code.back_points.clear();
        self.code.direct_points.clear();
        self.code.internal_edges.clear();
        self.counters.clear();
        self.heads.clear();
        self.code.tier1_words.clear();
        self.code.placed.clear();
        self.code.helper_eips = self.helper_histogram();
        self.code.helpers.clear();
        self.code.helper_counts.clear();
        self.code.generation += 1;
        self.code.faults.sites.clear();
        self.forget_dynamic();
    }

    /// Helper runs by EIP.
    pub(crate) fn helper_histogram(&self) -> FxHashMap<u32, u64> {
        let mut eips = self.code.helper_eips.clone();
        for (helper, &count) in self.code.helpers.iter().zip(&self.code.helper_counts) {
            *eips.entry(helper.pc).or_default() += count;
        }
        eips
    }

    /// Whether code at `eip` is known to run, so translating it pays.
    pub(super) fn ran(&self, eip: u32) -> bool {
        !self.first_visits || self.seen.contains(&eip)
    }

    /// Whether `eip`, which has no unit, runs for the first time; it will
    /// not be again.
    pub(super) fn first_visit(&mut self, eip: u32) -> bool {
        self.first_visits && !self.code.units.contains_key(&eip) && self.seen.insert(eip)
    }

    /// Translates the unit at `eip`, and ahead of time the units its main
    /// path leaves to that have run before, so they share one cache sync
    /// and link directly. Every link point of these units to a unit that
    /// exists is patched.
    pub(super) fn translate(&mut self, memory: &mut Memory, eip: u32) -> Result<Entry, String> {
        self.code.points.clear();
        let entry = match self.emit(memory, eip) {
            Some(entry) => entry,
            None => {
                self.clear();
                self.code.points.clear();
                self.emit(memory, eip)
                    .ok_or("a64 unit does not fit an empty cache")?
            }
        };
        // Only the first unit's main path is translated ahead: deeper
        // successors are less certain to run.
        let first = self.code.points.len();
        let mut index = 0;
        while let Some(&(point, target, main)) = self.code.points.get(index) {
            index += 1;
            // The owner must see stops.
            if self.is_stop(target) {
                continue;
            }
            let unit = match self.code.units.get(&target) {
                Some(entry) => entry.unit,
                // A full cache waits for the next miss to clear it.
                None if main
                    && index <= first
                    && self.ran(target)
                    && crate::translate::Fetcher::fetchable(memory, target) =>
                {
                    match self.emit(memory, target) {
                        Some(entry) => entry.unit,
                        None => break,
                    }
                }
                None => continue,
            };
            self.patch(point, target, unit);
        }
        Ok(entry)
    }

    pub(super) fn emit(&mut self, memory: &mut Memory, eip: u32) -> Option<Entry> {
        let at = self.code.cache.next();
        let mut work = std::mem::take(&mut self.code.work);
        work.unit.images.clone_from(&self.images.borrow());
        let (all, checked) = (self.checks != Checks::Adaptive, &self.checked);
        let (control, pe) = if self.runtime.cpu.is_null() {
            (self.control, self.pe)
        } else {
            // SAFETY: during a run the runtime points at its live CPU, which
            // nothing writes while the owner translates.
            let x87 = unsafe { &(*self.runtime.cpu).x87 };
            (x87.control, x87.status & 0x20 != 0)
        };
        work.cold = self.promotion != 0 && !self.promoted.contains(&eip);
        work.resident_links = true;
        translate::translate(
            &mut work,
            memory,
            eip,
            &self.stops,
            &|pc| all || checked.contains(&pc),
            self.code.helpers.len() as u32,
            self.features,
            at,
            self.code.stubs,
            &|pc, word| {
                if self.bridge.is_null() {
                    return None;
                }
                // SAFETY: translation occurs only during the bridge's run.
                unsafe { &*self.bridge.cast::<Bridge<'_>>() }
                    .handler
                    .as_ref()
                    .and_then(|handler| handler.host_thunk(pc, word))
            },
            &|pc| {
                if self.bridge.is_null() {
                    return None;
                }
                // SAFETY: as for `host_thunk`.
                let handler = unsafe { &*self.bridge.cast::<Bridge<'_>>() }
                    .handler
                    .as_ref()?;
                let (target, original) = handler.redirect(pc)?;
                Some(translate::Redirect {
                    target,
                    original: original.to_vec(),
                    checks: handler.redirect_checks(pc).to_vec(),
                    body: handler.direct_body(pc),
                })
            },
            &|pc, code| {
                if self.bridge.is_null() {
                    return Vec::new();
                }
                // SAFETY: admission runs synchronously on the execution owner.
                unsafe { &*self.bridge.cast::<Bridge<'_>>() }
                    .handler
                    .as_ref()
                    .map_or_else(Vec::new, |handler| handler.native_region(pc, code))
            },
            control,
            pe,
        );
        if work.retired() == 0 {
            self.diagnostics.empty_units[usize::from(work.unit.trace.failure & 3)] += 1;
        }
        let entry = self.place(
            eip,
            work.words(),
            work.sites(),
            work.retired(),
            work.marks(),
        );
        if let Some(entry) = entry {
            debug_assert_eq!(entry.unit, at);
            if !work.unit.tables.is_empty() {
                self.code.table_sources.insert(eip);
            }
            for table in &work.unit.tables {
                for (index, &target) in table.targets.iter().enumerate() {
                    let slot = (eip, at + table.offset + index * 8, at + table.stub);
                    self.code.rows.entry(target).or_default().push(slot);
                }
            }
            let mut targets: Vec<_> = work
                .unit
                .tables
                .iter()
                .flat_map(|t| t.targets.iter().copied())
                .collect();
            targets.sort_unstable();
            targets.dedup();
            for target in targets {
                if let Some(entry) = self.code.units.get(&target) {
                    self.resolve_tables(memory, target, entry.unit);
                }
            }
            for link in work.links() {
                let point = at + link.offset;
                self.code.points.push((point, link.eip, link.main));
                if let Some(transition) = link.direct {
                    self.code.direct_points.insert(point, at + transition);
                }
                if link.back {
                    self.code.back_points.insert(point);
                    let (address, _) = self.counter(link.eip);
                    self.bind_counter(point + runtime::BACK_COUNTER, address);
                }
            }
            for link in work.internal_links() {
                let edge = InternalEdge {
                    point: at + link.point,
                    target: at + link.target,
                    transition: at + link.transition,
                    link: at + link.link,
                    source: eip,
                    source_pc: link.source_pc,
                    back: link.counter.is_some(),
                    sample: None,
                };
                let value = if let Some(counter) = link.counter {
                    let (address, value) = self.counter(link.eip);
                    self.bind_counter(at + counter, address);
                    value
                } else {
                    u32::MAX
                };
                let resident = self.heads.get(&link.eip);
                if let Some(head) = resident.filter(|head| {
                    head.trampoline != 0
                        && !head.unprofitable
                        && (edge.back
                            || !head
                                .trace
                                .insts
                                .iter()
                                .any(|inst| inst.pc == edge.source_pc))
                }) {
                    self.code
                        .cache
                        .patch(edge.link, head::branch(edge.link, head.trampoline));
                    self.code
                        .cache
                        .patch(edge.point, head::branch(edge.point, edge.transition));
                } else if resident.is_some() || value == u32::MAX {
                    self.code
                        .cache
                        .patch(edge.point, head::branch(edge.point, edge.target));
                }
                self.code
                    .internal_edges
                    .entry(link.eip)
                    .or_default()
                    .push(edge);
            }
            if self.counters.contains_key(&eip) || !work.internal_links().is_empty() {
                self.code.tier1_words.insert(eip, tier1_words(&work));
            }
            if let Some(head) = self.heads.get(&eip).filter(|head| head.tier1 != 0) {
                self.code
                    .cache
                    .patch(head.tier1, head::branch(head.tier1, entry.unit));
            }
            self.watch_pages(eip, work.code());
            self.code.helpers.extend(work.helpers());
            self.code.helper_counts.resize(self.code.helpers.len(), 0);
        }
        self.code.uses_permissions |= entry.is_some() && work.code.uses_permissions;
        // Code discovery can protect a host page shared with ordinary data.
        // Refresh permission snapshots before linked code resumes.
        if self.code.uses_permissions {
            self.classify(memory);
        }
        self.code.work = work;
        entry
    }

    pub(super) fn watch_pages(&mut self, eip: u32, spans: &[(u32, u32)]) {
        let mut last = None;
        for &(start, len) in spans.iter().filter(|&&(_, len)| len != 0) {
            let end = (u64::from(start) + u64::from(len) - 1).min(u64::from(u32::MAX));
            for page in (start & !0xfff..=end as u32 & !0xfff).step_by(4096) {
                if last.is_none_or(|last| last < page) {
                    last = Some(page);
                    self.code.pages.entry(page).or_default().push(eip);
                }
            }
        }
    }

    /// Appends a unit for `eip` whose `sites` are at byte offsets.
    pub(super) fn place(
        &mut self,
        eip: u32,
        words: &[u32],
        sites: &[(usize, Site)],
        retired: u32,
        marks: &[crate::jit_map::Mark],
    ) -> Option<Entry> {
        let unit = self.code.cache.append(words)?;
        let from = self.code.faults.sites.len().saturating_sub(1);
        self.code
            .faults
            .sites
            .extend(sites.iter().map(|&(offset, site)| (unit + offset, site)));
        if !self.code.faults.sites[from..].is_sorted_by_key(|&(at, _)| at) {
            self.code.faults.sites.sort_unstable_by_key(|&(at, _)| at);
        }
        let entry = Entry { unit, retired };
        crate::jit_map::record(unit, words, eip, retired, marks, "a64");
        self.code.units.insert(eip, entry);
        self.code.placed.push((unit, eip));
        Some(entry)
    }
}

/// Counts the Tier-1 body words for each guest instruction. A taken
/// back edge adds its canonical publication and patch word, excluding
/// the unlinked dispatch and discovery counter.
fn tier1_words(work: &translate::Workspace) -> FxHashMap<u32, u32> {
    use crate::jit_map::Part;

    let mut words: FxHashMap<_, _> = work
        .unit
        .trace
        .insts
        .iter()
        .map(|inst| (inst.pc, 0))
        .collect();
    let marks = work.marks();
    let cold = marks
        .iter()
        .position(|mark| mark.part == Part::Cold)
        .unwrap_or(marks.len());
    for (i, mark) in marks[..cold].iter().enumerate() {
        let end = marks
            .get(i + 1)
            .map_or(work.words().len() * 4, |mark| mark.offset as usize);
        *words.entry(mark.eip).or_default() += ((end - mark.offset as usize) / 4) as u32;
    }
    for link in work.links().iter().filter(|link| link.back) {
        let at = marks.partition_point(|mark| mark.offset as usize <= link.offset);
        if at > cold {
            let start = marks[..at]
                .iter()
                .rfind(|mark| mark.part == Part::Cold)
                .unwrap();
            *words.entry(start.eip).or_default() +=
                ((link.offset + 4 - start.offset as usize) / 4) as u32;
        } else if let Some(mark) = marks.get(at - 1) {
            *words.entry(mark.eip).or_default() -= ((runtime::BACK_COUNTER + 4) / 4) as u32;
        }
    }
    words
}
