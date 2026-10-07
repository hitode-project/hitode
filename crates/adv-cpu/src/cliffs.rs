//! Always-on counters for the execution slow paths ("cliffs").
//!
//! Each counter marks a place where guest work left the fast native path:
//! an x87 helper that needed the exact softfloat model, native code handing
//! one instruction to the interpreter, or an interpreter entry. They are kept
//! per mnemonic or reason so a run report shows which cliff to remove next,
//! without the opt-in profilers. Counting happens only on paths that already
//! left generated code, so the native fast path pays nothing.
use std::collections::HashMap;

use iced_x86::{Code, Decoder, DecoderOptions, Mnemonic};

use crate::Memory;

/// Why an x87 helper call left the host-double fast path.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum X87Cliff {
    /// The instruction has no host-double form.
    NoFastForm,
    /// A pending unmasked exception or MMX mode.
    Pending,
    /// A control word outside the general arithmetic model (round to nearest,
    /// masked exceptions, PC=24 or 53), including failed exact-only PC=64 calls;
    /// loads and stores may still reject for operand reasons under it.
    Control,
    /// Operands, tags or memory outside the fast model.
    Operand,
}

/// Read-only opportunity classification for rejected PC=64 arithmetic.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum X87Pc64 {
    Other,
    Operand,
    Range,
    Inexact,
    Exact,
    Zero,
}

/// Why a block ran in the interpreter.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum InterpreterReason {
    /// No published entry owns the address and discovery has not attempted it.
    #[default]
    Uncovered,
    /// Discovery attempted this unowned entry. Compilation may still be queued,
    /// or publication may have excluded its form; this is not a failure count.
    Rejected,
    /// An indexed entry owns the address but has no native code yet. Automatic
    /// queued regions do not index entries until publication, so their waiting
    /// work remains in Uncovered/Rejected for historical comparability.
    Pending,
    /// Native code owns the address but has no entry there.
    Interior,
    /// The native code just requested an interpreted instruction.
    Fallback,
    /// Single-stepping, a pending x87 exception, or an interpreter-only run.
    Forced,
}

/// Observed entry state, not a guess about the instructions inside a batch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum InterpreterDetail {
    AutoJitDisabled,
    Tier1,
    NonExecutable,
    BelowThreshold,
    PendingCompilation,
    DeferredCompilation,
    QueueCapacity,
    RegionBoundary,
    UnregisteredEntry,
    NotDiscovered,
    DiscoveryBoundary,
    DiscoveryMemory,
    DiscoverySize,
    WorkBudget,
    CompileFailure,
    CachePolicy,
    NoPublishedEntry,
    Unsupported(Code),
}

#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Count {
    pub entries: u64,
    pub instructions: u64,
}

#[derive(Default, Clone, Debug)]
pub struct Cliffs {
    pub tier1_census: Option<crate::Tier1Census>,
    pub x87_helper_calls: u64,
    /// Helper calls completed by the host-double fast path.
    pub x87_fast: u64,
    /// Helper calls that ran the exact model, by instruction and reason.
    pub x87_exact: HashMap<(Mnemonic, X87Cliff), u64>,
    /// Control-classified rejections, keyed by PC, RC and exception mask bits.
    /// Kept separately so the truncated mnemonic list cannot hide controls.
    pub x87_control: HashMap<u16, u64>,
    /// PC=64, nearest, fully masked Control rejections; no operand data.
    pub x87_pc64: HashMap<(Mnemonic, X87Pc64), u64>,
    /// Native exits that handed the instruction at their PC to the interpreter.
    pub fallbacks: HashMap<Mnemonic, u64>,
    pub interpreter: HashMap<InterpreterReason, Count>,
    pub(crate) interpreter_details: HashMap<(InterpreterReason, InterpreterDetail), Count>,
}

