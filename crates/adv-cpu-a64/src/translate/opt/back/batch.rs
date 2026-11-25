use super::{
    Asm, COUNT, Cond, Emitter, Extend, Host, IntOp, Label, Lsl, Op, RT, Reg, T, Type, U, VReg,
    Value, W, X, X0, X1, X2, X3, add, add_ext, add_imm, add_immediate, and_imm, cmp, csel, eor_imm,
    int_immediate, ldr, loops, lsr_imm, mov, movz, mul, mvn, page_crossing, runtime, sub, sub_imm,
    subs, subs_imm, udiv, umulh,
};
use crate::translate::lower::{X4, X5};

use crate::translate::front::tags::ConditionCode as Cc;
use loops::{Affine, Batch, Span, Trip};

use super::Counter;
use crate::encode::{Shift, and, orr, vumov};
use loops::vector::ClassPath;

pub(super) struct ClassPrice {
    pub first: ClassPath,
    pub second: ClassPath,
    pub words: u32,
}

pub(super) struct Price {
    pub label: Label,
    pub admission: u32,
    pub common: u32,
    pub classes: Vec<ClassPrice>,
}

/// The kernel can emit instructions and read the checked addresses and
/// invariant SSA inputs. It cannot change the scalar CFG or recovery records.
pub(in crate::translate::opt) struct BatchEmitter<'b, 'f> {
    emitter: &'b mut Emitter<'f>,
    head: u32,
    bail: Option<Label>,
}

impl BatchEmitter<'_, '_> {
    pub fn asm(&mut self) -> &mut Asm {
        &mut self.emitter.asm
    }

    /// Lazily allocates a mid-strip bail target (`A64-TR-9` / `A64-OPT-10`):
    /// charges only completed strips `X3 - X4`, publishes recovery operands
    /// when at least one strip ran, and falls into `scalar` so the failing
    /// strip executes in exact guest order.
    pub fn bail(&mut self) -> Label {
        if let Some(label) = self.bail {
            label
        } else {
            let label = self.emitter.asm.label();
            self.bail = Some(label);
            label
        }
    }

    pub fn invariant(&mut self, value: Value, scratch: Reg) -> Reg {
        self.emitter.batch_gp(value, scratch)
    }

    pub fn immediate(&mut self, dst: Reg, value: u64) {
        self.emitter.imm(dst, value);
    }

    pub fn vector_invariant(&mut self, value: Value, scratch: VReg) -> VReg {
        self.emitter.vr(value, scratch)
    }

    pub fn adjust_charge(&mut self, count: Reg, extra: i32) {
        self.emitter.imm(U, i64::from(extra) as u64);
        self.asm()
            .emit_all(&[mul(X, U, count, U), add(X, X5, X5, U, Lsl, 0)]);
    }

    pub fn count_class(&mut self, class: u8) {
        self.emitter.count(self.head, Counter::Class(class));
    }

    pub fn price_class(&mut self, first: ClassPath, second: ClassPath, words: u32) {
        self.emitter.class_prices.push(ClassPrice {
            first,
            second,
            words,
        });
    }

    pub fn publish(&mut self, value: Value, lane: u32, ty: Type, from: VReg) {
        let loc = self.emitter.locations[value.0 as usize];
        if ty == Type::I32 {
            let out = self.emitter.rd(value);
            self.asm().emit(vumov(2, out, from, lane * 2));
            self.emitter.copy(loc, Host::Register(out), ty);
        } else {
            let out = self.emitter.vd(value);
            self.asm().emit(vumov(3, T, from, lane));
            self.asm().emit(super::fmov_to_v(X, out, T));
            self.emitter.copy(loc, Host::Vector(out), ty);
        }
    }

