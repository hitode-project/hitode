//! Window checks and the owner access call for resident forms.

use super::{
    AccessReg, Asm, COUNT, CheckedEmitter, Cond, Emitter, Index, IntOp, Label, Lsl, Narrow, Part,
    RT, RecordId, Reg, ResidentCold, Runtime, SP, Site, T, U, VReg, VSize, Value, W, X, add,
    add_imm, add_immediate, blr, cmp, cmp_imm, fmov_to_v, int_immediate, ldp, ldp_v, ldr,
    ldr_narrow, lsr_imm, mov, mov_sp, movk, movz, mrs_nzcv, msr_nzcv, offset_of, page_crossing,
    ret, runtime, stp, stp_v, str, sub, sub_imm, vldr, vstr, vumov,
};
use crate::translate::lower::{X0, X1, X2, X3, X4, X5, fp_state};
use adv_cpu_core::memory::{LINEAR, READ, SHARED, WRITE};

const RESIDENT_ACCESS: u32 = offset_of!(Runtime, resident_access) as u32;

pub(super) struct CheckedAccess {
    address: Reg,
    value: AccessReg,
    kind: u32,
    site: Label,
    resume: Label,
    slow: Label,
    crossing: Option<Label>,
    region: RecordId,
    eip: u32,
}

pub(super) struct PageCrossing {
    address: Reg,
    width: u32,
    crossing: Label,
    direct: u32,
    fault: Site,
    resume: Label,
    fail: Label,
    eip: u32,
}

