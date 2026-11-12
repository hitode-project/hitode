//! The state the head loop's outer iterations carry from one to the next
//! (A64-TR-9). Each component is invariant, affine in the iteration index,
//! private to an iteration, or exchanged. A private component is written
//! before it is read in every iteration and again before the loop exits.
//! Iterations that exchange nothing else can run as blocks: a block starts
//! from the entry context with its affine components advanced. Components
//! are registers, flags, SIMD registers and frame bytes relative to ESP.

use iced_x86::{CpuidFeature, Instruction, InstructionInfoFactory, Mnemonic, OpAccess, OpKind};
use iced_x86::{Register, RflagsBits};

use super::super::decode::Op;
use super::super::integer::Integer;
use super::super::trace::{Edge, Mode, Next, Trace};
use super::{Division, Graph, Loop};

const XMM: u32 = 8;
const MM: u32 = 16;
const FLAGS: u32 = 24;
/// TOP and the tag word, which every MMX instruction but EMMS sets.
const ENV: u64 = 1 << 31;
const FLAG_BITS: [u32; 7] = [
    RflagsBits::OF,
    RflagsBits::SF,
    RflagsBits::ZF,
    RflagsBits::AF,
    RflagsBits::CF,
    RflagsBits::PF,
    RflagsBits::DF,
];
/// The widest frame span the analysis tracks, in bytes.
const FRAME_SPAN: i32 = 4096;

/// Component sets: word 0 holds the fixed components, the rest frame bytes.
type Set = Vec<u64>;

#[derive(Clone, Default)]
struct Use {
    reads: Set,
    /// Full definitions: the old value cannot reach the result.
    defines: Set,
    writes: Set,
}

/// One instruction's fixed components, ESP change and frame accesses.
struct Decoded {
    reads: u64,
    defines: u64,
    writes: u64,
    stack: i32,
    frame: Vec<(i32, u32, OpAccess)>,
}

fn flags(bits: u32) -> Option<u64> {
    let mut set = 0;
    let mut rest = bits;
    for (i, &flag) in FLAG_BITS.iter().enumerate() {
        if bits & flag != 0 {
            set |= 1 << (FLAGS + i as u32);
            rest &= !flag;
        }
    }
    (rest == 0).then_some(set)
}

fn register(reg: Register) -> Option<(u64, bool)> {
    if reg.is_gpr() {
        Some((1 << reg.full_register32().number(), reg.size() == 4))
    } else if reg.is_xmm() {
        Some((1 << (XMM + reg.number() as u32), true))
    } else if reg.is_mm() {
        Some((1 << (MM + reg.number() as u32), true))
    } else {
        None
    }
}

/// How `ins` moves ESP, and whether by an immediate adjustment, or `None`
/// if it lets a stack address escape. ESP may move only by pushes, pops and
/// immediate adjustments, and may be read only as a memory base: a block's
/// frame is a private copy at another address.
fn stack(ins: &Instruction) -> Option<(i32, bool)> {
    let adjust = matches!(ins.mnemonic(), Mnemonic::Add | Mnemonic::Sub)
        && ins.op0_kind() == OpKind::Register
        && ins.op0_register() == Register::ESP
        && ins.op1_kind() != OpKind::Register
        && ins.op1_kind() != OpKind::Memory;
    let named = (0..ins.op_count())
        .any(|i| ins.op_kind(i) == OpKind::Register && ins.op_register(i) == Register::ESP);
    let indexed = (0..ins.op_count()).any(|i| ins.op_kind(i) == OpKind::Memory)
        && (ins.memory_index() == Register::ESP
            || ins.memory_base() == Register::ESP
                && (ins.memory_index() != Register::None || ins.mnemonic() == Mnemonic::Lea));
    if named && !adjust || indexed || matches!(ins.mnemonic(), Mnemonic::Leave | Mnemonic::Enter) {
        return None;
    }
    let mut stack = ins.stack_pointer_increment();
    if adjust {
        let imm = ins.immediate(1) as i32;
        stack += if ins.mnemonic() == Mnemonic::Add {
            imm
        } else {
            -imm
        };
    }
    Some((stack, adjust))
}

