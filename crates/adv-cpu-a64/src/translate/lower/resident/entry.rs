//! Owner-only resident entry sequences.

use std::cell::Cell;

use crate::asm::{Asm, Label};
use crate::encode::Shift::Lsl;
use crate::encode::Size::{W, X};
use crate::encode::*;
use crate::runtime::{self, Exit, PlanEvent};
use crate::translate::lower::stubs;

/// These counters are private to one version and written only by the owner.
#[derive(Default)]
#[repr(C)]
pub(crate) struct Slot {
    pub retry: Cell<u32>,
    pub env: [Cell<u32>; 8],
    /// Snapshot followed by total retirement, as read by the lowered calls.
    pub countdown: [Cell<u64>; 2],
    pub entries: Cell<u64>,
    pub probation: Cell<u32>,
}

const _: () =
    assert!(std::mem::offset_of!(Slot, entries) == std::mem::offset_of!(Slot, countdown) + 16);

const fn r(n: u8) -> Reg {
    Reg::new(n)
}

const RT: Reg = r(29);
const GATE: Reg = r(16);
const SLOT: Reg = r(15);

pub(crate) fn branch(from: usize, to: usize) -> u32 {
    b((to as i64 - from as i64) as i32)
}

pub(crate) struct RowFeedback {
    pub failures: [Cell<u64>; 6],
    pub next: Cell<u32>,
    pub target_next: Cell<u32>,
}

