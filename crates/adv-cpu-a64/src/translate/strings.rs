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
//! When one unit is left, the `interpret` helper runs the next element, so
//! a REP the countdown cannot finish stops part way exactly as in the
//! interpreter, flags and continuation included. Checked
//! instructions (A64-MEM-6) and segment or address-size overrides stay with the
//! interpreter too.

use iced_x86::{Code, Instruction, Register};

use crate::encode::Shift::Lsl;
use crate::encode::Size::{W, X};
use crate::encode::*;

use super::{State, Translator, WINDOW, X6, X9, X10, X11, X12, X16, X17, host, temp};

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
    /// The helper that runs the instruction when it is checked.
    pub helper: u8,
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

/// The string instruction `instruction` is, when the loop handles it.
pub fn plan(instruction: &Instruction, helper: u8) -> Option<Strings> {
    use Code::*;
    let (kind, width) = match instruction.code() {
        Movsb_m8_m8 => (Kind::Movs, 1),
        Movsw_m16_m16 => (Kind::Movs, 2),
        Movsd_m32_m32 => (Kind::Movs, 4),
        Stosb_m8_AL => (Kind::Stos, 1),
        Stosw_m16_AX => (Kind::Stos, 2),
        Stosd_m32_EAX => (Kind::Stos, 4),
        Lodsb_AL_m8 => (Kind::Lods, 1),
        Lodsw_AX_m16 => (Kind::Lods, 2),
        Lodsd_EAX_m32 => (Kind::Lods, 4),
        Scasb_AL_m8 => (Kind::Scas, 1),
        Scasw_AX_m16 => (Kind::Scas, 2),
        Scasd_EAX_m32 => (Kind::Scas, 4),
        Cmpsb_m8_m8 => (Kind::Cmps, 1),
        Cmpsw_m16_m16 => (Kind::Cmps, 2),
        Cmpsd_m32_m32 => (Kind::Cmps, 4),
        _ => return None,
    };
    // ESI in its default segment, and 32-bit addresses.
    use iced_x86::OpKind::{MemoryESDI, MemorySegDI, MemorySegSI};
    let short = (0..instruction.op_count()).any(|n| {
        matches!(
            instruction.op_kind(n),
            MemorySegSI | MemorySegDI | MemoryESDI
        )
    });
    if short
        || instruction.has_lock_prefix()
        || !matches!(instruction.segment_prefix(), Register::None | Register::DS)
    {
        return None;
    }
    let compares = matches!(kind, Kind::Scas | Kind::Cmps);
    let rep = match (instruction.has_rep_prefix(), instruction.has_repne_prefix()) {
        (false, false) => Rep::None,
        (true, false) => Rep::Rep,
        (false, true) if compares => Rep::Repne,
        _ => return None,
    };
    Some(Strings {
        kind,
        width,
        rep,
        helper,
    })
}

