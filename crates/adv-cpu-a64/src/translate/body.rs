//! Argument forwarding, register saves and constant memory-call expansions.

use super::decode::{Alu, ESP, Op, Src};
use super::{Cold, RUNTIME, Translator, WINDOW, X0, X1, X3, X4, X6, X9, X16, X17, host};
use crate::encode::Extend::Uxtw;
use crate::encode::Index::{Post, Pre};
use crate::encode::Shift::Lsl;
use crate::encode::Size::{W, X};
use crate::encode::*;
use crate::runtime::{self, Exit};

const SP: Reg = Reg::new(31);

impl Translator<'_> {
    pub(super) fn body_call(&mut self, body: adv_cpu_core::body::DirectBody) {
        let call = body.call as usize as u64;
        self.body_simd(false);
        self.emit(stp(X, Pre, X9, super::X12, SP, -16));
        self.emit(add_imm(X, X0, RUNTIME, runtime::BODY, false));
        self.emit(movz(X, X16, call as u16, 0));
        for shift in [16, 32, 48] {
            self.emit(movk(X, X16, (call >> shift) as u16, shift));
        }
        self.emit(blr(X16));
        self.emit(ldp(X, Post, X9, super::X12, SP, 16));
        self.body_simd(true);
    }

    /// All reads precede writes for moves, including overlapping copies.
    pub(super) fn inline_body(
        &mut self,
        kind: adv_cpu_core::body::Inline,
        len: u32,
        slow: crate::asm::Label,
        known: [Option<u32>; 2],
    ) {
        use adv_cpu_core::body::Inline;
        let x2 = Reg::new(2);
        if len != 0 {
            let pushes = self.body_pushes(self.index);
            let granted: [bool; 2] = std::array::from_fn(|arg| {
                self.body_stack_span().is_some()
                    && match pushes.get(arg) {
                        Some(Src::Imm(_)) => true,
                        Some(Src::Reg(reg)) => self.body_invariant(*reg),
                        _ => false,
                    }
            });
            for (arg, _) in known
                .iter()
                .enumerate()
                .take(if kind == Inline::Set { 1 } else { 2 })
            {
                if !granted[arg] {
                    self.emit(ldr(W, X3, RUNTIME, runtime::BODY_STACK));
                    self.emit(lsr_imm(W, X4, if arg == 0 { X1 } else { x2 }, 12));
                    self.emit(sub(W, X4, X4, X3, Lsl, 0));
                    self.asm.cbz(W, false, X4, slow);
                }
            }

            if granted
                .iter()
                .take(if kind == Inline::Set { 1 } else { 2 })
                .all(|grant| *grant)
            {
                if known
                    .iter()
                    .take(if kind == Inline::Set { 1 } else { 2 })
                    .flatten()
                    .any(|at| at % 4096 + len > 4096)
                {
                    self.asm.b(slow);
                }
            } else {
                self.body_page_known(X1, len, kind != Inline::Compare, slow, known[0]);
                if kind != Inline::Set {
                    if let (Some(a), Some(b)) = (known[0], known[1]) {
                        if a >> 12 != b >> 12 {
                            self.body_page_known(x2, len, false, slow, Some(b));
                        } else if b % 4096 + len > 4096 {
                            self.asm.b(slow);
                        }
                    } else {
                        let same = self.asm.label();
                        self.emit(eor(W, X4, X1, x2, Lsl, 0));
                        self.emit(lsr_imm(W, X4, X4, 12));
                        self.asm.cbz(W, false, X4, same);
                        self.body_page_known(x2, len, false, slow, known[1]);
                        self.asm.bind(same);
                        self.page_crossing(x2, len, slow);
                    }
                }
            }
            self.emit(add_ext(X, X16, WINDOW, X1, Uxtw, 0));
            if kind != Inline::Set {
                self.emit(add_ext(X, X17, WINDOW, x2, Uxtw, 0));
            }
        }
        match kind {
            Inline::Move | Inline::Set => {
                let mut chunks = Vec::new();
                let mut at = 0;
                while at < len {
                    let size = [VMem::Q, VMem::D, VMem::S, VMem::H, VMem::B]
                        .into_iter()
                        .find(|size| size.bytes() <= len - at)
                        .unwrap();
                    chunks.push((size, at));
                    at += size.bytes();
                }
                if kind == Inline::Set && len != 0 {
                    self.emit(vdup(Arr::new(0, true), VReg::new(0), x2));
                }
                if kind == Inline::Move {
                    let mut n = 0;
                    while n < chunks.len() {
                        let (size, at) = chunks[n];
                        if size == VMem::Q
                            && chunks.get(n + 1).is_some_and(|(next, _)| *next == VMem::Q)
                        {
                            self.emit(ldp_v(
                                VSize::Q,
                                Index::Offset,
                                VReg::new(n as u8),
                                VReg::new(n as u8 + 1),
                                X17,
                                at as i32,
                            ));
                            n += 2;
                        } else {
                            self.emit(vldr(size, VReg::new(n as u8), X17, at));
                            n += 1;
                        }
                    }
                }
                let mut n = 0;
                while n < chunks.len() {
                    let (size, at) = chunks[n];
                    let v = if kind == Inline::Set { 0 } else { n as u8 };
                    if size == VMem::Q
                        && chunks.get(n + 1).is_some_and(|(next, _)| *next == VMem::Q)
                    {
                        self.emit(stp_v(
                            VSize::Q,
                            Index::Offset,
                            VReg::new(v),
                            VReg::new(if kind == Inline::Set { 0 } else { v + 1 }),
                            X16,
                            at as i32,
                        ));
                        n += 2;
                    } else {
                        self.emit(vstr(size, VReg::new(v), X16, at));
                        n += 1;
                    }
                }
            }
            Inline::Compare => {
                if len == 0 {
                    self.mov_imm(X0, 0);
                    return;
                }
                if len == 8 {
                    self.emit(ldr(X, X0, X16, 0));
                    self.emit(ldr(X, X6, X17, 0));
                    self.emit(rev(X, X0, X0));
                    self.emit(rev(X, X6, X6));
                    self.emit(subs(X, X4, X0, X6, Lsl, 0));
                } else {
                    let (next, different) = (self.asm.label(), self.asm.label());
                    self.mov_imm(X3, len);
                    self.asm.bind(next);
                    self.emit(ldrb_indexed(Post, X0, X16, 1));
                    self.emit(ldrb_indexed(Post, X6, X17, 1));
                    self.emit(subs(W, X4, X0, X6, Lsl, 0));
                    self.asm.b_cond(Cond::Ne, different);
                    self.emit(sub_imm(W, X3, X3, 1, false));
                    self.asm.cbz(W, true, X3, next);
                    self.asm.bind(different);
                }
                self.emit(cset(W, X0, Cond::Hi));
                self.emit(cset(W, X6, Cond::Lo));
                self.emit(sub(W, X0, X0, X6, Lsl, 0));
            }
        }
    }

    fn body_invariant(&self, reg: u8) -> bool {
        use super::simd::Loc;
        reg != ESP
            && self.trace.insts.iter().all(|inst| match inst.op {
                Op::Mov(Src::Reg(dst), _)
                | Op::Alu(_, Src::Reg(dst), _)
                | Op::Step(_, dst)
                | Op::Pop(dst) => dst != reg,
                Op::Body(n) => {
                    let bit = match reg {
                        0 => adv_cpu_core::body::EAX,
                        1 => adv_cpu_core::body::ECX,
                        2 => adv_cpu_core::body::EDX,
                        _ => 0,
                    };
                    self.trace.bodies[usize::from(n)].body.outputs & bit == 0
                }
                Op::Simd(simd) => simd.dst != Loc::Gpr(reg),
                Op::Push(_)
                | Op::Nop
                | Op::Jcc(..)
                | Op::Jmp(_)
                | Op::Ret(_)
                | Op::Direction(_) => true,
                _ => false,
            })
    }

    /// A balanced trace can check its argument-stack page once before its
    /// loop labels. Backedges then reuse that grant until the next dispatch.
    pub(super) fn body_stack_span(&self) -> Option<(i32, u32)> {
        use super::trace::{Edge, Next};
        let mut offsets = Vec::new();
        let (mut delta, mut low, mut high) = (0i32, 0i32, 0i32);
        let mut found = false;
        for (index, inst) in self.trace.insts.iter().enumerate() {
            if index != 0 && self.trace.head(index) {
                return None;
            }
            offsets.push(delta);
            match inst.op {
                Op::Push(_) => delta = delta.checked_sub(4)?,
                Op::Pop(reg) if reg != ESP => delta = delta.checked_add(4)?,
                Op::Alu(Alu::Add, Src::Reg(ESP), Src::Imm(n)) => {
                    delta = delta.checked_add(n as i32)?
                }
                Op::Alu(Alu::Sub, Src::Reg(ESP), Src::Imm(n)) => {
                    delta = delta.checked_sub(n as i32)?
                }
                Op::Body(n) if !self.trace.bodies[usize::from(n)].entry => {
                    let bound = self.trace.bodies[usize::from(n)];
                    if !self.body_pushes(index).is_empty() {
                        found = true;
                        let bytes = bound
                            .body
                            .inputs
                            .iter()
                            .flatten()
                            .filter_map(|input| {
                                if let adv_cpu_core::body::Input::Stack(k) = input {
                                    Some(4 * (i32::from(*k) + 1))
                                } else {
                                    None
                                }
                            })
                            .max()
                            .unwrap();
                        low = low.min(delta);
                        high = high.max(delta.checked_add(bytes)?);
                    }
                    delta = delta.checked_add(i32::from(bound.body.pop))?;
                }
                Op::Mov(Src::Reg(reg), _) | Op::Alu(_, Src::Reg(reg), _) | Op::Step(_, reg)
                    if reg != ESP => {}
                Op::Nop | Op::Jcc(..) | Op::Jmp(_) | Op::Ret(_) | Op::Direction(_) => {}
                Op::Simd(simd) if simd.dst != super::simd::Loc::Gpr(ESP) => {}
                _ => return None,
            }
        }
        offsets.push(delta);
        for (index, inst) in self.trace.insts.iter().enumerate() {
            let next = if let Next::Edge(edge) = inst.next {
                Some(edge)
            } else {
                None
            };
            for edge in [inst.taken, next] {
                if let Some(Edge::Internal(to)) = edge
                    && offsets[index + 1] != offsets[to]
                {
                    return None;
                }
            }
        }
        (found && low >= -4095 && high <= 4095 && high - low <= 4096)
            .then_some((low, (high - low) as u32))
    }

    pub(super) fn body_stack_guard(&mut self) {
        if let Some((low, len)) = self.body_stack_span() {
            let slow = self.asm.label();
            if low < 0 {
                self.emit(sub_imm(W, X16, host(ESP), low.unsigned_abs(), false));
            } else {
                self.emit(add_imm(W, X16, host(ESP), low as u32, false));
            }
            self.body_page(X16, len, true, slow);
            self.emit(lsr_imm(W, X16, X16, 12));
            self.emit(str(W, X16, RUNTIME, runtime::BODY_STACK));
            let mut pages = Vec::new();
            let mut registers: Vec<(u8, u32, bool)> = Vec::new();
            for index in 0..self.trace.len() {
                let Op::Body(n) = self.trace.insts[index].op else {
                    continue;
                };
                let Some(kind) = self.trace.bodies[usize::from(n)].body.inline else {
                    continue;
                };
                let pushes = self.body_pushes(index);
                let Some(len @ 1..=64) = self.body_length(index) else {
                    continue;
                };
                for arg in 0..if kind == adv_cpu_core::body::Inline::Set {
                    1
                } else {
                    2
                } {
                    let write = arg == 0 && kind != adv_cpu_core::body::Inline::Compare;
                    if let Some(Src::Reg(reg)) = pushes.get(arg)
                        && self.body_invariant(*reg)
                    {
                        if let Some((_, span, grant)) =
                            registers.iter_mut().find(|(r, _, _)| r == reg)
                        {
                            *span = (*span).max(len);
                            *grant |= write;
                        } else {
                            registers.push((*reg, len, write));
                        }
                    }
                    if let Some(Src::Imm(at)) = pushes.get(arg) {
                        if let Some((_, grant)) =
                            pages.iter_mut().find(|(page, _)| *page == at >> 12)
                        {
                            *grant |= write;
                        } else {
                            pages.push((at >> 12, write));
                        }
                    }
                }
            }
            for (page, write) in pages {
                self.mov_imm(X4, page);
                self.emit(sub(W, X4, X16, X4, Lsl, 0));
                self.asm.cbz(W, false, X4, slow);
                self.body_page_known(X1, 1, write, slow, Some(page << 12));
            }

            for (reg, len, write) in registers {
                self.emit(lsr_imm(W, X4, host(reg), 12));
                self.emit(sub(W, X4, X16, X4, Lsl, 0));
                self.asm.cbz(W, false, X4, slow);
                self.body_page(host(reg), len, write, slow);
            }
            self.push_cold(Cold::Leave {
                label: slow,
                state: self.state,
                simd_dirty: self.vector.dirty,
                refund: self.trace.prepaid(0),
                exit: Exit::Interpret,
                eip: self.trace.insts[0].pc,
                link: false,
                x87: None,
            });
        }
    }

    fn pushed_value(&self, index: usize, source: Src) -> Src {
        let Src::Reg(reg) = source else {
            return source;
        };
        if reg == ESP {
            return source;
        }
        for before in (0..index).rev() {
            if self.trace.head(before + 1) || self.trace.merge[before + 1] {
                break;
            }
            match self.trace.insts[before].op {
                Op::Mov(Src::Reg(dst), Src::Imm(value)) if dst == reg => return Src::Imm(value),
                Op::Mov(Src::Reg(dst), _) if dst != reg => {}
                Op::Push(_) | Op::Nop => {}
                _ => break,
            }
        }
        source
    }

    /// Only expansions omit stores: ordinary body calls keep their original
    /// stack accesses, with no per-page stack-alias exclusion in the body.
    pub(super) fn body_length(&self, index: usize) -> Option<u32> {
        let Op::Body(n) = self.trace.insts[index].op else {
            return None;
        };
        let bound = self.trace.bodies[usize::from(n)];
        if bound.entry || bound.body.inline.is_none() {
            return None;
        }
        for (argument, before) in (index.saturating_sub(3)..index).rev().enumerate() {
            if self.trace.head(before + 1) || self.trace.merge[before + 1] {
                return None;
            }
            let Op::Push(source) = self.trace.insts[before].op else {
                return None;
            };
            if argument == 2 {
                return match self.pushed_value(before, source) {
                    Src::Imm(len) if len <= 64 => Some(len),
                    _ => None,
                };
            }
        }
        None
    }

    /// A suffix of adjacent argument pushes has no observer or budget edge.
    /// Its registers cannot change before the call, so cold exits can recreate it.
    pub(super) fn body_pushes(&self, index: usize) -> Vec<Src> {
        let Op::Body(n) = self.trace.insts[index].op else {
            return Vec::new();
        };
        let bound = self.trace.bodies[usize::from(n)];
        if self.body_length(index).is_none() {
            return Vec::new();
        }
        let count = bound
            .body
            .inputs
            .iter()
            .flatten()
            .filter_map(|input| {
                if let adv_cpu_core::body::Input::Stack(k) = input {
                    Some(usize::from(*k) + 1)
                } else {
                    None
                }
            })
            .max()
            .unwrap_or(0);
        let mut pushes = Vec::new();
        for n in (index.saturating_sub(count)..index).rev() {
            if self.trace.head(n + 1) || self.trace.merge[n + 1] {
                break;
            }
            match self.trace.insts[n].op {
                Op::Push(source @ (Src::Imm(_) | Src::Reg(0..=3) | Src::Reg(5..=7))) => {
                    pushes.push(self.pushed_value(n, source))
                }
                Op::Push(source @ (Src::Mem(_) | Src::Reg(ESP))) => {
                    pushes.push(source);
                    break;
                }
                _ => break,
            }
        }
        pushes
    }

    pub(super) fn forwarded_push(&self, index: usize) -> bool {
        let mut next = index + 1;
        while next < self.trace.len() && matches!(self.trace.insts[next].op, Op::Push(_)) {
            next += 1;
        }
        next < self.trace.len() && next - index <= self.body_pushes(next).len()
    }

    /// A single ordinary page; other spans take the body's checked path.
    pub(super) fn body_page(
        &mut self,
        address: Reg,
        len: u32,
        write: bool,
        slow: crate::asm::Label,
    ) {
        self.body_page_known(address, len, write, slow, None);
    }

    fn body_page_known(
        &mut self,
        address: Reg,
        len: u32,
        write: bool,
        slow: crate::asm::Label,
        known: Option<u32>,
    ) {
        use adv_cpu_core::memory::{READ, WRITE};
        if let Some(at) = known {
            if at % 4096 + len > 4096 {
                self.asm.b(slow);
                return;
            }
        } else {
            self.page_crossing(address, len, slow);
        }
        self.emit(ldr(X, X3, RUNTIME, runtime::BODY_CLASSES));
        if let Some(at) = known {
            self.mov_imm(X4, at >> 12);
        } else {
            self.emit(lsr_imm(W, X4, address, 12));
        }
        self.emit(ldr_narrow(Narrow::Byte, X4, X3, X4));
        let need = if write { READ | WRITE } else { READ };
        self.emit(and_imm(W, X4, X4, 0x3f));
        if !write {
            self.emit(and_imm(W, X4, X4, !6u32 as u64));
        }
        self.emit(sub_imm(W, X4, X4, u32::from(need), false));
        self.asm.cbz(W, true, X4, slow);
    }

    /// A stopped body has not committed its return. Restore the omitted
    /// arguments before the interpreter, full handler or exception observes them.
    pub(super) fn body_stores(&mut self, pushes: &[Src]) {
        for (n, source) in pushes.iter().enumerate().rev() {
            let value = match *source {
                Src::Imm(value) => self.imm(value, X16),
                Src::Reg(ESP) | Src::Mem(_) => {
                    self.emit(ldr(W, X16, RUNTIME, runtime::BODY_VALUE));
                    X16
                }
                Src::Reg(reg) => host(reg),
            };
            self.emit(str(W, value, host(ESP), n as u32 * 4));
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::translate::{Features, Redirect, Stubs, Workspace, translate};
    use adv_cpu_core::Memory;
    use adv_cpu_core::body::{BodyContext, BodyResult, DirectBody, EAX, EDX, Inline, Input};
    use iced_x86::code_asm::*;

    #[allow(clippy::too_many_arguments)]
    unsafe extern "C" fn unused_body(
        _: *const BodyContext,
        _: u32,
        _: u32,
        _: u32,
        _: u32,
        _: u32,
        _: u32,
        _: u32,
    ) -> BodyResult {
        unreachable!()
    }

    // spec: A64-CALL-2, A64-CALL-3, A64-CALL-8
    #[test]
    fn body_fast_paths_fit_the_original_call_sites() {
        for (kind, len, original, dynamic, arity) in [
            (Some(Inline::Move), 4, 23, false, 3),
            (Some(Inline::Move), 16, 23, false, 3),
            (Some(Inline::Move), 64, 23, false, 3),
            (Some(Inline::Compare), 8, 22, false, 3),
            (Some(Inline::Set), 64, 21, false, 3),
            (None, 16, 15, false, 1),
            (None, 32, 19, false, 2),
            (None, 64, 21, false, 3),
            (None, 64, 15, false, 1),
            (None, 256, 15, false, 1),
            (None, 16, 25, true, 3),
        ]
        .into_iter()
        .chain((1..=64).flat_map(|len| {
            [
                (Some(Inline::Move), len, 23, false, 3),
                (Some(Inline::Set), len, 21, false, 3),
                (Some(Inline::Compare), len, 22, false, 3),
            ]
        })) {
            let mut a = CodeAssembler::new(32).unwrap();
            let mut top = a.create_label();
            a.set_label(&mut top).unwrap();
            let args = if kind.is_some() || dynamic {
                vec![
                    0x20100u32,
                    if kind == Some(Inline::Set) {
                        0xa5
                    } else {
                        0x21200
                    },
                    len,
                ]
            } else {
                match arity {
                    2 => vec![0x20100, 0x21200],
                    3 => vec![1, 0x20100, len],
                    _ => vec![0x20100],
                }
            };
            for (index, &arg) in args.iter().enumerate().rev() {
                if dynamic && index == 2 {
                    a.push(dword_ptr(0x30010)).unwrap();
                } else {
                    a.push(arg as i32).unwrap();
                }
            }
            a.call(dword_ptr(0x30000)).unwrap();
            a.add(esp, args.len() as i32 * 4).unwrap();
            a.jmp(top).unwrap();
            let code = a.assemble(0x10000).unwrap();
            let mut memory = Memory::new(0x10000, 0x40000).unwrap();
            memory.protect(0x10000, 0x40000, 3).unwrap();
            memory.write(0x10000, &code).unwrap();
            memory.write(0x30000, &0x40000u32.to_le_bytes()).unwrap();
            memory.write(0x40000, &[0xc3]).unwrap();
            memory.protect(0x10000, 4096, 5).unwrap();
            memory.protect(0x40000, 4096, 5).unwrap();
            let body = DirectBody {
                call: unused_body,
                inline: kind,
                inputs: std::array::from_fn(|n| (n < args.len()).then_some(Input::Stack(n as u8))),
                outputs: EAX
                    | if kind == Some(Inline::Move) || dynamic {
                        EDX
                    } else {
                        0
                    },
                pop: 0,
            };
            let mut work = Workspace::default();
            translate(
                &mut work,
                &mut memory,
                0x10000,
                &[],
                &|_| false,
                0,
                Features::default(),
                0,
                Stubs {
                    exit: 0,
                    dispatch: 0,
                    lookup: 0,
                    lookup_slow: 0,
                    ret_miss: 0,
                    access: 0,
                    call: 0,
                    interpret: 0,
                    fault: 0,
                },
                &|_, _| None,
                &|pc| {
                    (pc == 0x40000).then_some(Redirect {
                        target: 0x40100,
                        original: vec![0xc3],
                        checks: Vec::new(),
                        body: Some(body),
                    })
                },
                &|_, _| Vec::new(),
                0x037f,
            );
            let mut pc = work.code.asm.position(work.code.labels[0]) / 4;
            let end = work.code.asm.position(work.code.labels[args.len() + 1]) / 4;
            let mut count = 0;
            while pc != end {
                count += 1;
                assert!(count < 100, "fast path did not reach its continuation");
                let word = work.words()[pc];
                if kind.is_some() {
                    assert_ne!(word & 0xfffffc1f, 0xd63f0000, "inline path calls out");
                }
                if word & 0xfc000000 == 0x14000000 {
                    pc = (pc as isize + ((word << 6) as i32 >> 6) as isize) as usize;
                } else {
                    pc += 1;
                }
            }
            println!(
                "{kind:?} {len} dynamic={dynamic}: {count} A64 instructions (original {original})"
            );
            if kind.is_none() {
                assert_eq!(
                    count, original,
                    "ordinary body calls keep their original emission"
                );
            } else {
                assert!(count <= original, "{kind:?} {len}: {count} > {original}");
            }
        }
    }
}
