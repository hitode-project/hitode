//! Tier 1 x87 contract: CpuState owns all eight physical raw80 registers, full
//! two-bit tags, TOP in SW[13:11], CW (PC/RC/masks), SW (C0-C3/ES/IE/SF), and
//! FIP/FDP/FOP. No floating register is cached in AArch64 registers. Each BLR
//! reads canonical x87 state and current GPR address inputs, and uses the
//! existing guarded fast helper. Successful writes refresh MMX aliases.
//!
//! The helper stages register/tag/SW changes until checked memory succeeds.
//! RC, PC and exception masks are read from CW on every call; selected forms
//! never write CW. Inexact PC=64 is refused by the existing residual guard,
//! never approximated with binary64. Stack overflow/underflow (IE+SF and C1),
//! pending ES, unmasked exceptions and special encodings retain Tier 0's
//! transaction. Refusal leaves x87 and memory untouched, with no retirement.
//!
//! Like Tier 1 load/store, the BLR uses AAPCS64 and returns 0 to continue,
//! 1 after a completed access requiring dispatch, or 2 to rerun at the original
//! PC. Its checked 4/8-byte memory operations use floating::fast's existing
//! Memory access contract (including permissions, shared/file refusal, complete
//! cross-page checks and code invalidation), not two independently committed
//! scalar accesses. A fault therefore cannot partially store or pop the stack.
use crate::floating::{NativeX87, X87_DONE, fast};
use crate::{CpuState, Memory};
use iced_x86::Instruction;
use std::collections::BTreeMap;

pub(super) struct Operations(Box<[NativeX87]>);

impl Operations {
    pub fn new(instructions: &BTreeMap<u32, Instruction>, input: &[(u32, Vec<u8>)]) -> Self {
        Self(
            instructions
                .values()
                .filter(|i| super::forms::x87(i))
                .map(|i| {
                    let bytes = &input.iter().find(|(pc, _)| *pc == i.ip32()).unwrap().1;
                    // Same FOP derivation as decoded Tier 0 and Cranelift.
                    let opcode = bytes
                        .windows(2)
                        .find(|bytes| (0xd8..=0xdf).contains(&bytes[0]))
                        .map(|bytes| (u16::from(bytes[0] & 7) << 8) | u16::from(bytes[1]));
                    NativeX87 {
                        instruction: *i,
                        opcode,
                        fast: fast::classify(i),
                    }
                })
                .collect(),
        )
    }

    pub fn get(&self, pc: u32) -> Option<u64> {
        self.0
            .binary_search_by_key(&pc, |op| op.instruction.ip32())
            .ok()
            .map(|n| std::ptr::from_ref(&self.0[n]) as u64)
    }
}

fn execute(op: &NativeX87, state: &mut CpuState, memory: &mut Memory) -> u32 {
    match op
        .fast
        .and_then(|fast| fast::execute(op, fast, state, memory))
    {
        Some(X87_DONE) => 0,
        Some(_) => 1,
        None => 2,
    }
}

#[cfg(all(
    target_arch = "aarch64",
    any(target_os = "linux", target_os = "android")
))]
pub(super) extern "C" fn call(entry: *mut super::native::Entry, op: *const NativeX87) -> u32 {
    // SAFETY: run_block exclusively owns CPU/memory and pins the immutable
    // operation array for the entire native call. Emitted code spilled GPRs.
    let (entry, op) = unsafe { (&mut *entry, &*op) };
    let (state, memory) = unsafe { (&mut *entry.state, &mut *entry.memory) };
    state.regs = entry.frame.regs;
    state.eip = op.instruction.ip32();
    let status = execute(op, state, memory);
    if status != 2 && op.instruction.op0_kind() == iced_x86::OpKind::Memory {
        entry.inline += 1;
    }
    status
}

#[cfg(test)]
mod tests;
