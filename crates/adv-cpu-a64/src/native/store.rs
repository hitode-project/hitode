//! Executable units, link records, fault recovery and page watches.
//!
//! The store places artifacts and holds the data for link, repoint and
//! invalidation operations. Worker placements retain their own fault sites
//! for the lifetime of a job.

use rustc_hash::{FxHashMap, FxHashSet};
use std::cell::Cell;
use std::collections::BTreeMap;
use std::sync::Arc;

use crate::cache::CodeCache;
use crate::encode::{b, bl};
use crate::facts::Key;
use adv_cpu_core::CpuState;

use crate::runtime::{self, Enter};
use crate::translate::front::{Unit, decode::Helper, trace::Trace};
use crate::translate::lower::{
    Link, Site, Stubs, VersionLayout,
    flags::{Operand, Sense},
};

#[derive(Clone, Copy)]
pub(super) struct Tier1Unit {
    pub(super) unit: usize,
    pub(super) body: usize,
    pub(super) miss: Option<usize>,
    pub(super) retired: u32,
}

pub(super) const CHAIN_LEN: usize = 3;

pub(super) struct Chain {
    pub(super) unit: Arc<Unit>,
    pub(super) mask: u64,
    pub(super) helpers: u32,
    pub(super) versions: Vec<Version>,
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
    pub(super) arrivals: [Cell<u64>; CHAIN_LEN],
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
    pub(super) sensor: Option<usize>,
}

#[derive(Default)]
pub(super) struct SlowPaths {
    pub(super) interpreter: BTreeMap<(u32, InterpreterCause), u64>,
    pub(super) values: BTreeMap<u32, u64>,
}

pub(super) struct CodeStore {
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
    pub(super) units: FxHashMap<u32, Tier1Unit>,
    pub(super) chains: FxHashMap<u32, Chain>,
    pub(super) version_stats: Option<FxHashMap<u32, Box<VersionStats>>>,
    pub(super) slow_paths: Option<SlowPaths>,
    #[cfg(test)]
    pub(super) value_counts: FxHashMap<u32, u64>,
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
    pub(super) shadow: ShadowStack,
    pub(super) faults: Faults,
}

impl CodeStore {
    pub(super) fn contains_unit(&self, entry: u32, address: usize) -> bool {
        self.units
            .get(&entry)
            .is_some_and(|unit| unit.unit == address)
            || self.chains.get(&entry).is_some_and(|chain| {
                chain
                    .versions
                    .iter()
                    .any(|version| version.entry.unit == address)
            })
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
        if self
            .units
            .get(&eip)
            .is_some_and(|entry| entry.unit == checked)
            && let Some(chain) = self.chains.get(&eip)
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
        self.chains.remove(&eip);
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
        self.units.remove(&eip).is_some()
    }

    pub(super) fn watch_pages(&mut self, eip: u32, spans: &[(u32, u32)]) {
        let mut last = None;
        for &(start, len) in spans.iter().filter(|&&(_, len)| len != 0) {
            let end = (u64::from(start) + u64::from(len) - 1).min(u64::from(u32::MAX));
            for page in (start & !0xfff..=end as u32 & !0xfff).step_by(4096) {
                if last.is_none_or(|last| last < page) {
                    last = Some(page);
                    self.pages.entry(page).or_default().push(eip);
                }
            }
        }
    }

    /// Appends a unit for `eip` whose `sites` are at byte offsets.
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
        let from = self.faults.sites.len().saturating_sub(1);
        self.faults
            .sites
            .extend(sites.iter().map(|&(offset, site)| (unit + offset, site)));
        if !self.faults.sites[from..].is_sorted_by_key(|&(at, _)| at) {
            self.faults.sites.sort_unstable_by_key(|&(at, _)| at);
        }
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
    /// Host PC of each site, ascending: units are appended in address
    /// order, and their sites in emission order.
    pub(super) sites: Vec<(usize, Site)>,
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
    pub(super) fn site(&self, pc: usize) -> Option<Site> {
        site_at(&self.sites, pc)
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

/// The shadow return stack: entries of {host continuation, guest EIP}
/// growing down from sentinels that never match, over guard pages.
pub(super) struct ShadowStack {
    base: *mut libc::c_void,
    len: usize,
    guard: usize,
}

impl ShadowStack {
    const ENTRIES: usize = 1 << 16;

    pub(super) fn new() -> Result<Self, String> {
        // SAFETY: sysconf has no preconditions.
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
        let guard = page.max(16 << 10);
        let len = guard + (Self::ENTRIES + runtime::SHADOW_SENTINELS) * 16;
        let len = len.next_multiple_of(page);
        // SAFETY: a fresh anonymous mapping.
        let base = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_NONE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        if base == libc::MAP_FAILED {
            return Err("cannot map the shadow stack".into());
        }
        let stack = Self { base, len, guard };
        // SAFETY: inside the mapping just made.
        let protected = unsafe {
            libc::mprotect(
                base.cast::<u8>().add(guard).cast(),
                len - guard,
                libc::PROT_READ | libc::PROT_WRITE,
            )
        };
        if protected != 0 {
            return Err("cannot protect the shadow stack".into());
        }
        let top = stack.top() as *mut u64;
        for k in 0..runtime::SHADOW_SENTINELS {
            // SAFETY: the sentinels are the last entries of the mapping.
            unsafe {
                top.add(2 * k).write(0);
                top.add(2 * k + 1).write(u64::MAX);
            }
        }
        Ok(stack)
    }

    /// The empty stack pointer, just below the sentinels.
    pub(super) fn top(&self) -> u64 {
        (self.base as usize + self.len - runtime::SHADOW_SENTINELS * 16) as u64
    }

    pub(super) fn guard(&self) -> (usize, usize) {
        (self.base as usize, self.base as usize + self.guard)
    }
}

impl Drop for ShadowStack {
    fn drop(&mut self) {
        // SAFETY: the mapping `new` made.
        unsafe { libc::munmap(self.base, self.len) };
    }
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
