//! Bounded read-only image switch tables (A64-CTL-6).
use super::decode::{Alu, Cc, Mem, Narrow8or16, Op, Src};
use super::trace::{Mode, Next, Trace};
use super::*;
use adv_cpu_core::memory::{Memory, READ, SHARED, WRITE};

#[derive(Default)]
pub(crate) struct Table {
    pub instruction: usize,
    pub targets: Vec<u32>,
    pub offset: usize,
    pub stub: usize,
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

pub(super) fn prepare(work: &mut Workspace, memory: &Memory) {
    work.tables.clear();
    for i in 0..work.trace.len() {
        let Op::JmpIndirect(Src::Mem(Mem {
            base: None,
            index: Some(reg),
            scale: 4,
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
                .chunks_exact(4)
                .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
                .collect(),
            ..Table::default()
        });
        work.trace.code.push((disp, len));
    }
    work.trace.code.sort_unstable();
}

impl Translator<'_> {
    pub(super) fn table_jump(&mut self, source: Src) -> bool {
        let Some(index) = self.tables.iter().position(|t| t.instruction == self.index) else {
            return false;
        };
        let Src::Mem(mem) = source else {
            unreachable!()
        };
        let reg = mem.index.unwrap();
        self.settle();
        let (table, stub) = (self.asm.label(), self.asm.label());
        self.asm.adr(X17, table);
        let load = ldr_reg(X, X17, X17, host(reg), true) | if biased(reg) { 0 } else { 1 << 13 };
        self.emit_all(&[load, br(X17)]);
        self.push_cold(Cold::Table { index, table, stub });
        true
    }

    pub(super) fn table_cold(&mut self, index: usize, table: Label, stub: Label, at: usize) {
        self.index = self.tables[index].instruction;
        self.checking = (self.checked)(self.trace.insts[self.index].pc);
        self.state = State::CANONICAL;
        self.vector = simd::Vector::default();
        self.asm.bind(stub);
        self.tables[index].stub = self.asm.offset();
        let first = self.sites.len();
        let Op::JmpIndirect(source) = self.trace.insts[self.index].op else {
            unreachable!()
        };
        self.target_to_w16(source);
        // Settlement already refunded the unused segment; faults refund only this jump.
        for (_, site) in &mut self.sites[first..] {
            site.refund = 1;
        }
        self.probe(false, IndirectBranchKind::Jump);
        if !(at + self.asm.offset()).is_multiple_of(8) {
            self.emit(NOP);
        }
        self.asm.bind(table);
        self.tables[index].offset = self.asm.offset();
        let address = (at + self.asm.position(stub)) as u64;
        for _ in 0..self.tables[index].targets.len() {
            self.emit_all(&[address as u32, (address >> 32) as u32]);
        }
    }
}
