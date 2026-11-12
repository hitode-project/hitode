//! AArch64 emission for verified optimized computations. Values keep a single
//! location for their live interval; cold records read those same locations.

mod alloc;
mod flags;

use super::ir::*;
use super::recovery::{Host, Location, Recipe, Record, X87};
use crate::asm::{Asm, Label};
use crate::encode::Shift::{Asr, Lsl, Lsr};
use crate::encode::Size::{W, X};
use crate::encode::*;
use crate::jit_map::{Mark, Part};
use crate::runtime::{self, Runtime, Stubs};
use crate::translate::{Carry, Overflow, Sense, Site};
use std::mem::offset_of;

const T: Reg = Reg::new(16);
const U: Reg = Reg::new(17);
const RT: Reg = Reg::new(29);
const ALIAS: Reg = Reg::new(28);
const COUNT: Reg = Reg::new(9);
const A: VReg = VReg::new(29);
const B: VReg = VReg::new(30);
const C: VReg = VReg::new(31);
const PARAMS: u32 = offset_of!(Runtime, opt_params) as u32;
const SPILLS: u32 = offset_of!(Runtime, opt_spills) as u32;
const FPCR: u32 = offset_of!(Runtime, opt_fpcr) as u32;
const RECORD: u32 = offset_of!(Runtime, opt_record) as u32;

pub(crate) struct Lowered {
    pub words: Vec<u32>,
    pub sites: Vec<(usize, Site)>,
    pub marks: Vec<Mark>,
    pub records: Vec<Record<Host>>,
    /// Required length of the per-thread spill area, in 16-byte cells.
    pub spill_cells: usize,
}

