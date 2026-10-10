//! Translated execution on AArch64 hosts.
//!
//! Units come from `translate`. An exit to a static target returns here
//! once: the owner then patches its link point into a branch to the target
//! unit, so hot paths chain without leaving translated code. Each unit's
//! safepoint checks the countdown, which `run` sets from the fuel and
//! `SLICE`, so a chain returns within its budget. A link is undone when its
//! target is dropped. The shadow stack comes with the next M1 slice.
//!
//! Faults (§6.3, §6.4): every host instruction that may fault has a side-table
//! site naming its guest instruction. The signal handler only checks that the
//! PC is such a site, records it, and resumes at the fault stub. Because a
//! translated instruction commits nothing before its last possible fault
//! (§5.2), the owner then rebuilds the flags from the site's carry sense and
//! PF/AF recipe and interprets that one instruction from the exact prior
//! state: the interpreter either completes it (a soft page) or reports the
//! precise guest fault.
//!
//! Code invalidation: translation registers its guest bytes with
//! `watch_code`, which write-protects them in the window, so any store to
//! them reaches the interpreter or Rust and marks the page dirty. Dirty
//! pages drop their units before the next dispatch.

use std::cell::Cell;
use std::collections::HashMap;

use adv_cpu_core::CpuState;
use adv_cpu_core::memory::Memory;

use crate::cache::CodeCache;
use crate::encode::Size::W;
use crate::encode::{Reg, b, movz};
use crate::runtime::{self, Exit, Runtime};
use crate::translate::{self, Features, Operand, Sense, Site};

const CACHE_LEN: usize = 64 << 20;

/// Most guest instructions one `run` completes, so the owner still sees
/// deadlines and interrupts while linked units loop.
const SLICE: u64 = 1 << 16;

type Enter = unsafe extern "C" fn(*mut Runtime, usize);

/// What the signal handler may read while translated code runs.
struct Faults {
    /// Executable range of translated code.
    start: usize,
    end: Cell<usize>,
    /// Host PC of each site.
    sites: HashMap<usize, Site>,
    stub: usize,
    /// The site that faulted, written in signal context.
    pc: Cell<usize>,
}

thread_local! {
    /// The `Faults` of the code this thread is running, or null.
    static ACTIVE: Cell<*const Faults> = const { Cell::new(std::ptr::null()) };
}

