//! The access emitter (A64-MEM-7): integer and SIMD code describe each guest data
//! access, and this picks its form. An unchecked access records a site and
//! uses the window or a biased stack register. A checked one tests the page
//! class inline and leaves the line only for pages the window cannot take;
//! a refused access faults as a host fault at its site would, so dirty SIMD
//! registers need no publication first.

use crate::asm::{Asm, Label};
use crate::encode::Shift::Lsl;
use crate::encode::Size::{W, X};
use crate::encode::*;

use super::{RUNTIME, X3, X4};
use crate::runtime::{self, ACCESS_SIGNED, ACCESS_STORE};
use adv_cpu_core::memory::{EXEC, READ, SHARED, WRITE};

pub(in crate::translate) const PAGE_SHIFT: u32 = 12;

/// An address operand ready for a load or store.
#[derive(Clone, Copy)]
pub(in crate::translate) enum Addr {
    /// A W register holding the guest address relative to the data view.
    Window(Reg),
    /// A biased stack register plus an offset the guards cover.
    Biased(Reg, i32),
    /// A proved loop invariant in an unused high SIMD lane.
    #[cfg(native)]
    FrameCache(u8),
    /// The slot below ESP, which a store pushes to.
    Push,
    /// The slot at ESP, which a load pops from.
    Pop,
}

/// What an access does with its register.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::translate) enum Dir {
    /// Zero-extends the bytes read.
    Load,
    /// Sign-extends the bytes read.
    Signed,
    Store,
}

/// The register a checked access transfers through the stub.
#[derive(Clone, Copy)]
pub(in crate::translate) enum Value {
    Scalar(Reg),
    Vector(VMem, VReg),
}

impl Dir {
    /// The `access` kind bits of a `width`-byte access.
    pub(in crate::translate) fn kind(self, width: u8) -> u32 {
        u32::from(width)
            | match self {
                Self::Load => 0,
                Self::Signed => ACCESS_SIGNED,
                Self::Store => ACCESS_STORE,
            }
    }

    /// A `width`-byte access of `rt` through the selected data view.
    pub(in crate::translate) fn window(self, width: u8, rt: Reg, base: Reg, address: Reg) -> u32 {
        let narrow = |half| match (half, self) {
            (false, Self::Signed) => Narrow::SignedByte,
            (false, _) => Narrow::Byte,
            (true, Self::Signed) => Narrow::SignedHalf,
            (true, _) => Narrow::Half,
        };
        match (width, self) {
            (1 | 2, Self::Store) => str_narrow(narrow(width == 2), rt, base, address),
            (1 | 2, _) => ldr_narrow(narrow(width == 2), rt, base, address),
            (4, Self::Store) => str_reg(W, rt, base, address, false),
            (4, _) => ldr_reg(W, rt, base, address, false),
            _ => unreachable!("no {width}-byte integer access"),
        }
    }
}

pub(in crate::translate) trait CheckedEmitter {
    fn access_asm(&mut self) -> &mut Asm;
    fn access_site(&mut self);

    /// Emit one scalar or vector access after the same window/alias checks.
    /// The caller owns the slow path.
    fn checked_memory(
        &mut self,
        address: Reg,
        width: u32,
        store: bool,
        direct: u32,
        slow: Label,
        crossing: Label,
    ) -> (Label, Label) {
        let alias = self.access_asm().label();
        page_crossing(self.access_asm(), address, width, crossing);
        check_class(
            self.access_asm(),
            address,
            runtime::CLASSES,
            store,
            alias,
            slow,
        );
        let (site, resume) = (self.access_asm().label(), self.access_asm().label());
        self.access_asm().bind(site);
        self.access_site();
        self.access_asm().emit(direct);
        self.access_asm().b(resume);
        self.access_asm().bind(alias);
        // The window class already granted the guest R/W bit, so this index
        // is inside Memory's raw table even when its base is not zero.
        checked_alias(self.access_asm(), address, store, false, slow);
        self.access_asm().emit(ldr(X, X3, RUNTIME, runtime::ALIAS));
        self.access_site();
        // Keep the access width and extension; only replace its base register.
        self.access_asm()
            .emit((direct & !(31 << 5)) | (X3.number() << 5));
        self.access_asm().bind(resume);
        (site, resume)
    }
}

/// Branch when the last byte enters another page, preserving NZCV.
pub(in crate::translate) fn page_crossing(
    asm: &mut Asm,
    address: Reg,
    width: u32,
    crossing: Label,
) {
    if width > 1 {
        asm.emit_all(&[
            add_imm(W, X4, address, width - 1, false),
            eor(W, X4, X4, address, Lsl, 0),
        ]);
        asm.tbz(true, X4, PAGE_SHIFT, crossing);
    }
}

