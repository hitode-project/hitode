//! Register-allocated AArch64 computations with cold recovery recipes.
mod alloc;
mod flags;

use super::ir::*;
use super::recovery::{FlagRecipe, Host, Record};
use crate::asm::{Asm, Label};
use crate::encode::Shift::{Asr, Lsl, Lsr};
use crate::encode::Size::{W, X};
use crate::encode::*;
use crate::jit_map::{Mark, Part};
use crate::runtime::{self, Runtime};
use crate::translate::{Carry, Overflow, Sense, Site};
use std::mem::offset_of;

const T: Reg = Reg::new(16);
const U: Reg = Reg::new(17);
const RT: Reg = Reg::new(29);
const SP: Reg = Reg::new(31);
const ALIAS: Reg = Reg::new(28);
const COUNT: Reg = Reg::new(9);
const A: VReg = VReg::new(29);
const B: VReg = VReg::new(30);
const C: VReg = VReg::new(31);
const PARAMS: u32 = offset_of!(Runtime, opt_params) as u32;
const FPCR: u32 = offset_of!(Runtime, opt_fpcr) as u32;
const RECORD: u32 = offset_of!(Runtime, opt_record) as u32;

pub(crate) struct Lowered {
    pub words: Vec<u32>,
    pub sites: Vec<(usize, Site)>,
    pub marks: Vec<Mark>,
    pub records: Vec<Record<Host>>,
    pub flags: Vec<Option<FlagRecipe>>,
    /// 16-byte cells at the captured SP, discarded by `opt_exit`.
    pub spill_cells: usize,
    /// The word every record leaves through, a branch to `opt_exit` once
    /// the code is placed.
    pub exit: usize,
}

impl Lowered {
    /// Makes the exit word branch to `opt_exit` from code placed at `at`.
    pub(crate) fn place(&mut self, at: usize, opt_exit: usize) {
        self.words[self.exit] = b(opt_exit as i32 - (at + self.exit * 4) as i32);
    }
}

