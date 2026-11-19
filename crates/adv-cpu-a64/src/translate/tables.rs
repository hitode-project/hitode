//! Switch-table lowering (A64-CTL-6).
use super::decode::{Op, Src};
use super::{Cold, Label, NOP, State, Translator, X, X17, biased, br, host, ldr_reg, simd};
use adv_cpu_core::IndirectBranchKind;

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
        self.table_positions[index].1 = self.asm.offset();
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
        self.table_positions[index].0 = self.asm.offset();
        let address = (at + self.asm.position(stub)) as u64;
        for _ in 0..self.tables[index].targets.len() {
            self.emit_all(&[address as u32, (address >> 32) as u32]);
        }
    }
}