impl Default for RowFeedback {
    fn default() -> Self {
        Self {
            failures: Default::default(),
            next: Cell::new(32),
            target_next: Cell::new(1),
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct RowPrice {
    pub guard: u32,
    pub saved: u32,
}

/// Relocations in a lowered resident artifact, in words except link and site offsets.
pub(crate) struct Layout<'a> {
    pub words: &'a mut [u32],
    pub body: usize,
    pub miss: usize,
    pub cold: usize,
    pub env_miss: usize,
    pub environment: &'a [(usize, usize)],
    pub exit: usize,
    pub spill_cells: usize,
    pub probation_slot: usize,
    pub probation_sensor: &'a (Vec<usize>, usize, usize),
    pub accounting: &'a [usize],
    pub links: &'a [(usize, u32)],
    pub transitions: &'a [(usize, usize)],
    pub sites: &'a [(usize, super::super::Site)],
    pub marks: &'a [crate::jit_map::Mark],
    pub split: Option<SplitPoint>,
    pub row_price: Option<RowPrice>,
    pub row_failures: &'a [(super::RowFailure, usize)],
    pub counters: &'a [(u32, super::Counter, usize)],
}

#[derive(Clone, Copy)]
pub(crate) struct SplitPoint {
    pub sensor: usize,
    pub threshold: usize,
    pub leave: usize,
}

pub(crate) struct Targets {
    pub at: usize,
    pub proved_body: Option<usize>,
    pub tier1: usize,
    pub exit: usize,
    pub resident_exit: usize,
    pub split: bool,
}

pub(crate) struct MissCounter {
    pub entry: usize,
    pub next: usize,
    pub literal: usize,
}

pub(crate) struct Entry {
    pub words: Vec<u32>,
    pub entry: usize,
    pub proved: Option<usize>,
    pub proved_cold: Option<usize>,
    pub probe: usize,
    pub next: usize,
    pub retry: usize,
    pub form: usize,
    pub tail: usize,
    pub environment: usize,
    pub probation: usize,
    pub split: Option<usize>,
    pub miss_counter: Option<MissCounter>,
}

/// Prefix and resident form, with the context-miss continuation patchable
/// independently of the Tier-1 fallback shared by the head's versions.
pub(crate) fn emit(
    (head, version): (u32, usize),
    sample: &Cell<u32>,
    slot: &Slot,
    gate: Option<OfferEntry>,
    spill_cells: usize,
    count_misses: bool,
    targets: Targets,
) -> Entry {
    let at = targets.at;
    let mut asm = Asm::new();
    let offer_exit = asm.label();
    let offer_fallback = asm.label();
    let offer_counter = asm.label();
    let retry = asm.label();
    let retry_counter = asm.label();
    let miss = asm.label();
    let env = asm.label();
    let tail = asm.label();
    let probe = asm.label();
    let sample_counter = asm.label();
    let sampled = asm.label();
    let entry = asm.label();
    let probation = asm.label();
    let proved_cold = asm.label();
    let next = asm.label();
    let exit = asm.label();
    let opt_exit = asm.label();

    if gate.is_some() {
        asm.bind(offer_exit);
        sample_offer(&mut asm, head, offer_fallback);
        asm.b(exit);
    }
    if let Some(gate) = gate {
        literal(&mut asm, offer_counter, gate.counter);
    }
    literal(&mut asm, retry_counter, slot.retry.as_ptr() as u64);
    if version == 0 {
        literal(&mut asm, sample_counter, sample.as_ptr() as u64);
    }

    let miss_counter = count_misses.then(|| {
        let address = asm.label();
        let entry = at + asm.offset();
        asm.ldr_literal(SLOT, address);
        asm.emit_all(&[
            ldr(X, r(0), SLOT, 0),
            add_imm(X, r(0), r(0), 1, false),
            str(X, r(0), SLOT, 0),
        ]);
        let next = at + asm.offset();
        asm.emit(0);
        let literal_at = at + asm.offset();
        literal(&mut asm, address, 0);
        MissCounter {
            entry,
            next,
            literal: literal_at,
        }
    });

    asm.bind(next);
    asm.emit(0);

    asm.bind(miss);
    runtime::emit_sensor(&mut asm, retry_counter, SLOT, r(0), retry);
    asm.b(next);

    asm.bind(retry);
    observe(&mut asm, head, version, runtime::RESIDENT_VERSION_MISS);
    asm.b(next);
    asm.bind(exit);
    asm.emit(0);

    asm.bind(env);
    guest_exit(&mut asm, head, Exit::Plan(PlanEvent::EnvironmentMiss));
    asm.emit_all(&[
        movk(X, r(0), (version as u16) << 8, 32),
        orr(X, r(0), r(0), r(3), Lsl, 32),
    ]);
    asm.b(exit);

    // x16 and x17 hold no form values at a cold exit (A64-OPT-7).
    asm.bind(tail);
    constant(&mut asm, r(16), spill_cells as u64);
    asm.emit(str(X, r(16), RT, runtime::RECOVERY_CELLS));
    constant(&mut asm, r(16), u64::from(head));
    asm.emit_all(&[
        str(W, r(16), RT, runtime::RECOVERY_HEAD),
        movz(W, r(16), version as u16, 0),
        str(W, r(16), RT, runtime::RECOVERY_VERSION),
    ]);
    asm.bind(opt_exit);
    asm.emit(0);

    asm.bind(probation);
    asm.emit_all(&[
        movz(
            X,
            r(0),
            Exit::Plan(PlanEvent::Probation(version as u8)).word(),
            0,
        ),
        movk(X, r(0), head as u16, 32),
        movk(X, r(0), (head >> 16) as u16, 48),
    ]);
    asm.b(exit);

    let split = targets.split.then(|| {
        let at = at + asm.offset();
        guest_exit(
            &mut asm,
            head,
            Exit::Plan(PlanEvent::ResidentSplitOffer(version as u8)),
        );
        asm.emit(orr(X, r(0), r(0), r(7), Lsl, 32));
        asm.b(exit);
        at
    });

    if version == 0 {
        asm.bind(sampled);
        observe(&mut asm, head, 0, runtime::RESIDENT_LINK_SAMPLE);
        asm.b(entry);
    }
    let proved = targets.proved_body.map(|offset| {
        asm.bind(proved_cold);
        let fallback = at + asm.offset();
        asm.emit(branch(fallback, targets.tier1));
        let entry = at + asm.offset();
        if let Some(gate) = gate {
            offer(&mut asm, gate, offer_counter, offer_exit);
        }
        let rep_pending = std::mem::offset_of!(runtime::Runtime, rep_pending) as u32;
        asm.emit_all(&[
            ldr(X, r(0), RT, runtime::CPU_STATE),
            ldr(W, r(16), RT, rep_pending),
        ]);
        asm.cbz(W, true, r(16), proved_cold);
        let branch = at + asm.offset();
        asm.emit(0);
        (entry, branch, offset)
    });
    asm.bind(probe);
    if version == 0 {
        runtime::emit_sensor(&mut asm, sample_counter, SLOT, r(0), sampled);
    }
    asm.bind(entry);
    if let Some(gate) = gate {
        offer(&mut asm, gate, offer_counter, offer_exit);
    }
    asm.bind(offer_fallback);

    let form_at = at + asm.offset();
    let address = |label| at + asm.position(label);
    let patches = [
        (address(next), targets.tier1),
        (address(exit), targets.exit),
        (address(opt_exit), targets.resident_exit),
    ];
    let (entry, probe, next, retry) =
        (address(entry), address(probe), address(next), address(miss));
    let (tail, environment, probation) = (address(tail), address(env), address(probation));
    let mut words = asm.finish();
    for (from, to) in patches {
        words[(from - at) / 4] = branch(from, to);
    }
    if let Some((_, point, offset)) = proved {
        words[(point - at) / 4] = branch(point, form_at + offset);
    }
    Entry {
        words,
        entry,
        proved: proved.map(|(entry, _, _)| entry),
        proved_cold: proved.map(|(entry, _, _)| entry - 4),
        probe,
        next,
        retry,
        form: form_at,
        tail,
        environment,
        probation,
        split,
        miss_counter,
    }
}

fn observe(asm: &mut Asm, head: u32, version: usize, kind: u16) {
    stubs::emit_save_light(asm);
    asm.emit(ldr(X, r(0), RT, runtime::CPU_STATE));
    stubs::emit_store_guest(asm, r(0));
    constant(asm, r(1), u64::from(head));
    asm.emit_all(&[
        mov(X, r(0), RT),
        if kind == runtime::RESIDENT_LINK_SAMPLE {
            mov(X, r(2), r(14))
        } else {
            movz(X, r(2), version as u16, 0)
        },
        movz(W, r(3), kind, 0),
        ldr(X, r(16), RT, runtime::PLAN_OBSERVATION),
        blr(r(16)),
    ]);
    stubs::emit_restore_light(asm);
}

/// An active decision reads one owner-published limit. A settled sensor
/// replaces the first word with a branch over the other two words.
pub(crate) fn split_sensor(asm: &mut Asm, count: Reg, threshold: Label, accepted: Label) {
    asm.ldr_literal(r(16), threshold);
    asm.emit(sub(X, r(17), count, r(16), Lsl, 0));
    asm.tbz(false, r(17), 63, accepted);
}

fn guest_exit(asm: &mut Asm, head: u32, exit: Exit) {
    asm.emit_all(&[
        movz(W, r(0), exit.word(), 0),
        movz(W, r(1), head as u16, 0),
        movk(W, r(1), (head >> 16) as u16, 16),
    ]);
}

/// Read an ordinary invariant slot once and select its prepared case.
pub(crate) fn row_entry(
    head: u32,
    (base, offset): (u8, u32),
    cases: &[(u32, usize)],
    (at, fallback, exit): (usize, usize, usize),
    (feedback, arrivals): (&RowFeedback, Option<&std::cell::Cell<u64>>),
) -> Vec<u32> {
    let mut asm = Asm::new();
    let safe = asm.label();
    let search = asm.label();
    let sample = asm.label();
    let count = asm.label();
    let poll = asm.label();
    let bad = asm.label();
    let failed = asm.label();
    let mut destinations = Vec::new();
    let counter = arrivals.map(|_| asm.label());
    if let Some(counter) = counter {
        asm.ldr_literal(r(0), counter);
        asm.emit_all(&[
            ldr(X, r(1), r(0), 0),
            add_imm(X, r(1), r(1), 1, false),
            str(X, r(1), r(0), 0),
        ]);
    }

    asm.emit(mrs_nzcv(r(6)));
    constant(&mut asm, r(7), u64::from(offset));
    asm.emit_all(&[
        add(W, r(7), r(19 + base), r(7), Lsl, 0),
        mov(W, r(0), r(7)),
        add_imm(X, r(1), r(0), 3, false),
        lsr_imm(X, r(2), r(1), 32),
    ]);
    asm.cbz(X, true, r(2), bad);
    crate::facts::batch_span(&mut asm, false, false, search, safe);
    asm.b(bad);
    asm.bind(safe);
    asm.emit(add_ext(X, r(1), r(27), r(7), Extend::Uxtw, 0));
    asm.emit(ldr(W, r(7), r(1), 0));
    for &(value, target) in cases {
        let selected = asm.label();
        constant(&mut asm, r(0), u64::from(value));
        asm.emit(cmp(W, r(7), r(0), Lsl, 0));
        asm.b_cond(Cond::Eq, selected);
        destinations.push((selected, target));
    }
    asm.ldr_literal(r(0), count);
    asm.emit_all(&[
        ldr(X, r(1), r(0), 0),
        add_imm(X, r(1), r(1), 1, false),
        str(X, r(1), r(0), 0),
    ]);
    runtime::emit_sensor(&mut asm, poll, r(0), r(1), sample);
    asm.b(failed);
    asm.bind(sample);
    asm.emit(msr_nzcv(r(6)));
    guest_exit(&mut asm, head, Exit::Plan(PlanEvent::RowTargetMiss));
    asm.emit(orr(X, r(0), r(0), r(7), Lsl, 32));
    asm.emit(branch(at + asm.offset(), exit));
    for (label, target) in destinations {
        asm.bind(label);
        asm.emit(msr_nzcv(r(6)));
        asm.emit(branch(at + asm.offset(), target));
    }
    asm.bind(bad);
    asm.emit(msr_nzcv(r(6)));
    row_feedback(
        &mut asm,
        head,
        super::RowFailure::Parameter,
        (at, fallback, exit),
        feedback,
    );
    asm.bind(failed);
    asm.emit(msr_nzcv(r(6)));
    asm.emit(branch(at + asm.offset(), fallback));
    crate::facts::batch_span_search(&mut asm, search, false);
    literal(
        &mut asm,
        count,
        feedback.failures[super::RowFailure::Target as usize].as_ptr() as u64,
    );
    literal(&mut asm, poll, feedback.target_next.as_ptr() as u64);
    if let Some((label, counter)) = counter.zip(arrivals) {
        literal(&mut asm, label, counter.as_ptr() as u64);
    }
    asm.finish()
}

pub(crate) fn row_failure(
    head: u32,
    reason: super::RowFailure,
    addresses: (usize, usize, usize),
    feedback: &RowFeedback,
) -> Vec<u32> {
    let mut asm = Asm::new();
    row_feedback(&mut asm, head, reason, addresses, feedback);
    asm.finish()
}

fn row_feedback(
    asm: &mut Asm,
    head: u32,
    reason: super::RowFailure,
    (at, fallback, exit): (usize, usize, usize),
    feedback: &RowFeedback,
) {
    let count = asm.label();
    let poll = asm.label();
    let sample = asm.label();
    asm.ldr_literal(r(0), count);
    asm.emit_all(&[
        ldr(X, r(1), r(0), 0),
        add_imm(X, r(1), r(1), 1, false),
        str(X, r(1), r(0), 0),
    ]);
    runtime::emit_sensor(asm, poll, r(0), r(1), sample);
    asm.emit(branch(at + asm.offset(), fallback));
    asm.bind(sample);
    guest_exit(asm, head, Exit::Plan(PlanEvent::RowGuardMiss));
    asm.emit(branch(at + asm.offset(), exit));
    literal(
        asm,
        count,
        feedback.failures[reason as usize].as_ptr() as u64,
    );
    literal(asm, poll, feedback.next.as_ptr() as u64);
}

pub(crate) fn fallback(head: u32, at: usize, exit: usize) -> Vec<u32> {
    let mut asm = Asm::new();
    guest_exit(&mut asm, head, Exit::Dispatch);
    asm.emit(branch(at + asm.offset(), exit));
    asm.finish()
}

pub(crate) fn version_miss(head: u32, at: usize, dispatch: usize) -> Vec<u32> {
    let mut asm = Asm::new();
    super::super::emit_target(&mut asm, head);
    asm.emit(mvn(X, r(2), Reg::ZR));
    asm.emit(branch(at + asm.offset(), dispatch));
    asm.finish()
}

/// Stable plan cells read only by the owner.
#[derive(Clone, Copy)]
pub(crate) struct OfferEntry {
    pub counter: u64,
    pub split: bool,
}

pub(crate) fn offer_link(
    head: u32,
    at: usize,
    exit: usize,
    unit: Option<usize>,
    gate: OfferEntry,
) -> (Vec<u32>, usize) {
    let mut asm = Asm::new();
    let request = asm.label();
    let counter = asm.label();
    let no_offer = asm.label();
    offer(&mut asm, gate, counter, request);
    asm.bind(no_offer);
    let fallback = at + asm.offset();
    if let Some(unit) = unit {
        asm.emit(branch(fallback, unit));
    } else {
        asm.emit_all(&self::fallback(head, fallback, exit));
    }
    asm.bind(request);
    sample_offer(&mut asm, head, no_offer);
    asm.emit(branch(at + asm.offset(), exit));
    literal(&mut asm, counter, gate.counter);
    (asm.finish(), fallback)
}

fn offer(asm: &mut Asm, gate: OfferEntry, counter: Label, leave: Label) {
    let proceed = asm.label();
    if !gate.split {
        asm.emit_all(&[
            ldr(X, GATE, RT, runtime::OFFER_GATE),
            ldr(W, r(17), GATE, 0),
        ]);
        asm.cbz(W, false, r(17), proceed);
    }
    runtime::emit_sensor(asm, counter, SLOT, r(0), leave);
    asm.bind(proceed);
}

fn sample_offer(asm: &mut Asm, head: u32, fallback: Label) {
    stubs::emit_save_light(asm);
    constant(asm, r(1), u64::from(head));
    asm.emit_all(&[
        mov(X, r(0), RT),
        movz(W, r(3), runtime::CHECK_OFFER, 0),
        ldr(X, r(16), RT, runtime::PLAN_OBSERVATION),
        blr(r(16)),
    ]);
    stubs::emit_restore_light(asm);
    asm.cbz(W, false, r(0), fallback);
    guest_exit(asm, head, Exit::Plan(PlanEvent::OfferReady));
}

fn literal(asm: &mut Asm, label: Label, value: u64) {
    asm.bind(label);
    asm.emit_all(&[value as u32, (value >> 32) as u32]);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::CodeCache;

    // spec: A64A-PLACE-3
    #[test]
    fn split_offer_rearms_and_a_decided_sensor_executes_one_word() {
        let counter = Cell::new(2u32);
        let mut asm = Asm::new();
        let data = asm.label();
        let sample = asm.label();
        asm.emit(cmp(W, r(0), r(0), Lsl, 0));
        let sensor = asm.offset();
        let gate = OfferEntry {
            counter: counter.as_ptr() as u64,
            split: true,
        };
        offer(&mut asm, gate, data, sample);
        let after = asm.offset();
        assert_eq!(after - sensor, 5 * 4);
        asm.emit_all(&[cset(W, r(0), Cond::Ne), ret(Reg::LR)]);
        asm.bind(sample);
        asm.emit_all(&[cset(W, r(0), Cond::Eq), ret(Reg::LR)]);
        literal(&mut asm, data, gate.counter);
        let words = asm.finish();
        assert_eq!(
            &words[sensor / 4..after / 4],
            &[
                ldr_literal(SLOT, 36),
                ldr(W, r(0), SLOT, 0),
                sub_imm(W, r(0), r(0), 1, false),
                str(W, r(0), SLOT, 0),
                cbz(W, false, r(0), 12),
            ]
        );
        let mut cache = CodeCache::new(64 << 10).unwrap();
        let at = cache.append(&words).unwrap();
        cache.sync();
        // SAFETY: the emitted leaf returns u32 and uses only volatile registers.
        let enter: extern "C" fn() -> u32 = unsafe { std::mem::transmute(at) };
        assert_eq!((enter(), counter.get()), (0, 1));
        assert_eq!((enter(), counter.get()), (1, 0));
        counter.set(2);
        assert_eq!((enter(), counter.get()), (0, 1));
        assert_eq!((enter(), counter.get()), (1, 0));
        counter.set(1);
        cache.patch(at + sensor, b((after - sensor) as i32));
        cache.sync();
        assert_eq!(cache.word(at + sensor), b(20));
        for (index, &word) in words.iter().enumerate().skip(sensor / 4 + 1) {
            assert_eq!(cache.word(at + index * 4), word);
        }
        assert_eq!((enter(), counter.get()), (0, 1));
    }

    // spec: A64A-PLACE-3, A64-OPT-9
    #[test]
    fn a_split_decline_reads_one_limit_and_preserves_nzcv() {
        let mut asm = Asm::new();
        let threshold = asm.label();
        let accept = asm.label();
        split_sensor(&mut asm, r(7), threshold, accept);
        assert_eq!(asm.offset(), 12);
        asm.bind(accept);
        asm.bind(threshold);
        asm.emit_all(&[0, 0]);
        let words = asm.finish();
        assert_eq!(words[1], sub(X, r(17), r(7), r(16), Lsl, 0));
        assert_eq!(words[2], tbz(false, r(17), 63, 4));
        assert_eq!(b(12), 0x14000003);
    }

    // spec: A64A-PLACE-3
    #[test]
    fn a_whole_job_gate_checks_the_published_flag_in_three_words() {
        let mut asm = Asm::new();
        let data = asm.label();
        let sample = asm.label();
        let gate = OfferEntry {
            counter: 0,
            split: false,
        };
        offer(&mut asm, gate, data, sample);
        asm.bind(sample);
        literal(&mut asm, data, gate.counter);
        let words = asm.finish();
        assert_eq!(
            &words[..3],
            &[
                ldr(X, GATE, RT, runtime::OFFER_GATE),
                ldr(W, r(17), GATE, 0),
                cbz(W, false, r(17), 24),
            ]
        );
    }
}
