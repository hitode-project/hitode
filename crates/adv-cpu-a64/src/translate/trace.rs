//! Traces: the guest instructions one unit covers, in emission order
//! (execution ABI §A7).
//!
//! A trace follows the fall-through path, direct jumps and direct calls. A
//! call inlines its callee in a new frame while depth and room allow, so
//! the callee's `ret` becomes a check that the popped EIP is the expected
//! one; otherwise it becomes a real call whose continuation is the next
//! instruction of the trace, reached again through the shadow stack.
//! Reaching an instruction already in the trace in the same frame closes
//! a loop with an internal edge; conditional branches to instructions in
//! the trace are internal edges too, and other ones side exits.
//!
//! Budget (§A5.4): a region starts at the entry and at each continuation and
//! prepays every instruction up to the next one. An edge then settles the
//! difference between what its target region still owes and what the
//! source region prepaid but will not complete; only an edge that owes
//! more checks the countdown.
//!
//! Flags (§A3): a backward liveness pass over the trace, with every exit,
//! fault site and countdown check observing all flags.

use adv_cpu_core::memory::Memory;

use super::decode::{Effects, Fetcher, Helper, Op};
use super::flags::FlagSet;

/// Longest trace, in guest instructions.
pub const MAX_INSTRUCTIONS: usize = 96;
/// What an x87 instruction counts toward `MAX_INSTRUCTIONS`: each emits
/// some hundred host instructions, and a unit must stay within the 32 KiB
/// reach of `tbz` to its cold code.
const X87_WEIGHT: usize = 4;
/// Deepest inlined call.
const MAX_DEPTH: u8 = 8;
/// Room an inlined callee should still have.
const INLINE_ROOM: usize = 24;
/// Slots of the (EIP, frame) index: a power of two well above the longest
/// trace.
const SLOTS: usize = 256;

/// Where control goes from an instruction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    /// Instruction `index` of the same trace.
    Internal(usize),
    /// Dispatch to a static guest EIP.
    Exit(u32),
    /// Leave the instruction at the EIP to the interpreter.
    Interpret(u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Next {
    /// The next instruction of the trace.
    Fall,
    Edge(Edge),
    /// A target computed at run time, which the instruction branches to.
    Dynamic,
}

/// How a call or a ret is translated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Plain,
    /// A call that pushes a shadow entry; the next instruction is its
    /// continuation and starts a region.
    Real,
    /// A ret from an inlined callee, expected to return to the EIP.
    Inline(u32),
}

#[derive(Clone, Copy, Debug)]
pub struct Inst {
    pub pc: u32,
    pub next_pc: u32,
    pub op: Op,
    pub effects: Effects,
    pub mode: Mode,
    pub next: Next,
    /// Where a taken jcc goes.
    pub taken: Option<Edge>,
    frame: u16,
}

#[derive(Clone, Copy)]
struct Frame {
    parent: u16,
    ret: u32,
    depth: u8,
}

/// A trace and its analysis; its buffers are reused from trace to trace.
#[derive(Default)]
pub struct Trace {
    pub insts: Vec<Inst>,
    /// Guest bytes the trace was decoded from, as (address, length).
    pub code: Vec<(u32, u32)>,
    /// End of each instruction's region: the next region start.
    region_end: Vec<usize>,
    /// Instructions that internal edges reach.
    pub merge: Vec<bool>,
    /// Flags read on entry to each instruction.
    pub live_in: Vec<FlagSet>,
    /// Flags read after each instruction, on any path.
    pub live_out: Vec<FlagSet>,
    frames: Vec<Frame>,
    /// Instruction index + 1 by (EIP, frame), open addressing.
    slots: Vec<u8>,
    /// `Fetcher::failure` of the fetch that ended the trace.
    pub failure: u8,
    /// What each `Op::Helper` runs.
    pub helpers: Vec<Helper>,
}

/// The stops, behind a one-word filter: most EIPs are not stops.
struct Stops<'s> {
    stops: &'s [u32],
    filter: u64,
}

impl<'s> Stops<'s> {
    fn new(stops: &'s [u32]) -> Self {
        let filter = stops
            .iter()
            .fold(0, |filter, &stop| filter | 1 << (stop & 63));
        Self { stops, filter }
    }

    fn contains(&self, pc: u32) -> bool {
        self.filter >> (pc & 63) & 1 != 0 && self.stops.contains(&pc)
    }
}

fn slot(pc: u32, frame: u16) -> usize {
    let key = pc ^ u32::from(frame) << 24;
    (key.wrapping_mul(0x9E37_79B1) >> 24) as usize & (SLOTS - 1)
}

