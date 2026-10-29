//! SIMD residency across merges (A64-REG-5).
//!
//! A merge used to drop the SIMD cache, so a loop reloaded every MMX and
//! XMM register it read on every iteration. Instead each merge has an entry
//! set: registers the unit holds clean in d8-d15 and v16-v23 on arrival.
//! Arrivals publish their dirty registers, as every edge did, and load any
//! of the set they lack; registers outside it are forgotten.
//!
//! The set never adds work to a path. A register is in it only if every
//! path from the merge reads it before writing it, dropping the cache or
//! leaving, so a load an arrival makes replaces the one the merge's code
//! would make. A jcc's taken edge cannot load without a branch of its own,
//! so the set also holds only registers that every jcc arrival holds, as a
//! forward pass over the trace finds. Paths that hold less than that pass
//! assumed load through cold code.
//!
//! An entry also holds the guards (`Guards`) every arrival has passed, so
//! loops stop rechecking pending x87 exceptions and MXCSR each iteration.
//! Guards cost nothing to keep and cannot be re-established on an edge, so
//! every arrival counts, fall-through and jumps included.

use super::decode::Op;
use super::simd::{Guards, Kind};
use super::trace::{Edge, Mode, Next, Trace};

/// What a merge holds on arrival.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Entry {
    /// Clean SIMD registers, as `Vector::dirty` numbers them.
    pub regs: u16,
    pub guards: Guards,
}

/// The SIMD registers an instruction reads and writes, as `Vector::dirty`
/// numbers them, or `None` when it may drop the cache.
fn footprint(trace: &Trace, index: usize, checked: &dyn Fn(u32) -> bool) -> Option<(u16, u16)> {
    let inst = trace.insts[index];
    if inst.call.is_some() || inst.mode == Mode::Real {
        return None;
    }
    match inst.op {
        Op::Simd(simd)
            if !(checked(inst.pc)
                && simd.memory()
                && matches!(simd.kind, Kind::InsertWord | Kind::StoreControl)) =>
        {
            Some(simd.footprint())
        }
        // Helpers, slow paths that call them and owner calls drop it.
        Op::Simd(_)
        | Op::Helper(_)
        | Op::Strings(_)
        | Op::Integer(_)
        | Op::X87(_)
        | Op::Fild64(..)
        | Op::Call(_)
        | Op::CallIndirect(_)
        | Op::JmpIndirect(_)
        | Op::Ret(_) => None,
        _ => Some((0, 0)),
    }
}

