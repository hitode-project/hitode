//! AArch64 emission shared by the Tier-1 and optimized lowerings (A64A-ROLE-4).
//!
//! These sequences use the fixed runtime ABI and leave allocation and guest
//! state tracking to the caller.

use crate::asm::{Asm, Label};
use crate::encode::Size::{W, X};
use crate::encode::{Reg, adr, b, movk, movz, mvn};
use crate::facts::Key;
use crate::runtime::{self, Exit};
use flags::{Carry, Operand, Overflow, Recipe, Sense};

pub(crate) mod access;
pub(crate) mod body;
pub(crate) mod flags;
pub(super) mod fp_class;
pub(crate) mod fp_state;
pub(crate) mod resident;
pub(crate) mod stubs;
pub(crate) mod x87_stubs;

pub use x87_stubs::X87Stubs;

pub(in crate::translate) const X0: Reg = Reg::new(0);
pub(in crate::translate) const X1: Reg = Reg::new(1);
pub(in crate::translate) const X2: Reg = Reg::new(2);
pub(in crate::translate) const X3: Reg = Reg::new(3);
pub(in crate::translate) const X4: Reg = Reg::new(4);
pub(in crate::translate) const X5: Reg = Reg::new(5);
pub(in crate::translate) const X9: Reg = Reg::new(9);
pub(in crate::translate) const X12: Reg = Reg::new(12);
pub(in crate::translate) const X16: Reg = Reg::new(16);
pub(in crate::translate) const WINDOW: Reg = Reg::new(27);
pub(in crate::translate) const RUNTIME: Reg = Reg::new(29);

/// What the owner needs at a host instruction that may fault.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Site {
    /// The guest instruction, which has committed nothing (A64-FLAG-4).
    pub eip: u32,
    /// Instructions the countdown prepaid that the unit has not completed,
    /// this one included.
    pub refund: u32,
    pub sense: Sense,
    pub carry: Carry,
    pub recipe: Recipe,
    /// A pending result supplies SF and ZF; otherwise NZCV holds them.
    pub(crate) nz: Option<Operand>,
    /// OF as hardware V or a recipe over recoverable operands.
    pub(crate) overflow: Overflow,
    /// Whether the access checks its page first, so a fault here is not
    /// a reason to check it.
    pub checked: bool,
    /// Unpublished MMX (bits 0–7) and XMM (bits 8–15) values in host SIMD.
    pub simd_dirty: u16,
    /// Where a fault here resumes, relative to the site, when x87 values
    /// cached in host registers must be published first; 0 for the fault
    /// stub itself.
    pub resume: i32,
    /// Optimized region record, when this access belongs to an optimized form.
    pub opt_record: Option<u32>,
}

/// Offsets of the body and the patchable miss branch in a checked unit.
#[derive(Clone, Copy, Default)]
pub(crate) struct VersionLayout {
    pub body: usize,
    pub miss: Option<usize>,
}

/// A link point of a unit: a branch to the owner's helper that may become
/// a direct branch to the unit at `eip`.
#[derive(Clone, Copy, Debug)]
pub struct Link {
    /// Byte offset of the point in the unit.
    pub offset: usize,
    pub eip: u32,
    /// Whether the trace's main path leaves through it: a real call, or
    /// the end of the trace, rather than a side exit.
    pub main: bool,
    pub back: bool,
    pub direct: Option<usize>,
}

/// A patchable internal edge and its out-of-line paths, in byte offsets.
pub(crate) struct InternalLink {
    pub eip: u32,
    pub point: usize,
    pub target: usize,
    pub transition: usize,
    pub link: usize,
    pub counter: Option<usize>,
    pub sensor: Option<usize>,
    pub source_pc: u32,
}

pub(crate) struct Tier1Placement<'a> {
    pub words: &'a [u32],
    pub sites: &'a [(usize, Site)],
    pub marks: &'a [crate::jit_map::Mark],
    pub retired: u32,
    pub version: VersionLayout,
    pub links: &'a [Link],
    pub internal_links: &'a [InternalLink],
    pub table_positions: &'a [(usize, usize)],
    pub uses_permissions: bool,
    pub far_overflow: bool,
}

