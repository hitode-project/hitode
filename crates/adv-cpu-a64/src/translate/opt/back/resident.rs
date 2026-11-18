//! Window checks and the owner access call for resident forms.

use super::*;
use crate::translate::{X0, X1, X2, X3, X4, X5};
use adv_cpu_core::memory::{LINEAR, READ, SHARED, WRITE};

const MEMORY_BASE: u32 = offset_of!(Runtime, memory_base) as u32;
const MEMORY_PAGES: u32 = offset_of!(Runtime, memory_pages) as u32;
const PLAIN_PAGES: u32 = offset_of!(Runtime, plain_pages) as u32;
const RESIDENT_SITE: u32 = offset_of!(Runtime, resident_site) as u32;
const RESIDENT_ACCESS: u32 = offset_of!(Runtime, resident_access) as u32;

pub(super) struct CheckedAccess {
    address: Reg,
    value: AccessReg,
    kind: u32,
    site: Label,
    resume: Label,
    slow: Label,
    crossing: Label,
    region: RecordId,
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
        if !self.resident || !checked {
            self.site(false);
            self.asm.emit(direct);
            return;
        }

        let (slow, crossing) = (self.asm.label(), self.asm.label());
        let width = u32::from(width);
        let (site, resume) = self.checked_memory(address, width, store, direct, slow, crossing);
        let kind = width | if store { runtime::ACCESS_STORE } else { 0 };
        self.checked_accesses.push(CheckedAccess {
            address,
            value,
            kind,
            site,
            resume,
            slow,
            crossing,
            region: self.region,
            eip: self.guest_eip,
        });
    }

    pub(super) fn checked_accesses(&mut self) {
        const PAGE_SHIFT: u32 = crate::translate::access::PAGE_SHIFT;
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

            self.asm.bind(crossing);
            let width = kind & 31;
            self.asm.emit(mov(W, X1, address));
            if store {
                self.asm.emit(match (value, width) {
                    (AccessReg::Scalar(src), 8) => str(X, src, RT, runtime::ACCESS_VECTOR),
                    (AccessReg::Scalar(src), _) => mov(W, X3, src),
                    (AccessReg::Vector(size, src), 8 | 16) => {
                        vstr(size, src, RT, runtime::ACCESS_VECTOR)
                    }
                    (AccessReg::Vector(_, src), _) => vumov(2, X3, src, 0),
                });
            }
            self.asm.adr(X2, site);
            self.asm.emit_all(&[
                movz(W, X4, kind as u16, 0),
                ldr(X, T, RT, RESIDENT_ACCESS),
                blr(T),
            ]);
            self.asm.tbz(true, X0, 32, blocked);
            if !store {
                self.asm.emit(match (value, width) {
                    (AccessReg::Scalar(dst), 8) => ldr(X, dst, RT, runtime::ACCESS_VECTOR),
                    (AccessReg::Scalar(dst), _) => mov(W, dst, X0),
                    (AccessReg::Vector(size, dst), 8 | 16) => {
                        vldr(size, dst, RT, runtime::ACCESS_VECTOR)
                    }
                    (AccessReg::Vector(_, dst), _) => fmov_to_v(W, dst, X0),
                });
            }
            self.asm.b(resume);

            self.asm.bind(blocked);
            self.imm(T, u64::from(eip));
            self.asm.emit(str(W, T, RT, RESIDENT_SITE));
            self.asm.b(self.cold[region.0 as usize]);
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
        let next = self.asm.label();
        self.asm
            .emit_all(&[ldr(W, X0, RT, MEMORY_BASE), cmp(W, addr, X0, Lsl, 0)]);
        self.asm.b_cond(Cond::Lo, miss);
        self.asm
            .emit_all(&[mov(W, X1, addr), sub_ext(X, X1, X1, X0, Extend::Uxtw, 0)]);
        self.imm(X2, u64::from(bytes - 1));
        self.asm.emit_all(&[
            add(X, X2, X1, X2, Lsl, 0),
            lsr_imm(X, X1, X1, 12),
            lsr_imm(X, X2, X2, 12),
            ldr(W, X3, RT, MEMORY_PAGES),
            cmp(X, X2, X3, Lsl, 0),
        ]);
        self.asm.b_cond(Cond::Hs, miss);
        self.asm.emit(ldr(X, X3, RT, PLAIN_PAGES));
        if write {
            self.asm.emit(lsr_imm(W, X0, X0, 12));
        }

        self.asm.bind(next);
        self.asm.emit_all(&[
            lsr_imm(W, X4, X1, 6),
            add(W, X4, X4, X4, Lsl, 1),
            add_ext(X, X4, X3, X4, Extend::Uxtw, 3),
            ldr(X, X5, X4, if write { 8 } else { 0 }),
            shiftv(X, Lsr, X5, X5, X1),
        ]);
        self.asm.tbz(false, X5, 0, miss);
        if write {
            self.asm
                .emit_all(&[ldr(X, X5, X4, 16), shiftv(X, Lsr, X5, X5, X1)]);
            self.asm.tbz(true, X5, 0, miss);
            // The guest permission bitmap does not include the host
            // protection intersection when host pages exceed 4 KiB.
            self.asm.emit_all(&[
                add(W, X5, X0, X1, Lsl, 0),
                ldr(X, X4, RT, runtime::CLASSES),
                ldr_narrow(Narrow::Byte, X5, X4, X5),
            ]);
            self.asm.tbz(false, X5, WRITE.trailing_zeros(), miss);
            self.asm
                .tbz(true, X5, adv_cpu_core::memory::EXEC.trailing_zeros(), miss);
        }
        self.asm
            .emit_all(&[add_imm(W, X1, X1, 1, false), cmp(W, X1, X2, Lsl, 0)]);
        self.asm.b_cond(Cond::Ls, next);
    }
}

