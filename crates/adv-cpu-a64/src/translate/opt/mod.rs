//! The optimizing tier for admitted computation.
//!
//! An optimized form lowers the same `Unit` as the ordinary computation form,
//! but through an SSA IR ([`ir`]) whose values hold the whole guest state:
//! registers, EFLAGS, the x87 stack, XMM registers and frame bytes. Only
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
//! values differ from memory; before a validation only region-start values
//! may reach frame memory, so the write-back restores the state at `p`.
//!
//! **Floating point.** Let `a` and `b` be non-NaN binary64 values.
//! Under precision 53 and rounding to nearest, x87 rounds `a op b` once to 53
//! bits with an exponent range containing binary64's. With `FPCR.FZ` set, no
//! IDC means neither operand is a nonzero denormal; no IOC, no NaN result;
//! no OFC, no overflow; no UFC, a result that is zero or normal. In the
//! remaining cases both round the same exact value once to the same set, so
//! the results agree bit for bit. The status word changes only in PE, set at
//! entry, and C1, which recovery recomputes. Operands stay non-NaN by
//! induction: entry values are checked at admission, x87 loads check, and an
//! operation without IOC on non-NaN operands has a non-NaN result.
//!
//! **Recovery (A64-OPT-5, A64-OPT-6).** Between a region start `p` and the next
//! effect point nothing reaches memory outside the frame, because stores are
//! effect points, and frame memory holds only values from `p`. Guest memory at
//! a failure therefore equals the reference memory at `p`, and the record
//! gives every component execution from `p` can read, so the recovered state
//! is the reference state at `p` in every observable respect. No assumption
//! about faults is needed: a load from a wrong address faults and recovers
//! like any failure. A store executes only after validation, with an address
//! and a path the reference shares, and stores keep their order, so the
//! guest boundary after a store is again a region start.
//!
//! **Retirement (A64-OPT-6).** Loop heads prepay their segment; a record holds
//! the instructions prepaid but not retired at its region start.

pub(crate) mod ir;
pub(crate) mod recovery;
pub(crate) mod verify;
