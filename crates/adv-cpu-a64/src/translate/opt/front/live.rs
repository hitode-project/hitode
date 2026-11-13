//! The guest registers, XMM registers and frame bytes that execution from
//! each trace position may read (A64-OPT-6). Exits read everything, and so
//! does an instruction that may raise an exception. A block takes a
//! parameter, and a record a location, only for a live component.

use iced_x86::{InstructionInfoFactory, OpAccess};

use super::{Offsets, frame_offset};
use crate::translate::Unit;
use crate::translate::computation::Proof;
use crate::translate::decode::{self, ESP, Mem};
use crate::translate::integer::Integer;
use crate::translate::trace::{Edge, Next};

/// Registers 0–7 as bits 0–7 and XMM registers as bits 8–15, and frame
/// bytes by their index in the layout's frame.
#[derive(Clone, PartialEq, Eq)]
pub(super) struct Set {
    pub regs: u16,
    frame: Vec<u64>,
}

impl Set {
    fn empty(bytes: usize) -> Self {
        Set {
            regs: 0,
            frame: vec![0; bytes.div_ceil(64)],
        }
    }
    fn all(bytes: usize) -> Self {
        Set {
            regs: u16::MAX,
            frame: vec![u64::MAX; bytes.div_ceil(64)],
        }
    }
    pub fn reg(&self, n: usize) -> bool {
        self.regs >> n & 1 != 0
    }
    /// Whether the frame byte at index `i` of the layout's frame is live.
    pub fn byte(&self, i: usize) -> bool {
        self.frame[i / 64] >> (i % 64) & 1 != 0
    }
    fn mark(&mut self, i: usize) {
        self.frame[i / 64] |= 1 << (i % 64);
    }
    fn union(&mut self, other: &Set) {
        self.regs |= other.regs;
        for (a, b) in self.frame.iter_mut().zip(&other.frame) {
            *a |= b;
        }
    }
    fn without(&mut self, other: &Set) {
        self.regs &= !other.regs;
        for (a, b) in self.frame.iter_mut().zip(&other.frame) {
            *a &= !b;
        }
    }
}

pub(super) struct Live {
    /// Components live on entry to each instruction.
    pub entry: Vec<Set>,
    /// Frame bytes guest instructions from each position may read: what
    /// exits and exceptions read is left out. A byte live only for them can
    /// wait in memory once committed.
    pub guest: Vec<Set>,
    /// Frame bytes each instruction reads or writes.
    pub touched: Vec<Set>,
    /// Components live after each instruction, where a store's region starts.
    pub after: Vec<Set>,
    /// Everything, as exits need.
    pub all: Set,
    /// Registers no instruction writes: records leave them at their entry
    /// values.
    pub kept: u16,
}

pub(super) fn analyze(
    unit: &Unit,
    proof: &Proof,
    offsets: &[Offsets],
    frame: &[i32],
    models: &[super::model::Model],
) -> Live {
    let n = unit.trace.len();
    let all = Set::all(frame.len());
    let mut info = InstructionInfoFactory::new();
    let mut reads = Vec::with_capacity(n);
    let mut defines = Vec::with_capacity(n);
    let mut raising = Vec::with_capacity(n);
    let mut touched = Vec::with_capacity(n);
    let mut written = 0u16;
    for (i, inst) in unit.trace.insts.iter().enumerate() {
        let mut read = Set::empty(frame.len());
        let mut define = Set::empty(frame.len());
        let raises = match inst.op {
            decode::Op::Integer(Integer::Divide(..)) => true,
            decode::Op::X87(_) | decode::Op::Fild64(..) => {
                models[i].control.is_none_or(|c| c & 0x3f != 0x3f)
            }
            _ => false,
        };
        // A nop accesses nothing, also where it stands for a cut instruction
        // that leaves (`Trace::cut`).
        if inst.op == decode::Op::Nop {
            reads.push(read);
            defines.push(define);
            raising.push(false);
            touched.push(Set::empty(frame.len()));
            continue;
        }
        let Some(ins) = proof.instruction(i) else {
            reads.push(all.clone());
            defines.push(define);
            raising.push(false);
            touched.push(all.clone());
            written = u16::MAX;
            continue;
        };
        let mut touch = Set::empty(frame.len());
        let info = info.info(ins);
        for used in info.used_registers() {
            let reg = used.register();
            let (bit, full) = if reg.is_gpr() {
                (reg.full_register32().number(), reg.size() == 4)
            } else if reg.is_xmm() {
                (8 + reg.number(), true)
            } else {
                continue;
            };
            let bit = 1u16 << bit;
            match used.access() {
                OpAccess::Read | OpAccess::CondRead => read.regs |= bit,
                OpAccess::Write if full => {
                    define.regs |= bit;
                    written |= bit;
                }
                OpAccess::None | OpAccess::NoMemAccess => {}
                _ => {
                    read.regs |= bit;
                    written |= bit;
                }
            }
        }
        for used in info.used_memory() {
            let base = used.base();
            if !base.is_gpr() || used.index() != iced_x86::Register::None {
                continue;
            }
            let mem = Mem {
                base: Some(base.full_register32().number() as u8),
                index: None,
                scale: 1,
                address_bits: 32,
                disp: used.displacement() as u32,
                fs: false,
            };
            let Some(start) = frame_offset(mem, &offsets[i]) else {
                continue;
            };
            let size = used.memory_size().size() as i32;
            for pos in start..start + size {
                let Ok(byte) = frame.binary_search(&pos) else {
                    continue;
                };
                match used.access() {
                    OpAccess::Write => define.mark(byte),
                    OpAccess::None | OpAccess::NoMemAccess => continue,
                    _ => read.mark(byte),
                }
                touch.mark(byte);
            }
        }
        reads.push(read);
        defines.push(define);
        raising.push(raises);
        touched.push(touch);
    }
    let registers = Set {
        regs: u16::MAX,
        frame: vec![0; all.frame.len()],
    };
    let (guest, _) = solve(unit, &reads, &defines, &raising, &registers);
    let (entry, after) = solve(unit, &reads, &defines, &raising, &all);
    Live {
        entry,
        guest,
        touched,
        after,
        all,
        kept: !written,
    }
}

/// Live sets on entry to and after each instruction, where exits and
/// raising instructions read `all`.
fn solve(
    unit: &Unit,
    reads: &[Set],
    defines: &[Set],
    raising: &[bool],
    all: &Set,
) -> (Vec<Set>, Vec<Set>) {
    let n = unit.trace.len();
    let empty = Set {
        regs: 0,
        frame: vec![0; all.frame.len()],
    };
    let mut entry = vec![empty.clone(); n];
    let mut after = entry.clone();
    let mut changed = true;
    while changed {
        changed = false;
        for i in (0..n).rev() {
            let inst = &unit.trace.insts[i];
            let fall = match inst.next {
                Next::Fall => Some(Edge::Internal(i + 1)),
                Next::Edge(e) => Some(e),
                Next::Dynamic => None,
            };
            let mut out = if fall.is_none() {
                all.clone()
            } else {
                empty.clone()
            };
            for edge in [fall, inst.taken].into_iter().flatten() {
                match edge {
                    Edge::Internal(to) if to < n => out.union(&entry[to]),
                    _ => out.union(all),
                }
            }
            let mut live = out.clone();
            live.without(&defines[i]);
            live.union(&reads[i]);
            if raising[i] {
                live.union(all);
            }
            // ESP addresses the frame everywhere.
            live.regs |= 1 << ESP;
            after[i] = out;
            if live != entry[i] {
                entry[i] = live;
                changed = true;
            }
        }
    }
    (entry, after)
}
