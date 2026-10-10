//! Translated execution on AArch64 hosts.
//!
//! Units come from `translate`. A dispatch does not leave translated code: the
//! `dispatch` stub calls `dispatch`, which finds or translates the target
//! unit and, for a static target, patches the link point into a direct
//! branch, so hot paths chain without even that call. Each unit's safepoint
//! checks the countdown, which `run` sets from the fuel and `SLICE`, so a
//! chain returns within its budget. A link is undone when its target is
//! dropped. Dynamic targets go through the indirect table, which the helper
//! fills, and returns through the shadow stack; both are emptied whenever a
//! unit is dropped, so they only ever lead to live units.
//!
//! Faults (§6.3, §6.4): every host instruction that may fault has a side-table
//! site naming its guest instruction. The signal handler only checks that the
//! PC is such a site, records it, and resumes at the fault stub. Because a
//! translated instruction commits nothing before its last possible fault
//! (§5.2), the owner then rebuilds the flags from the site's carry and
//! PF/AF recipe and interprets that one instruction from the exact prior
//! state: the interpreter either completes it (a soft page) or reports the
//! precise guest fault.
//!
//! Code invalidation: translation registers its guest bytes with
//! `watch_code`, which write-protects them in the window, so any store to
//! them reaches the interpreter or Rust and marks the page dirty. Dirty
//! pages drop their units before the next dispatch.

use rustc_hash::{FxHashMap, FxHashSet};
use std::cell::Cell;

use adv_cpu_core::CpuState;
use adv_cpu_core::memory::Memory;

use crate::cache::CodeCache;
use crate::encode::{b, bl};
use crate::runtime::{self, Exit, Runtime};
use crate::translate::{self, Features, Operand, Sense, Site, Stubs};

const CACHE_LEN: usize = 64 << 20;

/// Most guest instructions one `run` completes, so the owner still sees
/// deadlines and interrupts while linked units loop.
const SLICE: u64 = 1 << 16;

type Enter = unsafe extern "C" fn(*mut Runtime, usize);

/// An indirect table entry no EIP matches.
const EMPTY: [u64; 2] = [u64::MAX, 0];

/// What the signal handler may read while translated code runs.
struct Faults {
    /// Executable range of translated code.
    start: usize,
    end: Cell<usize>,
    /// Host PC of each site, ascending: units are appended in address
    /// order, and their sites in emission order.
    sites: Vec<(usize, Site)>,
    stub: usize,
    /// The site that faulted, written in signal context.
    pc: Cell<usize>,
    /// The shadow stack's guard pages.
    guard: (usize, usize),
    /// Where an overflow resumes, and the runtime field it reads the
    /// faulting PC from.
    reset: usize,
    resume: *mut u64,
}

impl Faults {
    fn site(&self, pc: usize) -> Option<Site> {
        let index = self.sites.binary_search_by_key(&pc, |&(at, _)| at).ok()?;
        Some(self.sites[index].1)
    }
}

thread_local! {
    /// The `Faults` of the code this thread is running, or null.
    static ACTIVE: Cell<*const Faults> = const { Cell::new(std::ptr::null()) };
}

/// Runs in signal context: no allocation, no locks.
unsafe fn on_fault(pc: usize, address: usize) -> usize {
    ACTIVE
        .try_with(|slot| {
            let faults = slot.get();
            if faults.is_null() {
                return 0;
            }
            // SAFETY: `run` keeps the pointee alive and unmodified, apart
            // from the `pc` cell, until it clears the slot.
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
            if faults.site(pc).is_none() {
                return 0;
            }
            faults.pc.set(pc);
            faults.stub
        })
        .unwrap_or(0)
}

/// The shadow return stack: entries of {host continuation, guest EIP}
/// growing down from sentinels that never match, over guard pages.
struct ShadowStack {
    base: *mut libc::c_void,
    len: usize,
    guard: usize,
}

impl ShadowStack {
    const ENTRIES: usize = 1 << 16;

