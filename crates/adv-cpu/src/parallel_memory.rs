//! Synchronization and foreign-memory accesses for bounded guest execution.
//!
//! Guest RAM is machine memory, not a Rust slice shared between workers. x64
//! accesses below use explicit instructions and never manufacture references to
//! bytes another guest CPU can mutate. Device PCM keeps its byte-atomic protocol.
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};

#[derive(Default)]
pub(crate) struct Fence {
    pub stop: AtomicBool,
    active: AtomicUsize,
    lock: Mutex<()>,
    completed: Condvar,
    pub waits: AtomicU64,
    pub wait_ns: AtomicU64,
    pub wait_max_ns: AtomicU64,
}

impl Fence {
    /// Only the controller can issue a new permit. While it borrows memory,
    /// it cannot dispatch another job, so an empty set permits a scoped borrow.
    pub fn is_idle(&self) -> bool {
        self.active.load(Ordering::Acquire) == 0
    }
    /// Called only by the controller, before handing a job to a worker.
    pub fn enter(self: &Arc<Self>) -> Permit {
        if self.stop.load(Ordering::Acquire) {
            assert_eq!(self.active.load(Ordering::Acquire), 0);
            self.stop.store(false, Ordering::Release);
        }
        self.active.fetch_add(1, Ordering::AcqRel);
        Permit(self.clone())
    }

    /// Controller-only barrier. Workers return through their native safepoints;
    /// their completion records retain CPU state until the machine consumes it.
    pub fn quiesce(&self) {
        self.stop.store(true, Ordering::Release);
        if self.is_idle() {
            std::sync::atomic::fence(Ordering::SeqCst);
            return;
        }
        let started = std::time::Instant::now();
        let mut guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        while self.active.load(Ordering::Acquire) != 0 {
            guard = self
                .completed
                .wait(guard)
                .unwrap_or_else(|e| e.into_inner());
        }
        std::sync::atomic::fence(Ordering::SeqCst);
        let elapsed = started.elapsed().as_nanos() as u64;
        self.waits.fetch_add(1, Ordering::Relaxed);
        self.wait_ns.fetch_add(elapsed, Ordering::Relaxed);
        self.wait_max_ns.fetch_max(elapsed, Ordering::Relaxed);
    }
}

pub(crate) struct Permit(Arc<Fence>);
impl Drop for Permit {
    fn drop(&mut self) {
        let _guard = self.0.lock.lock().unwrap_or_else(|e| e.into_inner());
        assert_ne!(self.0.active.fetch_sub(1, Ordering::AcqRel), 0);
        self.0.completed.notify_all();
    }
}

/// # Safety
/// The caller pins and validates the whole range. No Rust reference into guest
/// RAM may remain live. Alignment guarantees follow the actual x86 operand.
#[cfg(target_arch = "x86_64")]
#[inline]
pub(crate) unsafe fn load(pointer: *const u8, width: usize) -> u64 {
    let value: u64;
    unsafe {
        match width {
            1 => {
                core::arch::asm!("movzx {value:e}, byte ptr [{pointer}]", pointer=in(reg) pointer, value=out(reg) value, options(nostack, preserves_flags))
            }
            2 => {
                core::arch::asm!("movzx {value:e}, word ptr [{pointer}]", pointer=in(reg) pointer, value=out(reg) value, options(nostack, preserves_flags))
            }
            4 => {
                core::arch::asm!("mov {value:e}, dword ptr [{pointer}]", pointer=in(reg) pointer, value=out(reg) value, options(nostack, preserves_flags))
            }
            8 => {
                core::arch::asm!("mov {value}, qword ptr [{pointer}]", pointer=in(reg) pointer, value=out(reg) value, options(nostack, preserves_flags))
            }
            _ => unreachable!("invalid foreign load width"),
        }
    }
    value
}

/// # Safety
/// Same pinned, validated machine-memory contract as load; range must be writable.
#[cfg(target_arch = "x86_64")]
#[inline]
pub(crate) unsafe fn store(pointer: *mut u8, value: u64, width: usize) {
    unsafe {
        match width {
            1 => {
                core::arch::asm!("mov byte ptr [{pointer}], {value}", pointer=in(reg) pointer, value=in(reg_byte) value as u8, options(nostack, preserves_flags))
            }
            2 => {
                core::arch::asm!("mov word ptr [{pointer}], {value:x}", pointer=in(reg) pointer, value=in(reg) value, options(nostack, preserves_flags))
            }
            4 => {
                core::arch::asm!("mov dword ptr [{pointer}], {value:e}", pointer=in(reg) pointer, value=in(reg) value, options(nostack, preserves_flags))
            }
            8 => {
                core::arch::asm!("mov qword ptr [{pointer}], {value}", pointer=in(reg) pointer, value=in(reg) value, options(nostack, preserves_flags))
            }
            _ => unreachable!("invalid foreign store width"),
        }
    }
}

