//! Opt-in tier verification for the AArch64 engine: the first native entries are
//! checked against the interpreter.
//!
//! With `ADV_JIT_VERIFY=1` (or `strict`), the executor first interprets from
//! the entry one instruction at a time, keeping the architectural state before
//! each instruction and the bytes each one writes. It restores those bytes and
//! the entry state, then runs the native code once. The engine checks each entry
//! PC's first runs and reports how many instructions they retired, which names
//! the interpreted state directly. Native writes outside the interpreted
//! footprint go unseen. Strict mode turns a mismatch into a run error, so a
//! bisection stops at the first divergent unit.
use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;

use iced_x86::{
    Decoder, DecoderOptions, Instruction, InstructionInfoFactory, OpAccess, Register, RflagsBits,
};

use crate::interpreter::{Interpreter, Step};
use crate::{ArchitecturalState, CpuState, Memory};

/// Interpreted instructions per trace; a native exit beyond it is inconclusive.
pub const MAX_STEPS: usize = 4096;
/// Native entries verified per entry PC.
pub const ENTRIES: u8 = 4;
/// Mismatch descriptions kept for the run report.
pub const REPORTS: usize = 16;

#[derive(Default, Clone, Debug)]
pub struct VerifyStats {
    pub verified: u64,
    /// Native exits that no interpreted state reached.
    pub inconclusive: u64,
    pub mismatches: u64,
    /// Exits that differed only in MXCSR exception flags, which native SSE
    /// does not accumulate.
    pub mxcsr_flags: u64,
    /// The first mismatches, with guest addresses, for local triage only.
    pub reports: Vec<String>,
}

impl VerifyStats {
    /// The `tier_verification` report object.
    pub fn json(&self) -> String {
        format!(
            "{{\"verified\":{},\"inconclusive\":{},\"mismatches\":{},\"mxcsr_flags\":{},\"reports\":[{}]}}",
            self.verified,
            self.inconclusive,
            self.mismatches,
            self.mxcsr_flags,
            self.reports
                .iter()
                .map(|r| crate::engine::json_string(r))
                .collect::<Vec<_>>()
                .join(",")
        )
    }
}

pub struct Verifier {
    pub strict: bool,
    /// Published regions and how many of their next native entries to verify.
    pub pending: HashMap<u32, u8>,
    factory: InstructionInfoFactory,
}

pub struct Trace {
    owner: u32,
    entry: u32,
    /// State before each interpreted step, with the write log length and the
    /// EFLAGS bits left undefined by the steps so far.
    states: Vec<(ArchitecturalState, usize, u32)>,
    writes: Vec<Written>,
    /// The interpreter stopped before an instruction it could not trace, which
    /// native code may have executed; a difference is then inconclusive.
    cut: bool,
}

struct Written {
    address: u32,
    old: Vec<u8>,
    new: Vec<u8>,
}

impl Verifier {
    pub fn from_env() -> Option<Box<Self>> {
        let mode = std::env::var("ADV_JIT_VERIFY").ok()?;
        match mode.as_str() {
            "" | "0" => None,
            mode => Some(Box::new(Self::new(mode == "strict"))),
        }
    }

    pub fn new(strict: bool) -> Self {
        Self {
            strict,
            pending: HashMap::new(),
            factory: InstructionInfoFactory::new(),
        }
    }

    pub fn take(&mut self, owner: u32) {
        if let Some(left) = self.pending.get_mut(&owner) {
            *left -= 1;
            if *left == 0 {
                self.pending.remove(&owner);
            }
        }
    }