impl Emitter<'_> {
    pub(super) fn memory_access(
        &mut self,
        address: Reg,
        width: u8,
        value: AccessReg,
        direct: u32,
        store: bool,
        checked: bool,
    ) {
        let width = u32::from(width);
        let checked = self.resident && checked;
        let page_cross = self.page_cross.take();
        let crossing = (checked || page_cross.is_some()).then(|| self.asm.label());
        let site_index = self.sites.len();
        let resume = if checked {
            let slow = self.asm.label();
            let crossing = crossing.unwrap();
            let (site, resume) = self.checked_memory(address, width, store, direct, slow, crossing);
            let kind = width | if store { runtime::ACCESS_STORE } else { 0 };
            self.checked_accesses.push(CheckedAccess {
                address,
                value,
                kind,
                site,
                resume,
                slow,
                crossing: page_cross.is_none().then_some(crossing),
                region: self.region,
                eip: self.guest_eip,
            });
            Some(resume)
        } else {
            if let Some(crossing) = crossing {
                page_crossing(&mut self.asm, address, width, crossing);
            }
            self.site(false);
            self.asm.emit(direct);
            crossing.map(|_| {
                let label = self.asm.label();
                self.asm.bind(label);
                label
            })
        };
        if let Some(record) = page_cross {
            let fail = self.failure(record, ResidentCold::PageCross);
            self.page_crossings.push(PageCrossing {
                address,
                width,
                crossing: crossing.unwrap(),
                direct,
                fault: self.sites[site_index].1,
                resume: resume.unwrap(),
                fail,
                eip: self.guest_eip,
            });
        }
    }

    pub(super) fn page_crossings(&mut self, count: bool) {
        let counter = offset_of!(Runtime, resident_crossing_counter) as u32;
        for cross in std::mem::take(&mut self.page_crossings) {
            self.asm.bind(cross.crossing);
            self.mark(Part::Cold, cross.eip);
            let blocked = self.asm.label();
            // Preserve the flags and any scalar store value reloaded into T.
            self.asm
                .emit_all(&[mrs_nzcv(X5), stp(X, Index::Pre, T, X5, SP, -16)]);
            crate::facts::plain_span(&mut self.asm, cross.address, cross.width, true, blocked);
            self.asm
                .emit_all(&[ldp(X, Index::Post, T, X5, SP, 16), msr_nzcv(X5)]);
            self.sites.push((self.asm.offset(), cross.fault));
            self.asm.emit(cross.direct);
            if count {
                self.asm.emit_all(&[
                    ldr(X, X1, RT, counter),
                    ldr(X, X0, X1, 0),
                    add_imm(X, X0, X0, 1, false),
                    str(X, X0, X1, 0),
                ]);
            }
            self.asm.b(cross.resume);
            self.asm.bind(blocked);
            self.asm
                .emit_all(&[ldp(X, Index::Post, T, X5, SP, 16), msr_nzcv(X5)]);
            self.asm.b(cross.fail);
        }
    }

    pub(super) fn checked_accesses(&mut self) {
        const PAGE_SHIFT: u32 = crate::translate::lower::access::PAGE_SHIFT;
        for access in std::mem::take(&mut self.checked_accesses) {
            let CheckedAccess {
                address,
                value,
                kind,
                site,
                resume,
                slow,
                crossing,
                region,
                eip,
            } = access;
            let blocked = self.asm.label();
            self.asm.bind(slow);
            self.mark(Part::Cold, eip);
            // The full window class table grants guest permission before
            // indexing Memory's raw permissions, which start at memory_base.
            let store = kind & runtime::ACCESS_STORE != 0;
            let needed = if store { WRITE } else { READ };
            self.asm.emit_all(&[
                lsr_imm(W, X5, address, PAGE_SHIFT),
                ldr(X, X3, RT, runtime::CLASSES),
                ldr_narrow(Narrow::Byte, X4, X3, X5),
            ]);
            self.asm.tbz(false, X4, needed.trailing_zeros(), blocked);
            self.asm.emit_all(&[
                ldr(X, X3, RT, runtime::ALIAS_CLASSES),
                ldr_narrow(Narrow::Byte, X4, X3, X5),
            ]);
            self.asm.tbz(false, X4, SHARED.trailing_zeros(), blocked);
            self.asm.tbz(true, X4, LINEAR.trailing_zeros(), blocked);

            if let Some(crossing) = crossing {
                self.asm.bind(crossing);
            }
            let width = kind & 31;
            self.asm.emit(mov(W, X1, address));
            if store {
                self.asm.emit(match (value, width) {
                    (AccessReg::Scalar(src), 8) => str(X, src, RT, runtime::VALUE_OPERAND),
                    (AccessReg::Scalar(src), _) => mov(W, X3, src),
                    (AccessReg::Vector(size, src), 8 | 16) => {
                        vstr(size, src, RT, runtime::VALUE_OPERAND)
                    }
                    (AccessReg::Vector(_, src), _) => vumov(2, X3, src, 0),
                });
            }
            self.asm.adr(X2, site);
            if self.modifies_fpcr && !self.has_afp {
                fp_state::emit_restore_host_fpcr(&mut self.asm, T, U);
            }
            self.asm.emit_all(&[
                movz(W, X4, kind as u16, 0),
                ldr(X, T, RT, RESIDENT_ACCESS),
                blr(T),
            ]);
            self.asm.tbz(true, X0, 32, blocked);
            if !store {
                self.asm.emit(match (value, width) {
                    (AccessReg::Scalar(dst), 8) => ldr(X, dst, RT, runtime::VALUE_OPERAND),
                    (AccessReg::Scalar(dst), _) => mov(W, dst, X0),
                    (AccessReg::Vector(size, dst), 8 | 16) => {
                        vldr(size, dst, RT, runtime::VALUE_OPERAND)
                    }
                    (AccessReg::Vector(_, dst), _) => fmov_to_v(W, dst, X0),
                });
            }
            self.asm.b(resume);

            self.asm.bind(blocked);
            self.cold_exit(region, ResidentCold::BlockedAccess, eip);
        }
    }

    pub(super) fn frame_disjoint(
        &mut self,
        addr: Value,
        width: u8,
        base: Value,
        lo: i32,
        hi: i32,
        fail: Label,
    ) {
        let last = u32::from(width - 1);
        let span = (i64::from(hi) - i64::from(lo)) as u64 + u64::from(last);
        if span > u64::from(u32::MAX) {
            self.asm.b(fail);
            return;
        }
        let address = self.gp(addr, T);
        let base = self.gp(base, U);
        self.asm.emit(sub(W, T, address, base, Lsl, 0));
        let bias = u64::from(last.wrapping_sub(lo as u32));
        if bias != 0 {
            if let Some(add) = int_immediate(IntOp::Add, W, T, T, bias) {
                self.asm.emit(add);
            } else {
                self.imm(U, bias);
                self.asm.emit(add(W, T, T, U, Lsl, 0));
            }
        }
        if let Some((length, shift)) = add_immediate(span) {
            self.asm.emit(cmp_imm(W, T, length, shift));
        } else {
            self.imm(U, span);
            self.asm.emit(cmp(W, T, U, Lsl, 0));
        }
        self.asm.b_cond(Cond::Lo, fail);
    }

    pub(super) fn plain_page(&mut self, addr: Value, bytes: u32, write: bool) {
        let miss = self.asm.label();
        self.fact_misses.push(miss);
        if bytes == 0 {
            return;
        }

        let addr = self.gp(addr, U);
        crate::facts::plain_span(&mut self.asm, addr, bytes, write, miss);
    }
}

