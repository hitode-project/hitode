//! Resident entry, version misses and recovery tails (A64-OPT-8).

use std::cell::Cell;

use adv_cpu_core::CpuState;

use crate::asm::{Asm, Label};
use crate::encode::Shift::Lsl;
use crate::encode::Size::{W, X};
use crate::encode::*;
use crate::runtime::{self, Exit};
use crate::translate::opt::{back::Lowered, front::Assumed, ir::Source, recovery::*};

use super::plan::{MISSES, PROBATION, SAMPLE};

/// These counters are private to one version and written only by the owner.
#[derive(Default)]
#[repr(C)]
pub(super) struct Slot {
    pub misses: Cell<u32>,
    pub limit: Cell<u32>,
    pub env: [Cell<u32>; 8],
    pub entries: Cell<u64>,
    /// Snapshot followed by total retirement, as read by the lowered calls.
    pub countdown: [Cell<u64>; 2],
}

pub(super) struct Version {
    pub entry: usize,
    pub probe: usize,
    pub next: usize,
    pub at: CpuState,
    pub assumed: Assumed,
    pub sources: Vec<Source>,
    pub plain_sites: Vec<u32>,
    pub records: Vec<Record<Host>>,
    pub flags: Vec<Option<FlagRecipe>>,
    pub slot: Box<Slot>,
    pub probation: Option<Probation>,
    pub iteration_retired: u64,
    pub accounting: Vec<(usize, u32)>,
    pub batch_caches: Vec<crate::translate::opt::back::BatchCache>,
}

impl Version {
    pub fn iterations(&self, retired: u64) -> u64 {
        retired.checked_div(self.iteration_retired).unwrap_or(0)
    }

    pub fn account(&self, cache: &mut crate::cache::CodeCache, enabled: bool) {
        for &(point, call) in &self.accounting {
            cache.patch(point, if enabled { call } else { NOP });
        }
    }
}

pub(super) struct Probation {
    pub iteration_retired: u64,
    pub saved_words: u64,
    pub entry_exit_words: u64,
}

impl Probation {
    pub fn profitable(&self, slot: &Slot) -> bool {
        let iterations = slot.countdown[1].get() / slot.entries.get() / self.iteration_retired;
        iterations * self.saved_words >= self.entry_exit_words
    }
}

const fn r(n: u8) -> Reg {
    Reg::new(n)
}

const RT: Reg = r(29);
const GATE: Reg = r(16);
const SLOT: Reg = r(15);

pub(super) fn branch(from: usize, to: usize) -> u32 {
    b((to as i64 - from as i64) as i32)
}

pub(super) struct Targets {
    pub tier1: usize,
    pub exit: usize,
    pub dispatch: usize,
    pub opt_exit: usize,
}

pub(super) struct Entry {
    pub words: Vec<u32>,
    pub entry: usize,
    pub probe: usize,
    pub next: usize,
    pub form: usize,
}