impl Translator<'_> {
    pub(super) fn strings(&mut self, op: Strings) {
        self.flush_simd();
        if self.checking {
            self.helper(op.helper);
            return;
        }
        self.canonicalize();
        let (narrow, log) = match op.width {
            1 => (Some(Narrow::Byte), 0),
            2 => (Some(Narrow::Half), 1),
            _ => (None, 2),
        };
        let load = |rt: Reg, address: Reg| match narrow {
            Some(narrow) => ldr_narrow(narrow, rt, WINDOW, address),
            None => ldr_reg(W, rt, WINDOW, address, false),
        };
        let store = |rt: Reg, address: Reg| match narrow {
            Some(narrow) => str_narrow(narrow, rt, WINDOW, address),
            None => str_reg(W, rt, WINDOW, address, false),
        };
        let (eax, ecx, esi, edi) = (host(EAX), host(ECX), host(ESI), host(EDI));
        let rep = op.rep != Rep::None;
        let (top, finish, slow, done) = (
            self.asm.label(),
            self.asm.label(),
            self.asm.label(),
            self.asm.label(),
        );
        if rep {
            self.asm.cbz(W, false, ecx, done);
            self.asm.cbz(X, false, X9, slow);
        }
        // The step: the width, negated when DF is set.
        self.emit_all(&[
            movz(W, X17, u16::from(op.width), 0),
            sub(W, X17, X17, X12, Lsl, log + 1),
        ]);
        self.asm.bind(top);
        // The compared operands: the left one and w7.
        let left = match op.kind {
            Kind::Movs => {
                self.site();
                self.emit(load(X6, esi));
                self.site();
                self.emit_all(&[
                    store(X6, edi),
                    add(W, esi, esi, X17, Lsl, 0),
                    add(W, edi, edi, X17, Lsl, 0),
                ]);
                None
            }
            Kind::Stos => {
                self.site();
                self.emit_all(&[store(eax, edi), add(W, edi, edi, X17, Lsl, 0)]);
                None
            }
            Kind::Lods => {
                self.site();
                self.emit(load(X6, esi));
                self.emit_all(&[
                    match op.width {
                        4 => mov(W, eax, X6),
                        width => bfi(W, eax, X6, 0, 8 * u32::from(width)),
                    },
                    add(W, esi, esi, X17, Lsl, 0),
                ]);
                None
            }
            Kind::Scas => {
                self.site();
                self.emit_all(&[load(temp(0), edi), add(W, edi, edi, X17, Lsl, 0)]);
                Some(eax)
            }
            Kind::Cmps => {
                self.site();
                self.emit(load(X6, esi));
                self.site();
                self.emit_all(&[
                    load(temp(0), edi),
                    add(W, esi, esi, X17, Lsl, 0),
                    add(W, edi, edi, X17, Lsl, 0),
                ]);
                Some(X6)
            }
        };
        if rep {
            // An element charges the countdown once it completes; the
            // instruction's own charge pays for one of them.
            self.emit_all(&[sub_imm(W, ecx, ecx, 1, false), sub_imm(X, X9, X9, 1, false)]);
            if let Some(left) = left {
                // Flags change only once the REP ends: a fault in it
                // leaves those from before the instruction, as the
                // interpreter does.
                self.emit(sub(W, X16, left, temp(0), Lsl, 0));
                if op.width != 4 {
                    self.emit(lsl_imm(W, X16, X16, 32 - 8 * u32::from(op.width)));
                }
                self.asm.cbz(W, op.rep == Rep::Rep, X16, finish);
            }
            self.asm.cbz(W, false, ecx, finish);
            self.asm.cbz(X, true, X9, top);
            self.asm.b(slow);
            self.asm.bind(finish);
            self.emit(add_imm(X, X9, X9, 1, false));
        }
        if let Some(left) = left {
            self.compare(left, temp(0), op.width);
        }
        if rep {
            self.asm.b(done);
            self.asm.bind(slow);
            self.helper(op.helper);
        }
        self.asm.bind(done);
        self.state = State::CANONICAL;
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
        let plan = |build: fn(&mut CodeAssembler)| plan(&decode(build), 3);
        let movsd = plan(|a| a.rep().movsd().unwrap()).unwrap();
        assert_eq!(
            (movsd.kind, movsd.width, movsd.rep),
            (Kind::Movs, 4, Rep::Rep)
        );
        let scasb = plan(|a| a.repne().scasb().unwrap()).unwrap();
        assert_eq!(
            (scasb.kind, scasb.rep, scasb.helper),
            (Kind::Scas, Rep::Repne, 3)
        );
        let cmpsw = plan(|a| a.repe().cmpsw().unwrap()).unwrap();
        assert_eq!((cmpsw.width, cmpsw.rep), (2, Rep::Rep));
        assert_eq!(plan(|a| a.lodsb().unwrap()).unwrap().rep, Rep::None);
        // A source in FS, and REPNE before a move, stay with the interpreter.
        let fs_movsd = [0x64, 0xa5];
        let decoded =
            iced_x86::Decoder::with_ip(32, &fs_movsd, 0x1000, iced_x86::DecoderOptions::NONE)
                .decode();
        assert!(super::plan(&decoded, 0).is_none());
        assert!(plan(|a| a.repne().stosd().unwrap()).is_none());
    }
}
