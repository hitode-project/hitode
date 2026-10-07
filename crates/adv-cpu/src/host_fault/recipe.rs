//! Cold-only scalar recovery. No guest memory is read or instruction replayed.
use crate::CpuState;

/// Architectural register access independent of the operating system context.
/// Implementations read a frozen snapshot, never guest memory.
pub(crate) trait HostRegisters {
    fn gpr(&self, register: u8) -> Option<u64>;
}

pub(crate) const MAX_NODES: usize = 128;
pub(crate) const MAX_DEPTH: usize = 24;

#[derive(Clone, Copy, Debug)]
pub(crate) enum Input {
    Register(u8),
    Flags,
    Fuel,
    FsBase,
    Zero,
    Carry,
}
#[derive(Clone, Copy, Debug)]
pub(crate) enum Operation {
    Add,
    Sub,
    Mul,
    And,
    Or,
    Xor,
    Shl,
    Ushr,
    Sshr,
    Not,
    Neg,
    Cast,
    SignExtend(u8),
    UnsignedLessThan,
}
#[derive(Clone, Copy, Debug)]
pub(crate) enum Source {
    Input(Input),
    Constant(u64),
    HostRegister(u8),
    Calculate(Operation, u8, u8),
}
#[derive(Clone, Copy, Debug)]
pub(crate) struct Node {
    pub source: Source,
    pub bits: u8,
}
#[derive(Debug)]
pub(crate) struct Recipe {
    pub entry_pc: u32,
    pub nodes: Vec<Node>,
    // GPRs, raw flags, seven lazy flag operands, fuel, effective address.
    pub outputs: [u8; 18],
}
pub(crate) struct Recovered {
    pub regs: [u32; 8],
    pub flags: u32,
    pub fuel: u64,
    pub address: u32,
}

fn mask(bits: u8) -> u64 {
    u64::MAX >> (64 - bits)
}
fn signed(value: u64, bits: u8) -> i64 {
    ((value << (64 - bits)) as i64) >> (64 - bits)
}

impl Recipe {
    pub fn valid(&self) -> bool {
        let mut depths = [0u8; MAX_NODES];
        if self.nodes.is_empty() || self.nodes.len() > MAX_NODES {
            return false;
        }
        for (index, node) in self.nodes.iter().enumerate() {
            if !matches!(node.bits, 8 | 16 | 32 | 64) {
                return false;
            }
            depths[index] = match node.source {
                Source::Input(Input::Register(register)) if register >= 8 => return false,
                Source::HostRegister(register) if register >= 32 => return false,
                Source::Calculate(operation, left, right) => {
                    if usize::from(left) >= index || usize::from(right) >= index {
                        return false;
                    }
                    if let Operation::SignExtend(bits) = operation
                        && !matches!(bits, 8 | 16 | 32 | 64)
                    {
                        return false;
                    }
                    depths[left as usize].max(depths[right as usize]) + 1
                }
                _ => 1,
            };
            if usize::from(depths[index]) > MAX_DEPTH {
                return false;
            }
        }
        self.outputs.iter().enumerate().all(|(output, &index)| {
            self.nodes
                .get(index as usize)
                .is_some_and(|node| node.bits == if output == 16 { 64 } else { 32 })
        })
    }

    /// All reads finish before the caller changes architectural state.
    /// The execution owner proves all Input nodes still describe the entry
    /// checkpoint. HostRegister nodes describe the actual stopped instruction.
    pub fn evaluate(&self, state: &CpuState, registers: &impl HostRegisters) -> Option<Recovered> {
        if state.eip != self.entry_pc || !self.valid() {
            return None;
        }
        let mut values = [0u64; MAX_NODES];
        for (index, node) in self.nodes.iter().enumerate() {
            let value = match node.source {
                Source::Input(input) => match input {
                    Input::Register(register) => u64::from(state.regs[register as usize]),
                    Input::Flags => u64::from(state.eflags),
                    Input::Fuel => state.fuel,
                    Input::FsBase => u64::from(state.fs_base),
                    Input::Zero => 0,
                    Input::Carry => u64::from(state.eflags & 1),
                },
                Source::Constant(value) => value,
                Source::HostRegister(register) => registers.gpr(register)?,
                Source::Calculate(operation, left, right) => {
                    let left = values[left as usize];
                    let right = values[right as usize];
                    match operation {
                        Operation::Add => left.wrapping_add(right),
                        Operation::Sub => left.wrapping_sub(right),
                        Operation::Mul => left.wrapping_mul(right),
                        Operation::UnsignedLessThan => u64::from(left < right),
                        Operation::And => left & right,
                        Operation::Or => left | right,
                        Operation::Xor => left ^ right,
                        Operation::Shl => {
                            left.wrapping_shl((right & u64::from(node.bits - 1)) as u32)
                        }
                        Operation::Ushr => left >> (right & u64::from(node.bits - 1)),
                        Operation::Sshr => {
                            (signed(left, node.bits) >> (right & u64::from(node.bits - 1))) as u64
                        }
                        Operation::Not => !left,
                        Operation::Neg => left.wrapping_neg(),
                        Operation::Cast => left,
                        Operation::SignExtend(bits) => signed(left, bits) as u64,
                    }
                }
            };
            values[index] = value & mask(node.bits);
        }
        let output = |index: usize| values[self.outputs[index] as usize];
        let lazy: [u32; 7] = std::array::from_fn(|index| output(index + 9) as u32);
        if lazy[3] & 0xff > 7 || !matches!(lazy[3] >> 8, 0 | 16 | 24) || lazy[6] > 1 {
            return None;
        }
        Some(Recovered {
            regs: std::array::from_fn(|index| output(index) as u32),
            flags: flags(output(8) as u32, lazy),
            fuel: output(16),
            address: output(17) as u32,
        })
    }
}