    /// Interpret from the current state, then restore the state and memory.
    pub fn record(
        &mut self,
        owner: u32,
        stop: u32,
        interpreter: &mut Interpreter,
        state: &mut CpuState,
        memory: &mut Memory,
    ) -> Result<Trace, String> {
        let start = state.clone();
        let mut trace = Trace {
            owner,
            entry: state.eip,
            states: Vec::new(),
            writes: Vec::new(),
            cut: true,
        };
        let mut undefined = 0;
        for step in 0..=MAX_STEPS {
            trace.states.push((
                state.clone().capture_architecture(),
                trace.writes.len(),
                undefined,
            ));
            if step == MAX_STEPS || state.eip == stop {
                trace.cut = false;
                break;
            }
            let Some(i) = decode(memory, state.eip) else {
                break;
            };
            let Some(footprint) = writes(&mut self.factory, &i, state, memory) else {
                break;
            };
            let Some(old) = footprint
                .iter()
                .map(|&(address, len)| memory.read_data(address, len).ok().map(|b| b.into_owned()))
                .collect::<Option<Vec<_>>>()
            else {
                break;
            };
            let execution = interpreter.run_chunk(state, memory, 1);
            if execution.step != Step::Executed || execution.units != 1 {
                // Undo whatever a stopped instruction committed; no recorded
                // state follows it, so its new bytes are never expected.
                for (&(address, len), old) in footprint.iter().zip(old) {
                    let new = memory.read_data(address, len)?.into_owned();
                    trace.writes.push(Written { address, old, new });
                }
                break;
            }
            undefined = undefined & !eflags(i.rflags_modified()) | eflags(i.rflags_undefined());
            for (&(address, len), old) in footprint.iter().zip(old) {
                let new = memory.read_data(address, len)?.into_owned();
                trace.writes.push(Written { address, old, new });
            }
        }
        for written in trace.writes.iter().rev() {
            memory.write(written.address, &written.old)?;
        }
        *state = start;
        Ok(trace)
    }
}

impl Trace {
    /// Compare the native exit with interpreted states at its PC. `None` when
    /// no interpreted state reached it, or none matched a cut trace.
    pub fn check(&self, state: &CpuState, memory: &Memory) -> Option<Result<Check, String>> {
        let native = state.clone().capture_architecture();
        let mut best: Option<Vec<String>> = None;
        for recorded in self.states.iter().filter(|(s, ..)| s.eip == native.eip) {
            match self.compare(recorded, &native, memory) {
                Ok(check) => return Some(Ok(check)),
                Err(diff) => {
                    if best.as_ref().is_none_or(|b| diff.len() < b.len()) {
                        best = Some(diff);
                    }
                }
            }
        }
        let diff = best.filter(|_| !self.cut)?;
        Some(Err(self.report(&native, &diff)))
    }

    /// Compare the native exit after `retired` instructions with the
    /// interpreted state after as many: for engines that count what a run
    /// retired, whose units may loop. `None` when the interpreter did not
    /// get that far.
    pub fn check_retired(
        &self,
        retired: usize,
        state: &CpuState,
        memory: &Memory,
    ) -> Option<Result<Check, String>> {
        let native = state.clone().capture_architecture();
        let recorded = self.states.get(retired)?;
        Some(
            self.compare(recorded, &native, memory)
                .map_err(|diff| self.report(&native, &diff)),
        )
    }

    fn compare(
        &self,
        (interpreted, logged, undefined): &(ArchitecturalState, usize, u32),
        native: &ArchitecturalState,
        memory: &Memory,
    ) -> Result<Check, Vec<String>> {
        let mut interpreted = interpreted.clone();
        interpreted.eflags = interpreted.eflags & !undefined | native.eflags & undefined;
        let mut mxcsr = false;
        if interpreted.mxcsr & !MXCSR_FLAGS == native.mxcsr & !MXCSR_FLAGS
            && interpreted.mxcsr != native.mxcsr
        {
            interpreted.mxcsr = native.mxcsr;
            mxcsr = true;
        }
        let mut diff = differences(&interpreted, native);
        diff.extend(self.memory_differences(*logged, memory));
        if !diff.is_empty() {
            return Err(diff);
        }
        Ok(if mxcsr {
            Check::MxcsrFlags
        } else {
            Check::Equal
        })
    }

