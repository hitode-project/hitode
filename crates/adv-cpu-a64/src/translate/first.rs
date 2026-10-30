//! First visits: code runs here once before anything translates it.
//!
//! Most startup code runs once, and translating it costs far more than
//! running it. An entry the owner has not seen before runs here instead, one
//! decoded `Op` at a time with the flags computed in full, until control
//! transfers; the owner translates an entry when it comes back to it. Only
//! the plain case runs here: an instruction `quick`/`plan` does not handle,
//! or whose memory is not ordinary (`Memory::direct_read`, `Memory::direct`
//! for stores), is declined
//! before it changes anything, and the core interpreter runs it.

use adv_cpu_core::memory::Memory;
use adv_cpu_core::{CpuState, IndirectBranch, IndirectBranchKind};
use iced_x86::ConditionCode;

use super::decode::{Alu, ESP, Fetcher, Mem, Narrow8or16, Op, ShiftKind, Src};

const CF: u32 = 0x1;
const PF: u32 = 0x4;
const AF: u32 = 0x10;
const ZF: u32 = 0x40;
const SF: u32 = 0x80;
const OF: u32 = 0x800;

/// How a first visit ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Visit {
    /// Instructions completed.
    pub retired: u32,
    /// Whether the instruction at EIP was declined, for the interpreter.
    pub declined: bool,
}

/// Runs from `cpu.eip` until a control transfer, `stop`, `fuel`
/// instructions, or an instruction it declines.
pub fn run(cpu: &mut CpuState, memory: &Memory, stop: u32, fuel: u64) -> Visit {
    let mut fetcher = Fetcher::new(memory);
    let mut retired = 0u32;
    loop {
        if u64::from(retired) == fuel || retired > 0 && cpu.eip == stop {
            return Visit {
                retired,
                declined: false,
            };
        }
        let Some((op, len)) = fetcher.fetch(cpu.eip) else {
            return Visit {
                retired,
                declined: true,
            };
        };
        let next = cpu.eip.wrapping_add(len);
        match step(op, next, cpu, memory) {
            None => {
                return Visit {
                    retired,
                    declined: true,
                };
            }
            Some(branch) => {
                retired += 1;
                if branch {
                    return Visit {
                        retired,
                        declined: false,
                    };
                }
            }
        }
    }
}

/// The flat address of `mem`; FS-relative ones are declined.
fn address(mem: Mem, regs: &[u32; 8]) -> Option<u32> {
    if mem.fs {
        return None;
    }
    let base = mem.base.map_or(0, |base| regs[usize::from(base)]);
    let index = mem
        .index
        .map_or(0, |index| regs[usize::from(index)].wrapping_mul(mem.scale));
    let address = base.wrapping_add(index).wrapping_add(mem.disp);
    Some(if mem.address_bits == 16 {
        address & 0xffff
    } else {
        address
    })
}

fn load(address: u32, len: usize, memory: &Memory) -> Option<u32> {
    let p = memory.direct_read(address, len)?;
    // SAFETY: `direct_read` checked the whole linear readable range, and the
    // borrow of `memory` keeps it mapped.
    Some(unsafe {
        match len {
            1 => u32::from(p.read()),
            2 => u32::from(u16::from_le(p.cast::<u16>().read_unaligned())),
            _ => u32::from_le(p.cast::<u32>().read_unaligned()),
        }
    })
}

/// A checked store that commits later, so an instruction declines before
/// it changes anything.
struct Store(*mut u8);

impl Store {
    fn at(address: u32, memory: &Memory) -> Option<Self> {
        memory.direct(address, 4, true).map(Self)
    }

    fn commit(self, value: u32) {
        // SAFETY: `at` checked the ordinary writable range, which nothing
        // remaps before the commit.
        unsafe { self.0.cast::<u32>().write_unaligned(value.to_le()) };
    }
}