/// `at` and the stub offsets share a code origin. `opt_params` points to
/// 16-byte cells in entry-source order; `opt_spills` holds `spill_cells` cells.
pub(crate) fn lower(func: &Func, at: usize, stubs: &Stubs) -> Lowered {
    assert_eq!(super::verify::verify(func), Ok(()));
    let allocation = alloc::allocate(func);
    let mut asm = Asm::new();
    let blocks: Vec<_> = func.blocks.iter().map(|_| asm.label()).collect();
    let cold: Vec<_> = func.records.iter().map(|_| asm.label()).collect();
    let mut emit = Emitter {
        func,
        asm,
        blocks,
        cold,
        locations: allocation.locations,
        sites: Vec::new(),
        marks: Vec::new(),
        region: RecordId(0),
        checkpoints: Vec::new(),
        // Parallel copies occupy the tail of the spill area.
        copies: allocation.spill_cells
            - func
                .blocks
                .iter()
                .map(|b| b.params.len())
                .max()
                .unwrap_or(0),
    };
    // spec: A64-OPT-4
    emit.mark(Part::Body, func.records[0].eip);
    emit.asm.emit_all(&[
        mrs_fpcr(T),
        str(X, T, RT, FPCR),
        // Admission proves nearest rounding. Clear DN so SSE preserves NaN payloads.
        and_imm(X, T, T, !(1 << 25)),
        orr_imm(X, T, T, 1 << 24),
        msr_fpcr(T),
        msr_fpsr(Reg::ZR),
    ]);
    for (i, &param) in func.blocks[0].params.iter().enumerate() {
        emit.asm.emit(ldr(X, T, RT, PARAMS));
        emit.asm.emit(ldr_v(VSize::Q, A, T, i as u32 * 16));
        emit.put_vec(param, A);
    }
    let first = func.blocks[0].head.unwrap().record;
    emit.charge(
        i64::from(func.records[first.0 as usize].refund),
        first,
        false,
    );
    for (b, data) in func.blocks.iter().enumerate() {
        emit.asm.bind(emit.blocks[b]);
        emit.region = allocation.regions[b];
        emit.mark(Part::Body, func.records[emit.region.0 as usize].eip);
        let fused = fused_branch(data).filter(|_| {
            let End::Branch { cond, .. } = data.end else {
                unreachable!()
            };
            !func.records.iter().any(|r| r.uses().any(|v| v == cond))
                && func
                    .blocks
                    .iter()
                    .map(|block| {
                        block
                            .insts
                            .iter()
                            .flat_map(|(_, op)| op.uses())
                            .filter(|&v| v == cond)
                            .count()
                            + block
                                .end
                                .targets()
                                .flat_map(|t| &t.args)
                                .filter(|&&v| v == cond)
                                .count()
                            + usize::from(
                                matches!(block.end, End::Branch { cond: c, .. } if c == cond),
                            )
                    })
                    .sum::<usize>()
                    == 1
        });
        for (i, &(result, op)) in data.insts.iter().enumerate() {
            if fused.is_some() && i + 1 == data.insts.len() {
                continue;
            }
            emit.op(result, op);
        }
        match &data.end {
            End::Jump(target) => emit.edge(target),
            End::Branch { cond, taken, fall } => {
                let fall_label = emit.asm.label();
                if let Some((a, b, width, condition)) = fused {
                    emit.get_gp(a, T);
                    emit.get_gp(b, U);
                    if width < 4 {
                        emit.asm.emit_all(&[
                            lsl_imm(W, T, T, 32 - u32::from(width) * 8),
                            lsl_imm(W, U, U, 32 - u32::from(width) * 8),
                        ]);
                    }
                    emit.asm.emit(cmp(W, T, U, Lsl, 0));
                    emit.asm.b_cond(condition.invert(), fall_label);
                } else {
                    emit.get_gp(*cond, T);
                    emit.asm.cbz(W, false, T, fall_label);
                }
                emit.edge(taken);
                emit.asm.bind(fall_label);
                emit.edge(fall);
            }
            End::Exit(id) => emit.asm.b(emit.cold[id.0 as usize]),
            End::Unset => unreachable!(),
        }
    }
    emit.checkpoints();
    // spec: A64-OPT-6
    for (i, record) in func.records.iter().enumerate() {
        emit.asm.bind(emit.cold[i]);
        emit.mark(Part::Cold, record.eip);
        emit.imm(T, i as u64);
        emit.asm.emit(str(W, T, RT, RECORD));
        emit.asm
            .emit(b(stubs.opt_exit as i32 - (at + emit.asm.offset()) as i32));
    }
    let records = func.records.iter().map(|r| emit.record(r)).collect();
    Lowered {
        words: emit.asm.finish(),
        sites: emit.sites,
        marks: emit.marks,
        records,
        spill_cells: allocation.spill_cells,
    }
}

struct Emitter<'a> {
    func: &'a Func,
    asm: Asm,
    blocks: Vec<Label>,
    cold: Vec<Label>,
    locations: Vec<Host>,
    sites: Vec<(usize, Site)>,
    marks: Vec<Mark>,
    region: RecordId,
    copies: usize,
    checkpoints: Vec<(Label, RecordId, Label)>,
}