    fn report(&self, native: &ArchitecturalState, diff: &[String]) -> String {
        format!(
            "region {:#x} entry {:#x} exit {:#x}: {}",
            self.owner,
            self.entry,
            native.eip,
            diff.join(", ")
        )
    }

    /// Bytes whose native value differs from the interpreted memory after the
    /// first `logged` writes.
    fn memory_differences(&self, logged: usize, memory: &Memory) -> Vec<String> {
        let mut expected = BTreeMap::new();
        for written in self.writes.iter().rev() {
            for (offset, &byte) in written.old.iter().enumerate() {
                expected.insert(written.address.wrapping_add(offset as u32), byte);
            }
        }
        for written in &self.writes[..logged] {
            for (offset, &byte) in written.new.iter().enumerate() {
                expected.insert(written.address.wrapping_add(offset as u32), byte);
            }
        }
        let mut diff = Vec::new();
        for (address, byte) in expected {
            let actual = memory.read_data(address, 1).ok().map(|b| b[0]);
            if actual != Some(byte) {
                diff.push(format!(
                    "[{address:#x}] native={} interpreter={byte:#04x}",
                    actual.map_or_else(|| "unreadable".into(), |b| format!("{b:#04x}"))
                ));
            }
        }
        diff
    }
}

/// MXCSR exception flags (IE, DE, ZE, OE, UE, PE).
const MXCSR_FLAGS: u32 = 0x3f;

#[derive(Debug)]
pub enum Check {
    Equal,
    /// Equal except for MXCSR exception flags.
    MxcsrFlags,
}

/// The EFLAGS bits of iced `RflagsBits`.
fn eflags(bits: u32) -> u32 {
    [
        (RflagsBits::CF, 0x1),
        (RflagsBits::PF, 0x4),
        (RflagsBits::AF, 0x10),
        (RflagsBits::ZF, 0x40),
        (RflagsBits::SF, 0x80),
        (RflagsBits::OF, 0x800),
    ]
    .into_iter()
    .filter(|&(bit, _)| bits & bit != 0)
    .fold(0, |flags, (_, flag)| flags | flag)
}

fn differences(interpreted: &ArchitecturalState, native: &ArchitecturalState) -> Vec<String> {
    const NAMES: [&str; 8] = ["eax", "ecx", "edx", "ebx", "esp", "ebp", "esi", "edi"];
    let mut diff = Vec::new();
    if interpreted.eip != native.eip {
        diff.push(format!(
            "eip native={:#x} interpreter={:#x}",
            native.eip, interpreted.eip
        ));
    }
    for (index, name) in NAMES.iter().enumerate() {
        if interpreted.regs[index] != native.regs[index] {
            diff.push(format!(
                "{name} native={:#x} interpreter={:#x}",
                native.regs[index], interpreted.regs[index]
            ));
        }
    }
    if interpreted.eflags != native.eflags {
        diff.push(format!(
            "eflags native={:#x} interpreter={:#x}",
            native.eflags, interpreted.eflags
        ));
    }
    for (index, (a, b)) in interpreted.xmm.iter().zip(&native.xmm).enumerate() {
        if a != b {
            diff.push(format!("xmm{index}"));
        }
    }
    if interpreted.mxcsr != native.mxcsr {
        diff.push(format!(
            "mxcsr native={:#x} interpreter={:#x}",
            native.mxcsr, interpreted.mxcsr
        ));
    }
    // Architectural snapshots materialize MMX writes into physical raw80.
    // Comparing x87 therefore includes MMX payloads, exponents, TOP and tags,
    // without comparing a potentially stale nonarchitectural MMX cache.
    if interpreted.x87 != native.x87 {
        let mut text = String::from("x87");
        let (a, b) = (&interpreted.x87, &native.x87);
        if a.control != b.control {
            let _ = write!(
                text,
                " control native={:#x} interpreter={:#x}",
                b.control, a.control
            );
        }
        if a.status != b.status {
            let _ = write!(
                text,
                " status native={:#x} interpreter={:#x}",
                b.status, a.status
            );
        }
        if a.top() != b.top() {
            let _ = write!(text, " top native={} interpreter={}", b.top(), a.top());
        }
        if a.tag != b.tag {
            let _ = write!(text, " tag native={:#x} interpreter={:#x}", b.tag, a.tag);
        }
        for (physical, (a, b)) in a.regs.iter().zip(&b.regs).enumerate() {
            if a != b {
                let _ = write!(text, " raw80 physical{physical}/mm{physical}");
            }
        }
        diff.push(text);
    }
    diff
}

