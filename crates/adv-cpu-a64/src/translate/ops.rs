//! Each guest instruction's host code, with the flag work liveness allows.

use crate::encode::Extend::{self, Uxtw};
use crate::encode::Index::{Post, Pre};
use crate::encode::Shift::{Asr, Lsl, Lsr};
use crate::encode::Size::{W, X};
use crate::encode::*;

use super::decode::{Alu, ESP, Mem, Narrow8or16, Op, Part, ShiftKind, Src};
use super::flags::{Carry, FlagSet, Operand, Recipe, Sense};
use super::trace::{Inst, Mode};
use super::{
    Cold, RUNTIME, SHADOW, State, Translator, WINDOW, X0, X1, X3, X4, X5, X6, X16, X17, biased,
    host, temp,
};
use crate::runtime::{self, ACCESS_SIGNED, ACCESS_STORE};
use adv_cpu_core::memory::{EXEC, READ, SHARED, WRITE};

/// An address operand ready for a load or store.
#[derive(Clone, Copy)]
pub(super) enum Addr {
    /// A W register holding the guest address: `[x27, w, uxtw]`.
    Window(Reg),
    /// A biased stack register plus an offset the guards cover.
    Biased(Reg, i32),
}

/// The register a 32-bit result for guest `index` is computed into.
pub(super) fn result(index: u8) -> Reg {
    if biased(index) { X16 } else { host(index) }
}

/// The guest register and bit offset of a narrow register operand: AH to
/// BH are bits 8 to 15 of EAX to EBX.
fn part_register(number: u8, width: u8) -> (u8, u32) {
    if width == 1 && number >= 4 {
        (number - 4, 8)
    } else {
        (number, 0)
    }
}

