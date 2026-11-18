use super::*;
use crate::translate::X4;

use iced_x86::ConditionCode as Cc;
use loops::{Affine, Batch, Span, Trip};

/// The kernel can emit instructions and read the checked addresses and
/// invariant SSA inputs. It cannot change the scalar CFG or recovery records.
pub(in crate::translate::opt) struct BatchEmitter<'b, 'f> {
    emitter: &'b mut Emitter<'f>,
}

impl BatchEmitter<'_, '_> {
    pub fn asm(&mut self) -> &mut Asm {
        &mut self.emitter.asm
    }

    pub fn invariant(&mut self, value: Value, scratch: Reg) -> Reg {
        self.emitter.batch_gp(value, scratch)
    }

    pub fn address(&mut self, addr: Affine) -> Reg {
        self.emitter.batch_address(addr, T, U);
        let view = self.emitter.data_view();
        self.asm().emit(add_ext(X, T, view, T, Extend::Uxtw, 0));
        T
    }
}

impl Emitter<'_> {
    pub(super) fn batch(&mut self, batch: &Batch) {
        let scalar = self.asm.label();
        let lanes = u32::from(batch.plan.lanes);
        let charge = u64::from(batch.loop_.charge) * u64::from(lanes);
        self.imm(T, charge);
        self.asm.emit(cmp(X, COUNT, T, Lsl, 0));
        self.asm.b_cond(Cond::Lt, scalar);
        if !self.resident && self.func.head_exit == Some(batch.loop_.head) {
            // Split jobs and deferred recalls still stop at their scalar edge.
            self.asm.emit(ldr(W, T, RT, runtime::HEAD_EXIT));
            self.asm.cbz(W, true, T, scalar);
        }
        self.batch_trip(batch.loop_.trip, lanes, scalar);
        for &span in &batch.spans {
            self.batch_address(span.start, U, T);
            if self.resident {
                self.plain_span(U, span.bytes, span.write, true, scalar);
            } else {
                // The job lease grants the scalar accesses through the alias.
                // Check that the interval is a single contiguous guest span.
                self.imm(T, u64::from(span.bytes - 1));
                self.asm.emit(adds(W, Reg::new(31), U, T, Lsl, 0));
                self.asm.b_cond(Cond::Hs, scalar);
            }
        }
        for &(a, b) in &batch.alias {
            let a = batch.spans[a];
            let b = batch.spans[b];
            self.batch_disjoint(a, b, scalar);
        }
        for frame in &batch.frame {
            for &span in batch.spans.iter().filter(|s| s.write) {
                let frame = Span {
                    start: frame.start,
                    bytes: frame.bytes,
                    write: false,
                };
                self.batch_disjoint(span, frame, scalar);
            }
        }
        for stream in &batch.crossings {
            let addr = stream
                .addr
                .shifted((stream.stride as u32).wrapping_mul(lanes));
            self.batch_address(addr, T, U);
            page_crossing(&mut self.asm, T, u32::from(stream.width), scalar);
        }

        batch.plan.body.lower(&mut BatchEmitter { emitter: self });
        self.imm(T, charge);
        BatchEmitter { emitter: self }
            .asm()
            .emit(sub(X, COUNT, COUNT, T, Lsl, 0));
        for iv in &batch.loop_.ivs {
            let loc = self.locations[iv.base.0 as usize];
            let src = self.gp_at(loc, T);
            let dst = match loc {
                Host::Register(r) => r,
                _ => T,
            };
            self.batch_add(dst, src, (iv.stride as u32).wrapping_mul(lanes), U);
            self.copy(loc, Host::Register(dst), Type::I32);
        }
        self.asm.bind(scalar);
    }

    fn batch_gp(&mut self, value: Value, scratch: Reg) -> Reg {
        if let Some(Op::Constant(_, bits)) = self.defs[value.0 as usize] {
            self.imm(scratch, bits as u64);
            scratch
        } else {
            self.gp(value, scratch)
        }
    }

    fn batch_add(&mut self, dst: Reg, src: Reg, offset: u32, scratch: Reg) {
        if offset == 0 {
            if dst != src {
                self.asm.emit(mov(W, dst, src));
            }
        } else if let Some(word) = int_immediate(IntOp::Add, W, dst, src, u64::from(offset)) {
            self.asm.emit(word);
        } else {
            self.imm(scratch, u64::from(offset));
            self.asm.emit(add(W, dst, src, scratch, Lsl, 0));
        }
    }

    fn batch_address(&mut self, addr: Affine, dst: Reg, scratch: Reg) {
        let src = self.batch_gp(addr.base, dst);
        self.batch_add(dst, src, addr.offset, scratch);
    }

    fn batch_disjoint(&mut self, a: Span, b: Span, scalar: Label) {
        self.batch_address(a.start, X0, T);
        self.batch_address(b.start, X1, T);
        for (l, r, bytes) in [(X1, X0, a.bytes), (X0, X1, b.bytes)] {
            self.asm.emit(sub(W, T, l, r, Lsl, 0));
            self.batch_cmp(T, bytes, U);
            self.asm.b_cond(Cond::Lo, scalar);
        }
    }

    fn batch_cmp(&mut self, r: Reg, bits: u32, scratch: Reg) {
        if let Some((imm, shift)) = add_immediate(u64::from(bits)) {
            self.asm.emit(cmp_imm(W, r, imm, shift));
        } else {
            self.imm(scratch, u64::from(bits));
            self.asm.emit(cmp(W, r, scratch, Lsl, 0));
        }
    }

    fn batch_trip(&mut self, trip: Trip, lanes: u32, scalar: Label) {
        self.batch_address(trip.value, T, U);
        let bound = trip.bound.map_or(Reg::new(31), |v| self.batch_gp(v, X0));
        if trip.again == Cc::ne {
            let (shift, inverse, mask) = trip.equality();
            self.asm.emit(sub(W, T, bound, T, Lsl, 0));
            let passes = self.asm.label();
            if shift != 0 {
                self.asm.emit(and_imm(W, U, T, (1u64 << shift) - 1));
                self.asm.cbz(W, true, U, passes);
                self.asm.emit(lsr_imm(W, T, T, shift));
            }
            if inverse != 1 {
                self.imm(U, u64::from(inverse));
                self.asm.emit(mul(W, T, T, U));
            }
            if mask != u32::MAX {
                self.asm.emit(and_imm(W, T, T, u64::from(mask)));
            }
            self.batch_cmp(T, lanes, U);
            self.asm.b_cond(Cond::Lo, scalar);
            self.asm.bind(passes);
            return;
        }
        if trip.again == Cc::e {
            if lanes > 1 {
                self.asm.b(scalar);
            } else {
                self.asm.emit(cmp(W, T, bound, Lsl, 0));
                self.asm.b_cond(Cond::Ne, scalar);
            }
            return;
        }
        let (fail, signed) = match trip.again {
            Cc::a => (Cond::Ls, false),
            Cc::ae => (Cond::Lo, false),
            Cc::b => (Cond::Hs, false),
            Cc::be => (Cond::Hi, false),
            Cc::g => (Cond::Le, true),
            Cc::ge => (Cond::Lt, true),
            Cc::l => (Cond::Ge, true),
            Cc::le => (Cond::Gt, true),
            _ => unreachable!(),
        };
        self.asm.emit(cmp(W, T, bound, Lsl, 0));
        self.asm.b_cond(fail, scalar);
        if signed {
            self.asm.emit(sbfx(X, T, T, 0, 32));
        }
        self.imm(U, (i64::from(trip.stride) * i64::from(lanes - 1)) as u64);
        self.asm.emit(add(X, U, T, U, Lsl, 0));
        self.asm.emit(if signed {
            sbfx(X, X4, U, 0, 32)
        } else {
            mov(W, X4, U)
        });
        self.asm.emit(cmp(X, U, X4, Lsl, 0));
        self.asm.b_cond(Cond::Ne, scalar);
        self.asm.emit(cmp(W, U, bound, Lsl, 0));
        self.asm.b_cond(fail, scalar);
    }
}
