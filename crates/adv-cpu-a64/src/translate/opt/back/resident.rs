//! Window checks and the owner access call for resident forms.

use super::*;
use crate::translate::{X0, X1, X2, X3, X4, X5};
use adv_cpu_core::memory::{LINEAR, READ, SHARED, WRITE};

const MEMORY_BASE: u32 = offset_of!(Runtime, memory_base) as u32;
const MEMORY_PAGES: u32 = offset_of!(Runtime, memory_pages) as u32;
const PLAIN_PAGES: u32 = offset_of!(Runtime, plain_pages) as u32;
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

    pub(super) fn page_crossings(&mut self) {
        let counter = offset_of!(Runtime, resident_page_crossings) as u32;
        for cross in std::mem::take(&mut self.page_crossings) {
            self.asm.bind(cross.crossing);
            self.mark(Part::Cold, cross.eip);
            let blocked = self.asm.label();
            // Preserve the flags and any scalar store value reloaded into T.
            self.asm
                .emit_all(&[mrs_nzcv(X5), stp(X, Index::Pre, T, X5, SP, -16)]);
            self.plain_span(cross.address, cross.width, true, blocked);
            self.asm
                .emit_all(&[ldp(X, Index::Post, T, X5, SP, 16), msr_nzcv(X5)]);
            self.sites.push((self.asm.offset(), cross.fault));
            self.asm.emit_all(&[
                cross.direct,
                ldr(X, X0, RT, counter),
                add_imm(X, X0, X0, 1, false),
                str(X, X0, RT, counter),
            ]);
            self.asm.b(cross.resume);
            self.asm.bind(blocked);
            self.asm
                .emit_all(&[ldp(X, Index::Post, T, X5, SP, 16), msr_nzcv(X5)]);
            self.asm.b(cross.fail);
        }
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

            if let Some(crossing) = crossing {
                self.asm.bind(crossing);
            }
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
            self.record_cold(ResidentCold::BlockedAccess, eip);
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
        self.plain_span(addr, bytes, write, miss);
    }

    fn plain_span(&mut self, addr: Reg, bytes: u32, write: bool, miss: Label) {
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
        self.asm.emit(lsr_imm(W, X0, X0, 12));

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
        }
        // The guest permission bitmap does not include the host
        // protection intersection when host pages exceed 4 KiB.
        self.asm.emit_all(&[
            add(W, X5, X0, X1, Lsl, 0),
            ldr(X, X4, RT, runtime::CLASSES),
            ldr_narrow(Narrow::Byte, X5, X4, X5),
        ]);
        if write {
            self.asm.tbz(false, X5, WRITE.trailing_zeros(), miss);
            self.asm
                .tbz(true, X5, adv_cpu_core::memory::EXEC.trailing_zeros(), miss);
        } else {
            self.asm.tbz(false, X5, READ.trailing_zeros(), miss);
            self.asm.tbz(true, X5, SHARED.trailing_zeros(), miss);
        }
        self.asm
            .emit_all(&[add_imm(W, X1, X1, 1, false), cmp(W, X1, X2, Lsl, 0)]);
        self.asm.b_cond(Cond::Ls, next);
    }
}

const SPAN_WORDS: [usize; 4] = adv_cpu_core::memory::WINDOW_SPAN_WORDS;
const WINDOW_SPAN_TABLES: u32 = offset_of!(Runtime, window_span_tables) as u32;

/// Test a range's first word. A large range follows the summaries to the
/// first unavailable page, irrespective of the number of eligible pages.
pub(super) fn window_span(asm: &mut Asm, write: bool, backwards: bool, search: Label, hit: Label) {
    let across = asm.label();
    let farther = asm.label();
    let blocked = asm.label();
    let table = 2 * u32::from(backwards) + u32::from(write);
    asm.emit_all(&[
        ldr(X, X5, RT, WINDOW_SPAN_TABLES + 8 * table),
        lsr_imm(X, T, X0, 12),
        lsr_imm(X, X2, T, 6),
        ldr_reg(X, X4, X5, X2, true),
        shiftv(X, Lsr, X4, X4, T),
        rbit(X, X4, X4),
        clz(X, X4, X4),
        add(X, X4, T, X4, Lsl, 0),
        lsl_imm(X, X4, X4, 12),
        cmp(X, X2, X1, Lsr, 18),
    ]);
    asm.b_cond(Cond::Ne, across);
    asm.emit(cmp(X, X1, X4, Lsl, 0));
    asm.b_cond(Cond::Lo, hit);
    asm.b(blocked);

    asm.bind(across);
    asm.emit_all(&[add_imm(X, X2, X2, 1, false), cmp(X, X4, X2, Lsl, 18)]);
    asm.b_cond(Cond::Hs, farther);
    asm.bind(blocked);
    asm.emit(mov(X, X2, X4));
    let limited = asm.label();
    asm.b(limited);
    asm.bind(farther);
    asm.bl(search);
    asm.emit(cmp(X, X1, X2, Lsl, 0));
    asm.b_cond(Cond::Lo, hit);
    asm.bind(limited);
}