impl Emitter<'_> {
    fn mark(&mut self, part: Part, eip: u32) {
        let mark = Mark {
            offset: self.asm.offset() as u32,
            eip,
            part,
        };
        if self.marks.last().is_some_and(|m| m.offset == mark.offset) {
            *self.marks.last_mut().unwrap() = mark;
        } else {
            self.marks.push(mark);
        }
    }

    fn imm(&mut self, reg: Reg, bits: u64) {
        self.asm.emit(movz(X, reg, bits as u16, 0));
        for shift in [16, 32, 48] {
            let part = (bits >> shift) as u16;
            if part != 0 {
                self.asm.emit(movk(X, reg, part, shift));
            }
        }
    }

    fn size(&self, value: Value) -> Size {
        if self.func.ty(value) == Type::I64 {
            X
        } else {
            W
        }
    }

    fn get_gp(&mut self, value: Value, reg: Reg) {
        match self.locations[value.0 as usize] {
            Host::Register(n) => self.asm.emit(mov(X, reg, Reg::new(n))),
            Host::Vector(n) => self.asm.emit(fmov_from_v(X, reg, VReg::new(n))),
            Host::Constant(bits) => self.imm(reg, bits as u64),
            Host::Spill(n) => {
                self.asm.emit(ldr(X, reg, RT, SPILLS));
                self.asm.emit(ldr(X, reg, reg, n * 16));
            }
        }
    }

    fn get_vec(&mut self, value: Value, reg: VReg) {
        match self.locations[value.0 as usize] {
            Host::Vector(n) => self.asm.emit(vmov(true, reg, VReg::new(n))),
            Host::Register(n) => self.asm.emit(fmov_to_v(X, reg, Reg::new(n))),
            Host::Constant(bits) => {
                self.imm(T, bits as u64);
                self.asm.emit(fmov_to_v(X, reg, T));
                if bits >> 64 != 0 {
                    self.imm(T, (bits >> 64) as u64);
                    self.asm.emit(vins_general(3, reg, 1, T));
                }
            }
            Host::Spill(n) => {
                self.asm.emit(ldr(X, T, RT, SPILLS));
                self.asm.emit(ldr_v(VSize::Q, reg, T, n * 16));
            }
        }
    }

    fn put_gp(&mut self, value: Value) {
        match self.locations[value.0 as usize] {
            Host::Register(n) => self.asm.emit(mov(self.size(value), Reg::new(n), T)),
            Host::Vector(n) => self.asm.emit(fmov_to_v(X, VReg::new(n), T)),
            Host::Spill(n) => {
                self.asm.emit_all(&[
                    fmov_to_v(X, A, T),
                    ldr(X, U, RT, SPILLS),
                    str_v(VSize::Q, A, U, n * 16),
                ]);
            }
            Host::Constant(_) => {}
        }
    }

    fn put_vec(&mut self, value: Value, reg: VReg) {
        match self.locations[value.0 as usize] {
            Host::Register(n) => self
                .asm
                .emit(fmov_from_v(self.size(value), Reg::new(n), reg)),
            Host::Vector(n) => self.asm.emit(vmov(true, VReg::new(n), reg)),
            Host::Spill(n) => {
                self.asm.emit(ldr(X, T, RT, SPILLS));
                self.asm.emit(str_v(VSize::Q, reg, T, n * 16));
            }
            Host::Constant(_) => {}
        }
    }

    fn edge(&mut self, target: &Target) {
        // Snapshot all arguments before writing any parameter (including cycles).
        for (i, &arg) in target.args.iter().enumerate() {
            self.get_vec(arg, A);
            self.asm.emit(ldr(X, T, RT, SPILLS));
            self.asm
                .emit(str_v(VSize::Q, A, T, (self.copies + i) as u32 * 16));
        }
        for (i, &param) in self.func.blocks[target.block.0 as usize]
            .params
            .iter()
            .enumerate()
        {
            self.asm.emit(ldr(X, T, RT, SPILLS));
            self.asm
                .emit(ldr_v(VSize::Q, A, T, (self.copies + i) as u32 * 16));
            self.put_vec(param, A);
        }
        let record = self.func.blocks[target.block.0 as usize]
            .head
            .map(|h| h.record);
        if target.charge != 0 {
            self.charge(
                i64::from(target.charge),
                record.unwrap_or(self.region),
                self.func.head_exit == Some(target.block),
            );
        }
        self.asm.b(self.blocks[target.block.0 as usize]);
    }

    fn record(&self, record: &Record<Value>) -> Record<Host> {
        let loc = |location: Location<Value>| match location {
            Location::Constant(bits) => Location::Constant(bits),
            Location::At(v) => Location::At(self.locations[v.0 as usize]),
            Location::Recipe(Recipe::Eflags(v)) => {
                Location::Recipe(Recipe::Eflags(self.locations[v.0 as usize]))
            }
            Location::Recipe(Recipe::C1 { op, a, b }) => Location::Recipe(Recipe::C1 {
                op,
                a: self.locations[a.0 as usize],
                b: self.locations[b.0 as usize],
            }),
        };
        Record {
            eip: record.eip,
            refund: record.refund,
            regs: record.regs.map(|l| l.map(loc)),
            eflags: record.eflags.map(loc),
            x87: X87 {
                top: record.x87.top,
                valid: record.x87.valid,
                control: record.x87.control,
                stack: record.x87.stack.map(|l| l.map(loc)),
                status: record.x87.status,
                c1: record.x87.c1.map(loc),
                ip: record.x87.ip.map(loc),
                opcode: record.x87.opcode.map(loc),
            },
            xmm: record.xmm.map(|l| l.map(loc)),
            frame: record
                .frame
                .iter()
                .map(|&(o, w, l)| (o, w, loc(l)))
                .collect(),
        }
    }

    fn fail(&self) -> Label {
        self.cold[self.region.0 as usize]
    }

    fn charge(&mut self, charge: i64, record: RecordId, head_exit: bool) {
        self.imm(T, charge.unsigned_abs());
        self.asm.emit(if charge >= 0 {
            sub(X, COUNT, COUNT, T, Lsl, 0)
        } else {
            add(X, COUNT, COUNT, T, Lsl, 0)
        });
        if head_exit {
            let resume = self.asm.label();
            self.asm.emit(ldr(W, T, RT, runtime::HEAD_EXIT));
            self.asm.cbz(W, false, T, resume);
            self.asm.emit_all(&[
                sub_imm(W, T, T, 1, false),
                str(W, T, RT, runtime::HEAD_EXIT),
            ]);
            self.asm.cbz(W, false, T, self.cold[record.0 as usize]);
            self.asm.bind(resume);
        }
        if charge > 0 {
            let checkpoint = self.asm.label();
            let resume = self.asm.label();
            self.asm.emit(ubfx(X, T, COUNT, 63, 1));
            self.asm.cbz(W, true, T, checkpoint);
            self.asm.bind(resume);
            self.checkpoints.push((checkpoint, record, resume));
        }
    }

    fn checkpoints(&mut self) {
        for (label, record, resume) in std::mem::take(&mut self.checkpoints) {
            self.asm.bind(label);
            self.mark(Part::Cold, self.func.records[record.0 as usize].eip);
            let leave = self.cold[record.0 as usize];
            let recalled = if self.func.head_exit.is_some() {
                self.asm.label()
            } else {
                leave
            };
            let refill = self.asm.label();
            for (pointer, mask) in [
                (runtime::RECALL, runtime::RECALL_MASK),
                (
                    runtime::COMPUTATION_PENDING,
                    runtime::COMPUTATION_PENDING_MASK,
                ),
            ] {
                self.asm.emit_all(&[
                    ldr(X, T, RT, pointer),
                    ldr(W, T, T, 0),
                    ldr(W, U, RT, mask),
                    and(W, T, T, U, Lsl, 0),
                ]);
                self.asm.cbz(W, true, T, recalled);
            }
            if self.func.head_exit.is_some() {
                self.asm.b(refill);
                self.asm.bind(recalled);
                self.asm.emit_all(&[
                    movz(W, T, 1, 0),
                    str(W, T, RT, runtime::HEAD_EXIT),
                    ldr(W, T, RT, runtime::DEFERRED),
                    add_imm(W, T, T, 1, false),
                    str(W, T, RT, runtime::DEFERRED),
                    and_imm(W, T, T, u64::from(runtime::RECALL_GRACE)),
                ]);
                self.asm.cbz(W, true, T, leave);
            }
            self.asm.bind(refill);
            let refund = self.func.records[record.0 as usize].refund;
            self.imm(T, u64::from(runtime::COMPUTATION_SLICE - refund));
            self.asm.emit_all(&[
                sub(X, T, T, COUNT, Lsl, 0),
                ldr(X, U, RT, runtime::COMPLETED),
                add(X, T, T, U, Lsl, 0),
                str(X, T, RT, runtime::COMPLETED),
            ]);
            self.imm(COUNT, u64::from(runtime::COMPUTATION_SLICE - refund));
            self.asm.b(resume);
        }
    }

    fn site(&mut self) {
        let record = &self.func.records[self.region.0 as usize];
        self.sites.push((
            self.asm.offset(),
            Site {
                eip: record.eip,
                refund: record.refund,
                sense: Sense::Subtraction,
                carry: Carry::Host,
                recipe: crate::translate::Recipe::Canonical,
                nz: None,
                overflow: Overflow::Host,
                checked: false,
                simd_dirty: 0,
                resume: 0,
                opt_record: Some(self.region.0),
            },
        ));
    }

    fn op(&mut self, result: Option<Value>, op: Op) {
        let out = result.unwrap_or(Value(0));
        match op {
            Op::Constant(..) => {}
            Op::Int(op, a, b) => {
                self.get_gp(a, T);
                self.get_gp(b, U);
                let size = self.size(a);
                let word = match op {
                    IntOp::Add => add(size, T, T, U, Lsl, 0),
                    IntOp::Sub => sub(size, T, T, U, Lsl, 0),
                    IntOp::And => and(size, T, T, U, Lsl, 0),
                    IntOp::Or => orr(size, T, T, U, Lsl, 0),
                    IntOp::Xor => eor(size, T, T, U, Lsl, 0),
                    IntOp::Shl => shiftv(size, Lsl, T, T, U),
                    IntOp::Shr => shiftv(size, Lsr, T, T, U),
                    IntOp::Sar => shiftv(size, Asr, T, T, U),
                    IntOp::Mul => mul(size, T, T, U),
                    IntOp::MulHighUnsigned if size == X => umulh(T, T, U),
                    IntOp::MulHighSigned if size == X => {
                        // SMULH has the same fields as UMULH, with U clear.
                        umulh(T, T, U) & !(1 << 23)
                    }
                    IntOp::MulHighSigned | IntOp::MulHighUnsigned => {
                        self.asm.emit(if op == IntOp::MulHighSigned {
                            smull(T, T, U)
                        } else {
                            umull(T, T, U)
                        });
                        lsr_imm(X, T, T, 32)
                    }
                };
                self.asm.emit(word);
                self.put_gp(out);
            }
            Op::IntUnary(op, a) => {
                self.get_gp(a, T);
                let size = self.size(a);
                self.asm.emit(match op {
                    IntUnary::Not => mvn(size, T, T),
                    IntUnary::Neg => sub(size, T, Reg::ZR, T, Lsl, 0),
                    IntUnary::ZeroExtend8 => ubfx(W, T, T, 0, 8),
                    IntUnary::ZeroExtend16 => ubfx(W, T, T, 0, 16),
                    IntUnary::SignExtend8 => sbfx(W, T, T, 0, 8),
                    IntUnary::SignExtend16 => sbfx(W, T, T, 0, 16),
                    IntUnary::High32 => lsr_imm(X, T, T, 32),
                    IntUnary::Low32 | IntUnary::Widen => mov(W, T, T),
                });
                self.put_gp(out);
            }
            Op::Div {
                signed,
                remainder,
                dividend,
                divisor,
                proved,
            } => {
                self.get_gp(dividend, T);
                self.get_gp(divisor, U);
                if !proved {
                    self.asm.cbz(W, false, U, self.fail());
                }
                self.asm.emit(if signed {
                    sbfx(X, U, U, 0, 32)
                } else {
                    mov(W, U, U)
                });
                self.asm.emit_all(&[
                    fmov_to_v(X, B, T),
                    fmov_to_v(X, C, U),
                    if signed {
                        sdiv(X, T, T, U)
                    } else {
                        udiv(X, T, T, U)
                    },
                ]);
                if !proved {
                    self.asm.emit_all(&[
                        if signed {
                            sbfx(X, U, T, 0, 32)
                        } else {
                            mov(W, U, T)
                        },
                        cmp(X, T, U, Lsl, 0),
                    ]);
                    self.asm.b_cond(Cond::Ne, self.fail());
                }
                if remainder {
                    self.asm.emit_all(&[
                        fmov_from_v(X, U, C),
                        mul(X, T, T, U),
                        fmov_from_v(X, U, B),
                        sub(W, T, U, T, Lsl, 0),
                    ]);
                } else {
                    self.asm.emit(mov(W, T, T));
                }
                self.put_gp(out);
            }
            Op::Flags {
                kind,
                width,
                a,
                b,
                prior,
            } => self.flags(out, kind, width, a, b, prior),
            Op::Test { flags, cond } => {
                self.test(flags, cond);
                self.put_gp(out);
            }
            Op::Select { cond, a, b } => {
                self.get_vec(a, A);
                self.get_vec(b, B);
                self.get_gp(cond, T);
                let done = self.asm.label();
                self.asm.cbz(W, true, T, done);
                self.asm.emit(vmov(true, A, B));
                self.asm.bind(done);
                self.put_vec(out, A);
            }
            Op::Bits { to, value } => {
                self.get_vec(value, A);
                if to != Type::V128 {
                    self.asm
                        .emit(fmov_from_v(if to == Type::I32 { W } else { X }, T, A));
                    self.asm.emit(fmov_to_v(X, A, T));
                }
                self.put_vec(out, A);
            }
            Op::Fp(op, a, b) => {
                self.get_vec(a, A);
                self.get_vec(b, B);
                self.asm.emit(dfloat(
                    match op {
                        FpOp::Add => Float::Fadd,
                        FpOp::Sub => Float::Fsub,
                        FpOp::Mul => Float::Fmul,
                        FpOp::Div => Float::Fdiv,
                    },
                    A,
                    A,
                    B,
                ));
                self.put_vec(out, A);
            }
            Op::FpUnary(op, a) => {
                self.get_vec(a, A);
                self.asm.emit(unary(
                    match op {
                        FpUnary::Neg => Unary::Fneg,
                        FpUnary::Abs => Unary::Fabs,
                        FpUnary::RoundZero => Unary::Frintz,
                        FpUnary::RoundDown => Unary::Frintm,
                    },
                    A,
                    A,
                ));
                self.put_vec(out, A);
            }
            Op::FromInt(a) => {
                self.get_gp(a, T);
                self.asm.emit_all(&[
                    mrs_fpsr(U),
                    and_imm(W, U, U, !0x10u32 as u64),
                    msr_fpsr(U),
                    scvtf(self.size(a), A, T),
                    mrs_fpsr(T),
                ]);
                let ok = self.asm.label();
                self.asm.tbz(false, T, 4, ok);
                self.asm.b(self.fail());
                self.asm.bind(ok);
                self.put_vec(out, A);
            }
            Op::ToInt { value, wide } => {
                self.get_vec(value, A);
                self.asm
                    .emit(fcvt_int(if wide { X } else { W }, true, T, A));
                self.put_gp(out);
            }
            Op::Vec(op, a, b) => self.vector(out, op, a, b),
            Op::Load { ty, width, addr } => {
                self.get_gp(addr, U);
                self.site();
                let size = mem_size(width);
                self.asm.emit(vldr_reg(size, A, ALIAS, U));
                if ty == Type::I32 || ty == Type::I64 {
                    self.asm
                        .emit(fmov_from_v(if ty == Type::I32 { W } else { X }, T, A));
                    self.put_gp(out);
                } else {
                    self.put_vec(out, A);
                }
            }
            Op::Store {
                width,
                addr,
                value,
                after,
            } => {
                self.get_vec(value, A);
                self.get_gp(addr, U);
                self.site();
                self.asm.emit(vstr_reg(mem_size(width), A, ALIAS, U));
                self.region = after;
                self.mark(Part::Body, self.func.records[after.0 as usize].eip);
            }
            Op::X87Load { bits, denormal } => {
                self.get_gp(bits, T);
                self.asm.emit(and_imm(X, T, T, 0x7fff_ffff_ffff_ffff));
                self.imm(U, 0x7ff0_0000_0000_0000);
                self.asm.emit(cmp(X, T, U, Lsl, 0));
                self.asm.b_cond(Cond::Hi, self.fail());
                if denormal {
                    let zero = self.asm.label();
                    self.asm.cbz(X, false, T, zero);
                    self.asm.emit(lsr_imm(X, T, T, 52));
                    self.asm.cbz(X, false, T, self.fail());
                    self.asm.bind(zero);
                }
                self.get_vec(bits, A);
                self.put_vec(out, A);
            }
            Op::Validate => {
                // spec: A64-OPT-4
                self.asm.emit(mrs_fpsr(T));
                self.asm.emit(and_imm(W, T, T, 0xffff_ff8f));
                self.asm.cbz(W, true, T, self.fail());
            }
        }
    }

    fn vector(&mut self, out: Value, op: VecOp, a: Value, b: Value) {
        self.get_vec(a, A);
        self.get_vec(b, B);
        match op {
            VecOp::And | VecOp::AndNot | VecOp::Or | VecOp::Xor => {
                self.asm.emit(match op {
                    VecOp::And => vlogic(Logic::And, true, A, A, B),
                    VecOp::AndNot => vlogic(Logic::Bic, true, A, B, A),
                    VecOp::Or => vlogic(Logic::Orr, true, A, A, B),
                    _ => vlogic(Logic::Eor, true, A, A, B),
                });
            }
            VecOp::SubI32 => self.asm.emit(vsame(Same::Sub, Arr::S4, A, A, B)),
            VecOp::LessF64 => self.asm.emit(vcompare(Float::Fcmgt, false, true, A, B, A)),
            VecOp::ShlI64 | VecOp::ShrI64 => {
                self.asm
                    .emit_all(&[fmov_from_v(X, T, B), cmp_imm(X, T, 63, false)]);
                let zero = self.asm.label();
                let done = self.asm.label();
                self.asm.b_cond(Cond::Hi, zero);
                if op == VecOp::ShrI64 {
                    self.asm.emit(sub(X, T, Reg::ZR, T, Lsl, 0));
                }
                self.asm
                    .emit_all(&[vdup(Arr::D2, B, T), vsame(Same::Ushl, Arr::D2, A, A, B)]);
                self.asm.b(done);
                self.asm.bind(zero);
                self.asm.emit(vzero(true, A));
                self.asm.bind(done);
            }
        }
        self.put_vec(out, A);
    }
}

