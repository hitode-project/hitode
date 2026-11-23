//! Proof for straight-line spin loops. No register carried between iterations
//! may advance the address or the exit condition.

use iced_x86::{Decoder, DecoderOptions, Instruction, Mnemonic as M, OpKind, Register};

pub const MAX_BYTES: usize = 64;

/// `bytes` start at the backedge's target and include the branch at `latch`.
pub fn backedge(bytes: &[u8], target: u32, latch: u32) -> bool {
    if bytes.len() > MAX_BYTES {
        return false;
    }
    let decoder = Decoder::with_ip(32, bytes, u64::from(target), DecoderOptions::NONE);
    decoded_backedge(decoder, target, latch)
}

/// Whether `bytes` at `target` begin with a proved straight-line spin loop
/// whose latch branches back to `target` (`A64-CTL-15`).
pub fn starts_at(bytes: &[u8], target: u32) -> bool {
    let bytes = &bytes[..bytes.len().min(MAX_BYTES)];
    let decoder = Decoder::with_ip(32, bytes, u64::from(target), DecoderOptions::NONE);
    let mut state = State::default();
    for i in decoder {
        if i.is_invalid() {
            return false;
        }
        if state.closes(&i, target) {
            return true;
        }
        if !state.step(&i) {
            return false;
        }
    }
    false
}

/// The same proof over a previously decoded contiguous instruction sequence.
pub fn decoded_backedge(
    instructions: impl IntoIterator<Item = Instruction>,
    target: u32,
    latch: u32,
) -> bool {
    let mut state = State::default();
    for i in instructions {
        if i.ip32() == latch {
            return state.closes(&i, target);
        }
        if i.ip32() > latch || !state.step(&i) {
            return false;
        }
    }
    false
}

#[derive(Default)]
struct State {
    pause: bool,
    loaded: Option<Register>,
    compared: bool,
}

impl State {
    fn closes(&self, i: &Instruction, target: u32) -> bool {
        i.op0_kind() == OpKind::NearBranch32
            && i.near_branch32() == target
            && ((i.mnemonic() == M::Jmp && self.pause && self.loaded.is_none() && !self.compared)
                || (matches!(i.mnemonic(), M::Je | M::Jne)
                    && (self.compared || self.pause && self.loaded.is_none())))
    }

    fn step(&mut self, i: &Instruction) -> bool {
        if i.has_lock_prefix() {
            return false;
        }
        match i.mnemonic() {
            M::Nop => true,
            M::Pause => {
                self.pause = true;
                true
            }
            M::Mov | M::Movzx | M::Movsx
                if self.loaded.is_none()
                    && !self.compared
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
                self.loaded = Some(i.op0_register());
                true
            }
            M::Cmp | M::Test if !self.compared && comparison(i, self.loaded) => {
                self.compared = true;
                true
            }
            _ => false,
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    // spec: A64-CTL-15
    #[test]
    fn starts_at_matches_only_proved_spin_loops() {
        // mov eax, [ebx]; cmp eax, 0; je -7
        assert!(starts_at(
            &[0x8b, 0x03, 0x83, 0xf8, 0x00, 0x74, 0xf9],
            0x1000
        ));
        // pause; jmp -4
        assert!(starts_at(&[0xf3, 0x90, 0xeb, 0xfc], 0x1000));
        // nop; mov [ebx+4], esi (store is not a spin loop)
        assert!(!starts_at(&[0x90, 0x89, 0x73, 0x04, 0xeb, 0xfa], 0x1000));
    }
}