// Identical raw/add/sub/logic (plus carried AF/OF) and separately preserved
// carry representation used by the integer emitter. This runs only after a
// verified hardware exception.
fn flags(old: u32, lazy: [u32; 7]) -> u32 {
    let [
        left,
        right,
        result,
        kind,
        carry_left,
        carry_right,
        carry_kind,
    ] = lazy;
    let carry = if carry_kind == 0 {
        carry_left
    } else {
        u32::from(carry_left < carry_right)
    };
    let mut flags = (old & !0x8d5) | (carry & 1);
    // Narrow operations keep operands shifted to bit 31; the high byte is the shift.
    let shift = kind >> 8;
    let kind = kind & 0xff;
    // Bit 4: a CF/OF writer moved OF into right and folded the old right into
    // left, keeping AF's left ^ right ^ result.
    let base = kind & 3;
    for bit in [2, 4, 6, 7, 11] {
        let value = if bit == 11 && kind & 4 != 0 {
            right & 1
        } else if kind == 0 || bit != 11 && base == 0 {
            (old >> bit) & 1
        } else {
            match bit {
                2 => (((result >> shift) as u8).count_ones() ^ 1) & 1,
                4 if base != 3 => ((left ^ right ^ result) >> shift >> 4) & 1,
                6 => u32::from(result == 0),
                7 => result >> 31,
                11 if base != 3 => {
                    let lr = left ^ right;
                    let first = if base == 1 { !lr } else { lr };
                    (first & (left ^ result)) >> 31
                }
                _ => 0,
            }
        };
        flags |= value << bit;
    }
    flags
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn malformed_or_overdeep_recipes_are_rejected_before_any_context_read() {
        let mut recipe = Recipe {
            entry_pc: 0,
            nodes: vec![
                Node {
                    source: Source::Constant(1),
                    bits: 32,
                },
                Node {
                    source: Source::Constant(1),
                    bits: 64,
                },
            ],
            outputs: [0; 18],
        };
        recipe.outputs[16] = 1;
        assert!(recipe.valid());
        for index in 2..MAX_DEPTH + 2 {
            recipe.nodes.push(Node {
                source: Source::Calculate(Operation::Add, (index - 1) as u8, 0),
                bits: 32,
            });
        }
        assert!(!recipe.valid());
        recipe.nodes = vec![Node {
            source: Source::Calculate(Operation::Add, 0, 0),
            bits: 32,
        }];
        assert!(!recipe.valid());
        recipe.nodes = vec![Node {
            source: Source::Input(Input::Register(8)),
            bits: 32,
        }];
        assert!(!recipe.valid());
    }
    #[test]
    fn pure_recipes_read_architectural_snapshots_without_an_os_context() {
        struct Registers;
        impl HostRegisters for Registers {
            fn gpr(&self, register: u8) -> Option<u64> {
                (register == 19).then_some(0xffff_fffd)
            }
        }
        let state = CpuState {
            eip: 7,
            regs: [10, 11, 12, 13, 14, 15, 16, 17],
            eflags: 0x203,
            fuel: 99,
            ..CpuState::default()
        };
        let mut nodes: Vec<_> = (0..8)
            .map(|register| Node {
                source: Source::Input(Input::Register(register)),
                bits: 32,
            })
            .collect();
        nodes.push(Node {
            source: Source::Input(Input::Flags),
            bits: 32,
        });
        for _ in 0..7 {
            nodes.push(Node {
                source: Source::Constant(0),
                bits: 32,
            });
        }
        nodes[13].source = Source::Input(Input::Carry);
        nodes.push(Node {
            source: Source::Input(Input::Fuel),
            bits: 64,
        });
        nodes.push(Node {
            source: Source::HostRegister(19),
            bits: 32,
        });
        nodes.push(Node {
            source: Source::Constant(8),
            bits: 32,
        });
        nodes.push(Node {
            source: Source::Calculate(Operation::Add, 17, 18),
            bits: 32,
        });
        let mut outputs = std::array::from_fn(|index| index as u8);
        outputs[17] = 19;
        let mut recipe = Recipe {
            entry_pc: 7,
            nodes,
            outputs,
        };
        let recovered = recipe.evaluate(&state, &Registers).unwrap();
        assert_eq!(recovered.regs, state.regs);
        assert_eq!(recovered.flags, state.eflags);
        assert_eq!(recovered.fuel, state.fuel);
        assert_eq!(recovered.address, 5);
        assert_eq!(state.eip, 7);
        recipe.entry_pc = 8;
        assert!(recipe.evaluate(&state, &Registers).is_none());
        recipe.entry_pc = 7;
        recipe.nodes[17].source = Source::HostRegister(20);
        assert!(recipe.evaluate(&state, &Registers).is_none());
        recipe.nodes[17].source = Source::HostRegister(32);
        assert!(!recipe.valid());
    }

    #[test]
    fn deferred_overflow_parity_auxiliary_and_preserved_carry_are_exact() {
        assert_eq!(
            flags(
                0x202,
                [0x7fffffff, 1, 0x80000000, 1, 0x80000000, 0x7fffffff, 1]
            ) & 0x8d5,
            0x894
        );
        assert_eq!(flags(0x203, [0, 1, u32::MAX, 2, 1, 0, 0]) & 0x8d5, 0x95);
        assert_eq!(
            flags(0x203, [u32::MAX, u32::MAX, 0, 3, 0, 0, 0]) & 0x8d5,
            0x44
        );
    }
}
