//! Register-allocated AArch64 computations with cold recovery recipes.
//! Jobs load entry cells and leave through the capture stub. Inline forms
//! admit canonical Tier-1 state and publish normal exits at Tier-1 links.
//! Floating point runs under the canonical FPCR and validates by value
//! (A64-OPT-4).
mod alloc;
mod cost;
mod flags;
mod inline;
mod resident;

#[cfg(any(native, test))]
pub(crate) use resident::access_stub;

use super::front::Assumed;
use super::ir::*;
use super::recovery::{FlagRecipe, Host, Record};
use crate::asm::{Asm, Label};
use crate::encode::Shift::{Asr, Lsl, Lsr};
use crate::encode::Size::{W, X};
use crate::encode::*;
use crate::jit_map::{Mark, Part};
use crate::runtime::{self, ResidentCold, Runtime};
use crate::translate::access::{CheckedEmitter, Value as AccessReg, page_crossing};
use crate::translate::{Carry, Overflow, Sense, Site, WINDOW, fp_class};
use std::mem::offset_of;

const T: Reg = Reg::new(16);
const U: Reg = Reg::new(17);
const RT: Reg = Reg::new(29);
const SP: Reg = Reg::new(31);
const ALIAS: Reg = Reg::new(28);
const COUNT: Reg = Reg::new(9);
const D: VReg = VReg::new(28);
const E: VReg = VReg::new(27);
const F: VReg = VReg::new(26);
const A: VReg = VReg::new(29);
const B: VReg = VReg::new(30);
const C: VReg = VReg::new(31);
const PARAMS: u32 = offset_of!(Runtime, opt_params) as u32;
const RECORD: u32 = offset_of!(Runtime, opt_record) as u32;

pub(crate) struct Lowered {
    /// The inline entry, miss and cold branch words.
    pub entry: usize,
    pub miss: usize,
    /// The branch word a failed `PlainPage` entry fact takes (A64-OPT-9).
    pub env_miss: usize,
    pub cold: usize,
    /// Static costs of the resident entry, longest normal exit, and a loop iteration.
    pub entry_words: u32,
    pub exit_words: u32,
    pub iteration_words: u32,
    pub iteration_retired: u32,
    pub iteration_pcs: Vec<u32>,
    /// Entry snapshot and normal exit accounting calls, patched to NOP after probation.
    pub probation: Vec<usize>,
    /// Two words holding the address of the probation counters, bound at placement.
    pub probation_slot: usize,
    /// Tier-1 link points as (byte offset, guest EIP). The fourth word
    /// of each link is bound to the dispatch stub during placement.
    pub links: Vec<(usize, u32)>,
    pub words: Vec<u32>,
    pub sites: Vec<(usize, Site)>,
    pub marks: Vec<Mark>,
    pub records: Vec<Record<Host>>,
    pub flags: Vec<Option<FlagRecipe>>,
    /// 16-byte cells at the captured SP, discarded by `opt_exit`.
    pub spill_cells: usize,
    /// The word job and cold inline exits leave through. Placement binds
    /// it to `opt_exit`.
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
    lower_form(func, None, false, false)
}

/// Code entered and linked in the Tier-1 register assignment.
pub(crate) fn lower_inline(func: &Func, assumed: &Assumed, entry: &[Source]) -> Lowered {
    debug_assert_eq!(func.entry, entry);
    lower_form(func, Some((assumed, entry)), false, false)
}

/// A resident form: entered from Tier-1 back edges, accessing guest memory
/// through the window (A64-OPT-9).
pub(crate) fn lower_resident(
    func: &Func,
    assumed: &Assumed,
    entry: &[Source],
    count_entries: bool,
) -> Lowered {
    debug_assert_eq!(func.entry, entry);
    lower_form(func, Some((assumed, entry)), true, count_entries)
}

fn lower_form(
    func: &Func,
    inline: Option<(&Assumed, &[Source])>,
    resident: bool,
    count_entries: bool,
) -> Lowered {
    assert_eq!(super::verify::verify(func), Ok(()));
    let allocation = alloc::allocate(func, inline.is_some(), resident);
    let mut asm = Asm::new();
    // A bit test whose target lands out of range is emitted wide, and the
    // form again, as the ordinary translator does.
    let (mut e, exit, miss, cold) = loop {
        let (mut e, exit, miss, cold) =
            emit(func, &allocation, asm, inline, resident, count_entries);
        if !e.asm.widen_bit_tests() {
            break (e, exit, miss, cold);
        }
        asm = e.asm;
        asm.rewind();
    };
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
    let miss = miss.map_or(0, |label| e.asm.position(label) / 4);
    let cold = cold.map_or(0, |label| e.asm.position(label) / 4);
    let env_miss = e.env_miss.map_or(miss, |label| e.asm.position(label) / 4);
    let probation_slot = e
        .probation
        .as_ref()
        .map_or(0, |p| e.asm.position(p.slot) / 4);
    let words = e.asm.finish();
    #[cfg(test)]
    assert_no_fp_state(&words);
    Lowered {
        entry: 0,
        miss,
        env_miss,
        cold,
        entry_words: e.entry_words,
        exit_words: e.exit_words,
        iteration_words: e.iteration.words,
        iteration_retired: e.iteration.retired,
        iteration_pcs: e.iteration.pcs,
        probation: e.probation.map_or_else(Vec::new, |p| p.calls),
        probation_slot,
        links: e.links,
        exit,
        words,
        sites: e.sites,
        marks: e.marks,
        records,
        flags: e.flags,
        spill_cells: allocation.spill_cells,
    }
}

