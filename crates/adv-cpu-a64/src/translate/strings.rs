//! String instructions, with or without a REP prefix, as host loops.
//!
//! An element loads and stores through the window at sites of its own, then
//! advances ESI and EDI by the width, negated when x12 (DF) is set, and a
//! REP counts ECX down. A fault in the middle of a REP leaves ESI, EDI and
//! ECX at the faulting element, as the hardware does, and the interpreter
//! continues from there. Flags are canonical from the start of the
//! instruction and a compare writes them only once it ends, so every site
//! sees those from before the instruction; the compares then write NZCV in
//! subtraction sense, x10 the difference and x11 the operands' and
//! difference's exclusive or, which is the canonical form.
//!
//! As in the interpreter, each element of a REP is a unit of the countdown.
//! A budget cut publishes the completed elements and resumes at the REP.
//! Comparisons publish the last flags and carry the original flags for an
//! eventual fault. Checked instructions reuse
//! the loop, caching each pointer's page and window or alias base. Entering
//! a different page, including an element's last byte, uses the access
//! emitter's permission checks without changing NZCV. A crossing element
//! needs one view that allows both pages. A blocked element leaves with a
//! precise access event. Source overrides use FS's thread base or the flat
//! segment base. Word addresses wrap before adding the segment base.

#[cfg(test)]
use super::front::plans::strings as plan;
use crate::translate::lower::stubs;
#[cfg(test)]
use iced_x86::Instruction;

use crate::asm::Label;
use crate::encode::Shift::Lsl;
use crate::encode::Size::{W, X};
use crate::encode::*;
use crate::runtime::{self, Event, Exit};

