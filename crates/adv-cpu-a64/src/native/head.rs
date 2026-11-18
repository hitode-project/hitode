//! Resident entry, version misses and recovery tails (A64-OPT-8).

use std::cell::Cell;

use adv_cpu_core::CpuState;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::asm::{Asm, Label};
use crate::encode::Shift::Lsl;
use crate::encode::Size::{W, X};
use crate::encode::*;
use crate::runtime::{self, Exit, ResidentCold};
use crate::translate::Trace;
use crate::translate::opt::{back::Lowered, front::Assumed, ir::Source, recovery::*};

use super::compile;

pub(super) const HOT: u32 = 64;
pub(super) const VERSIONS: usize = 3;
pub(super) const SAMPLE: u32 = 8;
pub(super) const PROBES: u8 = 8;
pub(super) const MISSES: u32 = 4;
pub(super) const COLD_EXITS: u8 = 2;
const PROBATION: u32 = 256;

pub(super) struct Head {
    /// The Tier-1 address at discovery identifies this trace's lifetime.
    pub code: usize,
    /// The Tier-1 unit containing the back edge that reached the threshold.
    pub source: u32,
    pub trace: Trace,
    pub trampoline: usize,
    pub tier1: usize,
    pub sample: Box<Cell<u32>>,
    pub probes: u8,
    pub versions: Vec<Version>,
    pub checked: FxHashSet<u32>,
    pub refused: FxHashSet<u32>,
    pub cuts: FxHashSet<u32>,
    pub cold_counts: FxHashMap<(u32, ResidentCold), u64>,
    /// Versions to rebuild once the current request settles.
    pub rebuilds: u8,
    pub pending: Option<compile::Task>,
    pub unprofitable: bool,
}

impl Head {
    pub fn new(code: usize, source: u32, trace: Trace) -> Self {
        Self {
            code,
            source,
            trace,
            trampoline: 0,
            tier1: 0,
            sample: Box::new(SAMPLE.into()),
            probes: PROBES,
            versions: Vec::new(),
            checked: FxHashSet::default(),
            refused: FxHashSet::default(),
            cuts: FxHashSet::default(),
            cold_counts: FxHashMap::default(),
            rebuilds: 0,
            pending: None,
            unprofitable: false,
        }
    }
}

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
}

pub(super) struct Probation {
    pub patches: Vec<usize>,
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
    (head, version, probation): (u32, usize, bool),
    sample: &Cell<u32>,
    slot: &Slot,
    gate: Option<Option<u64>>,
    form: &mut Lowered,
    at: usize,
    targets: Targets,
) -> Entry {
    let mut asm = Asm::new();
    let leave = asm.label();
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
    guest_exit(&mut asm, head);
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
    if probation {
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
    }
    asm.bind(snapshot);
    if probation {
        asm.emit(0);
    }

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
    if let Some(split_ticks) = gate {
        offer(&mut asm, split_ticks, leave);
    }

    let form_at = at + asm.offset();
    let address = |label| at + asm.position(label);
    let mut patches = vec![
        (address(next), targets.tier1),
        (address(tier1), targets.tier1),
        (address(exit), targets.exit),
        (address(opt_exit), targets.opt_exit),
    ];
    if probation {
        let call = form.probation[0];
        let from = form_at + call * 4;
        let offset = ((form.words[call] << 6) as i32 >> 4) as isize;
        patches.push((address(snapshot), from.wrapping_add_signed(offset)));
        form.words[call] = bl((address(account) as i64 - from as i64) as i32);
        let pointer = slot.countdown.as_ptr() as u64;
        form.words[form.probation_slot] = pointer as u32;
        form.words[form.probation_slot + 1] = (pointer >> 32) as u32;
    } else {
        for &call in &form.probation {
            form.words[call] = NOP;
        }
    }
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

fn guest_exit(asm: &mut Asm, head: u32) {
    asm.emit_all(&[
        movz(W, r(0), Exit::Dispatch as u16, 0),
        movz(W, r(1), head as u16, 0),
        movk(W, r(1), (head >> 16) as u16, 16),
    ]);
}

pub(super) fn fallback(head: u32, at: usize, exit: usize) -> Vec<u32> {
    let mut asm = Asm::new();
    guest_exit(&mut asm, head);
    asm.emit(branch(at + asm.offset(), exit));
    asm.finish()
}

fn offer(asm: &mut Asm, split_ticks: Option<u64>, leave: Label) {
    asm.emit_all(&[
        ldr(X, GATE, RT, runtime::OFFER_GATE),
        ldr(W, r(17), GATE, 0),
    ]);
    asm.cbz(W, true, r(17), leave);
    if let Some(split_ticks) = split_ticks {
        let affordable = asm.label();
        constant(asm, SLOT, split_ticks);
        asm.emit(ldr(X, r(17), SLOT, 0));
        asm.cbz(X, false, r(17), affordable);
        asm.emit(ldr(W, r(0), GATE, 4));
        asm.cbz(W, true, r(0), leave);
        asm.emit_all(&[
            mrs_nzcv(r(1)),
            mrs_cntvct(r(0)),
            add(X, r(0), r(0), r(17), Lsl, 0),
            ldr(X, r(2), GATE, 8),
            cmp(X, r(0), r(2), Lsl, 0),
            cset(W, r(3), Cond::Hi),
            msr_nzcv(r(1)),
        ]);
        asm.cbz(W, true, r(3), leave);
        asm.bind(affordable);
    }
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