/// Foreign-to-private or private-to-foreign nonoverlapping copy. Short operands
/// retain their width (not four byte loads for an aligned guest DWORD).
/// # Safety
/// Both ranges are valid for len bytes, pinned, and do not overlap.
#[cfg(target_arch = "x86_64")]
#[inline]
pub(crate) unsafe fn copy(source: *const u8, destination: *mut u8, len: usize) {
    unsafe {
        if matches!(len, 1 | 2 | 4 | 8) {
            store(destination, load(source, len), len);
        } else {
            core::arch::asm!("rep movsb", inout("rsi") source => _, inout("rdi") destination => _, inout("rcx") len => _, options(nostack, preserves_flags));
        }
    }
}

/// # Safety
/// Validated, nonoverlapping forward REP operands. Retain guest element width.
#[cfg(target_arch = "x86_64")]
pub(crate) unsafe fn repeat(
    source: *const u8,
    destination: *mut u8,
    count: usize,
    width: usize,
    fill: Option<u32>,
) {
    unsafe {
        if let Some(value) = fill {
            match width {
                1 => {
                    core::arch::asm!("rep stosb", in("eax") value, inout("rdi") destination => _, inout("rcx") count => _, options(nostack, preserves_flags))
                }
                2 => {
                    core::arch::asm!("rep stosw", in("eax") value, inout("rdi") destination => _, inout("rcx") count => _, options(nostack, preserves_flags))
                }
                4 => {
                    core::arch::asm!("rep stosd", in("eax") value, inout("rdi") destination => _, inout("rcx") count => _, options(nostack, preserves_flags))
                }
                _ => unreachable!(),
            }
        } else {
            match width {
                1 => {
                    core::arch::asm!("rep movsb", inout("rsi") source => _, inout("rdi") destination => _, inout("rcx") count => _, options(nostack, preserves_flags))
                }
                2 => {
                    core::arch::asm!("rep movsw", inout("rsi") source => _, inout("rdi") destination => _, inout("rcx") count => _, options(nostack, preserves_flags))
                }
                4 => {
                    core::arch::asm!("rep movsd", inout("rsi") source => _, inout("rdi") destination => _, inout("rcx") count => _, options(nostack, preserves_flags))
                }
                _ => unreachable!(),
            }
        }
    }
}

#[cfg(not(target_arch = "x86_64"))]
pub(crate) unsafe fn repeat(_: *const u8, _: *mut u8, _: usize, _: usize, _: Option<u32>) {
    unreachable!("parallel memory requires x64")
}

// Parallel execution is rejected on other ISAs. These functions are used only
// by unreachable parallel branches there, avoiding implicit weaker semantics.
#[cfg(not(target_arch = "x86_64"))]
pub(crate) unsafe fn load(_: *const u8, _: usize) -> u64 {
    unreachable!("parallel memory requires x64")
}
#[cfg(not(target_arch = "x86_64"))]
pub(crate) unsafe fn store(_: *mut u8, _: u64, _: usize) {
    unreachable!("parallel memory requires x64")
}
#[cfg(not(target_arch = "x86_64"))]
pub(crate) unsafe fn copy(_: *const u8, _: *mut u8, _: usize) {
    unreachable!("parallel memory requires x64")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quiescence_waits_for_every_lease_and_can_be_reused() {
        let fence = Arc::new(Fence::default());
        let a = fence.enter();
        let b = fence.enter();
        let stopped = fence.clone();
        let waiter = std::thread::spawn(move || stopped.quiesce());
        drop(a);
        assert_eq!(fence.active.load(Ordering::Acquire), 1);
        drop(b);
        waiter.join().unwrap();
        let next = fence.enter();
        assert!(!fence.stop.load(Ordering::Acquire));
        drop(next);
        fence.quiesce();
    }

    #[test]
    #[cfg(target_arch = "x86_64")]
    fn foreign_copy_preserves_short_widths_and_unaligned_bytes() {
        let input = [0x82, 0x39, 0xa4, 0xe5, 0x21, 0x92, 0x58, 0xab, 0xcd];
        for len in 0..=8 {
            let mut output = [0; 12];
            unsafe { copy(input.as_ptr(), output.as_mut_ptr().add(1), len) };
            assert_eq!(&output[1..1 + len], &input[..len]);
            assert_eq!(output[0], 0);
            assert_eq!(output[1 + len], 0);
        }
    }
}