fn read(src: Src, regs: &[u32; 8], memory: &Memory) -> Option<u32> {
    match src {
        Src::Reg(index) => Some(regs[usize::from(index)]),
        Src::Imm(value) => Some(value),
        Src::Mem(mem) => load(address(mem, regs)?, 4, memory),
    }
}

/// A destination checked before any commit.
enum Place {
    Reg(u8),
    Mem(Store),
}

impl Place {
    fn of(dst: Src, regs: &[u32; 8], memory: &Memory) -> Option<Self> {
        match dst {
            Src::Reg(index) => Some(Self::Reg(index)),
            Src::Mem(mem) => Store::at(address(mem, regs)?, memory).map(Self::Mem),
            Src::Imm(_) => None,
        }
    }

    fn commit(self, value: u32, cpu: &mut CpuState) {
        match self {
            Self::Reg(index) => cpu.regs[usize::from(index)] = value,
            Self::Mem(store) => store.commit(value),
        }
    }
}

fn set(flags: &mut u32, bit: u32, value: bool) {
    *flags = *flags & !bit | if value { bit } else { 0 };
}

/// ZF, SF and PF of a 32-bit result.
fn szp(flags: &mut u32, value: u32) {
    set(flags, ZF, value == 0);
    set(flags, SF, value >> 31 != 0);
    set(flags, PF, (value as u8).count_ones().is_multiple_of(2));
}

fn arithmetic(flags: &mut u32, a: u32, b: u32, carry: u32, subtract: bool) -> u32 {
    let (result, wide) = if subtract {
        (
            a.wrapping_sub(b).wrapping_sub(carry),
            i64::from(a as i32) - i64::from(b as i32) - i64::from(carry),
        )
    } else {
        (
            a.wrapping_add(b).wrapping_add(carry),
            i64::from(a as i32) + i64::from(b as i32) + i64::from(carry),
        )
    };
    let cf = if subtract {
        u64::from(a) < u64::from(b) + u64::from(carry)
    } else {
        u64::from(a) + u64::from(b) + u64::from(carry) > u64::from(u32::MAX)
    };
    set(flags, CF, cf);
    set(flags, OF, wide != i64::from(result as i32));
    set(flags, AF, (a ^ b ^ result) & 0x10 != 0);
    szp(flags, result);
    result
}

fn logic(flags: &mut u32, value: u32) -> u32 {
    // AF is undefined; hardware clears it, as the interpreter does.
    *flags &= !(CF | OF | AF);
    szp(flags, value);
    value
}

fn condition(cc: ConditionCode, flags: u32) -> bool {
    use ConditionCode as C;
    let bit = |mask| flags & mask != 0;
    let less = bit(SF) != bit(OF);
    match cc {
        C::o => bit(OF),
        C::no => !bit(OF),
        C::b => bit(CF),
        C::ae => !bit(CF),
        C::e => bit(ZF),
        C::ne => !bit(ZF),
        C::be => bit(CF) || bit(ZF),
        C::a => !bit(CF) && !bit(ZF),
        C::s => bit(SF),
        C::ns => !bit(SF),
        C::p => bit(PF),
        C::np => !bit(PF),
        C::l => less,
        C::ge => !less,
        C::le => less || bit(ZF),
        C::g => !less && !bit(ZF),
        C::None => true,
    }
}