impl Translator<'_> {
    pub(super) fn op(&mut self, inst: Inst) {
        match inst.op {
            Op::Nop | Op::Jmp(_) => {}
            Op::Helper(index) => self.helper(index),
            Op::Simd(simd) => self.simd(simd),
            Op::Strings(strings) => self.strings(strings),
            Op::Integer(integer) => self.integer(integer),
            Op::X87(x87) => self.x87(x87),
            Op::Direction(set) => self.emit(movz(W, super::X12, u16::from(set), 0)),
            Op::Mov(Src::Reg(dst), source) => self.mov_register(dst, source),
            Op::Mov(Src::Mem(mem), source) => {
                let value = match source {
                    // The low halves of x23 and x24 are ESP and EBP.
                    Src::Reg(source) => host(source),
                    Src::Imm(value) => self.imm(value, X6),
                    Src::Mem(_) => unreachable!(),
                };
                let addr = self.address(mem);
                self.store(addr, value);
            }
            Op::Mov(Src::Imm(_), _) => unreachable!(),
            Op::Lea(dst, mem) => {
                let address = self.effective(mem);
                self.clobber(Operand::Guest(dst));
                if biased(dst) {
                    self.emit(add_ext(X, host(dst), WINDOW, address, Uxtw, 0));
                } else if address != host(dst) {
                    self.emit(mov(W, host(dst), address));
                }
            }
            Op::Alu(alu, Src::Reg(dst), source) => self.alu_register(alu, dst, source),
            Op::Alu(alu, Src::Mem(mem), source) => self.alu_memory(alu, mem, source),
            Op::Alu(_, Src::Imm(_), _) => unreachable!(),
            Op::Step(increment, index) => self.step(increment, index),
            Op::StepMemory(increment, mem) => self.step_memory(increment, mem),
            Op::Narrow(alu, width, dst, src) => self.narrow_op(alu, width, dst, src),
            Op::Neg(index) => {
                let guest = Operand::Guest(index);
                self.emit(subs(W, result(index), Reg::ZR, host(index), Lsl, 0));
                self.commit(index);
                self.state = State {
                    sense: Sense::Subtraction,
                    carry: Carry::Host,
                    derivable: Some(Carry::Nonzero(guest)),
                    recipe: Recipe::Neg(guest),
                };
            }
            Op::Not(index) => {
                self.clobber(Operand::Guest(index));
                self.emit(mvn(W, result(index), host(index)));
                self.commit(index);
            }
            Op::Extend(dst, source, signed) => self.extend(dst, source, signed),
            Op::Cdq => {
                self.clobber(Operand::Guest(2));
                self.emit(sbfx(W, host(2), host(0), 31, 1));
            }
            Op::Push(source) => {
                let value = match source {
                    Src::Reg(ESP) => {
                        self.emit(mov(W, X6, host(ESP)));
                        X6
                    }
                    Src::Reg(index) => host(index),
                    Src::Imm(value) => self.imm(value, X6),
                    Src::Mem(mem) => {
                        let addr = self.address(mem);
                        self.load(addr, X6);
                        X6
                    }
                };
                self.push(value);
            }
            Op::Pop(index) => {
                let (esp, guest) = (Operand::Guest(ESP), Operand::Guest(index));
                self.prepare(esp);
                self.prepare(guest);
                if self.checking {
                    let direct = ldr_reg(W, result(index), WINDOW, host(ESP), false);
                    self.checked(host(ESP), result(index), 4, direct);
                    self.emit(add_imm(X, host(ESP), host(ESP), 4, false));
                } else {
                    self.site();
                    self.emit(ldr_indexed(W, Post, result(index), host(ESP), 4));
                }
                self.forget(esp);
                self.forget(guest);
                self.commit(index);
            }
            Op::Call(target) => {
                self.mov_imm(X6, inst.next_pc);
                self.push(X6);
                if inst.mode == Mode::Real {
                    self.real_call(Some(target));
                }
            }
            Op::Jcc(cc, _) => self.branch(cc, inst.taken.expect("jcc edges are resolved")),
            Op::Ret(release) => {
                let esp = Operand::Guest(ESP);
                self.prepare(esp);
                if self.checking {
                    let direct = ldr_reg(W, X16, WINDOW, host(ESP), false);
                    self.checked(host(ESP), X16, 4, direct);
                    self.emit(add_imm(X, host(ESP), host(ESP), 4, false));
                } else {
                    self.site();
                    self.emit(ldr_indexed(W, Post, X16, host(ESP), 4));
                }
                self.forget(esp);
                if release != 0 {
                    self.emit(add_imm(X, host(ESP), host(ESP), u32::from(release), false));
                }
                match inst.mode {
                    Mode::Inline(expected) => {
                        // The callee may have changed its return address.
                        self.mov_imm(X17, expected);
                        self.emit(sub(W, X17, X16, X17, Lsl, 0));
                        let label = self.asm.label();
                        self.asm.cbz(W, true, X17, label);
                        self.cold.push(Cold::Lookup {
                            label,
                            state: self.state,
                            refund: self.trace.unused(self.index),
                        });
                    }
                    _ => {
                        self.settle();
                        self.shadow_return();
                    }
                }
            }
            Op::JmpIndirect(source) => {
                self.target_to_w16(source);
                self.settle();
                self.probe(false);
            }
            Op::CallIndirect(source) => {
                self.target_to_w16(source);
                self.mov_imm(X6, inst.next_pc);
                self.push(X6);
                if inst.mode == Mode::Real {
                    self.real_call(None);
                } else {
                    self.settle();
                    self.probe(false);
                }
            }
            Op::Setcc(cc, byte) => {
                let (index, lsb) = if byte < 4 { (byte, 0) } else { (byte - 4, 8) };
                let cond = self.condition(cc);
                self.clobber(Operand::Guest(index));
                self.emit_all(&[cset(W, X17, cond), bfi(W, host(index), X17, lsb, 8)]);
            }
            Op::Cmov(cc, dst, source) => {
                let value = match source {
                    Src::Reg(index) => host(index),
                    Src::Mem(mem) => {
                        // The load happens whatever the condition.
                        let addr = self.address(mem);
                        self.load(addr, X6);
                        X6
                    }
                    Src::Imm(_) => unreachable!(),
                };
                let cond = self.condition(cc);
                self.clobber(Operand::Guest(dst));
                self.emit(csel(W, host(dst), value, host(dst), cond));
            }
            Op::Shift(kind, dst, count) => self.shift(kind, dst, count),
            Op::Imul(dst, source, imm) => self.imul(dst, source, imm),
            Op::Carry(subtract, dst, source) => self.carry(subtract, dst, source),
        }
    }

    /// Commits a result computed into `result(index)`.
    pub(super) fn commit(&mut self, index: u8) {
        if biased(index) {
            self.emit(add_ext(X, host(index), WINDOW, X16, Uxtw, 0));
        }
    }

    /// Canonical flags and the refund before leaving through a dynamic
    /// transfer, whose EIP is in w16.
    fn settle(&mut self) {
        self.canonicalize();
        let refund = self.trace.unused(self.index);
        if refund != 0 {
            self.emit(add_imm(X, super::X9, super::X9, refund, false));
        }
    }

    /// Branches to the unit for the EIP in w16 through the indirect table,
    /// from this site's own branch (§7.3); `link` calls it.
    fn probe(&mut self, link: bool) {
        self.emit_all(&[
            ldr(X, X17, RUNTIME, runtime::TABLE),
            ubfx(X, X6, X16, 2, 16),
            add(X, X17, X17, X6, Lsl, 4),
            ldp(X, crate::encode::Index::Offset, X6, X17, X17, 0),
            sub(X, X6, X6, X16, Lsl, 0),
        ]);
        self.far_cbnz(X, X6, self.stubs.lookup_slow);
        self.emit(if link { blr(X17) } else { br(X17) });
    }

    /// A return to the EIP in w16 through the shadow stack (§7.2).
    fn shadow_return(&mut self) {
        self.emit_all(&[
            ldp(X, crate::encode::Index::Offset, Reg::LR, X17, SHADOW, 0),
            sub(X, X17, X17, X16, Lsl, 0),
        ]);
        self.far_cbnz(X, X17, self.stubs.ret_miss);
        self.emit_all(&[add_imm(X, SHADOW, SHADOW, 16, false), ret(Reg::LR)]);
    }

    /// After a call pushed the return EIP from w6: pushes a shadow entry
    /// for the continuation, which follows, and calls the target, static
    /// or in w16.
    fn real_call(&mut self, target: Option<u32>) {
        self.settle();
        let continuation = self.asm.label();
        self.asm.adr(X17, continuation);
        self.emit(stp(X, Pre, X17, X6, SHADOW, -16));
        match target {
            Some(target) => {
                let (point, stub) = (self.asm.label(), self.asm.label());
                self.asm.bind(point);
                self.asm.bl(stub);
                self.cold.push(Cold::Call {
                    label: stub,
                    target,
                    point,
                });
            }
            None => self.probe(true),
        }
        self.asm.bind(continuation);
        self.state = State::CANONICAL;
    }

    fn push(&mut self, value: Reg) {
        let esp = Operand::Guest(ESP);
        self.prepare(esp);
        if self.checking {
            // ESP wraps like the guest's, and stays biased.
            self.emit(sub_imm(W, X5, host(ESP), 4, false));
            let direct = str_reg(W, value, WINDOW, X5, false);
            self.checked(X5, value, 4 | ACCESS_STORE, direct);
            self.emit(add_ext(X, host(ESP), WINDOW, X5, Uxtw, 0));
        } else {
            self.site();
            self.emit(str_indexed(W, Pre, value, host(ESP), -4));
        }
        self.forget(esp);
    }

    fn target_to_w16(&mut self, source: Src) {
        match source {
            Src::Reg(index) => self.emit(mov(W, X16, host(index))),
            Src::Mem(mem) => {
                let addr = self.address(mem);
                self.load(addr, X16);
            }
            Src::Imm(_) => unreachable!(),
        }
    }

    fn mov_register(&mut self, dst: u8, source: Src) {
        let guest = Operand::Guest(dst);
        match source {
            Src::Mem(mem) => {
                let addr = self.address(mem);
                self.prepare(guest);
                self.load(addr, result(dst));
                self.forget(guest);
                self.commit(dst);
            }
            Src::Reg(source) => {
                self.clobber(guest);
                if biased(dst) {
                    self.emit(add_ext(X, host(dst), WINDOW, host(source), Uxtw, 0));
                } else {
                    self.emit(mov(W, host(dst), host(source)));
                }
            }
            Src::Imm(value) => {
                self.clobber(guest);
                if biased(dst) {
                    let value = self.imm(value, X16);
                    self.emit(add_ext(X, host(dst), WINDOW, value, Uxtw, 0));
                } else {
                    self.mov_imm(host(dst), value);
                }
            }
        }
    }

    /// The guest address of `mem` in a W register; uses w16 and w17.
    pub(super) fn effective(&mut self, mem: Mem) -> Reg {
        let flat = self.flat(mem);
        if !mem.fs {
            return flat;
        }
        self.emit_all(&[
            ldr(X, X17, RUNTIME, runtime::CPU_STATE),
            ldr(W, X17, X17, runtime::FS_BASE),
            add(W, X16, flat, X17, Lsl, 0),
        ]);
        X16
    }

    /// `effective` without a segment base.
    fn flat(&mut self, mem: Mem) -> Reg {
        let mut current = mem.base.map(host);
        if let Some(index) = mem.index {
            let shift = mem.scale.trailing_zeros();
            current = Some(match current {
                Some(base) => {
                    self.emit(add(W, X16, base, host(index), Lsl, shift));
                    X16
                }
                None if shift == 0 => host(index),
                None => {
                    self.emit(lsl_imm(W, X16, host(index), shift));
                    X16
                }
            });
        }
        let disp = mem.disp;
        match current {
            None => self.imm(disp, X16),
            Some(base) if disp == 0 => base,
            Some(base) if disp < 4096 => {
                self.emit(add_imm(W, X16, base, disp, false));
                X16
            }
            Some(base) if disp.wrapping_neg() < 4096 => {
                self.emit(sub_imm(W, X16, base, disp.wrapping_neg(), false));
                X16
            }
            Some(base) => {
                self.mov_imm(X17, disp);
                self.emit(add(W, X16, base, X17, Lsl, 0));
                X16
            }
        }
    }

    /// An address for a 32-bit access to `mem`; a checked one is always in
    /// a register.
    pub(super) fn address(&mut self, mem: Mem) -> Addr {
        if let (Some(base), None) = (mem.base, mem.index)
            && biased(base)
            && !self.checking
            && !mem.fs
        {
            // Offsets well inside the guards: an access past 0 or 4 GiB
            // faults there and the interpreter wraps it exactly.
            let disp = mem.disp as i32;
            if (0..16384).contains(&disp) && disp % 4 == 0 || (-256..256).contains(&disp) {
                return Addr::Biased(host(base), disp);
            }
        }
        Addr::Window(self.effective(mem))
    }

    pub(super) fn load(&mut self, addr: Addr, rt: Reg) {
        if let (true, Addr::Window(a)) = (self.checking, addr) {
            self.checked(a, rt, 4, ldr_reg(W, rt, WINDOW, a, false));
            return;
        }
        self.site();
        self.emit(match addr {
            Addr::Window(a) => ldr_reg(W, rt, WINDOW, a, false),
            Addr::Biased(b, d) if d >= 0 && d % 4 == 0 => ldr(W, rt, b, d as u32),
            Addr::Biased(b, d) => ldur(W, rt, b, d),
        });
    }

    pub(super) fn store(&mut self, addr: Addr, rt: Reg) {
        if let (true, Addr::Window(a)) = (self.checking, addr) {
            self.checked(a, rt, 4 | ACCESS_STORE, str_reg(W, rt, WINDOW, a, false));
            return;
        }
        self.site();
        self.emit(match addr {
            Addr::Window(a) => str_reg(W, rt, WINDOW, a, false),
            Addr::Biased(b, d) if d >= 0 && d % 4 == 0 => str(W, rt, b, d as u32),
            Addr::Biased(b, d) => stur(W, rt, b, d),
        });
    }

    /// Has the interpreter run the instruction at helper `index` of the
    /// unit, with canonical flags; leaves when it asks to.
    pub(super) fn helper(&mut self, index: u8) {
        self.canonicalize();
        // The interpreter may change any SIMD register.
        self.vector = super::simd::Vector::default();
        self.mov_imm(X1, self.helpers + u32::from(index));
        self.far_call(self.stubs.interpret);
        self.state = State::CANONICAL;
        let leave = self.asm.label();
        self.asm.cbz(W, true, X0, leave);
        self.cold.push(Cold::Helper {
            label: leave,
            refund: self.trace.unused(self.index),
        });
    }

    /// A checked access (§6.3) of `kind` at the guest address in `address`,
    /// whose window form is `direct`: the window takes it if it stays in
    /// one page whose class allows it, else the `access` stub does, and
    /// loads `rt` or stores it. Uses x3 and x4.
    fn checked(&mut self, address: Reg, rt: Reg, kind: u32, direct: u32) {
        let slow = self.asm.label();
        let width = kind & 7;
        if width > 1 {
            // The last byte's address differs in bit 12 exactly when the
            // access crosses a page.
            self.emit_all(&[
                add_imm(W, X4, address, width - 1, false),
                eor(W, X4, X4, address, Lsl, 0),
            ]);
            self.asm.tbz(true, X4, 12, slow);
        }
        self.emit_all(&[
            ldr(X, X3, RUNTIME, runtime::CLASSES),
            lsr_imm(W, X4, address, 12),
            ldr_narrow(Narrow::Byte, X4, X3, X4),
        ]);
        // Permitted, not shared, and for a store not a code barrier.
        let bit = |permission: u8| permission.trailing_zeros();
        let (needed, excluded) = if kind & ACCESS_STORE != 0 {
            (WRITE, &[EXEC, SHARED][..])
        } else {
            (READ, &[SHARED][..])
        };
        self.asm.tbz(false, X4, bit(needed), slow);
        for &permission in excluded {
            self.asm.tbz(true, X4, bit(permission), slow);
        }
        let (site, resume) = (self.asm.label(), self.asm.label());
        self.asm.bind(site);
        self.site();
        self.emit(direct);
        self.asm.bind(resume);
        self.cold.push(Cold::Access {
            label: slow,
            resume,
            site,
            address,
            value: rt,
            kind,
        });
    }

    /// Loads a 32-bit source into a free recipe temporary.
    fn load_temp(&mut self, mem: Mem) -> u8 {
        let t = self.take_temp();
        let addr = self.address(mem);
        self.load(addr, temp(t));
        self.forget(Operand::Temp(t));
        t
    }

    fn extend(&mut self, dst: u8, source: Narrow8or16, signed: bool) {
        let field = |rd, rn, lsb, width| {
            if signed {
                sbfx(W, rd, rn, lsb, width)
            } else {
                ubfx(W, rd, rn, lsb, width)
            }
        };
        let guest = Operand::Guest(dst);
        match source {
            Narrow8or16::Byte(byte) => {
                let (index, lsb) = if byte < 4 { (byte, 0) } else { (byte - 4, 8) };
                self.clobber(guest);
                self.emit(field(result(dst), host(index), lsb, 8));
            }
            Narrow8or16::Word(index) => {
                self.clobber(guest);
                self.emit(field(result(dst), host(index), 0, 16));
            }
            Narrow8or16::Mem(mem, half) => {
                let address = self.effective(mem);
                let narrow = match (half, signed) {
                    (false, false) => Narrow::Byte,
                    (false, true) => Narrow::SignedByte,
                    (true, false) => Narrow::Half,
                    (true, true) => Narrow::SignedHalf,
                };
                self.prepare(guest);
                let direct = ldr_narrow(narrow, result(dst), WINDOW, address);
                if self.checking {
                    let width = if half { 2 } else { 1 };
                    let kind = width | if signed { ACCESS_SIGNED } else { 0 };
                    self.checked(address, result(dst), kind, direct);
                } else {
                    self.site();
                    self.emit(direct);
                }
                self.forget(guest);
            }
        }
        self.commit(dst);
    }

    /// `inc` or `dec`, which keep CF.
    fn step(&mut self, increment: bool, index: u8) {
        let guest = Operand::Guest(index);
        self.step_flags(increment, host(index), result(index), guest);
        self.commit(index);
        self.state.recipe = if increment {
            Recipe::AddResult(guest, Operand::Imm(1))
        } else {
            Recipe::SubResult(guest, Operand::Imm(1))
        };
    }

    /// `inc` or `dec` of a 32-bit memory operand: load, store the result,
    /// then the flags from the loaded value (§5.2).
    fn step_memory(&mut self, increment: bool, mem: Mem) {
        let t = self.take_temp();
        let addr = self.address(mem);
        let left = temp(t);
        self.load(addr, left);
        self.forget(Operand::Temp(t));
        self.emit(if increment {
            add_imm(W, X6, left, 1, false)
        } else {
            sub_imm(W, X6, left, 1, false)
        });
        self.store(addr, X6);
        self.step_flags(increment, left, Reg::ZR, Operand::Temp(t));
        self.state.recipe = if increment {
            Recipe::Add(Operand::Temp(t), Operand::Imm(1))
        } else {
            Recipe::Sub(Operand::Temp(t), Operand::Imm(1))
        };
    }

    /// The flags of `inc` or `dec` of `source`, which holds `guest`, with
    /// the result in `result`: CF keeps its value, lazily if it can. Leaves
    /// the recipe to the caller. Uses x6 and x17.
    fn step_flags(&mut self, increment: bool, source: Reg, result: Reg, guest: Operand) {
        let step = |rd| {
            if increment {
                adds_imm(W, rd, source, 1, false)
            } else {
                subs_imm(W, rd, source, 1, false)
            }
        };
        // CF is needed after: keep it lazy if possible, else in C.
        let mut keep_c = false;
        if self.keep.any(FlagSet::CF) {
            if self.state.carry.uses(guest) {
                self.host_carry();
            }
            if self.state.carry == Carry::Host {
                match self.state.derivable {
                    Some(carry) if !carry.uses(guest) => self.state.carry = carry,
                    _ => keep_c = true,
                }
            }
        }
        if !keep_c {
            self.emit(step(result));
        } else if self.features.flagm {
            self.emit_all(&[cset(W, X17, Cond::Hs), step(result), rmif(X17, 63, 2)]);
        } else {
            self.emit_all(&[
                mrs_nzcv(X17),
                step(result),
                mrs_nzcv(X6),
                lsr_imm(X, X17, X17, 29),
                bfi(X, X6, X17, 29, 1),
                msr_nzcv(X6),
            ]);
        }
        if !keep_c && self.state.carry == Carry::Host {
            // C is the step's, and CF dead.
            self.state.carry = Carry::Host;
        }
        self.state.derivable = None;
    }

    /// A 1- or 2-byte `mov` or ALU operation. Values are zero-extended into
    /// the recipe temporaries, so PF and AF recipes see their low bits; the
    /// flags come from the operands shifted to the top of a W register.
    /// Stores precede the flags (§5.2).
    fn narrow_op(&mut self, alu: Option<Alu>, width: u8, dst: Part, src: Part) {
        let shift = 32 - 8 * u32::from(width);
        // Both temporaries are overwritten. A fault site observes every
        // flag, so then nothing may still read them.
        let site = matches!(dst, Part::Mem(_)) || matches!(src, Part::Mem(_));
        for t in [Operand::Temp(0), Operand::Temp(1)] {
            if !site {
                self.prepare(t);
            } else if self.state.recipe.uses(t) {
                self.materialize();
            }
            if site && self.state.carry.uses(t) {
                self.host_carry();
            }
        }
        let address = match (dst, src) {
            (Part::Mem(mem), _) | (_, Part::Mem(mem)) => self.effective(mem),
            _ => Reg::ZR,
        };
        let right = match src {
            Part::Imm(value) => Operand::Imm(value),
            part => {
                self.narrow_read(part, width, address, temp(1));
                Operand::Temp(1)
            }
        };
        let right_reg = |t: &mut Self| match right {
            Operand::Imm(value) => t.imm(value, X17),
            _ => temp(1),
        };
        let Some(alu) = alu else {
            let value = right_reg(self);
            self.narrow_write(dst, width, address, value);
            for t in [Operand::Temp(0), Operand::Temp(1)] {
                self.forget(t);
            }
            return;
        };
        self.narrow_read(dst, width, address, temp(0));
        let left = Operand::Temp(0);
        let value = right_reg(self);
        let stored = match alu {
            Alu::Add => Some(add(W, X6, temp(0), value, Lsl, 0)),
            Alu::Sub => Some(sub(W, X6, temp(0), value, Lsl, 0)),
            Alu::And => Some(and(W, X6, temp(0), value, Lsl, 0)),
            Alu::Or => Some(orr(W, X6, temp(0), value, Lsl, 0)),
            Alu::Xor => Some(eor(W, X6, temp(0), value, Lsl, 0)),
            Alu::Cmp | Alu::Test => None,
        };
        if let Some(word) = stored {
            self.emit(word);
            self.narrow_write(dst, width, address, X6);
        }
        for t in [Operand::Temp(0), Operand::Temp(1)] {
            self.forget(t);
        }
        if !self.out.any(FlagSet::ALL) {
            self.dead();
            return;
        }
        // The right operand shifted to the top, as a register and a shift.
        let shifted = |t: &mut Self| match right {
            Operand::Imm(value) => (t.imm(value << shift, X16), 0),
            _ => (temp(1), shift),
        };
        let (sense, recipe) = match alu {
            Alu::Add | Alu::Sub | Alu::Cmp => {
                let (reg, amount) = shifted(self);
                self.emit(lsl_imm(W, X17, temp(0), shift));
                if alu == Alu::Add {
                    self.emit(adds(W, Reg::ZR, X17, reg, Lsl, amount));
                    (Sense::Addition, Recipe::Add(left, right))
                } else {
                    self.emit(subs(W, Reg::ZR, X17, reg, Lsl, amount));
                    (Sense::Subtraction, Recipe::Sub(left, right))
                }
            }
            Alu::And | Alu::Test => {
                let (reg, amount) = shifted(self);
                self.emit_all(&[
                    lsl_imm(W, X17, temp(0), shift),
                    ands(W, Reg::ZR, X17, reg, Lsl, amount),
                ]);
                (Sense::Addition, Recipe::And(left, right))
            }
            Alu::Or | Alu::Xor => {
                // N and Z of the result; C and V clear.
                self.emit(adds(W, Reg::ZR, Reg::ZR, X6, Lsl, shift));
                let recipe = if alu == Alu::Or {
                    Recipe::Or(left, right)
                } else {
                    Recipe::Xor(left, right)
                };
                (Sense::Addition, recipe)
            }
        };
        let logic = matches!(alu, Alu::And | Alu::Test | Alu::Or | Alu::Xor);
        self.state = State {
            sense,
            carry: Carry::Host,
            derivable: logic.then_some(Carry::Clear),
            recipe,
        };
    }

    /// Zero-extends the narrow `part` into `rt`; a memory part is at the
    /// guest address in `address`.
    fn narrow_read(&mut self, part: Part, width: u8, address: Reg, rt: Reg) {
        match part {
            Part::Reg(number) => {
                let (index, lsb) = part_register(number, width);
                self.emit(ubfx(W, rt, host(index), lsb, 8 * u32::from(width)));
            }
            Part::Imm(value) => self.mov_imm(rt, value),
            Part::Mem(_) => {
                let narrow = if width == 1 {
                    Narrow::Byte
                } else {
                    Narrow::Half
                };
                let direct = ldr_narrow(narrow, rt, WINDOW, address);
                if self.checking {
                    self.checked(address, rt, u32::from(width), direct);
                } else {
                    self.site();
                    self.emit(direct);
                }
            }
        }
    }

    /// Writes the low bytes of `value` to the narrow `part`.
    fn narrow_write(&mut self, part: Part, width: u8, address: Reg, value: Reg) {
        match part {
            Part::Reg(number) => {
                let (index, lsb) = part_register(number, width);
                let guest = Operand::Guest(index);
                self.prepare(guest);
                // SP and BP stay biased: keep the window in the high half.
                let size = if biased(index) { X } else { W };
                self.emit(bfi(size, host(index), value, lsb, 8 * u32::from(width)));
                self.forget(guest);
            }
            Part::Mem(_) => {
                let narrow = if width == 1 {
                    Narrow::Byte
                } else {
                    Narrow::Half
                };
                let direct = str_narrow(narrow, value, WINDOW, address);
                if self.checking {
                    self.checked(address, value, u32::from(width) | ACCESS_STORE, direct);
                } else {
                    self.site();
                    self.emit(direct);
                }
            }
            Part::Imm(_) => unreachable!("not a destination"),
        }
    }

    /// `alu reg, source`.
    fn alu_register(&mut self, alu: Alu, dst: u8, source: Src) {
        let left = host(dst);
        let result = result(dst);
        // The right operand as a register and as a recipe operand.
        let (right, operand) = match source {
            Src::Reg(index) => (Some(host(index)), Operand::Guest(index)),
            Src::Imm(value) => (None, Operand::Imm(value)),
            Src::Mem(mem) => {
                let t = self.load_temp(mem);
                (Some(temp(t)), Operand::Temp(t))
            }
        };
        let imm = match source {
            Src::Imm(value) => value,
            _ => 0,
        };
        let right = |t: &mut Self| right.unwrap_or_else(|| t.imm(imm, X17));
        let guest = Operand::Guest(dst);
        let live = self.out.any(FlagSet::ALL);
        match alu {
            Alu::Cmp | Alu::Test if !live => self.dead(),
            Alu::Add | Alu::Sub | Alu::Cmp => {
                let subtract = alu != Alu::Add;
                let rd = if alu == Alu::Cmp { Reg::ZR } else { result };
                let negated = imm.wrapping_neg();
                let word = if matches!(source, Src::Imm(_)) && imm < 4096 {
                    if subtract {
                        subs_imm(W, rd, left, imm, false)
                    } else {
                        adds_imm(W, rd, left, imm, false)
                    }
                } else if matches!(source, Src::Imm(_)) && imm != 0 && negated < 4096 {
                    // x + (2^32 - n) carries exactly when x - n does not
                    // borrow, so the sense is unchanged.
                    if subtract {
                        adds_imm(W, rd, left, negated, false)
                    } else {
                        subs_imm(W, rd, left, negated, false)
                    }
                } else {
                    let right = right(self);
                    if subtract {
                        subs(W, rd, left, right, Lsl, 0)
                    } else {
                        adds(W, rd, left, right, Lsl, 0)
                    }
                };
                self.emit(word);
                // The result overwrites a right operand that is `dst` itself.
                let same = operand == guest;
                self.state = State {
                    sense: if subtract {
                        Sense::Subtraction
                    } else {
                        Sense::Addition
                    },
                    carry: Carry::Host,
                    derivable: match alu {
                        Alu::Add if same => None,
                        Alu::Add => Some(Carry::Below(guest, operand)),
                        Alu::Sub if same => Some(Carry::Clear),
                        Alu::Sub => Some(Carry::Sum(guest, operand)),
                        _ => Some(Carry::Below(guest, operand)),
                    },
                    recipe: match alu {
                        Alu::Add if same => Recipe::Doubled(guest),
                        Alu::Sub if same => Recipe::Logic(guest),
                        Alu::Add => Recipe::AddResult(guest, operand),
                        Alu::Sub => Recipe::SubResult(guest, operand),
                        _ => Recipe::Sub(guest, operand),
                    },
                };
                if alu != Alu::Cmp {
                    self.commit(dst);
                }
            }
            Alu::And | Alu::Test => {
                let rd = if alu == Alu::Test { Reg::ZR } else { result };
                let word = match source {
                    Src::Imm(value) if bitmask(W, u64::from(value)).is_some() => {
                        ands_imm(W, rd, left, u64::from(value))
                    }
                    _ => {
                        let right = right(self);
                        ands(W, rd, left, right, Lsl, 0)
                    }
                };
                self.emit(word);
                self.state = State {
                    sense: Sense::Addition,
                    carry: Carry::Host,
                    derivable: Some(Carry::Clear),
                    recipe: if alu == Alu::And {
                        Recipe::Logic(guest)
                    } else {
                        Recipe::And(guest, operand)
                    },
                };
                if alu == Alu::And {
                    self.commit(dst);
                }
            }
            Alu::Or | Alu::Xor => {
                let word = match source {
                    Src::Imm(value) if bitmask(W, u64::from(value)).is_some() => {
                        if alu == Alu::Or {
                            orr_imm(W, result, left, u64::from(value))
                        } else {
                            eor_imm(W, result, left, u64::from(value))
                        }
                    }
                    _ => {
                        let right = right(self);
                        if alu == Alu::Or {
                            orr(W, result, left, right, Lsl, 0)
                        } else {
                            eor(W, result, left, right, Lsl, 0)
                        }
                    }
                };
                self.emit(word);
                // The flags NZCV holds, only if read; CF is clear anyway.
                let nzcv = self.out.any(FlagSet::NZV.or(FlagSet::CF));
                if nzcv {
                    self.emit(tst(W, result, result, Lsl, 0));
                }
                self.state = State {
                    sense: Sense::Addition,
                    carry: if nzcv { Carry::Host } else { Carry::Clear },
                    derivable: nzcv.then_some(Carry::Clear),
                    recipe: Recipe::Logic(guest),
                };
                self.commit(dst);
            }
        }
    }

    /// `alu [mem], source`: load, compute, store, then the flags (§5.2).
    fn alu_memory(&mut self, alu: Alu, mem: Mem, source: Src) {
        let t = self.take_temp();
        // The address first: it may use w17, and the result goes to w6.
        let addr = self.address(mem);
        let (right, operand) = match source {
            Src::Reg(index) => (host(index), Operand::Guest(index)),
            Src::Imm(value) => (self.imm(value, X17), Operand::Imm(value)),
            Src::Mem(_) => unreachable!(),
        };
        let left = temp(t);
        self.load(addr, left);
        let left_operand = Operand::Temp(t);
        self.forget(left_operand);
        let stored = match alu {
            Alu::Add => Some(add(W, X6, left, right, Lsl, 0)),
            Alu::Sub => Some(sub(W, X6, left, right, Lsl, 0)),
            Alu::And => Some(and(W, X6, left, right, Lsl, 0)),
            Alu::Or => Some(orr(W, X6, left, right, Lsl, 0)),
            Alu::Xor => Some(eor(W, X6, left, right, Lsl, 0)),
            Alu::Cmp | Alu::Test => None,
        };
        if let Some(word) = stored {
            self.emit(word);
            self.store(addr, X6);
        }
        if !self.out.any(FlagSet::ALL) {
            self.dead();
            return;
        }
        let nzcv = self.out.any(FlagSet::NZV.or(FlagSet::CF));
        let (word, sense, derivable, recipe) = match alu {
            Alu::Add => (
                Some(cmn(W, left, right, Lsl, 0)),
                Sense::Addition,
                Carry::Sum(left_operand, operand),
                Recipe::Add(left_operand, operand),
            ),
            Alu::Sub | Alu::Cmp => (
                Some(cmp(W, left, right, Lsl, 0)),
                Sense::Subtraction,
                Carry::Below(left_operand, operand),
                Recipe::Sub(left_operand, operand),
            ),
            Alu::And | Alu::Test => (
                Some(tst(W, left, right, Lsl, 0)),
                Sense::Addition,
                Carry::Clear,
                Recipe::And(left_operand, operand),
            ),
            Alu::Or => (
                nzcv.then(|| tst(W, X6, X6, Lsl, 0)),
                Sense::Addition,
                Carry::Clear,
                Recipe::Or(left_operand, operand),
            ),
            Alu::Xor => (
                nzcv.then(|| tst(W, X6, X6, Lsl, 0)),
                Sense::Addition,
                Carry::Clear,
                Recipe::Xor(left_operand, operand),
            ),
        };
        let carry = match word {
            Some(word) => {
                self.emit(word);
                Carry::Host
            }
            None => Carry::Clear,
        };
        self.state = State {
            sense,
            carry,
            derivable: (carry == Carry::Host).then_some(derivable),
            recipe,
        };
    }

    /// A register shifted or rotated by `count`, 1 to 31. Flags follow the
    /// interpreter: OF only changes for a one-bit count, AF never does.
    fn shift(&mut self, kind: ShiftKind, dst: u8, count: u32) {
        use ShiftKind::*;
        let guest = Operand::Guest(dst);
        let (source, result) = (host(dst), result(dst));
        let out = self.out;
        if matches!(kind, Rol | Ror) {
            // Only CF and OF change; the rest keep their form.
            self.clobber(guest);
            let rotate = if kind == Rol { 32 - count } else { count };
            self.emit(ror_imm(W, result, source, rotate));
            // CF is the bit that wrapped around. OF for one bit is the new
            // top bit XOR CF (rol) or XOR the next bit down (ror).
            let bit = if kind == Rol { 0 } else { 31 };
            let overflow = (count == 1 && out.any(FlagSet::OF)).then(|| {
                let shift = if kind == Rol { 31 } else { 1 };
                self.emit(eor(W, X17, result, result, Lsl, shift));
                (X17, 31)
            });
            let lazy = out.any(FlagSet::CF) && !biased(dst);
            let carry = (out.any(FlagSet::CF) && !lazy).then_some((result, bit));
            if carry.is_some() || overflow.is_some() {
                self.insert_flags(carry, overflow);
            }
            self.commit(dst);
            if lazy {
                self.state.carry = Carry::Bit(guest, bit as u8);
            } else {
                self.state.sense = Sense::Addition;
                self.state.carry = Carry::Host;
            }
            self.state.derivable = None;
            return;
        }
        // PF comes from the result and AF stays.
        let recipe = if self.state.recipe.clears_af() || !self.keep.any(FlagSet::AF) {
            Recipe::Logic(guest)
        } else {
            self.materialize();
            Recipe::Parity(guest)
        };
        let nzcv = out.any(FlagSet::NZV.or(FlagSet::CF));
        if !nzcv {
            self.emit(match kind {
                Shl => lsl_imm(W, result, source, count),
                Shr => lsr_imm(W, result, source, count),
                _ => asr_imm(W, result, source, count),
            });
        } else if kind == Shl && count == 1 {
            // CF is the old top bit and OF the new top bit XOR CF.
            self.emit(adds(W, result, source, source, Lsl, 0));
        } else {
            let kept = count > 1 && out.any(FlagSet::OF);
            if kept {
                self.emit(cset(W, X17, Cond::Vs));
            }
            let shift = match kind {
                Shl => Lsl,
                Shr => Lsr,
                _ => Asr,
            };
            // N and Z from the result, C and V clear.
            self.emit(adds(W, X16, Reg::ZR, source, shift, count));
            let carry = out
                .any(FlagSet::CF)
                .then_some((source, if kind == Shl { 32 - count } else { count - 1 }));
            let overflow = match kind {
                _ if kept => Some((X17, 0)),
                Shr if count == 1 => Some((source, 31)),
                _ => None,
            };
            if carry.is_some() || overflow.is_some() {
                self.insert_flags(carry, overflow);
            }
            if !biased(dst) {
                self.emit(mov(W, source, X16));
            }
        }
        self.commit(dst);
        self.state = State {
            sense: Sense::Addition,
            carry: Carry::Host,
            derivable: None,
            recipe,
        };
    }

    /// `imul dst, source[, imm]`: CF and OF report a product that does not
    /// fit; SF, ZF, AF and PF stay, as in the interpreter.
    fn imul(&mut self, dst: u8, source: Src, imm: Option<u32>) {
        let value = match source {
            Src::Reg(index) => host(index),
            Src::Mem(mem) => {
                let addr = self.address(mem);
                self.load(addr, X6);
                X6
            }
            Src::Imm(_) => unreachable!(),
        };
        self.clobber(Operand::Guest(dst));
        let (left, right) = match imm {
            Some(imm) => (value, self.imm(imm, X17)),
            None => (host(dst), value),
        };
        self.emit(smull(X16, left, right));
        if self.out.any(FlagSet::CF.or(FlagSet::OF)) {
            self.emit_all(&[
                // Nonzero when the product differs from its sign-extended
                // low half; OR with its negation puts that in bit 63.
                sub_ext(X, X17, X16, X16, Extend::Sxtw, 0),
                sub(X, X6, Reg::ZR, X17, Lsl, 0),
                orr(X, X17, X17, X6, Lsl, 0),
            ]);
            self.insert_flags(Some((X17, 63)), Some((X17, 63)));
            self.state.sense = Sense::Addition;
        }
        self.state.carry = Carry::Host;
        self.state.derivable = None;
        if biased(dst) {
            self.commit(dst);
        } else {
            self.emit(mov(W, host(dst), X16));
        }
    }

    /// `adc` or `sbb` into a register: the host's carrying forms, with C in
    /// the sense each reads.
    fn carry(&mut self, subtract: bool, dst: u8, source: Src) {
        let (right, loaded) = match source {
            Src::Reg(index) => (host(index), None),
            Src::Imm(_) => (Reg::ZR, None),
            Src::Mem(mem) => {
                let t = self.load_temp(mem);
                (temp(t), Some(t))
            }
        };
        self.host_carry();
        self.set_sense(if subtract {
            Sense::Subtraction
        } else {
            Sense::Addition
        });
        let right = match source {
            Src::Imm(value) => self.imm(value, X17),
            _ => right,
        };
        let (left, result, guest) = (host(dst), result(dst), Operand::Guest(dst));
        // AF needs left ^ right; no site follows, so the old recipe's
        // temporaries are free.
        let mixed = self.out.any(FlagSet::AF).then(|| {
            let mixed = loaded.map_or_else(|| self.take_temp(), |t| 1 - t);
            self.emit(eor(W, temp(mixed), left, right, Lsl, 0));
            mixed
        });
        self.emit(if subtract {
            sbcs(W, result, left, right)
        } else {
            adcs(W, result, left, right)
        });
        self.commit(dst);
        self.state.carry = Carry::Host;
        self.state.derivable = None;
        self.state.recipe = match mixed {
            Some(mixed) => Recipe::Mixed(guest, Operand::Temp(mixed)),
            None => Recipe::Logic(guest),
        };
    }
}
