# Execution

ID prefix: EXEC

This spec covers the contract that Tier 0 and both platform engines share, and the interfaces of x64 and GIR. The a64 engine has its own spec, [a64.md](a64.md); [README.md](README.md) explains the format. Source code and layout tests define layouts, so offsets are not repeated here. The module docs of `adv-cpu-core::execution_abi` and `adv-cpu-x64::compile` explain the design.

## Shared

### EXEC-CORE-1 Authoritative state

**Rule**: `CpuState` is the authoritative guest state that Tier 0, Tier 2 and a64 share. `ExecutionContext` MAY hold restartable REP progress. `ExecutionContext` MUST NOT hold a live host continuation that belongs to another owner.
**Verification**: Review — no single test can observe the ownership constraint.

### EXEC-CORE-2 Engine selection by target

**Rule**: The `adv-cpu` facade MUST select the engine by target architecture: `adv-cpu-x64` on x86_64 and `adv-cpu-a64` on AArch64. Engine selection MUST NOT be a runtime switch or a feature.
**Verification**: Check — `adv-cpu` compiles conditionally on `target_arch`, and `tools/layering/check_layers.py` checks the dependencies.

### EXEC-CORE-3 BSWAP with an operand-size prefix

**Rule**: `BSWAP` with an operand-size prefix MUST clear the low 16 bits of the destination and keep the high 16 bits and EFLAGS. The 32-bit form MUST reverse all four bytes. Every engine MUST match the interpreter and MUST NOT emit an undefined host form.
**Verification**: Test
**Basis**: [Intel SDM Volume 2, BSWAP](https://cdrdv2-public.intel.com/774492/325383-sdm-vol-2abcd.pdf) leaves the 16-bit result undefined.

> Note: this is a compatibility choice of the guest CPU, not a claim about every physical processor.

### EXEC-CORE-4 Audio call marks

**Rule**: Thunks MUST classify audio bindings at registration, and dispatch MUST record their latest call using the guest wait-deadline clock. Thread queries MUST report whether that timestamp is strictly after the supplied time, without affecting scheduling. Audio bindings MUST reject both Leaf and EnvLeaf registration.
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

Exact retirement is enabled by `--profile` or `--frame-budget`. The synthetic `tier0_baseline` example exercises integer loops, REP strings, x87, MMX mixed with integer operations and indirect calls, asserting completed-instruction counts.

### EXEC-CORE-6 Tier 0 decode cache validity

**Rule**: Tier 0 MAY skip private page validation only when the block's last validated owner generation matches. Page-version, mapping and permission changes MUST advance that generation, and generations MUST distinguish memory owners. Changed generations and SHARED code MUST trigger validation on each entry, retaining decoded blocks only when permissions, mappings and bytes match.
**Verification**: Test

Cached successors MUST match entry EIP and block-cache generation. They MUST pass the same memory and native-publication revision checks as a lookup. Clearing either decode cache MUST advance the cache generation before any entry is reused. Successor hits MUST preserve entry stops, budgets and observation boundaries.

### EXEC-CORE-7 Observed data write barriers

**Rule**: `Memory::direct` MUST refuse writes intersecting observed bytes, including on non-executable pages. Checked stores intersecting observed bytes MUST advance the corresponding dependency versions. On non-executable pages, stores outside observed bytes MUST NOT invalidate dependencies.
**Verification**: Test

## Native computation admission

### EXEC-NATIVE-1 Verified bindings

**Rule**: `VerifiedBinding` MUST be constructed only by `adv_abi::native::verify`. Admission MUST bind complete code dependencies, the reference contract version and the verification rule version. Recognition and sample comparisons MUST NOT issue verified bindings.
**Verification**: Test

### EXEC-NATIVE-2 Forward byte recurrence proofs

**Rule**: Admission MUST check each decoded instruction, the byte recurrence, all register and flag effects, and the decreasing counter's complete control flow. Unsupported instructions or recurrences MUST return Unsupported, and exhausted proof budgets MUST return Timeout. Counterexamples MUST remain distinct from both results.
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

### EXEC-NATIVE-3 Typed state and shared integer semantics

**Rule**: Integer admission MUST use CPU-core operand decoding and arithmetic flag semantics. Expressions MUST retain their bit widths and memory versions. Every proved exit MUST identify its PC, registers, flags and memory version.
**Verification**: Test

## x64

### EXEC-X64-1 Frozen engine

**Rule**: Changes to x64 MUST fix correctness. Changes made only for performance MUST NOT be accepted.
**Applies to**: x86_64
**Verification**: Review — this limits which changes are accepted.

### EXEC-X64-2 Calling contract

**Rule**: Tier 2 GIR segments and opaque operations MUST share `NativeContext`, lazy flags, memory and exits. Regions, loop kernels and row trampolines MUST use the context and memory pointer pair of `compile::contract`. Tier 2 MUST NOT pin guest registers to host registers.
**Applies to**: x86_64
**Verification**: Review — only `compile::contract` implements the contract, and no separate test covers it.

### EXEC-X64-3 State published at region boundaries

**Rule**: Region boundaries MUST publish `CpuState`. A pure region MAY save only the state in its write set.
**Applies to**: x86_64
**Verification**: Test

### EXEC-X64-4 PollWord

**Rule**: `PollWord` is an owner-pinned `AtomicBool` that writers set with Release ordering and native code reads atomically. Only its owner MUST consume it, and it MUST NOT serve as a bitmap of causes or as a revocation acknowledgement. Polling MUST NOT replace fuel or change exception priority.
**Applies to**: x86_64
**Verification**: Review — a single-threaded test cannot refute a memory-ordering constraint.

### EXEC-X64-5 Poll sites

**Rule**: The C entry MUST check the first entry. Internal back edges, indirect jumps and region links MUST keep their polls. Per-block fuel, stops, zero budgets and exception priority MUST stay consistent.
**Applies to**: x86_64
**Verification**: Review — `compile::contract` emits every poll site, so testing each site is impractical.

### EXEC-X64-6 Host context snapshot

**Rule**: `HostContextSnapshot` MUST hold the host integer, SIMD, SP, PC, flags and FP control state in a fixed size. Host flags MUST NOT be used as guest EFLAGS directly. Extended state that the platform does not support MUST be refused.
**Applies to**: x86_64
**Verification**: Test

### EXEC-X64-7 Recovery state sources

**Rule**: `NativeRecoveryPoint` MUST bind the fault site, the same-frame recovery point, current and next guest PC, direction, width, commit proof and state sources to real code. A state source MUST be a captured host value, a canonical checkpoint or a constant. Recovery MUST NOT reread mutable guest memory.
**Applies to**: x86_64
**Verification**: Test

### EXEC-X64-8 Fuel accounting

**Rule**: `committed_units` MUST describe only the committed prefix. The caller MUST materialize fuel before entering a segment, and recovery MUST NOT charge it twice.
**Applies to**: x86_64
**Verification**: Review — the structure of the callers enforces it.

### EXEC-X64-9 Forms that access memory directly

**Rule**: A scalar load MUST NOT commit its destination before recovery. A store MAY access memory directly only when it is proven aligned, on one host page, privately and stably backed, and committed all or nothing. Cross-page, shared, vector, atomic, read-modify-write and unproven forms MUST take the checked path.
**Applies to**: x86_64
**Verification**: Test

### EXEC-X64-10 Fault handler limits

**Rule**: The handler MAY claim only synchronous, registered faults. After capturing state, it MUST redirect to the same-frame recovery point.
**Applies to**: x86_64
**Verification**: Test

> Caution: the handler does not allocate, take ordinary locks, enter the scheduler or call checked alias accesses.

### EXEC-X64-11 Recovery and commit

**Rule**: After returning to an ordinary context, recovery MUST restore the prefix at once and complete at most one checked access. Only a successful access MUST commit the destination and the next PC, and a real violation MUST keep the current PC. A legitimate mixed page MUST complete through a stable alias and MUST NOT widen to a whole-host-page retry.
**Applies to**: x86_64
**Verification**: Test

### EXEC-X64-12 Return to the previous handler chain

**Rule**: A write to code MUST return an invalidation result. Faults at unknown PCs, outside the window or at addresses of unknown origin MUST return to the previous handler chain. So MUST asynchronous signals, reentrant faults and faults without provable commits.
**Applies to**: x86_64
**Verification**: Test

### EXEC-X64-13 Descriptor lifetime

**Rule**: Side tables, code, recovery points, mappings and aliases MUST live at least until the current native lease returns. Publication and retirement MUST NOT let the handler see a stale descriptor.
**Applies to**: x86_64
**Verification**: Test

### EXEC-X64-14 Windows guarded reads

**Rule**: A guarded read MUST cover only the first provable scalar read, and other accesses MUST be checked. The VEH MUST recover through the private result slot that the IR publishes, and MUST NOT derive RSP or parse instruction bytes. Windows unwind prologue validation MUST stay.
**Applies to**: Windows x86_64
**Verification**: Test

### EXEC-X64-15 Persistent code cache identity

**Rule**: Loading the persistent native cache MUST check AOT relocations and build identity. A change of source or compiler identity MUST invalidate old artifacts. Windows guarded-read recipes MUST NOT enter the persistent cache.
**Applies to**: x86_64
**Verification**: Test

### EXEC-X64-16 Code publication

**Rule**: On Linux, native code MUST flush the instruction cache after writing, then switch to RX and publish. Windows MUST keep its own VEH and unwind lifetime. POSIX recovery MUST keep the previous SIGSEGV and SIGBUS handler chain.
**Applies to**: x86_64
**Verification**: Review — the publication order depends on operating system behavior.

## GIR

### EXEC-GIR-1 Single Tier 2 front end

**Rule**: GIR MUST be the only Tier 2 front end. A front-end selector or a comparison front end MUST NOT be added.
**Applies to**: x86_64
**Verification**: Review — this constrains the structure.

### EXEC-GIR-2 Production SSA scope

**Rule**: `gir/lift::tier2` MUST form contiguous integer SSA segments only within the admitted decode set. Entries and merge points with several predecessors MUST split segments. SSA segments MUST NOT enlarge regions or call inlining.
**Applies to**: x86_64
**Verification**: Test

Admitted set: NOP; MOV, MOVZX, MOVSX and LEA with a 32-bit destination; ADD, SUB, CMP, INC, DEC, NEG and NOT. Supported forms may read narrow sources.

### EXEC-GIR-3 Opaque operations

**Rule**: Forms outside the admitted set MUST stay opaque operations that reuse the existing emitters. Logical operations MUST NOT enter production SSA while their undefined AF differs from the interpreter.
**Applies to**: x86_64
**Verification**: Test

### EXEC-GIR-4 State between segments

**Rule**: `compile/gir` MUST borrow the state variables of the Emitter, and Commit MUST update the same variables. Segments MUST NOT add a save and reload of the whole state between them.
**Applies to**: x86_64
**Verification**: Test

### EXEC-GIR-5 Shared planning

**Rule**: A compile Snapshot MUST fix the GIR plan. The production graph MUST plan decoding, reachable targets, overlapping entries and page dependencies together. Integer segments MUST NOT rediscover call context.
**Applies to**: x86_64
**Verification**: Test

### EXEC-GIR-6 Standalone IR boundary

**Rule**: `ir`, `lift`, `cfg`, `state_ssa`, `verify`, `eval` and `recovery` MUST use region-local indices and MUST NOT store Cranelift values or host pointers. `eval` MUST serve only for differential tests against the interpreter, not as an executor. `prototype` MUST compile only in test configurations.
**Applies to**: x86_64
**Verification**: Review — module boundaries and `cfg(test)` enforce it.

> Note: the standalone IR covers more than production SSA, so standalone coverage does not prove production coverage.

Withdrawn: none