impl Cliffs {
    pub(crate) fn merge(&mut self, other: Cliffs) {
        if let Some(census) = other.tier1_census {
            self.tier1_census
                .get_or_insert_with(Default::default)
                .merge(census);
        }
        self.x87_helper_calls += other.x87_helper_calls;
        self.x87_fast += other.x87_fast;
        for (key, n) in other.x87_exact {
            *self.x87_exact.entry(key).or_default() += n;
        }
        for (key, n) in other.x87_control {
            *self.x87_control.entry(key).or_default() += n;
        }
        for (key, n) in other.x87_pc64 {
            *self.x87_pc64.entry(key).or_default() += n;
        }
        for (key, n) in other.fallbacks {
            *self.fallbacks.entry(key).or_default() += n;
        }
        for (key, n) in other.interpreter {
            let count = self.interpreter.entry(key).or_default();
            count.entries += n.entries;
            count.instructions += n.instructions;
        }
        for (key, n) in other.interpreter_details {
            let count = self.interpreter_details.entry(key).or_default();
            count.entries += n.entries;
            count.instructions += n.instructions;
        }
    }

    pub(crate) fn fallback(&mut self, memory: &Memory, pc: u32) {
        *self.fallbacks.entry(mnemonic_at(memory, pc)).or_default() += 1;
    }

    pub(crate) fn interpreted(&mut self, reason: InterpreterReason, units: u32) {
        let count = self.interpreter.entry(reason).or_default();
        count.entries += 1;
        count.instructions += u64::from(units);
    }

    pub(crate) fn detail(
        &mut self,
        reason: InterpreterReason,
        detail: InterpreterDetail,
        units: u32,
    ) {
        let count = self
            .interpreter_details
            .entry((reason, detail))
            .or_default();
        count.entries += 1;
        count.instructions += u64::from(units);
    }

    /// Uncovered/Rejected entry states, ranked by interpreted instruction units.
    /// Code labels contain only instruction forms, never operands or addresses.
    pub fn interpreter_detail_rows(&self) -> Vec<(String, Count)> {
        let mut rows: Vec<_> = self
            .interpreter_details
            .iter()
            .map(|(&(reason, detail), &n)| {
                let label = match detail {
                    InterpreterDetail::Unsupported(code) => {
                        format!("Unsupported/{:?}/{code:?}", code.mnemonic())
                    }
                    other => format!("{other:?}"),
                };
                (format!("{reason:?}/{label}"), n)
            })
            .collect();
        rows.sort_unstable_by(|a, b| b.1.instructions.cmp(&a.1.instructions).then(a.0.cmp(&b.0)));
        rows
    }

    /// Exact-model x87 calls as (`mnemonic/reason`, count), most frequent first.
    pub fn x87_exact_rows(&self) -> Vec<(String, u64)> {
        rows(
            self.x87_exact
                .iter()
                .map(|(&(m, r), &n)| (format!("{m:?}/{r:?}"), n)),
        )
    }

    /// Joint control distribution for Control rejections, without addresses.
    pub fn x87_control_rows(&self) -> Vec<(String, u64)> {
        rows(self.x87_control.iter().map(|(&cw, &n)| {
            let pc = ["24", "reserved", "53", "64"][usize::from(cw >> 8 & 3)];
            let rc = ["nearest", "down", "up", "zero"][usize::from(cw >> 10 & 3)];
            (format!("PC={pc}/RC={rc}/masks={:02x}", cw & 0x3f), n)
        }))
    }

    /// Complete PC=64 opportunity distribution, without values or addresses.
    pub fn x87_pc64_rows(&self) -> Vec<(String, u64)> {
        rows(
            self.x87_pc64
                .iter()
                .map(|(&(m, r), &n)| (format!("{m:?}/{r:?}"), n)),
        )
    }

    /// Independent failed control guards; a call can fail more than one.
    pub fn x87_control_rejections(&self) -> Vec<(String, u64)> {
        let mut counts = [0; 3];
        for (&cw, &n) in &self.x87_control {
            counts[0] += n * u64::from(!matches!(cw >> 8 & 3, 0 | 2));
            counts[1] += n * u64::from(cw & 0xc00 != 0);
            counts[2] += n * u64::from(cw & 0x3f != 0x3f);
        }
        ["PC", "RC", "Masks"]
            .into_iter()
            .zip(counts)
            .map(|(key, n)| (key.to_owned(), n))
            .collect()
    }