/// Verify the alias class after the window class granted the guest
/// permission. A store to a writable page that is not shared takes the
/// alias unless its line or the next holds watched code (A64-CTL-10):
/// the raw class marks a guest RWX page, the derived class a watched
/// data page. An access is at most 64 bytes; with `page` the caller
/// keeps the alias for the rest of the page, which then holds no code.
pub(in crate::translate) fn checked_alias(
    asm: &mut Asm,
    address: Reg,
    store: bool,
    page: bool,
    slow: Label,
) {
    if !store {
        check_class(asm, address, runtime::ALIAS_CLASSES, false, slow, slow);
        return;
    }
    let (lines, done) = (asm.label(), asm.label());
    let bit = |permission: u8| permission.trailing_zeros();
    let class = |table| {
        [
            ldr(X, X3, RUNTIME, table),
            lsr_imm(W, X4, address, PAGE_SHIFT),
            ldr_narrow(Narrow::Byte, X4, X3, X4),
        ]
    };
    asm.emit_all(&class(runtime::ALIAS_CLASSES));
    asm.tbz(false, X4, bit(WRITE), slow);
    asm.tbz(true, X4, bit(SHARED), slow);
    asm.tbz(true, X4, bit(EXEC), lines);
    asm.emit_all(&class(runtime::BODY_CLASSES));
    asm.tbz(false, X4, bit(EXEC), done);
    asm.bind(lines);
    asm.emit_all(&[
        ldr(X, X3, RUNTIME, runtime::CODE_LINES),
        lsr_imm(W, X4, address, PAGE_SHIFT),
        ldr_reg(X, X3, X3, X4, true),
    ]);
    if !page {
        asm.emit_all(&[
            lsr_imm(W, X4, address, 6),
            shiftv(X, Shift::Lsr, X3, X3, X4),
            and_imm(X, X3, X3, 3),
        ]);
    }
    asm.cbz(X, true, X3, slow);
    asm.bind(done);
}

/// Test guest permission and excluded classes with the same scratch and NZCV contract.
pub(in crate::translate) fn check_class(
    asm: &mut Asm,
    address: Reg,
    table: u32,
    store: bool,
    excluded: Label,
    slow: Label,
) {
    asm.emit_all(&[
        ldr(X, X3, RUNTIME, table),
        lsr_imm(W, X4, address, PAGE_SHIFT),
        ldr_narrow(Narrow::Byte, X4, X3, X4),
    ]);
    // Permitted, not shared, and for a store not a code barrier.
    let bit = |permission: u8| permission.trailing_zeros();
    let (needed, blocked) = if store {
        (WRITE, &[EXEC, SHARED][..])
    } else {
        (READ, &[SHARED][..])
    };
    asm.tbz(false, X4, bit(needed), slow);
    for &permission in blocked {
        asm.tbz(true, X4, bit(permission), excluded);
    }
}

/// Record writes to GPR bytes whose latest writer may be in another block.
pub(in crate::translate) fn scope_written(asm: &mut Asm, runtime: Reg, a: Reg, b: Reg, mask: u32) {
    let offset = runtime::SCOPE_GPR_WRITTEN;
    if mask == u32::MAX {
        asm.emit(mvn(W, a, Reg::new(31)));
    } else {
        asm.emit(ldr(W, a, runtime, offset));
        if bitmask(W, u64::from(mask)).is_some() {
            asm.emit(orr_imm(W, a, a, u64::from(mask)));
        } else {
            asm.emit(movz(W, b, mask as u16, 0));
            if mask >> 16 != 0 {
                asm.emit(movk(W, b, (mask >> 16) as u16, 16));
            }
            asm.emit(orr(W, a, a, b, Lsl, 0));
        }
    }
    asm.emit(str(W, a, runtime, offset));
}

/// A proved, fully written dword redirected only in a scoped worker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ScopeCell {
    pub base: u8,
    pub offset: i32,
}

#[derive(Clone, Debug)]
pub(crate) struct ScopeFrame {
    pub cells: Vec<ScopeCell>,
    pub access: Vec<Option<u8>>,
    pub mmx: u16,
    pub gpr: u32,
    pub gpr_writes: Vec<u32>,
    pub latch: (usize, bool),
}

#[derive(Clone)]
pub(crate) struct FrameCache {
    pub head: usize,
    /// Byte displacements from ESP on entry to the loop.
    pub offsets: Vec<i32>,
    pub(crate) reads: Vec<Option<u8>>,
}

impl FrameCache {
    pub(crate) fn read(&self, index: usize) -> Option<u8> {
        self.reads[index]
    }
}
