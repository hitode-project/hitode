//! A batch retries at its scalar head when its trip, budget or stream facts
//! cannot cover the strip and its scalar successor. Resident page facts are
//! cached in page-aligned byte intervals, invalidated by the owner's epoch.

use super::*;
use crate::translate::{X4, X5};

use iced_x86::ConditionCode as Cc;
use loops::{Affine, Batch, BatchClass, ClassCosts, Span, Trip};

/// The kernel can emit instructions and read the checked addresses and
/// invariant SSA inputs. It cannot change the scalar CFG or recovery records.
pub(in crate::translate::opt) struct BatchEmitter<'b, 'f> {
    emitter: &'b mut Emitter<'f>,
    diagnostics: Option<&'b BatchCache>,
    #[cfg(native)]
    worker: Option<(&'b SpanSplit, u32)>,
}

impl BatchEmitter<'_, '_> {
    pub fn asm(&mut self) -> &mut Asm {
        &mut self.emitter.asm
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

    pub fn fixed_charge(&mut self, extra: i32) {
        let amount = u64::from(extra.unsigned_abs());
        let op = if let Some((imm, shift)) = add_immediate(amount) {
            if extra < 0 {
                sub_imm(X, X5, X5, imm, shift)
            } else {
                add_imm(X, X5, X5, imm, shift)
            }
        } else {
            self.emitter.imm(U, i64::from(extra) as u64);
            add(X, X5, X5, U, Lsl, 0)
        };
        self.asm().emit(op);
    }

    pub fn count_class(&mut self, class: BatchClass) {
        if let Some(cache) = self.diagnostics {
            let offset = 16 + 8 * class as u32;
            self.emitter.imm(U, cache.cells.as_ptr() as u64);
            self.asm().emit_all(&[
                ldr(X, T, U, offset),
                add_imm(X, T, T, 1, false),
                str(X, T, U, offset),
            ]);
        }
        #[cfg(native)]
        if let Some((site, slot)) = self.worker
            && site.diagnostics()
        {
            let offset = site.published.len() as u32 * 32 + 8 * class as u32;
            self.asm().emit_all(&[
                ldr(X, U, SP, slot),
                ldr(X, T, U, offset),
                add_imm(X, T, T, 1, false),
                str(X, T, U, offset),
            ]);
        }
    }

    pub fn address(&mut self, addr: Affine) -> Reg {
        self.emitter.batch_address(addr, T, U);
        let view = self.emitter.data_view();
        self.asm().emit(add_ext(X, T, view, T, Extend::Uxtw, 0));
        T
    }

    pub fn publish_lane(&mut self, value: Value, lane: u8, src: VReg) {
        let ty = self.emitter.func.ty(value);
        let location = self.emitter.locations[value.0 as usize];
        if ty == Type::I32 {
            let dst = self.emitter.rd(value);
            self.asm().emit(vumov(2, dst, src, u32::from(lane)));
            self.emitter.copy(location, Host::Register(dst), ty);
        } else {
            let dst = self.emitter.vd(value);
            if lane == 1 {
                self.asm().emit(vext(dst, src, src, 8));
            }
            self.asm()
                .emit(vmov(false, dst, if lane == 0 { src } else { dst }));
            self.emitter.copy(location, Host::Vector(dst), ty);
        }
        #[cfg(native)]
        if let Some((site, slot)) = self.worker {
            let i = site
                .published
                .iter()
                .position(|&(v, _)| v == value)
                .unwrap();
            self.asm().emit_all(&[
                ldr(X, U, SP, slot),
                str(X, Reg::ZR, U, i as u32 * 32 + 16),
                str(X, Reg::ZR, U, i as u32 * 32 + 24),
            ]);
        }
    }

    pub fn publish_bits(&mut self, value: Value, keep: u32, set: u32) {
        let loc = self.emitter.locations[value.0 as usize];
        let dst = self.emitter.rd(value);
        if keep & !set == 0 {
            self.emitter.imm(dst, u64::from(set));
        } else {
            let mut src = self.emitter.gp_at(loc, T);
            for (op, mask) in [(IntOp::And, keep | set), (IntOp::Or, set)] {
                if op == IntOp::And && mask == u32::MAX || op == IntOp::Or && mask == 0 {
                    continue;
                }
                if let Some(word) = int_immediate(op, W, dst, src, u64::from(mask)) {
                    self.asm().emit(word);
                } else {
                    self.emitter.imm(U, u64::from(mask));
                    self.asm().emit(if op == IntOp::And {
                        and(W, dst, src, U, Lsl, 0)
                    } else {
                        orr(W, dst, src, U, Lsl, 0)
                    });
                }
                src = dst;
            }
        }
        self.emitter.copy(loc, Host::Register(dst), Type::I32);
        #[cfg(native)]
        if let Some((site, slot)) = self.worker {
            let i = site
                .published
                .iter()
                .position(|&(v, _)| v == value)
                .unwrap();
            let offset = i as u32 * 32 + 16;
            self.asm()
                .emit_all(&[ldr(X, X0, SP, slot), ldr(W, T, X0, offset)]);
            self.emitter.imm(U, u64::from(keep & !set));
            self.asm()
                .emit_all(&[and(W, T, T, U, Lsl, 0), str(W, T, X0, offset)]);
        }
    }
}

impl Emitter<'_> {
    pub(super) fn batch(
        &mut self,
        batch: &Batch,
        pages: Option<(Label, &BatchCache)>,
        backedge: Label,
        diagnostics: bool,
        spill_cells: usize,
    ) -> (u32, Option<ClassCosts>) {
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
        self.asm.cbz(X, false, X3, scalar);

        if let Some((_, cache)) = pages {
            self.batch_cache(cache, batch.spans.len());
        }
        for (index, &span) in batch.spans.iter().enumerate() {
            self.batch_range(
                span,
                batch.backwards,
                pages.map(|(walker, _)| {
                    (
                        walker,
                        BatchCache::INTERVALS + (16 * index) as u32,
                        diagnostics,
                    )
                }),
                scalar,
            );
            self.asm.cbz(X, false, X3, scalar);
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
        self.asm.cbz(X, false, X3, scalar);
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
        #[cfg(native)]
        let split = pages
            .and_then(|(_, cache)| cache.split.as_deref().map(|site| (site, cache)))
            .map(|(site, cache)| {
                let offer = self.asm.label();
                let serial = self.asm.label();
                let finish = self.asm.label();
                if !batch.crossings.is_empty() {
                    self.imm(X4, cache.cells.as_ptr() as u64);
                }
                self.asm
                    .emit_all(&[ldr(X, T, X4, BatchCache::THRESHOLD), cmp(X, X3, T, Lsl, 0)]);
                self.asm.b_cond(Cond::Hs, offer);
                if diagnostics {
                    self.count_cell(&site.counts[1]);
                }
                self.asm.bind(serial);
                (site, offer, serial, finish)
            });

        self.imm(T, charge);
        self.asm.emit_all(&[mul(X, X5, X3, T), mov(X, X4, X3)]);
        self.asm.bind(inner);
        let inner_start = self.asm.offset();
        let mut emitter = BatchEmitter {
            emitter: self,
            diagnostics: pages.filter(|_| diagnostics).map(|(_, cache)| cache),
            #[cfg(native)]
            worker: None,
        };
        if !batch.plan.conditional_stores {
            emitter.count_class(BatchClass::Uniform);
        }
        let mut cases = batch.plan.body.lower(&mut emitter);
        let tail_start = self.asm.offset();
        self.advance_ivs(batch, None);
        self.asm.emit(sub_imm(X, X4, X4, 1, false));
        self.asm.cbz(X, true, X4, inner);
        let words = self.words_since(inner_start);
        let words = if let Some(cases) = &mut cases {
            let tail = self.words_since(tail_start);
            for words in &mut cases.batch {
                *words += tail;
            }
            *cases.batch.iter().max().unwrap()
        } else {
            words
        };
        #[cfg(native)]
        if let Some((_, _, _, finish)) = split {
            self.asm.bind(finish);
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

        #[cfg(native)]
        if let Some((site, offer, serial, finish)) = split {
            self.asm.bind(offer);
            self.span_offer(site, batch, serial, finish);
            site.entry.set(self.asm.offset());
            self.span_worker(site, batch, spill_cells as u64 * 16);
        }
        #[cfg(not(native))]
        let _ = spill_cells;

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
        (words, cases)
    }

    fn advance_ivs(&mut self, batch: &Batch, count: Option<Reg>) {
        for iv in &batch.loop_.ivs {
            let loc = self.locations[iv.base.0 as usize];
            let src = self.gp_at(loc, T);
            let dst = match loc {
                Host::Register(r) => r,
                _ => T,
            };
            let step = (iv.stride as u32).wrapping_mul(u32::from(batch.plan.lanes));
            if let Some(count) = count {
                self.imm(U, u64::from(step));
                self.asm
                    .emit_all(&[mul(W, U, U, count), add(W, dst, src, U, Lsl, 0)]);
            } else {
                self.batch_add(dst, src, step, U);
            }
            self.copy(loc, Host::Register(dst), Type::I32);
        }
    }

    #[cfg(native)]
    fn span_offer(&mut self, site: &SpanSplit, batch: &Batch, serial: Label, finish: Label) {
        let outputs = site.inputs.len() as u32 * 16;
        let count_slot = outputs + site.published.len() as u32 * 16;
        let args = count_slot + 16;
        self.stack_frame(u64::from(args), true);
        self.asm.emit(str(X, X3, SP, count_slot));
        for (i, &input) in site.inputs.iter().enumerate() {
            let offset = 16 * i as u32;
            let loc = self.locations[input.0 as usize];
            match loc {
                Host::Register(r) => self
                    .asm
                    .emit_all(&[str(X, r, SP, offset), str(X, Reg::ZR, SP, offset + 8)]),
                Host::Vector(r) => self.asm.emit(vstr(VMem::Q, r, SP, offset)),
                Host::Spill(slot, ty) => {
                    if ty == Type::V128 {
                        self.asm.emit_all(&[
                            vldr(VMem::Q, A, SP, args + slot * 16),
                            vstr(VMem::Q, A, SP, offset),
                        ]);
                    } else {
                        self.asm.emit_all(&[
                            ldr(W, T, SP, args + slot * 16),
                            str(X, T, SP, offset),
                            str(X, Reg::ZR, SP, offset + 8),
                        ]);
                    }
                }
                Host::Constant(bits) => {
                    for (offset, bits) in [(offset, bits as u64), (offset + 8, (bits >> 64) as u64)]
                    {
                        self.imm(T, bits);
                        self.asm.emit(str(X, T, SP, offset));
                    }
                }
                Host::Flags(_) => unreachable!("span inputs are values"),
            }
        }
        self.asm.emit(mov(X, X0, RT));
        self.imm(X1, std::ptr::from_ref(site) as u64);
        self.asm.emit(add_imm(X, X2, SP, 0, false));
        self.asm.emit(add_imm(X, X4, SP, outputs, false));
        let call = self.asm.label();
        self.asm.bl(call);
        self.asm.emit(ldr(X, X3, SP, count_slot));
        let declined = self.asm.label();
        self.asm.cbz(X, false, X0, declined);
        for (i, &(value, _)) in site.published.iter().enumerate() {
            let offset = outputs + i as u32 * 16;
            match self.locations[value.0 as usize] {
                Host::Register(r) => self.asm.emit(ldr(W, r, SP, offset)),
                Host::Vector(r) => self.asm.emit(vldr(VMem::Q, r, SP, offset)),
                Host::Spill(n, Type::V128) => self.asm.emit_all(&[
                    vldr(VMem::Q, A, SP, offset),
                    vstr(VMem::Q, A, SP, args + n * 16),
                ]),
                Host::Spill(n, _) => self
                    .asm
                    .emit_all(&[ldr(W, T, SP, offset), str(W, T, SP, args + n * 16)]),
                _ => unreachable!("published value has a location"),
            }
        }
        self.stack_frame(u64::from(args), false);
        self.asm.emit(mov(X, X5, X0));
        self.advance_ivs(batch, Some(X3));
        self.asm.b(finish);
        self.asm.bind(declined);
        self.stack_frame(u64::from(args), false);
        self.asm.b(serial);
        self.asm.bind(call);
        super::resident::preserve_call(
            &mut self.asm,
            crate::native::offer_span as *const () as u64,
        );
    }

    #[cfg(native)]
    fn span_worker(&mut self, site: &SpanSplit, batch: &Batch, spills: u64) {
        // The detached entry uses the same allocation and batch body as the
        // resident strip. It saves AAPCS64 state and reads through the alias.
        const SAVED: u32 = 160;
        self.stack_frame(u64::from(SAVED), true);
        for n in (19..30).step_by(2) {
            self.asm.emit(stp(
                X,
                Index::Offset,
                Reg::new(n),
                Reg::new(n + 1),
                SP,
                i32::from(n - 19) * 8,
            ));
        }
        for n in (8..16).step_by(2) {
            self.asm.emit(stp_v(
                VSize::D,
                Index::Offset,
                VReg::new(n),
                VReg::new(n + 1),
                SP,
                96 + i32::from(n - 8) * 8,
            ));
        }
        self.stack_frame(spills + 16, true);
        self.asm.emit(str(X, X4, SP, spills as u32));
        self.asm
            .emit_all(&[mov(X, WINDOW, X1), mov(X, X4, X3), mov(X, X5, Reg::ZR)]);
        for (i, &input) in site.inputs.iter().enumerate() {
            let offset = i as u32 * 16;
            let loc = self.locations[input.0 as usize];
            if matches!(loc, Host::Constant(_)) {
                continue;
            }
            let ty = self.func.ty(input);
            if ty == Type::V128 {
                self.asm.emit(vldr(VMem::Q, A, X0, offset));
                self.copy(loc, Host::Vector(A), ty);
            } else {
                self.asm.emit(ldr(W, T, X0, offset));
                if let Some(iv) = batch.loop_.ivs.iter().find(|iv| iv.base == input) {
                    let step = (iv.stride as u32).wrapping_mul(u32::from(batch.plan.lanes));
                    self.imm(U, u64::from(step));
                    self.asm
                        .emit_all(&[mul(W, U, U, X2), add(W, T, T, U, Lsl, 0)]);
                }
                self.copy(loc, Host::Register(T), ty);
            }
        }
        let inner = self.asm.label();
        self.asm.bind(inner);
        let mut emitter = BatchEmitter {
            emitter: self,
            diagnostics: None,
            worker: Some((site, spills as u32)),
        };
        if !batch.plan.conditional_stores {
            emitter.count_class(BatchClass::Uniform);
        }
        batch.plan.body.lower(&mut emitter);
        self.advance_ivs(batch, None);
        self.asm.emit(sub_imm(X, X4, X4, 1, false));
        self.asm.cbz(X, true, X4, inner);
        if !site.published.is_empty() {
            self.asm.emit(ldr(X, X1, SP, spills as u32));
            for (i, &(value, _)) in site.published.iter().enumerate() {
                let offset = i as u32 * 32;
                if self.func.ty(value) == Type::V128 {
                    let reg = self.vr(value, A);
                    self.asm.emit(vstr(VMem::Q, reg, X1, offset));
                } else {
                    let reg = self.gp(value, T);
                    self.asm.emit(str(W, reg, X1, offset));
                }
            }
        }
        self.asm.emit(mov(X, X0, X5));
        self.stack_frame(spills + 16, false);
        for n in (8..16).step_by(2) {
            self.asm.emit(ldp_v(
                VSize::D,
                Index::Offset,
                VReg::new(n),
                VReg::new(n + 1),
                SP,
                96 + i32::from(n - 8) * 8,
            ));
        }
        for n in (19..30).step_by(2) {
            self.asm.emit(ldp(
                X,
                Index::Offset,
                Reg::new(n),
                Reg::new(n + 1),
                SP,
                i32::from(n - 19) * 8,
            ));
        }
        self.stack_frame(u64::from(SAVED), false);
        self.asm.emit(ret(Reg::LR));
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

    fn batch_cache(&mut self, cache: &BatchCache, streams: usize) {
        self.imm(X4, cache.cells.as_ptr() as u64);
        self.asm.emit_all(&[
            ldr(X, X0, RT, offset_of!(Runtime, page_fact_epoch) as u32),
            ldr(X, T, X4, 0),
            cmp(X, X0, T, Lsl, 0),
        ]);
        let current = self.asm.label();
        self.asm.b_cond(Cond::Eq, current);
        self.asm.emit(str(X, X0, X4, 0));
        for index in 0..streams {
            store_span(
                &mut self.asm,
                Reg::ZR,
                Reg::ZR,
                BatchCache::INTERVALS + (16 * index) as u32,
            );
        }
        self.asm.bind(current);
    }

    fn batch_range(
        &mut self,
        span: Span,
        backwards: bool,
        pages: Option<(Label, u32, bool)>,
        scalar: Label,
    ) {
        const PAGE_SHIFT: u32 = crate::translate::access::PAGE_SHIFT;
        self.batch_span_start(span, backwards, X0);
        if backwards && span.width > 1 {
            // A first scalar access that straddles guest zero has no interval.
            self.imm(T, u64::from(u32::MAX - (span.width - 1)));
            self.asm.emit(cmp(W, X0, T, Lsl, 0));
            self.asm.b_cond(Cond::Hi, scalar);
        }
        if let Some((walker, slot, diagnostics)) = pages {
            let hit = self.asm.label();
            let miss = self.asm.label();
            if span.step.is_power_of_two() {
                self.asm
                    .emit(add(X, X1, X0, X3, Lsl, span.step.trailing_zeros()));
            } else if span.step == 0 {
                self.asm.emit(mov(X, X1, X0));
            } else {
                self.imm(T, u64::from(span.step));
                self.asm
                    .emit_all(&[mul(X, X1, X3, T), add(X, X1, X1, X0, Lsl, 0)]);
            }
            self.asm.emit(add_imm(X, X1, X1, span.width - 1, false));
            cached_span(&mut self.asm, slot, miss, hit);
            self.asm.bind(miss);
            self.asm.emit_all(&[
                lsr_imm(X, X1, X1, PAGE_SHIFT),
                lsr_imm(W, X0, X0, PAGE_SHIFT),
                stp(X, Index::Pre, X4, X0, SP, -16),
            ]);
            self.imm(
                X2,
                if backwards {
                    u64::from(u32::MAX >> PAGE_SHIFT)
                } else {
                    0
                },
            );
            self.imm(X5, if span.write { 8 } else { 0 });
            self.asm.bl(walker);
            self.asm.emit(ldp(X, Index::Post, X4, X2, SP, 16));
            if diagnostics {
                self.asm.emit_all(&[
                    lsr_imm(X, T, X0, PAGE_SHIFT),
                    cmp(X, T, X1, Lsl, 0),
                    cset(X, U, Cond::Ls),
                    sub(X, T, T, X2, Lsl, 0),
                    add(X, T, T, U, Lsl, 0),
                    ldr(X, U, X4, 8),
                    add(X, T, T, U, Lsl, 0),
                    str(X, T, X4, 8),
                ]);
            }
            self.asm.emit(lsl_imm(X, X2, X2, PAGE_SHIFT));
            self.asm.emit(cmp(X, X2, X0, Lsl, 0));
            let unavailable = self.asm.label();
            let replace = self.asm.label();
            self.asm.b_cond(Cond::Eq, unavailable);
            load_span(&mut self.asm, T, U, slot);
            self.asm.emit(cmp(X, X2, U, Lsl, 0));
            self.asm.b_cond(Cond::Hi, replace);
            self.asm.emit(cmp(X, X0, T, Lsl, 0));
            self.asm.b_cond(Cond::Lo, replace);
            self.asm.emit_all(&[
                cmp(X, U, X0, Lsl, 0),
                csel(X, U, U, X0, Cond::Hs),
                cmp(X, X2, T, Lsl, 0),
                csel(X, X2, X2, T, Cond::Ls),
            ]);
            store_span(&mut self.asm, X2, U, slot);
            self.asm.b(unavailable);
            self.asm.bind(replace);
            store_span(&mut self.asm, X2, X0, slot);
            self.asm.bind(unavailable);
            self.batch_span_start(span, backwards, X1);
            self.asm.emit(sub(X, X2, X0, X1, Lsl, 0));
            self.batch_span_limit(span, scalar);
            self.asm.bind(hit);
        } else {
            // The owner admits all scalar accesses of a job. The combined
            // interval must also stay contiguous in the alias.
            self.imm(T, 1 << 32);
            self.asm.emit(sub(X, X2, T, X0, Lsl, 0));
            self.batch_span_limit(span, scalar);
        }
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
        } else {
            let (product, shift, adjust) = reciprocal(divisor);
            self.imm(U, product);
            if adjust {
                self.asm.emit_all(&[
                    umulh(U, src, U),
                    sub(X, dst, src, U, Lsl, 0),
                    add(X, dst, U, dst, Lsr, 1),
                    lsr_imm(X, dst, dst, shift),
                ]);
            } else {
                self.asm
                    .emit_all(&[umulh(dst, src, U), lsr_imm(X, dst, dst, shift)]);
            }
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

fn load_span(asm: &mut Asm, lo: Reg, hi: Reg, slot: u32) {
    if slot <= 504 {
        asm.emit(ldp(X, Index::Offset, lo, hi, X4, slot as i32));
    } else {
        asm.emit_all(&[ldr(X, lo, X4, slot), ldr(X, hi, X4, slot + 8)]);
    }
}

fn store_span(asm: &mut Asm, lo: Reg, hi: Reg, slot: u32) {
    if slot <= 504 {
        asm.emit(stp(X, Index::Offset, lo, hi, X4, slot as i32));
    } else {
        asm.emit_all(&[str(X, lo, X4, slot), str(X, hi, X4, slot + 8)]);
    }
}

fn cached_span(asm: &mut Asm, slot: u32, miss: Label, hit: Label) {
    load_span(asm, X2, T, slot);
    asm.emit(cmp(X, X0, X2, Lsl, 0));
    asm.b_cond(Cond::Lo, miss);
    asm.emit(cmp(X, X1, T, Lsl, 0));
    asm.b_cond(Cond::Lo, hit);
}

/// Multiplication by the rounded-up reciprocal needs either one shift or
/// an additional average when that reciprocal needs 65 bits.
fn reciprocal(divisor: u64) -> (u64, u32, bool) {
    let shift = 63 - divisor.leading_zeros();
    let numerator = 1u128 << (64 + shift);
    let divisor = u128::from(divisor);
    let quotient = numerator / divisor;
    let remainder = numerator % divisor;
    if divisor - remainder < 1 << shift {
        ((quotient + 1) as u64, shift, false)
    } else {
        (
            (2 * quotient + u128::from(2 * remainder >= divisor) + 1) as u64,
            shift,
            true,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // spec: A64-OPT-10
    #[test]
    fn a_cached_span_checks_both_bounds_without_calling_the_walker() {
        let mut asm = Asm::new();
        let miss = asm.label();
        let hit = asm.label();
        cached_span(&mut asm, BatchCache::INTERVALS, miss, hit);
        assert_eq!(asm.offset() / 4, 5);
        assert_eq!(
            asm.words()[0],
            ldp(X, Index::Offset, X2, T, X4, BatchCache::INTERVALS as i32)
        );
        assert_eq!(asm.words()[1], cmp(X, X0, X2, Lsl, 0));
        assert_eq!(asm.words()[3], cmp(X, X1, T, Lsl, 0));
    }

    // spec: A64-OPT-10
    #[test]
    fn reciprocal_matches_unsigned_division() {
        let mut n = 0x8242_8900_faff_0853u64;
        for divisor in (3..8192)
            .chain([u32::MAX as u64, u64::MAX / 2, u64::MAX])
            .filter(|d| !d.is_power_of_two())
        {
            let (multiplier, shift, adjust) = reciprocal(divisor);
            n ^= n << 13;
            n ^= n >> 7;
            n ^= n << 17;
            for value in [0, divisor - 1, divisor, n, u64::MAX] {
                let high = ((u128::from(value) * u128::from(multiplier)) >> 64) as u64;
                let adjusted = if adjust {
                    high + ((value - high) >> 1)
                } else {
                    high
                };
                assert_eq!(adjusted >> shift, value / divisor);
            }
        }
    }
}
