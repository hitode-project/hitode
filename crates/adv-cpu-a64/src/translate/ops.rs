//! Each guest instruction's host code, with the flag work liveness allows.

use crate::encode::Extend::{self, Uxtw};
use crate::encode::Index::{Post, Pre};
use crate::encode::Shift::{Asr, Lsl, Lsr};
use crate::encode::Size::{W, X};
use crate::encode::*;

use super::access::{Addr, Dir};
use super::decode::{Alu, ESP, Mem, Narrow8or16, Op, Part, ShiftKind, Src};
use super::flags::{Carry, FlagSet, Operand, Recipe, Sense};
use super::trace::{Inst, Mode};
use super::{
    Cold, RUNTIME, SHADOW, State, Translator, WINDOW, X0, X1, X3, X4, X5, X6, X9, X10, X11, X13,
    X14, X15, X16, X17, biased, host, temp,
};
use crate::runtime::{self, Exit};
use adv_cpu_core::IndirectBranchKind;
use adv_cpu_core::memory::WRITE;

/// The stack pointer, which register 31 names in load and store pairs.
const SP: Reg = Reg::new(31);

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
        if let Some((mem, width, helper)) = inst.locked {
            self.locked(inst, mem, width, helper);
            return;
        }
        match inst.op {
            Op::Nop | Op::Jmp(_) => {}
            Op::Helper(index) => self.helper(index),
            Op::Syscall => self.host_call(inst, inst.call.unwrap_or(u32::MAX)),
            Op::Timestamp => self.timestamp(),
            Op::PushFlags(width) => {
                self.read_flags();
                self.emit(and_imm(W, X6, X6, !0x30000u32 as u64));
                if width == 4 {
                    self.push(X6);
                } else {
                    self.push_part(Part::Reg(0), width, true);
                }
            }
            Op::PopFlags(width, helper) => self.pop_flags(width, helper),
            Op::Stack(kind, part, width, helper) => self.stack(kind, part, width, helper),
            Op::Lahf => {
                self.read_flags();
                self.emit(orr_imm(W, X6, X6, 2));
                self.narrow_write(Part::Reg(4), 1, Reg::ZR, X6);
            }
            Op::PopMemory(mut mem) => {
                self.prepare(Operand::Guest(ESP));
                self.load(Addr::Biased(host(ESP), 0), X6);
                if mem.base == Some(ESP) {
                    mem.disp = mem.disp.wrapping_add(4);
                }
                let addr = self.address(mem);
                self.store(addr, X6);
                self.emit(add_imm(W, X16, host(ESP), 4, false));
                self.emit(add_ext(X, host(ESP), WINDOW, X16, Uxtw, 0));
                self.forget(Operand::Guest(ESP));
            }
            Op::Simd(simd) => self.simd(simd),
            Op::Strings(strings) => self.strings(strings),
            Op::Parts(parts) => self.parts(parts),
            Op::Integer(integer) => self.integer(integer),
            Op::X87(x87) => self.x87(x87),
            Op::Fild64(mem, helper, opcode) => self.fild64(mem, helper, opcode),
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
            Op::Narrow(alu, width, dst, src) => self.narrow_op(alu, width, dst, src, false),
            Op::NarrowStep(increment, width, dst) => {
                let alu = if increment { Alu::Add } else { Alu::Sub };
                self.narrow_op(Some(alu), width, dst, Part::Imm(1), true);
            }
            Op::Neg(index) => {
                let guest = Operand::Guest(index);
                self.emit(subs(W, result(index), Reg::ZR, host(index), Lsl, 0));
                self.commit(index);
                self.state = State {
                    nz: None,
                    overflow: super::Overflow::Host,
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
                if index == ESP {
                    self.load(Addr::Biased(host(ESP), 0), X16);
                } else {
                    self.load(Addr::Pop, result(index));
                }
                self.forget(esp);
                self.forget(guest);
                self.commit(index);
            }
            Op::Leave => {
                self.prepare(Operand::Guest(ESP));
                self.prepare(Operand::Guest(5));
                self.load(Addr::Biased(host(5), 0), X6);
                self.emit(add_imm(W, X16, host(5), 4, false));
                self.emit(add_ext(X, host(ESP), WINDOW, X16, Uxtw, 0));
                self.emit(add_ext(X, host(5), WINDOW, X6, Uxtw, 0));
                self.forget(Operand::Guest(ESP));
                self.forget(Operand::Guest(5));
            }
            Op::Call(target) => {
                self.mov_imm(X6, inst.next_pc);
                self.push(X6);
                if inst.mode == Mode::Real {
                    self.real_call(Some(target));
                }
            }
            Op::Jcc(cc, _) => self.branch(cc, inst.taken.expect("jcc edges are resolved")),
            Op::Body(index) => self.body(inst, self.trace.bodies[usize::from(index)]),
            Op::Ret(release) => self.ret(inst, release),
            Op::JmpIndirect(source) => {
                if !self.table_jump(source) {
                    self.target_to_w16(source);
                    self.settle();
                    self.probe(false, IndirectBranchKind::Jump);
                }
            }
            Op::CallIndirect(source) => {
                self.target_to_w16(source);
                self.mov_imm(X6, inst.next_pc);
                self.push(X6);
                if inst.mode == Mode::Real {
                    self.real_call(None);
                } else {
                    self.settle();
                    self.probe(false, IndirectBranchKind::Call);
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
                if biased(dst) {
                    self.emit(add(X, host(dst), WINDOW, host(dst), Lsl, 0));
                }
            }
            Op::Shift(kind, Src::Reg(dst), Some(count)) => self.shift(kind, dst, count),
            Op::Shift(kind, dst, None) => self.shift_cl(kind, dst),
            Op::Shift(_, _, Some(_)) => unreachable!(),
            Op::Imul(dst, source, imm) => self.imul(dst, source, imm),
            Op::Carry(subtract, dst, source) => self.carry(subtract, dst, source),
        }
    }

    fn locked(&mut self, mut inst: Inst, mem: Mem, width: u8, helper: u8) {
        self.flush_simd();
        self.vector = super::simd::Vector::default();
        self.canonicalize();
        *self.uses_permissions = true;
        let (slow, done) = (self.asm.label(), self.asm.label());
        let mut address = self.effective(mem);
        if let Op::Integer(super::integer::Integer::Bit(_, _, index @ Part::Reg(_), width)) =
            inst.op
        {
            self.narrow_read(index, width, Reg::ZR, X6);
            self.emit_all(&[
                sbfx(W, X6, X6, 0, 8 * u32::from(width)),
                asr_imm(W, X6, X6, if width == 2 { 4 } else { 5 }),
                add(W, X16, address, X6, Lsl, if width == 2 { 1 } else { 2 }),
            ]);
            address = X16;
        }
        self.locked_page(address, slow);
        if width > 1 {
            self.emit(add_imm(W, X5, address, u32::from(width) - 1, false));
            self.locked_page(X5, slow);
        }
        inst.locked = None;
        self.op(inst);
        self.canonicalize();
        self.asm.b(done);
        self.asm.bind(slow);
        self.state = State::CANONICAL;
        self.helper(helper);
        self.asm.bind(done);
    }

    /// Raw SHARED is authoritative even when a shared allocation is window-visible.
    fn locked_page(&mut self, address: Reg, slow: crate::asm::Label) {
        self.emit(lsr_imm(W, X4, address, 12));
        let (first, count) = self.trace.memory_pages;
        if first != 0 || count != 1 << 20 {
            if first != 0 {
                self.mov_imm(X3, first);
                self.emit(sub(W, X3, X4, X3, Lsl, 0));
            } else {
                self.emit(mov(W, X3, X4));
            }
            self.mov_imm(super::X2, count);
            self.emit(sub(X, X3, X3, super::X2, Lsl, 0));
            self.asm.tbz(false, X3, 63, slow);
        }
        self.emit_all(&[
            ldr(X, X3, RUNTIME, runtime::ALIAS_CLASSES),
            ldr_narrow(Narrow::Byte, X4, X3, X4),
        ]);
        self.asm.tbz(true, X4, 3, slow);
    }

    /// Materialize the arithmetic flags and DF over the other EFLAGS bits.
    fn read_flags(&mut self) {
        self.canonicalize();
        self.emit_all(&[
            mrs_nzcv(X13),
            ldr(X, X5, RUNTIME, runtime::CPU_STATE),
            ldr(W, X6, X5, runtime::EFLAGS),
            movz(W, X14, runtime::MAPPED_FLAGS as u16, 0),
            bic(W, X6, X6, X14, Lsl, 0),
            ubfx(W, X14, X13, 30, 2),
            orr(W, X6, X6, X14, Lsl, 6),
            ubfx(W, X14, X13, 29, 1),
            eor_imm(W, X14, X14, 1),
            orr(W, X6, X6, X14, Lsl, 0),
            ubfx(W, X14, X13, 28, 1),
            orr(W, X6, X6, X14, Lsl, 11),
            and_imm(W, X14, X11, 0x10),
            orr(W, X6, X6, X14, Lsl, 0),
            eor(W, X14, X10, X10, Lsr, 4),
            eor(W, X14, X14, X14, Lsr, 2),
            eor(W, X14, X14, X14, Lsr, 1),
            and_imm(W, X14, X14, 1),
            eor_imm(W, X14, X14, 1),
            orr(W, X6, X6, X14, Lsl, 2),
            orr(W, X6, X6, super::X12, Lsl, 10),
        ]);
    }

    fn pop_flags(&mut self, width: u8, helper: u8) {
        self.flush_simd();
        self.vector = super::simd::Vector::default();
        self.canonicalize();
        self.access(Addr::Window(host(ESP)), width, Dir::Load, X6);
        let (slow, done) = (self.asm.label(), self.asm.label());
        self.asm.tbz(true, X6, 8, slow);
        self.prepare(Operand::Guest(ESP));
        self.emit_all(&[
            add_imm(W, X16, host(ESP), u32::from(width), false),
            add_ext(X, host(ESP), WINDOW, X16, Uxtw, 0),
            ldr(X, X5, RUNTIME, runtime::CPU_STATE),
            ldr(W, X13, X5, runtime::EFLAGS),
        ]);
        self.mov_imm(X14, 0x244dd5 & if width == 2 { 0xffff } else { u32::MAX });
        self.emit_all(&[
            bic(W, X13, X13, X14, Lsl, 0),
            and(W, X6, X6, X14, Lsl, 0),
            orr(W, X6, X6, X13, Lsl, 0),
            orr_imm(W, X6, X6, 2),
            str(W, X6, X5, runtime::EFLAGS),
            and_imm(W, X13, X6, 0xc0),
            ubfx(W, X14, X6, 0, 1),
            eor_imm(W, X14, X14, 1),
            orr(W, X13, X13, X14, Lsl, 5),
            ubfx(W, X14, X6, 11, 1),
            orr(W, X13, X13, X14, Lsl, 4),
            lsl_imm(W, X13, X13, 24),
            msr_nzcv(X13),
            ubfx(W, X10, X6, 2, 1),
            eor_imm(W, X10, X10, 1),
            and_imm(W, X11, X6, 0x10),
            ubfx(W, super::X12, X6, 10, 1),
        ]);
        self.forget(Operand::Guest(ESP));
        self.asm.b(done);
        self.asm.bind(slow);
        self.helper(helper);
        self.asm.bind(done);
        self.state = State::CANONICAL;
    }

    fn push_part(&mut self, part: Part, width: u8, flags: bool) {
        self.canonicalize();
        if !flags {
            let address = self.part_address(part);
            self.narrow_read(part, width, address, X6);
        }
        self.emit(sub_imm(W, X5, host(ESP), u32::from(width), false));
        self.access(Addr::Window(X5), width, Dir::Store, X6);
        self.clobber(Operand::Guest(ESP));
        self.emit(add_ext(X, host(ESP), WINDOW, X5, Uxtw, 0));
    }

    fn stack(&mut self, kind: iced_x86::Mnemonic, mut part: Part, width: u8, helper: u8) {
        use iced_x86::Mnemonic as M;
        if kind == M::Push {
            self.push_part(part, width, false);
            return;
        }
        self.canonicalize();
        if kind == M::Pop || kind == M::Leave {
            let source = if kind == M::Leave { host(5) } else { host(ESP) };
            self.access(Addr::Window(source), width, Dir::Load, X6);
            if let Part::Mem(ref mut mem) = part {
                if mem.base == Some(ESP) {
                    mem.disp = mem.disp.wrapping_add(u32::from(width));
                }
                let addr = self.effective(*mem);
                self.narrow_write(part, width, addr, X6);
            }
            self.emit(add_imm(W, X16, source, u32::from(width), false));
            self.emit(add_ext(X, host(ESP), WINDOW, X16, Uxtw, 0));
            if matches!(part, Part::Reg(_)) {
                self.narrow_write(part, width, Reg::ZR, X6);
            }
            return;
        }
        self.flush_simd();
        self.vector = super::simd::Vector::default();
        // Validate the complete stack span before any write or register change.
        *self.uses_permissions = true;
        let push = matches!(kind, M::Pushad | M::Pusha);
        let span = u32::from(width) * 8;
        let (slow, done) = (self.asm.label(), self.asm.label());
        if push {
            self.emit(sub_imm(W, X5, host(ESP), span, false));
        } else {
            self.emit(mov(W, X5, host(ESP)));
        }
        self.page_crossing(X5, span, slow);
        self.checked_base(X5, push, X6, slow);
        if push {
            for index in 0..8u8 {
                self.emit(add_imm(
                    W,
                    X16,
                    X5,
                    u32::from(7 - index) * u32::from(width),
                    false,
                ));
                self.site();
                self.emit(if width == 4 {
                    str_reg(W, host(index), X6, X16, false)
                } else {
                    str_narrow(Narrow::Half, host(index), X6, X16)
                });
            }
            self.emit(add_ext(X, host(ESP), WINDOW, X5, Uxtw, 0));
        } else {
            let indices = [7u8, 6, 5, 3, 2, 1, 0];
            let values = [X0, X1, super::X2, temp(0), temp(1), X13, X14];
            for (index, value) in indices.into_iter().zip(values) {
                self.emit(add_imm(
                    W,
                    X16,
                    X5,
                    u32::from(7 - index) * u32::from(width),
                    false,
                ));
                self.site();
                self.emit(if width == 4 {
                    ldr_reg(W, value, X6, X16, false)
                } else {
                    ldr_narrow(Narrow::Half, value, X6, X16)
                });
            }
            for (index, value) in indices.into_iter().zip(values) {
                self.narrow_write(Part::Reg(index), width, Reg::ZR, value);
            }
            self.emit(add_imm(W, X5, X5, span, false));
            self.emit(add_ext(X, host(ESP), WINDOW, X5, Uxtw, 0));
        }
        self.asm.b(done);
        self.asm.bind(slow);
        self.helper(helper);
        self.asm.bind(done);
    }

    fn timestamp(&mut self) {
        self.flush_simd();
        self.vector = super::simd::Vector::default();
        self.canonicalize();
        self.emit_all(&[
            stp(X, Pre, X9, X10, SP, -48),
            stp(X, Index::Offset, X11, super::X12, SP, 16),
            mrs_nzcv(X13),
            str(X, X13, SP, 32),
            mov(X, X0, RUNTIME),
            ldr(X, X16, RUNTIME, runtime::TIMESTAMP),
            blr(X16),
            mov(W, host(0), X0),
            lsr_imm(X, host(2), X0, 32),
            ldr(X, X13, SP, 32),
            msr_nzcv(X13),
            ldp(X, Index::Offset, X11, super::X12, SP, 16),
            ldp(X, Post, X9, X10, SP, 48),
        ]);
    }

    /// A return popping `release` argument bytes besides its address.
    fn ret(&mut self, inst: Inst, release: u16) {
        self.flush_simd();
        let esp = Operand::Guest(ESP);
        self.prepare(esp);
        self.load(Addr::Pop, X16);
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
                self.push_cold(Cold::Lookup {
                    label,
                    state: self.state,
                    refund: self.trace.unused(self.index),
                    branch: self.indirect(IndirectBranchKind::Return),
                    x87: self.x87_leaving(),
                });
            }
            _ => {
                self.settle();
                self.shadow_return();
            }
        }
    }

    /// Calls direct body `bound` (A64-CALL-2): at a call, whose arguments
    /// start at ESP and whose return address is never stored, or at the
    /// entry, returning as the function does. A body that stops leaves for
    /// the full path before anything changed: a call runs again in the
    /// interpreter, whose transfer reaches the thunk, and an entry continues
    /// at the thunk.
    fn body(&mut self, inst: Inst, bound: super::Bound) {
        use adv_cpu_core::body::{EAX, ECX, EDX, Input};
        // AAPCS64 keeps x19-x29 and d8-d15: the guest map survives; the
        // countdown, DF in x12 and the XMM cache do not.
        self.flush_simd();
        self.vector = super::simd::Vector::default();
        *self.uses_permissions = true;
        let body = bound.body;
        let base = if bound.entry { 4 } else { 0 };
        for (n, &input) in body.inputs.iter().flatten().enumerate() {
            let rd = Reg::new(1 + n as u8);
            match input {
                Input::Stack(k) => {
                    let disp = base + 4 * u32::from(k);
                    if self.checking {
                        self.emit(add_imm(W, X16, host(ESP), disp, false));
                        self.load(Addr::Window(X16), X6);
                        self.emit(mov(W, rd, X6));
                    } else {
                        self.load(Addr::Biased(host(ESP), disp as i32), rd);
                    }
                }
                Input::Const(value) => self.mov_imm(rd, value),
                Input::Eax => self.emit(mov(W, rd, host(0))),
                Input::Ecx => self.emit(mov(W, rd, host(1))),
                Input::Edx => self.emit(mov(W, rd, host(2))),
            }
        }
        let call = body.call as usize as u64;
        self.emit(stp(X, Pre, X9, super::X12, SP, -16));
        self.emit(add_imm(X, X0, RUNTIME, runtime::BODY, false));
        self.emit(movz(X, X16, call as u16, 0));
        for shift in [16, 32, 48] {
            self.emit(movk(X, X16, (call >> shift) as u16, shift));
        }
        self.emit(blr(X16));
        self.emit(ldp(X, Post, X9, super::X12, SP, 16));
        self.state = State::CANONICAL;
        self.emit(ubfx(X, X16, X1, 32, 32));
        let slow = self.asm.label();
        self.asm.cbz(W, true, X16, slow);
        let (exit, eip, link) = if bound.entry {
            (Exit::Dispatch, bound.thunk, true)
        } else {
            (Exit::Interpret, inst.pc, false)
        };
        self.push_cold(Cold::Leave {
            label: slow,
            state: State::CANONICAL,
            simd_dirty: 0,
            refund: self.trace.owed(self.index),
            exit,
            eip,
            link,
            x87: None,
        });
        if body.outputs & EAX != 0 {
            self.clobber(Operand::Guest(0));
            self.emit(mov(W, host(0), X0));
        }
        if body.outputs & EDX != 0 {
            self.clobber(Operand::Guest(2));
            self.emit(ubfx(X, host(2), X0, 32, 32));
        }
        if body.outputs & ECX != 0 {
            self.clobber(Operand::Guest(1));
            self.emit(mov(W, host(1), X1));
        }
        if bound.entry {
            self.ret(inst, body.pop);
        } else if body.pop != 0 {
            self.emit(add_imm(X, host(ESP), host(ESP), u32::from(body.pop), false));
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
    pub(super) fn settle(&mut self) {
        self.flush_simd();
        self.canonicalize();
        let refund = self.trace.unused(self.index);
        if refund != 0 {
            self.emit(add_imm(X, super::X9, super::X9, refund, false));
        }
    }

    /// Unresolved switch entries and ordinary indirect transfers share this
    /// probe; resolved switch entries use the site table (A64-CTL-6).
    /// Branches to the unit for the EIP in w16 through the indirect table,
    /// from this site's own branch (A64-CTL-6); `link` calls it. A miss records
    /// a transfer of `kind`.
    pub(super) fn probe(&mut self, link: bool, kind: IndirectBranchKind) {
        self.emit_all(&[
            ldr(X, X17, RUNTIME, runtime::TABLE),
            eor(W, X6, X16, X16, Lsr, 16),
            ubfx(W, X6, X6, 0, 16),
            add(X, X17, X17, X6, Lsl, 4),
            ldp(X, crate::encode::Index::Offset, X6, X17, X17, 0),
            sub(X, X6, X6, X16, Lsl, 0),
        ]);
        self.miss_cbnz(X6, self.stubs.lookup_slow, kind);
        self.emit(if link { blr(X17) } else { br(X17) });
    }

    /// A return to the EIP in w16 through the shadow stack (A64-CTL-4).
    fn shadow_return(&mut self) {
        self.emit_all(&[
            ldp(X, crate::encode::Index::Offset, Reg::LR, X17, SHADOW, 0),
            sub(X, X17, X17, X16, Lsl, 0),
        ]);
        self.miss_cbnz(X17, self.stubs.ret_miss, IndirectBranchKind::Return);
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
                self.push_cold(Cold::Call {
                    label: stub,
                    target,
                    point,
                });
            }
            None => self.probe(true, IndirectBranchKind::Call),
        }
        self.asm.bind(continuation);
        self.state = State::CANONICAL;
    }

    fn push(&mut self, value: Reg) {
        let esp = Operand::Guest(ESP);
        self.prepare(esp);
        self.store(Addr::Push, value);
        self.forget(esp);
    }

    pub(super) fn target_to_w16(&mut self, source: Src) {
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
        self.finish_address(mem, flat)
    }

    /// Wraps an offset to its address size, then adds the segment base.
    pub(super) fn finish_address(&mut self, mem: Mem, mut flat: Reg) -> Reg {
        if mem.address_bits == 16 {
            self.emit(ubfx(W, X16, flat, 0, 16));
            flat = X16;
        }
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

    /// The base, index and displacement before wrapping or adding a segment base.
    pub(super) fn flat(&mut self, mem: Mem) -> Reg {
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

    /// Has the interpreter run the instruction at helper `index` of the
    /// unit, with canonical flags; leaves when it asks to. Conditional callers
    /// must also flush before the fork, so their fast path publishes the prefix.
    pub(super) fn helper(&mut self, index: u8) {
        let index = self.helpers + u32::from(index);
        self.mov_imm(X1, index);
        self.owner_call(self.stubs.interpret);
    }

    /// Makes the host call through `thunk` whose INT is `inst`, with the
    /// guest synchronized as for a helper (A64-CTL-12).
    fn host_call(&mut self, inst: Inst, thunk: u32) {
        self.mov_imm(X1, inst.pc);
        let raw = thunk == u32::MAX;
        if !raw {
            self.emit(movk(X, X1, u16::try_from(thunk).expect("thunk index"), 32));
        }
        let length = inst.next_pc.wrapping_sub(inst.pc) as u16;
        self.emit(movk(X, X1, length | u16::from(raw) << 15, 48));
        self.owner_call(self.stubs.call);
    }

    /// Calls `stub` with the argument in x1 and the guest in `CpuState`.
    fn owner_call(&mut self, stub: usize) {
        self.canonicalize();
        // The owner may change any SIMD register.
        self.flush_simd();
        self.vector = super::simd::Vector::default();
        self.far_call(stub);
        self.state = State::CANONICAL;
        let leave = self.asm.label();
        self.asm.cbz(W, true, X0, leave);
        self.push_cold(Cold::Helper {
            label: leave,
            refund: self.trace.unused(self.index),
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
                self.prepare(guest);
                let dir = if signed { Dir::Signed } else { Dir::Load };
                self.access(Addr::Window(address), 1 + u8::from(half), dir, result(dst));
                self.forget(guest);
            }
        }
        self.commit(dst);
    }

    /// `inc` or `dec`, which keep CF.
    fn step(&mut self, increment: bool, index: u8) {
        let guest = Operand::Guest(index);
        self.prepare(guest);
        self.emit(if increment {
            add_imm(W, result(index), host(index), 1, false)
        } else {
            sub_imm(W, result(index), host(index), 1, false)
        });
        self.forget(guest);
        self.commit(index);
        self.state.nz = Some(guest);
        self.state.overflow = super::Overflow::Step(guest, increment);
        self.state.recipe = if increment {
            Recipe::AddResult(guest, Operand::Imm(1))
        } else {
            Recipe::SubResult(guest, Operand::Imm(1))
        };
    }

    /// `inc` or `dec` of a 32-bit memory operand: load, store the result,
    /// then the flags from the loaded value (A64-FLAG-4).
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
        self.state.nz = None;
        self.state.overflow = super::Overflow::Host;
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
    /// Stores precede the flags (A64-FLAG-4).
    pub(super) fn narrow_op(
        &mut self,
        alu: Option<Alu>,
        width: u8,
        dst: Part,
        src: Part,
        step: bool,
    ) {
        let step = step && self.keep.any(FlagSet::CF);
        if step {
            self.convert(FlagSet::CF);
        }
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
            if site && (self.state.nz == Some(t) || self.state.overflow.uses(t)) {
                self.host_nzv(FlagSet::NZV);
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
            // Not x17: writing a register part may first materialize flags
            // through it.
            let value = match right {
                Operand::Imm(value) => self.imm(value, X6),
                _ => temp(1),
            };
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
        if step {
            let carry = if alu == Alu::Add { Cond::Lo } else { Cond::Hs };
            self.emit(cset(W, X13, carry));
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
        if step {
            if self.features.flagm {
                self.emit(rmif(X13, 63, 2));
            } else {
                self.emit_all(&[mrs_nzcv(X16), bfi(X, X16, X13, 29, 1), msr_nzcv(X16)]);
            }
        }
        let logic = matches!(alu, Alu::And | Alu::Test | Alu::Or | Alu::Xor);
        self.state = State {
            nz: None,
            overflow: super::Overflow::Host,
            sense,
            carry: Carry::Host,
            derivable: logic.then_some(Carry::Clear),
            recipe,
        };
    }

    /// Zero-extends the 1-, 2- or 4-byte `part` into `rt`; a memory part is
    /// at the guest address in `address`.
    pub(super) fn narrow_read(&mut self, part: Part, width: u8, address: Reg, rt: Reg) {
        match part {
            Part::Reg(number) => {
                let (index, lsb) = part_register(number, width);
                self.emit(ubfx(W, rt, host(index), lsb, 8 * u32::from(width)));
            }
            Part::Imm(value) => self.mov_imm(rt, value),
            Part::Mem(_) => self.access(Addr::Window(address), width, Dir::Load, rt),
        }
    }

    /// Writes the low 1, 2 or 4 bytes of `value` to `part`. A register part
    /// first prepares its guest register, which may use x15 to x17, so
    /// `value` is never one of them.
    pub(super) fn narrow_write(&mut self, part: Part, width: u8, address: Reg, value: Reg) {
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
            Part::Mem(_) => self.access(Addr::Window(address), width, Dir::Store, value),
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
                    nz: None,
                    overflow: super::Overflow::Host,
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
                    nz: None,
                    overflow: super::Overflow::Host,
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
                let nzcv = self.trace.live_out[self.index].any(FlagSet::NZV);
                if nzcv {
                    self.emit(tst(W, result, result, Lsl, 0));
                }
                self.state = State {
                    nz: None,
                    overflow: super::Overflow::Host,
                    sense: Sense::Addition,
                    carry: if nzcv { Carry::Host } else { Carry::Clear },
                    derivable: nzcv.then_some(Carry::Clear),
                    recipe: Recipe::Logic(guest),
                };
                if !nzcv {
                    self.state.nz = Some(guest);
                    self.state.overflow = super::Overflow::Clear;
                }
                self.commit(dst);
            }
        }
    }

    /// `alu [mem], source`: load, compute, store, then the flags (A64-FLAG-4).
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
            nz: None,
            overflow: super::Overflow::Host,
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
        let out = self.trace.observe_out[self.index];
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
            if count == 1 {
                self.state.overflow = super::Overflow::Host;
            }
            if lazy {
                self.state.carry = Carry::Bit(guest, bit as u8);
            } else {
                self.state.sense = Sense::Addition;
                self.state.carry = Carry::Host;
            }
            self.state.derivable = None;
            return;
        }
        // The original supplies CF; the result supplies N/Z/PF. AF and
        // multi-bit OF keep their previous sources (A64-FLAG-2).
        let clear_af = self.state.recipe.clears_af();
        if clear_af {
            // PF is replaced, and AF is recoverable as zero.
            self.state.recipe = Recipe::Canonical;
        }
        self.prepare(guest);
        let recipe = if clear_af {
            Recipe::Logic(guest)
        } else {
            self.materialize();
            Recipe::Parity(guest)
        };
        self.state.carry = Carry::Host;
        self.state.nz = None;
        if count == 1 {
            self.state.overflow = super::Overflow::Host;
        }
        let t = self.take_temp();
        let original = Operand::Temp(t);
        self.clobber(original);
        self.emit(mov(W, temp(t), source));
        self.emit(match kind {
            Shl => lsl_imm(W, result, source, count),
            Shr => lsr_imm(W, result, source, count),
            _ => asr_imm(W, result, source, count),
        });
        self.commit(dst);
        self.state.carry = Carry::Bit(
            original,
            if kind == Shl { 32 - count } else { count - 1 } as u8,
        );
        self.state.derivable = None;
        self.state.recipe = recipe;
        self.state.nz = Some(guest);
        if count == 1 {
            self.state.overflow = match kind {
                Shl => super::Overflow::Xor(original, 31, 30),
                Shr => super::Overflow::Bit(original, 31),
                _ => super::Overflow::Clear,
            };
        }
    }

    /// CL masks the count to five bits; zero keeps the destination and flags.
    fn shift_cl(&mut self, kind: ShiftKind, dst: Src) {
        use ShiftKind::*;
        let ecx = host(1);
        let written = Op::Shift(kind, dst, None).effects().writes;
        if let Src::Reg(dst) = dst
            && !self.out.any(written)
        {
            let source = host(dst);
            // A variable shift takes the count modulo 32, as x86 masks it.
            self.clobber(Operand::Guest(dst));
            let result = result(dst);
            let word = match kind {
                Shl => shiftv(W, Lsl, result, source, ecx),
                Shr => shiftv(W, Lsr, result, source, ecx),
                Sar => shiftv(W, Asr, result, source, ecx),
                Rol => {
                    self.emit(sub(W, X17, Reg::ZR, ecx, Lsl, 0));
                    rorv(W, result, source, X17)
                }
                Ror => rorv(W, result, source, ecx),
            };
            self.emit(word);
            self.commit(dst);
            return;
        }
        self.canonicalize();
        let done = self.asm.label();
        let zero = matches!(dst, Src::Mem(_)).then(|| self.asm.label());
        // x7 holds the memory source and x5 its address across checked accesses.
        let source = match dst {
            Src::Reg(dst) => host(dst),
            Src::Mem(mem) => {
                *self.uses_permissions = true;
                let address = self.effective(mem);
                self.emit(mov(W, X5, address));
                self.load(Addr::Window(X5), temp(0));
                temp(0)
            }
            Src::Imm(_) => unreachable!(),
        };
        self.emit(and_imm(W, X16, ecx, 31));
        self.asm.cbz(W, false, X16, zero.unwrap_or(done));
        // The result in w6; CF and OF as bits of registers when live.
        let flags = self.out.any(written);
        let (carry, overflow) = if !flags {
            let word = match kind {
                Shl => shiftv(W, Lsl, X6, source, X16),
                Shr => shiftv(W, Lsr, X6, source, X16),
                Sar => shiftv(W, Asr, X6, source, X16),
                Rol => {
                    self.emit(sub(W, X17, Reg::ZR, X16, Lsl, 0));
                    rorv(W, X6, source, X17)
                }
                Ror => rorv(W, X6, source, X16),
            };
            self.emit(word);
            ((Reg::ZR, 0), (Reg::ZR, 0))
        } else {
            match kind {
                Shl => {
                    self.emit_all(&[
                        shiftv(W, Lsl, X6, source, X16),
                        sub(W, X17, Reg::ZR, X16, Lsl, 0),
                        shiftv(W, Lsr, X17, source, X17),
                        eor(W, X15, X17, X6, Lsr, 31),
                    ]);
                    ((X17, 0), (X15, 0))
                }
                Shr | Sar => {
                    let shift = if kind == Shr { Lsr } else { Asr };
                    self.emit_all(&[
                        shiftv(W, shift, X6, source, X16),
                        sub_imm(W, X17, X16, 1, false),
                        shiftv(W, Lsr, X17, source, X17),
                    ]);
                    let overflow = if kind == Shr {
                        (source, 31)
                    } else {
                        (Reg::ZR, 0)
                    };
                    ((X17, 0), overflow)
                }
                Rol => {
                    self.emit_all(&[
                        sub(W, X17, Reg::ZR, X16, Lsl, 0),
                        rorv(W, X6, source, X17),
                        eor(W, X15, X6, X6, Lsr, 31),
                    ]);
                    ((X6, 0), (X15, 0))
                }
                Ror => {
                    self.emit_all(&[rorv(W, X6, source, X16), eor(W, X15, X6, X6, Lsl, 1)]);
                    ((X6, 31), (X15, 31))
                }
            }
        };
        // Store before changing any guest flag (A64-FLAG-4).
        if matches!(dst, Src::Mem(_)) {
            self.store(Addr::Window(X5), X6);
        }
        if flags {
            // OF changes only for a count of 1; C holds CF in subtraction sense.
            let old = temp(1);
            self.emit_all(&[
                mrs_nzcv(old),
                ubfx(W, X14, overflow.0, overflow.1, 1),
                ubfx(W, X13, old, 28, 1),
                cmp_imm(W, X16, 1, false),
                csel(W, X14, X14, X13, Cond::Eq),
                bfi(X, old, X14, 28, 1),
                ubfx(W, X14, carry.0, carry.1, 1),
                eor_imm(W, X14, X14, 1),
                bfi(X, old, X14, 29, 1),
            ]);
            if !matches!(kind, Rol | Ror) {
                self.emit_all(&[
                    lsr_imm(W, X14, X6, 31),
                    bfi(X, old, X14, 31, 1),
                    cmp_imm(W, X6, 0, false),
                    cset(W, X14, Cond::Eq),
                    bfi(X, old, X14, 30, 1),
                    mov(W, X10, X6),
                ]);
            }
            self.emit(msr_nzcv(old));
        }
        if let Src::Reg(dst) = dst {
            self.emit(mov(W, result(dst), X6));
            self.commit(dst);
        }
        if let Some(zero) = zero {
            self.asm.b(done);
            self.asm.bind(zero);
            let slow = self.asm.label();
            // Zero counts still require WRITE, but must not perform a store.
            // The preceding load established that both page indices are valid.
            self.emit_all(&[
                ldr(X, X3, RUNTIME, runtime::ALIAS_CLASSES),
                lsr_imm(W, X4, X5, 12),
                ldr_narrow(Narrow::Byte, X4, X3, X4),
            ]);
            self.asm.tbz(false, X4, WRITE.trailing_zeros(), slow);
            self.emit_all(&[
                add_imm(W, X4, X5, 3, false),
                lsr_imm(W, X4, X4, 12),
                ldr_narrow(Narrow::Byte, X4, X3, X4),
            ]);
            self.asm.tbz(false, X4, WRITE.trailing_zeros(), slow);
            self.push_cold(Cold::Leave {
                label: slow,
                state: State::CANONICAL,
                simd_dirty: self.vector.dirty,
                refund: self.trace.owed(self.index),
                exit: super::Exit::Interpret,
                eip: self.trace.insts[self.index].pc,
                link: false,
                x87: self.x87_leaving(),
            });
        }
        self.asm.bind(done);
        self.state = State::CANONICAL;
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
            self.state.overflow = super::Overflow::Host;
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
        self.state.nz = None;
        self.state.overflow = super::Overflow::Host;
        self.state.recipe = match mixed {
            Some(mixed) => Recipe::Mixed(guest, Operand::Temp(mixed)),
            None => Recipe::Logic(guest),
        };
    }
}
