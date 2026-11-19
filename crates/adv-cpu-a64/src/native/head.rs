//! Resident entry, version misses and recovery tails (A64-OPT-8).
//!
//! Owner-only sensors use plan cells embedded as code literals. Their
//! cold paths either patch a stable link in place or leave before the
//! owner changes the version chain. Detached jobs have their own entry
//! code and retain no pointers to these cells.

use std::cell::Cell;

use adv_cpu_core::CpuState;

use crate::asm::{Asm, Label};
use crate::encode::Shift::Lsl;
use crate::encode::Size::{W, X};
use crate::encode::*;
use crate::runtime::{self, Exit, PlanEvent};
use crate::translate::opt::{back::Lowered, front::Assumed, ir::Source, recovery::*};

/// These counters are private to one version and written only by the owner.
#[derive(Default)]
#[repr(C)]
pub(super) struct Slot {
    pub retry: Cell<u32>,
    pub env: [Cell<u32>; 8],
    /// Snapshot followed by total retirement, as read by the lowered calls.
    pub countdown: [Cell<u64>; 2],
    pub entries: Cell<u64>,
    pub probation: Cell<u32>,
}

const _: () =
    assert!(std::mem::offset_of!(Slot, entries) == std::mem::offset_of!(Slot, countdown) + 16);

pub(super) struct Version {
    pub entry: usize,
    pub probe: usize,
    pub probe_word: u32,
    pub next: usize,
    pub retry: (usize, u32),
    pub environment: Vec<usize>,
    pub at: CpuState,
    pub assumed: Assumed,
    pub sources: Vec<Source>,
    pub plain_sites: Vec<u32>,
    pub records: Vec<Record<Host>>,
    pub flags: Vec<Option<FlagRecipe>>,
    pub slot: Box<Slot>,
    pub probation: Option<Probation>,
    pub probation_sensors: Vec<usize>,
    pub iteration_retired: u64,
    pub accounting: Vec<(usize, u32)>,
}

impl Version {
    pub fn set_retry(&self, cache: &mut crate::cache::CodeCache, period: Option<u32>) {
        let (point, word) = self.retry;
        let replacement = match period {
            Some(count) => {
                self.slot.retry.set(count);
                word
            }
            None => branch(point, self.next),
        };
        cache.patch(point, replacement);
    }

    pub fn iterations(&self, retired: u64) -> u64 {
        retired.checked_div(self.iteration_retired).unwrap_or(0)
    }

    pub fn account(&self, cache: &mut crate::cache::CodeCache, enabled: bool) {
        for &(point, call) in &self.accounting {
            cache.patch(point, if enabled { call } else { NOP });
        }
        if self.probation.is_none() {
            let skip = if enabled {
                runtime::SENSOR_BYTES
            } else {
                runtime::SENSOR_BYTES + 4
            };
            for &point in &self.probation_sensors {
                cache.patch(point, b(skip as i32));
            }
        }
    }
}

pub(super) struct Probation {
    pub iteration_retired: u64,
    pub saved_words: u64,
    pub entry_exit_words: u64,
    pub cold_entries: u64,
    pub cold_retired: u64,
}

impl Probation {
    pub fn profitable(&self, slot: &Slot) -> bool {
        let entries = slot.entries.get() + self.cold_entries;
        let retired = slot.countdown[1].get() + self.cold_retired;
        retired / entries / self.iteration_retired * self.saved_words >= self.entry_exit_words
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
    pub resident_exit: usize,
}

pub(super) struct Entry {
    pub words: Vec<u32>,
    pub entry: usize,
    pub probe: usize,
    pub next: usize,
    pub retry: usize,
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
    let next = asm.label();
    let tier1 = asm.label();
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

    asm.bind(next);
    asm.emit(0);
    asm.bind(tier1);
    asm.emit(0);

    asm.bind(miss);
    runtime::emit_sensor(&mut asm, retry_counter, SLOT, r(0), retry);
    asm.b(next);

    asm.bind(retry);
    guest_exit(&mut asm, head, Exit::Plan(PlanEvent::ContextRetry));
    asm.emit(movk(X, r(0), version as u16, 32));
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
    constant(&mut asm, r(16), form.spill_cells as u64);
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

    if version == 0 {
        asm.bind(sampled);
        runtime::emit_save_light(&mut asm);
        asm.emit(ldr(X, r(0), RT, runtime::CPU_STATE));
        runtime::emit_store_guest(&mut asm, r(0));
        constant(&mut asm, r(1), u64::from(head));
        asm.emit_all(&[
            mov(X, r(0), RT),
            mov(X, r(2), r(14)),
            movz(W, r(3), runtime::RESIDENT_LINK_SAMPLE, 0),
            ldr(X, r(16), RT, runtime::PLAN_OBSERVATION),
            blr(r(16)),
        ]);
        runtime::emit_restore_light(&mut asm);
        asm.b(entry);
    }
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
    let pointer = slot.countdown.as_ptr() as u64;
    form.words[form.probation_slot] = pointer as u32;
    form.words[form.probation_slot + 1] = (pointer >> 32) as u32;
    let (_, counter, leave) = form.probation_sensor.as_ref().unwrap();
    let pointer = slot.probation.as_ptr() as u64;
    form.words[*counter] = pointer as u32;
    form.words[*counter + 1] = (pointer >> 32) as u32;
    form.words[*leave] = branch(form_at + leave * 4, address(probation));
    for (fact, &(_, literal)) in form.environment.iter().enumerate() {
        let cell = &slot.env[fact];
        let pointer = cell.as_ptr() as u64;
        form.words[literal] = pointer as u32;
        form.words[literal + 1] = (pointer >> 32) as u32;
    }
    let patches = [
        (address(next), targets.tier1),
        (address(tier1), targets.tier1),
        (address(exit), targets.exit),
        (address(opt_exit), targets.resident_exit),
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
    let (entry, probe, next, retry) =
        (address(entry), address(probe), address(next), address(miss));
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
        retry,
        form: form_at,
    }
}

fn guest_exit(asm: &mut Asm, head: u32, exit: Exit) {
    asm.emit_all(&[
        movz(W, r(0), exit.word(), 0),
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
    pub counter: u64,
    pub split: bool,
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
    runtime::emit_save_light(asm);
    constant(asm, r(1), u64::from(head));
    asm.emit_all(&[
        mov(X, r(0), RT),
        movz(W, r(3), runtime::CHECK_OFFER, 0),
        ldr(X, r(16), RT, runtime::PLAN_OBSERVATION),
        blr(r(16)),
    ]);
    runtime::emit_restore_light(asm);
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