#[cfg(test)]
pub(in crate::translate) fn assert_no_fp_state(words: &[u32]) {
    let accesses = [
        mrs_fpcr(Reg::ZR),
        msr_fpcr(Reg::ZR),
        mrs_fpsr(Reg::ZR),
        msr_fpsr(Reg::ZR),
    ];
    for &word in words {
        assert!(
            !accesses.contains(&(word | 31)),
            "FP state access {word:08x}"
        );
    }
}

/// Emits the form into `asm`, returning its emitter and exit label.
fn emit<'a>(
    func: &'a Func,
    allocation: &alloc::Allocation,
    mut asm: Asm,
    inline: Option<(&Assumed, &[Source])>,
    resident: bool,
    count_entries: bool,
) -> (Emitter<'a>, Label, Option<Label>, Option<Label>) {
    let flags = super::recovery::lower_flags(&allocation.defs, &allocation.locations);
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
    let env_miss = resident.then(|| asm.label());
    let probation = resident.then(|| resident::Probation::new(&mut asm));
    let last_fact = resident
        .then(|| {
            func.blocks[0]
                .insts
                .iter()
                .rposition(|(_, op)| matches!(op, Op::PlainPage { .. }))
        })
        .flatten();
    let mut e = Emitter {
        func,
        asm,
        blocks,
        cold,
        failures: Vec::new(),
        normal: if inline.is_some() {
            vec![None; func.records.len()]
        } else {
            Vec::new()
        },
        resident,
        env_miss,
        probation,
        entry_words: 0,
        exit_words: 0,
        iteration: cost::Iteration::default(),
        fact_misses: Vec::new(),
        checked_accesses: Vec::new(),
        guest_eip: 0,
        locations: allocation.locations.clone(),
        defs: allocation.defs.clone(),
        flags,
        sites: Vec::new(),
        marks: Vec::new(),
        region: RecordId(0),
        checkpoints: Vec::new(),
        fp_checks: Vec::new(),
        edges: Vec::new(),
        links: Vec::new(),
    };
    let first = func.blocks[0].head.unwrap().record;
    // spec: A64-OPT-7
    e.mark(Part::Body, func.records[first.0 as usize].eip);
    let bytes = allocation.spill_cells as u64 * 16;
    let (miss, cold) = if let Some((assumed, entry)) = inline {
        let miss = e.asm.label();
        let cold = e.asm.label();
        e.inline_entry(assumed, entry, bytes, miss, cold, last_fact.is_some());
        (Some(miss), Some(cold))
    } else {
        e.stack_frame(bytes, true);
        e.job_entry();
        (None, None)
    };
    if last_fact.is_none() {
        e.begin_body(first, count_entries);
    }
    let mut costs = vec![cost::BlockCost::default(); func.blocks.len()];
    for (b, data) in func.blocks.iter().enumerate() {
        if e.through(Block(b as u32)).is_some() {
            continue;
        }
        let delayed = b == 0 && last_fact.is_some();
        if !delayed {
            e.asm.bind(e.blocks[b]);
        }
        let mut start = e.asm.offset();
        e.region = allocation.regions[b];
        e.mark(
            Part::Body,
            data.pcs
                .first()
                .copied()
                .unwrap_or(func.records[e.region.0 as usize].eip),
        );
        for (i, &(result, op)) in data.insts.iter().enumerate() {
            if matches!(op, Op::Test { .. }) && result.is_some_and(|v| branch_only[v.0 as usize]) {
                continue;
            }
            e.guest_eip = if op.region_guard().is_some() {
                data.insts[i + 1..]
                    .iter()
                    .take(2)
                    .find_map(|&(_, next)| match next {
                        Op::Store { pc, .. } => Some(pc),
                        _ => None,
                    })
                    .expect("store guard precedes its store")
            } else {
                result
                    .and_then(|value| func.cold_pcs.get(&value).copied())
                    .unwrap_or(func.records[e.region.0 as usize].eip)
            };
            e.op(result, op);
            if b == 0 && last_fact == Some(i) {
                e.entry_frames(&func.entry);
                e.begin_body(first, count_entries);
                e.asm.bind(e.blocks[b]);
                start = e.asm.offset();
            }
        }
        match &data.end {
            End::Jump(t) => {
                let edge = e.asm.offset();
                e.edge(t, b + 1);
                costs[b].edges[0] = e.words_since(edge);
                costs[b].body = ((edge - start) / 4) as u32;
            }
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
                    e.edges.push((label, jump.clone(), b, usize::from(!when)));
                    label
                };
                match test {
                    Ok(c) => e.asm.b_cond(if when { c } else { c.invert() }, target),
                    Err(r) => e.asm.cbz(W, when, r, target),
                }
                let edge = e.asm.offset();
                e.edge(inline, b + 1);
                costs[b].edges[usize::from(when)] = e.words_since(edge);
                costs[b].body = ((edge - start) / 4) as u32;
            }
            End::Exit(id) => {
                let exit = if inline.is_some() {
                    e.normal_exit(*id)
                } else {
                    e.cold[id.0 as usize]
                };
                e.asm.b(exit);
            }
            End::Unset => unreachable!(),
        }
    }
    for (label, target, block, index) in std::mem::take(&mut e.edges) {
        e.asm.bind(label);
        let start = e.asm.offset();
        e.edge(&target, usize::MAX);
        costs[block].edges[index] = e.words_since(start);
    }
    if resident {
        e.iteration = cost::iteration(func, &costs);
        e.checkpoints();
    }
    if let Some((assumed, _)) = inline {
        for (i, label) in std::mem::take(&mut e.normal).into_iter().enumerate() {
            if let Some(label) = label {
                e.asm.bind(label);
                e.mark(Part::Body, func.records[i].eip);
                e.inline_exit(&func.records[i], assumed, bytes);
            }
        }
    }
    if !resident {
        e.checkpoints();
    }
    e.fp_checks();
    e.checked_accesses();
    for (label, record, reason, pc) in std::mem::take(&mut e.failures) {
        e.asm.bind(label);
        e.mark(Part::Cold, pc);
        e.record_cold(reason, pc);
        e.asm.b(e.cold[record.0 as usize]);
    }
    if let Some(probation) = &e.probation {
        probation.emit(&mut e.asm);
    }
    if let Some(miss) = env_miss {
        e.environment_misses(bytes, miss);
    }
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
    for label in [miss, cold, env_miss].into_iter().flatten() {
        e.asm.bind(label);
        e.asm.emit(0);
    }
    (e, exit, miss, cold)
}