impl Trace {
    /// Traces from `entry`, ending before any of `stops` except at the
    /// entry.
    pub fn build(&mut self, memory: &Memory, entry: u32, stops: &[u32]) {
        self.insts.clear();
        self.code.clear();
        self.frames.clear();
        self.frames.push(Frame {
            parent: 0,
            ret: 0,
            depth: 0,
        });
        self.slots.clear();
        self.slots.resize(SLOTS, 0);
        let mut fetcher = Fetcher::new(memory);
        self.helpers.clear();
        fetcher.helpers = Some(std::mem::take(&mut self.helpers));
        let stops = Stops::new(stops);
        let (mut pc, mut frame) = (entry, 0u16);
        let mut weight = 0;
        loop {
            let len = self.insts.len();
            let end = if len > 0 && stops.contains(pc) {
                Some(Edge::Exit(pc))
            } else if let Some(index) = self.find(pc, frame) {
                Some(Edge::Internal(index))
            } else if len == MAX_INSTRUCTIONS || weight >= MAX_INSTRUCTIONS {
                Some(Edge::Exit(pc))
            } else {
                None
            };
            if let Some(edge) = end {
                self.insts[len - 1].next = Next::Edge(edge);
                break;
            }
            let Some((op, size)) = fetcher.fetch(pc) else {
                self.failure = fetcher.failure;
                if let Some(last) = self.insts.last_mut() {
                    last.next = Next::Edge(Edge::Interpret(pc));
                }
                break;
            };
            let next_pc = pc.wrapping_add(size);
            weight += if matches!(op, Op::X87(_)) {
                X87_WEIGHT
            } else {
                1
            };
            match self.code.last_mut() {
                // Spans grow along the fall-through path.
                Some((start, bytes)) if start.wrapping_add(*bytes) == pc => *bytes += size,
                _ => self.code.push((pc, size)),
            }
            let mut inst = Inst {
                pc,
                next_pc,
                op,
                effects: op.effects(),
                mode: Mode::Plain,
                next: Next::Fall,
                taken: None,
                frame,
            };
            // A continuation must be new to the trace and leave room after it.
            let continuation = || {
                len + 4 < MAX_INSTRUCTIONS
                    && !stops.contains(next_pc)
                    && self.find(next_pc, frame).is_none()
            };
            let then = match op {
                Op::Jmp(target) => Some((target, frame)),
                // `call $+5` only pushes its own address.
                Op::Call(target) if target == next_pc => Some((next_pc, frame)),
                Op::Call(target) => {
                    let depth = self.frames[usize::from(frame)].depth;
                    if depth < MAX_DEPTH
                        && len + 1 + INLINE_ROOM <= MAX_INSTRUCTIONS
                        && !stops.contains(target)
                    {
                        Some((target, self.callee(frame, next_pc, depth)))
                    } else if continuation() {
                        inst.mode = Mode::Real;
                        Some((next_pc, frame))
                    } else {
                        inst.next = Next::Edge(Edge::Exit(target));
                        None
                    }
                }
                Op::CallIndirect(_) if continuation() => {
                    inst.mode = Mode::Real;
                    Some((next_pc, frame))
                }
                Op::Ret(_) if frame != 0 => {
                    let Frame { parent, ret, .. } = self.frames[usize::from(frame)];
                    inst.mode = Mode::Inline(ret);
                    Some((ret, parent))
                }
                Op::Ret(_) | Op::JmpIndirect(_) | Op::CallIndirect(_) => {
                    inst.next = Next::Dynamic;
                    None
                }
                _ => Some((next_pc, frame)),
            };
            self.insert(pc, frame, len);
            self.insts.push(inst);
            match then {
                Some((to, to_frame)) => (pc, frame) = (to, to_frame),
                None => break,
            }
        }
        self.helpers = fetcher.helpers.take().unwrap_or_default();
        // Inlined callees and loops revisit bytes: watch each one once.
        self.code.sort_unstable();
        let mut merged = 0;
        for index in 0..self.code.len() {
            let (start, len) = self.code[index];
            match merged {
                0 => merged = 1,
                _ => {
                    let last = &mut self.code[merged - 1];
                    let end = last.0.saturating_add(last.1);
                    if start <= end {
                        last.1 = last.1.max(start.saturating_add(len) - last.0);
                    } else {
                        self.code[merged] = (start, len);
                        merged += 1;
                    }
                }
            }
        }
        self.code.truncate(merged);
        for index in 0..self.insts.len() {
            let inst = self.insts[index];
            if let Op::Jcc(_, target) = inst.op {
                self.insts[index].taken = Some(match self.find(target, inst.frame) {
                    Some(to) if !stops.contains(target) => Edge::Internal(to),
                    _ => Edge::Exit(target),
                });
            }
        }
        self.analyze();
    }

