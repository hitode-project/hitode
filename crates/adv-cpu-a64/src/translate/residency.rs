//! SIMD residency across merges (A64-REG-5, A64-TR-5).
//!
//! A merge used to drop the SIMD cache, so a loop reloaded every MMX and
//! XMM register it read on every iteration. Instead each merge has an
//! entry: registers the unit holds in d8-d15 and v16-v23 on arrival, those
//! of them it treats as dirty, and guards. Arrivals publish their other
//! dirty registers, load any of the entry they lack, and forget the rest.
//!
//! A register is in the entry if every path from the merge reads it before
//! writing it, dropping the cache or leaving, so a load an arrival makes
//! replaces the one the merge's code would make. A loop head also holds
//! the registers its body writes when no instruction there drops the cache:
//! the loop's entry loads them once, and its backedges then publish
//! nothing, as a peeled first iteration would achieve with twice the code.
//! A jcc's taken edge cannot load without a branch of its own, so the entry
//! also holds only registers that every jcc arrival holds, as a forward
//! pass over the trace finds. Paths that hold less than that pass assumed
//! load through cold code.
//!
//! Any dirty set is correct: an arrival publishes what it holds dirty
//! outside it, and a clean register counted dirty is only published again.
//! A merge counts dirty what some arrival brings dirty, so no edge
//! publishes a register the merge's code keeps writing.
//!
//! An entry also holds the guards (`Guards`) every arrival has passed, so
//! loops stop rechecking pending x87 exceptions and MXCSR each iteration.
//! Guards cost nothing to keep and cannot be re-established on an edge, so
//! every arrival counts, fall-through and jumps included.

use super::decode::Op;
use super::simd::Guards;
use super::trace::{Edge, Mode, Next, Trace};

/// What a merge holds on arrival.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Entry {
    /// Cached SIMD registers, as `Vector::dirty` numbers them.
    pub regs: u16,
    /// Those of `regs` counted dirty.
    pub dirty: u16,
    pub guards: Guards,
}

/// The SIMD registers an instruction reads and writes, as `Vector::dirty`
/// numbers them, or `None` when it may drop the cache.
fn footprint(trace: &Trace, index: usize, _checked: &dyn Fn(u32) -> bool) -> Option<(u16, u16)> {
    let inst = trace.insts[index];
    if inst.call.is_some() || inst.locked.is_some() || inst.mode == Mode::Real {
        return None;
    }
    match inst.op {
        Op::Simd(simd) => Some(simd.footprint()),
        // Helpers, slow paths that call them and owner calls drop it.
        Op::Syscall
        | Op::Timestamp
        | Op::PopFlags(..)
        | Op::Stack(..)
        | Op::Helper(_)
        | Op::Strings(_)
        | Op::Integer(_)
        | Op::X87(_)
        | Op::Fild64(..)
        | Op::Call(_)
        | Op::Body(_)
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
    // Loop heads also hold what their bodies write, unless the cache drops.
    for (index, inst) in trace.insts.iter().enumerate() {
        let jump = match inst.next {
            Next::Edge(edge) => Some(edge),
            _ => None,
        };
        for edge in [inst.taken, jump] {
            if let Some(Edge::Internal(head)) = edge
                && head <= index
                && let Some(writes) =
                    (head..=index).try_fold(0, |writes, n| footprints[n].map(|(_, w)| writes | w))
            {
                need[head] |= writes;
            }
        }
    }
    // Forward: registers jcc arrivals hold and guards every arrival
    // holds, narrowing loop heads until the backedges agree.
    let mut entry: Vec<_> = need
        .iter()
        .map(|&regs| Entry {
            regs,
            dirty: 0,
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
                    dirty: 0,
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
                dirty: 0,
                guards: entry.guards.meet(guards),
            };
            changed |= narrowed != *entry;
            *entry = narrowed;
        }
        if !changed {
            break;
        }
    }
    // Dirty: what some arrival brings dirty, widening until arrivals agree.
    // An internal jcc publishes, on both paths, what its target does not
    // count dirty.
    loop {
        let mut arrived = vec![0u16; len];
        let mut dirty = 0;
        for index in 0..len {
            if trace.head(index) {
                dirty = 0;
            } else if trace.merge[index] {
                if trace.insts[index - 1].next == Next::Fall {
                    arrived[index] |= dirty;
                }
                dirty = entry[index].dirty;
            }
            let inst = trace.insts[index];
            dirty = footprints[index].map_or(0, |(_, writes)| dirty | writes);
            if let Some(Edge::Internal(to)) = inst.taken {
                arrived[to] |= dirty;
                dirty &= entry[to].dirty;
            }
            if let Next::Edge(Edge::Internal(to)) = inst.next {
                arrived[to] |= dirty;
            }
        }
        let mut changed = false;
        for (entry, arrived) in entry.iter_mut().zip(arrived) {
            let widened = entry.dirty | (arrived & entry.regs);
            changed |= widened != entry.dirty;
            entry.dirty = widened;
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
            5 => a.lock().add(dword_ptr(esi), 1).unwrap(),
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

    // spec: A64-REG-5, A64-ISA-6
    #[test]
    fn merges_hold_what_every_path_reads_and_jcc_arrivals_hold() {
        // mm0, mm1 and xmm2 are read every iteration; the body writes mm0
        // and xmm3, which stay dirty across the backedge, and the prefix
        // arrives with mm1 and xmm2 dirty.
        let (written_mm0, written_xmm3) = (1u16, 1u16 << 11);
        let entry = head_entry(0);
        assert_eq!(entry.regs, 0b11 | 1 << 10 | written_xmm3);
        assert_eq!(entry.dirty, entry.regs);
        // A side exit before the reads makes their loads speculative; the
        // written registers still stay.
        assert_eq!(head_entry(1).regs, written_mm0 | written_xmm3);
        // A helper in the body drops the cache every iteration.
        assert_eq!(
            head_entry(2),
            Entry {
                guards: head_entry(2).guards,
                ..Entry::default()
            }
        );
        // A locked access may take its SHARED instruction helper.
        assert_eq!(
            head_entry(5),
            Entry {
                guards: head_entry(5).guards,
                ..Entry::default()
            }
        );
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