/// What `ins` reads and writes, or `None` for state a block cannot
/// reproduce: x87, MXCSR, segments, system flags, or an escaping stack
/// address.
fn decoded(ins: &Instruction, info: &mut InstructionInfoFactory) -> Option<Decoded> {
    if ins.cpuid_features().contains(&CpuidFeature::FPU)
        || matches!(
            ins.mnemonic(),
            Mnemonic::Ldmxcsr | Mnemonic::Stmxcsr | Mnemonic::Fxrstor | Mnemonic::Fxsave
        )
    {
        return None;
    }
    let (stack, _) = stack(ins)?;
    let info = info.info(ins);
    let mut out = Decoded {
        reads: 0,
        defines: 0,
        writes: 0,
        stack,
        frame: Vec::new(),
    };
    for used in info.used_memory() {
        if used.base() == Register::ESP {
            let size = u32::try_from(used.memory_size().size()).ok()?;
            out.frame
                .push((used.displacement() as i32, size, used.access()));
        }
    }
    let mut mmx = false;
    for used in info.used_registers() {
        let reg = used.register();
        if reg.is_segment_register() && used.access() == OpAccess::Read {
            continue;
        }
        // ESP is tracked as the frame offset.
        if reg == Register::ESP {
            continue;
        }
        let (bit, full) = register(reg)?;
        mmx |= bit >> MM & 0xff != 0;
        match used.access() {
            OpAccess::Read | OpAccess::CondRead => out.reads |= bit,
            OpAccess::Write if full => {
                out.defines |= bit;
                out.writes |= bit;
            }
            OpAccess::Write
            | OpAccess::CondWrite
            | OpAccess::ReadWrite
            | OpAccess::ReadCondWrite => {
                out.reads |= bit;
                out.writes |= bit;
            }
            OpAccess::None | OpAccess::NoMemAccess => {}
        }
    }
    out.reads |= flags(ins.rflags_read())?;
    let modified = flags(ins.rflags_modified())?;
    // A shift or rotate by a zero count keeps every flag.
    let counted = matches!(
        ins.mnemonic(),
        Mnemonic::Shl
            | Mnemonic::Sal
            | Mnemonic::Shr
            | Mnemonic::Sar
            | Mnemonic::Rol
            | Mnemonic::Ror
            | Mnemonic::Rcl
            | Mnemonic::Rcr
            | Mnemonic::Shld
            | Mnemonic::Shrd
    );
    let last = ins.op_count() - 1;
    if counted && (ins.op_kind(last) != OpKind::Immediate8 || ins.immediate8() & 31 == 0) {
        out.reads |= modified;
    } else {
        out.defines |= modified;
    }
    out.writes |= modified;
    if mmx {
        out.defines |= ENV;
        out.writes |= ENV;
    } else if ins.mnemonic() == Mnemonic::Emms {
        out.writes |= ENV;
    }
    Some(out)
}

fn mark(set: &mut Set, lo: i32, start: i32, len: u32) {
    for byte in start - lo..start - lo + len as i32 {
        set[1 + byte as usize / 64] |= 1 << (byte % 64);
    }
}

fn any(a: &Set, b: &Set, without: &Set) -> bool {
    a.iter()
        .zip(b)
        .zip(without)
        .any(|((a, b), c)| a & b & !c != 0)
}

