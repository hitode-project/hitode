//! Opt-in form coverage of retired interpreter work. No guest identity is kept.
use std::collections::HashMap;

use iced_x86::{Code, Decoder, DecoderOptions};

use crate::{InterpreterReason, cliffs::InterpreterDetail};

#[derive(Clone, Copy)]
pub(crate) struct Form {
    code: Code,
    supported: bool,
}

impl Form {
    pub fn decode(bytes: &[u8]) -> Self {
        let i = Decoder::new(32, bytes, DecoderOptions::NONE).decode();
        Self {
            code: i.code(),
            supported: super::forms::supported(&i),
        }
    }
}

/// One observed interpreter entry. REP units are elements, not decoded opcodes.
#[derive(Default)]
pub(crate) struct Entry {
    pub counts: Coverage,
    prefix: u32,
    blocked: bool,
}

impl Entry {
    pub fn record(&mut self, form: Form, units: u32) {
        // Faults with no committed work do not claim coverage or break a prefix.
        if units == 0 {
            return;
        }
        self.counts.instructions += 1;
        if form.supported {
            self.counts.supported_units += u64::from(units);
            if !self.blocked {
                self.prefix += 1;
                self.counts.prefix_units += u64::from(units);
            }
        } else {
            self.counts.unsupported_units += u64::from(units);
            *self.counts.unsupported.entry(form.code).or_default() += u64::from(units);
            self.blocked = true;
        }
    }
}

/// Counts have a fixed number of buckets and at most one row per iced Code.
/// Prefix histograms count decoded instructions, independently of REP units.
#[derive(Clone, Debug, Default)]
pub struct Coverage {
    pub entries: u64,
    pub instructions: u64,
    pub supported_units: u64,
    pub unsupported_units: u64,
    pub prefix_units: u64,
    pub blocked_prefixes: [u64; 8],
    pub censored_prefixes: [u64; 8],
    unsupported: HashMap<Code, u64>,
}

impl Coverage {
    fn merge(&mut self, other: Self) {
        self.entries += other.entries;
        self.instructions += other.instructions;
        self.supported_units += other.supported_units;
        self.unsupported_units += other.unsupported_units;
        self.prefix_units += other.prefix_units;
        for n in 0..8 {
            self.blocked_prefixes[n] += other.blocked_prefixes[n];
            self.censored_prefixes[n] += other.censored_prefixes[n];
        }
        for (code, units) in other.unsupported {
            *self.unsupported.entry(code).or_default() += units;
        }
    }

    /// Descending units, then ascending form name, like the existing cliff rows.
    pub fn unsupported_rows(&self) -> Vec<(String, u64)> {
        let mut rows: Vec<_> = self
            .unsupported
            .iter()
            .map(|(code, &n)| (format!("{code:?}"), n))
            .collect();
        rows.sort_unstable_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        rows
    }
}

#[derive(Clone, Debug, Default)]
pub struct Tier1Census {
    /// Disjoint groups: pending entries are removed from Uncovered/Rejected.
    pub groups: [Coverage; 3],
}