/// Like the Tier-1 access stub, calls the owner's access helper. The
/// optimized allocation can use full Q registers and owns its recovery,
/// so keep those registers and return the refusal bit to the form.
#[cfg(any(native, test))]
pub(crate) fn access_stub(helper: u64) -> Vec<u32> {
    access_stub_for(helper, false)
}

#[cfg(any(native, test))]
pub(crate) fn access_stub_for(helper: u64, afp: bool) -> Vec<u32> {
    let mut asm = Asm::new();
    asm.emit(mov(X, X0, RT));
    preserve_call(&mut asm, helper, afp);
    asm.finish()
}

/// Calls a helper with arguments in x0-x5, preserving all allocated values.
pub(super) fn preserve_call(asm: &mut Asm, helper: u64, afp: bool) {
    let frame = 96 + 28 * 16 + if afp { 16 } else { 0 };
    asm.emit_all(&[
        sub_imm(X, SP, SP, frame, false),
        mrs_nzcv(T),
        stp(X, Index::Offset, T, Reg::LR, SP, 0),
    ]);
    for n in (6..16).step_by(2) {
        asm.emit(stp(
            X,
            Index::Offset,
            Reg::new(n),
            Reg::new(n + 1),
            SP,
            16 + 8 * i32::from(n - 6),
        ));
    }
    for n in (0..27).step_by(2) {
        asm.emit(stp_v(
            VSize::Q,
            Index::Offset,
            VReg::new(n),
            VReg::new(n + 1),
            SP,
            96 + 16 * i32::from(n),
        ));
    }
    if afp {
        // x0..x5 hold arguments to `helper`; x6 and x7 were saved above.
        fp_state::emit_restore_host_fpcr(asm, Reg::new(6), Reg::new(7));
        fp_state::emit_isolate_fpsr_before(asm, Reg::new(6), Reg::new(7));
        asm.emit(str(X, Reg::new(6), SP, frame - 16));
    }
    asm.emit(movz(X, T, helper as u16, 0));
    for shift in [16, 32, 48] {
        asm.emit(movk(X, T, (helper >> shift) as u16, shift));
    }
    asm.emit_all(&[
        stp(X, Index::Pre, RT, Reg::LR, SP, -16),
        mov_sp(RT, SP),
        blr(T),
        ldp(X, Index::Post, RT, Reg::LR, SP, 16),
    ]);
    if afp {
        // x0 holds the return value; x6..x8 are restored below.
        asm.emit(ldr(X, Reg::new(6), SP, frame - 16));
        fp_state::emit_isolate_fpsr_after(asm, Reg::new(6), Reg::new(7), Reg::new(8));
    }
    for n in (0..27).step_by(2) {
        asm.emit(ldp_v(
            VSize::Q,
            Index::Offset,
            VReg::new(n),
            VReg::new(n + 1),
            SP,
            96 + 16 * i32::from(n),
        ));
    }
    for n in (6..16).step_by(2) {
        asm.emit(ldp(
            X,
            Index::Offset,
            Reg::new(n),
            Reg::new(n + 1),
            SP,
            16 + 8 * i32::from(n - 6),
        ));
    }
    asm.emit_all(&[
        ldp(X, Index::Offset, T, Reg::LR, SP, 0),
        add_imm(X, SP, SP, frame, false),
        msr_nzcv(T),
        ret(Reg::LR),
    ]);
}