fn shift(flags: &mut u32, kind: ShiftKind, value: u32, count: u32) -> u32 {
    let sign = value >> 31 != 0;
    match kind {
        ShiftKind::Shl => {
            set(flags, CF, value >> (32 - count) & 1 != 0);
            let result = value << count;
            if count == 1 {
                set(flags, OF, (result >> 31 != 0) != (*flags & CF != 0));
            }
            szp(flags, result);
            result
        }
        ShiftKind::Shr => {
            set(flags, CF, value >> (count - 1) & 1 != 0);
            let result = value >> count;
            if count == 1 {
                set(flags, OF, sign);
            }
            szp(flags, result);
            result
        }
        ShiftKind::Sar => {
            set(flags, CF, value >> (count - 1) & 1 != 0);
            let result = ((value as i32) >> count) as u32;
            if count == 1 {
                set(flags, OF, false);
            }
            szp(flags, result);
            result
        }
        ShiftKind::Rol | ShiftKind::Ror => {
            let result = if kind == ShiftKind::Rol {
                value.rotate_left(count)
            } else {
                value.rotate_right(count)
            };
            let cf = if kind == ShiftKind::Rol {
                result & 1 != 0
            } else {
                result >> 31 != 0
            };
            set(flags, CF, cf);
            if count == 1 {
                let of = if kind == ShiftKind::Rol {
                    (result >> 31 != 0) != cf
                } else {
                    (result >> 31 ^ result >> 30) & 1 != 0
                };
                set(flags, OF, of);
            }
            result
        }
    }
}

/// Pushes `value`; the caller has read everything it needs from the stack.
fn push(value: u32, cpu: &mut CpuState, memory: &Memory) -> Option<()> {
    let esp = cpu.regs[usize::from(ESP)].wrapping_sub(4);
    Store::at(esp, memory)?.commit(value);
    cpu.regs[usize::from(ESP)] = esp;
    Some(())
}

/// Executes `op`, which ends at `next`; whether it transferred control, or
/// `None`, having changed nothing, to decline it.
/// Records the indirect transfer at EIP, as the interpreter does.
fn indirect(cpu: &mut CpuState, target: u32, kind: IndirectBranchKind) {
    cpu.last_indirect_branch = IndirectBranch {
        source: cpu.eip,
        target,
        kind,
    };
}