impl Tier1Census {
    pub const GROUPS: [&'static str; 3] = ["PendingCompilation", "Uncovered", "Rejected"];
    pub const PREFIXES: [&'static str; 8] =
        ["0", "1", "2-3", "4-7", "8-15", "16-31", "32-63", "64+"];

    pub(crate) fn record(
        &mut self,
        reason: InterpreterReason,
        detail: InterpreterDetail,
        mut entry: Entry,
    ) {
        let group = if detail == InterpreterDetail::PendingCompilation {
            0
        } else if reason == InterpreterReason::Uncovered {
            1
        } else {
            2
        };
        let bucket = if entry.prefix == 0 {
            0
        } else {
            (32 - entry.prefix.leading_zeros()).min(7) as usize
        };
        entry.counts.entries = 1;
        if entry.blocked {
            entry.counts.blocked_prefixes[bucket] = 1;
        } else {
            entry.counts.censored_prefixes[bucket] = 1;
        }
        self.groups[group].merge(entry.counts);
    }

    pub(crate) fn merge(&mut self, other: Self) {
        for (dst, src) in self.groups.iter_mut().zip(other.groups) {
            dst.merge(src);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CpuState, EXEC, Memory, READ, WRITE, interpreter::Interpreter};
    use iced_x86::code_asm::*;

    fn code(f: impl FnOnce(&mut CodeAssembler)) -> Vec<u8> {
        let mut a = CodeAssembler::new(32).unwrap();
        f(&mut a);
        a.assemble(0x10000).unwrap()
    }

    #[test]
    fn shared_guards_distinguish_prefixes_and_address_width() {
        for (bytes, expected) in [
            (code(|a| a.add(eax, ebx).unwrap()), true),
            (code(|a| a.lock().add(dword_ptr(eax), ebx).unwrap()), false),
            (code(|a| a.mov(eax, dword_ptr(bx + si)).unwrap()), false),
            (code(|a| a.mov(eax, dword_ptr(ebx).gs()).unwrap()), false),
            (code(|a| a.rep().movsb().unwrap()), true),
            (code(|a| a.fld1().unwrap()), false),
        ] {
            assert_eq!(Form::decode(&bytes).supported, expected);
        }
    }

    #[test]
    fn prefix_stops_at_first_unsupported_and_rep_units_remain_separate() {
        let yes = Form::decode(&code(|a| a.rep().movsb().unwrap()));
        let no = Form::decode(&code(|a| a.cpuid().unwrap()));
        let mut entry = Entry::default();
        entry.record(yes, 17);
        entry.record(no, 0);
        entry.record(yes, 1);
        entry.record(no, 1);
        entry.record(yes, 1);
        let mut census = Tier1Census::default();
        census.record(
            InterpreterReason::Rejected,
            InterpreterDetail::PendingCompilation,
            entry,
        );
        let c = &census.groups[0];
        assert_eq!(
            (
                c.entries,
                c.instructions,
                c.supported_units,
                c.unsupported_units,
                c.prefix_units
            ),
            (1, 4, 19, 1, 18)
        );
        assert_eq!(c.blocked_prefixes[2], 1);
        assert_eq!(c.unsupported_rows(), vec![("Cpuid".into(), 1)]);
        let mut other = Tier1Census::default();
        other.record(
            InterpreterReason::Uncovered,
            InterpreterDetail::BelowThreshold,
            Entry::default(),
        );
        census.merge(other);
        assert_eq!(census.groups[1].censored_prefixes[0], 1);
        assert_eq!(census.groups[2].entries, 0);
    }

    #[test]
    fn observed_blocks_preserve_execution_and_revalidate_changed_forms() {
        let start = 0x10000;
        let bytes = code(|a| {
            a.nop().unwrap();
            a.cpuid().unwrap();
            a.add(eax, 1).unwrap();
        });
        let mut memory = Memory::new(start, 4096).unwrap();
        memory.protect(start, 4096, READ | WRITE | EXEC).unwrap();
        memory.write(start, &bytes).unwrap();
        let mut interpreter = Interpreter::new();
        let initial = CpuState {
            eip: start,
            ..Default::default()
        };
        let mut plain = initial.clone();
        let expected =
            interpreter.run_block(&mut plain, &mut memory, 3, start + bytes.len() as u32, None);
        let mut observed = initial.clone();
        interpreter.census = Some(Entry::default());
        let result = interpreter.run_block(
            &mut observed,
            &mut memory,
            3,
            start + bytes.len() as u32,
            None,
        );
        assert_eq!(result.0.units, expected.0.units);
        assert_eq!(observed.regs, plain.regs);
        assert_eq!(observed.eflags, plain.eflags);
        assert_eq!(observed.eip, plain.eip);
        let entry = interpreter.census.take().unwrap();
        assert_eq!(
            (
                entry.prefix,
                entry.counts.supported_units,
                entry.counts.unsupported_units
            ),
            (1, 2, 1)
        );
        let replacement = code(|a| {
            a.nop().unwrap();
            a.nop().unwrap();
            a.nop().unwrap();
        });
        memory.write(start, &replacement).unwrap();
        interpreter.census = Some(Entry::default());
        interpreter.run_block(&mut initial.clone(), &mut memory, 3, start + 3, None);
        let entry = interpreter.census.take().unwrap();
        assert_eq!((entry.prefix, entry.blocked), (3, false));
    }
}