fn decode(memory: &Memory, pc: u32) -> Option<Instruction> {
    let bytes = (1..=15)
        .rev()
        .find_map(|len| memory.read_data(pc, len).ok())?;
    let i = Decoder::with_ip(32, &bytes, u64::from(pc), DecoderOptions::NONE).decode();
    (!i.is_invalid()).then_some(i)
}

/// Bytes the instruction may write, or `None` when the trace must stop:
/// Unresolved addresses, code pages and memory shared with host threads cannot
/// be restored privately.
fn writes(
    factory: &mut InstructionInfoFactory,
    i: &Instruction,
    state: &CpuState,
    memory: &Memory,
) -> Option<Vec<(u32, usize)>> {
    if i.is_string_instruction() && (i.has_rep_prefix() || i.has_repne_prefix()) {
        return None;
    }
    let value = |r: Register, _: usize, _: usize| -> Option<u64> {
        if r == Register::FS {
            return Some(u64::from(state.fs_base));
        }
        if r.is_segment_register() {
            return Some(0);
        }
        if r.is_gpr32() {
            return Some(u64::from(state.regs[r.number()]));
        }
        if r.is_gpr16() {
            return Some(u64::from(state.regs[r.number()] & 0xffff));
        }
        None
    };
    let mut footprint = Vec::new();
    for used in factory.info(i).used_memory() {
        if !matches!(
            used.access(),
            OpAccess::Write | OpAccess::CondWrite | OpAccess::ReadWrite | OpAccess::ReadCondWrite
        ) {
            continue;
        }
        let address = used.virtual_address(0, value)? as u32;
        let len = used.memory_size().size();
        if len == 0 {
            return None;
        }
        let last = address.wrapping_add(len as u32 - 1);
        if memory.executable(address, 1)
            || memory.executable(last, 1)
            || memory.shared_range(address, len)
        {
            return None;
        }
        footprint.push((address, len));
    }
    Some(footprint)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EXEC, READ, WRITE};

    const CODE: u32 = 0x1000;
    const DATA: u32 = 0x2000;

    fn traced(code: &[u8]) -> (Trace, CpuState, Memory) {
        let mut memory = Memory::new(CODE, 0x2000).unwrap();
        memory.protect(CODE, 0x2000, READ | WRITE).unwrap();
        memory.write(CODE, code).unwrap();
        memory.protect(CODE, 0x1000, READ | EXEC).unwrap();
        let mut state = CpuState {
            eip: CODE,
            regs: [1, 0, 0, 0, DATA + 0x800, 0, 0, DATA],
            ..CpuState::default()
        };
        let before = state.clone();
        let trace = Verifier::new(true)
            .record(CODE, 0, &mut Interpreter::new(), &mut state, &mut memory)
            .unwrap();
        assert_eq!(state.regs, before.regs, "record restores the state");
        assert_eq!(memory.read(DATA, 4).unwrap(), [0; 4], "and memory");
        // The native side: replay the same instructions in the interpreter.
        let mut interpreter = Interpreter::new();
        for _ in 0..3 {
            assert_eq!(interpreter.step(&mut state, &mut memory), Step::Executed);
        }
        (trace, state, memory)
    }

    // add eax, eax; mov [edi], eax; push eax; ud2
    const CODE_BYTES: [u8; 7] = [0x01, 0xc0, 0x89, 0x07, 0x50, 0x0f, 0x0b];

    #[test]
    fn matching_exit_verifies_and_restores_before_native_execution() {
        let (trace, state, memory) = traced(&CODE_BYTES);
        assert!(trace.cut, "the trace stops before UD2");
        assert!(matches!(
            trace.check(&state, &memory),
            Some(Ok(Check::Equal))
        ));
    }

    #[test]
    fn register_and_memory_differences_are_reported() {
        let (mut trace, mut state, mut memory) = traced(&CODE_BYTES);
        trace.cut = false;
        state.regs[0] = 3;
        let report = trace.check(&state, &memory).unwrap().unwrap_err();
        assert!(
            report.contains("eax native=0x3 interpreter=0x2"),
            "{report}"
        );
        state.regs[0] = 2;
        memory.write(DATA, &[9]).unwrap();
        let report = trace.check(&state, &memory).unwrap().unwrap_err();
        assert!(report.contains("native=0x09 interpreter=0x02"), "{report}");
    }

    #[test]
    fn strict_comparison_observes_vectors_and_canonical_mmx_aliases() {
        let mut cpu = CpuState::default();
        let before = cpu.capture_architecture();
        cpu.xmm[7][15] = 0x80;
        cpu.mmx[3] = 0x1234;
        cpu.mmx_mode = 5 | (1 << 11);
        let after = cpu.capture_architecture();
        let report = differences(&before, &after).join(" ");
        assert!(report.contains("xmm7"), "{report}");
        assert!(report.contains("physical3/mm3"), "{report}");
        assert!(report.contains("tag native="), "{report}");
        let mut changed = after.clone();
        changed.x87.set_top(5);
        assert!(
            differences(&after, &changed)
                .join(" ")
                .contains("status native=")
        );
        changed = after.clone();
        changed.x87.regs[3] = crate::X87Register::from_parts(0x1234, 0x3fff);
        assert!(
            differences(&after, &changed)
                .join(" ")
                .contains("physical3/mm3")
        );
    }

    #[test]
    fn x87_verification_detects_raw80_top_tags_and_every_status_bit() {
        let (mut trace, state, memory) = traced(&CODE_BYTES);
        trace.cut = false;
        for part in 0..27 {
            let mut changed = state.clone();
            let expected = match part {
                0..=7 => {
                    changed.x87.regs[part].0[0] ^= 1;
                    "raw80"
                }
                8 => {
                    changed.x87.set_top((state.x87.top() + 1) & 7);
                    "top"
                }
                9 => {
                    changed.x87.tag ^= 1;
                    "tag"
                }
                10 => {
                    changed.x87.control ^= 0x400;
                    "control"
                }
                _ => {
                    changed.x87.status ^= 1 << (part - 11);
                    "status"
                }
            };
            let report = trace.check(&changed, &memory).unwrap().unwrap_err();
            assert!(report.contains(expected), "{report}");
        }
    }

    #[test]
    fn undefined_flags_and_mxcsr_exception_flags_are_not_mismatches() {
        let (mut trace, mut state, memory) = traced(&CODE_BYTES);
        trace.cut = false;
        // ADD defines every arithmetic flag, so a flipped AF is a mismatch.
        state.eflags ^= 0x10;
        assert!(trace.check(&state, &memory).unwrap().is_err());
        for (.., undefined) in &mut trace.states {
            *undefined = 0x10;
        }
        assert!(matches!(
            trace.check(&state, &memory),
            Some(Ok(Check::Equal))
        ));
        state.mxcsr |= 0x20;
        assert!(matches!(
            trace.check(&state, &memory),
            Some(Ok(Check::MxcsrFlags))
        ));
    }
}