fn mem_size(width: u8) -> VMem {
    match width {
        1 => VMem::B,
        2 => VMem::H,
        4 => VMem::S,
        8 => VMem::D,
        16 => VMem::Q,
        _ => unreachable!(),
    }
}

/// Fuse the adjacent compare/test/branch when the condition is in NZCV.
fn fused_branch(data: &BlockData) -> Option<(Value, Value, u8, Cond)> {
    use iced_x86::ConditionCode as CC;
    let End::Branch { cond, .. } = data.end else {
        return None;
    };
    let [
        (
            Some(flags),
            Op::Flags {
                kind: FlagKind::Sub,
                width,
                a,
                b,
                ..
            },
        ),
        (
            Some(test),
            Op::Test {
                flags: used,
                cond: cc,
            },
        ),
    ] = data.insts.get(data.insts.len().checked_sub(2)?..)?
    else {
        return None;
    };
    if test != &cond || flags != used {
        return None;
    }
    let condition = match cc {
        CC::o => Cond::Vs,
        CC::no => Cond::Vc,
        CC::b => Cond::Lo,
        CC::ae => Cond::Hs,
        CC::e => Cond::Eq,
        CC::ne => Cond::Ne,
        CC::be => Cond::Ls,
        CC::a => Cond::Hi,
        CC::s => Cond::Mi,
        CC::ns => Cond::Pl,
        CC::l => Cond::Lt,
        CC::ge => Cond::Ge,
        CC::le => Cond::Le,
        CC::g => Cond::Gt,
        _ => return None,
    };
    Some((*a, *b, *width, condition))
}
