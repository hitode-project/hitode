//! Cold entries execute as they decode. This avoids filling a window or
//! publishing cache dependencies for instructions that may only run once.

use super::*;
use quick::{Alu, Mem, Narrow8or16, QuickOp, ShiftKind, Src, Window};

/// A cold interpreter run ending at a transfer, stop, budget, or owner event.
pub struct FirstVisit {
    pub execution: Execution,
    pub last_units: u32,
    pub quick_decodes: u32,
    pub full_decodes: u32,
    pub slow: bool,
}

impl Interpreter {
    pub fn run_first_visit(
        &mut self,
        state: &mut CpuState,
        memory: &mut Memory,
        stop: u32,
        fuel: u32,
    ) -> FirstVisit {
        let mut window = Window::default();
        let mut visit = FirstVisit {
            execution: Execution {
                step: Step::Executed,
                units: 0,
            },
            last_units: 0,
            quick_decodes: 0,
            full_decodes: 0,
            slow: false,
        };
        while visit.execution.units < fuel && state.eip != stop {
            let pc = state.eip;
            let (decoded, quick) = match window.fetch(memory, pc) {
                Ok(bytes) => {
                    #[cfg(test)]
                    tests::count(|work| work.quick_attempts += 1);
                    if let Some((op, len)) = quick::quick(bytes, pc) {
                        visit.quick_decodes += 1;
                        let action = action(op);
                        let next = pc.wrapping_add(len);
                        if state.rep.is_none()
                            && state.eflags & 0x10100 == 0
                            && scalar::execute_action(action, next, state, memory).is_some()
                        {
                            if reads_clock(action) {
                                state.clock_read = 1;
                            }
                            visit.execution.units += 1;
                            visit.last_units = 1;
                            if let Some(retired) = &mut self.frame_retired {
                                *retired += 1;
                            }
                            if action.is_transfer() {
                                break;
                            }
                            continue;
                        }
                        (
                            Decoded::Ready(Predecoded {
                                action,
                                next,
                                register_only: false,
                                clock: reads_clock(action),
                                checked_scalar: false,
                                exclusive: false,
                            }),
                            true,
                        )
                    } else {
                        let denied =
                            (bytes.len() < 15).then(|| pc.wrapping_add(bytes.len() as u32));
                        (decode_bytes(pc, bytes, denied).0, false)
                    }
                }
                Err(_) => {
                    let (entry, denied) = fetch_bytes::<true>(memory, pc);
                    (
                        decode_bytes(pc, &entry.bytes[..entry.len as usize], denied).0,
                        false,
                    )
                }
            };
            visit.full_decodes += u32::from(!quick);
            visit.slow = true;
            let epoch = memory.epoch;
            let count = state.regs[1];
            let execution =
                execute_decoded(&decoded, state, memory, fuel - visit.execution.units, None);
            if let Some(retired) = &mut self.frame_retired {
                *retired += retired_instruction(&decoded, execution, state, pc);
                self.frame_rep_elements += rep_elements(&decoded, count, state);
            }
            visit.execution.step = execution.step;
            visit.execution.units += execution.units;
            visit.last_units = execution.units;
            if execution.step != Step::Executed
                || decoded.is_transfer()
                || state.eip == pc
                || state.eflags & 0x10100 != 0
                || memory.epoch != epoch
            {
                break;
            }
        }
        visit
    }
}

fn reg(index: u8, width: u8) -> Reg {
    Reg {
        index: if width == 1 { index & 3 } else { index },
        shift: if width == 1 && index >= 4 { 8 } else { 0 },
        width,
    }
}

fn address(mem: Mem) -> Address {
    let word = mem.address_bits == 16;
    let width = if word { 2 } else { 4 };
    Address {
        base: mem.base.map(|r| reg(r, width)),
        index: mem.index.map(|r| reg(r, width)),
        scale: mem.scale as u8,
        displacement: mem.disp,
        word,
        fs: mem.fs,
    }
}

fn operand(src: Src) -> Operand {
    match src {
        Src::Reg(r) => Operand::Register(reg(r, 4)),
        Src::Imm(value) => Operand::Immediate(value),
        Src::Mem(mem) => Operand::Memory(address(mem), 4),
    }
}

