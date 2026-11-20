//! Optimized lowering for worker jobs and resident forms (A64A-ROLE-4).
//!
//! Compile passes an owned CPU snapshot and the arbiter's build configuration.
//! The optimizer derives the form's entry facts from that snapshot and builds
//! from the decoded unit, explicit assumptions and configuration.
//!
//! An optimized form lowers the same `Unit` as the ordinary computation form,
//! but through an SSA IR ([`ir`]) whose values hold the whole guest state:
//! registers, EFLAGS, the x87 stack, XMM and MMX registers, and frame bytes. Only
//! memory outside the frame stays memory. The IR is checked by [`verify`]
//! before emission, and every point that can fail names a recovery record
//! ([`recovery`]) from which the owner continues on the ordinary path.
//!
//! # Correctness
//!
//! Write the interpreter's step relation as the reference. A job starts from a
//! context in the admission predicate: the facts the form assumes at entry. Nothing
//! else reads or writes the job's admitted ranges while it runs (A64-TR-9,
//! A64-MEM-5), so only its end is observable: an exit, a recall or a fault.
//! An optimized run either reaches an exit whose state equals the reference
//! state there, or stops at a region start `p` with a state from which
//! continuing the reference observes what continuing from the reference
//! state at `p` would (`Cont`). The second case is a recovery.
//!
//! **Constants.** Abstract interpretation from the entry facts
//! computes, at each point, facts every reference state reaching it satisfies:
//! control words, TOP and tags, ESP as entry ESP plus a constant, and frame
//! bytes last written with a constant. Folding a fact changes no reachable
//! result.
//!
//! **Frame values.** Every frame access addresses entry ESP plus a
//! constant, and admission makes the frame span disjoint from every other
//! range of the job. Memory is then the frame bytes and the rest, and the
//! frame bytes behave as variables. Recovery writes back the bytes whose
//! values differ from memory and that execution from `p` can read; before a
//! validation only region-start values may reach frame memory, so the
//! write-back restores the state at `p` in every byte that matters. A commit
//! of frame values to memory on an edge into a loop head follows validation,
//! and nothing that can fail follows it before the head starts a region, so
//! no failure returns past it.
//!
//! **Floating point (A64-OPT-4).** Under the default FPCR and x87 precision
//! 53, AArch64 and x87 round once to the same precision. The value checks
//! exclude exceptions other than PE, assumed at entry, and results near the
//! binary64 range limits that could differ from x87's wider exponent range.
//! Recovery recomputes C1. x87 operands stay non-NaN: admission and loads
//! reject NaNs, and arithmetic rejects a NaN from numeric operands.
//! SSE can propagate quiet NaNs and checks subnormal inputs when DE is clear.
//!
//! **Recovery (A64-OPT-5, A64-OPT-6).** Between a region start `p` and the next
//! effect point nothing reaches memory outside the frame, because stores are
//! effect points, and frame memory holds only values from `p`. Guest memory at
//! a failure therefore equals the reference memory at `p`, and the record
//! gives every component execution from `p` can read, so the recovered state
//! is the reference state at `p` in every observable respect. Liveness is
//! computed over the guest instructions from `p`, so moving IR operations
//! across a store leaves its record intact. A component the form never
//! changes still holds its entry value, so records omit it. No assumption
//! about faults is needed: a load from a wrong address faults and recovers
//! like any failure. A store executes only after validation, with an address
//! and a path the reference shares, and stores keep their order, so the
//! guest boundary after a store is again a region start.
//!
//! **Retirement (A64-OPT-6).** Edges charge the countdown as the trace's
//! edge nets do, so the instructions charged and not retired at a point
//! depend only on its trace index; a record holds that refund.

pub(crate) mod back;
pub(crate) mod computation;
pub(crate) mod eval;
pub(crate) mod front;
pub(crate) mod ir;
mod loops;
pub(crate) mod recovery;
pub(crate) mod verify;

use adv_cpu_core::CpuState;

pub(crate) use crate::facts::Assumed;
use ir::Source;

/// Fills `out` with the entry cells of an optimized form when `cpu` meets the
/// facts it assumes, returning whether it admitted. `frame` reads `width` guest
/// bytes at an address.
pub(crate) fn admit_into(
    entry: &[Source],
    assumed: &Assumed,
    cpu: &CpuState,
    out: &mut Vec<u128>,
    frame: impl Fn(u32, u8) -> u64,
) -> bool {
    out.clear();
    let x87 = &cpu.x87;
    let esp = cpu.regs[4];
    let holds = (!assumed.x87
        || x87.control == assumed.control
            && x87.status & !0x3800 == assumed.status
            && x87.top() as u8 == assumed.top
            && x87.tag == assumed.tags)
        && (!assumed.mmx || x87.status & 0x80 == 0)
        && (cpu.mxcsr ^ assumed.mxcsr) & assumed.mxcsr_mask == 0
        && assumed.esp_low.is_none_or(|low| (esp & 15) as u8 == low);
    if !holds {
        return false;
    }
    for &source in entry {
        let cell = match source {
            Source::Reg(i) => u128::from(cpu.regs[usize::from(i)]),
            Source::FsBase => u128::from(cpu.fs_base),
            Source::Eflags => u128::from(cpu.eflags),
            Source::Xmm(i) => u128::from_le_bytes(cpu.xmm[usize::from(i)]),
            Source::Mmx(i) => u128::from(cpu.mmx[usize::from(i)]),
            Source::X87(i) => {
                let Some(value) = crate::facts::binary64(x87.regs[usize::from(i)]) else {
                    return false;
                };
                u128::from(value)
            }
            Source::X87Ip => u128::from(x87.ip),
            Source::X87Opcode => u128::from(x87.opcode),
            Source::Frame { offset, width } => {
                u128::from(frame(esp.wrapping_add_signed(offset), width))
            }
        };
        out.push(cell);
    }
    true
}

/// The entry cells of an optimized form when `cpu` meets the facts it
/// assumes, or `None`. `frame` reads `width` guest bytes at an address.
#[cfg(test)]
pub(crate) fn admit(
    entry: &[Source],
    assumed: &Assumed,
    cpu: &CpuState,
    frame: impl Fn(u32, u8) -> u64,
) -> Option<Vec<u128>> {
    let mut out = Vec::with_capacity(entry.len());
    admit_into(entry, assumed, cpu, &mut out, frame).then_some(out)
}
