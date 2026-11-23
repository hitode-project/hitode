//! Canonical Tier-1 edges for register-allocated forms.

use super::super::recovery::{
    self, FlagExpr, FlagGroup, FlagSelect, IrRecord, Location, Recipe, X87,
};
use super::{
    A, ALIAS, Assumed, B, C, Cond, D, E, Emitter, Extend, FlagKind, FlagRecipe, Float, FpOp, Host,
    Index, IntOp, Label, Lsl, Narrow, Op, RT, Record, RecordId, Reg, Runtime, Source, T, Type, U,
    Unary, VMem, Value, W, WINDOW, X, add, add_ext, add_imm, and, and_imm, bfi, bic, clz, cmp,
    cmp_imm, csel, cset, dfloat, eor, fcmp, fcmp_zero, fmov_from_v, fmsub, fnmsub, int_immediate,
    ldp, ldr, ldr_narrow, ldr_reg, ldrh, ldur, lsl_imm, lsr_imm, mov, movz, mrs_nzcv, msr_nzcv,
    offset_of, orr, orr_imm, runtime, shiftv, stp, str, str_narrow, str_reg, strh, stur, sub, ubfx,
    unary, vfneg2d, vldr, vldur, vstur,
};
use crate::translate::lower::stubs;
use crate::translate::lower::{emit_link_point, emit_link_transition};

const CPU: Reg = Reg::new(0);
const FLAGS: Reg = Reg::new(1);
const S2: Reg = Reg::new(2);
const S3: Reg = Reg::new(3);
const S4: Reg = Reg::new(4);
const S5: Reg = Reg::new(5);
const ESP: Reg = Reg::new(23);
const PF: Reg = Reg::new(10);
const AF: Reg = Reg::new(11);
const DF: Reg = Reg::new(12);
const ENTRY: u32 = offset_of!(Runtime, recovery.entry) as u32;
const ENTRY_REGS: u32 = offset_of!(runtime::OptEntryState, regs) as u32;
const ENTRY_NZCV: i32 = offset_of!(runtime::OptEntryState, nzcv) as i32;
const ENTRY_AF: i32 = offset_of!(runtime::OptEntryState, af) as i32;
const REP_PENDING: u32 = offset_of!(Runtime, rep_pending) as u32;
const CPU_PTR: u32 = offset_of!(Runtime, cpu) as u32;
const SHADOW: u32 = offset_of!(Runtime, shadow) as u32;
const WINDOW_BASE: u32 = offset_of!(Runtime, window) as u32;

