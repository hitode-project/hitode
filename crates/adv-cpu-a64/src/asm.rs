//! An instruction buffer with labels.
//!
//! Branches to unbound labels are emitted as placeholders and patched by
//! `finish`, so code can jump forward without a second emission pass.

use crate::encode::{self, Cond, Reg, Size};

/// A position in an `Asm` buffer, bound at most once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Label(usize);

#[derive(Clone, Copy)]
enum Fixup {
    B,
    Bl,
    Cond(Cond),
    Cbz(Size, bool, Reg),
    Tbz(bool, Reg, u32),
    Adr(Reg),
}

impl Fixup {
    const fn encode(self, offset: i32) -> u32 {
        match self {
            Self::B => encode::b(offset),
            Self::Bl => encode::bl(offset),
            Self::Cond(cond) => encode::b_cond(cond, offset),
            Self::Cbz(size, nonzero, rt) => encode::cbz(size, nonzero, rt, offset),
            Self::Tbz(nonzero, rt, bit) => encode::tbz(nonzero, rt, bit, offset),
            Self::Adr(rd) => encode::adr(rd, offset),
        }
    }
}

#[derive(Default)]
pub struct Asm {
    words: Vec<u32>,
    labels: Vec<Option<usize>>,
    fixups: Vec<(usize, Label, Fixup)>,
}

impl Asm {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn emit(&mut self, word: u32) {
        self.words.push(word);
    }

    pub fn emit_all(&mut self, words: &[u32]) {
        self.words.extend_from_slice(words);
    }

    /// Byte offset of the next instruction.
    pub fn offset(&self) -> usize {
        self.words.len() * 4
    }

    pub fn label(&mut self) -> Label {
        self.labels.push(None);
        Label(self.labels.len() - 1)
    }

    pub fn bind(&mut self, label: Label) {
        assert!(self.labels[label.0].is_none(), "label bound twice");
        self.labels[label.0] = Some(self.words.len());
    }

    fn refer(&mut self, label: Label, fixup: Fixup) {
        self.fixups.push((self.words.len(), label, fixup));
        self.words.push(0);
    }

    pub fn b(&mut self, label: Label) {
        self.refer(label, Fixup::B);
    }

    pub fn bl(&mut self, label: Label) {
        self.refer(label, Fixup::Bl);
    }

    pub fn b_cond(&mut self, cond: Cond, label: Label) {
        self.refer(label, Fixup::Cond(cond));
    }

    pub fn cbz(&mut self, size: Size, nonzero: bool, rt: Reg, label: Label) {
        self.refer(label, Fixup::Cbz(size, nonzero, rt));
    }

    pub fn tbz(&mut self, nonzero: bool, rt: Reg, bit: u32, label: Label) {
        self.refer(label, Fixup::Tbz(nonzero, rt, bit));
    }

    pub fn adr(&mut self, rd: Reg, label: Label) {
        self.refer(label, Fixup::Adr(rd));
    }

    /// Byte offset of a bound label.
    pub fn position(&self, label: Label) -> usize {
        self.labels[label.0].expect("label is unbound") * 4
    }

    /// Patches every reference; panics on an unbound label.
    pub fn finish(mut self) -> Vec<u32> {
        for (at, label, fixup) in std::mem::take(&mut self.fixups) {
            let target = self.labels[label.0].expect("branch to an unbound label");
            let offset = (target as i64 - at as i64) * 4;
            self.words[at] = fixup.encode(offset as i32);
        }
        self.words
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encode::{NOP, Reg};

    #[test]
    fn forward_and_backward_references_resolve_to_their_labels() {
        let mut asm = Asm::new();
        let top = asm.label();
        let end = asm.label();
        asm.bind(top);
        asm.emit(NOP);
        asm.b_cond(Cond::Ne, end);
        asm.cbz(Size::X, true, Reg::new(9), top);
        asm.adr(Reg::new(17), end);
        asm.bind(end);
        asm.b(top);
        let words = asm.finish();
        assert_eq!(words[1], encode::b_cond(Cond::Ne, 12));
        assert_eq!(words[2], encode::cbz(Size::X, true, Reg::new(9), -8));
        assert_eq!(words[3], encode::adr(Reg::new(17), 4));
        assert_eq!(words[4], encode::b(-16));
    }

    #[test]
    #[should_panic(expected = "unbound")]
    fn an_unbound_label_is_rejected() {
        let mut asm = Asm::new();
        let nowhere = asm.label();
        asm.b(nowhere);
        asm.finish();
    }
}