use super::{
    State, Translator, WINDOW, X0, X1, X2, X4, X5, X6, X9, X10, X11, X12, X13, X14, X15, X16, X17,
    host, temp,
};
use crate::translate::lower::access::PAGE_SHIFT;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Movs,
    Stos,
    Lods,
    Scas,
    Cmps,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rep {
    None,
    /// `rep`, or `repe` before a compare.
    #[allow(clippy::enum_variant_names)] // The x86 prefix name.
    Rep,
    /// `repne` before a compare.
    Repne,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Strings {
    pub kind: Kind,
    /// 1, 2 or 4 bytes.
    pub width: u8,
    pub rep: Rep,
    pub word: bool,
    pub fs: bool,
}

impl Strings {
    /// Whether it writes the arithmetic flags, which a REP of no elements
    /// leaves as they were.
    pub fn compares(self) -> bool {
        matches!(self.kind, Kind::Scas | Kind::Cmps)
    }
}

const EAX: u8 = 0;
const ECX: u8 = 1;
const ESI: u8 = 6;
const EDI: u8 = 7;

impl Translator<'_> {
    pub(super) fn strings(&mut self, op: Strings) {
        self.flush_simd();
        self.canonicalize();
        let (narrow, log) = match op.width {
            1 => (Some(Narrow::Byte), 0),
            2 => (Some(Narrow::Half), 1),
            _ => (None, 2),
        };
        let source = if self.checking { X2 } else { WINDOW };
        let destination = if self.checking { X15 } else { WINDOW };
        let load = |rt: Reg, address: Reg, base: Reg| match narrow {
            Some(narrow) => ldr_narrow(narrow, rt, base, address),
            None => ldr_reg(W, rt, base, address, false),
        };
        let store = |rt: Reg, address: Reg| match narrow {
            Some(narrow) => str_narrow(narrow, rt, destination, address),
            None => str_reg(W, rt, destination, address, false),
        };
        let (eax, ecx, esi, edi) = (host(EAX), host(ECX), host(ESI), host(EDI));
        let rep = op.rep != Rep::None;
        if rep {
            self.remark(crate::jit_map::Part::Rep);
        }
        let (top, finish, blocked, exhausted, done) = (
            self.asm.label(),
            self.asm.label(),
            self.asm.label(),
            self.asm.label(),
            self.asm.label(),
        );
        let refund = if rep {
            self.string_count_branch(op.word, ecx, done);
            // The instruction's prepaid unit can pay for its first element.
            self.emit(add_imm(X, X9, X9, 1, false));
            self.trace.unused(self.index)
        } else {
            self.trace.owed(self.index)
        };
        // The step: the width, negated when DF is set.
        self.emit_all(&[
            movz(W, X17, u16::from(op.width), 0),
            sub(W, X17, X17, X12, Lsl, log + 1),
        ]);
        if self.checking {
            // No guest page has this index: the first element checks both pointers.
            self.emit_all(&[movz(W, X13, 0xffff, 16), movz(W, X14, 0xffff, 16)]);
        }
        self.asm.bind(top);
        let mut from = esi;
        if op.word && matches!(op.kind, Kind::Movs | Kind::Lods | Kind::Cmps) {
            self.emit(ubfx(W, temp(1), esi, 0, 16));
            from = temp(1);
        }
        if op.fs {
            self.emit(ldr(X, X16, super::RUNTIME, runtime::CPU_STATE));
            self.emit(ldr(W, X16, X16, runtime::FS_BASE));
            self.emit(add(W, temp(1), X16, from, Lsl, 0));
            from = temp(1);
        }
        let to = if op.word && op.kind != Kind::Lods {
            self.emit(ubfx(W, temp(0), edi, 0, 16));
            temp(0)
        } else {
            edi
        };
        if self.checking {
            if matches!(op.kind, Kind::Movs | Kind::Lods | Kind::Cmps) {
                self.string_page(from, op.width, false, (X13, X0, source), blocked);
            }
            if op.kind != Kind::Lods {
                self.string_page(
                    to,
                    op.width,
                    matches!(op.kind, Kind::Movs | Kind::Stos),
                    (X14, X1, destination),
                    blocked,
                );
            }
        }
        // The compared operands: the left one and w7.
        let left = match op.kind {
            Kind::Movs => {
                self.site_with_refund(refund);
                self.emit(load(X6, from, source));
                self.site_with_refund(refund);
                self.emit(store(X6, to));
                self.string_advance(esi, op.word);
                self.string_advance(edi, op.word);
                None
            }
            Kind::Stos => {
                self.site_with_refund(refund);
                self.emit(store(eax, to));
                self.string_advance(edi, op.word);
                None
            }
            Kind::Lods => {
                self.site_with_refund(refund);
                self.emit(load(X6, from, source));
                self.emit(match op.width {
                    4 => mov(W, eax, X6),
                    width => bfi(W, eax, X6, 0, 8 * u32::from(width)),
                });
                self.string_advance(esi, op.word);
                None
            }
            Kind::Scas => {
                self.site_with_refund(refund);
                self.emit(load(temp(0), to, destination));
                self.string_advance(edi, op.word);
                Some(eax)
            }
            Kind::Cmps => {
                self.site_with_refund(refund);
                self.emit(load(X6, from, source));
                self.site_with_refund(refund);
                self.emit(load(temp(0), to, destination));
                self.string_advance(esi, op.word);
                self.string_advance(edi, op.word);
                Some(X6)
            }
        };
        if rep {
            let count = if op.word { X16 } else { ecx };
            self.emit(sub_imm(W, count, ecx, 1, false));
            if op.word {
                self.emit(bfi(W, ecx, count, 0, 16));
            }
            self.emit(sub_imm(X, X9, X9, 1, false));
            if let Some(left) = left {
                self.emit(sub(W, X16, left, temp(0), Lsl, 0));
                if op.width != 4 {
                    self.emit(lsl_imm(W, X16, X16, 32 - 8 * u32::from(op.width)));
                }
                self.asm.cbz(W, op.rep == Rep::Rep, X16, finish);
            }
            self.string_count_branch(op.word, ecx, finish);
            self.asm.cbz(X, true, X9, top);
            self.asm.b(exhausted);
            self.asm.bind(finish);
        }
        if let Some(left) = left {
            self.compare(left, temp(0), op.width);
        }
        let inst = self.trace.insts[self.index];
        if rep || self.checking {
            self.asm.b(done);
        }
        if self.checking {
            self.asm.bind(blocked);
            self.follow_exit(Exit::Event(Event::CheckedAccess), inst.pc, refund, false);
        }
        if rep {
            self.asm.bind(exhausted);
            if let Some(left) = left {
                self.emit(ldr(X, X0, super::RUNTIME, runtime::CPU_STATE));
                stubs::emit_read_flags(self.asm, X0);
                self.compare(left, temp(0), op.width);
                let saved_flags = if let Some(x87) = self.x87_leaving() {
                    self.emit(mov(W, temp(0), X4));
                    self.x87_leave(x87);
                    temp(0)
                } else {
                    X4
                };
                if refund != 0 {
                    self.emit(add_imm(X, X9, X9, refund, false));
                }
                let descriptor = 6
                    + op.width.trailing_zeros() as u16
                    + 6 * u16::from(op.kind == Kind::Scas)
                    + 3 * u16::from(op.rep == Rep::Repne)
                    + 32 * u16::from(op.word)
                    + 64 * u16::from(op.fs);
                self.emit_all(&[
                    movz(W, X0, runtime::REP_BUDGET | descriptor, 0),
                    orr(X, X0, X0, saved_flags, Lsl, 32),
                ]);
                self.target(inst.pc);
                self.far(self.stubs.exit);
            } else {
                self.follow_exit(self.budget_exit(), inst.pc, refund, false);
            }
        }
        self.asm.bind(done);
        self.vector.cpu = false;
        self.state = State::CANONICAL;
    }

    fn string_count_branch(&mut self, word: bool, count: Reg, label: Label) {
        let count = if word {
            self.emit(ubfx(W, X16, count, 0, 16));
            X16
        } else {
            count
        };
        self.asm.cbz(W, false, count, label);
    }

    fn string_advance(&mut self, reg: Reg, word: bool) {
        let result = if word { X16 } else { reg };
        self.emit(add(W, result, reg, X17, Lsl, 0));
        if word {
            self.emit(bfi(W, reg, result, 0, 16));
        }
    }

    /// Cache one pointer's last checked page and base, selecting a base for this element.
    fn string_page(
        &mut self,
        address: Reg,
        width: u8,
        store: bool,
        (page, base, element): (Reg, Reg, Reg),
        slow: Label,
    ) {
        let same = self.asm.label();
        self.emit_all(&[
            lsr_imm(W, X4, address, PAGE_SHIFT),
            eor(W, X4, X4, page, Lsl, 0),
        ]);
        self.asm.cbz(W, false, X4, same);
        self.checked_base(address, store, true, base, slow);
        self.emit(lsr_imm(W, page, address, PAGE_SHIFT));
        self.asm.bind(same);
        self.emit(mov(X, element, base));
        if width > 1 {
            let (crossing, done) = (self.asm.label(), self.asm.label());
            self.page_crossing(address, u32::from(width), crossing);
            self.asm.b(done);
            self.asm.bind(crossing);
            self.emit(add_imm(W, X5, address, u32::from(width) - 1, false));
            self.checked_base(X5, store, true, base, slow);
            self.emit_all(&[
                lsr_imm(W, page, X5, PAGE_SHIFT),
                eor(X, X4, element, base, Lsl, 0),
            ]);
            self.asm.cbz(X, false, X4, done);
            // Mixed views need the alias on both pages. Verify the raw class
            // of the window page too; its window grant may cover LINEAR or EXEC.
            let last = self.asm.label();
            self.emit(eor(X, X4, element, WINDOW, Lsl, 0));
            self.asm.cbz(X, true, X4, last);
            self.checked_alias(address, store, true, slow);
            self.emit(mov(X, element, base));
            self.asm.b(done);
            self.asm.bind(last);
            self.checked_alias(X5, store, true, slow);
            self.asm.bind(done);
        }
    }

    /// Canonical flags of `left - right` at `width`; `left` may have bits
    /// above the width, `right` none.
    fn compare(&mut self, left: Reg, right: Reg, width: u8) {
        if width == 4 {
            self.emit(subs(W, Reg::ZR, left, right, Lsl, 0));
        } else {
            let shift = 32 - 8 * u32::from(width);
            self.emit_all(&[
                lsl_imm(W, X16, left, shift),
                subs(W, Reg::ZR, X16, right, Lsl, shift),
            ]);
        }
        self.emit_all(&[
            sub(W, X10, left, right, Lsl, 0),
            eor(W, X11, left, right, Lsl, 0),
            eor(W, X11, X11, X10, Lsl, 0),
        ]);
    }
}