/// Prefix and resident form, with the context-miss continuation patchable
/// independently of the Tier-1 fallback shared by the head's versions.
pub(super) fn emit(
    (head, version): (u32, usize),
    sample: &Cell<u32>,
    slot: &Slot,
    gate: Option<OfferEntry>,
    form: &mut Lowered,
    at: usize,
    targets: Targets,
) -> Entry {
    let mut asm = Asm::new();
    let leave = asm.label();
    let offer_exit = asm.label();
    let retry = asm.label();
    let miss = asm.label();
    let env = asm.label();
    let tail = asm.label();
    let probe = asm.label();
    let entry = asm.label();
    let account = asm.label();
    let snapshot = asm.label();
    let next = asm.label();
    let tier1 = asm.label();
    let exit = asm.label();
    let opt_exit = asm.label();

    if gate.is_some() {
        asm.bind(offer_exit);
        guest_exit(&mut asm, head, Exit::Computation);
        asm.b(exit);
    }

    asm.bind(next);
    asm.emit(0);
    asm.bind(tier1);
    asm.emit(0);

    asm.bind(miss);
    constant(&mut asm, SLOT, std::ptr::from_ref(slot) as u64);
    asm.emit_all(&[
        ldp(W, Index::Offset, r(0), r(1), SLOT, 0),
        add_imm(W, r(0), r(0), 1, false),
        str(W, r(0), SLOT, 0),
        sub(W, r(1), r(1), r(0), Lsl, 0),
    ]);
    asm.cbz(W, false, r(1), retry);
    asm.b(next);

    asm.bind(retry);
    asm.emit_all(&[
        movz(X, r(0), 1, 0),
        str(X, r(0), RT, runtime::RESIDENT_POINT),
    ]);
    asm.bind(leave);
    guest_exit(&mut asm, head, Exit::Dispatch);
    asm.bind(exit);
    asm.emit(0);

    asm.bind(env);
    constant(&mut asm, SLOT, slot.env.as_ptr() as u64);
    asm.emit_all(&[
        ldr(W, r(0), RT, runtime::RESIDENT_FACT),
        add_ext(X, SLOT, SLOT, r(0), Extend::Uxtw, 2),
        ldr(W, r(1), SLOT, 0),
        add_imm(W, r(1), r(1), 1, false),
        str(W, r(1), SLOT, 0),
        sub_imm(W, r(1), r(1), MISSES, false),
    ]);
    asm.tbz(true, r(1), 31, tier1);
    constant(
        &mut asm,
        r(1),
        (u64::from(head) << 32) | ((version as u64 + 1) << 8),
    );
    asm.emit_all(&[
        orr(X, r(1), r(1), r(0), Lsl, 0),
        str(X, r(1), RT, runtime::RESIDENT_ENV),
    ]);
    asm.b(tier1);

    // x16 and x17 hold no form values at a cold exit (A64-OPT-7).
    asm.bind(tail);
    constant(&mut asm, r(16), form.spill_cells as u64);
    asm.emit(str(X, r(16), RT, runtime::OPT_CELLS));
    constant(&mut asm, r(16), u64::from(head));
    asm.emit_all(&[
        str(W, r(16), RT, runtime::OPT_HEAD),
        movz(W, r(16), version as u16, 0),
        str(W, r(16), RT, runtime::RESIDENT_VERSION),
    ]);
    asm.bind(opt_exit);
    asm.emit(0);

    asm.bind(account);
    constant(&mut asm, r(16), std::ptr::from_ref(&slot.entries) as u64);
    asm.emit_all(&[
        ldr(X, r(17), r(16), 0),
        add_imm(X, r(17), r(17), 1, false),
        str(X, r(17), r(16), 0),
        sub_imm(X, r(17), r(17), PROBATION, false),
    ]);
    asm.tbz(true, r(17), 63, snapshot);
    constant(
        &mut asm,
        r(17),
        (u64::from(head) << 32) | (version as u64 + 1),
    );
    asm.emit(str(X, r(17), RT, runtime::RESIDENT_PROBATION));
    asm.bind(snapshot);
    asm.emit(0);

    asm.bind(probe);
    if version == 0 {
        constant(&mut asm, SLOT, std::ptr::from_ref(sample) as u64);
        asm.emit_all(&[
            ldr(W, r(0), SLOT, 0),
            sub_imm(W, r(0), r(0), 1, false),
            str(W, r(0), SLOT, 0),
        ]);
        asm.cbz(W, true, r(0), entry);
        asm.emit_all(&[
            movz(W, r(0), SAMPLE as u16, 0),
            str(W, r(0), SLOT, 0),
            str(X, r(14), RT, runtime::RESIDENT_POINT),
        ]);
        asm.b(leave);
    }
    asm.bind(entry);
    if let Some(gate) = gate {
        offer(&mut asm, gate, offer_exit);
    }

    let form_at = at + asm.offset();
    let address = |label| at + asm.position(label);
    let call = form.probation[0];
    let from = form_at + call * 4;
    let offset = ((form.words[call] << 6) as i32 >> 4) as isize;
    form.words[call] = bl((address(account) as i64 - from as i64) as i32);
    let pointer = slot.countdown.as_ptr() as u64;
    form.words[form.probation_slot] = pointer as u32;
    form.words[form.probation_slot + 1] = (pointer >> 32) as u32;
    let patches = [
        (address(next), targets.tier1),
        (address(tier1), targets.tier1),
        (address(exit), targets.exit),
        (address(opt_exit), targets.opt_exit),
        (address(snapshot), from.wrapping_add_signed(offset)),
    ];
    form.place(form_at, address(tail));
    form.words[form.miss] = branch(form_at + form.miss * 4, address(miss));
    // On the frozen back end the two labels share a word.
    if form.env_miss != form.miss {
        form.words[form.env_miss] = branch(form_at + form.env_miss * 4, address(env));
    }
    form.words[form.cold] = branch(form_at + form.cold * 4, address(tier1));
    for &(offset, _) in &form.links {
        let word = offset / 4 + 3;
        form.words[word] = branch(form_at + word * 4, targets.dispatch);
    }
    let (entry, probe, next) = (address(entry), address(probe), address(next));
    let mut words = asm.finish();
    for (from, to) in patches {
        words[(from - at) / 4] = branch(from, to);
    }
    words.extend_from_slice(&form.words);
    Entry {
        words,
        entry,
        probe,
        next,
        form: form_at,
    }
}

