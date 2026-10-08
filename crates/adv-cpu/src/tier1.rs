//! Explicit ARM64 Tier 1 lane: bounded integer/SIMD templates and guarded x87 helper calls.
//! Memory operations retain the existing checked Memory contract, through
//! helper calls from native code and an exit when a helper refuses. No hardware
//! fault recovery, floating approximation or parallel guest execution is enabled.
#![cfg_attr(
    not(all(
        target_arch = "aarch64",
        any(target_os = "linux", target_os = "android", target_os = "macos")
    )),
    allow(dead_code)
)]
use iced_x86::{FlowControl, Instruction};
use std::collections::BTreeMap;
use std::time::Duration;

/// Selection is fixed before execution. Bridge preserves the caller's automatic
/// and asynchronous Cranelift policy; Exclusive retains the original opt-in lane.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tier1Mode {
    #[default]
    Off,
    Exclusive,
    Bridge,
}

impl std::str::FromStr for Tier1Mode {
    type Err = String;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "off" => Ok(Self::Off),
            "exclusive" => Ok(Self::Exclusive),
            "bridge" => Ok(Self::Bridge),
            _ => Err("ADV_TIER1 must be bridge, exclusive or off".into()),
        }
    }
}

pub(crate) mod census;
pub(crate) mod forms;
mod x87;

pub(crate) const DISPATCH: u32 = 0;
pub(crate) const FALLBACK: u32 = 1;
pub(crate) const BUDGET: u32 = 2;
pub(crate) const LOAD: u32 = 3;
pub(crate) const STORE: u32 = 4;

/// A suffix of one statically bounded, strictly forward straight-line batch.
/// Only its sequential edges may skip a budget guard; all other edges reenter
/// an ordinary checked entry. Charges and stop guards remain per instruction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PollBatch {
    pub units: u32,
    pub next: Option<u32>,
}

pub(super) fn poll_batches(instructions: &BTreeMap<u32, Instruction>) -> BTreeMap<u32, PollBatch> {
    fn finish(pcs: &mut Vec<u32>, batches: &mut BTreeMap<u32, PollBatch>) {
        if pcs.len() > 1 {
            for (index, &pc) in pcs.iter().enumerate() {
                batches.insert(
                    pc,
                    PollBatch {
                        units: (pcs.len() - index) as u32,
                        next: pcs.get(index + 1).copied(),
                    },
                );
            }
        }
        pcs.clear();
    }
    let mut batches = BTreeMap::new();
    let mut pcs = Vec::new();
    let mut next = None;
    for (&pc, instruction) in instructions {
        if next != Some(pc) || pcs.len() == 32 {
            finish(&mut pcs, &mut batches);
        }
        if forms::supported(instruction)
            && instruction.flow_control() == FlowControl::Next
            && !instruction.has_rep_prefix()
            && !instruction.has_repne_prefix()
            && instruction.next_ip32() > pc
        {
            pcs.push(pc);
            next = Some(instruction.next_ip32());
        } else {
            finish(&mut pcs, &mut batches);
            next = None;
        }
    }
    finish(&mut pcs, &mut batches);
    batches
}

#[derive(Clone, Debug, Default)]
pub struct Tier1Stats {
    /// Retired native units selected as a Cranelift waiting bridge, excluding fallback.
    pub bridge_units: u64,
    pub bridge_entries: u64,
    /// Live RX mappings, including page rounding; code_bytes is cumulative emission.
    pub resident_bytes: usize,
    pub cache_evictions: u64,
    pub cache_rejections: u64,
    /// Cranelift revisions observed, including revisions with an empty cache.
    pub publication_revisions: u64,
    /// Nonempty whole-cache retirements caused by Cranelift publication.
    pub publication_flushes: u64,
    pub compile_misses: u64,
    /// Entries returning through Tier 0, including unsupported native exits.
    pub fallback_entries: u64,
    pub compilations: u64,
    pub compile_time: Duration,
    pub code_bytes: u64,
    pub native_entries: u64,
    pub native_instructions: u64,
    pub direct_transfers: u64,
    pub saves: u64,
    pub restores: u64,
    pub fallback_instructions: u64,
    /// Accesses that left native code for the checked Rust exit.
    pub checked_accesses: u64,
    /// Accesses completed by a helper call without leaving native code.
    pub inline_accesses: u64,
    pub invalidations: u64,
}

#[cfg(all(
    target_arch = "aarch64",
    any(target_os = "linux", target_os = "android", target_os = "macos")
))]
mod emit;
#[cfg(all(
    target_arch = "aarch64",
    any(target_os = "linux", target_os = "android", target_os = "macos")
))]
mod native;
#[cfg(all(
    target_arch = "aarch64",
    any(target_os = "linux", target_os = "android", target_os = "macos")
))]
mod simd;

#[cfg(all(
    target_arch = "aarch64",
    any(target_os = "linux", target_os = "android", target_os = "macos")
))]
mod sse;