fn step(op: Op, next: u32, cpu: &mut CpuState, memory: &Memory) -> Option<bool> {
    let regs = cpu.regs;
    let flags = &mut cpu.eflags;
    let mut target = None;
    match op {
        Op::Nop => {}
        // The interpreter runs these: `Fetcher::helpers` is off here, and
        // narrow forms are rare in code that runs once.
        Op::PopMemory(_)
        | Op::PushFlags(_)
        | Op::PopFlags(..)
        | Op::Stack(..)
        | Op::Lahf
        | Op::Timestamp
        | Op::Syscall
        | Op::Helper(_)
        | Op::Body(_)
        | Op::Parts(_)
        | Op::Narrow(..)
        | Op::StepMemory(..)
        | Op::NarrowStep(..)
        | Op::Direction(_)
        | Op::Simd(_)
        | Op::Strings(_)
        | Op::Integer(_)
        | Op::X87(_)
        | Op::Fild64(..) => {
            return None;
        }
        Op::Mov(dst, src) => {
            let value = read(src, &regs, memory)?;
            Place::of(dst, &regs, memory)?.commit(value, cpu);
        }
        Op::Lea(dst, mem) => cpu.regs[usize::from(dst)] = address(mem, &regs)?,
        Op::Alu(alu, dst, src) => {
            let (a, b) = (read(dst, &regs, memory)?, read(src, &regs, memory)?);
            let place = match alu {
                Alu::Cmp | Alu::Test => None,
                _ => Some(Place::of(dst, &regs, memory)?),
            };
            let result = match alu {
                Alu::Add => arithmetic(flags, a, b, 0, false),
                Alu::Sub | Alu::Cmp => arithmetic(flags, a, b, 0, true),
                Alu::And | Alu::Test => logic(flags, a & b),
                Alu::Or => logic(flags, a | b),
                Alu::Xor => logic(flags, a ^ b),
            };
            if let Some(place) = place {
                place.commit(result, cpu);
            }
        }
        Op::Carry(subtract, dst, src) => {
            let b = read(src, &regs, memory)?;
            let carry = *flags & CF;
            cpu.regs[usize::from(dst)] =
                arithmetic(flags, regs[usize::from(dst)], b, carry, subtract);
        }
        Op::Step(increment, dst) => {
            let carry = *flags & CF;
            let value = arithmetic(flags, regs[usize::from(dst)], 1, 0, !increment);
            *flags = *flags & !CF | carry;
            cpu.regs[usize::from(dst)] = value;
        }
        Op::Neg(dst) => {
            cpu.regs[usize::from(dst)] = arithmetic(flags, 0, regs[usize::from(dst)], 0, true);
        }
        Op::Not(dst) => cpu.regs[usize::from(dst)] = !regs[usize::from(dst)],
        Op::Extend(dst, source, signed) => {
            let (value, word) = match source {
                Narrow8or16::Byte(n) if n < 4 => (regs[usize::from(n)] & 0xff, false),
                Narrow8or16::Byte(n) => (regs[usize::from(n - 4)] >> 8 & 0xff, false),
                Narrow8or16::Word(n) => (regs[usize::from(n)] & 0xffff, true),
                Narrow8or16::Mem(mem, word) => {
                    let len = if word { 2 } else { 1 };
                    (load(address(mem, &regs)?, len, memory)?, word)
                }
            };
            cpu.regs[usize::from(dst)] = match (signed, word) {
                (false, _) => value,
                (true, false) => value as u8 as i8 as u32,
                (true, true) => value as u16 as i16 as u32,
            };
        }
        Op::Cdq => cpu.regs[2] = ((regs[0] as i32) >> 31) as u32,
        Op::Push(src) => push(read(src, &regs, memory)?, cpu, memory)?,
        Op::Pop(dst) => {
            let value = load(regs[usize::from(ESP)], 4, memory)?;
            cpu.regs[usize::from(ESP)] = regs[usize::from(ESP)].wrapping_add(4);
            cpu.regs[usize::from(dst)] = value;
        }
        Op::Leave => {
            let frame = load(regs[5], 4, memory)?;
            cpu.regs[usize::from(ESP)] = regs[5].wrapping_add(4);
            cpu.regs[5] = frame;
        }
        Op::Jmp(to) => target = Some(to),
        Op::Call(to) => {
            push(next, cpu, memory)?;
            target = Some(to);
        }
        Op::CallIndirect(src) => {
            let to = read(src, &regs, memory)?;
            push(next, cpu, memory)?;
            target = Some(to);
            indirect(cpu, to, IndirectBranchKind::Call);
        }
        Op::JmpIndirect(src) => {
            let to = read(src, &regs, memory)?;
            target = Some(to);
            indirect(cpu, to, IndirectBranchKind::Jump);
        }
        Op::Ret(bytes) => {
            let esp = regs[usize::from(ESP)];
            let to = load(esp, 4, memory)?;
            target = Some(to);
            cpu.regs[usize::from(ESP)] = esp.wrapping_add(4).wrapping_add(u32::from(bytes));
            indirect(cpu, to, IndirectBranchKind::Return);
        }
        Op::Jcc(cc, to) => {
            use super::decode::Cc;
            let ecx = regs[1];
            let taken = match cc {
                Cc::Flags(cc) => condition(cc, *flags),
                Cc::EcxZero => ecx == 0,
                Cc::CxZero => ecx & 0xffff == 0,
                Cc::Loop(zero) => {
                    let ecx = ecx.wrapping_sub(1);
                    cpu.regs[1] = ecx;
                    ecx != 0 && zero.is_none_or(|set| (*flags & ZF != 0) == set)
                }
            };
            // A not-taken branch still ends the block.
            target = Some(if taken { to } else { next });
        }
        Op::Setcc(cc, n) => {
            let value = u32::from(condition(cc, *flags));
            let (index, at) = if n < 4 { (n, 0) } else { (n - 4, 8) };
            let reg = &mut cpu.regs[usize::from(index)];
            *reg = *reg & !(0xff << at) | value << at;
        }
        Op::Cmov(cc, dst, src) => {
            // The source is read even when nothing moves.
            let value = read(src, &regs, memory)?;
            if condition(cc, *flags) {
                cpu.regs[usize::from(dst)] = value;
            }
        }
        Op::Shift(kind, dst, count) => {
            let value = read(dst, &regs, memory)?;
            let count = count.unwrap_or(regs[1] & 31);
            let place = Place::of(dst, &regs, memory)?;
            if count != 0 {
                let value = shift(flags, kind, value, count);
                place.commit(value, cpu);
            }
        }
        Op::Imul(dst, src, imm) => {
            let a = read(src, &regs, memory)?;
            let b = imm.unwrap_or(regs[usize::from(dst)]);
            let product = i64::from(a as i32) * i64::from(b as i32);
            let overflow = product != i64::from(product as i32);
            set(flags, CF, overflow);
            set(flags, OF, overflow);
            cpu.regs[usize::from(dst)] = product as u32;
        }
    }
    cpu.eip = target.unwrap_or(next);
    Some(target.is_some())
}