/// Code at any address; `Lowered::place` binds its exit. Entry cells are
/// 16 bytes.
pub(crate) fn lower(func: &Func) -> Lowered {
    assert_eq!(super::verify::verify(func), Ok(()));
    let allocation = alloc::allocate(func);
    let flags = super::recovery::lower_flags(&allocation.defs, &allocation.locations);
    let mut asm = Asm::new();
    let blocks = func.blocks.iter().map(|_| asm.label()).collect();
    let cold = func.records.iter().map(|_| asm.label()).collect();
    let mut branch_only = vec![true; func.types.len()];
    for v in func
        .blocks
        .iter()
        .flat_map(|b| {
            b.insts
                .iter()
                .flat_map(|(_, op)| op.uses())
                .chain(b.end.targets().flat_map(|t| t.args.iter().copied()))
        })
        .chain(func.records.iter().flat_map(|r| r.uses()))
    {
        branch_only[v.0 as usize] = false;
    }
    let mut e = Emitter {
        func,
        asm,
        blocks,
        cold,
        locations: allocation.locations,
        defs: allocation.defs,
        flags,
        sites: Vec::new(),
        marks: Vec::new(),
        region: RecordId(0),
        checkpoints: Vec::new(),
        edges: Vec::new(),
    };
    let first = func.blocks[0].head.unwrap().record;
    // spec: A64-OPT-4, A64-OPT-7
    e.mark(Part::Body, func.records[first.0 as usize].eip);
    e.asm.emit_all(&[
        mrs_fpcr(T),
        str(X, T, RT, FPCR),
        and_imm(X, T, T, !(1 << 25)),
        orr_imm(X, T, T, 1 << 24),
        msr_fpcr(T),
        msr_fpsr(Reg::ZR),
    ]);
    let bytes = allocation.spill_cells as u64 * 16;
    if bytes != 0 {
        if let Some((n, shift)) = add_immediate(bytes) {
            e.asm.emit(sub_imm(X, SP, SP, n, shift));
        } else {
            e.imm(T, bytes);
            e.asm.emit(sub_ext(X, SP, SP, T, Extend::Uxtx, 0));
        }
    }
    if !func.entry.is_empty() {
        e.asm.emit(ldr(X, U, RT, PARAMS));
    }
    for (i, &p) in func.blocks[0].params.iter().enumerate() {
        let loc = e.locations[p.0 as usize];
        match loc {
            Host::Register(n) => e.asm.emit(ldr(e.size(p), n, U, i as u32 * 16)),
            Host::Vector(n) => e.asm.emit(vldr(mem_type(func.ty(p)), n, U, i as u32 * 16)),
            _ => {
                e.asm.emit(vldr(mem_type(func.ty(p)), A, U, i as u32 * 16));
                e.copy(loc, Host::Vector(A), func.ty(p));
            }
        }
    }
    let refund = func.records[first.0 as usize].refund;
    e.charge(i64::from(refund), first, false);
    for (b, data) in func.blocks.iter().enumerate() {
        if e.through(Block(b as u32)).is_some() {
            continue;
        }
        e.asm.bind(e.blocks[b]);
        e.region = allocation.regions[b];
        e.mark(
            Part::Body,
            data.eip.unwrap_or(func.records[e.region.0 as usize].eip),
        );
        for &(result, op) in &data.insts {
            if matches!(op, Op::Test { .. }) && result.is_some_and(|v| branch_only[v.0 as usize]) {
                continue;
            }
            e.op(result, op);
        }
        match &data.end {
            End::Jump(t) => e.edge(t, b + 1),
            End::Branch { cond, taken, fall } => {
                let test = if let Some(Op::Test { flags, cond: cc }) = e.defs[cond.0 as usize]
                    && branch_only[cond.0 as usize]
                {
                    Ok(e.condition(e.locations[flags.0 as usize], cc))
                } else {
                    Err(e.gp(*cond, T))
                };
                // Branch straight to a target whose edge needs no code and
                // place the other edge inline, so a charged backedge costs no
                // extra taken branch.
                let bare = |e: &Emitter, t: &Target| {
                    e.moves(t).is_empty() && t.charge == 0 && e.through(t.block).is_none()
                };
                let (jump, inline, when) = if bare(&e, taken) {
                    (taken, fall, true)
                } else {
                    (fall, taken, false)
                };
                let target = if bare(&e, jump) {
                    e.blocks[jump.block.0 as usize]
                } else {
                    let label = e.asm.label();
                    e.edges.push((label, jump.clone()));
                    label
                };
                match test {
                    Ok(c) => e.asm.b_cond(if when { c } else { c.invert() }, target),
                    Err(r) => e.asm.cbz(W, when, r, target),
                }
                e.edge(inline, b + 1);
            }
            End::Exit(id) => e.asm.b(e.cold[id.0 as usize]),
            End::Unset => unreachable!(),
        }
    }
    for (label, target) in std::mem::take(&mut e.edges) {
        e.asm.bind(label);
        e.edge(&target, usize::MAX);
    }
    e.checkpoints();
    // spec: A64-OPT-6
    let exit = e.asm.label();
    for (i, record) in func.records.iter().enumerate() {
        e.asm.bind(e.cold[i]);
        e.mark(Part::Cold, record.eip);
        e.imm(T, i as u64);
        e.asm.emit(str(W, T, RT, RECORD));
        e.asm.b(exit);
    }
    e.asm.bind(exit);
    // Permanently undefined until placed.
    e.asm.emit(0);
    // A host fault leaves through its region's record like a failed check.
    for (offset, site) in &mut e.sites {
        let cold = e.cold[site.opt_record.expect("optimized site") as usize];
        site.resume = (e.asm.position(cold) as i64 - *offset as i64) as i32;
    }
    let records = func
        .records
        .iter()
        .map(|r| super::recovery::lower_record(r, &e.locations))
        .collect();
    let exit = e.asm.position(exit) / 4;
    Lowered {
        exit,
        words: e.asm.finish(),
        sites: e.sites,
        marks: e.marks,
        records,
        flags: e.flags,
        spill_cells: allocation.spill_cells,
    }
}

struct Emitter<'a> {
    func: &'a Func,
    asm: Asm,
    blocks: Vec<Label>,
    cold: Vec<Label>,
    locations: Vec<Host>,
    defs: Vec<Option<Op>>,
    flags: Vec<Option<FlagRecipe>>,
    sites: Vec<(usize, Site)>,
    marks: Vec<Mark>,
    region: RecordId,
    checkpoints: Vec<(Label, RecordId, Label)>,
    edges: Vec<(Label, Target)>,
}