/// Runs in signal context: no allocation, no locks.
unsafe fn on_fault(pc: usize, _: usize) -> usize {
    ACTIVE
        .try_with(|slot| {
            let faults = slot.get();
            if faults.is_null() {
                return 0;
            }
            // SAFETY: `run` keeps the pointee alive and unmodified, apart
            // from the `pc` cell, until it clears the slot.
            let faults = unsafe { &*faults };
            if !(faults.start..faults.end.get()).contains(&pc) || !faults.sites.contains_key(&pc) {
                return 0;
            }
            faults.pc.set(pc);
            faults.stub
        })
        .unwrap_or(0)
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

pub(crate) struct Native {
    cache: CodeCache,
    enter: Enter,
    exit: usize,
    /// Cache bytes the stubs occupy; clearing keeps them.
    stub_len: usize,
    units: HashMap<u32, Entry>,
    /// Unit entries by the guest pages their code came from.
    pages: HashMap<u32, Vec<u32>>,
    /// Patched link points by the unit they branch to.
    links: HashMap<u32, Vec<usize>>,
    /// Counts cache clears, which invalidate link points.
    generation: u64,
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
        Ok(Self {
            stub_len: cache.used(),
            faults: Faults {
                start: base,
                end: Cell::new(cache.next()),
                sites: HashMap::new(),
                stub: base + stubs.fault,
                pc: Cell::new(0),
            },
            cache,
            enter,
            exit: base + stubs.exit,
            units: HashMap::new(),
            pages: HashMap::new(),
            links: HashMap::new(),
            generation: 0,
            stops: Vec::new(),
            features: features(),
            runtime: Box::new(Runtime::new(std::ptr::null_mut(), window as u64)),
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
        let (entry, mut translated) = match self.units.get(&cpu.eip) {
            Some(&entry) => (entry, 0),
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
        self.runtime.countdown = budget;
        self.runtime.link = 0;
        self.faults.end.set(self.cache.next());
        self.runtime.cpu = cpu;
        let previous = ACTIVE.replace(&self.faults);
        // SAFETY: `unit` is translated code ending at the exit stub, and the
        // runtime points at a live CPU for the duration of the call.
        unsafe { (self.enter)(&mut *self.runtime, entry.unit) };
        ACTIVE.set(previous);
        self.runtime.cpu = std::ptr::null_mut();
        let exit = Exit::from_raw(self.runtime.exit)
            .ok_or_else(|| format!("unknown a64 exit {}", self.runtime.exit))?;
        let mut retired = budget - self.runtime.countdown;
        if exit == Exit::Fault {
            let site = self.faults.sites[&self.faults.pc.get()];
            recover(cpu, site, self.runtime.temps);
            retired += u64::from(site.retired);
        }
        if exit == Exit::Dispatch && self.runtime.link != 0 {
            let before = self.cache.used();
            self.link(memory, self.runtime.link as usize, cpu.eip)?;
            translated += self.cache.used().saturating_sub(before);
        }
        let retired = retired as u32;
        Ok(Some(Run {
            exit,
            retired,
            translated,
        }))
    }

    /// Patches the link point at `point` into a branch to the unit at
    /// `eip`, translating it if needed. Stops are never linked: the owner
    /// must see them.
    fn link(&mut self, memory: &mut Memory, point: usize, eip: u32) -> Result<(), String> {
        if self.stops.contains(&eip) {
            return Ok(());
        }
        let generation = self.generation;
        let entry = match self.units.get(&eip) {
            Some(&entry) => entry,
            None => self.translate(memory, eip)?,
        };
        if self.generation != generation {
            // Translating cleared the cache and the link point with it.
            return Ok(());
        }
        self.cache
            .patch(point, b((entry.unit as i64 - point as i64) as i32));
        self.links.entry(eip).or_default().push(point);
        Ok(())
    }

    /// Drops units whose guest code changed, and restores the link points
    /// that branch to them.
    fn invalidate(&mut self, memory: &mut Memory) {
        if memory.dirty_pages.is_empty() {
            return;
        }
        for page in std::mem::take(&mut memory.dirty_pages).into_keys() {
            for eip in self.pages.remove(&page).unwrap_or_default() {
                self.units.remove(&eip);
                for point in self.links.remove(&eip).unwrap_or_default() {
                    self.cache
                        .patch(point, movz(W, Reg::new(0), Exit::Dispatch as u16, 0));
                }
            }
        }
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
        self.generation += 1;
        self.faults.sites.clear();
    }

    fn translate(&mut self, memory: &mut Memory, eip: u32) -> Result<Entry, String> {
        if let Some(entry) = self.emit(memory, eip) {
            return Ok(entry);
        }
        self.clear();
        self.emit(memory, eip)
            .ok_or_else(|| "a64 unit does not fit an empty cache".into())
    }

    fn emit(&mut self, memory: &mut Memory, eip: u32) -> Option<Entry> {
        let at = self.cache.next();
        let unit = translate::translate(memory, eip, &self.stops, self.features, at, self.exit);
        let entry = self.place(eip, &unit.words, &unit.sites, unit.retired)?;
        debug_assert_eq!(entry.unit, at);
        let mut pages: Vec<u32> = unit
            .code
            .iter()
            .flat_map(|&(start, len)| [start & !0xfff, (start + len - 1) & !0xfff])
            .collect();
        pages.sort_unstable();
        pages.dedup();
        for page in pages {
            self.pages.entry(page).or_default().push(eip);
        }
        Some(entry)
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
        for &(offset, site) in sites {
            self.faults.sites.insert(unit + offset, site);
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
    if site.sense == Sense::Addition {
        cpu.eflags ^= 1;
    }
    let regs = cpu.regs;
    let value = |operand| match operand {
        Operand::Guest(index) => regs[usize::from(index)],
        Operand::Temp(index) => temps[usize::from(index)] as u32,
        Operand::Imm(value) => value,
    };
    if let Some((parity, adjust)) = site.recipe.evaluate(value) {
        let pf = u32::from((parity as u8).count_ones().is_multiple_of(2));
        cpu.eflags = cpu.eflags & !0x14 | pf << 2 | adjust & 0x10;
    }
}

#[cfg(test)]
mod tests {
    use adv_cpu_core::memory::Memory;

    use super::*;
    use crate::encode::Size::X;
    use crate::encode::{ldr, ldr_reg, ldur, movk, sub_imm};
    use crate::translate::Recipe;

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
        let site = Site {
            eip,
            retired: 0,
            sense: Sense::Subtraction,
            recipe: Recipe::Canonical,
        };
        let words = [
            load,
            sub_imm(X, Reg::new(9), Reg::new(9), 1, false),
            movz(W, Reg::new(0), Exit::Dispatch as u16, 0),
            movz(W, Reg::new(1), next as u16, 0),
            movk(W, Reg::new(1), (next >> 16) as u16, 16),
            movz(X, Reg::new(2), 0, 0),
            b((native.exit as i64 - (at + 24) as i64) as i32),
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
                retired: 3,
                sense: Sense::Addition,
                recipe: Recipe::AddResult(Operand::Guest(0), Operand::Imm(1)),
            },
            [0; 2],
        );
        assert_eq!(cpu.eip, 0x1234);
        assert_eq!(cpu.eflags, 0x202 | 0x10);
    }
}