struct FpCheck {
    op: FpOp,
    a: VReg,
    b: VReg,
    check: Label,
    resume: Label,
    region: RecordId,
    fail: Label,
}

struct Emitter<'a> {
    func: &'a Func,
    asm: Asm,
    blocks: Vec<Label>,
    cold: Vec<Label>,
    failures: Vec<(Label, RecordId, ResidentCold, u32)>,
    normal: Vec<Option<Label>>,
    resident: bool,
    env_miss: Option<Label>,
    probation: Option<resident::Probation>,
    entry_words: u32,
    exit_words: u32,
    iteration: cost::Iteration,
    fact_misses: Vec<Label>,
    checked_accesses: Vec<resident::CheckedAccess>,
    guest_eip: u32,
    locations: Vec<Host>,
    defs: Vec<Option<Op>>,
    flags: Vec<Option<FlagRecipe>>,
    sites: Vec<(usize, Site)>,
    marks: Vec<Mark>,
    region: RecordId,
    checkpoints: Vec<(Label, RecordId, Label)>,
    fp_checks: Vec<FpCheck>,
    edges: Vec<(Label, Target, usize, usize)>,
    links: Vec<(usize, u32)>,
}

impl CheckedEmitter for Emitter<'_> {
    fn access_asm(&mut self) -> &mut Asm {
        &mut self.asm
    }

    fn access_site(&mut self) {
        self.site(true);
    }
}

impl<'a> Emitter<'a> {
    fn data_view(&self) -> Reg {
        if self.resident { WINDOW } else { ALIAS }
    }

    fn normal_exit(&mut self, record: RecordId) -> Label {
        *self.normal[record.0 as usize].get_or_insert_with(|| self.asm.label())
    }

    fn words_since(&self, start: usize) -> u32 {
        ((self.asm.offset() - start) / 4) as u32
    }

    fn begin_body(&mut self, record: RecordId, count_entries: bool) {
        let start = self.asm.offset();
        if count_entries {
            let entries = offset_of!(Runtime, resident_entries) as u32;
            self.asm.emit_all(&[
                ldr(X, T, RT, entries),
                add_imm(X, T, T, 1, false),
                str(X, T, RT, entries),
            ]);
        }
        let diagnostic_words = self.words_since(start);
        if let Some(probation) = &mut self.probation {
            probation.call(&mut self.asm, true);
        }
        self.charge(
            i64::from(self.func.records[record.0 as usize].refund),
            record,
            false,
        );
        if self.resident {
            self.entry_words = self.words_since(0) - diagnostic_words;
        }
    }

    fn stack_frame(&mut self, bytes: u64, enter: bool) {
        if bytes == 0 {
            return;
        }
        if let Some((n, shift)) = add_immediate(bytes) {
            let op = if enter { sub_imm } else { add_imm };
            self.asm.emit(op(X, SP, SP, n, shift));
        } else {
            self.imm(T, bytes);
            let op = if enter { sub_ext } else { add_ext };
            self.asm.emit(op(X, SP, SP, T, Extend::Uxtx, 0));
        }
    }