impl<'a> Emitter<'a> {
    fn mark(&mut self, part: Part, eip: u32) {
        self.marks.push(Mark {
            offset: self.asm.offset() as u32,
            eip,
            part,
        });
    }
    fn imm(&mut self, reg: Reg, bits: u64) {
        let first = if bits == 0 {
            0
        } else {
            bits.trailing_zeros() / 16 * 16
        };
        self.asm.emit(movz(X, reg, (bits >> first) as u16, first));
        for shift in [0, 16, 32, 48] {
            let part = (bits >> shift) as u16;
            if shift != first && part != 0 {
                self.asm.emit(movk(X, reg, part, shift));
            }
        }
    }
    fn size(&self, v: Value) -> Size {
        if self.func.ty(v) == Type::I64 { X } else { W }
    }
    fn gp(&mut self, v: Value, scratch: Reg) -> Reg {
        self.gp_at(self.locations[v.0 as usize], scratch)
    }
    fn gp_at(&mut self, loc: Host, scratch: Reg) -> Reg {
        if let Host::Register(r) = loc {
            return r;
        }
        self.copy(Host::Register(scratch), loc, Type::I64);
        scratch
    }
    fn vr(&mut self, v: Value, scratch: VReg) -> VReg {
        self.v_at(self.locations[v.0 as usize], scratch)
    }
    fn v_at(&mut self, loc: Host, scratch: VReg) -> VReg {
        if let Host::Vector(r) = loc {
            return r;
        }
        self.copy(Host::Vector(scratch), loc, Type::V128);
        scratch
    }
    fn rd(&self, v: Value) -> Reg {
        if let Host::Register(n) = self.locations[v.0 as usize] {
            n
        } else {
            T
        }
    }
    fn vd(&self, v: Value) -> VReg {
        if let Host::Vector(n) = self.locations[v.0 as usize] {
            n
        } else {
            A
        }
    }
    fn copy(&mut self, dst: Host, src: Host, ty: Type) {
        if dst == src {
            return;
        }
        use Host::*;
        let size = scalar_size(ty);
        match (dst, src) {
            (Register(d), Register(s)) => self.asm.emit(mov(size, d, s)),
            (Vector(d), Vector(s)) => self.asm.emit(vmov(true, d, s)),
            (Register(d), Vector(s)) => self.asm.emit(fmov_from_v(size, d, s)),
            (Vector(d), Register(s)) => self.asm.emit(fmov_to_v(size, d, s)),
            (Register(d), Constant(n)) => self.imm(d, n as u64),
            (Vector(d), Constant(n)) => {
                self.imm(T, n as u64);
                self.asm.emit(fmov_to_v(X, d, T));
                if n >> 64 != 0 {
                    self.imm(T, (n >> 64) as u64);
                    self.asm.emit(vins_general(3, d, 1, T));
                }
            }
            (Register(d), Spill(n, ty)) => self.asm.emit(ldr(scalar_size(ty), d, SP, n * 16)),
            (Vector(d), Spill(n, ty)) => self.asm.emit(vldr(mem_type(ty), d, SP, n * 16)),
            (Spill(n, _), Register(s)) => self.asm.emit(str(size, s, SP, n * 16)),
            (Spill(n, _), Vector(s)) => self.asm.emit(vstr(mem_type(ty), s, SP, n * 16)),
            (Spill(..), _) => {
                let tmp = if matches!(ty, Type::F64 | Type::V128) {
                    Vector(B)
                } else {
                    Register(U)
                };
                self.copy(tmp, src, ty);
                self.copy(dst, tmp, ty);
            }
            _ => unreachable!("copies carry numeric values"),
        }
    }
    fn moves(&self, target: &Target) -> Vec<(Host, Host, Type)> {
        self.func.blocks[target.block.0 as usize]
            .params
            .iter()
            .zip(&target.args)
            .map(|(&p, &a)| {
                (
                    self.locations[p.0 as usize],
                    self.locations[a.0 as usize],
                    self.func.ty(p),
                )
            })
            .filter(|(d, s, _)| d != s)
            .collect()
    }
    fn edge(&mut self, target: &Target, next: usize) {
        let mut moves = self.moves(target);
        while !moves.is_empty() {
            if let Some(i) = moves
                .iter()
                .position(|(dst, _, _)| !moves.iter().any(|(_, src, _)| src == dst))
            {
                let (dst, src, ty) = moves.remove(i);
                self.copy(dst, src, ty);
            } else {
                let (dst, _, ty) = moves[0];
                let scratch = if matches!(ty, Type::F64 | Type::V128) {
                    Host::Vector(A)
                } else {
                    Host::Register(T)
                };
                self.copy(scratch, dst, ty);
                for (_, src, _) in &mut moves {
                    if *src == dst {
                        *src = scratch;
                    }
                }
            }
        }
        if target.charge != 0 {
            let record = self.func.blocks[target.block.0 as usize]
                .head
                .map_or(self.region, |h| h.record);
            let lands = self.through(target.block).map_or(target.block, |t| t.block);
            self.charge(
                i64::from(target.charge),
                record,
                self.func.head_exit == Some(lands),
            );
        }
        if let Some(through) = self.through(target.block) {
            self.edge(through, next);
        } else if target.block.0 as usize != next {
            self.asm.b(self.blocks[target.block.0 as usize]);
        }
    }
    /// A head that only names the record of one charged arrival and jumps on
    /// with charge 0. Its code is laid out inline on that arrival's edge.
    fn through(&self, block: Block) -> Option<&'a Target> {
        let data = &self.func.blocks[block.0 as usize];
        match &data.end {
            End::Jump(t)
                if data.head.is_some()
                    && block.0 != 0
                    && data.params.is_empty()
                    && data.insts.is_empty()
                    && t.charge == 0 =>
            {
                Some(t)
            }
            _ => None,
        }
    }
    /// A frame cell access at `offset` from the host address in `base`,
    /// through U when the offset needs computing.
    fn frame(&mut self, load: bool, size: Size, rt: Reg, base: Reg, offset: i32) {
        let bytes = if size == X { 8 } else { 4 };
        let emit = |rn: Reg, offset: i32| {
            let scaled = offset >= 0 && offset % bytes == 0 && offset / bytes < 4096;
            match (load, scaled) {
                (true, true) => ldr(size, rt, rn, offset as u32),
                (false, true) => str(size, rt, rn, offset as u32),
                (true, false) => ldur(size, rt, rn, offset),
                (false, false) => stur(size, rt, rn, offset),
            }
        };
        let fits = |offset: i32| {
            offset >= 0 && offset % bytes == 0 && offset / bytes < 4096
                || (-256..256).contains(&offset)
        };
        if fits(offset) {
            self.asm.emit(emit(base, offset));
        } else {
            let n = offset.unsigned_abs();
            let mut from = base;
            for (part, shift) in [(n & 0xfff, false), (n >> 12, true)] {
                if part != 0 {
                    self.asm.emit(if offset < 0 {
                        sub_imm(X, U, from, part, shift)
                    } else {
                        add_imm(X, U, from, part, shift)
                    });
                    from = U;
                }
            }
            self.asm.emit(emit(U, 0));
        }
    }
    /// An F64 frame cell access, as `frame` is an integer one.
    fn vframe(&mut self, load: bool, rt: VReg, base: Reg, offset: i32) {
        let scaled = offset >= 0 && offset % 8 == 0 && offset / 8 < 4096;
        if scaled || (-256..256).contains(&offset) {
            self.asm.emit(match (load, scaled) {
                (true, true) => vldr(VMem::D, rt, base, offset as u32),
                (false, true) => vstr(VMem::D, rt, base, offset as u32),
                (true, false) => vldur(VMem::D, rt, base, offset),
                (false, false) => vstur(VMem::D, rt, base, offset),
            });
        } else if load {
            self.frame(true, X, T, base, offset);
            self.asm.emit(fmov_to_v(X, rt, T));
        } else {
            self.asm.emit(fmov_from_v(X, T, rt));
            self.frame(false, X, T, base, offset);
        }
    }
    fn fail(&self) -> Label {
        self.cold[self.region.0 as usize]
    }

    fn charge(&mut self, charge: i64, record: RecordId, head_exit: bool) {
        let amount = charge.unsigned_abs();
        if amount == 0 {
            return;
        }
        let instruction = if amount <= 4095 {
            if charge > 0 {
                sub_imm(X, COUNT, COUNT, amount as u32, false)
            } else {
                add_imm(X, COUNT, COUNT, amount as u32, false)
            }
        } else {
            self.imm(T, amount);
            if charge > 0 {
                sub(X, COUNT, COUNT, T, Lsl, 0)
            } else {
                add(X, COUNT, COUNT, T, Lsl, 0)
            }
        };
        self.asm.emit(instruction);
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
            self.asm.tbz(true, COUNT, 63, checkpoint);
            self.asm.bind(resume);
            self.checkpoints.push((checkpoint, record, resume));
        }
    }

    fn checkpoints(&mut self) {
        for (label, record, resume) in std::mem::take(&mut self.checkpoints) {
            self.asm.bind(label);
            self.mark(Part::Cold, self.func.records[record.0 as usize].eip);
            // A recall leaves through the record as the ordinary form's
            // budget exit does.
            let leave = self.asm.label();
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
                ldr(X, U, RT, runtime::COMPLETED_LIMIT),
                cmp(X, T, U, Lsl, 0),
            ]);
            self.asm.b_cond(Cond::Hi, leave);
            self.asm.emit(str(X, T, RT, runtime::COMPLETED));
            self.imm(COUNT, u64::from(runtime::COMPUTATION_SLICE - refund));
            self.asm.b(resume);
            self.asm.bind(leave);
            self.asm
                .emit_all(&[movz(W, T, 1, 0), str(W, T, RT, runtime::OPT_RECALLED)]);
            self.asm.b(self.cold[record.0 as usize]);
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
        let loc = result.map(|v| self.locations[v.0 as usize]);
        let d = result.map_or(T, |v| self.rd(v));
        let q = result.map_or(A, |v| self.vd(v));
        match op {
            Op::Constant(_, bits) => {
                if !matches!(loc, Some(Host::Constant(_))) {
                    self.copy(loc.unwrap(), Host::Constant(bits), self.func.ty(out));
                }
                return;
            }
            Op::Flags { kind, a, b, .. } if kind.floating() => {
                let a = self.vr(a, A);
                let b = self.vr(b, B);
                self.asm.emit(compare(kind, a, b));
            }
            Op::Flags { .. } => {}
            Op::Test { flags, cond } => {
                let c = self.condition(self.locations[flags.0 as usize], cond);
                self.asm.emit(cset(W, d, c));
            }
            Op::Int(op, a, b) => {
                let size = self.size(a);
                let a = self.gp(a, T);
                let immediate = match self.defs[b.0 as usize] {
                    Some(Op::Constant(_, n)) => int_immediate(op, size, d, a, n as u64),
                    _ => None,
                };
                let word = if let Some(word) = immediate {
                    word
                } else {
                    let b = self.gp(b, U);
                    match op {
                        IntOp::Add => add(size, d, a, b, Lsl, 0),
                        IntOp::Sub => sub(size, d, a, b, Lsl, 0),
                        IntOp::And => and(size, d, a, b, Lsl, 0),
                        IntOp::Or => orr(size, d, a, b, Lsl, 0),
                        IntOp::Xor => eor(size, d, a, b, Lsl, 0),
                        IntOp::Shl => shiftv(size, Lsl, d, a, b),
                        IntOp::Shr => shiftv(size, Lsr, d, a, b),
                        IntOp::Sar => shiftv(size, Asr, d, a, b),
                        IntOp::Mul => mul(size, d, a, b),
                        IntOp::MulHighSigned if size == X => smulh(d, a, b),
                        IntOp::MulHighUnsigned if size == X => umulh(d, a, b),
                        _ => {
                            self.asm.emit(if op == IntOp::MulHighSigned {
                                smull(d, a, b)
                            } else {
                                umull(d, a, b)
                            });
                            lsr_imm(X, d, d, 32)
                        }
                    }
                };
                self.asm.emit(word);
            }
            Op::IntUnary(op, a) => {
                let size = self.size(a);
                let a = self.gp(a, T);
                self.asm.emit(match op {
                    IntUnary::Not => mvn(size, d, a),
                    IntUnary::Neg => sub(size, d, Reg::ZR, a, Lsl, 0),
                    IntUnary::ZeroExtend8 => ubfx(W, d, a, 0, 8),
                    IntUnary::ZeroExtend16 => ubfx(W, d, a, 0, 16),
                    IntUnary::SignExtend8 => sbfx(W, d, a, 0, 8),
                    IntUnary::SignExtend16 => sbfx(W, d, a, 0, 16),
                    IntUnary::High32 => lsr_imm(X, d, a, 32),
                    IntUnary::SignWiden => sbfx(X, d, a, 0, 32),
                    _ => mov(W, d, a),
                });
            }
            Op::Div {
                signed,
                dividend,
                divisor,
                proved,
            } => {
                let n = self.gp(dividend, T);
                let mut b = self.gp(divisor, U);
                if !proved {
                    self.asm.cbz(W, false, b, self.fail());
                }
                if signed {
                    self.asm.emit(sbfx(X, U, b, 0, 32));
                    b = U;
                }
                self.asm.emit(if signed {
                    sdiv(X, d, n, b)
                } else {
                    udiv(X, d, n, b)
                });
                if !proved {
                    self.asm.emit(if signed {
                        sbfx(X, U, d, 0, 32)
                    } else {
                        mov(W, U, d)
                    });
                    self.asm.emit(cmp(X, d, U, Lsl, 0));
                    self.asm.b_cond(Cond::Ne, self.fail());
                }
                if signed {
                    self.asm.emit(mov(W, d, d));
                }
            }
            Op::Remainder {
                quotient,
                dividend,
                divisor,
            } => {
                let q = self.gp(quotient, T);
                let b = self.gp(divisor, U);
                if let Host::Register(n) = self.locations[dividend.0 as usize] {
                    self.asm.emit(msub(W, d, q, b, n));
                } else {
                    self.asm.emit(mul(W, T, q, b));
                    let n = self.gp(dividend, U);
                    self.asm.emit(sub(W, d, n, T, Lsl, 0));
                }
            }
            Op::Select { cond, a, b } => {
                let c = self.gp(cond, T);
                self.asm.emit(cmp_imm(W, c, 0, false));
                if matches!(self.func.ty(out), Type::F64 | Type::V128) {
                    let a = self.vr(a, A);
                    let b = self.vr(b, B);
                    if self.func.ty(out) == Type::F64 {
                        self.asm.emit(fcsel(q, a, b, Cond::Ne));
                    } else {
                        self.asm.emit_all(&[
                            cset(W, T, Cond::Ne),
                            sub(X, T, Reg::ZR, T, Lsl, 0),
                            vdup(Arr::D2, C, T),
                            vlogic(Logic::Bsl, true, C, a, b),
                        ]);
                        self.asm.emit(vmov(true, q, C));
                    }
                } else {
                    let a = self.gp(a, T);
                    let b = self.gp(b, U);
                    self.asm.emit(csel(self.size(out), d, a, b, Cond::Ne));
                }
            }
            Op::Bits { to, value } => {
                let from = self.func.ty(value);
                if matches!(to, Type::I32 | Type::I64) {
                    if matches!(from, Type::F64 | Type::V128) {
                        let v = self.vr(value, A);
                        self.asm.emit(fmov_from_v(self.size(out), d, v));
                    } else {
                        // An I32 in a register keeps its upper half zero, as
                        // does an I64 shifted right by 32, so neither needs
                        // an instruction in place.
                        let r = self.gp(value, T);
                        let high = matches!(self.defs[value.0 as usize], Some(Op::Int(IntOp::Shr, _, k))
                            if matches!(self.defs[k.0 as usize], Some(Op::Constant(_, 32..))));
                        if !(r == d && (from == Type::I32 || high)) {
                            self.asm.emit(mov(self.size(out), d, r));
                        }
                    }
                } else {
                    if matches!(from, Type::I32 | Type::I64) {
                        let r = self.gp(value, T);
                        self.asm.emit(fmov_to_v(self.size(value), q, r));
                    } else {
                        let v = self.vr(value, A);
                        if to == Type::F64 {
                            self.asm.emit(fmov_d(q, v));
                        } else if q != v {
                            self.asm.emit(vmov(true, q, v));
                        }
                    }
                }
            }
            Op::Fp(op, a, b) => {
                let a = self.vr(a, A);
                let b = self.vr(b, B);
                self.asm.emit(dfloat(
                    match op {
                        FpOp::Add => Float::Fadd,
                        FpOp::Sub => Float::Fsub,
                        FpOp::Mul => Float::Fmul,
                        FpOp::Div => Float::Fdiv,
                    },
                    q,
                    a,
                    b,
                ));
            }
            Op::FpUnary(op, a) => {
                let a = self.vr(a, A);
                self.asm.emit(unary(
                    match op {
                        FpUnary::Neg => Unary::Fneg,
                        FpUnary::Abs => Unary::Fabs,
                    },
                    q,
                    a,
                ));
            }
            Op::FromInt(a) => {
                let r = self.gp(a, T);
                if self.size(a) == X {
                    self.asm.emit_all(&[
                        mrs_fpsr(U),
                        and_imm(W, U, U, !0x10u32 as u64),
                        msr_fpsr(U),
                    ]);
                }
                self.asm.emit(scvtf(self.size(a), q, r));
                if self.size(a) == X {
                    self.asm.emit(mrs_fpsr(T));
                    let ok = self.asm.label();
                    self.asm.tbz(false, T, 4, ok);
                    self.asm.b(self.fail());
                    self.asm.bind(ok);
                }
            }
            Op::ToInt { value, wide } => {
                let a = self.vr(value, A);
                self.asm
                    .emit(fcvt_int(if wide { X } else { W }, true, d, a));
            }
            Op::Vec(op, a, b) => {
                let a = self.vr(a, A);
                let b = self.vr(b, B);
                match op {
                    VecOp::SubI32 => self.asm.emit(vsame(Same::Sub, Arr::S4, q, a, b)),
                    VecOp::LessF64 => self.asm.emit(vcompare(Float::Fcmgt, false, true, q, b, a)),
                    VecOp::ShlI64 | VecOp::ShrI64 => {
                        self.asm
                            .emit_all(&[fmov_from_v(X, T, b), cmp_imm(X, T, 63, false)]);
                        let zero = self.asm.label();
                        let done = self.asm.label();
                        self.asm.b_cond(Cond::Hi, zero);
                        if op == VecOp::ShrI64 {
                            self.asm.emit(sub(X, T, Reg::ZR, T, Lsl, 0));
                        }
                        self.asm
                            .emit_all(&[vdup(Arr::D2, C, T), vsame(Same::Ushl, Arr::D2, q, a, C)]);
                        self.asm.b(done);
                        self.asm.bind(zero);
                        self.asm.emit(vzero(true, q));
                        self.asm.bind(done);
                    }
                    _ => self.asm.emit(match op {
                        VecOp::And => vlogic(Logic::And, true, q, a, b),
                        VecOp::AndNot => vlogic(Logic::Bic, true, q, b, a),
                        VecOp::Or => vlogic(Logic::Orr, true, q, a, b),
                        _ => vlogic(Logic::Eor, true, q, a, b),
                    }),
                }
            }
            Op::Load { ty, width, addr } => {
                let addr = self.gp(addr, U);
                self.site();
                if matches!(ty, Type::I32 | Type::I64) {
                    self.asm.emit(match width {
                        1 => ldr_narrow(Narrow::Byte, d, ALIAS, addr),
                        2 => ldr_narrow(Narrow::Half, d, ALIAS, addr),
                        _ => ldr_reg(if width == 8 { X } else { W }, d, ALIAS, addr, false),
                    });
                } else {
                    self.asm.emit(vldr_reg(mem_size(width), q, ALIAS, addr));
                }
            }
            Op::Host(addr) => {
                let addr = self.gp(addr, U);
                self.asm.emit(add_ext(X, d, ALIAS, addr, Extend::Uxtw, 0));
            }
            Op::FrameLoad { ty, base, offset } => {
                let base = self.gp(base, U);
                if ty == Type::F64 {
                    self.vframe(true, q, base, offset);
                } else {
                    let size = if ty == Type::I64 { X } else { W };
                    self.frame(true, size, d, base, offset);
                }
            }
            Op::FrameStore {
                base,
                offset,
                value,
            } => {
                let base = self.gp(base, U);
                if self.func.ty(value) == Type::F64 {
                    let value = self.vr(value, A);
                    self.vframe(false, value, base, offset);
                } else {
                    let size = self.size(value);
                    let value = self.gp(value, T);
                    self.frame(false, size, value, base, offset);
                }
            }
            Op::Store {
                width,
                addr,
                value,
                after,
            } => {
                let addr = self.gp(addr, U);
                if matches!(self.func.ty(value), Type::I32 | Type::I64) {
                    let value = self.gp(value, T);
                    self.site();
                    self.asm.emit(match width {
                        1 => str_narrow(Narrow::Byte, value, ALIAS, addr),
                        2 => str_narrow(Narrow::Half, value, ALIAS, addr),
                        _ => str_reg(if width == 8 { X } else { W }, value, ALIAS, addr, false),
                    });
                } else {
                    let value = self.vr(value, A);
                    self.site();
                    self.asm.emit(vstr_reg(mem_size(width), value, ALIAS, addr));
                }
                self.region = after;
                self.mark(Part::Body, self.func.records[after.0 as usize].eip);
            }
            Op::X87Load {
                bits,
                denormal,
                single,
            } => {
                let r = self.gp(bits, U);
                let nan = self.asm.label();
                let done = self.asm.label();
                // Exponent position and width, and the bits above the fraction.
                let (size, low, exponent, high) = if single {
                    (W, 23, 8, 9)
                } else {
                    (X, 52, 11, 12)
                };
                self.asm.emit_all(&[
                    ubfx(size, T, r, low, exponent),
                    cmp_imm(W, T, (1 << exponent) - 1, false),
                ]);
                self.asm.b_cond(Cond::Eq, nan);
                if denormal || single {
                    self.asm.cbz(W, true, T, done);
                    self.asm.emit(lsl_imm(size, T, r, 1));
                    self.asm.cbz(size, true, T, self.fail());
                }
                self.asm.b(done);
                self.asm.bind(nan);
                self.asm.emit(lsl_imm(size, T, r, high));
                self.asm.cbz(size, true, T, self.fail());
                self.asm.bind(done);
                self.asm.emit(fmov_to_v(size, q, r));
                if single {
                    self.asm.emit(unary(Unary::Widen, q, q));
                }
            }
            Op::Validate => {
                self.asm
                    .emit_all(&[mrs_fpsr(T), and_imm(W, T, T, 0xffff_ff8f)]);
                self.asm.cbz(W, true, T, self.fail());
            }
        }
        if let Some(loc) = loc
            && !matches!(loc, Host::Constant(_) | Host::Flags(_))
        {
            let ty = self.func.ty(out);
            let src = if matches!(ty, Type::F64 | Type::V128) {
                Host::Vector(q)
            } else {
                Host::Register(d)
            };
            self.copy(loc, src, ty);
        }
    }
}