    pub fn update(&mut self, value: Value, keep: u32, set: u32) {
        let loc = self.emitter.locations[value.0 as usize];
        let out = self.emitter.rd(value);
        if keep == 0 {
            self.emitter.imm(out, u64::from(set));
        } else {
            let old = self.emitter.gp_at(loc, T);
            if keep != u32::MAX {
                self.emitter.imm(U, u64::from(keep));
                self.asm().emit(and(W, out, old, U, Shift::Lsl, 0));
            } else if out != old {
                self.asm().emit(mov(W, out, old));
            }
            if set != 0 {
                self.emitter.imm(U, u64::from(set));
                self.asm().emit(orr(W, out, out, U, Shift::Lsl, 0));
            }
        }
        self.emitter.copy(loc, Host::Register(out), Type::I32);
    }

    pub fn debit(&mut self, cost: i32) {
        self.emitter.imm(T, cost.unsigned_abs() as u64);
        self.asm().emit(if cost >= 0 {
            add(X, X5, X5, T, Lsl, 0)
        } else {
            sub(X, X5, X5, T, Lsl, 0)
        });
    }

    pub fn address(&mut self, addr: Affine) -> Reg {
        self.emitter.batch_address(addr, T, U);
        let view = self.emitter.data_view();
        self.asm().emit(add_ext(X, T, view, T, Extend::Uxtw, 0));
        T
    }

    /// Commits a reduced `F64` accumulator back into its allocated host location.
    pub fn write_f64(&mut self, value: Value, src: VReg) {
        let loc = self.emitter.locations[value.0 as usize];
        self.emitter.copy(loc, Host::Vector(src), Type::F64);
    }

    /// Bails cleanly (before any strip executes) if planned strip count `X3`
    /// exceeds `max_strips`.
    pub fn bail_if_strips_exceed(&mut self, max_strips: u64) {
        self.emitter.imm(T, max_strips);
        self.asm().emit(cmp(X, X3, T, Lsl, 0));
        let bail = self.bail();
        self.asm().b_cond(Cond::Hi, bail);
    }
}

