//! Specialized, admitted bulk bridges (A64-TR-8). Admission runs while building
//! a hot translation. Generated code contains only fixed input expressions,
//! range guards, a direct kernel call and fixed cursor updates.
use adv_cpu_core::memory::{EXEC, READ, SHARED, WRITE};
use adv_cpu_core::native_region::{Affine, Aliasing, NativeRegion};

use super::trace::{Edge, Mode, Next};
use super::*;

const MINIMUM: u32 = 128;
const MAXIMUM: u32 = 1024;
const SP: Reg = Reg::new(31);
const ADDRESS: Reg = Reg::new(7);
const FRAME: Reg = Reg::new(8);

impl Unit {
    pub(super) fn prepare_regions(
        &mut self,
        memory: &adv_cpu_core::Memory,
        admit: &dyn Fn(u32, &[u8]) -> Vec<NativeRegion>,
        checked: &dyn Fn(u32) -> bool,
    ) {
        for start in 0..self.trace.len() {
            if !self.trace.merge[start] {
                continue;
            }
            let Some(end) = (start..self.trace.len())
                .find(|&n| self.trace.insts[n].taken == Some(Edge::Internal(start)))
            else {
                continue;
            };
            let body = &self.trace.insts[start..=end];
            if body.iter().enumerate().any(|(n, i)| {
                i.mode != Mode::Plain
                    || i.call.is_some()
                    || matches!(i.op, super::decode::Op::Helper(_))
                    || checked(i.pc)
                    || (n != 0 && (self.trace.head(start + n) || self.trace.merge[start + n]))
                    || (n + 1 != body.len()
                        && (i.next != Next::Fall
                            || i.taken.is_some()
                            || i.next_pc != body[n + 1].pc))
            }) {
                continue;
            }
            let pc = body[0].pc;
            let Some(len) = body.last().unwrap().next_pc.checked_sub(pc) else {
                continue;
            };
            let Ok(bytes) = memory.read(pc, len as usize) else {
                continue;
            };
            for region in admit(pc, bytes) {
                if region.instructions() as usize == body.len() {
                    self.regions.push((start, region));
                }
            }
        }
    }
}