    /// Native-to-interpreter handoffs as (mnemonic, count), most frequent first.
    pub fn fallback_rows(&self) -> Vec<(String, u64)> {
        rows(self.fallbacks.iter().map(|(m, &n)| (format!("{m:?}"), n)))
    }

    /// Interpreter work by reason, most instructions first.
    pub fn interpreter_rows(&self) -> Vec<(String, Count)> {
        let mut rows: Vec<_> = self
            .interpreter
            .iter()
            .map(|(r, &n)| (format!("{r:?}"), n))
            .collect();
        rows.sort_unstable_by(|a, b| b.1.instructions.cmp(&a.1.instructions).then(a.0.cmp(&b.0)));
        rows
    }
}

fn rows(rows: impl Iterator<Item = (String, u64)>) -> Vec<(String, u64)> {
    let mut rows: Vec<_> = rows.collect();
    rows.sort_unstable_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    rows
}

/// The mnemonic at `pc`, or INVALID when it cannot be read or decoded.
fn mnemonic_at(memory: &Memory, pc: u32) -> Mnemonic {
    let Some(bytes) = (1..=15)
        .rev()
        .find_map(|len| memory.read_data(pc, len).ok())
    else {
        return Mnemonic::INVALID;
    };
    Decoder::with_ip(32, &bytes, u64::from(pc), DecoderOptions::NONE)
        .decode()
        .mnemonic()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detail_merge_and_ranking_preserve_whole_batch_units() {
        let mut first = Cliffs::default();
        first.detail(
            InterpreterReason::Rejected,
            InterpreterDetail::Unsupported(Code::Cbw),
            9,
        );
        let mut second = Cliffs::default();
        second.detail(
            InterpreterReason::Rejected,
            InterpreterDetail::Unsupported(Code::Cbw),
            11,
        );
        second.detail(
            InterpreterReason::Uncovered,
            InterpreterDetail::BelowThreshold,
            20,
        );
        first.merge(second);
        let rows = first.interpreter_detail_rows();
        assert_eq!(
            rows,
            vec![
                (
                    "Rejected/Unsupported/Cbw/Cbw".into(),
                    Count {
                        entries: 2,
                        instructions: 20
                    }
                ),
                (
                    "Uncovered/BelowThreshold".into(),
                    Count {
                        entries: 1,
                        instructions: 20
                    }
                ),
            ]
        );
    }

    #[test]
    fn control_distribution_merges_and_counts_overlapping_guards() {
        let mut a = Cliffs::default();
        a.x87_control.insert(0x033f, 10);
        a.x87_pc64.insert((Mnemonic::Fadd, X87Pc64::Exact), 10);
        let mut b = Cliffs::default();
        b.x87_control.insert(0x033f, 2);
        b.x87_pc64.insert((Mnemonic::Fadd, X87Pc64::Exact), 2);
        b.x87_control.insert(0x0b3e, 3);
        b.x87_control.insert(0x063f, 5);
        a.merge(b);
        assert_eq!(a.x87_pc64_rows(), vec![("Fadd/Exact".into(), 12)]);
        assert_eq!(
            a.x87_control_rows(),
            vec![
                ("PC=64/RC=nearest/masks=3f".into(), 12),
                ("PC=53/RC=down/masks=3f".into(), 5),
                ("PC=64/RC=up/masks=3e".into(), 3),
            ]
        );
        assert_eq!(
            a.x87_control_rejections(),
            vec![("PC".into(), 15), ("RC".into(), 8), ("Masks".into(), 3)]
        );
        // Every PC/RC/mask combination is retained, beyond the top-24 list.
        a.x87_control = (0..64).map(|mask| (0x0300 | mask, 1)).collect();
        assert_eq!(a.x87_control_rows().len(), 64);
        assert_eq!(a.x87_control_rejections()[2].1, 63);
    }
}
