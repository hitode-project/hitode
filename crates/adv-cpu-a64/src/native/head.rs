//! The entry block every link to an admitted computation head reaches
//! (A64-OPT-8). It decides in Tier-1 state what `Native::run` decides
//! for an arrival at the head, and enters the inline form when the owner
//! would run it in place.

use crate::asm::{Asm, Label};
use crate::encode::Shift::Lsl;
use crate::encode::Size::{W, X};
use crate::encode::*;
use crate::runtime::{self, Exit};
use crate::translate::computation::{Proof, emit_guard};
use crate::translate::opt::back::Lowered;

use super::worker::Slot;

const fn r(n: u8) -> Reg {
    Reg::new(n)
}

const RUNTIME: Reg = r(29);
const GATE: Reg = r(16);
const SLOT: Reg = r(15);

/// Where the block continues outside itself, by absolute address.
pub(super) struct Targets {
    /// The ordinary Tier-1 unit at the head.
    pub unit: usize,
    pub exit: usize,
    pub dispatch: usize,
    pub opt_exit: usize,
}

/// The block followed by the inline form, placed for code at `at`, and
/// the byte offset of the block's entry.
pub(super) fn emit(
    proof: &Proof,
    head: u32,
    slot: &Slot,
    form: &mut Lowered,
    at: usize,
    targets: &Targets,
) -> (Vec<u32>, usize) {
    let mut asm = Asm::new();
    let labels = loop {
        let labels = block(&mut asm, proof, head, slot, form.spill_cells);
        if !asm.widen_bit_tests() {
            break labels;
        }
        asm.rewind();
    };
    let address = |label| at + asm.position(label);
    let entry = asm.position(labels.entry);
    let far = [
        (labels.unit, targets.unit),
        (labels.exit, targets.exit),
        (labels.opt_exit, targets.opt_exit),
    ]
    .map(|(label, target)| (asm.position(label) / 4, address(label), target));
    let (miss, leave, exit_tail) = (
        address(labels.miss),
        address(labels.leave),
        address(labels.exit_tail),
    );
    let form_at = address(labels.form);
    let mut words = asm.finish();
    for (index, from, to) in far {
        words[index] = branch(from, to);
    }
    form.place(form_at, exit_tail);
    form.words[form.miss] = branch(form_at + form.miss * 4, miss);
    form.words[form.cold] = branch(form_at + form.cold * 4, leave);
    for &(offset, _) in &form.links {
        // The link point's fourth word continues at the dispatch stub.
        let word = offset / 4 + 3;
        form.words[word] = branch(form_at + word * 4, targets.dispatch);
    }
    words.extend_from_slice(&form.words);
    (words, entry)
}

fn branch(from: usize, to: usize) -> u32 {
    b((to as i64 - from as i64) as i32)
}

struct Labels {
    entry: Label,
    form: Label,
    miss: Label,
    leave: Label,
    exit_tail: Label,
    /// Words that branch to `Targets`.
    unit: Label,
    exit: Label,
    opt_exit: Label,
}

/// Emits the block's cold paths and then its entry, which falls through
/// into the form. Tier-1 state is canonical on every path but the form's:
/// NZCV, x9 to x12 and the guest registers are live, and x0 to x8 and x13
/// to x17 are free.
fn block(asm: &mut Asm, proof: &Proof, head: u32, slot: &Slot, cells: usize) -> Labels {
    let labels = Labels {
        entry: asm.label(),
        form: asm.label(),
        miss: asm.label(),
        leave: asm.label(),
        exit_tail: asm.label(),
        unit: asm.label(),
        exit: asm.label(),
        opt_exit: asm.label(),
    };
    let slot = std::ptr::from_ref(slot) as u64;

    // The ordinary unit runs what admission or the form's facts reject.
    asm.bind(labels.unit);
    asm.emit(0);

    // A miss Rust would act on goes to Rust uncounted.
    asm.bind(labels.miss);
    constant(asm, SLOT, slot);
    asm.emit_all(&[
        ldp(W, Index::Offset, r(0), r(1), SLOT, Slot::MISSES as i32),
        sub(W, r(2), r(0), r(1), Lsl, 0),
    ]);
    asm.tbz(false, r(2), 31, labels.leave);
    asm.emit_all(&[
        add_imm(W, r(0), r(0), 1, false),
        str(W, r(0), SLOT, Slot::MISSES),
    ]);
    asm.b(labels.unit);

    // Rust decides the arrival as `Native::run` does.
    asm.bind(labels.leave);
    asm.emit_all(&[
        movz(W, r(0), Exit::Dispatch as u16, 0),
        movz(W, r(1), head as u16, 0),
        movk(W, r(1), (head >> 16) as u16, 16),
    ]);
    asm.bind(labels.exit);
    asm.emit(0);

    // Cold form exits name the form for the owner's materializer. x16 and
    // x17 hold no form value (A64-OPT-7).
    asm.bind(labels.exit_tail);
    constant(asm, r(16), cells as u64);
    asm.emit_all(&[
        str(X, r(16), RUNTIME, runtime::OPT_CELLS),
        movz(W, r(16), head as u16, 0),
        movk(W, r(16), (head >> 16) as u16, 16),
        str(W, r(16), RUNTIME, runtime::OPT_HEAD),
    ]);
    asm.bind(labels.opt_exit);
    asm.emit(0);

    asm.bind(labels.entry);
    // An offer decision Rust has not cleared goes to Rust (PROC-MACH-13).
    asm.emit_all(&[
        ldr(X, GATE, RUNTIME, runtime::OFFER_GATE),
        ldr(W, r(17), GATE, 0),
    ]);
    asm.cbz(W, true, r(17), labels.leave);
    constant(asm, SLOT, slot);
    if proof.splits() {
        // Rust may split a loop of `cost` ticks once the frame cannot
        // afford it: `split_over`, or `now + cost > split_deadline`.
        let affordable = asm.label();
        asm.emit(ldr(X, r(17), SLOT, Slot::SPLIT_TICKS));
        asm.cbz(X, false, r(17), affordable);
        asm.emit(ldr(W, r(0), GATE, 4));
        asm.cbz(W, true, r(0), labels.leave);
        asm.emit_all(&[
            mrs_nzcv(r(1)),
            mrs_cntvct(r(0)),
            add(X, r(0), r(0), r(17), Lsl, 0),
            ldr(X, r(2), GATE, 8),
            cmp(X, r(0), r(2), Lsl, 0),
            cset(W, r(3), Cond::Hi),
            msr_nzcv(r(1)),
        ]);
        asm.cbz(W, true, r(3), labels.leave);
        asm.bind(affordable);
    }
    // The arrival Rust counts as one that nominates a caller goes to Rust
    // uncounted.
    asm.emit_all(&[
        ldp(W, Index::Offset, r(0), r(1), SLOT, Slot::RUNS as i32),
        add_imm(W, r(0), r(0), 1, false),
        sub(W, r(1), r(1), r(0), Lsl, 0),
    ]);
    asm.cbz(W, false, r(1), labels.leave);
    asm.emit(str(W, r(0), SLOT, Slot::RUNS));
    emit_guard(asm, proof, labels.unit, labels.leave);
    asm.bind(labels.form);
    labels
}

/// `movz`/`movk` of a 64-bit value into `rd`.
fn constant(asm: &mut Asm, rd: Reg, value: u64) {
    asm.emit(movz(X, rd, value as u16, 0));
    for shift in [16, 32, 48] {
        let part = (value >> shift) as u16;
        if part != 0 {
            asm.emit(movk(X, rd, part, shift));
        }
    }
}