pub(crate) struct Execution {
    pub execution: crate::interpreter::Execution,
    pub interpreted_units: u32,
    pub interpreted: bool,
    pub last_units: u32,
}

#[cfg(all(
    target_arch = "aarch64",
    any(target_os = "linux", target_os = "android", target_os = "macos")
))]
pub(crate) use native::Engine;

#[cfg(not(all(
    target_arch = "aarch64",
    any(target_os = "linux", target_os = "android", target_os = "macos")
)))]
pub(crate) struct Engine;
#[cfg(not(all(
    target_arch = "aarch64",
    any(target_os = "linux", target_os = "android", target_os = "macos")
)))]
impl Engine {
    pub fn record_bridge(&mut self, _units: u32) {}
    pub fn set_cache_limits(&mut self, _entries: usize, _bytes: usize) {}
    pub fn new() -> Result<Self, String> {
        Err(
            "ARM64 Tier 1 requires aarch64 Linux, Android or macOS; other platforms are not supported"
                .into(),
        )
    }
    pub fn stats(&self) -> Tier1Stats {
        Tier1Stats::default()
    }
    pub fn run_block(
        &mut self,
        _state: &mut crate::CpuState,
        _memory: &mut crate::Memory,
        _interpreter: &mut crate::interpreter::Interpreter,
        _units: u32,
        _stop: u32,
        _interrupt: &std::sync::atomic::AtomicBool,
        _boundaries: Option<(&std::collections::BTreeMap<u32, u32>, u64)>,
    ) -> Result<Execution, String> {
        unreachable!("unsupported Tier 1 cannot be selected")
    }
}

#[cfg(test)]
mod poll_tests {
    use super::*;
    use iced_x86::code_asm::*;
    use iced_x86::{Decoder, DecoderOptions};

    fn instructions(f: impl FnOnce(&mut CodeAssembler)) -> BTreeMap<u32, Instruction> {
        let mut assembler = CodeAssembler::new(32).unwrap();
        f(&mut assembler);
        let bytes = assembler.assemble(0x10000).unwrap();
        Decoder::with_ip(32, &bytes, 0x10000, DecoderOptions::NONE)
            .into_iter()
            .map(|instruction| (instruction.ip32(), instruction))
            .collect()
    }

    #[test]
    fn straight_batches_bound_every_external_entry_suffix() {
        let instructions = instructions(|a| {
            for _ in 0..70 {
                a.add(eax, 1).unwrap();
            }
            a.ret().unwrap();
        });
        let batches = poll_batches(&instructions);
        let pcs: Vec<_> = instructions.keys().copied().collect();
        for (index, &pc) in pcs[..70].iter().enumerate() {
            let batch_end = ((index / 32 + 1) * 32).min(70);
            assert_eq!(batches[&pc].units, (batch_end - index) as u32);
            assert_eq!(
                batches[&pc].next,
                (index + 1 < batch_end).then(|| pcs[index + 1])
            );
        }
        assert!(!batches.contains_key(&pcs[70]));
    }

    #[test]
    fn control_flow_and_rep_end_fast_chains() {
        let instructions = instructions(|a| {
            let mut target = a.create_label();
            a.nop().unwrap();
            a.nop().unwrap();
            a.jnz(target).unwrap();
            a.nop().unwrap();
            a.rep().movsb().unwrap();
            a.set_label(&mut target).unwrap();
            a.nop().unwrap();
            a.nop().unwrap();
            a.jmp(eax).unwrap();
            a.nop().unwrap();
            a.call(target).unwrap();
            a.ret().unwrap();
        });
        let batches = poll_batches(&instructions);
        let pcs: Vec<_> = instructions.keys().copied().collect();
        assert_eq!(batches.len(), 4);
        for (first, last) in [(0, 1), (5, 6)] {
            assert_eq!(
                batches[&pcs[first]],
                PollBatch {
                    units: 2,
                    next: Some(pcs[last])
                }
            );
            assert_eq!(
                batches[&pcs[last]],
                PollBatch {
                    units: 1,
                    next: None
                }
            );
        }
        for (pc, batch) in &batches {
            if let Some(next) = batch.next {
                assert_eq!(next, instructions[pc].next_ip32());
                assert!(next > *pc);
                assert_eq!(batch.units, batches[&next].units + 1);
            }
        }
    }

    #[test]
    fn missing_or_overlapping_entries_cannot_form_a_fast_edge() {
        let mut instructions = instructions(|a| {
            a.mov(eax, 123).unwrap();
            a.nop().unwrap();
            a.nop().unwrap();
            a.ret().unwrap();
        });
        let pcs: Vec<_> = instructions.keys().copied().collect();
        instructions.remove(&pcs[1]);
        assert!(poll_batches(&instructions).is_empty());
        let mut overlap = Instruction::with(iced_x86::Code::Nopd);
        overlap.set_ip(u64::from(pcs[0] + 1));
        overlap.set_len(1);
        instructions.insert(overlap.ip32(), overlap);
        assert!(poll_batches(&instructions).is_empty());
    }
}