/// Host addresses of the `runtime` stubs units branch to.
#[derive(Clone, Copy, Debug)]
pub struct Stubs {
    pub exit: usize,
    pub dispatch: usize,
    /// Continues at the guest EIP in w16 through the indirect table.
    pub lookup: usize,
    /// `lookup` after its probe missed.
    pub lookup_slow: usize,
    /// A return whose shadow entry did not match.
    pub ret_miss: usize,
    /// A checked access the window cannot take.
    pub access: usize,
    /// An INT 0x2e handler call.
    pub call: usize,
    /// Where a faulting site leaves.
    pub fault: usize,
    /// Where an optimized form leaves through a record.
    pub opt_exit: usize,
    /// Shared cold helpers for Tier-1 x87 runs.
    pub x87: Option<X87Stubs>,
}

/// The backward link's patch word precedes the ordinary dispatch prefix.
/// x14 identifies the edge on the resident trampoline's sampled path.
pub(crate) fn emit_link_point(asm: &mut crate::asm::Asm, eip: u32, back: bool) -> usize {
    if back {
        asm.emit(adr(Reg::new(14), 4));
    }
    let point = asm.offset();
    if back {
        asm.emit(b(4));
    }
    asm.emit_all(&[
        movz(W, Reg::new(1), eip as u16, 0),
        movk(W, Reg::new(1), (eip >> 16) as u16, 16),
        adr(Reg::new(2), point as i32 - asm.offset() as i32 - 8),
    ]);
    point
}

pub(in crate::translate) fn emit_link_transition(asm: &mut Asm, point: usize) -> usize {
    let transition = asm.offset();
    asm.emit(adr(Reg::new(14), point as i32 - transition as i32));
    asm.emit(b(0));
    transition
}

pub(in crate::translate) fn emit_target(asm: &mut Asm, eip: u32) {
    asm.emit_all(&[
        movz(W, Reg::new(1), eip as u16, 0),
        movk(W, Reg::new(1), (eip >> 16) as u16, 16),
    ]);
}

pub(crate) fn emit_back_count(asm: &mut Asm) {
    let counter = asm.label();
    let trigger = asm.label();
    runtime::emit_sensor(asm, counter, Reg::new(3), Reg::new(4), trigger);
    asm.emit(b(0));
    asm.bind(counter);
    asm.emit_all(&[0, 0]);
    asm.bind(trigger);
}

pub(super) fn emit_exit(
    asm: &mut Asm,
    far: &mut Vec<(usize, usize)>,
    exit: Exit,
    eip: u32,
    stub: usize,
) {
    asm.emit(movz(W, Reg::new(0), exit.word(), 0));
    emit_target(asm, eip);
    emit_far(asm, far, stub, b(0));
}

pub(super) fn emit_far(asm: &mut Asm, far: &mut Vec<(usize, usize)>, to: usize, word: u32) {
    far.push((asm.offset() / 4, to));
    asm.emit(word);
}

pub(super) fn emit_version_prologue(asm: &mut Asm, facts: Option<Key>) -> (usize, Option<Label>) {
    let miss = facts.filter(|key| key.mask() != 0).map(|key| {
        let label = asm.label();
        key.guard(asm, label);
        label
    });
    (asm.offset(), miss)
}

pub(super) fn finish_version(asm: &Asm, body: usize, miss: Option<Label>) -> VersionLayout {
    VersionLayout {
        body,
        miss: miss.map(|label| asm.position(label)),
    }
}

pub(super) fn emit_miss_slot(asm: &mut Asm, label: Label) {
    asm.bind(label);
    asm.emit(b(4));
}

pub(super) fn emit_version_miss(
    asm: &mut Asm,
    far: &mut Vec<(usize, usize)>,
    eip: u32,
    dispatch: usize,
) {
    emit_target(asm, eip);
    asm.emit(mvn(X, Reg::new(2), Reg::ZR));
    emit_far(asm, far, dispatch, b(0));
}
