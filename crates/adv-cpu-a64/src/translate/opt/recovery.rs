//! Recovery records (A64-OPT-6). A record describes the guest state at a
//! region start: where each component lives, or `None` when no execution
//! from there reads it. The IR names values; emission replaces them by host
//! locations, and the owner's materializer builds `CpuState` and frame bytes
//! from a record and the captured host registers.

use super::ir::{FpOp, Value};

/// Where a component's value lives at the region start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Location<L> {
    Constant(u64),
    At(L),
    Recipe(Recipe<L>),
}

/// A component recomputed from live values instead of held.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Recipe<L> {
    /// C1 after `a op b` rounded under the form's control word.
    C1 { op: FpOp, a: L, b: L },
    /// EFLAGS of a `Flags` value: its kind and operands, as the interpreter
    /// computes them.
    Eflags(L),
}

/// The x87 state at a region start. TOP, tags, IP and opcode are proven
/// constants; the control word is the form's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct X87<L> {
    pub top: u8,
    pub tags: u16,
    /// Physical registers 0–7: valid slots hold binary64 values.
    pub stack: [Option<Location<L>>; 8],
    /// Condition and exception bits other than C1 and TOP.
    pub status: u16,
    pub c1: Option<Location<L>>,
    pub ip: u32,
    pub opcode: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Record<L> {
    pub eip: u32,
    /// Instructions prepaid at the last loop head and not retired at `eip`.
    pub refund: u32,
    pub regs: [Option<Location<L>>; 8],
    pub eflags: Option<Location<L>>,
    pub x87: X87<L>,
    pub xmm: [Option<Location<L>>; 8],
    /// Frame bytes whose values differ from memory: offset from entry ESP,
    /// width in bytes and value.
    pub frame: Vec<(i32, u8, Location<L>)>,
}

impl<L: Copy> Record<L> {
    /// Every location the record reads.
    pub(crate) fn uses(&self) -> impl Iterator<Item = L> + '_ {
        let x87 = self.x87.stack.iter().chain([&self.x87.c1]);
        let held = self.regs.iter().chain([&self.eflags]).chain(x87);
        held.chain(&self.xmm)
            .flatten()
            .chain(self.frame.iter().map(|(_, _, location)| location))
            .flat_map(|location| match *location {
                Location::Constant(_) => [None, None],
                Location::At(at) | Location::Recipe(Recipe::Eflags(at)) => [Some(at), None],
                Location::Recipe(Recipe::C1 { a, b, .. }) => [Some(a), Some(b)],
            })
            .flatten()
    }

    /// Whether every component is present, as exits and recall points
    /// require (A64-OPT-6).
    pub(crate) fn complete(&self) -> bool {
        self.regs.iter().all(Option::is_some)
            && self.eflags.is_some()
            && self.x87.c1.is_some()
            && self.xmm.iter().all(Option::is_some)
            && (0..8).all(|i| self.x87.tags >> (2 * i) & 3 == 3 || self.x87.stack[i].is_some())
    }
}

/// A record over IR values, as the front end builds it.
pub(crate) type IrRecord = Record<Value>;