#[cfg(test)]
mod tests {
    use iced_x86::code_asm::*;

    use super::*;

    fn decode(build: impl FnOnce(&mut CodeAssembler)) -> Instruction {
        let mut a = CodeAssembler::new(32).unwrap();
        build(&mut a);
        let bytes = a.assemble(0x1000).unwrap();
        iced_x86::Decoder::with_ip(32, &bytes, 0x1000, iced_x86::DecoderOptions::NONE).decode()
    }

    #[test]
    fn plans_follow_prefixes_and_segments() {
        let plan = |build: fn(&mut CodeAssembler)| plan(&decode(build));
        let movsd = plan(|a| a.rep().movsd().unwrap()).unwrap();
        assert_eq!(
            (movsd.kind, movsd.width, movsd.rep),
            (Kind::Movs, 4, Rep::Rep)
        );
        let scasb = plan(|a| a.repne().scasb().unwrap()).unwrap();
        assert_eq!(
            (scasb.kind, scasb.rep, scasb.word),
            (Kind::Scas, Rep::Repne, false)
        );
        let cmpsw = plan(|a| a.repe().cmpsw().unwrap()).unwrap();
        assert_eq!((cmpsw.width, cmpsw.rep), (2, Rep::Rep));
        assert_eq!(plan(|a| a.lodsb().unwrap()).unwrap().rep, Rep::None);
        // Source overrides and either repeat prefix retain a native loop.
        let fs_movsd = [0x64, 0xa5];
        let decoded =
            iced_x86::Decoder::with_ip(32, &fs_movsd, 0x1000, iced_x86::DecoderOptions::NONE)
                .decode();
        assert!(super::plan(&decoded).unwrap().fs);
        assert_eq!(
            plan(|a| a.repne().stosd().unwrap()).unwrap().rep,
            Rep::Repne
        );
    }
}
