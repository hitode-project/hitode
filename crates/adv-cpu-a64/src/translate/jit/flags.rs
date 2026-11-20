//! Tier-1 EFLAGS recipes and conversions at instruction and edge boundaries.

use crate::encode::Extend::Uxtw;
use crate::encode::Shift::Lsl;
use crate::encode::Size::{W, X};
use crate::encode::*;
use crate::translate::front::flags::FlagSet;
use crate::translate::front::tags::ConditionCode;
use crate::translate::lower::flags::{Carry, Operand, Overflow, Recipe, Sense};

use super::{State, Translator, X10, X11, X13, X14, X15, X17, biased, host, temp};

impl Translator<'_> {
    pub(super) fn needs(&self, flags: FlagSet) -> bool {
        let state = self.state;
        flags.any(FlagSet::CF) && (state.carry != Carry::Host || state.sense != Sense::Subtraction)
            || flags.any(FlagSet::PF.or(FlagSet::AF)) && state.recipe != Recipe::Canonical
            || flags.any(FlagSet::ZF.or(FlagSet::SF)) && state.nz.is_some()
            || flags.any(FlagSet::OF) && state.overflow != Overflow::Host
    }

    /// Puts `flags` in canonical form; the others keep their form.
    pub(super) fn convert(&mut self, flags: FlagSet) {
        self.host_nzv(flags);
        if flags.any(FlagSet::CF) {
            self.host_carry();
            self.set_sense(Sense::Subtraction);
        }
        if flags.any(FlagSet::PF.or(FlagSet::AF)) {
            self.materialize();
        }
    }

    pub(super) fn canonicalize(&mut self) {
        self.convert(FlagSet::ALL);
    }

    /// Every written flag is dead: forget how the old ones were held.
    pub(super) fn dead(&mut self) {
        self.state.carry = Carry::Host;
        self.state.derivable = None;
        self.state.recipe = Recipe::Canonical;
        self.state.nz = None;
        self.state.overflow = Overflow::Host;
    }

    /// Called before an instruction writes `operand`: keeps what the flags
    /// still need from it. `forget` must follow the write.
    pub(super) fn prepare(&mut self, operand: Operand) {
        if self.state.nz == Some(operand) && self.keep.any(FlagSet::SF.or(FlagSet::ZF)) {
            self.host_nzv(FlagSet::ZF.or(FlagSet::SF));
        }
        if self.state.overflow.uses(operand) && self.keep.any(FlagSet::OF) {
            self.host_nzv(FlagSet::OF);
        }
        if self.state.recipe.uses(operand) && self.keep.any(FlagSet::PF.or(FlagSet::AF)) {
            self.materialize();
        }
        if self.state.carry.uses(operand) && self.keep.any(FlagSet::CF) {
            self.host_carry();
        }
    }

    /// After `operand` changed: drops what still reads it, all dead.
    pub(super) fn forget(&mut self, operand: Operand) {
        let state = &mut self.state;
        if state.nz == Some(operand) {
            state.nz = None;
        }
        if state.overflow.uses(operand) {
            state.overflow = Overflow::Host;
        }
        if state.recipe.uses(operand) {
            state.recipe = Recipe::Canonical;
        }
        if state.derivable.is_some_and(|carry| carry.uses(operand)) {
            state.derivable = None;
        }
        if state.carry.uses(operand) {
            state.carry = Carry::Host;
        }
    }

    /// `prepare` and `forget` around a write with no site between.
    pub(super) fn clobber(&mut self, operand: Operand) {
        self.prepare(operand);
        self.forget(operand);
    }

    /// A recipe temporary neither the recipe nor a lazy carry reads.
    pub(super) fn take_temp(&mut self) -> u8 {
        let free = |state: &State, t: u8| {
            !state.recipe.uses(Operand::Temp(t))
                && !state.carry.uses(Operand::Temp(t))
                && state.nz != Some(Operand::Temp(t))
                && !state.overflow.uses(Operand::Temp(t))
        };
        if !free(&self.state, 0) && !free(&self.state, 1) {
            self.canonicalize();
        }
        if free(&self.state, 0) { 0 } else { 1 }
    }

    /// Restores pending N/Z/V without changing C or the other flag sources.
    pub(super) fn host_nzv(&mut self, flags: FlagSet) {
        let wanted = flags.any(FlagSet::ZF.or(FlagSet::SF.or(FlagSet::OF)));
        if self.features.flagm
            && wanted
            && let Some(result) = self.state.nz
            && self.state.overflow != Overflow::Host
        {
            // V is pending, so a compare may set it; C goes back after.
            let result = self.narrow(result);
            self.emit(cset(W, X13, Cond::Hs));
            match self.state.overflow {
                // The step again from its operand: N, Z and V as it sets them.
                Overflow::Step(a, increment) if self.state.nz == Some(a) => {
                    self.state.overflow = Overflow::Host;
                    self.emit_all(&if increment {
                        [
                            sub_imm(W, X14, result, 1, false),
                            adds_imm(W, Reg::ZR, X14, 1, false),
                        ]
                    } else {
                        [
                            add_imm(W, X14, result, 1, false),
                            subs_imm(W, Reg::ZR, X14, 1, false),
                        ]
                    });
                }
                _ => self.emit(tst(W, result, result, Lsl, 0)),
            }
            self.state.nz = None;
            self.insert_flags(Some((X13, 0)), None);
        }
        if flags.any(FlagSet::ZF.or(FlagSet::SF))
            && let Some(result) = self.state.nz.take()
        {
            let result = self.narrow(result);
            self.emit_all(&[
                mrs_nzcv(X13),
                ubfx(W, X14, result, 31, 1),
                bfi(X, X13, X14, 31, 1),
                clz(W, X14, result),
                lsr_imm(W, X14, X14, 5),
                bfi(X, X13, X14, 30, 1),
                msr_nzcv(X13),
            ]);
        }
        if !flags.any(FlagSet::OF) {
            return;
        }
        let overflow = std::mem::replace(&mut self.state.overflow, Overflow::Host);
        let (reg, bit) = match overflow {
            Overflow::Host => return,
            Overflow::Clear => (Reg::ZR, 0),
            Overflow::Bit(a, n) => (self.narrow(a), u32::from(n)),
            Overflow::Xor(a, n, m) => {
                let a = self.narrow(a);
                self.emit(eor(W, X17, a, a, Shift::Lsr, u32::from(n - m)));
                (X17, u32::from(m))
            }
            Overflow::Step(a, increment) => {
                let a = self.narrow(a);
                let expected = if increment { 0x8000_0000 } else { 0x7fff_ffff };
                self.emit_all(&[eor_imm(W, X17, a, expected), clz(W, X17, X17)]);
                (X17, 5)
            }
        };
        self.insert_flags(None, Some((reg, bit)));
    }

    /// Makes x10 and x11 canonical for the current recipe; uses w17.
    pub(super) fn materialize(&mut self) {
        use Recipe::*;
        let recipe = std::mem::replace(&mut self.state.recipe, Canonical);
        let value = |t: &mut Self, operand: Operand| match operand {
            Operand::Guest(index) => host(index),
            Operand::Temp(index) => temp(index),
            Operand::Imm(value) => t.imm(value, X17),
        };
        match recipe {
            Canonical => {}
            AddResult(r, b) | SubResult(r, b) => {
                let adding = matches!(recipe, AddResult(..));
                let r = value(self, r);
                self.emit(mov(W, X10, r));
                match b {
                    // The left operand back, XOR the result; bit 4 of a small
                    // right operand is often clear.
                    Operand::Imm(imm) if imm < 4096 => {
                        self.emit(if adding {
                            sub_imm(W, X11, r, imm, false)
                        } else {
                            add_imm(W, X11, r, imm, false)
                        });
                        if imm & 0x10 != 0 {
                            self.emit(eor_imm(W, X11, X11, 0x10));
                        }
                    }
                    _ => {
                        let b = value(self, b);
                        self.emit(if adding {
                            sub(W, X11, r, b, Lsl, 0)
                        } else {
                            add(W, X11, r, b, Lsl, 0)
                        });
                        self.emit(eor(W, X11, X11, b, Lsl, 0));
                    }
                }
                self.emit(eor(W, X11, X11, r, Lsl, 0));
            }
            Add(a, b) | Sub(a, b) => {
                let (a, b) = (value(self, a), value(self, b));
                if matches!(recipe, Add(..)) {
                    self.emit(add(W, X10, a, b, Lsl, 0));
                } else {
                    self.emit(sub(W, X10, a, b, Lsl, 0));
                }
                self.emit(eor(W, X11, a, b, Lsl, 0));
                self.emit(eor(W, X11, X11, X10, Lsl, 0));
            }
            Logic(r) => {
                let r = value(self, r);
                self.emit_all(&[mov(W, X10, r), mov(W, X11, Reg::ZR)]);
            }
            And(a, b) | Or(a, b) | Xor(a, b) => {
                let (a, b) = (value(self, a), value(self, b));
                self.emit(match recipe {
                    And(..) => and(W, X10, a, b, Lsl, 0),
                    Or(..) => orr(W, X10, a, b, Lsl, 0),
                    _ => eor(W, X10, a, b, Lsl, 0),
                });
                self.emit(mov(W, X11, Reg::ZR));
            }
            Neg(r) => {
                let r = value(self, r);
                self.emit_all(&[
                    mov(W, X10, r),
                    sub(W, X11, Reg::ZR, r, Lsl, 0),
                    eor(W, X11, X11, r, Lsl, 0),
                ]);
            }
            Doubled(r) => {
                let r = value(self, r);
                self.emit_all(&[mov(W, X10, r), mov(W, X11, r)]);
            }
            Parity(r) => {
                let r = value(self, r);
                self.emit(mov(W, X10, r));
            }
            Mixed(r, t) => {
                let (r, t) = (value(self, r), value(self, t));
                self.emit_all(&[mov(W, X10, r), eor(W, X11, t, r, Lsl, 0)]);
            }
        }
    }

    /// A zero-extended X register holding `operand`; uses x15.
    pub(super) fn wide(&mut self, operand: Operand) -> Reg {
        match operand {
            Operand::Guest(index) if biased(index) => {
                self.emit(mov(W, X15, host(index)));
                X15
            }
            Operand::Guest(index) => host(index),
            Operand::Temp(index) => temp(index),
            Operand::Imm(value) => {
                self.mov_imm(X15, value);
                X15
            }
        }
    }

    /// A register whose low half holds `operand`; uses w17.
    pub(super) fn narrow(&mut self, operand: Operand) -> Reg {
        match operand {
            Operand::Guest(index) => host(index),
            Operand::Temp(index) => temp(index),
            Operand::Imm(value) => self.imm(value, X17),
        }
    }

    /// Moves a lazy CF into C, in addition sense; uses x15 and w17.
    pub(super) fn host_carry(&mut self) {
        let carry = self.state.carry;
        let bit = match carry {
            Carry::Host => return,
            Carry::Bit(a, n) => {
                let a = self.narrow(a);
                (a, u32::from(n))
            }
            Carry::Below(a, b) => {
                // a - b in 64 bits is negative exactly when a < b.
                let (a, b) = (self.wide(a), self.narrow(b));
                self.emit(sub_ext(X, X15, a, b, Uxtw, 0));
                (X15, 63)
            }
            Carry::Sum(a, b) => {
                let (a, b) = (self.wide(a), self.narrow(b));
                self.emit(add_ext(X, X15, a, b, Uxtw, 0));
                (X15, 32)
            }
            Carry::Nonzero(a) => {
                let a = self.wide(a);
                self.emit(sub(X, X15, Reg::ZR, a, Lsl, 0));
                (X15, 63)
            }
            Carry::Clear => (Reg::ZR, 0),
        };
        self.insert_flags(Some(bit), None);
        self.state = State {
            sense: Sense::Addition,
            carry: Carry::Host,
            derivable: Some(carry),
            ..self.state
        };
    }

    /// Flips C to `sense`; C must hold CF. Uses x17.
    pub(super) fn set_sense(&mut self, sense: Sense) {
        if self.state.sense == sense {
            return;
        }
        if self.features.flagm {
            self.emit(CFINV);
        } else {
            self.emit_all(&[mrs_nzcv(X17), eor_imm(X, X17, X17, 1 << 29), msr_nzcv(X17)]);
        }
        self.state.sense = sense;
    }

    /// Sets C to bit `carry.1` of `carry.0` and V to bit `overflow.1` of
    /// `overflow.0`, each if given, keeping the other flags; uses x13 and
    /// x14 without FlagM.
    pub(super) fn insert_flags(&mut self, carry: Option<(Reg, u32)>, overflow: Option<(Reg, u32)>) {
        if self.features.flagm {
            if let Some((reg, bit)) = carry {
                self.emit(rmif(reg, (bit + 63) % 64, 0b0010));
            }
            if let Some((reg, bit)) = overflow {
                self.emit(rmif(reg, bit, 0b0001));
            }
            return;
        }
        self.emit(mrs_nzcv(X13));
        for ((reg, bit), at) in [(carry, 29), (overflow, 28)]
            .into_iter()
            .filter_map(|(flag, at)| Some((flag?, at)))
        {
            self.emit_all(&[ubfx(X, X14, reg, bit, 1), bfi(X, X13, X14, at, 1)]);
        }
        self.emit(msr_nzcv(X13));
    }

    /// The A64 condition for `cc`, moving CF to C where `cc` reads it and
    /// flipping to subtraction sense where no single condition reads it.
    pub(super) fn condition(&mut self, cc: ConditionCode) -> Cond {
        use ConditionCode as C;
        self.host_nzv(FlagSet::of(cc));
        if FlagSet::of(cc).any(FlagSet::CF) {
            self.host_carry();
        }
        let addition = self.state.sense == Sense::Addition;
        match cc {
            C::o => Cond::Vs,
            C::no => Cond::Vc,
            C::e => Cond::Eq,
            C::ne => Cond::Ne,
            C::s => Cond::Mi,
            C::ns => Cond::Pl,
            C::l => Cond::Lt,
            C::ge => Cond::Ge,
            C::le => Cond::Le,
            C::g => Cond::Gt,
            C::b if addition => Cond::Hs,
            C::b => Cond::Lo,
            C::ae if addition => Cond::Lo,
            C::ae => Cond::Hs,
            C::be | C::a => {
                self.set_sense(Sense::Subtraction);
                if cc == C::be { Cond::Ls } else { Cond::Hi }
            }
            C::p | C::np | C::None => unreachable!("tested by `test`, or filtered by `supported`"),
        }
    }
}
