# Execution

ID prefix: EXEC

This spec covers the contract that Tier 0 and the AArch64 translator share. The a64 engine has its own spec, [a64.md](a64.md); [README.md](README.md) explains the format. Source code and layout tests define layouts, so offsets are not repeated here. The module docs of `adv-cpu-core::execution_abi` explain the design.

## Shared

### EXEC-CORE-1 Authoritative state

**Rule**: `CpuState` is the authoritative guest state that Tier 0 and a64 share. `ExecutionContext` MAY hold restartable REP progress. `ExecutionContext` MUST NOT hold a live host continuation that belongs to another owner.
**Verification**: Review — no single test can observe the ownership constraint.

### EXEC-CORE-3 BSWAP with an operand-size prefix

**Rule**: `BSWAP` with an operand-size prefix MUST clear the low 16 bits of the destination and keep the high 16 bits and EFLAGS. The 32-bit form MUST reverse all four bytes. Every engine MUST match the interpreter and MUST NOT emit an undefined host form.
**Verification**: Test
**Basis**: [Intel SDM Volume 2, BSWAP](https://cdrdv2-public.intel.com/774492/325383-sdm-vol-2abcd.pdf) leaves the 16-bit result undefined.

> Note: this is a compatibility choice of the guest CPU, not a claim about every physical processor.

### EXEC-CORE-4 Audio call marks

**Rule**: Thunks MUST classify audio bindings at registration, and dispatch MUST record their latest call using the guest wait-deadline clock. Thread queries MUST report whether that timestamp is strictly after the supplied time, without affecting scheduling.
**Verification**: Test

| Binding | Audio classification |
|---|---|
| dsound.dll | All functions and COM methods |
| quartz.dll | All functions and COM methods, including interfaces registered by dshow |
| wmvcore.dll | All functions and COM methods |
| winmm.dll | Names starting with waveOut, waveIn, midiOut, mci, mixer, aux, PlaySound, or sndPlaySound |
| Other bindings | Not audio; winmm timer functions are excluded |
| Never called audio / unknown thread | No timestamp; query returns false |

### EXEC-CORE-5 Tier 0 baseline accounting

**Rule**: Diagnostic reports MUST count completed Tier 0 instructions separately from instruction/REP-element work units. Throughput MUST divide retired instructions by elapsed host wall seconds, including waits. Disabled counting MUST report null, without adding default hot-path timing.
**Verification**: Test

Exact retirement is enabled by `--profile`, `--frame-budget`, or the live debugger. The synthetic `tier0_baseline` example exercises integer loops, REP strings, x87, MMX mixed with integer operations and indirect calls, asserting completed-instruction counts.

### EXEC-CORE-6 Tier 0 decode cache validity

**Rule**: Tier 0 MAY skip private page validation only when the block's last validated owner generation matches. Page-version, mapping and permission changes MUST advance that generation, and generations MUST distinguish memory owners. Changed generations and SHARED code MUST trigger validation on each entry, retaining decoded blocks only when permissions, mappings and bytes match.
**Verification**: Test

Cached successors MUST match entry EIP and block-cache generation. They MUST pass the same memory and native-publication revision checks as a lookup. Clearing either decode cache MUST advance the cache generation before any entry is reused. Successor hits MUST preserve entry stops, budgets and observation boundaries.

### EXEC-CORE-7 Observed data write barriers

**Rule**: `Memory::direct` MUST refuse writes intersecting observed bytes, including on non-executable pages. Checked stores intersecting observed bytes MUST advance the corresponding dependency versions. On non-executable pages, stores outside observed bytes MUST NOT invalidate dependencies.
**Verification**: Test

With software-checked memory, executable permission alone MUST NOT prevent `Memory::direct` writes to ordinary writable bytes outside observed dependencies.
Native host protection MUST retain its executable-page write barrier.

## Native computation admission

### EXEC-NATIVE-1 Verified bindings

**Rule**: `VerifiedBinding` MUST be constructed only by `adv_abi::native::verify`. Admission MUST bind complete code dependencies, the reference contract version and the verification rule version. Recognition and sample comparisons MUST NOT issue verified bindings.
**Verification**: Test

### EXEC-NATIVE-2 Forward byte recurrence proofs

**Rule**: Admission MUST check each decoded instruction, the byte recurrence, all register and flag effects, and the decreasing byte count's complete control flow. Unsupported instructions or recurrences MUST return Unsupported, and exhausted proof budgets MUST return Timeout. Counterexamples MUST remain distinct from both results.
**Verification**: Test

The single-byte rule checks a positive-count loop body with one conditional
backedge and one fall-through exit. Entry conditions exclude instruction
observers, including single stepping, and require nonwrapping,
ordinary readable source and writable destination ranges. Destination writes
exclude all code and constant dependencies. PNG Up requires
disjoint rows; PNG Sub and LZ77 require a positive backward distance and retain
ordered reads of newly written history. A zero counter declines at this body;
the enclosing guest function owns its zero-iteration path. State-view offsets
are part of the checked address expressions, independent of register allocation.
The count may be an explicit down-counter or a bound minus an advancing index.
Affine address/count relations are checked modulo 2^32; entry conditions rule
out wrapping buffer spans.
Average admission checks a widened predictor sum before shifting and byte
truncation. Its left prefix follows the same ordered recurrence as Sub; the
previous-row input remains disjoint from all output writes.

### EXEC-NATIVE-3 Typed state and shared integer semantics

**Rule**: Integer admission MUST use CPU-core operand decoding and arithmetic flag semantics. Expressions MUST retain their bit widths and memory versions. Every proved exit MUST identify its PC, registers, flags and memory version.
**Verification**: Test

### EXEC-NATIVE-4 Fixed private frame effects

**Rule**: Fixed-frame admission MUST account for every dword read and write. A non-faulting scalar tail MUST restore all temporary slot values. Condition flags read by the body MUST have local definitions.
**Verification**: Test

- Invariant slots match entry registers that remain unchanged across the backedge.
- Temporary slots have complete dword stores before reads of their current values; partial overlaps are unsupported.
- The frame base remains unchanged, and its bounded span is ordinary and disjoint from all remaining input and output spans.
- Writable frames exclude code dependencies and observed writes; a declined bridge has no guest effects.
- Conditional moves retain their false destination and read memory sources even when false.
- Preserved flags, including CF across INC/DEC, are not local definitions.

### EXEC-NATIVE-5 Bounded Paeth integer identities

**Rule**: Paeth admission MUST check signedness, widths, absolute-value identities and strict predictor comparisons. Normalization MUST stay within signed 32-bit arithmetic and a finite node budget.
**Verification**: Test

The straight-line rule uses three symbolic byte inputs. Exact normalized
identities establish the predictor; sample agreement does not admit code.
Unrecognized identities and internal branches remain unsupported.

Withdrawn: EXEC-CORE-2, EXEC-X64-1, EXEC-X64-2, EXEC-X64-3, EXEC-X64-4, EXEC-X64-5, EXEC-X64-6, EXEC-X64-7, EXEC-X64-8, EXEC-X64-9, EXEC-X64-10, EXEC-X64-11, EXEC-X64-12, EXEC-X64-13, EXEC-X64-14, EXEC-X64-15, EXEC-X64-16, EXEC-GIR-1, EXEC-GIR-2, EXEC-GIR-3, EXEC-GIR-4, EXEC-GIR-5, EXEC-GIR-6