fn span_word(asm: &mut Asm, level: usize, dst: Reg, index: Reg) {
    let offset = SPAN_WORDS[..level].iter().sum::<usize>() * 8;
    let base = if offset == 0 {
        X5
    } else {
        asm.emit(movz(X, U, (offset >> 16) as u16, 16));
        if offset as u16 != 0 {
            asm.emit(movk(X, U, offset as u16, 0));
        }
        asm.emit(add(X, U, X5, U, Lsl, 0));
        U
    };
    asm.emit(ldr_reg(X, dst, base, index, true));
}

/// X2 starts at the next word of pages. Ascend past empty suffixes, then
/// follow the first nonempty child at each level. X2 returns a byte bound.
pub(super) fn span_search(asm: &mut Asm, entry: Label) {
    let end = asm.label();
    let finish = asm.label();
    let found: [Label; 3] = std::array::from_fn(|_| asm.label());
    asm.bind(entry);
    for (level, &found) in (1..SPAN_WORDS.len()).zip(&found) {
        let (max, shift) = add_immediate(SPAN_WORDS[level - 1] as u64).unwrap();
        asm.emit(cmp_imm(X, X2, max, shift));
        asm.b_cond(Cond::Hs, end);
        asm.emit(lsr_imm(X, T, X2, 6));
        span_word(asm, level, X4, T);
        asm.emit(shiftv(X, Lsr, X4, X4, X2));
        asm.cbz(X, true, X4, found);
        asm.emit(add_imm(X, X2, T, 1, false));
    }
    asm.bind(end);
    asm.emit(movz(X, X2, 1 << 4, 16));
    asm.b(finish);
    for (level, found) in (1..SPAN_WORDS.len()).zip(found) {
        asm.bind(found);
        asm.emit_all(&[rbit(X, X4, X4), clz(X, X4, X4), add(X, X2, X2, X4, Lsl, 0)]);
        // The upper word also denies indices outside the guest address space.
        if level == SPAN_WORDS.len() - 1 {
            asm.emit(cmp_imm(X, X2, SPAN_WORDS[level - 1] as u32, false));
            asm.b_cond(Cond::Hs, end);
        }
        for child in (0..level).rev() {
            span_word(asm, child, T, X2);
            asm.emit_all(&[rbit(X, T, T), clz(X, T, T), add(X, X2, T, X2, Lsl, 6)]);
        }
        asm.b(finish);
    }
    asm.bind(finish);
    asm.emit_all(&[lsl_imm(X, X2, X2, 12), ret(Reg::LR)]);
}

/// Counters and the threshold word use a stable address for the emitted code.
pub(crate) struct BatchCache {
    pub(crate) head: u32,
    pub(crate) cells: Box<[std::cell::Cell<u64>; 7]>,
    pub(crate) split: Option<Box<SpanSplit>>,
}

impl BatchCache {
    pub(super) const THRESHOLD: u32 = 40;
    pub(super) const READ_LIMIT: usize = 0;
    pub(super) const WRITE_LIMIT: usize = 1;
    pub(super) const WRAP_LIMIT: usize = 6;

    pub(super) fn new(batch: &loops::Batch, diagnostics: bool) -> Self {
        let cells = Box::new([const { std::cell::Cell::new(0) }; 7]);
        let split = (cfg!(native)
            && (batch.loop_.sign_latch || batch.spans.iter().any(|span| span.step == 0)))
        .then(|| {
            let mut inputs = batch.inputs().collect::<Vec<_>>();
            inputs.sort_unstable();
            inputs.dedup();
            let streams = batch
                .spans
                .iter()
                .map(|span| {
                    let step = i64::from(span.step) * if batch.backwards { -1 } else { 1 };
                    SplitSpan {
                        input: inputs.binary_search(&span.start.base).unwrap(),
                        offset: span.start.offset,
                        width: span.width,
                        step,
                        write: span.write,
                    }
                })
                .collect();
            let published = batch
                .plan
                .published
                .iter()
                .map(|&value| (value, inputs.binary_search(&value).unwrap()))
                .collect();
            Box::new(SpanSplit {
                inputs,
                streams,
                published,
                lanes: u64::from(batch.plan.lanes),
                charge: u64::from(batch.loop_.charge) * u64::from(batch.plan.lanes),
                entry: std::cell::Cell::new(0),
                ns: std::cell::Cell::new(0.0),
                threshold: cells[Self::THRESHOLD as usize / 8].as_ptr(),
                classes: cells[2].as_ptr(),
                trigger: std::cell::Cell::new(256),
                diagnostics,
                counts: [const { std::cell::Cell::new(0) }; 3],
            })
        });
        cells[Self::THRESHOLD as usize / 8].set(u64::MAX);
        Self {
            head: batch.head,
            cells,
            split,
        }
    }

