//! Integer SIMD loops with a register-resident NEON steady body (A64-TR-5).
//!
//! Recognition uses decoded operations and resolved edges, never addresses,
//! names, constants or library fingerprints. A single-entry loop may contain
//! integer address arithmetic, memory operations and conditional side exits.
//! Calls, helpers, checked accesses, floating arithmetic and internal merges
//! prevent recognition. Loads and stores retain their original order and width,
//! so overlap, fixed-point gathers and page faults need no special cases.
//!
//! The first iteration establishes the SIMD cache using ordinary instructions.
//! Only its backedge reaches the steady copy. That copy starts with the cache
//! left by the previous iteration, including its dirty mask at every fault site.
//! Both copies charge the original instruction budget and share the final flush.
//! There is no additional tier, register allocation or guest code patching.

use super::decode::{Op, Src};
use super::simd::{Kind, Loc, Vector};
use super::trace::{Edge, Mode, Next, Trace};
use super::{Label, State, Translator};

#[derive(Clone, Copy, Debug)]
pub(super) struct Plan {
    pub start: usize,
    pub end: usize,
}

impl Plan {
    pub fn at(trace: &Trace, start: usize, checked: &dyn Fn(u32) -> bool) -> Option<Self> {
        if !trace.merge[start] {
            return None;
        }
        let mut vectors = 0;
        let mut reads = false;
        let mut writes = false;
        for end in start..trace.len() {
            let inst = trace.insts[end];
            if (end != start && (trace.head(end) || trace.merge[end]))
                || inst.mode != Mode::Plain
                || inst.call.is_some()
                || checked(inst.pc)
            {
                return None;
            }
            if inst.taken == Some(Edge::Internal(start)) {
                return (end > start && vectors >= 2 && reads && writes)
                    .then_some(Self { start, end });
            }
            if inst.next != Next::Fall
                || matches!(inst.taken, Some(Edge::Internal(_) | Edge::Interpret(_)))
            {
                return None;
            }
            match inst.op {
                Op::Simd(s)
                    if !s.floating() && !matches!(s.kind, Kind::Emms | Kind::StoreControl) =>
                {
                    vectors += 1;
                    reads |= s.src == Loc::Mem;
                    writes |= s.dst == Loc::Mem;
                }
                Op::Mov(d, s) | Op::Alu(_, d, s) => {
                    reads |= matches!(s, Src::Mem(_));
                    writes |= matches!(d, Src::Mem(_));
                }
                Op::Nop
                | Op::Lea(..)
                | Op::Step(..)
                | Op::Neg(_)
                | Op::Not(_)
                | Op::Extend(..)
                | Op::Cdq
                | Op::Jmp(_)
                | Op::Jcc(..)
                | Op::Setcc(..)
                | Op::Cmov(..)
                | Op::Shift(_, Src::Reg(_), Some(_))
                | Op::Imul(..)
                | Op::Carry(..)
                | Op::Narrow(..)
                | Op::Direction(_) => {}
                _ => return None,
            }
        }
        None
    }
}

#[derive(Clone, Copy)]
pub(super) struct Active {
    pub plan: Plan,
    pub entry: Label,
    pub resume: Label,
    pub steady: bool,
}

pub(super) struct Copy {
    active: Active,
    vector: Vector,
}

impl Translator<'_> {
    pub(super) fn pixel_start(&mut self, index: usize) {
        if self.pixel.is_none()
            && let Some(plan) = Plan::at(self.trace, index, self.checked)
        {
            self.pixel = Some(Active {
                plan,
                entry: self.asm.label(),
                resume: self.asm.label(),
                steady: false,
            });
        }
    }

    /// Returns true when the steady copy has transferred to the shared tail.
    pub(super) fn pixel_end(&mut self) -> bool {
        let Some(active) = self.pixel.filter(|p| p.plan.end == self.index) else {
            return false;
        };
        // The first and steady copies need the same state at their shared tail.
        self.vector.cpu = false;
        if active.steady {
            self.asm.b(active.resume);
            return true;
        }
        self.pixel_copies.push(Copy {
            active,
            vector: self.vector,
        });
        self.asm.bind(active.resume);
        self.flush_simd();
        self.pixel = None;
        false
    }

    pub(super) fn pixel_copies(&mut self) {
        while let Some(copy) = self.pixel_copies.pop() {
            let active = Active {
                steady: true,
                ..copy.active
            };
            self.pixel = Some(active);
            self.vector = copy.vector;
            self.state = State::CANONICAL;
            self.asm.bind(active.entry);
            for index in active.plan.start..=active.plan.end {
                self.instruction(index);
            }
            self.pixel = None;
        }
        self.vector = Vector::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adv_cpu_core::Memory;
    use iced_x86::code_asm::*;

    // spec: A64-TR-5, A64-CTL-9
    #[test]
    fn semantic_recognition_respects_observers_merges_checked_accesses_and_stops() {
        for variant in 0..7 {
            let mut a = CodeAssembler::new(32).unwrap();
            let mut top = a.create_label();
            let mut merge = a.create_label();
            a.set_label(&mut top).unwrap();
            a.movdqu(xmm4, xmmword_ptr(ebp)).unwrap();
            let stop = a.instructions().len();
            match variant {
                1 => a.addps(xmm4, xmm2).unwrap(),
                2 => a.pushad().unwrap(),
                3 => {
                    a.cmp(eax, 0).unwrap();
                    a.je(merge).unwrap();
                }
                4 => a.emms().unwrap(),
                _ => {}
            }
            a.paddusb(xmm4, xmm2).unwrap();
            a.set_label(&mut merge).unwrap();
            a.movdqu(xmmword_ptr(edx), xmm4).unwrap();
            a.add(ebp, 16).unwrap();
            a.add(edx, 16).unwrap();
            a.dec(ebx).unwrap();
            a.jnz(top).unwrap();
            let bytes = a.assemble(0x10000).unwrap();
            let mut memory = Memory::new(0x10000, 0x1000).unwrap();
            memory.protect(0x10000, 0x1000, 7).unwrap();
            memory.write(0x10000, &bytes).unwrap();
            let mut trace = Trace::default();
            let mut stops = vec![0x10000 + bytes.len() as u32];
            if variant == 6 {
                let inst = iced_x86::Decoder::with_ip(32, &bytes, 0x10000, 0)
                    .iter()
                    .nth(stop)
                    .unwrap();
                stops.push(inst.ip32());
            }
            trace.build(&memory, 0x10000, &stops, &|_, _| None, &|_| None);
            assert_eq!(
                Plan::at(&trace, 0, &|_| variant == 5).is_some(),
                variant == 0
            );
        }
    }
}