#[cfg(test)]
mod tests {
    use adv_cpu_core::interpreter::{Interpreter, Step};

    use super::*;
    use crate::translate::decode::tests::{OPCODES, Rng};

    const BASE: u32 = 0x1_0000;
    const CODE: u32 = 0x1_0000;
    const DATA: u32 = 0x2_0000;
    const DATA_LEN: usize = 0x2000;

    fn memory(code: &[u8], data: &[u8]) -> Memory {
        let mut memory = Memory::new(BASE, 0x3_0000).unwrap();
        memory.protect(CODE, 0x1000, 7).unwrap();
        memory.write(CODE, code).unwrap();
        memory.protect(CODE, 0x1000, 5).unwrap();
        memory.protect(DATA, DATA_LEN, 3).unwrap();
        memory.write(DATA, data).unwrap();
        memory
    }

    /// Every instruction a first visit completes leaves what the core
    /// interpreter leaves: registers, EIP, EFLAGS and memory.
    // spec: A64-TR-2, A64-FLAG-2
    #[test]
    fn first_visits_match_the_interpreter() {
        let mut rng = Rng(0x2545_f491_4f6c_dd1d);
        let mut ran = 0;
        for _ in 0..100_000 {
            let mut code = [0u8; 16];
            for byte in &mut code {
                *byte = rng.byte();
            }
            let opcode = OPCODES[rng.next() as usize % OPCODES.len()];
            code[..opcode.len()].copy_from_slice(opcode);
            // Small displacements keep most memory operands in the data.
            if rng.next().is_multiple_of(2) {
                code[opcode.len()] &= 0x7f;
            }
            let data: Vec<u8> = (0..DATA_LEN).map(|_| rng.byte()).collect();
            let mut state = CpuState {
                eip: CODE,
                eflags: 0x202 | (rng.next() as u32 & 0x8d5),
                ..CpuState::default()
            };
            for value in &mut state.regs {
                *value = match rng.next() % 4 {
                    0 => rng.next() as u32,
                    _ => DATA + 0x800 + (rng.next() % 0x800) as u32,
                };
            }
            let mut ours = state.clone();
            let mine = memory(&code, &data);
            let visit = run(&mut ours, &mine, 0, 1);
            if visit.declined {
                assert_eq!(ours, state, "{code:02x?}: a decline changes nothing");
                continue;
            }
            ran += 1;
            let mut theirs = state.clone();
            let mut reference = memory(&code, &data);
            let step = Interpreter::new().step(&mut theirs, &mut reference);
            assert_eq!(step, Step::Executed, "{code:02x?}");
            let context = format!("{code:02x?} from {state:x?}");
            assert_eq!(ours.regs, theirs.regs, "{context}: registers");
            assert_eq!(ours.eip, theirs.eip, "{context}: eip");
            assert_eq!(ours.eflags, theirs.eflags, "{context}: eflags");
            let read = |memory: &Memory| memory.read(DATA, DATA_LEN).unwrap().to_vec();
            assert!(read(&mine) == read(&reference), "{context}: memory");
        }
        assert!(ran > 40_000, "{ran} instructions ran");
    }
}