/// The frame span `[lo, hi)` the head loop of `trace` writes, relative to
/// ESP at its head, or `Some(None)` if it writes none, when its iterations
/// carry only the `affine` registers and the affine dword `slots`, and leave
/// the loop only at its test. `still` registers and slots marked true step
/// by zero.
pub(super) fn independent(
    trace: &Trace,
    (affine, still): (u8, u8),
    slots: &[(i32, bool)],
    divisions: &[Option<Division>],
    decode: impl Fn(u32, u32) -> Option<Instruction>,
) -> Option<Option<(i32, i32)>> {
    let graph = Graph::build(trace)?;
    let lp = &graph.loops[graph.heads[0]?];
    let n = trace.len();
    let mut info = InstructionInfoFactory::new();
    let mut decoded_at: Vec<Option<Decoded>> = (0..n).map(|_| None).collect();
    for at in (0..n).filter(|&at| lp.inside[at]) {
        let inst = &trace.insts[at];
        let inside = |edge: Option<Edge>| match edge {
            Some(Edge::Internal(to)) => lp.inside[to] || at == lp.test,
            None => true,
            _ => at == lp.test,
        };
        let next = match inst.next {
            Next::Fall => None,
            Next::Edge(edge) => Some(edge),
            Next::Dynamic => return None,
        };
        // A side exit would let a later block run past it.
        if inst.mode != Mode::Plain
            || !inside(next)
            || !inside(inst.taken)
            || matches!(inst.op, Op::Integer(Integer::Divide(..)) if divisions[at].is_none())
        {
            return None;
        }
        let ins = decode(inst.pc, inst.next_pc.wrapping_sub(inst.pc))?;
        decoded_at[at] = Some(decoded(&ins, &mut info)?);
    }
    // ESP relative to the head on entry to each instruction: every path
    // agrees, and the loop returns to the head with it unchanged.
    let mut offset: Vec<Option<i32>> = vec![None; n];
    offset[0] = Some(0);
    let (mut lo, mut hi) = (0, 0);
    for &at in graph.order.iter().filter(|&&at| lp.inside[at]) {
        let here = offset[at]?;
        let d = decoded_at[at].as_ref()?;
        for &(disp, size, _) in &d.frame {
            lo = lo.min(here.checked_add(disp)?);
            hi = hi.max(here.checked_add(disp)?.checked_add(size as i32)?);
        }
        let after = here.checked_add(d.stack)?;
        for &to in graph.edges[at].iter().flatten() {
            if !lp.inside[to] {
                continue;
            }
            let expected = if to == 0 { Some(0) } else { offset[to] };
            match expected {
                Some(x) if x != after => return None,
                Some(_) => {}
                None => offset[to] = Some(after),
            }
        }
    }
    if hi - lo > FRAME_SPAN {
        return None;
    }
    let words = 1 + (hi - lo) as usize / 64 + 1;
    let empty = vec![0; words];
    let mut uses = vec![Use::default(); n];
    let mut written = empty.clone();
    for at in (0..n).filter(|&at| lp.inside[at]) {
        let d = decoded_at[at].as_ref()?;
        let here = offset[at]?;
        let mut used = Use {
            reads: empty.clone(),
            defines: empty.clone(),
            writes: empty.clone(),
        };
        used.reads[0] = d.reads;
        used.defines[0] = d.defines;
        used.writes[0] = d.writes;
        for &(disp, size, access) in &d.frame {
            let start = here + disp;
            match access {
                OpAccess::Read | OpAccess::CondRead => mark(&mut used.reads, lo, start, size),
                OpAccess::Write => {
                    mark(&mut used.defines, lo, start, size);
                    mark(&mut used.writes, lo, start, size);
                }
                OpAccess::None | OpAccess::NoMemAccess => {}
                _ => {
                    mark(&mut used.reads, lo, start, size);
                    mark(&mut used.writes, lo, start, size);
                }
            }
        }
        for (w, u) in written.iter_mut().zip(&used.writes) {
            *w |= u;
        }
        uses[at] = used;
    }
    let mut carried = empty.clone();
    let mut steady = empty.clone();
    carried[0] = u64::from(affine);
    steady[0] = u64::from(still);
    for &(slot, zero) in slots
        .iter()
        .filter(|&&(slot, _)| lo <= slot && slot + 4 <= hi)
    {
        mark(&mut carried, lo, slot, 4);
        if zero {
            mark(&mut steady, lo, slot, 4);
        }
    }
    let frame_written = written[1..].iter().any(|&w| w != 0);
    // The last block runs the tail on its private frame too.
    if frame_written {
        for inst in (0..n)
            .filter(|&at| !lp.inside[at])
            .map(|at| &trace.insts[at])
        {
            stack(&decode(inst.pc, inst.next_pc.wrapping_sub(inst.pc))?)?;
        }
    }
    // Components the loop never writes keep their entry value.
    let invariant: Set = written.iter().zip(&steady).map(|(w, s)| !w | s).collect();
    let uniform = uniform(&graph, lp, &uses, &invariant);
    let k = uniform.len().min(UNIFORM);
    // Every iteration of a run takes a uniform branch the same way, so
    // each choice of their directions is checked on its own.
    for choice in 0..1u32 << k {
        let follows = |at: usize, edge: usize| {
            uniform[..k]
                .iter()
                .position(|&u| u == at)
                .is_none_or(|i| choice >> i & 1 == edge as u32)
        };
        if !private(&graph, lp, &uses, &carried, follows) {
            return None;
        }
    }
    Some(frame_written.then_some((lo, hi)))
}

/// The most uniform branches whose directions are checked separately.
const UNIFORM: usize = 4;

fn bits(set: &Set) -> impl Iterator<Item = usize> + '_ {
    set.iter().enumerate().flat_map(|(w, &word)| {
        (0..64)
            .filter(move |b| word >> b & 1 != 0)
            .map(move |b| w * 64 + b)
    })
}

