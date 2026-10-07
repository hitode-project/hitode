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
    /// A control word outside the arithmetic fast model (round to nearest,
    /// masked exceptions, PC=24 or 53); loads and stores may still reject for
    /// operand reasons under it.
    Control,
    /// Operands, tags or memory outside the fast model.
    Operand,
}

/// Why a block ran in the interpreter.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum InterpreterReason {
    /// No region owns the address.
    #[default]
    Uncovered,
    /// Discovery tried the address and compiled nothing for it.
    Rejected,
    /// A region owns the address but has no native code yet.
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
    pub x87_helper_calls: u64,
    /// Helper calls completed by the host-double fast path.
    pub x87_fast: u64,
    /// Helper calls that ran the exact model, by instruction and reason.
    pub x87_exact: HashMap<(Mnemonic, X87Cliff), u64>,
    /// Native exits that handed the instruction at their PC to the interpreter.
    pub fallbacks: HashMap<Mnemonic, u64>,
    pub interpreter: HashMap<InterpreterReason, Count>,
    pub(crate) interpreter_details: HashMap<(InterpreterReason, InterpreterDetail), Count>,
}

impl Cliffs {
    pub(crate) fn merge(&mut self, other: Cliffs) {
        self.x87_helper_calls += other.x87_helper_calls;
        self.x87_fast += other.x87_fast;
        for (key, n) in other.x87_exact {
            *self.x87_exact.entry(key).or_default() += n;
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
}
