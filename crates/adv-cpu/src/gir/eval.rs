//! Software reference evaluator. This module is never an execution backend.
use super::ir::*;
use crate::{CpuState, Memory, READ, WRITE, interpreter::Step};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub gpr: [u32; 8],
    pub flags: u32,
    pub pc: u32,
    pub committed: u64,
}
impl Frame {
    pub fn from_cpu(cpu: &CpuState) -> Self {
        Self {
            gpr: cpu.regs,
            flags: cpu.eflags,
            pc: cpu.eip,
            committed: 0,
        }
    }
    pub fn restore(&self, cpu: &mut CpuState) {
        cpu.regs = self.gpr;
        cpu.eflags = self.flags;
        cpu.eip = self.pc;
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stop {
    Exit(ExitReason),
    Budget,
    Fault { step: Step, snapshot: SnapshotId },
    Guard(SnapshotId),
}
#[derive(Clone, Debug)]
pub struct Outcome {
    pub frame: Frame,
    pub stop: Stop,
    pub prefixes: Vec<Frame>,
}

/// Evaluation validates even manually constructed graphs before touching memory.
pub fn evaluate(
    r: &Region,
    memory: &mut Memory,
    initial: &Frame,
    budget: u64,
) -> Result<Outcome, super::verify::Error> {
    evaluate_observed(r, memory, initial, budget, |_, _| {})
}

/// The observer sees memory after each original instruction commit, including
/// writes that a later instruction overwrites. Tests use it for strict oracles.
pub fn evaluate_observed(
    r: &Region,
    memory: &mut Memory,
    initial: &Frame,
    budget: u64,
    mut observer: impl FnMut(&Frame, &Memory),
) -> Result<Outcome, super::verify::Error> {
    super::verify::verify(r)?;
    let mut values = vec![0u64; r.values.len()];
    let mut block = r.entry;
    let mut arguments: Vec<_> = initial
        .gpr
        .into_iter()
        .map(u64::from)
        .chain([u64::from(initial.flags), 0])
        .collect();
    let mut frame = initial.clone();
    let mut prefixes = vec![];
    let mut transitions = 0u64;
    loop {
        if frame.committed - initial.committed >= budget {
            return Ok(Outcome {
                frame,
                stop: Stop::Budget,
                prefixes,
            });
        }
        // Hand-built zero-commit cycles must not hang a diagnostic process.
        transitions += 1;
        if transitions
            > budget
                .saturating_add(1)
                .saturating_mul(r.blocks.len() as u64 + 1)
        {
            return Err(super::verify::Error::region("nonprogressing CFG"));
        }
        let current = &r.blocks[block.0];
        for (&id, value) in current.parameters.iter().zip(arguments) {
            values[id.0] = value;
        }
        for op in &current.operations {
            match op {
                Operation::Define(id) => values[id.0] = value(r, *id, &values),
                Operation::Memory(id) => {
                    let m = &r.memory[id.0];
                    let address = values[m.address.0] as u32;
                    let len = (m.width.bits() / 8) as usize;
                    let read = m.access == Access::Read;
                    let pc = r.commits[m.commit.0].pc;
                    let step = if read {
                        Step::ReadFault { pc, address }
                    } else {
                        Step::WriteFault { pc, address }
                    };
                    let result = if !crate::interpreter::accessible(
                        memory,
                        address,
                        len as u8,
                        if read { READ } else { WRITE },
                    ) {
                        Err(())
                    } else {
                        match m.access {
                            Access::Read => memory
                                .read_data(address, len)
                                .map(|data| {
                                    let mut bytes = [0; 8];
                                    bytes[..len].copy_from_slice(&data);
                                    values[m.result.unwrap().0] = u64::from_le_bytes(bytes);
                                })
                                .map_err(|_| ()),
                            Access::Write { value, enabled } => {
                                if enabled.is_none_or(|e| values[e.0] != 0) {
                                    memory
                                        .write(address, &values[value.0].to_le_bytes()[..len])
                                        .map_err(|_| ())
                                } else {
                                    Ok(())
                                }
                            }
                        }
                    };
                    if result.is_err() {
                        let snapshot = &r.snapshots[m.snapshot.0];
                        frame = materialize(&snapshot.state, pc, frame.committed, &values);
                        return Ok(Outcome {
                            frame,
                            stop: Stop::Fault {
                                step,
                                snapshot: m.snapshot,
                            },
                            prefixes,
                        });
                    }
                    values[m.output.0] = values[m.input.0].wrapping_add(1);
                }
                Operation::Commit {
                    point,
                    state,
                    next_pc,
                } => {
                    frame = materialize(
                        state,
                        values[next_pc.0] as u32,
                        frame.committed + u64::from(r.commits[point.0].units),
                        &values,
                    );
                    prefixes.push(frame.clone());
                    observer(&frame, memory);
                    if frame.committed - initial.committed >= budget {
                        return Ok(Outcome {
                            frame,
                            stop: Stop::Budget,
                            prefixes,
                        });
                    }
                }
            }
        }
        let target = match &current.terminator {
            Terminator::Jump(t) | Terminator::Call { callee: t, .. } => t.clone(),
            Terminator::Branch {
                condition,
                taken,
                fallthrough,
            } => {
                if values[condition.0] != 0 {
                    taken.clone()
                } else {
                    fallthrough.clone()
                }
            }
            Terminator::Exit(e) => Target::Exit(e.clone()),
            Terminator::Return {
                actual,
                expected,
                matched,
                mismatch,
            } => {
                if values[actual.0] as u32 == *expected {
                    Target::Block(matched.clone())
                } else {
                    Target::Exit(mismatch.clone())
                }
            }
            Terminator::Guard {
                condition,
                passed,
                failed,
                ..
            } => {
                if values[condition.0] != 0 {
                    Target::Block(passed.clone())
                } else {
                    let s = &r.snapshots[failed.0];
                    frame =
                        materialize(&s.state, r.commits[s.commit.0].pc, frame.committed, &values);
                    return Ok(Outcome {
                        frame,
                        stop: Stop::Guard(*failed),
                        prefixes,
                    });
                }
            }
        };
        match target {
            Target::Exit(e) => {
                frame = materialize(&e.state, values[e.pc.0] as u32, frame.committed, &values);
                return Ok(Outcome {
                    frame,
                    stop: Stop::Exit(e.reason),
                    prefixes,
                });
            }
            Target::Block(e) => {
                arguments = e.arguments.iter().map(|id| values[id.0]).collect();
                block = e.block;
            }
        }
    }
}
fn materialize(state: &StateMap, pc: u32, committed: u64, values: &[u64]) -> Frame {
    Frame {
        gpr: state.gpr.map(|id| values[id.0] as u32),
        flags: values[state.flags.0] as u32,
        pc,
        committed,
    }
}
fn signed(value: u64, width: Width) -> i64 {
    let shift = 64 - width.bits();
    ((value << shift) as i64) >> shift
}
fn value(r: &Region, id: ValueId, values: &[u64]) -> u64 {
    let v = &r.values[id.0];
    let get = |id: ValueId| values[id.0];
    let result = match &v.kind {
        ValueKind::Constant(n) => *n,
        ValueKind::Flags(f) => u64::from(flags(f, values)),
        ValueKind::Pure(p) => match *p {
            Pure::Extract { value, lsb } => get(value) >> lsb,
            Pure::Insert { base, value, lsb } => {
                (get(base) & !(r.values[value.0].width.mask() << lsb)) | (get(value) << lsb)
            }
            Pure::Extend {
                value,
                signed: true,
            } => signed(get(value), r.values[value.0].width) as u64,
            Pure::Extend {
                value,
                signed: false,
            } => get(value),
            Pure::Select { condition, yes, no } => get(if get(condition) != 0 { yes } else { no }),
            Pure::Condition { flags, code } => u64::from(condition(code, get(flags) as u32)),
            Pure::Nonzero(a) => u64::from(get(a) != 0),
            Pure::Binary(op, a, b) => {
                let (a, b) = (get(a), get(b));
                let bits = v.width.bits();
                let n = (b & 31) as u32;
                match op {
                    Binary::Add => a.wrapping_add(b),
                    Binary::Sub => a.wrapping_sub(b),
                    Binary::Mul => a.wrapping_mul(b),
                    Binary::And => a & b,
                    Binary::Or => a | b,
                    Binary::Xor => a ^ b,
                    Binary::Shl => a << n,
                    Binary::Shr => a >> n,
                    Binary::Sar => (signed(a, v.width) >> n) as u64,
                    Binary::Rol | Binary::Ror => {
                        let n = n % bits;
                        if n == 0 {
                            a
                        } else if op == Binary::Rol {
                            (a << n) | (a >> (bits - n))
                        } else {
                            (a >> n) | (a << (bits - n))
                        }
                    }
                }
            }
        },
        _ => unreachable!("verified definition"),
    };
    result & v.width.mask()
}
fn flags(f: &Flags, values: &[u64]) -> u32 {
    let get = |id: ValueId| values[id.0];
    let old = get(f.preserved) as u32;
    let (a, b, carry, result) = (get(f.lhs), get(f.rhs), get(f.carry), get(f.result));
    let bits = f.width.bits();
    let sign = 1u64 << (bits - 1);
    let mut mask = f.defined;
    let mut out = 0;
    let mut bit = |flag, yes| {
        if yes {
            out |= flag;
        }
    };
    bit(ZF, result == 0);
    bit(SF, result & sign != 0);
    bit(PF, (result as u8).count_ones() % 2 == 0);
    match f.kind {
        FlagKind::Add | FlagKind::Sub => {
            let sub = f.kind == FlagKind::Sub;
            bit(
                CF,
                if sub {
                    a < b + carry
                } else {
                    a + b + carry > f.width.mask()
                },
            );
            let wide = if sub {
                signed(a, f.width) - signed(b, f.width) - carry as i64
            } else {
                signed(a, f.width) + signed(b, f.width) + carry as i64
            };
            let limit = 1i64 << (bits - 1);
            bit(OF, wide < -limit || wide >= limit);
            bit(AF, (a ^ b ^ result) & 16 != 0);
        }
        FlagKind::Logic => {}
        FlagKind::SignedMultiply | FlagKind::UnsignedMultiply => {
            let overflow = if f.kind == FlagKind::SignedMultiply {
                signed(a, f.width) * signed(b, f.width) != signed(result, f.width)
            } else {
                a * b > f.width.mask()
            };
            bit(CF, overflow);
            bit(OF, overflow);
        }
        _ => {
            let n = b as u32 & 31;
            if n == 0 {
                return old;
            }
            if n != 1 {
                mask &= !OF;
            }
            let cf = match f.kind {
                FlagKind::Shl => {
                    if n <= bits {
                        a & (1 << (bits - n)) != 0
                    } else {
                        mask &= !CF;
                        old & CF != 0
                    }
                }
                FlagKind::Shr => {
                    if n <= bits {
                        a & (1 << (n - 1)) != 0
                    } else {
                        mask &= !CF;
                        old & CF != 0
                    }
                }
                FlagKind::Sar => {
                    if n >= bits {
                        a & sign != 0
                    } else {
                        a & (1 << (n - 1)) != 0
                    }
                }
                FlagKind::Rol => result & 1 != 0,
                FlagKind::Ror => result & sign != 0,
                _ => unreachable!(),
            };
            bit(CF, cf);
            bit(
                OF,
                match f.kind {
                    FlagKind::Shl | FlagKind::Rol => (result & sign != 0) != cf,
                    FlagKind::Shr => a & sign != 0,
                    FlagKind::Ror => ((result >> (bits - 1)) ^ (result >> (bits - 2))) & 1 != 0,
                    _ => false,
                },
            );
        }
    }
    (old & !mask) | (out & mask)
}
fn condition(code: iced_x86::ConditionCode, flags: u32) -> bool {
    use iced_x86::ConditionCode::*;
    let carry = flags & CF != 0;
    let zero = flags & ZF != 0;
    let sign = flags & SF != 0;
    let overflow = flags & OF != 0;
    let parity = flags & PF != 0;
    match code {
        None => true,
        o => overflow,
        no => !overflow,
        b => carry,
        ae => !carry,
        e => zero,
        ne => !zero,
        be => carry || zero,
        a => !carry && !zero,
        s => sign,
        ns => !sign,
        p => parity,
        np => !parity,
        l => sign != overflow,
        ge => sign == overflow,
        le => zero || sign != overflow,
        g => !zero && sign == overflow,
    }
}
