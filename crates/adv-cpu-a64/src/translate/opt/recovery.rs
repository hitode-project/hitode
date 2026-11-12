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

/// The x87 state at a region start. TOP, occupancy, the control word and
/// the status bits are proven constants.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct X87<L> {
    pub top: u8,
    /// Bit `i` set when physical register `i` is valid. The materializer
    /// derives each valid register's tag from its value; empty registers
    /// keep their contents and tags from the job's entry state.
    pub valid: u8,
    pub control: u16,
    /// Physical registers 0–7: valid slots hold binary64 values.
    pub stack: [Option<Location<L>>; 8],
    /// Condition and exception bits other than C1 and TOP.
    pub status: u16,
    pub c1: Option<Location<L>>,
    /// Last instruction pointer and opcode, as I32 values when arrivals
    /// at a head disagree.
    pub ip: Option<Location<L>>,
    pub opcode: Option<Location<L>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Record<L> {
    pub eip: u32,
    /// Instructions charged to the countdown and not retired at `eip`: the
    /// trace's `owed` at a head, its `unused` after an instruction.
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
        let x87 = self
            .x87
            .stack
            .iter()
            .chain([&self.x87.c1, &self.x87.ip, &self.x87.opcode]);
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
            && self.x87.ip.is_some()
            && self.x87.opcode.is_some()
            && self.xmm.iter().all(Option::is_some)
            && (0..8).all(|i| self.x87.valid >> i & 1 == 0 || self.x87.stack[i].is_some())
    }
}

/// A record over IR values, as the front end builds it.
pub(crate) type IrRecord = Record<Value>;