fn add_immediate(n: u64) -> Option<(u32, bool)> {
    if n <= 4095 {
        Some((n as u32, false))
    } else if n & 4095 == 0 && n >> 12 <= 4095 {
        Some(((n >> 12) as u32, true))
    } else {
        None
    }
}
fn int_immediate(op: IntOp, size: Size, d: Reg, a: Reg, n: u64) -> Option<u32> {
    Some(match op {
        IntOp::Add | IntOp::Sub => {
            // A negative addend subtracts its magnitude, and the reverse.
            let mask = if size == X {
                u64::MAX
            } else {
                u64::from(u32::MAX)
            };
            let (add, (n, shift)) = match add_immediate(n) {
                Some(n) => (op == IntOp::Add, n),
                None => (op == IntOp::Sub, add_immediate(n.wrapping_neg() & mask)?),
            };
            if add {
                add_imm(size, d, a, n, shift)
            } else {
                sub_imm(size, d, a, n, shift)
            }
        }
        IntOp::Mul if n.is_power_of_two() => lsl_imm(size, d, a, n.trailing_zeros()),
        IntOp::And | IntOp::Or | IntOp::Xor => {
            bitmask(size, n)?;
            match op {
                IntOp::And => and_imm(size, d, a, n),
                IntOp::Or => orr_imm(size, d, a, n),
                _ => eor_imm(size, d, a, n),
            }
        }
        IntOp::Shl | IntOp::Shr | IntOp::Sar => {
            let n = (n & if size == X { 63 } else { 31 }) as u32;
            match op {
                IntOp::Shl => lsl_imm(size, d, a, n),
                IntOp::Shr => lsr_imm(size, d, a, n),
                _ => asr_imm(size, d, a, n),
            }
        }
        _ => return None,
    })
}
fn scalar_size(ty: Type) -> Size {
    if matches!(ty, Type::I32 | Type::Flags) {
        W
    } else {
        X
    }
}
fn mem_size(width: u8) -> VMem {
    [VMem::B, VMem::H, VMem::S, VMem::D, VMem::Q][width.trailing_zeros() as usize]
}
fn mem_type(ty: Type) -> VMem {
    [VMem::S, VMem::D, VMem::D, VMem::Q, VMem::S][ty as usize]
}

/// The NZCV compare of a floating `Flags` kind; `comisd` raises invalid
/// for a quiet NaN as well.
pub(super) fn compare(kind: FlagKind, a: VReg, b: VReg) -> u32 {
    if kind == FlagKind::OrderedCompare {
        fcmpe(a, b)
    } else {
        fcmp(a, b)
    }
}