/// The entry of each instruction; only merges' are used.
pub(super) fn entries(trace: &Trace, checked: &dyn Fn(u32) -> bool) -> Vec<Entry> {
    let len = trace.len();
    let footprints: Vec<_> = (0..len).map(|n| footprint(trace, n, checked)).collect();
    // Backward: registers every path reads first. Exits and backward
    // edges need nothing, so loops need no fixed point here.
    let mut need = vec![0u16; len];
    for index in (0..len).rev() {
        let inst = trace.insts[index];
        let after = |edge: Option<Edge>| match edge {
            Some(Edge::Internal(to)) if to > index => need[to],
            _ => 0,
        };
        let mut out = match inst.next {
            Next::Fall => need.get(index + 1).copied().unwrap_or(0),
            Next::Edge(edge) => after(Some(edge)),
            Next::Dynamic => 0,
        };
        if inst.taken.is_some() {
            out &= after(inst.taken);
        }
        need[index] = match footprints[index] {
            Some((reads, writes)) => reads | (out & !writes),
            None => 0,
        };
    }
    // Forward: registers jcc arrivals hold and guards every arrival
    // holds, narrowing loop heads until the backedges agree.
    let mut entry: Vec<_> = need
        .iter()
        .map(|&regs| Entry {
            regs,
            guards: Guards::ALL,
        })
        .collect();
    loop {
        let mut held = vec![(u16::MAX, Guards::ALL); len];
        let mut state = Entry::default();
        for index in 0..len {
            if trace.head(index) {
                entry[index] = Entry::default();
            } else if trace.merge[index] && trace.insts[index - 1].next == Next::Fall {
                held[index].1 = held[index].1.meet(state.guards);
            }
            if trace.head(index) || trace.merge[index] {
                state = entry[index];
            }
            let inst = trace.insts[index];
            state = match (footprints[index], inst.op) {
                (None, _) => Entry::default(),
                (Some((reads, writes)), op) => Entry {
                    regs: state.regs | reads | writes,
                    guards: match op {
                        Op::Simd(simd) => simd.guarding(state.guards),
                        _ => state.guards,
                    },
                },
            };
            if let Some(Edge::Internal(to)) = inst.taken {
                held[to].0 &= state.regs;
                held[to].1 = held[to].1.meet(state.guards);
            }
            if let Next::Edge(Edge::Internal(to)) = inst.next {
                held[to].1 = held[to].1.meet(state.guards);
            }
        }
        let mut changed = false;
        for (entry, &(regs, guards)) in entry.iter_mut().zip(&held) {
            let narrowed = Entry {
                regs: entry.regs & regs,
                guards: entry.guards.meet(guards),
            };
            changed |= narrowed != *entry;
            *entry = narrowed;
        }
        if !changed {
            return entry;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adv_cpu_core::Memory;
    use iced_x86::code_asm::*;

    /// The entry of the loop head after a short prefix.
    fn head_entry(variant: u8) -> Entry {
        let mut a = CodeAssembler::new(32).unwrap();
        let (mut top, mut done) = (a.create_label(), a.create_label());
        a.movq(mm1, qword_ptr(esi)).unwrap();
        a.pxor(xmm2, xmm2).unwrap();
        if variant == 3 {
            a.mulps(xmm4, xmm5).unwrap();
        }
        a.set_label(&mut top).unwrap();
        match variant {
            1 => {
                a.dec(ebx).unwrap();
                a.jz(done).unwrap();
            }
            2 => a.cpuid().unwrap(),
            _ => {}
        }
        a.paddw(mm0, mm1).unwrap();
        a.addps(xmm3, xmm2).unwrap();
        if variant == 4 {
            a.emms().unwrap();
        }
        a.dec(ecx).unwrap();
        a.jnz(top).unwrap();
        a.ret().unwrap();
        a.set_label(&mut done).unwrap();
        a.nop().unwrap();
        let bytes = a.assemble(0x10000).unwrap();
        let mut memory = Memory::new(0x10000, 0x1000).unwrap();
        memory.protect(0x10000, 0x1000, 7).unwrap();
        memory.write(0x10000, &bytes).unwrap();
        let mut trace = Trace::default();
        trace.build(
            &memory,
            0x10000,
            &[0x10000 + bytes.len() as u32],
            &|_, _| None,
            &|_| None,
        );
        let head = 2 + usize::from(variant == 3);
        assert!(trace.merge[head]);
        entries(&trace, &|_| false)[head]
    }

    // spec: A64-REG-5
    #[test]
    fn merges_hold_what_every_path_reads_and_jcc_arrivals_hold() {
        // mm0, mm1 and xmm2 are read every iteration; xmm3 is only written.
        assert_eq!(head_entry(0).regs, 0b11 | 1 << 10);
        // A side exit before the reads, or a helper, makes loads speculative.
        assert_eq!(head_entry(1).regs, 0);
        assert_eq!(head_entry(2).regs, 0);
    }

    // spec: A64-REG-5
    #[test]
    fn merges_keep_guards_every_arrival_passed() {
        // The prefix's MMX load passed the x87 guard; only the backedge
        // passed the MXCSR guards.
        let ready = Guards {
            ready: true,
            ..Guards::default()
        };
        assert_eq!(head_entry(0).guards, ready);
        // A helper inside the loop is followed by guards again.
        assert_eq!(head_entry(2).guards, ready);
        assert_eq!(head_entry(3).guards, Guards::ALL);
        // EMMS on the backedge path loses the x87 guard.
        assert!(!head_entry(4).guards.ready);
    }
}
