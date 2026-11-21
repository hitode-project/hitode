# a64 engine architecture

ID prefix: A64A

Instruction-level contracts are in [a64.md](a64.md). The shared execution contract is in [execution.md](execution.md).

## Dependencies

### A64A-DEP-1 Module dependencies

**Rule**: A module of `adv-cpu-a64` MUST use only the modules its row lists. Code outside tests MUST NOT import a parent module with a glob. Edges in the checker's transitional list MAY remain, and the list MUST only shrink.
**Applies to**: AArch64
**Verification**: Test — cases for `tools/layering/check_modules.py`

| Module | May use |
|---|---|
| `encode`, `cache`, `jit_map`, `policy` | none |
| `asm` | `encode` |
| `runtime` | `asm`, `encode` |
| `facts` | `runtime`, `asm`, `encode` |
| `translate::front` | `encode` operation types |
| `translate::lower` | `translate::front`, `facts`, `runtime`, `asm`, `encode` |
| `translate::jit`, `translate::opt` | `translate::lower`, `translate::front`, `facts`, `runtime`, `asm`, `encode` |
| `native::store` | `cache`, `encode`, `facts`, `runtime`, `translate::front`, `translate::lower` |
| `native::compile` | `translate::jit`, `translate::opt`, `translate::front`, `translate::lower`, `facts` |
| `native::plan` | `native::store`, `native::compile`, `facts` |
| `native::offload` | `native::store`, `native::plan`, `runtime`, `translate::opt` |
| `native::run`, `executor` | every row above |
| `cpu` | `executor` |

Every module MAY use `policy` and MAY use `jit_map` behind its enable switch. Only `translate::front` MAY use `iced_x86`. Only `executor` and `native::offload` MAY use `adv_cpu_core::interpreter`.

## Modules

### A64A-ROLE-2 Front end

**Rule**: The translation front end MUST be the only module in `adv-cpu-a64` that decodes guest bytes. It MUST use reusable scratch snapshots for writable entry pages and Tier-1-only machines, and reuse valid instruction snapshots for other builds. The front end MAY name encoder operation types but MUST NOT call encoders or emit host code.
**Applies to**: AArch64
**Verification**: Test

A unit holds decoded operations, control flow, inlined frames, switch tables, addressing streams, access summaries and watched code spans. Tier-1 builds use reusable trace storage. The store keeps the immutable trace only for a nonempty Tier-1 key or a live resident form or job. Promotion and resident discovery MAY extend a trace with previously undecoded instructions. First visits execute in the core interpreter under A64-TR-2.

The page index records one dependency per site and page across all live shapes and placements. Dropping a site removes its entries from every page it depended on. Clearing generated code clears the placement index. Guest memory keeps watching decoded bytes so that writes still invalidate retained snapshots.

## Tier-1 modules

### A64A-ROLE-3 Facts

**Rule**: `facts` MUST define each generated entry assumption. Each fact kind MUST have one reader, one guard emitter and one encoding. A key MUST be a fixed-width mask and value pair.
**Applies to**: AArch64
**Verification**: Test

| Fact | Read from |
|---|---|
| x87 control word | `CpuState` x87 control |
| TOP and tags | x87 status and the tags of slots the trace reads |
| MMX mode and deferred EMMS | `CpuState` MMX mode |
| x87 status, including pending exceptions | `CpuState` x87 status |
| MXCSR class and sticky flags | `CpuState` MXCSR |
| ESP alignment and EBP relative to ESP | `CpuState` general registers |
| Exact x87 binary64 inputs | `CpuState` x87 registers |

The build key projects live facts onto the unit's trace mask. An empty mask assumes nothing. A lowering reads unassumed fields from `CpuState`.

Range guards over page facts use the Memory permission tables and stay outside the key. An invariant indirect target guard checks the slot’s readable range and value outside the key. The code store watches the prepared target’s code dependencies.

### A64A-ROLE-4 Lowerings