fn guest_exit(asm: &mut Asm, head: u32, exit: Exit) {
    asm.emit_all(&[
        movz(W, r(0), exit as u16, 0),
        movz(W, r(1), head as u16, 0),
        movk(W, r(1), (head >> 16) as u16, 16),
    ]);
}

pub(super) fn fallback(head: u32, at: usize, exit: usize) -> Vec<u32> {
    let mut asm = Asm::new();
    guest_exit(&mut asm, head, Exit::Dispatch);
    asm.emit(branch(at + asm.offset(), exit));
    asm.finish()
}

/// Stable plan cells read only by the owner.
#[derive(Clone, Copy)]
pub(super) struct OfferEntry {
    pub reoffer: u64,
    pub split_ticks: Option<u64>,
}

pub(super) fn offer_link(
    head: u32,
    at: usize,
    exit: usize,
    unit: Option<usize>,
    gate: OfferEntry,
) -> (Vec<u32>, usize) {
    let mut asm = Asm::new();
    let request = asm.label();
    offer(&mut asm, gate, request);
    let fallback = at + asm.offset();
    if let Some(unit) = unit {
        asm.emit(branch(fallback, unit));
    } else {
        asm.emit_all(&self::fallback(head, fallback, exit));
    }
    asm.bind(request);
    guest_exit(&mut asm, head, Exit::Computation);
    asm.emit(branch(at + asm.offset(), exit));
    (asm.finish(), fallback)
}

fn offer(asm: &mut Asm, gate: OfferEntry, leave: Label) {
    let due = asm.label();
    let proceed = asm.label();
    asm.emit_all(&[
        ldr(X, GATE, RT, runtime::OFFER_GATE),
        ldr(W, r(17), GATE, 0),
    ]);
    if let Some(split_ticks) = gate.split_ticks {
        asm.cbz(W, true, r(17), due);
        constant(asm, SLOT, split_ticks);
        asm.emit(ldr(X, r(17), SLOT, 0));
        asm.cbz(X, false, r(17), proceed);
        asm.emit(ldr(W, r(0), GATE, 4));
        asm.cbz(W, true, r(0), due);
        asm.emit_all(&[
            mrs_nzcv(r(1)),
            mrs_cntvct(r(0)),
            add(X, r(0), r(0), r(17), Lsl, 0),
            ldr(X, r(2), GATE, 8),
            cmp(X, r(0), r(2), Lsl, 0),
            cset(W, r(3), Cond::Hi),
            msr_nzcv(r(1)),
        ]);
        asm.cbz(W, false, r(3), proceed);
    } else {
        asm.cbz(W, false, r(17), proceed);
    }
    asm.bind(due);
    constant(asm, SLOT, gate.reoffer);
    asm.emit(ldr(W, r(0), SLOT, 0));
    asm.cbz(W, false, r(0), leave);
    asm.emit_all(&[sub_imm(W, r(0), r(0), 1, false), str(W, r(0), SLOT, 0)]);
    asm.bind(proceed);
}

fn constant(asm: &mut Asm, rd: Reg, value: u64) {
    asm.emit(movz(X, rd, value as u16, 0));
    for shift in [16, 32, 48] {
        let part = (value >> shift) as u16;
        if part != 0 {
            asm.emit(movk(X, rd, part, shift));
        }
    }
}