    fn job_entry(&mut self) {
        if !self.func.entry.is_empty() {
            self.asm.emit(ldr(X, U, RT, PARAMS));
        }
        for (i, &p) in self.func.blocks[0].params.iter().enumerate() {
            let loc = self.locations[p.0 as usize];
            match loc {
                Host::Register(n) => self.asm.emit(ldr(self.size(p), n, U, i as u32 * 16)),
                Host::Vector(n) => {
                    self.asm
                        .emit(vldr(mem_type(self.func.ty(p)), n, U, i as u32 * 16));
                }
                _ => {
                    self.asm
                        .emit(vldr(mem_type(self.func.ty(p)), A, U, i as u32 * 16));
                    self.copy(loc, Host::Vector(A), self.func.ty(p));
                }
            }
        }
    }

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
    fn parallel_moves(&mut self, mut moves: Vec<(Host, Host, Type)>) {
        moves.retain(|(dst, src, _)| dst != src);
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
    }

    fn edge(&mut self, target: &Target, next: usize) {
        self.parallel_moves(self.moves(target));
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
    fn failure(&mut self, record: RecordId, reason: ResidentCold) -> Label {
        if !self.resident {
            return self.cold[record.0 as usize];
        }
        let label = self.asm.label();
        self.failures.push((label, record, reason, self.guest_eip));
        label
    }

    fn fail(&mut self) -> Label {
        self.failure(self.region, ResidentCold::Validation)
    }

    fn record_cold(&mut self, reason: ResidentCold, pc: u32) {
        self.imm(T, u64::from(pc));
        self.asm
            .emit(str(W, T, RT, offset_of!(Runtime, resident_site) as u32));
        self.asm.emit_all(&[
            movz(W, T, reason as u16, 0),
            str(W, T, RT, offset_of!(Runtime, resident_cold) as u32),
        ]);
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
        if head_exit && !self.resident {
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
            let defer_recall = self.func.head_exit.is_some() && !self.resident;
            let recalled = if defer_recall {
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
            if defer_recall {
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
            if self.resident {
                let exit = self.normal_exit(record);
                self.asm.b(exit);
            } else {
                self.asm
                    .emit_all(&[movz(W, T, 1, 0), str(W, T, RT, runtime::OPT_RECALLED)]);
                self.asm.b(self.cold[record.0 as usize]);
            }
        }
    }

    // spec: A64-OPT-4
    fn fp(&mut self, op: FpOp, dst: VReg, a: VReg, b: VReg) {
        // A cold check may still need an operand after the arithmetic.
        let result = if dst == a || dst == b { C } else { dst };
        self.asm.emit(dfloat(
            match op {
                FpOp::Add => Float::Fadd,
                FpOp::Sub => Float::Fsub,
                FpOp::Mul => Float::Fmul,
                FpOp::Div => Float::Fdiv,
            },
            result,
            a,
            b,
        ));
        let check = self.asm.label();
        let resume = self.asm.label();
        let sum = matches!(op, FpOp::Add | FpOp::Sub);
        fp_class::result_bits(&mut self.asm, true, result, T, U, sum.then_some(U));
        if sum {
            self.asm.cbz(W, false, U, check);
        } else {
            // Exponents 2 through 2046 are safely normal.
            self.asm
                .emit_all(&[sub_imm(W, U, U, 3, false), cmp_imm(W, U, 2045, false)]);
            self.asm.b_cond(Cond::Hs, check);
        }
        self.asm.bind(resume);
        if dst != result {
            self.asm.emit(vmov(false, dst, result));
        }
        let fail = self.fail();
        self.fp_checks.push(FpCheck {
            op,
            a,
            b,
            check,
            resume,
            region: self.region,
            fail,
        });
    }

    fn fp_checks(&mut self) {
        for FpCheck {
            op,
            a,
            b,
            check,
            resume,
            region,
            fail,
        } in std::mem::take(&mut self.fp_checks)
        {
            self.asm.bind(check);
            self.mark(Part::Cold, self.func.records[region.0 as usize].eip);
            let nonfinite = self.asm.label();
            self.asm.emit_all(&[
                lsl_imm(X, T, T, 1),
                lsr_imm(X, U, T, 53),
                cmp_imm(W, U, 2047, false),
            ]);
            self.asm.b_cond(Cond::Eq, nonfinite);
            if matches!(op, FpOp::Mul | FpOp::Div) {
                let nonzero = self.asm.label();
                self.asm.cbz(X, true, T, nonzero);
                self.asm.emit(fcmp_zero(a));
                self.asm.b_cond(Cond::Eq, resume);
                if op == FpOp::Mul {
                    self.asm.emit(fcmp_zero(b));
                    self.asm.b_cond(Cond::Eq, resume);
                }
                self.asm.b(fail);
                self.asm.bind(nonzero);
                self.asm
                    .emit_all(&[movz(X, U, 0x0020, 48), cmp(X, T, U, Lsl, 0)]);
                self.asm.b_cond(Cond::Ls, fail);
            } else {
                self.asm.cbz(X, true, T, fail);
            }
            self.asm.b(resume);

            self.asm.bind(nonfinite);
            let infinite = self.asm.label();
            self.asm.emit(lsl_imm(X, T, T, 11));
            self.asm.cbz(X, false, T, infinite);
            // A NaN from two numeric operands is invalid. Quiet NaNs
            // propagate in operand order, provided neither operand signals.
            self.asm.emit(fcmp(a, b));
            self.asm.b_cond(Cond::Vc, fail);
            fp_class::snan(&mut self.asm, a, T, fail);
            fp_class::snan(&mut self.asm, b, T, fail);
            self.asm.b(resume);
            self.asm.bind(infinite);
            // Neither operand can be NaN when the result is infinity.
            self.asm.emit(movz(X, U, 0xffe0, 48));
            for operand in [a, b] {
                self.asm.emit_all(&[
                    fmov_from_v(X, T, operand),
                    lsl_imm(X, T, T, 1),
                    cmp(X, T, U, Lsl, 0),
                ]);
                self.asm.b_cond(Cond::Eq, resume);
            }
            self.asm.b(fail);
        }
    }

    fn site(&mut self, checked: bool) {
        let record = &self.func.records[self.region.0 as usize];
        self.sites.push((
            self.asm.offset(),
            Site {
                eip: if self.resident {
                    self.guest_eip
                } else {
                    record.eip
                },
                refund: record.refund,
                sense: Sense::Subtraction,
                carry: Carry::Host,
                recipe: crate::translate::Recipe::Canonical,
                nz: None,
                overflow: Overflow::Host,
                checked,
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
            Op::PlainPage { addr, bytes, write } => self.plain_page(addr, bytes, write),
            Op::PageCross {
                addr,
                width,
                record,
            } => {
                let addr = self.gp(addr, U);
                let fail = self.failure(record, ResidentCold::PageCross);
                page_crossing(&mut self.asm, addr, u32::from(width), fail);
            }
            Op::FrameDisjoint {
                addr,
                width,
                base,
                lo,
                hi,
                record,
            } => {
                let fail = self.failure(record, ResidentCold::FrameDisjoint);
                self.frame_disjoint(addr, width, base, lo, hi, fail);
            }
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
                let fail = self.fail();
                if kind == FlagKind::OrderedCompare {
                    self.asm.b_cond(Cond::Vs, fail);
                } else {
                    let ordered = self.asm.label();
                    self.asm.b_cond(Cond::Vc, ordered);
                    fp_class::snan(&mut self.asm, a, T, fail);
                    fp_class::snan(&mut self.asm, b, T, fail);
                    self.asm.bind(ordered);
                }
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
                        IntOp::Ror => rorv(size, d, a, b),
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
                    IntUnary::ByteSwap => rev(W, d, a),
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
                let fail = (!proved).then(|| self.failure(self.region, ResidentCold::Division));
                if let Some(fail) = fail {
                    self.asm.cbz(W, false, b, fail);
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
                if let Some(fail) = fail {
                    self.asm.emit(if signed {
                        sbfx(X, U, d, 0, 32)
                    } else {
                        mov(W, U, d)
                    });
                    self.asm.emit(cmp(X, d, U, Lsl, 0));
                    self.asm.b_cond(Cond::Ne, fail);
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
                self.fp(op, q, a, b);
            }
            Op::Fp32(op, a, b) => {
                let a = self.vr(a, A);
                let b = self.vr(b, B);
                self.asm.emit(sfloat(
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
                let fail = self.fail();
                let sum = matches!(op, FpOp::Add | FpOp::Sub);
                fp_class::result_bits(&mut self.asm, false, q, T, U, None);
                if sum {
                    self.asm.tbz(true, U, 8, fail);
                } else {
                    // The boundary exponents need Tier-1's status handling.
                    self.asm
                        .emit_all(&[sub_imm(W, U, U, 3, false), cmp_imm(W, U, 253, false)]);
                    self.asm.b_cond(Cond::Hs, fail);
                }
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
                self.asm.emit(scvtf(self.size(a), q, r));
                if self.size(a) == X {
                    // Count leading sign bits and trailing zeros. For a
                    // negative power of two the complement has one fewer
                    // significant bit than its magnitude, which is exact.
                    self.asm.emit_all(&[
                        eor(X, U, r, r, Asr, 63),
                        clz(X, U, U),
                        rbit(X, T, r),
                        clz(X, T, T),
                        add(W, T, T, U, Lsl, 0),
                        cmp_imm(W, T, 64 - 53, false),
                    ]);
                    let fail = self.fail();
                    self.asm.b_cond(Cond::Lo, fail);
                }
            }
            Op::ToInt { value, wide } => {
                let a = self.vr(value, A);
                // Compare the magnitude to the exclusive limit for the
                // operand's sign. NaNs and infinities also exceed either limit.
                let (bound, step) = if wide { (0x43e0, 1) } else { (0x41e0, 1 << 21) };
                self.asm.emit_all(&[
                    fmov_from_v(X, T, a),
                    asr_imm(X, U, T, 63),
                    and_imm(X, U, U, step),
                    movk(X, U, bound, 48),
                    and_imm(X, T, T, i64::MAX as u64),
                    cmp(X, T, U, Lsl, 0),
                ]);
                let fail = self.fail();
                self.asm.b_cond(Cond::Hs, fail);
                self.asm
                    .emit(fcvt_int(if wide { X } else { W }, true, d, a));
            }
            Op::VecUnary(op, a) => {
                let a = self.vr(a, A);
                match op {
                    VecUnary::NarrowScalar => self.asm.emit(unary(Unary::Narrow, q, a)),
                    VecUnary::Low64 => self.asm.emit(vmov(false, q, a)),
                    VecUnary::High64 => {
                        self.asm.emit_all(&[vins(3, q, 0, a, 1), vmov(false, q, q)])
                    }
                    VecUnary::ShiftImm(shift_op, log2, count, full) => {
                        let arr = Arr::new(u32::from(log2), full);
                        let bits = arr.bits();
                        let count = u32::from(count);
                        if count == 0 {
                            self.asm.emit(vmov(full, q, a));
                        } else if shift_op == ShiftImm::Sshr {
                            self.asm.emit(vshift(shift_op, arr, q, a, count.min(bits)));
                        } else if count >= bits {
                            self.asm.emit(vzero(full, q));
                        } else {
                            self.asm.emit(vshift(shift_op, arr, q, a, count));
                            if !full && log2 == 3 {
                                self.asm.emit(vmov(false, q, q));
                            }
                        }
                    }
                    VecUnary::Shuffle(log2, imm, full) => {
                        let log2 = u32::from(log2);
                        let lane = u32::from(imm & 3);
                        if u32::from(imm) == lane * 0x55 {
                            self.asm.emit(vdup_lane(Arr::new(log2, full), q, a, lane));
                        } else {
                            for i in 0..4 {
                                let from = u32::from(imm) >> (2 * i) & 3;
                                self.asm.emit(vins(log2, C, i, a, from));
                            }
                            self.asm.emit(vmov(full, q, C));
                        }
                    }
                    VecUnary::ShuffleWords(high, imm) => {
                        self.asm.emit(vmov(true, C, a));
                        let base = if high { 4 } else { 0 };
                        for i in 0..4 {
                            let from = u32::from(imm) >> (i * 2) & 3;
                            self.asm.emit(vins(1, C, base + i, a, base + from));
                        }
                        self.asm.emit(vmov(true, q, C));
                    }
                    VecUnary::ShiftBytes(left, count) => {
                        let count = u32::from(count);
                        if count >= 16 {
                            self.asm.emit(vzero(true, q));
                        } else if count == 0 {
                            if q != a {
                                self.asm.emit(vmov(true, q, a));
                            }
                        } else {
                            self.asm.emit(vzero(true, C));
                            self.asm.emit(if left {
                                vext(q, C, a, 16 - count)
                            } else {
                                vext(q, a, C, count)
                            });
                        }
                    }
                }
            }
            Op::VecToInt(op, a) => {
                let a = self.vr(a, A);
                match op {
                    VecToInt::ExtractWord(lane) => {
                        self.asm.emit(vumov(1, d, a, u32::from(lane)));
                    }
                    VecToInt::ByteMask(full) => {
                        self.asm.emit_all(&[
                            movz(X, U, 0x4081, 0),
                            movk(X, U, 0x1020, 16),
                            movk(X, U, 0x0408, 32),
                            movk(X, U, 0x0002, 48),
                            vumov(3, T, a, 0),
                            and_imm(X, T, T, 0x8080_8080_8080_8080),
                            mul(X, T, T, U),
                            lsr_imm(X, d, T, 56),
                        ]);
                        if full {
                            self.asm.emit_all(&[
                                vumov(3, T, a, 1),
                                and_imm(X, T, T, 0x8080_8080_8080_8080),
                                mul(X, T, T, U),
                                lsr_imm(X, T, T, 56),
                                orr(W, d, d, T, Lsl, 8),
                            ]);
                        }
                    }
                    VecToInt::SignMask(double) => {
                        self.asm.emit_all(&[
                            vshift(
                                ShiftImm::Ushr,
                                if double { Arr::D2 } else { Arr::S4 },
                                C,
                                a,
                                if double { 63 } else { 31 },
                            ),
                            vumov(3, T, C, 0),
                            vumov(3, U, C, 1),
                        ]);
                        if double {
                            self.asm.emit(orr(W, d, T, U, Lsl, 1));
                        } else {
                            self.asm.emit_all(&[
                                orr(X, T, T, T, Lsr, 31),
                                orr(X, U, U, U, Lsr, 31),
                                and_imm(W, T, T, 3),
                                and_imm(W, U, U, 3),
                                orr(W, d, T, U, Lsl, 2),
                            ]);
                        }
                    }
                }
            }
            Op::Vec(op, a, b) => {
                let a = self.vr(a, A);
                let b = self.vr(b, B);
                match op {
                    VecOp::And(full) => self.asm.emit(vlogic(Logic::And, full, q, a, b)),
                    VecOp::AndNot(full) => self.asm.emit(vlogic(Logic::Bic, full, q, b, a)),
                    VecOp::Or(full) => self.asm.emit(vlogic(Logic::Orr, full, q, a, b)),
                    VecOp::Xor(full) => self.asm.emit(vlogic(Logic::Eor, full, q, a, b)),
                    VecOp::Same(same_op, log2, full) => {
                        self.asm
                            .emit(vsame(same_op, Arr::new(u32::from(log2), full), q, a, b));
                        if !full && log2 == 3 {
                            self.asm.emit(vmov(false, q, q));
                        }
                    }
                    VecOp::Unpack(perm, log2, full) => {
                        self.asm
                            .emit(vpermute(perm, Arr::new(u32::from(log2), full), q, a, b));
                    }
                    VecOp::Pack(narrow, log2, full) => {
                        let log2 = u32::from(log2);
                        if full {
                            self.asm.emit_all(&[
                                vnarrow(narrow, Arr::new(log2, false), C, a),
                                vnarrow(narrow, Arr::new(log2, true), C, b),
                                vmov(true, q, C),
                            ]);
                        } else {
                            self.asm.emit_all(&[
                                vmov(false, C, a),
                                vins(3, C, 1, b, 0),
                                vnarrow(narrow, Arr::new(log2, false), q, C),
                            ]);
                        }
                    }
                    VecOp::MulHigh(signed, full) => {
                        self.asm.emit(vmull(signed, Arr::H4, C, a, b));
                        if full {
                            self.asm.emit_all(&[
                                vshrn(Arr::H4, C, C, 16),
                                vmull(signed, Arr::H8, q, a, b),
                                vshrn(Arr::H8, C, q, 16),
                                vmov(true, q, C),
                            ]);
                        } else {
                            self.asm.emit(vshrn(Arr::H4, q, C, 16));
                        }
                    }
                    VecOp::MulAdd(full) => {
                        self.asm.emit(vmull(true, Arr::H4, C, a, b));
                        if full {
                            self.asm.emit_all(&[
                                vmull(true, Arr::H8, q, a, b),
                                vsame(Same::Addp, Arr::S4, q, C, q),
                            ]);
                        } else {
                            self.asm.emit_all(&[
                                vsame(Same::Addp, Arr::S4, q, C, C),
                                vmov(false, q, q),
                            ]);
                        }
                    }
                    VecOp::Shift(shift_op, log2, full) => {
                        let arr = Arr::new(u32::from(log2), full);
                        self.asm.emit_all(&[
                            fmov_from_v(X, T, b),
                            movz(X, U, arr.bits() as u16, 0),
                            cmp(X, T, U, Lsl, 0),
                            csel(X, T, T, U, Cond::Lo),
                        ]);
                        if shift_op != ShiftImm::Shl {
                            self.asm.emit(sub(X, T, Reg::ZR, T, Lsl, 0));
                        }
                        let same = if shift_op == ShiftImm::Sshr {
                            Same::Sshl
                        } else {
                            Same::Ushl
                        };
                        self.asm
                            .emit_all(&[vdup(arr, C, T), vsame(same, arr, q, a, C)]);
                        if !full && log2 == 3 {
                            self.asm.emit(vmov(false, q, q));
                        }
                    }
                    VecOp::Shufps(imm) => {
                        for lane in 0..4 {
                            let from = u32::from(imm) >> (2 * lane) & 3;
                            self.asm
                                .emit(vins(2, C, lane, if lane < 2 { a } else { b }, from));
                        }
                        self.asm.emit(vmov(true, q, C));
                    }
                    VecOp::Shufpd(imm) => {
                        self.asm.emit_all(&[
                            vins(3, C, 0, a, u32::from(imm & 1)),
                            vins(3, C, 1, b, u32::from(imm >> 1 & 1)),
                            vmov(true, q, C),
                        ]);
                    }
                    VecOp::MulDwords(full) => {
                        if full {
                            self.asm.emit_all(&[
                                vpermute(Permute::Uzp1, Arr::S4, C, b, b),
                                vpermute(Permute::Uzp1, Arr::S4, q, a, a),
                                vmull(false, Arr::S2, q, q, C),
                            ]);
                        } else {
                            self.asm
                                .emit_all(&[vmull(false, Arr::S2, C, a, b), vmov(false, q, C)]);
                        }
                    }
                    VecOp::SadBytes(full) => {
                        self.asm.emit_all(&[
                            vsame(Same::Uabd, Arr::new(0, full), C, a, b),
                            vaddlp(Arr::new(0, full), C, C),
                            vaddlp(Arr::new(1, full), C, C),
                            vaddlp(Arr::new(2, full), C, C),
                            vmov(full, q, C),
                        ]);
                    }
                    VecOp::MoveLow32 => {
                        if q == a {
                            self.asm.emit(vins(2, q, 0, b, 0));
                        } else if q != b {
                            self.asm.emit_all(&[vmov(true, q, a), vins(2, q, 0, b, 0)]);
                        } else {
                            self.asm.emit_all(&[
                                vmov(true, C, a),
                                vins(2, C, 0, b, 0),
                                vmov(true, q, C),
                            ]);
                        }
                    }
                    VecOp::Insert64(dst_lane, src_lane) => {
                        let (dl, sl) = (u32::from(dst_lane), u32::from(src_lane));
                        if q == a {
                            if q != b || dl != sl {
                                self.asm.emit(vins(3, q, dl, b, sl));
                            }
                        } else if q != b {
                            self.asm
                                .emit_all(&[vmov(true, q, a), vins(3, q, dl, b, sl)]);
                        } else {
                            self.asm.emit_all(&[
                                vmov(true, C, a),
                                vins(3, C, dl, b, sl),
                                vmov(true, q, C),
                            ]);
                        }
                    }
                    VecOp::InsertWord(lane, full) => {
                        self.asm.emit(vumov(1, T, b, 0));
                        if q != a {
                            self.asm.emit(vmov(true, q, a));
                        }
                        self.asm.emit(vins_general(1, q, u32::from(lane), T));
                        if !full {
                            self.asm.emit(vmov(false, q, q));
                        }
                    }
                    VecOp::LessF64 => {
                        let fail = self.fail();
                        fp_class::nan_operands(
                            &mut self.asm,
                            false,
                            true,
                            &[a, b],
                            T,
                            [C, D],
                            fail,
                        );
                        self.asm.emit(vcompare(Float::Fcmgt, false, true, q, b, a));
                    }
                }
            }
            Op::Load {
                ty,
                width,
                addr,
                checked,
                pc,
                clock,
            } => {
                self.guest_eip = pc;
                let addr = self.gp(addr, U);
                let base = self.data_view();
                let (word, reg) = if matches!(ty, Type::I32 | Type::I64) {
                    (
                        match width {
                            1 => ldr_narrow(Narrow::Byte, d, base, addr),
                            2 => ldr_narrow(Narrow::Half, d, base, addr),
                            _ => ldr_reg(if width == 8 { X } else { W }, d, base, addr, false),
                        },
                        AccessReg::Scalar(d),
                    )
                } else {
                    let size = mem_size(width);
                    (vldr_reg(size, q, base, addr), AccessReg::Vector(size, q))
                };
                self.memory_access(addr, width, reg, word, false, checked);
                if clock {
                    self.asm.emit_all(&[
                        ldr(X, U, RT, runtime::CPU_STATE),
                        str(X, RT, U, runtime::CLOCK_READ),
                    ]);
                }
            }
            Op::Host(addr) => {
                let addr = self.gp(addr, U);
                self.asm
                    .emit(add_ext(X, d, self.data_view(), addr, Extend::Uxtw, 0));
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
                checked,
                pc,
            } => {
                self.guest_eip = pc;
                let addr = self.gp(addr, U);
                let base = self.data_view();
                let (word, reg) = if matches!(self.func.ty(value), Type::I32 | Type::I64) {
                    let src = self.gp(value, T);
                    (
                        match width {
                            1 => str_narrow(Narrow::Byte, src, base, addr),
                            2 => str_narrow(Narrow::Half, src, base, addr),
                            _ => str_reg(if width == 8 { X } else { W }, src, base, addr, false),
                        },
                        AccessReg::Scalar(src),
                    )
                } else {
                    let src = self.vr(value, A);
                    let size = mem_size(width);
                    (
                        vstr_reg(size, src, base, addr),
                        AccessReg::Vector(size, src),
                    )
                };
                self.memory_access(addr, width, reg, word, true, checked);
                self.region = after;
                self.mark(Part::Body, self.func.records[after.0 as usize].eip);
            }
            Op::X87Load {
                bits,
                denormal,
                single,
            } => {
                let fail = self.fail();
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
                if denormal {
                    self.asm.cbz(W, true, T, done);
                    self.asm.emit(lsl_imm(size, T, r, 1));
                    self.asm.cbz(size, true, T, fail);
                }
                self.asm.b(done);
                self.asm.bind(nan);
                self.asm.emit(lsl_imm(size, T, r, high));
                self.asm.cbz(size, true, T, fail);
                self.asm.bind(done);
                self.asm.emit(fmov_to_v(size, q, r));
                if single {
                    self.asm.emit(unary(Unary::Widen, q, q));
                }
            }
            Op::SseInput {
                a,
                b,
                packed,
                single,
                pc,
            } => {
                self.guest_eip = pc;
                let a = self.vr(a, A);
                let b = b.map(|b| self.vr(b, B));
                let fail = self.fail();
                if packed {
                    fp_class::packed_denormals(
                        &mut self.asm,
                        Arr::D2,
                        true,
                        [Some(a), b],
                        T,
                        [D, E, C, F],
                    );
                    fp_class::any_lane(&mut self.asm, C, T, fail);
                } else {
                    for input in [Some(a), b].into_iter().flatten() {
                        fp_class::scalar_denormal(
                            &mut self.asm,
                            !single,
                            input,
                            T,
                            U,
                            Some(fail),
                            Reg::ZR,
                        );
                    }
                }
            }
            Op::Validate => {}
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
        IntOp::Shl | IntOp::Shr | IntOp::Sar | IntOp::Ror => {
            let n = (n & if size == X { 63 } else { 31 }) as u32;
            match op {
                IntOp::Shl => lsl_imm(size, d, a, n),
                IntOp::Shr => lsr_imm(size, d, a, n),
                IntOp::Sar => asr_imm(size, d, a, n),
                _ => ror_imm(size, d, a, n),
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