**Rule**: `translate::jit` and `translate::opt` MUST lower under an explicit entry fact key and build configuration. Both lowerings MUST avoid reading live guest state or guest memory. Tier-1 lowering MUST accept any fact kind unassumed.
**Applies to**: AArch64
**Verification**: Test

### A64A-ROLE-5 Code store

**Rule**: `native::store` MUST key owner code by entry EIP and fact key. It MUST be the only module that places, links, versions, watches and drops code. Resident forms and Tier-1 versions at an entry MUST share one chain.
**Applies to**: AArch64
**Verification**: Test
**Basis**: A64-OPT-11

The chain has a resident prefix and a Tier-1 suffix. Dispatch, indirect slots, and same-head, countdown and REP continuations enter the checked Tier-1 suffix. Eligible backward links and outside direct arrivals enter the checked resident prefix. An empty key has no guards.

A failed key guard continues through its miss slot to the next checked version or the version-miss stub. A static edge that proves the key enters past those guards. A return continuation checks the facts its callee may change. Invalidation drops both segments.

### A64A-ROLE-6 Arbiter

**Rule**: `native::plan` MUST decide tiers, version selection, build configuration and offload in response to plan events. It MUST NOT emit host code or read guest memory.
**Applies to**: AArch64
**Verification**: Test
**Basis**: A64-OPT-12

Plan events include discovery, version misses, sensor triggers, compile completion, cold exits, faults, invalidation and probation end.

### A64A-ROLE-8 Plan events

**Rule**: Generated code MUST deliver each plan event through an in-place stub call or an exit payload. `Runtime` MUST NOT hold event fields that the owner polls after each run. Generated diagnostic counts MUST be written into diagnostic storage only when enabled.
**Applies to**: AArch64
**Verification**: Test

Events that may invalidate an active continuation MUST leave with their identity and recovery data. The owner MUST resolve recovery before changing the cache. Observations and stable link patches MAY continue through the stub. A deferred observation MUST retain the identity and generation of the source unit.

### A64A-ROLE-9 Evidence gates

**Rule**: Translation policy MUST assign each optimization to the evidence gate it needs. Static code prices MUST use AArch64 instruction words. Worker pacing MUST compare elapsed execution and wake costs in the same time unit.
**Applies to**: AArch64
**Verification**: Review — generated-code cost estimates and evidence ownership are checked at the decision sites.
**Basis**: A64-OPT-12

| Gate | Evidence | Permitted work |
|---|---|---|
| Tier-1 | Repeated arrival and decoded path | Bounded local lowering, entry-key versions and adaptive access checks |
| Loop trace | Budget-exit samples | Wider traces and loop-carried local state |
| Resident | Back-edge samples and validated recovery | Register and frame residency, reuse guarded by entry facts, checked rebuilds and cold cuts |
| Proved regions | Trip, span, alias and exact-tail proofs | Batch kernels and row-domain specialization |
| Worker | Computation proof, current range grants and scheduling demand | Detached jobs and independent slices |

The gates describe the evidence for a transform. A proved region can be local to Tier-1, resident, or a worker job under the corresponding memory contract. A detached job can be admitted independently of owner residency. Native bodies use their signature and code identity under A64-CALL-1 through A64-CALL-8.

The `policy` module owns static thresholds and budgets. `native::plan` owns feedback, break-even decisions and worker pacing. Lowering describes emitted costs and links the threshold operands. The code store binds code and retains counts for the owner.

## Leaving a Tier-1 fast path

### A64A-FALL-2 No interpreter for state

**Rule**: A key guard failure MUST continue in a version whose key holds. A Tier-1 version MUST NOT call the interpreter because the entry facts differ.
**Applies to**: AArch64
**Verification**: Test

### A64A-FALL-3 Version misses

**Rule**: A guard miss MUST look up or build the matching version on the owner without leaving generated code. A full Tier-1 suffix MUST replace its tail with a version keyed by the join of the tail and the observed facts.
**Applies to**: AArch64
**Verification**: Test

The Tier-1 suffix keeps up to three versions, from most specific to most general. A join keeps each complete field on which the keys agree. The resident prefix keeps up to three forms in discovery order. Its last miss proceeds through the checked Tier-1 suffix.

