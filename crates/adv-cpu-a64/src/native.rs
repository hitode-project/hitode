//! Translated execution on AArch64 hosts.
//!
//! M0 translates every unit to a single exit that interprets the instruction
//! at its EIP, which exercises the cache, the stubs and the dispatch loop
//! end to end. M1 replaces the unit bodies.
//!
//! Faults (§6.3, §6.4): every host instruction that may fault has a side-table
//! site naming its guest instruction. The signal handler only checks that the
//! PC is such a site, records it, and resumes at the fault stub. Because a
//! translated instruction commits nothing before its last possible fault
//! (§5.2), the owner then interprets that one instruction from the exact prior
//! state: the interpreter either completes it (a soft page) or reports the
//! precise guest fault.

use std::cell::Cell;
use std::collections::HashMap;

use adv_cpu_core::CpuState;

use crate::cache::CodeCache;
use crate::encode::Size::W;
use crate::encode::{Reg, b, movk, movz};
use crate::runtime::{self, Exit, Runtime};
use crate::window::Window;

const CACHE_LEN: usize = 64 << 20;

type Enter = unsafe extern "C" fn(*mut Runtime, usize);

/// What the signal handler may read while translated code runs.
struct Faults {
    /// Executable range of translated code.
    start: usize,
    end: Cell<usize>,
    /// Host PC of each site to its guest EIP.
    sites: HashMap<usize, u32>,
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

pub(crate) struct Native {
    cache: CodeCache,
    enter: Enter,
    exit: usize,
    /// Cache bytes the stubs occupy; clearing keeps them.
    stub_len: usize,
    units: HashMap<u32, usize>,
    faults: Faults,
    runtime: Box<Runtime>,
    /// Owns the reservation `runtime.window` points into. Guest memory moves
    /// into it in M1; until then translated code only uses its base to bias
    /// ESP and EBP.
    #[cfg_attr(not(test), expect(dead_code, reason = "held for its reservation"))]
    window: Window,
}

impl Native {
    pub fn new() -> Result<Self, String> {
        adv_cpu_core::fault_recovery::install_translated(on_fault)?;
        let mut cache = CodeCache::new(CACHE_LEN)?;
        let stubs = runtime::stubs();
        let base = cache.append(&stubs.code).ok_or("stubs do not fit")?;
        // SAFETY: `enter` follows the C calling convention with the
        // signature of `Enter`; see `runtime::emit_enter`.
        let enter = unsafe { std::mem::transmute::<usize, Enter>(base + stubs.enter) };
        let window = Window::new()?;
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
            runtime: Box::new(Runtime::new(std::ptr::null_mut(), window.base() as u64)),
            window,
        })
    }

    /// Runs translated code from `cpu.eip` until it exits. The second value
    /// is the number of bytes translated on the way, zero on a cache hit.
    /// A fault returns `Exit::Interpret` with EIP at the faulting instruction.
    pub fn run(&mut self, cpu: &mut CpuState) -> Result<(Exit, usize), String> {
        let (unit, translated) = match self.units.get(&cpu.eip) {
            Some(&unit) => (unit, 0),
            None => {
                let before = self.cache.used();
                let unit = self.translate(cpu.eip)?;
                (unit, self.cache.used().saturating_sub(before))
            }
        };
        debug_assert_eq!(cpu.lazy_flags[3], 0, "lazy flags settle before entry");
        self.faults.end.set(self.cache.next());
        self.runtime.cpu = cpu;
        let previous = ACTIVE.replace(&self.faults);
        // SAFETY: `unit` is translated code ending at the exit stub, and the
        // runtime points at a live CPU for the duration of the call.
        unsafe { (self.enter)(&mut *self.runtime, unit) };
        ACTIVE.set(previous);
        self.runtime.cpu = std::ptr::null_mut();
        let exit = Exit::from_raw(self.runtime.exit)
            .ok_or_else(|| format!("unknown a64 exit {}", self.runtime.exit))?;
        if exit == Exit::Fault {
            cpu.eip = self.faults.sites[&self.faults.pc.get()];
            return Ok((Exit::Interpret, translated));
        }
        Ok((exit, translated))
    }

    fn translate(&mut self, eip: u32) -> Result<usize, String> {
        if let Some(unit) = self.emit(eip) {
            return Ok(unit);
        }
        // A full cache is cleared as a whole (§7.1).
        self.cache.truncate(self.stub_len);
        self.units.clear();
        self.faults.sites.clear();
        self.emit(eip)
            .ok_or_else(|| "a64 unit does not fit an empty cache".into())
    }

    fn emit(&mut self, eip: u32) -> Option<usize> {
        let words = self.leave(Exit::Interpret, eip, self.cache.next());
        self.append(eip, &words, &[])
    }

    /// Words that leave through the exit stub with `exit` and `eip`, placed
    /// at `at`.
    fn leave(&self, exit: Exit, eip: u32, at: usize) -> [u32; 4] {
        let branch = at + 12;
        [
            movz(W, Reg::new(0), exit as u16, 0),
            movz(W, Reg::new(1), eip as u16, 0),
            movk(W, Reg::new(1), (eip >> 16) as u16, 16),
            b((self.exit as i64 - branch as i64) as i32),
        ]
    }

    /// Appends a unit for `eip` whose `sites` are (byte offset, guest EIP).
    fn append(&mut self, eip: u32, words: &[u32], sites: &[(usize, u32)]) -> Option<usize> {
        let unit = self.cache.append(words)?;
        for &(offset, site) in sites {
            self.faults.sites.insert(unit + offset, site);
        }
        self.units.insert(eip, unit);
        Some(unit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encode::{ldr, ldr_reg, ldur};
    use crate::window::GUARD;

    const X19: Reg = Reg::new(19);
    const X23: Reg = Reg::new(23);
    const X27: Reg = Reg::new(27);

    /// A unit at `eip` that loads EAX with `load` (a site of `eip`) and then
    /// dispatches to `eip + 3`.
    fn install(native: &mut Native, eip: u32, load: u32) {
        let at = native.cache.next();
        let mut words = vec![load];
        words.extend(native.leave(Exit::Dispatch, eip + 3, at + 4));
        native.append(eip, &words, &[(0, eip)]).unwrap();
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
        let native = Native::new().unwrap();
        let base = native.window.base();
        assert_eq!(base % (1 << 32), 0);
        assert!(base >= GUARD);
        assert_eq!(native.runtime.window, base as u64);
    }

    #[test]
    fn a_committed_window_page_loads_directly() {
        let mut native = Native::new().unwrap();
        let page = native.window.base() + 0x2_0000;
        // SAFETY: the page lies inside the window this test owns.
        unsafe {
            let mapped = libc::mmap(
                page as *mut _,
                0x4000,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANON | libc::MAP_FIXED,
                -1,
                0,
            );
            assert_eq!(mapped as usize, page);
            (page as *mut u32).add(1).write(0x1234_5678);
        }
        install(
            &mut native,
            0x1000,
            ldr_reg(W, X19, X27, Reg::new(20), false), // mov eax, [ecx]
        );
        let mut cpu = cpu(0x1000, 0x8000, 0x2_0004);
        assert_eq!(native.run(&mut cpu).unwrap().0, Exit::Dispatch);
        assert_eq!(cpu.regs[0], 0x1234_5678);
        assert_eq!(cpu.eip, 0x1003);
    }

    #[test]
    fn faults_in_guards_and_inaccessible_pages_resume_at_the_site() {
        let mut native = Native::new().unwrap();
        // push/pop past 0 and past 4 GiB, and an uncommitted window page.
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
        ] {
            let mut cpu = cpu(eip, esp, ecx);
            let before = cpu.clone();
            let (exit, _) = native.run(&mut cpu).unwrap();
            assert_eq!(exit, Exit::Interpret, "{eip:#x}");
            assert_eq!(
                cpu, before,
                "{eip:#x}: state is exactly that before the instruction"
            );
        }
    }
}
