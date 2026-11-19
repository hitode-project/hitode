//! Facts used by the front end and memory passes.

use std::sync::Arc;

use crate::translate::front::{
    summary::Summary,
    tags::{OpAccess, Register},
};

use super::{Mem, Offsets, Proof, Unit, decode, frame_offset, model::Model, part};
use crate::translate::{integer::Integer, trace::Inst};

pub(crate) trait Facts {
    fn invariant_read(&self, load: usize) -> bool;
    fn disjoint_effects(&self, store: usize, load: usize) -> bool;
    fn division_safe(&self, index: usize) -> bool;
    /// Non-frame accesses never reach the frame (A64-OPT-3 cells need no write-through).
    fn frame_private(&self) -> bool;
    /// Decoding retained by the legacy split proof.
    fn instruction(&self, _index: usize) -> Option<&Summary> {
        None
    }
}

impl Facts for Proof {
    fn invariant_read(&self, load: usize) -> bool {
        self.invariant_read(load)
    }
    fn disjoint_effects(&self, store: usize, load: usize) -> bool {
        self.disjoint_effects(store, load)
    }
    fn division_safe(&self, index: usize) -> bool {
        self.division(index).is_some()
    }
    fn frame_private(&self) -> bool {
        true
    }
    fn instruction(&self, index: usize) -> Option<&Summary> {
        self.instruction(index)
    }
}

#[derive(Clone, Copy)]
pub(super) struct Access {
    pub mem: Mem,
    pub width: u8,
    pub read: bool,
    pub write: bool,
}

/// Memory uses of each decoded instruction, or unknown when unavailable.
pub(super) fn accesses(unit: &Unit, facts: &dyn Facts) -> Vec<Option<Vec<Access>>> {
    unit.trace
        .insts
        .iter()
        .enumerate()
        .map(|(i, inst)| {
            if inst.op == decode::Op::Nop {
                return Some(Vec::new());
            }
            let ins = instruction(unit, facts, i)?;
            Some(
                ins.memory
                    .iter()
                    .filter_map(|used| {
                        use OpAccess::*;
                        let read =
                            matches!(used.access, Read | CondRead | ReadWrite | ReadCondWrite);
                        let write =
                            matches!(used.access, Write | CondWrite | ReadWrite | ReadCondWrite);
                        let reg =
                            |r: Register| r.is_gpr().then(|| r.full_register32().number() as u8);
                        (read || write).then(|| Access {
                            mem: Mem {
                                base: reg(used.base),
                                index: reg(used.index),
                                scale: used.scale,
                                disp: used.displacement,
                                address_bits: used.address_bits,
                                fs: used.segment == Register::FS,
                            },
                            width: used.width as u8,
                            read,
                            write,
                        })
                    })
                    .collect(),
            )
        })
        .collect()
}

pub(super) fn instruction<'a>(
    unit: &'a Unit,
    facts: &'a dyn Facts,
    i: usize,
) -> Option<&'a Summary> {
    if unit.trace.x86.is_empty() {
        facts.instruction(i)
    } else {
        unit.trace.x86.get(i)?.as_deref()
    }
}

pub(super) fn disjoint(a: u32, aw: u8, b: u32, bw: u8) -> bool {
    super::disjoint_ranges(a, aw.into(), b, bw.into())
}

#[derive(Default)]
pub(super) struct Local<'a> {
    pub insts: &'a [Inst],
    pub x86: &'a [Option<Arc<Summary>>],
    pub accesses: &'a [Option<Vec<Access>>],
    pub offsets: &'a [Offsets],
    pub models: &'a [Model],
    pub arrivals: &'a [Vec<usize>],
    pub unwritten: u32,
}

impl Facts for Local<'_> {
    fn invariant_read(&self, load: usize) -> bool {
        self.accesses.iter().enumerate().all(|(store, uses)| {
            uses.as_ref().is_some_and(|uses| {
                !uses.iter().any(|a| a.write) || self.disjoint_effects(store, load)
            })
        })
    }
    fn disjoint_effects(&self, store: usize, load: usize) -> bool {
        let Some(writes) = &self.accesses[store] else {
            return false;
        };
        let Some(reads) = &self.accesses[load] else {
            return false;
        };
        writes.iter().any(|a| a.write)
            && reads.iter().any(|a| a.read)
            && writes.iter().filter(|a| a.write).all(|a| {
                reads.iter().filter(|b| b.read).all(|b| {
                    let same_base = a.mem.base == b.mem.base
                        && a.mem.index == b.mem.index
                        && a.mem.scale == b.mem.scale
                        && !a.mem.fs
                        && !b.mem.fs
                        && a.mem.address_bits == 32
                        && b.mem.address_bits == 32
                        && [a.mem.base, a.mem.index]
                            .into_iter()
                            .flatten()
                            .all(|r| self.unwritten >> r & 1 != 0);
                    a.width != 0
                        && b.width != 0
                        && same_base
                        && disjoint(a.mem.disp, a.width, b.mem.disp, b.width)
                })
            })
    }
    fn division_safe(&self, i: usize) -> bool {
        let decode::Op::Integer(Integer::Divide(src, true, _)) = self.insts[i].op else {
            return false;
        };
        if !self.models[i]
            .read(part(src), 4, &self.offsets[i])
            .is_some_and(|d| d != 0 && d != u32::MAX)
        {
            return false;
        }
        let mut seen = vec![false; self.insts.len()];
        let mut work = vec![i];
        while let Some(next) = work.pop() {
            if next == 0 || self.arrivals[next].is_empty() {
                return false;
            }
            for &p in &self.arrivals[next] {
                if std::mem::replace(&mut seen[p], true) || self.insts[p].op == decode::Op::Cdq {
                    continue;
                }
                let Some(Some(ins)) = self.x86.get(p) else {
                    return false;
                };
                if ins.registers.iter().any(|reg| {
                    matches!(
                        reg.register.full_register32(),
                        Register::EAX | Register::EDX
                    ) && matches!(
                        reg.access,
                        OpAccess::Write
                            | OpAccess::CondWrite
                            | OpAccess::ReadWrite
                            | OpAccess::ReadCondWrite
                    )
                }) {
                    return false;
                }
                work.push(p);
            }
        }
        true
    }
    fn frame_private(&self) -> bool {
        self.accesses
            .iter()
            .zip(self.offsets)
            .all(|(uses, offsets)| {
                uses.as_ref()
                    .is_some_and(|uses| uses.iter().all(|a| frame_offset(a.mem, offsets).is_some()))
            })
    }
}

pub(super) struct ProofLocal<'a>(pub &'a Proof, pub &'a dyn Facts);

impl Facts for ProofLocal<'_> {
    fn invariant_read(&self, load: usize) -> bool {
        self.1.invariant_read(load)
    }
    fn disjoint_effects(&self, store: usize, load: usize) -> bool {
        self.1.disjoint_effects(store, load) || self.0.local_disjoint_effects(store, load)
    }
    fn division_safe(&self, index: usize) -> bool {
        self.1.division_safe(index)
    }
    fn frame_private(&self) -> bool {
        self.1.frame_private()
    }
}