impl Emitter<'_> {
    pub(super) fn batch(
        &mut self,
        batch: &Batch,
        pages: Option<Label>,
        backedge: Label,
    ) -> (u32, Option<Price>) {
        self.reciprocal_division = batch.priced();
        let price = batch.priced().then(|| self.asm.label());
        let admission = self.asm.offset();
        let entry = self.asm.label();
        let inner = self.asm.label();
        let scalar = self.asm.label();
        let body = self.asm.label();
        let lanes = u32::from(batch.plan.lanes);
        let charge = u64::from(batch.loop_.charge) * u64::from(lanes);
        let prefix = batch.prefix_charge;

        self.asm.bind(entry);
        #[cfg(test)]
        {
            use super::{Runtime, offset_of, str};

            let entries = offset_of!(Runtime, vector_regions) as u32;
            self.asm.emit_all(&[
                ldr(X, T, RT, entries),
                add_imm(X, T, T, 1, false),
                str(X, T, RT, entries),
            ]);
        }
        if !self.resident && self.func.head_exit == Some(batch.loop_.head) {
            // Split jobs and deferred recalls still stop at their scalar edge.
            self.asm.emit(ldr(W, T, RT, runtime::HEAD_EXIT));
            self.asm.cbz(W, true, T, scalar);
        }
        if prefix != 0 {
            self.imm(T, prefix);
            self.asm.emit(sub(X, X3, COUNT, T, Lsl, 0));
        } else {
            self.asm.emit(mov(X, X3, COUNT));
        }
        self.asm.tbz(true, X3, 63, scalar);
        self.batch_div(X3, X3, charge + u64::from(lanes) * prefix);
        self.batch_trip(batch.loop_.trip, lanes, scalar);
        self.batch_minimum(price, scalar);

        if self.row_budget.is_none() {
            for &span in &batch.spans {
                self.batch_range(span, batch.backwards, pages, scalar, batch.loop_.eip);
                self.asm.cbz(X, false, X3, scalar);
            }
        }
        for &(a, b, in_place) in &batch.alias {
            self.batch_disjoint(
                batch.spans[a],
                batch.spans[b],
                batch.backwards,
                in_place,
                scalar,
            );
        }
        for frame in &batch.frame {
            for &span in batch.spans.iter().filter(|s| s.write) {
                let frame = Span {
                    start: frame.start,
                    width: frame.bytes,
                    step: 0,
                    write: false,
                };
                self.batch_disjoint(span, frame, batch.backwards, false, scalar);
            }
        }
        self.batch_minimum(price, scalar);
        for stream in &batch.crossings {
            // The first scalar iteration must reconstruct the guest state
            // without taking a PageCross cut.
            let step = (stream.stride as u32).wrapping_mul(lanes);
            self.batch_address(stream.addr, T, U);
            self.imm(U, u64::from(step));
            self.asm
                .emit_all(&[mul(W, U, U, X3), add(W, T, T, U, Lsl, 0)]);
            page_crossing(&mut self.asm, T, u32::from(stream.width), scalar);
        }

        self.imm(T, charge);
        self.asm.emit_all(&[mul(X, X5, X3, T), mov(X, X4, X3)]);
        let mut bail = {
            let mut emitter = BatchEmitter {
                emitter: self,
                head: batch.loop_.eip,
                bail: None,
            };
            batch.plan.body.lower_preheader(&mut emitter);
            emitter.bail
        };
        let preheader_end = self.asm.offset();
        let strips_done = self.asm.label();
        let mut unroll2_words = None;
        if batch.plan.body.has_unroll2() {
            let unroll2_loop = self.asm.label();
            let single_tail = self.asm.label();
            self.asm.emit(super::cmp_imm(X, X4, 2, false));
            self.asm.b_cond(Cond::Lo, single_tail);
            self.asm.bind(unroll2_loop);
            let u2_start = self.asm.offset();
            {
                // On pre-store bail in a 2x pair, fall into the 1x `inner` loop
                // so a valid first strip still commits before the second bails
                // to the scalar tail.
                let mut emitter = BatchEmitter {
                    emitter: self,
                    head: batch.loop_.eip,
                    bail: Some(inner),
                };
                batch.plan.body.lower_unroll2(&mut emitter);
            }
            self.count(batch.loop_.eip, Counter::Batch);
            self.count(batch.loop_.eip, Counter::Batch);
            for iv in &batch.loop_.ivs {
                let loc = self.locations[iv.base.0 as usize];
                let src = self.gp_at(loc, T);
                let dst = match loc {
                    Host::Register(r) => r,
                    _ => T,
                };
                self.batch_add(dst, src, (iv.stride as u32).wrapping_mul(lanes * 2), U);
                self.copy(loc, Host::Register(dst), Type::I32);
            }
            self.asm.emit_all(&[
                sub_imm(X, X4, X4, 2, false),
                super::cmp_imm(X, X4, 2, false),
            ]);
            self.asm.b_cond(Cond::Hs, unroll2_loop);
            unroll2_words = Some(self.words_since(u2_start).div_ceil(2));
            self.asm.bind(single_tail);
            self.asm.cbz(X, false, X4, strips_done);
        }
        self.asm.bind(inner);
        self.count(batch.loop_.eip, Counter::Batch);
        let inner_start = self.asm.offset();
        {
            let mut emitter = BatchEmitter {
                emitter: self,
                head: batch.loop_.eip,
                bail,
            };
            batch.plan.body.lower(&mut emitter);
            bail = emitter.bail;
        }
        let body_end = self.asm.offset();
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
        self.asm.emit(sub_imm(X, X4, X4, 1, false));
        self.asm.cbz(X, true, X4, inner);
        self.asm.bind(strips_done);
        {
            let mut emitter = BatchEmitter {
                emitter: self,
                head: batch.loop_.eip,
                bail,
            };
            batch.plan.body.lower_epilogue(&mut emitter);
            bail = emitter.bail;
        }
        let words = self.words_since(inner_start);
        let is_float = batch.plan.body.is_float();
        if is_float {
            // Count completed FP strips once per batch outside the inner loop
            // (zero inner-loop overhead; `X3` still holds the completed count).
            crate::runtime::emit_add_fp_counter(
                &mut self.asm,
                crate::runtime::FP_VECTOR_STRIPS,
                X3,
                T,
            );
            crate::runtime::emit_add_fp_counter(
                &mut self.asm,
                crate::runtime::FP_FAST_COMPLETIONS,
                X3,
                T,
            );
        }
        let recovery_start = self.asm.offset();
        // Publish the last logical latch's recovery operands once per strip.
        for &(param, value) in &batch.recovery {
            let dst = self.rd(param);
            self.batch_address(value, dst, U);
            self.copy(
                self.locations[param.0 as usize],
                Host::Register(dst),
                Type::I32,
            );
        }
        let words = words + self.words_since(recovery_start);
        self.asm.emit(sub(X, COUNT, COUNT, X5, Lsl, 0));
        self.asm.b(entry);

        if let Some(bail) = bail {
            self.asm.bind(bail);
            if is_float {
                crate::runtime::emit_inc_fp_counter(
                    &mut self.asm,
                    crate::runtime::FP_STRIP_BAILS,
                    1,
                    T,
                );
                crate::runtime::emit_inc_fp_counter(
                    &mut self.asm,
                    crate::runtime::FP_STRIP_REPLAYS,
                    lanes,
                    T,
                );
            }
            // Completed strips = planned (`X3`) - remaining (`X4`).
            self.asm.emit(sub(X, X3, X3, X4, Lsl, 0));
            self.asm.cbz(X, false, X3, scalar);
            {
                let mut emitter = BatchEmitter {
                    emitter: self,
                    head: batch.loop_.eip,
                    bail: None,
                };
                batch.plan.body.lower_epilogue(&mut emitter);
            }
            if is_float {
                crate::runtime::emit_add_fp_counter(
                    &mut self.asm,
                    crate::runtime::FP_VECTOR_STRIPS,
                    X3,
                    T,
                );
                crate::runtime::emit_add_fp_counter(
                    &mut self.asm,
                    crate::runtime::FP_FAST_COMPLETIONS,
                    X3,
                    T,
                );
            }
            self.imm(T, charge);
            self.asm.emit(mul(X, X5, X3, T));
            for &(param, value) in &batch.recovery {
                let dst = self.rd(param);
                self.batch_address(value, dst, U);
                self.copy(
                    self.locations[param.0 as usize],
                    Host::Register(dst),
                    Type::I32,
                );
            }
            self.asm.emit(sub(X, COUNT, COUNT, X5, Lsl, 0));
            self.asm.b(scalar);
        }

        // The scalar latch still charges and checkpoints before reaching
        // this edge. Retry the guards after one strip, unless the loop has
        // already ended in its scalar tail.
        self.asm.bind(backedge);
        self.asm.emit(sub_imm(W, X5, X5, 1, false));
        self.asm.cbz(W, true, X5, body);
        self.asm.b(entry);
        self.asm.bind(scalar);
        self.asm.emit(movz(W, X5, lanes as u16, 0));
        self.asm.bind(body);
        self.reciprocal_division = false;
        // Include the bounded long-range search and the scalar tail transfer,
        // without charging the 2x unrolled body as preheader admission overhead.
        let prefix = ((preheader_end - admission) / 4) as u32 + 3;
        let words = unroll2_words.unwrap_or(words);
        let pages = pages.map_or(0, |_| {
            crate::policy::proved::SPAN_SEARCH_WORDS * batch.spans.len() as u32
        });
        let classes = std::mem::take(&mut self.class_prices);
        let common = if classes.is_empty() {
            words
        } else {
            words - ((body_end - inner_start) / 4) as u32
        };
        let worst = classes.iter().map(|class| class.words).max().unwrap_or(0);
        (
            common + worst,
            price.map(|label| Price {
                label,
                admission: prefix + pages,
                common,
                classes,
            }),
        )
    }

    fn batch_minimum(&mut self, price: Option<Label>, scalar: Label) {
        if let Some(price) = price {
            self.asm.ldr_literal(T, price);
            self.asm.emit(cmp(X, X3, T, Lsl, 0));
            self.asm.b_cond(Cond::Lo, scalar);
        } else {
            self.asm.cbz(X, false, X3, scalar);
        }
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

    // Reverse streams use the complemented address of the first high byte
    // so their union grows upward in the same coordinates as forward streams.
    fn batch_span_start(&mut self, span: Span, backwards: bool, dst: Reg) {
        self.batch_address(span.start, dst, T);
        if backwards {
            self.batch_add(dst, dst, span.width - 1, T);
            self.asm.emit(mvn(W, dst, dst));
        }
    }

    fn batch_range(
        &mut self,
        span: Span,
        backwards: bool,
        pages: Option<Label>,
        scalar: Label,
        head: u32,
    ) {
        self.batch_span_start(span, backwards, X0);
        if backwards && span.width > 1 {
            // A first scalar access that straddles guest zero has no interval.
            self.imm(T, u64::from(u32::MAX - (span.width - 1)));
            self.asm.emit(cmp(W, X0, T, Lsl, 0));
            self.asm.b_cond(Cond::Hi, scalar);
        }
        let hit = self.asm.label();
        if let Some(search) = pages {
            self.imm(T, u64::from(span.step));
            self.asm.emit_all(&[
                mul(X, X1, X3, T),
                add_imm(X, X1, X1, span.width - 1, false),
                add(X, X1, X1, X0, Lsl, 0),
            ]);
            crate::facts::batch_span(&mut self.asm, span.write, backwards, search, hit);
            self.count(head, Counter::SpanMiss);
            self.batch_span_start(span, backwards, X1);
            self.asm.emit(sub(X, X2, X2, X1, Lsl, 0));
        } else {
            // The owner admits all scalar accesses of a job. The combined
            // interval must also stay contiguous in the alias.
            self.imm(T, 1 << 32);
            self.asm.emit(sub(X, X2, T, X0, Lsl, 0));
        }
        self.batch_span_limit(span, scalar);
        self.asm.bind(hit);
    }

    fn batch_disjoint(&mut self, a: Span, b: Span, backwards: bool, in_place: bool, scalar: Label) {
        self.batch_span_start(a, backwards, X0);
        self.batch_span_start(b, backwards, X1);
        let same = in_place.then(|| {
            let same = self.asm.label();
            self.asm.emit(cmp(W, X0, X1, Lsl, 0));
            self.asm.b_cond(Cond::Eq, same);
            same
        });
        for (l, r, span) in [(X1, X0, a), (X0, X1, b)] {
            self.asm.emit(sub(W, X2, l, r, Lsl, 0));
            self.batch_span_limit(span, scalar);
        }
        if let Some(same) = same {
            self.asm.bind(same);
        }
    }

    // X2 is the distance to the first unavailable byte. Reserve the scalar
    // iteration as well as all whole batches before that byte.
    fn batch_span_limit(&mut self, span: Span, scalar: Label) {
        if let Some((width, shift)) = add_immediate(u64::from(span.width)) {
            self.asm.emit(subs_imm(X, X2, X2, width, shift));
        } else {
            self.imm(T, u64::from(span.width));
            self.asm.emit(subs(X, X2, X2, T, Lsl, 0));
        }
        self.asm.b_cond(Cond::Lt, scalar);
        if span.step != 0 {
            self.batch_div(X2, X2, u64::from(span.step));
            self.batch_min(X2);
        }
    }

    fn batch_div(&mut self, dst: Reg, src: Reg, divisor: u64) {
        if divisor.is_power_of_two() {
            self.asm
                .emit(lsr_imm(X, dst, src, divisor.trailing_zeros()));
        } else if self.reciprocal_division {
            // The countdown and guest spans bound x * divisor below 2^64,
            // so rounding the reciprocal up cannot advance the quotient.
            let reciprocal = (1u128 << 64).div_ceil(u128::from(divisor)) as u64;
            let label = self.asm.label();
            self.reciprocals.push((reciprocal, label));
            self.asm.ldr_literal(U, label);
            self.asm.emit(umulh(dst, src, U));
        } else {
            self.imm(U, divisor);
            self.asm.emit(udiv(X, dst, src, U));
        }
    }

    fn batch_min(&mut self, bound: Reg) {
        self.asm
            .emit_all(&[cmp(X, X3, bound, Lsl, 0), csel(X, X3, X3, bound, Cond::Ls)]);
    }

    fn batch_trip(&mut self, trip: Trip, lanes: u32, scalar: Label) {
        self.batch_address(trip.value, T, U);
        let bound = trip.bound.map_or(Reg::ZR, |v| self.batch_gp(v, X0));
        // trip.value is the first latch value. Count the consecutive latch
        // tests that pass, leaving the first failing iteration scalar.
        if trip.again == Cc::ne {
            let (shift, inverse, mask) = trip.equality();
            let decrement = shift == 0 && inverse == u32::MAX;
            let (a, b) = if decrement { (T, bound) } else { (bound, T) };
            self.asm.emit(sub(W, T, a, b, Lsl, 0));
            let unbounded = self.asm.label();
            if shift != 0 {
                self.asm.emit(and_imm(W, U, T, (1u64 << shift) - 1));
                self.asm.cbz(W, true, U, unbounded);
                self.asm.emit(lsr_imm(W, T, T, shift));
            }
            if inverse != 1 && !decrement {
                self.imm(U, u64::from(inverse));
                self.asm.emit(mul(W, T, T, U));
            }
            if mask != u32::MAX {
                self.asm.emit(and_imm(W, T, T, u64::from(mask)));
            }
            self.batch_div(T, T, u64::from(lanes));
            self.batch_min(T);
            self.asm.bind(unbounded);
            return;
        }
        let (fail, signed, upper, strict) = match trip.again {
            Cc::e => (Cond::Ne, false, false, false),
            Cc::a => (Cond::Ls, false, false, true),
            Cc::ae => (Cond::Lo, false, false, false),
            Cc::b => (Cond::Hs, false, true, true),
            Cc::be => (Cond::Hi, false, true, false),
            Cc::g => (Cond::Le, true, false, true),
            Cc::ge => (Cond::Lt, true, false, false),
            Cc::l => (Cond::Ge, true, true, true),
            Cc::le => (Cond::Gt, true, true, false),
            _ => unreachable!(),
        };
        self.asm.emit(cmp(W, T, bound, Lsl, 0));
        self.asm.b_cond(fail, scalar);
        if trip.again == Cc::e {
            self.imm(T, u64::from(1 / lanes));
        } else {
            let backwards = trip.stride < 0;
            let toward_bound = upper != backwards;
            let bias = (if signed { 1u64 << 31 } else { 0 })
                ^ if backwards { u64::from(u32::MAX) } else { 0 };
            let normalize = |dst, src| match bias {
                0 => mov(W, dst, src),
                0xffff_ffff => mvn(W, dst, src),
                _ => eor_imm(W, dst, src, bias),
            };
            if bias != 0 {
                self.asm.emit(normalize(T, T));
            }
            if toward_bound {
                self.asm.emit(normalize(X0, bound));
            } else {
                self.asm.emit(mvn(W, X0, Reg::ZR));
            }
            self.asm.emit(sub(W, T, X0, T, Lsl, 0));
            if toward_bound && strict {
                self.asm.emit(sub_imm(W, T, T, 1, false));
            }
            self.batch_div(T, T, u64::from(trip.stride.unsigned_abs()));
            self.asm.emit(add_imm(X, T, T, 1, false));
            self.batch_div(T, T, u64::from(lanes));
        }
        self.batch_min(T);
    }
}