    pub(crate) fn counts(&self) -> [u64; 4] {
        [
            0,
            self.cells[2].get(),
            self.cells[3].get(),
            self.cells[4].get(),
        ]
    }
    pub(crate) fn guard_counts(&self) -> [u64; 3] {
        [Self::READ_LIMIT, Self::WRITE_LIMIT, Self::WRAP_LIMIT].map(|index| self.cells[index].get())
    }
}

pub(crate) struct SplitSpan {
    pub input: usize,
    pub offset: u32,
    pub width: u32,
    pub step: i64,
    pub write: bool,
}

/// The generated worker entry takes only inputs of the guarded batch.
pub(crate) struct SpanSplit {
    pub(crate) inputs: Vec<Value>,
    pub(crate) streams: Vec<SplitSpan>,
    pub(crate) published: Vec<(Value, usize)>,
    pub(crate) lanes: u64,
    pub(crate) charge: u64,
    pub(crate) entry: std::cell::Cell<usize>,
    pub(crate) ns: std::cell::Cell<f64>,
    threshold: *mut u64,
    classes: *mut u64,
    pub(crate) trigger: std::cell::Cell<u64>,
    diagnostics: bool,
    pub(crate) counts: [std::cell::Cell<u64>; 3],
}

// SAFETY: lowering hands the whole site's allocation to one owner before use.
// The threshold pointer refers to the stable allocation owned by the site.
unsafe impl Send for SpanSplit {}

impl SpanSplit {
    pub(crate) fn enable(&self, enabled: bool) {
        // SAFETY: the owner owns this site's stable cells and updates them
        // between generated reads.
        unsafe {
            self.threshold.write(if enabled {
                self.trigger.get()
            } else {
                u64::MAX
            })
        };
    }

    pub(crate) fn count(&self, index: usize) {
        if self.diagnostics {
            let count = &self.counts[index];
            count.set(count.get() + 1);
        }
    }

    pub(crate) fn diagnostics(&self) -> bool {
        self.diagnostics
    }

    pub(crate) fn add_classes(&self, classes: [u64; 3]) {
        if self.diagnostics {
            for (index, count) in classes.into_iter().enumerate() {
                // SAFETY: only the owner updates these counters after joining the workers.
                unsafe {
                    let cell = self.classes.add(index);
                    cell.write(cell.read() + count);
                }
            }
        }
    }
}

/// Like the Tier-1 access stub, calls the owner's access helper. The
/// optimized allocation can use full Q registers and owns its recovery,
/// so keep those registers and return the refusal bit to the form.
#[cfg(any(native, test))]
pub(crate) fn access_stub(helper: u64) -> Vec<u32> {
    let mut asm = Asm::new();
    asm.emit(mov(X, X0, RT));
    preserve_call(&mut asm, helper);
    asm.finish()
}

