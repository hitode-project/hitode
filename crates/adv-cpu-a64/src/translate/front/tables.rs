//! Bounded read-only image switch tables (A64-CTL-6).
use super::Unit;
use super::decode::{Alu, Cc, Mem, Narrow8or16, Op, Src};
use super::tags::ConditionCode;
use super::trace::{Mode, Next, Trace};
use adv_cpu_core::memory::{Memory, READ, SHARED, WRITE};

#[derive(Clone)]
pub(crate) struct Table {
    pub instruction: usize,
    pub targets: Vec<u32>,
}

pub(crate) fn image(images: &[(u32, u32)], start: u32, len: u32) -> bool {
    images.iter().any(|&(base, size)| {
        start >= base && u64::from(start) + u64::from(len) <= u64::from(base) + u64::from(size)
    })
}

fn bound(trace: &Trace, end: usize, reg: u8) -> Option<u32> {
    for i in (0..end).rev() {
        let inst = trace.insts[i];
        if trace.merge[i + 1]
            || trace.head(i + 1)
            || inst.next != Next::Fall
            || inst.mode != Mode::Plain
        {
            return None;
        }
        match inst.op {
            Op::Extend(dst, Narrow8or16::Byte(_) | Narrow8or16::Mem(_, false), false)
                if dst == reg =>
            {
                return Some(256);
            }
            Op::Jcc(Cc::Flags(cc @ (ConditionCode::a | ConditionCode::ae)), _)
                if i > 0 && !trace.merge[i] =>
            {
                if let Op::Alu(Alu::Cmp, Src::Reg(r), Src::Imm(n)) = trace.insts[i - 1].op
                    && trace.insts[i - 1].next == Next::Fall
                    && r == reg
                {
                    return n
                        .checked_add(u32::from(cc == ConditionCode::a))
                        .filter(|&n| n != 0);
                }
                return None;
            }
            Op::Nop | Op::Alu(Alu::Cmp | Alu::Test, _, _) | Op::Mov(Src::Mem(_), _) => {}
            Op::Mov(Src::Reg(dst), _)
            | Op::Lea(dst, _)
            | Op::Extend(dst, _, _)
            | Op::Alu(_, Src::Reg(dst), _)
            | Op::Step(_, dst)
                if dst != reg => {}
            _ => return None,
        }
    }
    None
}

pub(super) fn prepare(work: &mut Unit, memory: &Memory) {
    work.tables.clear();
    for i in 0..work.trace.len() {
        let Op::JmpIndirect(Src::Mem(Mem {
            base: None,
            index: Some(reg),
            scale: 4,
            address_bits: 32,
            disp,
            fs: false,
        })) = work.trace.insts[i].op
        else {
            continue;
        };
        let Some(n @ 1..=1024) = bound(&work.trace, i, reg) else {
            continue;
        };
        let len = n * 4;
        if !image(&work.images, disp, len) {
            continue;
        }
        let Ok(range) = memory.committed(disp, len as usize) else {
            continue;
        };
        if !memory.perms[range.start / 4096..=(range.end - 1) / 4096]
            .iter()
            .all(|p| p & (READ | WRITE | SHARED) == READ)
        {
            continue;
        }
        let Ok(bytes) = memory.read(disp, len as usize) else {
            continue;
        };
        work.tables.push(Table {
            instruction: i,
            targets: bytes
                .as_chunks::<4>()
                .0
                .iter()
                .map(|b| u32::from_le_bytes(*b))
                .collect(),
        });
        work.trace.code.push((disp, len));
    }
    work.trace.code.sort_unstable();
}