    fn new() -> Result<Self, String> {
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
    fn top(&self) -> u64 {
        (self.base as usize + self.len - runtime::SHADOW_SENTINELS * 16) as u64
    }

    fn guard(&self) -> (usize, usize) {
        (self.base as usize, self.base as usize + self.guard)
    }
}

impl Drop for ShadowStack {
    fn drop(&mut self) {
        // SAFETY: the mapping `new` made.
        unsafe { libc::munmap(self.base, self.len) };
    }
}

/// Host features from the OS.
fn features() -> Features {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        const HWCAP_FLAGM: u64 = 1 << 35;
        // SAFETY: getauxval has no preconditions.
        let hwcap = unsafe { libc::getauxval(libc::AT_HWCAP) } as u64;
        Features {
            flagm: hwcap & HWCAP_FLAGM != 0,
        }
    }
    #[cfg(target_os = "macos")]
    {
        let mut value: libc::c_int = 0;
        let mut len = std::mem::size_of_val(&value);
        // SAFETY: a valid name, and an output buffer of `len` bytes.
        let found = unsafe {
            libc::sysctlbyname(
                c"hw.optional.arm.FEAT_FlagM".as_ptr(),
                (&raw mut value).cast(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        } == 0;
        Features {
            flagm: found && value != 0,
        }
    }
}

#[derive(Clone, Copy)]
struct Entry {
    unit: usize,
    retired: u32,
}

/// One trip through translated code.
pub(crate) struct Run {
    pub exit: Exit,
    /// Guest instructions completed.
    pub retired: u32,
    /// Bytes translated on the way, zero on a cache hit.
    pub translated: usize,
}

/// What `dispatch` works on during one `run`.
struct Owner {
    native: *mut Native,
    memory: *mut Memory,
    translated: usize,
    error: Option<String>,
}

/// Called by the `dispatch` stub with the guest EIP and the link point to
/// patch, or 0. Returns the unit to continue at, or 0 to leave.
unsafe extern "C" fn dispatch(runtime: *mut Runtime, eip: u32, point: usize) -> usize {
    // SAFETY: `run` points `owner` at a live `Owner` whose pointees it does
    // not touch while translated code runs.
    let owner = unsafe { &mut *((*runtime).owner as *mut Owner) };
    let (native, memory) = unsafe { (&mut *owner.native, &mut *owner.memory) };
    if native.stops.contains(&eip) || owner.error.is_some() {
        return 0;
    }
    // A first visit runs in the owner; the point links on the next one.
    if native.first_visit(eip) {
        return 0;
    }
    let before = native.cache.used();
    let unit = match native.link(memory, point, eip) {
        Ok(unit) => unit,
        Err(error) => {
            owner.error = Some(error);
            return 0;
        }
    };
    if point == 0 {
        native.table[runtime::table_slot(eip)] = [u64::from(eip), unit as u64];
    }
    owner.translated += native.cache.used().saturating_sub(before);
    native.cache.sync();
    native.faults.end.set(native.cache.next());
    unit
}

pub(crate) struct Native {
    cache: CodeCache,
    enter: Enter,
    stubs: Stubs,
    /// Cache bytes the stubs occupy; clearing keeps them.
    stub_len: usize,
    units: FxHashMap<u32, Entry>,
    /// Unit entries by the guest pages their code came from.
    pages: FxHashMap<u32, Vec<u32>>,
    /// Patched link points, with the words they held, by the unit they
    /// branch to: the head of a list in `linked`.
    links: FxHashMap<u32, u32>,
    /// Link point, the word it held, and the next point to the same unit.
    linked: Vec<(usize, u32, u32)>,
    /// Link points of the units the current translation placed.
    points: Vec<(usize, u32, bool)>,
    /// Entries that have run, so a return to one translates it.
    seen: FxHashSet<u32>,
    /// Whether entries run once before they are translated.
    pub(crate) first_visits: bool,
    /// Buffers of the last translation.
    work: translate::Workspace,
    /// Counts cache clears, which invalidate link points.
    generation: u64,
    /// The indirect table the runtime points at.
    table: Box<[[u64; 2]]>,
    shadow: ShadowStack,
    /// Stop addresses seen so far; units end before all of them.
    stops: Vec<u32>,
    pub(crate) features: Features,
    faults: Faults,
    runtime: Box<Runtime>,
}

impl Native {
    /// `window` is the host address of guest address 0 in the guest's
    /// `GuestWindow`, which must outlive this.
    pub fn new(window: usize) -> Result<Self, String> {
        adv_cpu_core::fault_recovery::install_translated(on_fault)?;
        let mut cache = CodeCache::new(CACHE_LEN)?;
        let stubs = runtime::stubs();
        let base = cache.append(&stubs.code).ok_or("stubs do not fit")?;
        // SAFETY: `enter` follows the C calling convention with the
        // signature of `Enter`; see `runtime::emit_enter`.
        let enter = unsafe { std::mem::transmute::<usize, Enter>(base + stubs.enter) };
        let mut table = vec![EMPTY; runtime::TABLE_LEN].into_boxed_slice();
        let shadow = ShadowStack::new()?;
        let mut runtime = Box::new(Runtime::new(std::ptr::null_mut(), window as u64));
        runtime.table = table.as_mut_ptr() as u64;
        runtime.shadow_top = shadow.top();
        runtime.shadow = shadow.top();
        Ok(Self {
            stub_len: cache.used(),
            faults: Faults {
                start: base,
                end: Cell::new(cache.next()),
                sites: Vec::new(),
                stub: base + stubs.fault,
                pc: Cell::new(0),
                guard: shadow.guard(),
                reset: base + stubs.shadow_reset,
                resume: &raw mut runtime.resume,
            },
            cache,
            enter,
            stubs: Stubs {
                exit: base + stubs.exit,
                dispatch: base + stubs.dispatch,
                lookup: base + stubs.lookup,
                lookup_slow: base + stubs.lookup_slow,
                ret_miss: base + stubs.ret_miss,
            },
            units: FxHashMap::default(),
            pages: FxHashMap::default(),
            links: FxHashMap::default(),
            linked: Vec::new(),
            points: Vec::new(),
            seen: FxHashSet::default(),
            first_visits: true,
            work: translate::Workspace::default(),
            generation: 0,
            table,
            shadow,
            stops: Vec::new(),
            features: features(),
            runtime,
        })
    }

    /// Runs the unit at `cpu.eip` if it completes at most `fuel` guest
    /// instructions; `None` leaves them to the interpreter. A fault returns
    /// `Exit::Fault` with EIP at the faulting instruction and the guest state
    /// exactly that before it.
    pub fn run(
        &mut self,
        cpu: &mut CpuState,
        memory: &mut Memory,
        stop: u32,
        fuel: u64,
    ) -> Result<Option<Run>, String> {
        self.invalidate(memory);
        self.observe_stop(stop);
        let (entry, translated) = match self.units.get(&cpu.eip).copied() {
            Some(entry) => (entry, 0),
            None if self.first_visit(cpu.eip) => {
                let visit = translate::first::run(cpu, memory, stop, fuel.min(SLICE));
                let exit = if visit.declined {
                    Exit::Interpret
                } else {
                    Exit::Dispatch
                };
                return Ok(Some(Run {
                    exit,
                    retired: visit.retired,
                    translated: 0,
                }));
            }
            None => {
                let before = self.cache.used();
                let entry = self.translate(memory, cpu.eip)?;
                (entry, self.cache.used().saturating_sub(before))
            }
        };
        if u64::from(entry.retired) > fuel {
            return Ok(None);
        }
        if entry.retired == 0 {
            // The first instruction is not translated: skip the round trip.
            return Ok(Some(Run {
                exit: Exit::Interpret,
                retired: 0,
                translated,
            }));
        }
        debug_assert_eq!(cpu.lazy_flags[3], 0, "lazy flags settle before entry");
        let budget = fuel.min(SLICE);
        self.cache.sync();
        self.faults.end.set(self.cache.next());
        let runtime: *mut Runtime = &mut *self.runtime;
        let faults: *const Faults = &self.faults;
        let enter = self.enter;
        let mut owner = Owner {
            native: self,
            memory,
            translated,
            error: None,
        };
        // SAFETY: until `enter` returns, `self` is reached only through
        // `owner`, by `dispatch`. `unit` is translated code ending at the
        // stubs, and the runtime points at a live CPU for the call.
        unsafe {
            (*runtime).countdown = budget;
            (*runtime).cpu = cpu;
            (*runtime).helper = dispatch as *const () as u64;
            (*runtime).owner = &raw mut owner as u64;
            let previous = ACTIVE.replace(faults);
            enter(runtime, entry.unit);
            ACTIVE.set(previous);
        }
        self.runtime.cpu = std::ptr::null_mut();
        self.runtime.owner = 0;
        if let Some(error) = owner.error {
            return Err(error);
        }
        let translated = owner.translated;
        let exit = Exit::from_raw(self.runtime.exit)
            .ok_or_else(|| format!("unknown a64 exit {}", self.runtime.exit))?;
        let mut retired = budget - self.runtime.countdown;
        if exit == Exit::Fault {
            let site = self
                .faults
                .site(self.faults.pc.get())
                .expect("faulting site");
            recover(cpu, site, self.runtime.temps);
            retired -= u64::from(site.refund);
        }
        let retired = retired as u32;
        Ok(Some(Run {
            exit,
            retired,
            translated,
        }))
    }

    /// The unit at `eip`, translated if needed; a nonzero `point` is
    /// patched into a branch to it. The caller keeps stops unlinked: the
    /// owner must see them.
    fn link(&mut self, memory: &mut Memory, point: usize, eip: u32) -> Result<usize, String> {
        let generation = self.generation;
        let entry = match self.units.get(&eip) {
            Some(&entry) => entry,
            None => self.translate(memory, eip)?,
        };
        // Translating may have cleared the cache and the link point with it.
        if point != 0 && self.generation == generation {
            self.patch(point, eip, entry.unit);
        }
        Ok(entry.unit)
    }

    /// Makes the link point at `point` branch to `unit`, the unit at `eip`.
    fn patch(&mut self, point: usize, eip: u32, unit: usize) {
        let word = self.cache.word(point);
        let offset = (unit as i64 - point as i64) as i32;
        // A real call's `bl` stays a call.
        let link = if word & 0xFC00_0000 == 0x9400_0000 {
            bl(offset)
        } else {
            b(offset)
        };
        self.cache.patch(point, link);
        let next = self.links.insert(eip, self.linked.len() as u32);
        self.linked.push((point, word, next.unwrap_or(u32::MAX)));
    }

    /// Drops units whose guest code changed, and restores the link points
    /// that branch to them.
    fn invalidate(&mut self, memory: &mut Memory) {
        if memory.dirty_pages.is_empty() {
            return;
        }
        let mut dropped = false;
        for page in std::mem::take(&mut memory.dirty_pages).into_keys() {
            for eip in self.pages.remove(&page).unwrap_or_default() {
                dropped |= self.units.remove(&eip).is_some();
                let mut next = self.links.remove(&eip).unwrap_or(u32::MAX);
                while let Some(&(point, word, after)) = self.linked.get(next as usize) {
                    self.cache.patch(point, word);
                    next = after;
                }
            }
        }
        if dropped {
            self.forget_dynamic();
        }
    }

    /// Empties the indirect table and the shadow stack, which may lead to
    /// dropped units.
    fn forget_dynamic(&mut self) {
        self.table.fill(EMPTY);
        self.runtime.shadow = self.shadow.top();
    }

    /// Units must not run past any stop; a new one clears them all.
    fn observe_stop(&mut self, stop: u32) {
        if self.stops.contains(&stop) {
            return;
        }
        self.stops.push(stop);
        self.clear();
    }

    /// Clears the cache as a whole (§7.1).
    fn clear(&mut self) {
        self.cache.truncate(self.stub_len);
        self.units.clear();
        self.pages.clear();
        self.links.clear();
        self.linked.clear();
        self.generation += 1;
        self.faults.sites.clear();
        self.forget_dynamic();
    }

    /// Whether code at `eip` is known to run, so translating it pays.
    fn ran(&self, eip: u32) -> bool {
        !self.first_visits || self.seen.contains(&eip)
    }

    /// Whether `eip`, which has no unit, runs for the first time; it will
    /// not be again.
    fn first_visit(&mut self, eip: u32) -> bool {
        self.first_visits && !self.units.contains_key(&eip) && self.seen.insert(eip)
    }

    /// Translates the unit at `eip`, and ahead of time the units its main
    /// path leaves to that have run before, so they share one cache sync
    /// and link directly. Every link point of these units to a unit that
    /// exists is patched.
    fn translate(&mut self, memory: &mut Memory, eip: u32) -> Result<Entry, String> {
        self.points.clear();
        let entry = match self.emit(memory, eip) {
            Some(entry) => entry,
            None => {
                self.clear();
                self.points.clear();
                self.emit(memory, eip)
                    .ok_or("a64 unit does not fit an empty cache")?
            }
        };
        // Only the first unit's main path is translated ahead: deeper
        // successors are less certain to run.
        let first = self.points.len();
        let mut index = 0;
        while let Some(&(point, target, main)) = self.points.get(index) {
            index += 1;
            // The owner must see stops.
            if self.stops.contains(&target) {
                continue;
            }
            let unit = match self.units.get(&target) {
                Some(entry) => entry.unit,
                // A full cache waits for the next miss to clear it.
                None if main && index <= first && self.ran(target) => {
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

    fn emit(&mut self, memory: &mut Memory, eip: u32) -> Option<Entry> {
        let at = self.cache.next();
        let mut work = std::mem::take(&mut self.work);
        translate::translate(
            &mut work,
            memory,
            eip,
            &self.stops,
            self.features,
            at,
            self.stubs,
        );
        let entry = self.place(eip, work.words(), work.sites(), work.retired());
        if let Some(entry) = entry {
            debug_assert_eq!(entry.unit, at);
            let points = work
                .links()
                .iter()
                .map(|link| (at + link.offset, link.eip, link.main));
            self.points.extend(points);
            // The spans are sorted, so a page repeats only next to itself.
            let mut last = None;
            for &(start, len) in work.code() {
                for page in [start & !0xfff, (start + len - 1) & !0xfff] {
                    if last != Some(page) {
                        last = Some(page);
                        self.pages.entry(page).or_default().push(eip);
                    }
                }
            }
        }
        self.work = work;
        entry
    }

    /// Appends a unit for `eip` whose `sites` are at byte offsets.
    fn place(
        &mut self,
        eip: u32,
        words: &[u32],
        sites: &[(usize, Site)],
        retired: u32,
    ) -> Option<Entry> {
        let unit = self.cache.append(words)?;
        let from = self.faults.sites.len().saturating_sub(1);
        self.faults
            .sites
            .extend(sites.iter().map(|&(offset, site)| (unit + offset, site)));
        if !self.faults.sites[from..].is_sorted_by_key(|&(at, _)| at) {
            self.faults.sites.sort_unstable_by_key(|&(at, _)| at);
        }
        let entry = Entry { unit, retired };
        self.units.insert(eip, entry);
        Some(entry)
    }
}

/// Rebuilds the x86 flags at a fault site, where the exit stub assumed
/// subtraction sense and canonical PF/AF sources.
fn recover(cpu: &mut CpuState, site: Site, temps: [u64; 2]) {
    cpu.eip = site.eip;
    let regs = cpu.regs;
    let value = |operand| match operand {
        Operand::Guest(index) => regs[usize::from(index)],
        Operand::Temp(index) => temps[usize::from(index)] as u32,
        Operand::Imm(value) => value,
    };
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

#[cfg(test)]
mod tests {
    use adv_cpu_core::memory::Memory;

    use super::*;
    use crate::encode::Size::{W, X};
    use crate::encode::{Reg, ldr, ldr_reg, ldur, movk, movz, sub_imm};
    use crate::translate::{Carry, Recipe};

    const X19: Reg = Reg::new(19);
    const X23: Reg = Reg::new(23);
    const X27: Reg = Reg::new(27);
    const STOP: u32 = 0xffff_0000;

    /// Guest memory over the whole window and its translator.
    fn native() -> (Memory, Native) {
        let memory = Memory::window(0, 1 << 32).unwrap();
        let mut native = Native::new(memory.window_base().unwrap()).unwrap();
        native.observe_stop(STOP);
        (memory, native)
    }

    /// A unit at `eip` that loads EAX with `load` (a site of `eip`) and then
    /// dispatches to `eip + 3`.
    fn install(native: &mut Native, eip: u32, load: u32) {
        let at = native.cache.next();
        let next = eip + 3;
        // Nothing is prepaid: the exit charges the countdown.
        let site = Site {
            eip,
            refund: 0,
            sense: Sense::Subtraction,
            carry: Carry::Host,
            recipe: Recipe::Canonical,
        };
        let words = [
            load,
            sub_imm(X, Reg::new(9), Reg::new(9), 1, false),
            movz(W, Reg::new(0), Exit::Dispatch as u16, 0),
            movz(W, Reg::new(1), next as u16, 0),
            movk(W, Reg::new(1), (next >> 16) as u16, 16),
            b((native.stubs.exit as i64 - (at + 20) as i64) as i32),
        ];
        native.place(eip, &words, &[(0, site)], 1).unwrap();
    }

    fn cpu(eip: u32, esp: u32, ecx: u32) -> CpuState {
        let mut cpu = CpuState {
            eip,
            eflags: 0x202,
            ..CpuState::default()
        };
        cpu.regs[0] = 0x5a5a_5a5a;
        cpu.regs[1] = ecx;
        cpu.regs[4] = esp;
        cpu
    }

    #[test]
    fn the_window_is_aligned_and_biases_the_stack_registers() {
        let (memory, native) = native();
        let base = memory.window_base().unwrap();
        assert_eq!(base % (1 << 32), 0);
        assert!(base >= adv_cpu_core::virtual_memory::WINDOW_GUARD);
        assert_eq!(native.runtime.window, base as u64);
    }

    #[test]
    fn a_readable_guest_page_loads_directly() {
        let (mut memory, mut native) = native();
        memory.map(0x2_0000, 0x4000, 1).unwrap();
        memory
            .write(0x2_0004, &0x1234_5678u32.to_le_bytes())
            .unwrap();
        install(
            &mut native,
            0x1000,
            ldr_reg(W, X19, X27, Reg::new(20), false), // mov eax, [ecx]
        );
        let mut cpu = cpu(0x1000, 0x8000, 0x2_0004);
        let run = native.run(&mut cpu, &mut memory, STOP, 1).unwrap().unwrap();
        assert_eq!((run.exit, run.retired), (Exit::Dispatch, 1));
        assert_eq!(cpu.regs[0], 0x1234_5678);
        assert_eq!(cpu.eip, 0x1003);
    }

    #[test]
    fn faults_in_guards_and_inaccessible_pages_resume_at_the_site() {
        let (mut memory, mut native) = native();
        // A page the guest cannot read stays inaccessible in the window.
        memory.map(0xa_0000, 0x1000, 0).unwrap();
        // push/pop past 0 and past 4 GiB, an unmapped and an unreadable page.
        install(&mut native, 0x1000, ldur(W, X19, X23, -4));
        install(&mut native, 0x2000, ldr(W, X19, X23, 4));
        install(
            &mut native,
            0x3000,
            ldr_reg(W, X19, X27, Reg::new(20), false),
        );
        for (eip, esp, ecx) in [
            (0x1000, 0, 0),
            (0x2000, 0xffff_fffc, 0),
            (0x3000, 0x8000, 0x9_0000),
            (0x3000, 0x8000, 0xa_0000),
        ] {
            let mut cpu = cpu(eip, esp, ecx);
            let before = cpu.clone();
            let run = native.run(&mut cpu, &mut memory, STOP, 1).unwrap().unwrap();
            assert_eq!((run.exit, run.retired), (Exit::Fault, 0), "{eip:#x}");
            assert_eq!(
                cpu, before,
                "{eip:#x}: state is exactly that before the instruction"
            );
        }
    }

    #[test]
    fn a_fault_rebuilds_flags_from_the_sites_sense_and_recipe() {
        let mut cpu = cpu(0, 0, 0);
        cpu.regs[0] = 0x10;
        // After `add eax, 1` gave 0x10: C set means CF clear in addition
        // sense, which the stub read as CF set; 0x10 has odd parity, and
        // 0x0f ^ 1 ^ 0x10 has bit 4 set.
        cpu.eflags = 0x202 | 1 | 0x04;
        recover(
            &mut cpu,
            Site {
                eip: 0x1234,
                refund: 3,
                sense: Sense::Addition,
                carry: Carry::Host,
                recipe: Recipe::AddResult(Operand::Guest(0), Operand::Imm(1)),
            },
            [0; 2],
        );
        assert_eq!(cpu.eip, 0x1234);
        assert_eq!(cpu.eflags, 0x202 | 0x10);
    }
}
