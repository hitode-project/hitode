//! Proof for straight-line spin loops. No register carried between iterations
//! may advance the address or the exit condition.

use iced_x86::{Decoder, DecoderOptions, Instruction, Mnemonic as M, OpKind, Register};

pub const MAX_BYTES: usize = 64;

/// `bytes` start at the backedge's target and include the branch at `latch`.
pub fn backedge(bytes: &[u8], target: u32, latch: u32) -> bool {
    if bytes.len() > MAX_BYTES {
        return false;
    }
    let mut decoder = Decoder::with_ip(32, bytes, u64::from(target), DecoderOptions::NONE);
    let mut pause = false;
    let mut loaded = None;
    let mut compared = false;
    while decoder.can_decode() {
        let i = decoder.decode();
        if i.ip32() == latch {
            return i.op0_kind() == OpKind::NearBranch32
                && i.near_branch32() == target
                && ((i.mnemonic() == M::Jmp && pause && loaded.is_none() && !compared)
                    || (matches!(i.mnemonic(), M::Je | M::Jne)
                        && (compared || pause && loaded.is_none())));
        }
        if i.ip32() > latch || i.has_lock_prefix() {
            return false;
        }
        match i.mnemonic() {
            M::Nop => {}
            M::Pause => pause = true,
            M::Mov | M::Movzx | M::Movsx
                if loaded.is_none()
                    && !compared
                    && i.op0_kind() == OpKind::Register
                    && i.op1_kind() == OpKind::Memory =>
            {
                let dst = i.op0_register().full_register32();
                if !dst.is_gpr32()
                    || dst == Register::ESP
                    || dst == i.memory_base().full_register32()
                    || dst == i.memory_index().full_register32()
                {
                    return false;
                }
                loaded = Some(i.op0_register());
            }
            M::Cmp | M::Test if !compared && comparison(&i, loaded) => compared = true,
            _ => return false,
        }
    }
    false
}

fn comparison(i: &Instruction, loaded: Option<Register>) -> bool {
    let [a, b] = [i.op0_kind(), i.op1_kind()];
    let value = |kind| {
        matches!(
            kind,
            OpKind::Register
                | OpKind::Immediate8
                | OpKind::Immediate8to16
                | OpKind::Immediate8to32
                | OpKind::Immediate16
                | OpKind::Immediate32
        )
    };
    if let Some(reg) = loaded {
        let loaded_bits = |r: Register| {
            r == reg
                || reg.size() > 1
                    && r.size() <= reg.size()
                    && r.full_register32() == reg.full_register32()
        };
        (a == OpKind::Register && loaded_bits(i.op0_register()) && value(b)
            || b == OpKind::Register && loaded_bits(i.op1_register()) && value(a))
            && (i.mnemonic() == M::Test || i.op0_register() != i.op1_register())
    } else {
        a == OpKind::Memory && value(b) || b == OpKind::Memory && value(a)
    }
}
