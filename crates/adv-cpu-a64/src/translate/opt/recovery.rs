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

/// A stable location in an optimized form. Spill cells are 16 bytes each.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Host {
    Register(u8),
    Vector(u8),
    Spill(u32),
    Constant(u128),
}

/// The captured registers and the same thread's spill cells. Neither the
/// materializer nor its records depend on the source of the capture.
pub(crate) struct Dump<'a> {
    pub registers: &'a crate::runtime::RegisterDump,
    pub spills: &'a [u128],
}

impl Dump<'_> {
    fn get(&self, host: Host) -> u128 {
        match host {
            Host::Register(n) => u128::from(self.registers.x[n as usize]),
            Host::Vector(n) => self.registers.v[n as usize],
            Host::Spill(n) => self.spills[n as usize],
            Host::Constant(bits) => bits,
        }
    }

    fn locate(&self, location: Location<Host>) -> u128 {
        match location {
            Location::Constant(bits) => u128::from(bits),
            Location::At(host) | Location::Recipe(Recipe::Eflags(host)) => self.get(host),
            Location::Recipe(Recipe::C1 { op, a, b }) => super::eval::locate(
                &Location::Recipe(Recipe::C1 {
                    op,
                    a: Value(0),
                    b: Value(1),
                }),
                &[self.get(a), self.get(b)],
            ),
        }
    }
}

/// Restores the record's observable state and frame bytes. The returned
/// refund is added to the captured countdown to obtain the retired count.
// spec: A64-OPT-6
pub(crate) fn materialize(
    record: &Record<Host>,
    dump: &Dump<'_>,
    cpu: &mut adv_cpu_core::CpuState,
    mut frame: impl FnMut(i32, &[u8]),
) -> u32 {
    for (reg, location) in cpu.regs.iter_mut().zip(record.regs) {
        if let Some(location) = location {
            *reg = dump.locate(location) as u32;
        }
    }
    if let Some(location) = record.eflags {
        cpu.eflags = dump.locate(location) as u32;
    }
    cpu.eip = record.eip;
    for (i, location) in record.x87.stack.iter().enumerate() {
        if record.x87.valid >> i & 1 != 0
            && let Some(location) = location
        {
            let value = binary80(dump.locate(*location) as u64);
            cpu.x87.regs[i] = value;
            cpu.x87.tag = cpu.x87.tag & !(3 << (2 * i)) | value.tag() << (2 * i);
        }
    }
    cpu.x87.control = record.x87.control;
    cpu.x87.status = record.x87.status & !((7 << 11) | (1 << 9)) | u16::from(record.x87.top) << 11;
    if let Some(location) = record.x87.c1 {
        cpu.x87.status |= (dump.locate(location) as u16 & 1) << 9;
    }
    if let Some(location) = record.x87.ip {
        cpu.x87.ip = dump.locate(location) as u32;
    }
    if let Some(location) = record.x87.opcode {
        cpu.x87.opcode = dump.locate(location) as u16;
    }
    for (reg, location) in cpu.xmm.iter_mut().zip(record.xmm) {
        if let Some(location) = location {
            *reg = dump.locate(location).to_le_bytes();
        }
    }
    for &(offset, width, location) in &record.frame {
        frame(
            offset,
            &dump.locate(location).to_le_bytes()[..usize::from(width)],
        );
    }
    record.refund
}

fn binary80(bits: u64) -> adv_cpu_core::state::X87Register {
    let sign = ((bits >> 48) as u16) & 0x8000;
    let exponent = (bits >> 52) & 0x7ff;
    let fraction = bits & ((1 << 52) - 1);
    let (significand, exponent) = match exponent {
        0 if fraction == 0 => (0, 0),
        0 => {
            let shift = fraction.leading_zeros();
            (fraction << shift, (16383 - 1074 + 63 - shift) as u16)
        }
        0x7ff => ((1 << 63) | fraction << 11, 0x7fff),
        _ => ((1 << 63) | fraction << 11, (exponent + 16383 - 1023) as u16),
    };
    adv_cpu_core::state::X87Register::from_parts(significand, sign | exponent)
}
