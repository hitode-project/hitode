//! Private backing and frozen mapping proofs for the research runner.
use super::{Frame, Graph};
use crate::{Memory, READ, WRITE};
use std::collections::BTreeSet;

pub(super) fn prove(graph: &Graph, frame: &Frame, memory: &Memory) -> BTreeSet<u32> {
    graph
        .instructions
        .values()
        .filter_map(|i| {
            let address = graph.invariant_address(i, frame)?;
            let write = i.op0_kind() == iced_x86::OpKind::Memory
                && i.mnemonic() != iced_x86::Mnemonic::Cmp
                && i.mnemonic() != iced_x86::Mnemonic::Test;
            ordinary(memory, address, 4, write).then_some(i.ip32())
        })
        .collect()
}

pub(super) fn ordinary(memory: &Memory, address: u32, width: usize, write: bool) -> bool {
    let Some(offset) = address.checked_sub(memory.base).map(|n| n as usize) else {
        return false;
    };
    let Some(end) = offset.checked_add(width) else {
        return false;
    };
    if width == 0 || end > memory.len || u64::from(address) + width as u64 > 1 << 32 {
        return false;
    }
    let required = if write { READ | WRITE } else { READ };
    (offset / 4096..=(end - 1) / 4096).all(|p| {
        memory.mapped[p]
            && memory.perms[p] & required == required
            && memory.perms[p] & !(READ | WRITE) == 0
    })
}

/// Only this exclusive, serial borrow authorizes unchecked backing accesses.
/// No helper, callback, mapping mutation, external backing or owner handoff can
/// occur inside the generated region. This is not a production mapping lease.
pub(super) struct Lease<'a> {
    pub memory: &'a mut Memory,
    epoch: u64,
}
impl<'a> Lease<'a> {
    pub fn new(memory: &'a mut Memory) -> Self {
        assert!(!memory.is_parallel() && !memory.foreign_access_required());
        Self {
            epoch: memory.epoch,
            memory,
        }
    }
}
impl Drop for Lease<'_> {
    fn drop(&mut self) {
        assert_eq!(
            self.epoch, self.memory.epoch,
            "mapping changed inside GIR lease"
        );
    }
}

/// A hardware-protected, private mirror, used only by the backend experiment.
/// The mirror excludes code, file, device and shared pages. Copying is outside
/// the measured compilation interval; no runtime speed claim uses this runner.
pub(super) struct Window {
    storage: crate::virtual_memory::Reservation,
    pub len: usize,
    writable: Vec<usize>,
}

impl Window {
    pub fn new(memory: &Memory) -> Self {
        let storage = crate::virtual_memory::Reservation::new(memory.len + 16384).unwrap();
        let mut writable = Vec::new();
        for page in 0..memory.len / 4096 {
            let p = memory.perms[page];
            if !memory.mapped[page] || p & READ == 0 || p & !(READ | WRITE) != 0 {
                continue;
            }
            storage.commit(page * 4096, 4096).unwrap();
            // SAFETY: both allocations own the entire ordinary committed page.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    memory.ptr.as_ptr().add(page * 4096),
                    storage.ptr.as_ptr().add(page * 4096),
                    4096,
                )
            };
            if p & WRITE != 0 {
                writable.push(page);
            } else {
                protect(storage.ptr.as_ptr(), page * 4096, 4096, false);
            }
        }
        Self {
            storage,
            len: memory.len,
            writable,
        }
    }
    pub fn pointer(&self) -> *mut u8 {
        self.storage.ptr.as_ptr()
    }
    pub fn copy_back(&self, memory: &mut Memory) {
        for &page in &self.writable {
            // SAFETY: the mirror and original are exclusively borrowed and RW.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    self.pointer().add(page * 4096),
                    memory.ptr.as_ptr().add(page * 4096),
                    4096,
                )
            };
        }
    }
}

#[cfg(windows)]
unsafe extern "system" {
    fn VirtualProtect(
        address: *mut std::ffi::c_void,
        len: usize,
        protection: u32,
        old: *mut u32,
    ) -> i32;
}
fn protect(base: *mut u8, offset: usize, len: usize, write: bool) {
    #[cfg(windows)]
    {
        let mut old = 0;
        assert_ne!(
            unsafe {
                VirtualProtect(
                    base.add(offset).cast(),
                    len,
                    if write { 4 } else { 2 },
                    &mut old,
                )
            },
            0
        );
    }
    #[cfg(unix)]
    {
        assert_eq!(
            unsafe {
                libc::mprotect(
                    base.add(offset).cast(),
                    len,
                    libc::PROT_READ | if write { libc::PROT_WRITE } else { 0 },
                )
            },
            0
        );
    }
}