/// Like the Tier-1 access stub, calls the owner's access helper. The
/// optimized allocation can use full Q registers and owns its recovery,
/// so keep those registers and return the refusal bit to the form.
#[cfg(any(native, test))]
pub(crate) fn access_stub(helper: u64) -> Vec<u32> {
    let mut asm = Asm::new();
    let frame = 96 + 28 * 16;
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
    asm.emit(movz(X, T, helper as u16, 0));
    for shift in [16, 32, 48] {
        asm.emit(movk(X, T, (helper >> shift) as u16, shift));
    }
    asm.emit_all(&[
        mov(X, X0, RT),
        stp(X, Index::Pre, RT, Reg::LR, SP, -16),
        mov_sp(RT, SP),
        blr(T),
        ldp(X, Index::Post, RT, Reg::LR, SP, 16),
    ]);
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
    asm.finish()
}

pub(super) struct Probation {
    pub slot: Label,
    pub calls: Vec<usize>,
    snapshot: Label,
    account: Label,
}

impl Probation {
    pub fn new(asm: &mut Asm) -> Self {
        Self {
            slot: asm.label(),
            calls: Vec::new(),
            snapshot: asm.label(),
            account: asm.label(),
        }
    }

    pub fn call(&mut self, asm: &mut Asm, entry: bool) {
        self.calls.push(asm.offset() / 4);
        asm.bl(if entry { self.snapshot } else { self.account });
    }

    pub fn emit(&self, asm: &mut Asm) {
        asm.bind(self.snapshot);
        asm.adr(T, self.slot);
        // A refill adds the same amount to completed and the countdown.
        asm.emit_all(&[
            ldr(X, T, T, 0),
            ldr(X, U, RT, runtime::COMPLETED),
            sub(X, U, U, COUNT, Lsl, 0),
            str(X, U, T, 0),
            ret(Reg::LR),
        ]);
        asm.bind(self.account);
        asm.adr(T, self.slot);
        asm.emit_all(&[
            ldr(X, T, T, 0),
            ldr(X, U, RT, runtime::COMPLETED),
            sub(X, U, U, COUNT, Lsl, 0),
            ldr(X, X0, T, 0),
            sub(X, U, U, X0, Lsl, 0),
            ldr(X, X0, T, 8),
            add(X, U, U, X0, Lsl, 0),
            str(X, U, T, 8),
            ret(Reg::LR),
        ]);
        asm.bind(self.slot);
        asm.emit_all(&[0, 0]);
    }
}