impl Emitter<'_> {
    pub(super) fn inline_entry(
        &mut self,
        key: crate::facts::Key,
        entry: &[Source],
        bytes: u64,
        miss: Label,
        cold: Label,
        delay_frames: bool,
    ) {
        self.asm.emit(ldr(X, CPU, RT, CPU_PTR));
        key.guard_mmx(
            &mut self.asm,
            CPU,
            true,
            if self.resident {
                self.mmx_miss.unwrap_or(miss)
            } else {
                cold
            },
        );
        self.asm.emit(ldr(W, T, RT, REP_PENDING));
        self.asm.cbz(W, true, T, cold);
        key.guard_resident(&mut self.asm, CPU, miss);
        self.body = self.asm.offset();
        if let Some((domain, offer)) = self.domain {
            let prices = (self.asm.label(), self.asm.label());
            let head = self.func.records[0].eip;
            if domain.target().is_none() {
                self.count(head, super::Counter::RowArrival);
            }
            let short = if self.count_entries {
                self.asm.label()
            } else {
                cold
            };
            use crate::translate::lower::resident::RowFailure;
            let failures = [
                RowFailure::Range,
                RowFailure::Alias,
                RowFailure::Parameter,
                RowFailure::Count,
                RowFailure::Span,
            ]
            .map(|kind| (kind, self.asm.label()));
            self.row_failures.extend(failures);
            let leave = offer.then(|| self.asm.label());
            let strip = self.row_strip.unwrap();
            let (mut words, bias, sample) = domain.emit(
                &mut self.asm,
                (
                    prices,
                    strip.count,
                    strip.shift,
                    strip.shift == 0 && domain.call_savings() >= strip.saved,
                ),
                short,
                failures,
                leave,
            );
            if self.count_entries {
                let hit = self.asm.label();
                self.asm.b(hit);
                self.asm.bind(short);
                self.count(head, super::Counter::RowShort);
                self.asm.b(cold);
                self.asm.bind(hit);
            }
            if let Some(leave) = leave {
                let (sensor, threshold) = (self.asm.label(), self.asm.label());
                self.asm.bind(sensor);
                crate::translate::lower::resident::split_sensor(
                    &mut self.asm,
                    Reg::new(7),
                    threshold,
                    leave,
                );
                self.split = Some((sensor, threshold, leave));
                words += 3;
            }
            self.count(head, super::Counter::RowEntry);
            if let Some(target) = domain.target() {
                self.count(target.value, super::Counter::RowEntry);
            }
            self.row_budget = Some((prices, bias, words));
            self.row_sample = Some(sample);
            self.asm.emit(ldr(X, CPU, RT, CPU_PTR));
        }

        self.stack_frame(bytes, true);
        if entry.iter().any(|s| matches!(s, Source::X87(_))) {
            let inexact = self.asm.label();
            let admitted = self.asm.label();
            self.asm.emit(mrs_nzcv(S5));
            for (&source, &param) in entry.iter().zip(&self.func.blocks[0].params) {
                if let Source::X87(i) = source {
                    crate::facts::guard_binary64(&mut self.asm, i, inexact);
                    self.copy(
                        self.locations[param.0 as usize],
                        Host::Register(T),
                        Type::F64,
                    );
                }
            }
            self.asm.b(admitted);
            self.asm.bind(inexact);
            self.stack_frame(bytes, false);
            self.asm.emit(msr_nzcv(S5));
            self.asm.b(miss);
            self.asm.bind(admitted);
            self.asm.emit(msr_nzcv(S5));
        }

        self.save_entry_state();
        if entry.contains(&Source::Eflags) {
            stubs::emit_read_flags(&mut self.asm, Reg::new(0));
            self.asm.emit(mov(W, FLAGS, S4));
        }
        if !self.resident {
            self.asm
                .emit_all(&[str(X, ALIAS, RT, SHADOW), ldr(X, ALIAS, RT, runtime::ALIAS)]);
        }
        let mut moves = Vec::new();
        for (&source, &param) in entry.iter().zip(&self.func.blocks[0].params) {
            if let Source::Reg(i) = source {
                let src = guest(i);
                let dst = self.locations[param.0 as usize];
                if dst == Host::Register(src) {
                    self.asm.emit(mov(W, src, src));
                } else {
                    moves.push((dst, Host::Register(src), Type::I32));
                }
            }
        }
        self.parallel_moves(moves);

        for (&source, &param) in entry.iter().zip(&self.func.blocks[0].params) {
            let dst = self.locations[param.0 as usize];
            let ty = self.func.ty(param);
            let src = match source {
                Source::Reg(_) | Source::X87(_) | Source::Frame { .. } => continue,
                Source::Eflags => Host::Register(FLAGS),
                Source::Xmm(i) => {
                    let out = self.vd(param);
                    self.asm.emit(vldur(
                        VMem::Q,
                        out,
                        CPU,
                        runtime::XMM as i32 + 16 * i32::from(i),
                    ));
                    Host::Vector(out)
                }
                Source::Mmx(i) => {
                    let out = self.vd(param);
                    self.asm
                        .emit(vldr(VMem::D, out, CPU, runtime::MMX + 8 * u32::from(i)));
                    Host::Vector(out)
                }
                Source::FsBase => {
                    let out = self.rd(param);
                    self.asm.emit(ldr(W, out, CPU, runtime::FS_BASE));
                    Host::Register(out)
                }
                Source::Mxcsr => {
                    let out = self.rd(param);
                    self.asm.emit(ldr(W, out, CPU, runtime::MXCSR));
                    Host::Register(out)
                }
                Source::X87Ip => {
                    let out = self.rd(param);
                    self.asm.emit(ldr(W, out, CPU, runtime::X87_IP));
                    Host::Register(out)
                }
                Source::X87Opcode => {
                    let out = self.rd(param);
                    self.asm.emit(ldrh(out, CPU, runtime::X87_OPCODE));
                    Host::Register(out)
                }
            };
            self.copy(dst, src, ty);
        }
        if !delay_frames {
            self.entry_frames(entry);
        }
    }

    pub(super) fn entry_frames(&mut self, entry: &[Source]) {
        for (&source, &param) in entry.iter().zip(&self.func.blocks[0].params) {
            if let Source::Frame { offset, width } = source {
                let out = self.rd(param);
                self.frame_cell(true, out, offset, width);
                self.copy(
                    self.locations[param.0 as usize],
                    Host::Register(out),
                    source.ty(),
                );
            }
        }
    }

    /// Leave an entry whose page facts failed in the Tier-1 assignment.
    pub(super) fn environment_misses(&mut self, bytes: u64, miss: Label, fallback: Label) {
        let facts = std::mem::take(&mut self.fact_misses);
        if facts.is_empty() {
            return;
        }
        let restore = self.asm.label();
        let triggered = self.asm.label();
        for (ordinal, label) in facts.into_iter().enumerate() {
            let counter = self.asm.label();
            let report = self.asm.label();
            self.asm.bind(label);
            self.environment.push((self.asm.offset(), counter));
            runtime::emit_sensor(&mut self.asm, counter, S2, S3, report);
            self.asm.b(restore);
            self.asm.bind(report);
            self.imm(S3, ordinal as u64);
            self.asm.b(triggered);
            self.asm.bind(counter);
            self.asm.emit_all(&[0, 0]);
        }
        self.asm.bind(triggered);
        self.restore_environment(bytes, miss);
        self.asm.bind(restore);
        self.restore_environment(bytes, fallback);
    }

    /// Lower one `Block(0)` numerical domain guard predicate.
    pub(super) fn domain_guard(&mut self, kind: super::DomainGuardKind, ordinal: u8) {
        let miss = if self.resident {
            *self.domain_guard_misses[usize::from(ordinal)].get_or_insert_with(|| self.asm.label())
        } else {
            self.fail()
        };
        let start = self.asm.offset();
        match kind {
            super::DomainGuardKind::FpWindow {
                value,
                min_exp,
                max_exp,
                exact_f32,
            } => {
                let v = self.vr(value, A);
                let min_biased = (min_exp + 1023) as u32;
                let span = (max_exp - min_exp + 1) as u32;
                let ok = self.asm.label();
                self.asm
                    .emit_all(&[fmov_from_v(X, T, v), ubfx(X, U, T, 52, 11)]);
                if min_biased != 0 {
                    self.asm.emit(super::sub_imm(W, U, U, min_biased, false));
                }
                self.asm.emit(cmp_imm(W, U, span, false));
                self.asm.b_cond(Cond::Lo, ok);
                self.asm.emit(lsl_imm(X, T, T, 1));
                self.asm.cbz(X, true, T, miss);
                self.asm.bind(ok);
                if exact_f32 {
                    self.asm.emit_all(&[
                        unary(Unary::Narrow, C, v),
                        unary(Unary::Widen, C, C),
                        fcmp(v, C),
                    ]);
                    self.asm.b_cond(Cond::Ne, miss);
                }
            }
            super::DomainGuardKind::SameSign { a, b } => {
                let a = self.vr(a, A);
                let b = self.vr(b, B);
                let ok = self.asm.label();
                self.asm.emit(fcmp_zero(a));
                self.asm.b_cond(Cond::Eq, ok);
                self.asm.emit(fcmp_zero(b));
                self.asm.b_cond(Cond::Eq, ok);
                self.asm.emit_all(&[
                    fmov_from_v(X, T, a),
                    fmov_from_v(X, U, b),
                    eor(X, T, T, U, Lsl, 0),
                ]);
                self.asm.tbz(true, T, 63, miss);
                self.asm.bind(ok);
            }
            super::DomainGuardKind::FpLe { value, bound_bits } => {
                let v = self.vr(value, A);
                self.imm(T, bound_bits);
                self.asm.emit_all(&[super::fmov_to_v(X, C, T), fcmp(v, C)]);
                self.asm.b_cond(Cond::Hi, miss);
            }
            super::DomainGuardKind::U32Le { value, max } => {
                let r = self.gp(value, T);
                if let Some((imm, shift)) = super::add_immediate(u64::from(max)) {
                    self.asm.emit(cmp_imm(W, r, imm, shift));
                } else {
                    self.imm(U, u64::from(max));
                    self.asm.emit(cmp(W, r, U, Lsl, 0));
                }
                self.asm.b_cond(Cond::Hi, miss);
            }
            super::DomainGuardKind::ExactF32 { value } => {
                let v = self.vr(value, A);
                self.asm.emit_all(&[
                    unary(Unary::Narrow, C, v),
                    unary(Unary::Widen, C, C),
                    fcmp(v, C),
                ]);
                self.asm.b_cond(Cond::Ne, miss);
            }
        }
        self.domain_guard_words += self.words_since(start);
    }

    /// Leave an entry whose numerical domain guard failed before any guest side
    /// effect, restoring entry countdown and Tier-1 state.
    pub(super) fn domain_misses_tail(
        &mut self,
        bytes: u64,
        refund: u32,
        miss: Label,
        fallback: Label,
    ) {
        let guards = std::mem::take(&mut self.domain_guard_misses);
        if guards.iter().all(Option::is_none) {
            return;
        }
        let restore = self.asm.label();
        let triggered = self.asm.label();
        let misses_ptr = self.asm.label();
        self.domain_misses_slot = Some(misses_ptr);
        for (ordinal, label) in guards.into_iter().enumerate() {
            let Some(label) = label else {
                continue;
            };
            let counter = self.asm.label();
            let report = self.asm.label();
            self.asm.bind(label);
            self.asm.ldr_literal(S2, misses_ptr);
            self.asm.emit_all(&[
                ldr(X, S3, S2, 0),
                add_imm(X, S3, S3, 1, false),
                str(X, S3, S2, 0),
            ]);
            self.domain_sensors
                .push((ordinal as u8, self.asm.offset(), counter));
            runtime::emit_sensor(&mut self.asm, counter, S2, S3, report);
            self.asm.b(restore);
            self.asm.bind(report);
            self.imm(S3, ordinal as u64);
            self.asm.b(triggered);
            self.asm.bind(counter);
            self.asm.emit_all(&[0, 0]);
        }
        self.asm.bind(misses_ptr);
        self.asm.emit_all(&[0, 0]);
        self.asm.bind(triggered);
        self.refund_entry_count(refund);
        self.restore_environment(bytes, miss);
        self.asm.bind(restore);
        self.refund_entry_count(refund);
        self.restore_environment(bytes, fallback);
    }

    fn refund_entry_count(&mut self, refund: u32) {
        if refund == 0 {
            return;
        }
        if let Some((imm, shift)) = super::add_immediate(u64::from(refund)) {
            self.asm
                .emit(add_imm(X, super::COUNT, super::COUNT, imm, shift));
        } else {
            self.imm(T, u64::from(refund));
            self.asm.emit(add(X, super::COUNT, super::COUNT, T, Lsl, 0));
        }
    }

    fn restore_environment(&mut self, bytes: u64, target: Label) {
        self.asm.emit(add_imm(X, S4, RT, ENTRY, false));
        for i in (0..8).step_by(2) {
            self.asm.emit(ldp(
                W,
                Index::Offset,
                guest(i),
                guest(i + 1),
                S4,
                ENTRY_REGS as i32 + 4 * i32::from(i),
            ));
        }
        self.asm.emit_all(&[
            ldp(X, Index::Offset, T, PF, S4, ENTRY_NZCV),
            ldp(X, Index::Offset, AF, DF, S4, ENTRY_AF),
            msr_nzcv(T),
        ]);
        self.bias_stack();
        self.stack_frame(bytes, false);
        self.asm.b(target);
    }

    /// Keeps the entry baseline in the runtime for cold recovery and frame access.
    fn save_entry_state(&mut self) {
        self.asm.emit(add_imm(X, S4, RT, ENTRY, false));
        for i in (0..8).step_by(2) {
            self.asm.emit(stp(
                W,
                Index::Offset,
                guest(i),
                guest(i + 1),
                S4,
                ENTRY_REGS as i32 + 4 * i32::from(i),
            ));
        }
        self.asm.emit_all(&[
            mrs_nzcv(T),
            stp(X, Index::Offset, T, PF, S4, ENTRY_NZCV),
            stp(X, Index::Offset, AF, DF, S4, ENTRY_AF),
        ]);
    }

    /// Reads or writes the little-endian bytes of a frame cell.
    fn frame_cell(&mut self, load: bool, reg: Reg, offset: i32, width: u8) {
        self.asm.emit(ldr(W, S2, RT, ENTRY + ENTRY_REGS + 4 * 4));
        if offset != 0 {
            self.imm(U, u64::from(offset as u32));
            self.asm.emit(add(W, S2, S2, U, Lsl, 0));
        }
        let base = self.data_view();
        let mut consumed = 0;
        for chunk in [8, 4, 2, 1].into_iter().filter(|chunk| width & chunk != 0) {
            let value = if consumed == 0 { reg } else { S3 };
            if !load && consumed != 0 {
                self.asm.emit(lsr_imm(X, value, reg, consumed * 8));
            }
            self.asm.emit(match (load, chunk) {
                (true, 1) => ldr_narrow(Narrow::Byte, value, base, S2),
                (true, 2) => ldr_narrow(Narrow::Half, value, base, S2),
                (true, 4) => ldr_reg(W, value, base, S2, false),
                (true, 8) => ldr_reg(X, value, base, S2, false),
                (false, 1) => str_narrow(Narrow::Byte, value, base, S2),
                (false, 2) => str_narrow(Narrow::Half, value, base, S2),
                (false, 4) => str_reg(W, value, base, S2, false),
                (false, 8) => str_reg(X, value, base, S2, false),
                _ => unreachable!(),
            });
            if load && consumed != 0 {
                self.asm.emit(orr(X, reg, reg, value, Lsl, consumed * 8));
            }
            consumed += u32::from(chunk);
            if consumed < u32::from(width) {
                self.asm.emit(add_imm(W, S2, S2, u32::from(chunk), false));
            }
        }
    }

    pub(super) fn inline_exit(
        &mut self,
        ir: &IrRecord,
        assumed: &Assumed,
        bytes: u64,
        arrival: bool,
    ) {
        let start = self.asm.offset();
        let record = recovery::lower_record(ir, &self.locations);
        self.asm.emit(ldr(X, CPU, RT, CPU_PTR));
        for &(offset, width, value) in &record.frame {
            let src = self.gp_at(numeric(value), T);
            self.frame_cell(false, src, offset, width);
        }
        if let Some(x87) = &record.x87 {
            self.write_x87(x87);
        }
        for (i, value) in record.xmm.into_iter().enumerate() {
            if let Some(value) = value {
                let v = self.v_at(numeric(value), A);
                self.asm
                    .emit(vstur(VMem::Q, v, CPU, runtime::XMM as i32 + 16 * i as i32));
            }
        }
        self.write_mmx(&record, assumed.mmx);
        // The optimized IR keeps MXCSR sticky bits in CpuState. An SSE
        // arithmetic form admits only with precision already set.

        self.exit_flags(
            numeric(record.eflags.expect("normal exit has EFLAGS")),
            self.keeps_unmapped_flags(ir.eflags.expect("normal exit has EFLAGS")),
        );
        for group in &record.flag_groups {
            self.exit_flag_group(group);
        }
        self.parallel_moves(
            record
                .regs
                .into_iter()
                .enumerate()
                .filter_map(|(i, value)| {
                    Some((Host::Register(guest(i as u8)), numeric(value?), Type::I32))
                })
                .collect(),
        );
        for (i, value) in record.regs.iter().enumerate() {
            if value.is_none() {
                self.asm.emit(ldr(
                    W,
                    guest(i as u8),
                    RT,
                    ENTRY + ENTRY_REGS + 4 * i as u32,
                ));
            }
        }
        if !self.resident {
            self.asm
                .emit_all(&[ldr(X, ALIAS, RT, SHADOW), ldr(X, WINDOW, RT, WINDOW_BASE)]);
        }
        self.bias_stack();
        stubs::emit_load_flags(&mut self.asm);
        self.charge(-i64::from(record.refund), RecordId(0), false);
        self.stack_frame(bytes, false);
        let settled_exit_words = self.words_since(start) + 1;
        if let Some(probation) = &mut self.probation {
            // A settled exit branches over both the sensor and the tally call.
            self.exit_words = self.exit_words.max(settled_exit_words);
            let rows = self.domain.and_then(|(domain, _)| {
                if record.eip == self.func.records[0].eip {
                    Some(true)
                } else {
                    domain.completes_rows_at(record.eip).then_some(false)
                }
            });
            probation.exit(&mut self.asm, record.eip, rows);
        }
        let point = self.asm.offset();
        self.links.push((point, record.eip));
        emit_link_point(&mut self.asm, record.eip, false);
        self.asm.emit(0); // Bound to the Tier-1 dispatch stub on placement.
        if arrival {
            let transition = emit_link_transition(&mut self.asm, point);
            self.transitions.push((point, transition));
        }
    }

    fn bias_stack(&mut self) {
        self.asm.emit_all(&[
            add_ext(X, ESP, WINDOW, ESP, Extend::Uxtw, 0),
            add_ext(X, guest(5), WINDOW, guest(5), Extend::Uxtw, 0),
        ]);
    }

    /// Flags computed from the entry value preserve every unmapped bit.
    fn keeps_unmapped_flags(&self, flags: Location<Value>) -> bool {
        let mut base = match flags {
            Location::At(v) | Location::Recipe(Recipe::Eflags(v)) => Some(v),
            _ => None,
        };
        while let Some(Op::Flags { prior, .. }) = base.and_then(|v| self.defs[v.0 as usize]) {
            base = prior;
        }
        self.func
            .entry
            .iter()
            .zip(&self.func.blocks[0].params)
            .any(|(&source, &value)| source == Source::Eflags && base == Some(value))
    }

    fn exit_flags(&mut self, flags: Host, keeps_unmapped: bool) {
        if let Host::Flags(_) = flags {
            let prior = recovery::flag_base(flags, &self.flags);
            let r = self.gp_at(prior, T);
            if keeps_unmapped {
                self.asm.emit(and_imm(W, FLAGS, r, 1 << 10));
            } else {
                self.imm(U, u64::from(runtime::MAPPED_FLAGS & !(1 << 10)));
                self.asm.emit(bic(W, FLAGS, r, U, Lsl, 0));
            }
            for bit in [0, 2, 4, 6, 7, 11] {
                self.bit(flags, bit);
                self.asm.emit(orr(W, FLAGS, FLAGS, T, Lsl, bit));
            }
        } else {
            self.copy(Host::Register(FLAGS), flags, Type::I32);
        }
        if !keeps_unmapped {
            self.asm.emit_all(&[
                ldr(W, T, CPU, runtime::EFLAGS),
                movz(W, U, runtime::MAPPED_FLAGS as u16, 0),
                and(W, T, T, U, Lsl, 0),
                bic(W, U, FLAGS, U, Lsl, 0),
                orr(W, T, T, U, Lsl, 0),
                str(W, T, CPU, runtime::EFLAGS),
            ]);
        }
    }

    fn exit_flag_group(&mut self, group: &FlagGroup<Host>) {
        let done = self.asm.label();
        let mut arms = group.arms.iter().peekable();
        while let Some((tag, expr)) = arms.next() {
            let next = self.asm.label();
            if arms.peek().is_some() {
                match group.select {
                    FlagSelect::Same => unreachable!(),
                    FlagSelect::Tag(v) => {
                        let tag_reg = self.gp_at(v, T);
                        self.imm(U, u64::from(*tag));
                        self.asm.emit(cmp(W, tag_reg, U, Lsl, 0));
                    }
                    FlagSelect::Induction { current, initial } => {
                        let current = self.gp_at(current, T);
                        let initial = self.gp_at(initial, U);
                        self.asm.emit(cmp(W, current, initial, Lsl, 0));
                    }
                }
                self.asm.b_cond(Cond::Ne, next);
            }
            for bit in [0, 2, 4, 6, 7, 11] {
                if group.mask & (1 << bit) != 0 {
                    self.exit_flag_bit(expr, bit);
                    self.asm.emit(bfi(W, FLAGS, T, bit, 1));
                }
            }
            if arms.peek().is_some() {
                self.asm.b(done);
            }
            self.asm.bind(next);
        }
        self.asm.bind(done);
    }

    fn exit_flag_bit(&mut self, expr: &FlagExpr<Host>, bit: u32) {
        let FlagExpr::Produce {
            kind,
            width,
            a,
            b,
            prior,
            adjust,
        } = expr
        else {
            self.bit(flag_operand(expr), bit);
            return;
        };
        let prior = if **prior == FlagExpr::Constant(0) {
            None
        } else {
            let bit = if matches!(kind, FlagKind::AddCarry | FlagKind::SubBorrow) {
                0
            } else {
                bit
            };
            self.exit_flag_bit(prior, bit);
            self.asm.emit(lsl_imm(W, S2, T, bit));
            Some(Host::Register(S2))
        };
        let mut a = flag_operand(a);
        if *adjust != 0 {
            let src = self.gp_at(a, T);
            if let Some(word) = int_immediate(IntOp::Add, W, S3, src, u64::from(*adjust)) {
                self.asm.emit(word);
            } else {
                self.imm(U, u64::from(*adjust));
                self.asm.emit(add(W, S3, src, U, Lsl, 0));
            }
            a = Host::Register(S3);
        }
        let b = flag_operand(b);
        let flags = Host::Flags(self.flags.len() as u32);
        self.flags.push(Some(FlagRecipe {
            kind: *kind,
            width: *width,
            a,
            b,
            prior,
            immediate: match b {
                Host::Constant(n) => Some(n as u64),
                _ => None,
            },
        }));
        self.bit(flags, bit);
        self.flags.pop();
    }

    fn write_mmx(&mut self, record: &Record<Host>, refresh: bool) {
        if record.mmx_meta.is_none() && record.mmx.iter().all(Option::is_none) && !refresh {
            return;
        }
        let meta = record.mmx_meta.map_or(Host::Constant(0), numeric);
        self.copy(Host::Register(S5), meta, Type::I32);
        self.asm.emit(add_imm(X, S4, CPU, runtime::X87_REGS, false));
        for (i, value) in record.mmx.into_iter().enumerate() {
            let Some(value) = value else { continue };
            let src = self.gp_at(numeric(value), T);
            if !refresh {
                self.asm.emit(str(X, src, CPU, runtime::MMX + 8 * i as u32));
            }
            let skip = self.asm.label();
            self.asm.tbz(false, S5, i as u32, skip);
            self.asm.emit_all(&[
                stur(X, src, S4, 10 * i as i32),
                movz(W, U, 0xffff, 0),
                strh(U, S4, 10 * i as u32 + 8),
            ]);
            self.asm.bind(skip);
        }
        if record.mmx_meta.is_some() {
            let keep_top = self.asm.label();
            self.asm.tbz(false, S5, 10, keep_top);
            self.asm.emit_all(&[
                ldrh(T, CPU, runtime::X87_STATUS),
                and_imm(W, T, T, u64::from(!0x3800u32)),
                strh(T, CPU, runtime::X87_STATUS),
            ]);
            self.asm.bind(keep_top);
            let tag_empty = self.asm.label();
            let tags_done = self.asm.label();
            self.asm.emit(ubfx(W, T, S5, 8, 2));
            self.asm.emit(cmp_imm(W, T, 1, false));
            self.asm.b_cond(Cond::Ne, tag_empty);
            self.asm.emit(strh(Reg::ZR, CPU, runtime::X87_TAG));
            self.asm.b(tags_done);
            self.asm.bind(tag_empty);
            self.asm.emit(cmp_imm(W, T, 2, false));
            self.asm.b_cond(Cond::Ne, tags_done);
            self.asm
                .emit_all(&[movz(W, T, 0xffff, 0), strh(T, CPU, runtime::X87_TAG)]);
            self.asm.bind(tags_done);
        }
        if refresh {
            for i in 0..8u32 {
                self.asm.emit_all(&[
                    ldur(X, T, S4, 10 * i as i32),
                    str(X, T, CPU, runtime::MMX + 8 * i),
                ]);
            }
        }
    }

    fn write_x87(&mut self, x87: &X87<Host>) {
        self.asm.emit_all(&[
            add_imm(X, S4, CPU, runtime::X87_REGS, false),
            movz(W, S5, u16::MAX, 0),
        ]);
        for (i, value) in x87.stack.into_iter().enumerate() {
            if x87.valid >> i & 1 != 0
                && let Some(value) = value
            {
                let value = self.gp_at(numeric(value), T);
                self.binary80(i as u32, value);
                self.asm.emit(bfi(W, S5, U, 2 * i as u32, 2));
            }
        }
        self.asm.emit(strh(S5, CPU, runtime::X87_TAG));
        self.imm(T, u64::from(x87.control));
        self.asm.emit(strh(T, CPU, runtime::X87_CONTROL));
        self.imm(
            S5,
            u64::from(x87.status & !0x3a00 | u16::from(x87.top) << 11),
        );
        if let Some(value) = x87.c1 {
            let value = match value {
                Location::Recipe(Recipe::C1 { op, a, b, single }) => {
                    self.c1(op, a, b, single);
                    T
                }
                Location::Recipe(Recipe::C1Fma {
                    op,
                    a,
                    b,
                    c,
                    single,
                }) => {
                    self.c1_fma(op, a, b, c, single);
                    T
                }
                other => self.gp_at(numeric(other), T),
            };
            self.asm.emit(bfi(W, S5, value, 9, 1));
        }
        self.asm.emit(strh(S5, CPU, runtime::X87_STATUS));
        if let Some(ip) = x87.ip {
            let ip = self.gp_at(numeric(ip), T);
            self.asm.emit(str(W, ip, CPU, runtime::X87_IP));
        }
        if let Some(opcode) = x87.opcode {
            let opcode = self.gp_at(numeric(opcode), T);
            self.asm.emit(strh(opcode, CPU, runtime::X87_OPCODE));
        }
    }

    /// Stores the binary80 register and its MMX alias, returning its tag in U.
    fn binary80(&mut self, i: u32, value: Reg) {
        let small = self.asm.label();
        let special = self.asm.label();
        let zero = self.asm.label();
        let done = self.asm.label();
        self.asm.emit_all(&[
            ubfx(X, S3, value, 63, 1),
            ubfx(X, S2, value, 52, 11),
            ubfx(X, T, value, 0, 52),
        ]);
        self.asm.cbz(W, false, S2, small);
        self.asm.emit(cmp_imm(W, S2, 0x7ff, false));
        self.asm.b_cond(Cond::Eq, special);
        self.imm(U, 16383 - 1023);
        self.asm.emit(add(W, S2, S2, U, Lsl, 0));
        self.imm(U, 0);
        self.asm
            .emit_all(&[lsl_imm(X, T, T, 11), orr_imm(X, T, T, 1 << 63)]);
        self.asm.b(done);

        self.asm.bind(special);
        self.imm(S2, 0x7fff);
        self.imm(U, 2);
        self.asm
            .emit_all(&[lsl_imm(X, T, T, 11), orr_imm(X, T, T, 1 << 63)]);
        self.asm.b(done);

        self.asm.bind(small);
        self.asm.cbz(X, false, T, zero);
        self.asm.emit_all(&[clz(X, U, T), shiftv(X, Lsl, T, T, U)]);
        self.imm(S2, 16383 - 1074 + 63);
        self.asm.emit(sub(W, S2, S2, U, Lsl, 0));
        self.imm(U, 0);
        self.asm.b(done);
        self.asm.bind(zero);
        self.imm(U, 1);

        self.asm.bind(done);
        self.asm.emit_all(&[
            orr(W, S2, S2, S3, Lsl, 15),
            stur(X, T, S4, 10 * i as i32),
            strh(S2, S4, 10 * i + 8),
            str(X, T, CPU, runtime::MMX + 8 * i),
        ]);
    }

    /// Recomputes the rounding direction used by recovery::materialize.
    fn c1(&mut self, op: FpOp, a: Host, b: Host, single: bool) {
        use Float::*;
        let a = self.v_at(a, A);
        let mut b = self.v_at(b, B);
        if op == FpOp::Sub {
            self.asm.emit(vfneg2d(B, b));
            b = B;
        }
        match op {
            FpOp::Add | FpOp::Sub => {
                crate::translate::lower::fp_class::two_sum(&mut self.asm, a, b, C, D, E);
            }
            FpOp::Mul => self
                .asm
                .emit_all(&[dfloat(Fmul, C, a, b), fnmsub(D, a, b, C)]),
            FpOp::Div => self
                .asm
                .emit_all(&[dfloat(Fdiv, C, a, b), fmsub(D, C, b, a)]),
        }
        self.c1_finish(op == FpOp::Div, b, single);
    }

    /// Recomputes C1 for a contracted exact-product FMA (`a * b ± c` or `c - a * b`).
    fn c1_fma(&mut self, op: super::super::ir::FmaOp, a: Host, b: Host, c: Host, single: bool) {
        use super::super::ir::FmaOp;
        let va = self.v_at(a, A);
        let vb = self.v_at(b, B);
        self.asm.emit(dfloat(Float::Fmul, A, va, vb));
        let vc = self.v_at(c, B);
        match op {
            FmaOp::Add => {
                crate::translate::lower::fp_class::two_sum(&mut self.asm, A, vc, C, D, E);
            }
            FmaOp::Sub => {
                self.asm.emit(vfneg2d(B, vc));
                crate::translate::lower::fp_class::two_sum(&mut self.asm, A, B, C, D, E);
            }
            FmaOp::RevSub => {
                self.asm.emit(vfneg2d(A, A));
                crate::translate::lower::fp_class::two_sum(&mut self.asm, vc, A, C, D, E);
            }
        }
        self.c1_finish(false, B, single);
    }

    fn c1_finish(&mut self, is_div: bool, b: crate::encode::VReg, single: bool) {
        self.asm.emit_all(&[
            fcmp_zero(D),
            fmov_from_v(X, T, D),
            fmov_from_v(X, U, C),
            eor(X, T, T, U, Lsl, 0),
        ]);
        if is_div {
            self.asm
                .emit_all(&[fmov_from_v(X, U, b), eor(X, T, T, U, Lsl, 0)]);
        }
        self.asm
            .emit_all(&[lsr_imm(X, T, T, 63), csel(W, T, Reg::ZR, T, Cond::Eq)]);
        if single {
            self.asm.emit_all(&[
                unary(Unary::Narrow, E, C),
                unary(Unary::Widen, E, E),
                unary(Unary::Fabs, E, E),
                unary(Unary::Fabs, C, C),
                fcmp(E, C),
                cset(W, U, Cond::Gt),
                csel(W, T, T, U, Cond::Eq),
                fcmp_zero(C),
                csel(W, T, Reg::ZR, T, Cond::Eq),
            ]);
        }
    }
}

pub(super) fn guest(i: u8) -> Reg {
    Reg::new(19 + i)
}

fn numeric(location: Location<Host>) -> Host {
    match location {
        Location::At(host) | Location::Recipe(Recipe::Eflags(host)) => host,
        Location::Constant(bits) => Host::Constant(u128::from(bits)),
        Location::Recipe(Recipe::C1 { .. } | Recipe::C1Fma { .. }) => {
            unreachable!("C1 has its own emission")
        }
    }
}

fn flag_operand(expr: &FlagExpr<Host>) -> Host {
    match *expr {
        FlagExpr::At(v) => v,
        FlagExpr::Constant(bits) => Host::Constant(bits),
        FlagExpr::Produce { .. } => unreachable!("numeric flag operand"),
    }
}