/// Calls a helper with arguments in x0-x5, preserving all allocated values.
pub(super) fn preserve_call(asm: &mut Asm, helper: u64) {
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

#[cfg(test)]
mod span_tests {
    use super::*;

    // spec: A64-OPT-10
    #[test]
    fn a_local_window_proof_has_two_loads_two_compares_and_no_call() {
        let mut asm = Asm::new();
        let search = asm.label();
        let hit = asm.label();
        window_span(&mut asm, false, false, search, hit);
        asm.bind(hit);
        let target = asm.offset();
        asm.emit(ret(Reg::LR));
        span_search(&mut asm, search);
        let words = asm.finish();
        assert_eq!(
            &words[..13],
            &[
                ldr(X, X5, RT, WINDOW_SPAN_TABLES),
                lsr_imm(X, T, X0, 12),
                lsr_imm(X, X2, T, 6),
                ldr_reg(X, X4, X5, X2, true),
                shiftv(X, Lsr, X4, X4, T),
                rbit(X, X4, X4),
                clz(X, X4, X4),
                add(X, X4, T, X4, Lsl, 0),
                lsl_imm(X, X4, X4, 12),
                cmp(X, X2, X1, Lsr, 18),
                b_cond(Cond::Ne, 16),
                cmp(X, X1, X4, Lsl, 0),
                b_cond(Cond::Lo, (target - 12 * 4) as i32),
            ],
        );
    }

    #[cfg(unix)]
    // spec: A64-OPT-10
    #[test]
    fn a_live_window_proof_obeys_revoke_watch_and_class_changes() {
        use crate::emulator::Emulator;
        use adv_cpu_core::memory::{Memory, PAGE_SIZE};

        const HOST_PAGE: usize = 16384;
        const BASE: u32 = 0x40000;
        const LENGTH: usize = 8192 * PAGE_SIZE;
        const TABLE: u64 = 0x2000_0000;
        const RUNTIME: u64 = 0x3000_0000;
        const SIZE: usize = 8 * (SPAN_WORDS[0] + SPAN_WORDS[1] + SPAN_WORDS[2] + SPAN_WORDS[3]);
        const STRIDE: u64 = SIZE.div_ceil(PAGE_SIZE) as u64 * PAGE_SIZE as u64;

        let mut memory = Memory::window_at_host_size(BASE, LENGTH, HOST_PAGE).unwrap();
        memory.map(BASE, LENGTH, READ | WRITE).unwrap();
        drop(memory.native_permissions());
        let mut emu = Emulator::new();
        emu.map(TABLE, 4 * STRIDE);
        emu.map(RUNTIME, PAGE_SIZE as u64);
        emu.set_x(RT, RUNTIME);
        let mut queries = Vec::new();
        let saved = Reg::new(19);
        let mut asm = Asm::new();
        let search = asm.label();
        for backwards in [false, true] {
            for write in [false, true] {
                queries.push(asm.offset());
                let hit = asm.label();
                asm.emit(mov(X, saved, Reg::LR));
                window_span(&mut asm, write, backwards, search, hit);
                asm.emit(ret(saved));
                asm.bind(hit);
                asm.emit_all(&[mov(X, X2, Reg::ZR), ret(saved)]);
            }
        }
        span_search(&mut asm, search);
        let code = asm.finish();

        let mut check = |memory: &mut Memory, read_first: u32, write_first: u32, write_end: u32| {
            for (i, table) in memory.window_span_tables(false).into_iter().enumerate() {
                let at = TABLE + i as u64 * STRIDE;
                emu.write(
                    RUNTIME + u64::from(WINDOW_SPAN_TABLES) + 8 * i as u64,
                    &at.to_le_bytes(),
                );
                // SAFETY: these tables are live throughout the owner call.
                emu.write(at, unsafe {
                    std::slice::from_raw_parts(table.cast(), SIZE)
                });
            }
            let last = BASE + LENGTH as u32 - 1;
            for (i, (begin, end, expected)) in [
                (BASE, last, u64::from(read_first)),
                (BASE, last, u64::from(write_first)),
                (!last, !BASE, 0),
                (
                    !last,
                    !BASE,
                    if write_end == 0 {
                        0
                    } else {
                        (1u64 << 32) - u64::from(write_end)
                    },
                ),
            ]
            .into_iter()
            .enumerate()
            {
                emu.set_x(X0, u64::from(begin));
                emu.set_x(X1, u64::from(end));
                emu.call_at(&code, queries[i]).unwrap();
                assert_eq!(emu.x(X2), expected, "query {i}: {begin:#x}..={end:#x}");
            }
        };

        check(&mut memory, 0, 0, 0);
        for offset in [
            0,
            63 * PAGE_SIZE,
            64 * PAGE_SIZE,
            (4096 - 64) * PAGE_SIZE,
            LENGTH - PAGE_SIZE,
        ] {
            let addr = BASE + offset as u32;
            let first = addr & !(HOST_PAGE as u32 - 1);
            memory.protect(addr, PAGE_SIZE, READ).unwrap();
            drop(memory.native_permissions());
            check(&mut memory, 0, first, first + HOST_PAGE as u32);
            memory.protect(addr, PAGE_SIZE, READ | WRITE).unwrap();
            drop(memory.native_permissions());
            check(&mut memory, 0, 0, 0);
        }

        let addr = BASE + (8192 / 2 * PAGE_SIZE) as u32;
        memory.watch_code(addr, 1);
        // The observed-code bit denies this page before the next class refresh.
        let tables = memory.window_span_tables(false);
        for table in tables.into_iter().take(2) {
            let page = addr as usize / PAGE_SIZE;
            // SAFETY: the live table includes the entire guest page space.
            assert_ne!(
                unsafe { table.add(page / 64).read() } & (1 << (page % 64)),
                0
            );
        }
    }
}