pub(super) struct Probation {
    pub slot: Label,
    pub calls: Vec<usize>,
    pub counter: Label,
    pub leave: Label,
    pub sensors: Vec<usize>,
    exits: Vec<(Label, u32, Label)>,
    snapshot: Label,
    account: Label,
    rows: Option<(Option<u8>, [Label; 2])>,
}

impl Probation {
    pub fn new(asm: &mut Asm, row_counter: Option<Option<u8>>) -> Self {
        Self {
            slot: asm.label(),
            calls: Vec::new(),
            counter: asm.label(),
            leave: asm.label(),
            sensors: Vec::new(),
            exits: Vec::new(),
            snapshot: asm.label(),
            account: asm.label(),
            rows: row_counter.map(|reg| (reg, [asm.label(), asm.label()])),
        }
    }

    pub fn entry(&mut self, asm: &mut Asm) {
        self.calls.push(asm.offset() / 4);
        asm.bl(self.snapshot);
    }

    pub fn exit(&mut self, asm: &mut Asm, eip: u32, returning: Option<bool>) {
        let account =
            self.rows
                .zip(returning)
                .map_or(self.account, |((reg, labels), returning)| {
                    if returning && reg.is_none() {
                        self.account
                    } else {
                        labels[usize::from(returning)]
                    }
                });
        let trigger = asm.label();
        self.sensors.push(asm.offset() / 4);
        self.exits.push((trigger, eip, account));
        runtime::emit_sensor(asm, self.counter, T, U, trigger);
        self.calls.push(asm.offset() / 4);
        asm.bl(account);
    }

    pub fn emit(&self, asm: &mut Asm) {
        asm.bind(self.snapshot);
        asm.ldr_literal(T, self.slot);
        // A refill adds the same amount to completed and the countdown.
        asm.emit_all(&[
            ldr(X, U, RT, runtime::COMPLETED),
            sub(X, U, U, COUNT, Lsl, 0),
            str(X, U, T, 0),
            ret(Reg::LR),
        ]);
        asm.bind(self.account);
        asm.ldr_literal(T, self.slot);
        asm.emit_all(&[
            ldr(X, U, RT, runtime::COMPLETED),
            sub(X, U, U, COUNT, Lsl, 0),
            ldr(X, X0, T, 0),
            sub(X, U, U, X0, Lsl, 0),
            ldr(X, X0, T, 8),
            add(X, U, U, X0, Lsl, 0),
            str(X, U, T, 8),
            ldr(X, U, T, 16),
            add_imm(X, U, U, 1, false),
            str(X, U, T, 16),
            ret(Reg::LR),
        ]);
        if let Some((reg, labels)) = self.rows {
            // Accounting runs after recovery at the outer head or beyond the latch.
            for (returning, label) in labels.into_iter().enumerate() {
                if returning != 0 && reg.is_none() {
                    continue;
                }
                asm.bind(label);
                asm.emit(ldr(W, U, RT, runtime::ROW_COUNT));
                if let Some(reg) = reg.filter(|_| returning != 0) {
                    asm.emit(sub(W, U, U, super::inline::guest(reg), Lsl, 0));
                }
                asm.ldr_literal(T, self.slot);
                asm.emit_all(&[
                    ldr(X, X0, T, 24),
                    add(X, X0, U, X0, Lsl, 0),
                    str(X, X0, T, 24),
                    ldr(W, X0, RT, runtime::ROW_WORK),
                    crate::encode::umull(U, U, X0),
                    ldr(X, X0, T, 32),
                    add(X, U, U, X0, Lsl, 0),
                    str(X, U, T, 32),
                ]);
                asm.b(self.account);
            }
        }
        for &(trigger, eip, account) in &self.exits {
            asm.bind(trigger);
            asm.bl(account);
            crate::translate::lower::emit_target(asm, eip);
            asm.b(self.leave);
        }
        asm.bind(self.leave);
        asm.emit(0);
        asm.bind(self.slot);
        asm.emit_all(&[0, 0]);
        asm.bind(self.counter);
        asm.emit_all(&[0, 0]);
    }
}