fn action(op: QuickOp) -> Action {
    use Operand::{Immediate, Register};
    let dword = |r| Register(reg(r, 4));
    match op {
        QuickOp::Nop => Action::Nop,
        QuickOp::Pause => Action::Pause,
        QuickOp::Mov(dst, src) => Action::Mov(operand(dst), operand(src), false),
        QuickOp::Lea(dst, mem) => Action::Lea(reg(dst, 4), address(mem)),
        QuickOp::Alu(kind, dst, src) => {
            let kind = match kind {
                Alu::Add => M::Add,
                Alu::Sub => M::Sub,
                Alu::And => M::And,
                Alu::Or => M::Or,
                Alu::Xor => M::Xor,
                Alu::Cmp => M::Cmp,
                Alu::Test => M::Test,
            };
            Action::Binary(kind, operand(dst), operand(src))
        }
        QuickOp::Step(inc, dst) => Action::Unary(if inc { M::Inc } else { M::Dec }, dword(dst)),
        QuickOp::Neg(dst) => Action::Unary(M::Neg, dword(dst)),
        QuickOp::Not(dst) => Action::Unary(M::Not, dword(dst)),
        QuickOp::Extend(dst, src, signed) => {
            let src = match src {
                Narrow8or16::Byte(r) => Register(reg(r, 1)),
                Narrow8or16::Word(r) => Register(reg(r, 2)),
                Narrow8or16::Mem(mem, word) => {
                    Operand::Memory(address(mem), if word { 2 } else { 1 })
                }
            };
            Action::Mov(dword(dst), src, signed)
        }
        QuickOp::Cdq => Action::Sign(M::Cdq),
        QuickOp::Push(src) => Action::Push(operand(src), 4),
        QuickOp::Pop(dst) => Action::Pop(dword(dst), 4),
        QuickOp::Leave => Action::Leave(4),
        QuickOp::Jmp(to) => Action::Branch(Immediate(to), ConditionCode::None),
        QuickOp::Call(to) => Action::Call(Immediate(to)),
        QuickOp::Jcc(condition, to) => Action::Branch(Immediate(to), condition),
        QuickOp::Ret(bytes) => Action::Ret(bytes),
        QuickOp::JmpIndirect(src) => Action::Branch(operand(src), ConditionCode::None),
        QuickOp::CallIndirect(src) => Action::Call(operand(src)),
        QuickOp::Setcc(condition, dst) => Action::Set(Register(reg(dst, 1)), condition),
        QuickOp::Cmov(condition, dst, src) => Action::Cmov(dword(dst), operand(src), condition),
        QuickOp::Shift(kind, dst, count) => {
            let kind = match kind {
                ShiftKind::Shl => M::Shl,
                ShiftKind::Shr => M::Shr,
                ShiftKind::Sar => M::Sar,
                ShiftKind::Rol => M::Rol,
                ShiftKind::Ror => M::Ror,
            };
            let count = count.map_or(Register(reg(1, 1)), Immediate);
            Action::Shift(kind, operand(dst), count)
        }
        QuickOp::Imul(dst, src, imm) => {
            Action::Multiply(dword(dst), Some(operand(src)), imm.map(Immediate), true)
        }
        QuickOp::Carry(subtract, dst, src) => Action::Binary(
            if subtract { M::Sbb } else { M::Adc },
            dword(dst),
            operand(src),
        ),
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use std::cell::Cell;

    #[derive(Clone, Copy, Debug, Default)]
    pub(in crate::interpreter) struct Work {
        pub quick_attempts: u32,
        pub loads: u32,
        pub stores: u32,
        pub flag_updates: u32,
        pub transactions: u32,
    }

    thread_local! {
        static WORK: Cell<Option<Work>> = const { Cell::new(None) };
    }

    pub(in crate::interpreter) fn count(update: impl FnOnce(&mut Work)) {
        WORK.with(|work| {
            if let Some(mut counts) = work.get() {
                update(&mut counts);
                work.set(Some(counts));
            }
        });
    }

    const CODE: u32 = 0x10000;
    const DATA: u32 = 0x20000;

    fn memory(code: &[u8]) -> Memory {
        let mut memory = Memory::new(CODE, 0x30000).unwrap();
        memory.protect(CODE, PAGE_SIZE, READ | WRITE).unwrap();
        memory.write(CODE, code).unwrap();
        memory.protect(CODE, PAGE_SIZE, READ | EXEC).unwrap();
        memory.protect(DATA, 2 * PAGE_SIZE, READ | WRITE).unwrap();
        memory
    }

    // spec: A64-TR-2, A64-FLAG-2
    #[test]
    fn first_visit_work_stays_within_baseline() {
        let registers = [0xb8, 3, 0, 0, 0, 0x83, 0xc0, 1, 0x85, 0xc0, 0x75, 1];
        let stack = [
            0x50, 0x8b, 0x0c, 0x24, 0x01, 0x04, 0x24, 0x0f, 0x44, 0x14, 0x24, 0x5b, 0xc3,
        ];
        let read = [0x8b, 0x06, 0xeb, 0];
        let write = [0x89, 0x06];
        let narrow = [0xb0, 7, 0xeb, 0];
        for (code, eflags, source, retired, baseline) in [
            (&registers[..], 2, DATA, 4, [4, 0, 0, 0, 2, 0]),
            (&registers[..], 2, DATA, 3, [3, 0, 0, 0, 2, 0]),
            (&stack[..], 2, DATA, 6, [6, 0, 5, 2, 1, 0]),
            (
                &read[..],
                2,
                DATA + PAGE_SIZE as u32 - 1,
                2,
                [2, 1, 2, 0, 0, 1],
            ),
            (&write[..], 2, 0x30000, 0, [1, 1, 0, 2, 0, 1]),
            (&narrow[..], 2, DATA, 2, [2, 2, 0, 0, 0, 0]),
        ] {
            let mut memory = memory(code);
            let mut state = CpuState {
                eip: CODE,
                eflags,
                ..CpuState::default()
            };
            state.regs[4] = DATA + 16;
            state.regs[6] = source;
            memory
                .write(DATA + 16, &(CODE + 0x80).to_le_bytes())
                .unwrap();
            let generation = memory.map_generation();
            let mut interpreter = Interpreter::new();
            let fuel = if retired == 3 { 3 } else { 20 };
            let stop = CODE + 0x100;
            WORK.set(Some(Work::default()));
            let visit = interpreter.run_first_visit(&mut state, &mut memory, stop, fuel);
            let work = WORK.take().unwrap();
            assert_eq!(visit.execution.units, retired, "{code:02x?}");
            let measured = [
                work.quick_attempts,
                visit.full_decodes,
                work.loads,
                work.stores,
                work.flag_updates,
                work.transactions,
            ];
            assert!(
                measured
                    .into_iter()
                    .zip(baseline)
                    .all(|(new, old)| new <= old),
                "{code:02x?}: {measured:?} exceeds {baseline:?}"
            );
            assert_eq!(memory.map_generation(), generation);
            assert_eq!(interpreter.cached_instructions(), 0);
            if code == write {
                assert_eq!(
                    visit.execution.step,
                    Step::WriteFault {
                        pc: CODE,
                        address: source
                    }
                );
            } else {
                assert_eq!(visit.execution.step, Step::Executed);
            }
        }
    }

    // spec: A64-TR-2
    #[test]
    fn first_visit_stops_at_an_untaken_transfer() {
        let mut memory = memory(&[0x31, 0xc0, 0x75, 0x01, 0x40, 0x90]);
        let mut state = CpuState {
            eip: CODE,
            ..CpuState::default()
        };
        let mut interpreter = Interpreter::new();
        let visit = interpreter.run_first_visit(&mut state, &mut memory, CODE + 6, 10);
        assert_eq!(visit.execution.units, 2);
        assert_eq!(state.eip, CODE + 4);
        assert_eq!(state.regs[0], 0);
        let visit = interpreter.run_first_visit(&mut state, &mut memory, CODE + 5, 10);
        assert_eq!(visit.execution.units, 1);
        assert_eq!(state.eip, CODE + 5);
        assert_eq!(state.regs[0], 1);
    }
}
