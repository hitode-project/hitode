//! Versioned native entry slots. Only the owning execution thread publishes or
//! retires code, between native calls. Generated tail calls never own code pages.
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::CpuState;

pub(crate) const PROBES: usize = 8;
pub(crate) const CHAIN_LIMIT: u32 = 1024;

pub(crate) fn hash(pc: u32) -> u32 {
    let mixed = (pc ^ (pc >> 16)).wrapping_mul(0x9e37_79b1);
    mixed ^ (mixed >> 16)
}

#[repr(C)]
pub(crate) struct Slot {
    pub pc: u32,
    pub entry: AtomicUsize,
}

#[derive(Default)]
pub(crate) struct Table {
    slots: Vec<Slot>,
}

impl Table {
    pub fn rebuild(&mut self, entries: impl Iterator<Item = (u32, usize)>) {
        let entries: Vec<_> = entries.collect();
        if entries.is_empty() {
            self.slots.clear();
            return;
        }
        let mut capacity = (entries.len() * 4).next_power_of_two().max(256);
        loop {
            let mut slots: Vec<_> = (0..capacity)
                .map(|_| Slot {
                    pc: 0,
                    entry: AtomicUsize::new(0),
                })
                .collect();
            let mask = capacity - 1;
            let mut complete = true;
            for &(pc, entry) in &entries {
                let first = hash(pc) as usize & mask;
                let index = (0..PROBES)
                    .map(|offset| (first + offset) & mask)
                    .find(|&index| slots[index].entry.load(Ordering::Relaxed) == 0);
                let Some(index) = index else {
                    complete = false;
                    break;
                };
                slots[index].pc = pc;
                slots[index].entry.store(entry, Ordering::Release);
            }
            if complete {
                self.slots = slots;
                return;
            }
            capacity *= 2;
        }
    }

    pub fn parts(&self) -> (*const Slot, u32) {
        if self.slots.is_empty() {
            (std::ptr::null(), 0)
        } else {
            (self.slots.as_ptr(), (self.slots.len() - 1) as u32)
        }
    }
}

/// ABI used only by generated code. The C entry trampoline passes this context
/// and the guest memory base to a Tail-convention region. No Rust stack grows
/// when regions call one another. The table and code allocations outlive a chain.
#[repr(C)]
pub(crate) struct NativeContext {
    pub state: *mut CpuState,
    pub perms: *const u8,
    pub bridge: *mut std::ffi::c_void,
    pub slots: *const Slot,
    pub mask: u32,
    pub remaining: u32,
    pub stop: u32,
    pub return_pad: u64,
    pub linked_entries: u64,
}