/// The head loop's branches other than its test whose inputs hold the same
/// value whenever they run, in every iteration: a component's reaching
/// definition is the entry value of an `invariant` one, or one instruction
/// whose own inputs are steady. A load from memory is steady with its
/// address, since admitted iterations never write what another reads.
fn uniform(graph: &Graph, lp: &Loop, uses: &[Use], invariant: &Set) -> Vec<usize> {
    const UNSEEN: u16 = u16::MAX - 2;
    const HEAD: u16 = u16::MAX - 1;
    const MANY: u16 = u16::MAX;
    let n = uses.len();
    let width = invariant.len() * 64;
    if n >= usize::from(UNSEEN) {
        return Vec::new();
    }
    let inside = || graph.order.iter().copied().filter(|&at| lp.inside[at]);
    let successors = |at: usize| {
        graph.edges[at]
            .into_iter()
            .flatten()
            .filter(|&to| lp.inside[to] && to != 0)
    };
    // The reaching definitions on entry to each instruction.
    let mut reaching = vec![UNSEEN; n * width];
    reaching[..width].fill(HEAD);
    let mut out = vec![UNSEEN; width];
    let mut changed = true;
    while changed {
        changed = false;
        for at in inside() {
            out.copy_from_slice(&reaching[at * width..][..width]);
            for b in bits(&uses[at].writes) {
                out[b] = at as u16;
            }
            for to in successors(at) {
                for (r, &o) in reaching[to * width..][..width].iter_mut().zip(&out) {
                    let joined = match (*r, o) {
                        (_, UNSEEN) => *r,
                        (UNSEEN, o) => o,
                        (r, o) if r == o => r,
                        _ => MANY,
                    };
                    changed |= joined != *r;
                    *r = joined;
                }
            }
        }
    }
    let mut steady = vec![true; n];
    let mut changed = true;
    while changed {
        changed = false;
        for at in inside() {
            let holds = bits(&uses[at].reads).all(|b| match reaching[at * width + b] {
                HEAD => invariant[b / 64] >> (b % 64) & 1 != 0,
                UNSEEN | MANY => false,
                site => steady[usize::from(site)],
            });
            if steady[at] && !holds {
                steady[at] = false;
                changed = true;
            }
        }
    }
    inside()
        .filter(|&at| {
            at != lp.test
                && steady[at]
                && graph.edges[at]
                    .iter()
                    .all(|to| to.is_some_and(|to| lp.inside[to] && to != 0))
        })
        .collect()
}

/// Whether, along the edges `follows` keeps, every component an iteration
/// reads from another is affine, and each exchanged one is defined again
/// before the loop exits.
fn private(
    graph: &Graph,
    lp: &Loop,
    uses: &[Use],
    carried: &Set,
    follows: impl Fn(usize, usize) -> bool,
) -> bool {
    let n = uses.len();
    let words = carried.len();
    let mut reached = vec![false; n];
    reached[0] = true;
    let mut incoming = vec![Vec::new(); n];
    let mut written = vec![0; words];
    for &at in graph.order.iter().filter(|&&at| lp.inside[at]) {
        if !reached[at] {
            continue;
        }
        for (w, u) in written.iter_mut().zip(&uses[at].writes) {
            *w |= u;
        }
        for (edge, to) in graph.edges[at].into_iter().enumerate() {
            if let Some(to) = to
                && lp.inside[to]
                && to != 0
                && follows(at, edge)
            {
                reached[to] = true;
                incoming[to].push(at);
            }
        }
    }
    let exchanged: Set = written.iter().zip(carried).map(|(w, c)| w & !c).collect();
    // The components every path from the head has defined.
    let mut defined = vec![vec![u64::MAX; words]; n];
    let mut changed = true;
    while changed {
        changed = false;
        for &at in graph.order.iter().filter(|&&at| reached[at]) {
            let before = if at == 0 {
                vec![0; words]
            } else {
                incoming[at]
                    .iter()
                    .fold(vec![u64::MAX; words], |mut set, &p| {
                        for (s, d) in set.iter_mut().zip(&defined[p]) {
                            *s &= d;
                        }
                        set
                    })
            };
            if any(&uses[at].reads, &exchanged, &before) {
                return false;
            }
            let after: Set = before
                .iter()
                .zip(&uses[at].defines)
                .map(|(b, d)| b | d)
                .collect();
            if after != defined[at] {
                defined[at] = after;
                changed = true;
            }
        }
    }
    let all = vec![u64::MAX; words];
    !reached[lp.test] || !any(&exchanged, &all, &defined[lp.test])
}
