//! Canonical Tier-1 edges for register-allocated forms.

use super::super::recovery::{self, IrRecord, Location, Recipe, X87};
use super::*;
use crate::translate::emit_link_point;

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
const ENTRY: u32 = offset_of!(Runtime, opt_entry) as u32;
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
        assumed: &Assumed,
        entry: &[Source],
        bytes: u64,
        miss: Label,
        cold: Label,
        delay_frames: bool,
    ) {
        self.asm
            .emit_all(&[ldr(X, CPU, RT, CPU_PTR), ldr(W, T, CPU, runtime::MMX_MODE)]);
        self.asm.cbz(W, true, T, cold);
        self.asm.emit(ldr(W, T, RT, REP_PENDING));
        self.asm.cbz(W, true, T, cold);
        self.entry_facts(assumed, miss);

        self.stack_frame(bytes, true);
        if entry.iter().any(|s| matches!(s, Source::X87(_))) {
            let inexact = self.asm.label();
            let admitted = self.asm.label();
            self.asm.emit(mrs_nzcv(S5));
            for (&source, &param) in entry.iter().zip(&self.func.blocks[0].params) {
                if let Source::X87(i) = source {
                    self.binary64(i, inexact);
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
            runtime::emit_read_flags(&mut self.asm);
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
    pub(super) fn environment_misses(&mut self, bytes: u64, miss: Label) {
        let facts = std::mem::take(&mut self.fact_misses);
        if facts.is_empty() {
            return;
        }
        let restore = self.asm.label();
        for (ordinal, label) in facts.into_iter().enumerate() {
            self.asm.bind(label);
            self.imm(T, ordinal as u64);
            self.asm
                .emit(str(W, T, RT, offset_of!(Runtime, resident_fact) as u32));
            self.asm.b(restore);
        }
        self.asm.bind(restore);
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
        self.asm.b(miss);
    }

    fn entry_facts(&mut self, assumed: &Assumed, miss: Label) {
        if assumed.x87 {
            for (field, value) in [
                (runtime::X87_CONTROL, assumed.control),
                (
                    runtime::X87_STATUS,
                    assumed.status | u16::from(assumed.top) << 11,
                ),
                (runtime::X87_TAG, assumed.tags),
            ] {
                self.asm.emit(ldrh(T, CPU, field));
                self.imm(U, u64::from(value));
                self.asm.emit(eor(W, T, T, U, Lsl, 0));
                self.asm.cbz(W, true, T, miss);
            }
        }
        if assumed.mmx {
            self.asm.emit(ldrh(T, CPU, runtime::X87_STATUS));
            self.asm.tbz(true, T, 7, miss);
        }
        if assumed.mxcsr_mask != 0 {
            self.asm.emit(ldr(W, T, CPU, runtime::MXCSR));
            self.imm(U, u64::from(assumed.mxcsr & assumed.mxcsr_mask));
            self.asm.emit(eor(W, T, T, U, Lsl, 0));
            if assumed.mxcsr_mask != u32::MAX {
                if bitmask(W, u64::from(assumed.mxcsr_mask)).is_some() {
                    self.asm
                        .emit(and_imm(W, T, T, u64::from(assumed.mxcsr_mask)));
                } else {
                    self.imm(U, u64::from(assumed.mxcsr_mask));
                    self.asm.emit(and(W, T, T, U, Lsl, 0));
                }
            }
            self.asm.cbz(W, true, T, miss);
        }
        if let Some(low) = assumed.esp_low {
            self.asm.emit(and_imm(W, T, ESP, 15));
            self.imm(U, u64::from(low));
            self.asm.emit(eor(W, T, T, U, Lsl, 0));
            self.asm.cbz(W, true, T, miss);
        }
        if let Some(offset) = assumed.ebp {
            self.asm.emit(sub(W, T, guest(5), ESP, Lsl, 0));
            self.imm(U, u64::from(offset as u32));
            self.asm.emit(eor(W, T, T, U, Lsl, 0));
            self.asm.cbz(W, true, T, miss);
        }
    }

    /// Converts the physical x87 input to binary64 bits in T.
    fn binary64(&mut self, i: u8, miss: Label) {
        let zero = self.asm.label();
        let special = self.asm.label();
        let tiny = self.asm.label();
        let done = self.asm.label();
        self.asm.emit_all(&[
            add_imm(X, S4, CPU, runtime::X87_REGS, false),
            ldur(X, T, S4, 10 * i32::from(i)),
            ldrh(S2, S4, 10 * u32::from(i) + 8),
            ubfx(W, S3, S2, 0, 15),
        ]);
        self.asm.cbz(X, false, T, zero);
        self.asm.tbz(false, T, 63, miss);
        self.imm(U, 0x7fff);
        self.asm.emit(cmp(W, S3, U, Lsl, 0));
        self.asm.b_cond(Cond::Eq, special);

        // Biased binary64 exponent. Subnormals use a variable right shift.
        self.imm(U, 16383 - 1023);
        self.asm.emit(sub(W, S3, S3, U, Lsl, 0));
        self.asm.emit(cmp_imm(W, S3, 0, false));
        self.asm.b_cond(Cond::Le, tiny);
        self.asm.emit(cmp_imm(W, S3, 0x7ff, false));
        self.asm.b_cond(Cond::Ge, miss);
        self.asm.emit(tst_imm(X, T, 0x7ff));
        self.asm.b_cond(Cond::Ne, miss);
        self.asm
            .emit_all(&[ubfx(X, T, T, 11, 52), orr(X, T, T, S3, Lsl, 52)]);
        self.asm.b(done);

        self.asm.bind(tiny);
        self.imm(U, 12);
        self.asm.emit(sub(W, S3, U, S3, Lsl, 0));
        self.asm.emit(cmp_imm(W, S3, 64, false));
        self.asm.b_cond(Cond::Hs, miss);
        self.asm.emit_all(&[
            shiftv(X, Lsr, U, T, S3),
            shiftv(X, Lsl, S4, U, S3),
            cmp(X, S4, T, Lsl, 0),
        ]);
        self.asm.b_cond(Cond::Ne, miss);
        self.asm.emit(mov(X, T, U));
        self.asm.b(done);

        self.asm.bind(special);
        self.imm(U, 1 << 63);
        self.asm.emit(cmp(X, T, U, Lsl, 0));
        self.asm.b_cond(Cond::Ne, miss);
        self.imm(T, 0x7ff << 52);
        self.asm.b(done);

        self.asm.bind(zero);
        self.asm.cbz(W, true, S3, miss);
        self.asm.bind(done);
        self.asm
            .emit_all(&[ubfx(W, S2, S2, 15, 1), orr(X, T, T, S2, Lsl, 63)]);
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

    pub(super) fn inline_exit(&mut self, ir: &IrRecord, assumed: &Assumed, bytes: u64) {
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
        runtime::emit_load_flags(&mut self.asm);
        self.charge(-i64::from(record.refund), RecordId(0), false);
        self.stack_frame(bytes, false);
        if let Some(probation) = &mut self.probation {
            probation.call(&mut self.asm, false);
            self.exit_words = self.exit_words.max(self.words_since(start));
        }
        self.links.push((self.asm.offset(), record.eip));
        emit_link_point(&mut self.asm, record.eip);
        self.asm.emit(0); // Bound to the Tier-1 dispatch stub on placement.
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
            ldrh(S5, CPU, runtime::X87_TAG),
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
            let value = if let Location::Recipe(Recipe::C1 { op, a, b }) = value {
                self.c1(op, a, b);
                T
            } else {
                self.gp_at(numeric(value), T)
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
    fn c1(&mut self, op: FpOp, a: Host, b: Host) {
        use Float::*;
        let a = self.v_at(a, A);
        let mut b = self.v_at(b, B);
        if op == FpOp::Sub {
            self.asm.emit(vfneg2d(B, b));
            b = B;
        }
        match op {
            FpOp::Add | FpOp::Sub => self.asm.emit_all(&[
                dfloat(Fadd, C, a, b),
                dfloat(Fsub, D, C, a),
                dfloat(Fsub, E, C, D),
                dfloat(Fsub, E, a, E),
                dfloat(Fsub, D, b, D),
                dfloat(Fadd, D, E, D),
            ]),
            FpOp::Mul => self
                .asm
                .emit_all(&[dfloat(Fmul, C, a, b), fnmsub(D, a, b, C)]),
            FpOp::Div => self
                .asm
                .emit_all(&[dfloat(Fdiv, C, a, b), fmsub(D, C, b, a)]),
        }
        self.asm.emit_all(&[
            fcmp_zero(D),
            fmov_from_v(X, T, D),
            fmov_from_v(X, U, C),
            eor(X, T, T, U, Lsl, 0),
        ]);
        if op == FpOp::Div {
            self.asm
                .emit_all(&[fmov_from_v(X, U, b), eor(X, T, T, U, Lsl, 0)]);
        }
        self.asm
            .emit_all(&[lsr_imm(X, T, T, 63), csel(W, T, Reg::ZR, T, Cond::Eq)]);
    }
}

fn guest(i: u8) -> Reg {
    Reg::new(19 + i)
}

fn numeric(location: Location<Host>) -> Host {
    match location {
        Location::At(host) | Location::Recipe(Recipe::Eflags(host)) => host,
        Location::Constant(bits) => Host::Constant(u128::from(bits)),
        Location::Recipe(Recipe::C1 { .. }) => unreachable!("C1 has its own emission"),
    }
}