impl Translator<'_> {
    fn region_address(&mut self, address: Affine, region: NativeRegion) -> Reg {
        if address == region.inputs()[0] {
            return X0;
        }
        if address == region.inputs()[1] {
            return X1;
        }
        self.affine(address, ADDRESS);
        ADDRESS
    }
    fn affine(&mut self, value: Affine, out: Reg) {
        self.mov_imm(out, value.constant);
        for (r, coefficient) in value.registers.into_iter().enumerate() {
            match coefficient {
                1 => self.emit(add(W, out, out, host(r as u8), Lsl, 0)),
                -1 => self.emit(sub(W, out, out, host(r as u8), Lsl, 0)),
                _ => {}
            }
        }
    }

    pub(super) fn native_batch(&mut self, index: usize) {
        if self.x87.active || self.pixel.is_some() {
            return;
        }
        let regions: Vec<_> = self
            .regions
            .iter()
            .filter_map(|&(at, region)| (at == index).then_some(region))
            .collect();
        if regions.is_empty() {
            return;
        }
        let done = self.asm.label();
        for region in regions {
            self.native_choice(region, done);
        }
        self.asm.bind(done);
    }

    fn native_choice(&mut self, region: NativeRegion, done: Label) {
        // As with channel batches, page-class snapshots must be refreshed
        // after code discovery changes host-page protection.
        *self.uses_permissions = true;
        let scalar = self.asm.label();
        self.emit(mrs_nzcv(X15));
        for (input, target) in region.inputs().iter().zip([X0, X1, X2]) {
            self.affine(*input, target);
        }
        self.emit(cmp_imm(W, X2, MINIMUM + 1, false));
        self.asm.b_cond(Cond::Lo, scalar);
        // Full input/output spans may not wrap. Conservative at 4 GiB itself.
        self.emit_all(&[add(X, X4, X1, X2, Lsl, 0), lsr_imm(X, X5, X4, 32)]);
        self.asm.cbz(X, true, X5, scalar);
        for read in region.reads().iter().flatten() {
            let address = self.region_address(read.address, region);
            self.emit_all(&[add(X, X3, address, X2, Lsl, 0), lsr_imm(X, X5, X3, 32)]);
            self.asm.cbz(X, true, X5, scalar);
            match read.aliasing {
                Aliasing::Backward => {
                    self.emit(cmp(W, address, X1, Lsl, 0));
                    self.asm.b_cond(Cond::Hs, scalar);
                }
                Aliasing::Disjoint => {
                    let safe = self.asm.label();
                    self.emit(cmp(X, X3, X1, Lsl, 0));
                    self.asm.b_cond(Cond::Ls, safe);
                    self.emit(cmp(X, X4, address, Lsl, 0));
                    self.asm.b_cond(Cond::Hi, scalar);
                    self.asm.bind(safe);
                }
            }
        }
        if let Some(parameter) = region.scalar() {
            self.affine(parameter.value, X6);
            for bound in parameter.upper_bounds.into_iter().flatten() {
                let bound = self.region_address(bound, region);
                self.emit(cmp(W, X6, bound, Lsl, 0));
                self.asm.b_cond(Cond::Hi, scalar);
            }
        }
        if let Some(frame) = region.frame() {
            self.affine(frame.address, FRAME);
            if frame.bytes == 4096 {
                self.mov_imm(X16, frame.bytes);
                self.emit(add(X, X3, FRAME, X16, Lsl, 0));
            } else {
                self.emit(add_imm(X, X3, FRAME, frame.bytes, false));
            }
            self.emit(lsr_imm(X, X5, X3, 32));
            self.asm.cbz(X, true, X5, scalar);
            // A bounded fixed frame and all data buffers remain disjoint for
            // the batch. Skipped scratch stores cannot affect native inputs.
            for expression in region
                .reads()
                .iter()
                .flatten()
                .map(|r| r.address)
                .chain(std::iter::once(region.inputs()[1]))
            {
                let address = self.region_address(expression, region);
                let safe = self.asm.label();
                self.emit(cmp(X, X3, address, Lsl, 0));
                self.asm.b_cond(Cond::Ls, safe);
                self.emit_all(&[add(X, X4, address, X2, Lsl, 0), cmp(X, X4, FRAME, Lsl, 0)]);
                self.asm.b_cond(Cond::Hi, scalar);
                self.asm.bind(safe);
            }
            self.emit_all(&[
                add_imm(X, X3, FRAME, frame.bytes - 1, false),
                eor(X, X4, X3, FRAME, Lsl, 0),
            ]);
            self.asm.tbz(true, X4, 12, scalar);
            self.emit_all(&[
                ldr(X, X5, RUNTIME, crate::runtime::CLASSES),
                lsr_imm(W, X3, FRAME, 12),
                ldr_narrow(Narrow::Byte, X4, X5, X3),
            ]);
            self.asm.tbz(false, X4, READ.trailing_zeros(), scalar);
            self.asm.tbz(true, X4, SHARED.trailing_zeros(), scalar);
            if frame.writes {
                self.asm.tbz(false, X4, WRITE.trailing_zeros(), scalar);
                self.asm.tbz(true, X4, EXEC.trailing_zeros(), scalar);
            }
            for invariant in frame.invariants.into_iter().flatten() {
                self.affine(invariant.value, X6);
                self.emit_all(&[
                    add_imm(W, X16, FRAME, invariant.offset, false),
                    ldr_reg(W, X3, WINDOW, X16, false),
                    cmp(W, X3, X6, Lsl, 0),
                ]);
                self.asm.b_cond(Cond::Ne, scalar);
            }
        }
        // Leave one non-faulting scalar iteration to reconstruct all temps
        // and flags. Bound each batch for scheduling and memory-class checks.
        self.emit(sub_imm(W, X2, X2, 1, false));
        self.mov_imm(X3, MAXIMUM);
        self.emit_all(&[cmp(W, X2, X3, Lsl, 0), csel(W, X2, X2, X3, Cond::Lo)]);
        for expression in region
            .reads()
            .iter()
            .flatten()
            .map(|r| r.address)
            .chain(std::iter::once(region.inputs()[1]))
        {
            let address = self.region_address(expression, region);
            self.emit(and_imm(W, X3, address, 4095));
            self.mov_imm(X4, 4095);
            self.emit_all(&[
                sub(W, X3, X4, X3, Lsl, 0),
                cmp(W, X2, X3, Lsl, 0),
                csel(W, X2, X2, X3, Cond::Lo),
            ]);
        }
        self.emit(cmp_imm(W, X2, MINIMUM, false));
        self.asm.b_cond(Cond::Lo, scalar);
        self.emit(ldr(X, X5, RUNTIME, crate::runtime::CLASSES));
        for (expression, write) in region
            .reads()
            .iter()
            .flatten()
            .map(|r| (r.address, false))
            .chain(std::iter::once((region.inputs()[1], true)))
        {
            let address = self.region_address(expression, region);
            self.emit_all(&[
                lsr_imm(W, X3, address, 12),
                ldr_narrow(Narrow::Byte, X4, X5, X3),
            ]);
            self.asm.tbz(false, X4, READ.trailing_zeros(), scalar);
            self.asm.tbz(true, X4, SHARED.trailing_zeros(), scalar);
            if write {
                self.asm.tbz(false, X4, WRITE.trailing_zeros(), scalar);
                self.asm.tbz(true, X4, EXEC.trailing_zeros(), scalar);
            }
        }
        self.mov_imm(X16, region.instructions());
        self.emit_all(&[mul(W, X6, X2, X16), cmp(X, X9, X6, Lsl, 0)]);
        self.asm.b_cond(Cond::Lo, scalar);
        self.emit(sub(X, X9, X9, X6, Lsl, 0));
        // Guest GPRs are callee-saved. Only canonical flags, fuel, count and
        // LR cross this ordinary host ABI; there is no CPU snapshot.
        self.emit_all(&[
            stp(X, Index::Pre, X9, X10, SP, -64),
            stp(X, Index::Offset, X11, X12, SP, 16),
            stp(X, Index::Offset, X15, Reg::LR, SP, 32),
            stp(X, Index::Offset, X2, X3, SP, 48),
            add(X, X0, WINDOW, X0, Lsl, 0),
            add(X, X1, WINDOW, X1, Lsl, 0),
        ]);
        if let Some(parameter) = region.scalar() {
            self.affine(parameter.value, X3);
        }
        let target = region.kernel() as usize as u64;
        self.emit(movz(X, X16, target as u16, 0));
        for shift in [16, 32, 48] {
            self.emit(movk(X, X16, (target >> shift) as u16, shift));
        }
        self.emit(blr(X16));
        self.emit_all(&[
            ldp(X, Index::Offset, X2, X3, SP, 48),
            ldp(X, Index::Offset, X15, Reg::LR, SP, 32),
            ldp(X, Index::Offset, X11, X12, SP, 16),
            ldp(X, Index::Post, X9, X10, SP, 64),
        ]);
        for (r, advance) in region.advances().iter().enumerate() {
            match advance {
                1 => self.emit(add(W, host(r as u8), host(r as u8), X2, Lsl, 0)),
                -1 => self.emit(sub(W, host(r as u8), host(r as u8), X2, Lsl, 0)),
                _ => {}
            }
            if *advance != 0 && biased(r as u8) {
                self.emit(add(X, host(r as u8), WINDOW, host(r as u8), Lsl, 0));
            }
        }
        #[cfg(test)]
        self.emit_all(&[
            ldr(X, X3, RUNTIME, crate::runtime::NATIVE_BATCHES),
            add_imm(X, X3, X3, 1, false),
            str(X, X3, RUNTIME, crate::runtime::NATIVE_BATCHES),
        ]);
        self.emit(msr_nzcv(X15));
        self.asm.b(done);
        self.asm.bind(scalar);
        self.emit(msr_nzcv(X15));
    }
}
