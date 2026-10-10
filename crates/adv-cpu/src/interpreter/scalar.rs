//! Checked ordinary-page operations with narrow, infallible commits.
use super::*;

enum Destination {
    Register(Reg),
    Memory(*mut u8, u8),
}

impl Destination {
    fn prepare(operand: Operand, state: &CpuState, memory: &Memory) -> Option<Self> {
        match operand {
            Operand::Register(r) => Some(Self::Register(r)),
            Operand::Memory(a, width) => {
                Self::memory(a.resolve_regs(&state.regs, state.fs_base), width, memory)
            }
            Operand::Immediate(_) => None,
        }
    }

    fn memory(address: u32, width: u8, memory: &Memory) -> Option<Self> {
        Some(Self::Memory(
            memory.direct(address, width as usize, true)?,
            width,
        ))
    }

    fn commit(self, value: u32, state: &mut CpuState) {
        match self {
            Self::Register(r) => r.write(&mut state.regs, value),
            Self::Memory(p, width) => {
                // SAFETY: prepare checked the entire ordinary, non-code range.
                // execute holds Memory exclusively; no intervening operation can
                // remap it, revoke access, or introduce a shared/code overlay.
                unsafe {
                    match width {
                        1 => p.write(value as u8),
                        2 => p.cast::<u16>().write_unaligned((value as u16).to_le()),
                        4 => p.cast::<u32>().write_unaligned(value.to_le()),
                        _ => unreachable!("decoded scalar width"),
                    }
                }
            }
        }
    }
}

fn read_memory(address: u32, width: u8, memory: &Memory) -> Option<u32> {
    let p = memory.direct(address, width as usize, false)?;
    // SAFETY: direct checked the full ordinary readable range and the Memory
    // borrow prevents concurrent guest writes or remapping during this read.
    Some(unsafe {
        match width {
            1 => u32::from(p.read()),
            2 => u32::from(u16::from_le(p.cast::<u16>().read_unaligned())),
            4 => u32::from_le(p.cast::<u32>().read_unaligned()),
            _ => unreachable!("decoded scalar width"),
        }
    })
}

fn read(operand: Operand, state: &CpuState, memory: &Memory) -> Option<u32> {
    match operand {
        Operand::Register(r) => Some(r.read(&state.regs)),
        Operand::Immediate(value) => Some(value),
        Operand::Memory(a, width) => {
            read_memory(a.resolve_regs(&state.regs, state.fs_base), width, memory)
        }
    }
}

pub(super) fn eligible(action: Action) -> bool {
    matches!(
        action,
        Action::Mov(..)
            | Action::Binary(..)
            | Action::Unary(..)
            | Action::Shift(..)
            | Action::Multiply(Operand::Register(_), Some(_), _, _)
            | Action::Set(..)
            | Action::Branch(..)
            | Action::Push(..)
            | Action::Pop(Operand::Register(_), _)
            | Action::Call(..)
            | Action::Ret(..)
    )
}

/// Decline without changing state or memory. Shared, executable writes,
/// cross-page and denied ranges retain the full checked transaction, including
/// its fault priority. Every successful arm validates before its first commit.
pub(super) fn execute(i: &Predecoded, state: &mut CpuState, memory: &mut Memory) -> Option<()> {
    let mut next = i.next;
    match i.action {
        Action::Mov(dst, src, extend) => {
            let value = read(src, state, memory)?;
            let destination = Destination::prepare(dst, state, memory)?;
            let value = if extend {
                signed(value, src.width()?) as u32
            } else {
                value
            };
            destination.commit(value, state);
        }
        Action::Binary(kind, dst, src) => {
            let a = read(dst, state, memory)?;
            let b = read(src, state, memory)?;
            let destination = if matches!(kind, M::Cmp | M::Test) {
                None
            } else {
                Some(Destination::prepare(dst, state, memory)?)
            };
            let result = registers::binary(&mut state.eflags, kind, a, b, dst.width()?);
            if let Some(destination) = destination {
                destination.commit(result, state);
            }
        }
        Action::Unary(kind, dst) => {
            let value = read(dst, state, memory)?;
            let destination = Destination::prepare(dst, state, memory)?;
            let result = registers::unary(&mut state.eflags, kind, value, dst.width()?);
            destination.commit(result, state);
        }
        Action::Shift(kind, dst, count) => {
            let value = read(dst, state, memory)?;
            let count = read(count, state, memory)? & 31;
            // Even a zero count requires a writable memory destination.
            let destination = Destination::prepare(dst, state, memory)?;
            if count != 0 {
                let result = registers::shift(&mut state.eflags, kind, value, count, dst.width()?);
                destination.commit(result, state);
            }
        }
        Action::Multiply(Operand::Register(dst), Some(src), immediate, signed) => {
            let (a, b) = if let Some(imm) = immediate {
                (read(src, state, memory)?, read(imm, state, memory)?)
            } else {
                (dst.read(&state.regs), read(src, state, memory)?)
            };
            let product = registers::product(&mut state.eflags, a, b, dst.width, signed);
            dst.write(&mut state.regs, product as u32);
        }
        Action::Set(dst, condition) => {
            let destination = Destination::prepare(dst, state, memory)?;
            destination.commit(u32::from(condition_true(condition, state.eflags)), state);
        }
        Action::Branch(target, condition) => {
            if condition == ConditionCode::None || condition_true(condition, state.eflags) {
                next = read(target, state, memory)?;
                if !matches!(target, Operand::Immediate(_)) {
                    state.last_indirect_branch = crate::IndirectBranch {
                        source: state.eip,
                        target: next,
                        kind: crate::IndirectBranchKind::Jump,
                    };
                }
            }
        }
        Action::Push(src, width) => {
            let value = read(src, state, memory)?;
            let address = state.regs[4].wrapping_sub(u32::from(width));
            let destination = Destination::memory(address, width, memory)?;
            destination.commit(value, state);
            state.regs[4] = address;
        }
        Action::Pop(Operand::Register(dst), width) => {
            let value = read_memory(state.regs[4], width, memory)?;
            state.regs[4] = state.regs[4].wrapping_add(u32::from(width));
            dst.write(&mut state.regs, value);
        }
        Action::Call(target) => {
            next = read(target, state, memory)?;
            let address = state.regs[4].wrapping_sub(4);
            let destination = Destination::memory(address, 4, memory)?;
            destination.commit(i.next, state);
            state.regs[4] = address;
            if !matches!(target, Operand::Immediate(_)) {
                state.last_indirect_branch = crate::IndirectBranch {
                    source: state.eip,
                    target: next,
                    kind: crate::IndirectBranchKind::Call,
                };
            }
        }
        Action::Ret(bytes) => {
            next = read_memory(state.regs[4], 4, memory)?;
            state.regs[4] = state.regs[4].wrapping_add(4 + u32::from(bytes));
            state.last_indirect_branch = crate::IndirectBranch {
                source: state.eip,
                target: next,
                kind: crate::IndirectBranchKind::Return,
            };
        }
        _ => return None,
    }
    state.eip = next;
    Some(())
}

#[cfg(test)]
#[path = "scalar_tests.rs"]
mod tests;