    fn find(&self, pc: u32, frame: u16) -> Option<usize> {
        let mut at = slot(pc, frame);
        loop {
            match self.slots[at] {
                0 => return None,
                n => {
                    let inst = &self.insts[usize::from(n) - 1];
                    if inst.pc == pc && inst.frame == frame {
                        return Some(usize::from(n) - 1);
                    }
                }
            }
            at = (at + 1) & (SLOTS - 1);
        }
    }

    fn insert(&mut self, pc: u32, frame: u16, index: usize) {
        let mut at = slot(pc, frame);
        while self.slots[at] != 0 {
            at = (at + 1) & (SLOTS - 1);
        }
        self.slots[at] = (index + 1) as u8;
    }

    /// The frame of a callee inlined from `frame`, returning to `ret`.
    fn callee(&mut self, frame: u16, ret: u32, depth: u8) -> u16 {
        let found = self
            .frames
            .iter()
            .position(|f| f.depth > 0 && f.parent == frame && f.ret == ret);
        let index = found.unwrap_or_else(|| {
            self.frames.push(Frame {
                parent: frame,
                ret,
                depth: depth + 1,
            });
            self.frames.len() - 1
        });
        index as u16
    }

    fn analyze(&mut self) {
        let len = self.insts.len();
        self.region_end.clear();
        self.region_end.resize(len, len);
        let mut end = len;
        for index in (0..len).rev() {
            self.region_end[index] = end;
            if index > 0 && self.insts[index - 1].mode == Mode::Real {
                end = index;
            }
        }
        self.merge.clear();
        self.merge.resize(len, false);
        let mut backward = false;
        for (index, inst) in self.insts.iter().enumerate() {
            if let Some(Edge::Internal(to)) = inst.taken {
                self.merge[to] = true;
                backward |= to <= index;
            }
            if let Next::Edge(Edge::Internal(to)) = inst.next {
                self.merge[to] = true;
                backward |= to <= index;
            }
        }
        self.live_in.clear();
        self.live_in.resize(len, FlagSet::NONE);
        self.live_out.clear();
        self.live_out.resize(len, FlagSet::NONE);
        // Without a backward edge, one backward pass sees every successor.
        while self.liveness() && backward {}
    }

    pub fn len(&self) -> usize {
        self.insts.len()
    }

    /// Whether `index` starts a region with its own countdown check.
    pub fn head(&self, index: usize) -> bool {
        index == 0 || self.insts[index - 1].mode == Mode::Real
    }

    /// Instructions a region head prepays.
    pub fn prepaid(&self, head: usize) -> u32 {
        (self.region_end[head] - head) as u32
    }

    /// What an arrival at `index` owes its region: it and the rest.
    pub fn owed(&self, index: usize) -> u32 {
        (self.region_end[index] - index) as u32
    }

    /// Prepaid instructions left uncompleted once `index` completes.
    pub fn unused(&self, index: usize) -> u32 {
        (self.region_end[index] - index - 1) as u32
    }

    /// The countdown an edge from `from` to `to` takes: positive when it
    /// must check, negative when it refunds.
    pub fn net(&self, from: usize, to: usize) -> i64 {
        i64::from(self.owed(to)) - i64::from(self.unused(from))
    }

    fn edge_live(&self, from: usize, edge: Edge) -> FlagSet {
        match edge {
            // A check that refuses leaves with every flag.
            Edge::Internal(to) if self.net(from, to) > 0 => FlagSet::ALL,
            Edge::Internal(to) => self.live_in[to],
            Edge::Exit(_) | Edge::Interpret(_) => FlagSet::ALL,
        }
    }

    /// One backward pass; whether anything changed.
    fn liveness(&mut self) -> bool {
        let mut changed = false;
        for index in (0..self.len()).rev() {
            let inst = &self.insts[index];
            let mut out = match inst.next {
                Next::Fall => self.live_in[index + 1],
                Next::Edge(edge) => self.edge_live(index, edge),
                Next::Dynamic => FlagSet::ALL,
            };
            if let Some(taken) = inst.taken {
                out = out.or(self.edge_live(index, taken));
            }
            // The callee, and a mismatching return, see every flag.
            if inst.mode != Mode::Plain {
                out = FlagSet::ALL;
            }
            let effects = inst.effects;
            let live = if effects.site {
                FlagSet::ALL
            } else {
                effects.reads.or(out.minus(effects.writes))
            };
            if live != self.live_in[index] || out != self.live_out[index] {
                changed = true;
                self.live_in[index] = live;
                self.live_out[index] = out;
            }
        }
        changed
    }
}