### A64A-FALL-4 Events

**Rule**: Generated events MUST name a cause in the table. Only the events marked for interpreter execution MAY run through the interpreter.
**Applies to**: AArch64
**Verification**: Test

| Event | Owner action |
|---|---|
| A host fault or a precise checked-access refusal | Replay one instruction with the recovered state (A64-MEM-5) |
| A fetch failure or invalid encoding | Resolve the outcome through the interpreter at the original EIP |
| TF or RF in EFLAGS | Execute one instruction through the interpreter |
| A waiting x87 instruction with a pending unmasked exception | Deliver #MF through the interpreter at the faulting instruction |
| A misaligned aligned access or invalid LDMXCSR value (#GP), division (#DE), UD2 (#UD), or an unmasked SSE exception (#XM) | Deliver the architectural exception with any committed state |
| A software interrupt, including INT 0x2e without a host handler | Complete the owner trap action |
| SGDT, SIDT, SLDT, CLI, STI, and segment register pushes and loads | Execute through the interpreter using the core guest system profile |
| A core semantic gap carrying the decoded form and EIP | Report the terminal unsupported-instruction outcome. This includes SMSW, STR, DAA, DAS, AAA, AAS, AAM and AAD, and unsupported operands. |

### A64A-FALL-5 Value calls

**Rule**: Value calls MUST evaluate prepared core semantics on operands supplied by generated code. They MUST retain unaffected cache entries and publish and reload only the architectural sub-state the operation uses. A representable result MUST continue in the same unit.
**Applies to**: AArch64
**Verification**: Test

An exception raised by a value call MUST deliver the committed state through an event without replay. Generated access paths transfer memory operands before core evaluation and commit store results before publishing their architectural effects. A result outside the x87 run's model, including incompatible TOP or tags, dispatches with the committed state. Opt-in JIT diagnostics count value calls by site.

## Exits

### A64A-EXIT-1 Exit kinds

**Rule**: Generated code MUST leave with a kind in the table. Each kind MUST have one handler.
**Applies to**: AArch64
**Verification**: Test

| Kind | Handler |
|---|---|
| Dispatch | Continue at the guest EIP through the code store or select a first visit |
| Event | Complete the named cause in the execution owner (A64A-FALL-4) |
| Budget | Account for retirement and dispatch, preserving any REP continuation |
| Yield and Reschedule | Apply the execution owner's scheduling action |
| Computation | Offer the job through `native::offload` |
| Optimized | Materialize the recovery record at the native boundary |
| Recall | Return a worker's exact prefix to the execution owner |
| Plan | Apply the plan action after leaving generated code |

Dispatch, version misses, value calls and host calls use stubs that continue in generated code when possible.

## Tier-1 placement

### A64A-PLACE-2 Guard cost

**Rule**: An entry guard MUST cost a bounded number of loads and compares. A static edge that proves the target key MUST enter its body. An empty-key unit MUST emit its original instruction sequence when diagnostics are off.
**Applies to**: AArch64
**Verification**: Test

### A64A-PLACE-3 Policy sensors

**Rule**: An active sensor MUST use at most five words and preserve NZCV. A settled sensor MUST execute one unconditional branch past the countdown. Profitability and worth decisions MUST run on plan events or at owner safe points.
**Applies to**: AArch64
**Verification**: Test

The countdown uses a literal address or a fixed runtime slot, followed by a load, SUB, store, and a compare-and-branch to cold code. A sensor MUST NOT use a reserved register or LSE. Dynamic offers MUST recur at an owner-rearmed sampling period. A point that reads a single owner-published decision MAY use a load, test and branch when that path costs no more than a countdown.

### A64A-PLACE-4 Build location

**Rule**: A Tier-1 version miss MAY build on the owner. A fact-version rebuild MUST reuse the site's unit without reading or decoding guest code.
**Applies to**: AArch64
**Verification**: Test

Closing an execution owner MUST discard queued compilation and wait for any
compilation already in flight to finish.
