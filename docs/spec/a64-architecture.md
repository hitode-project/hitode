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
| `encode`, `asm`, `cache`, `jit_map` | none |
| `runtime` | `asm`, `encode` |
| `facts` | `runtime`, `asm`, `encode` |
| `translate::front` | none |
| `translate::lower` | `translate::front`, `facts`, `runtime`, `asm`, `encode` |
| `translate::jit`, `translate::opt` | `translate::lower`, `translate::front`, `facts`, `runtime`, `asm`, `encode` |
| `native::store` | `cache`, `facts`, `runtime` |
| `native::compile` | `translate::jit`, `translate::opt`, `translate::front`, `facts` |
| `native::plan` | `native::store`, `native::compile`, `facts` |
| `native::offload` | `native::store`, `native::plan`, `runtime` |
| `native::run`, `executor` | every row above |
| `cpu` | `executor` |

Every module MAY use `jit_map` behind its enable switch. Only `translate::front` MAY use `iced_x86`. Only `executor` and `native::offload` MAY use `adv_cpu_core::interpreter`.

## Modules

### A64A-ROLE-2 Front end

**Rule**: The translation front end MUST be the only module in `adv-cpu-a64` that decodes guest bytes. It MUST decode each guest instruction at most once until its code is invalidated. It MUST NOT emit host code.
**Applies to**: AArch64
**Verification**: Test

A site's immutable unit holds decoded operations, control flow, inlined frames, switch tables, addressing streams, access summaries and watched code spans. The first build prepares the cold shape. Promotion or resident discovery MAY extend the unit with previously undecoded instructions. First visits execute in the core interpreter under A64-TR-2.

## Tier-1 modules

### A64A-ROLE-3 Facts

**Rule**: `facts` MUST define each Tier-1 entry assumption. Each fact kind MUST have one reader, one guard emitter and one encoding. A key MUST be a fixed-width mask and value pair.
**Applies to**: AArch64
**Verification**: Test

| Fact | Read from |
|---|---|
| x87 control word | `CpuState` x87 control |
| TOP and tags | x87 status and the tags of slots the trace reads |
| MMX mode and deferred EMMS | `CpuState` MMX mode |
| Pending x87 exception | x87 status |
| MXCSR class | `CpuState` MXCSR control bits |

The build key projects live facts onto the unit's trace mask. An empty mask assumes nothing. A lowering reads unassumed fields from `CpuState`.

### A64A-ROLE-4 Lowerings

**Rule**: Tier-1 lowering MUST receive the fact key as an explicit build input. The lowering MUST NOT read live guest state or guest memory. It MUST accept any fact kind unassumed.
**Applies to**: AArch64
**Verification**: Test

### A64A-ROLE-5 Code store

**Rule**: `native::store` MUST key Tier-1 versions by entry EIP and fact key. Dispatch, indirect transfers and resident continuations MUST enter the chain's checked head. Invalidation MUST drop all versions of the entry.
**Applies to**: AArch64
**Verification**: Test
**Basis**: A64-OPT-11

Each version has guards, a body and a miss slot. The slot branches to the next checked entry or to the version-miss stub. A return continuation checks facts its callee may change.

## Leaving a Tier-1 fast path

### A64A-FALL-2 No interpreter for state

**Rule**: A key guard failure MUST continue in a version whose key holds. A Tier-1 version MUST NOT call the interpreter because the entry facts differ.
**Applies to**: AArch64
**Verification**: Test

### A64A-FALL-3 Version misses

**Rule**: A guard miss MUST look up or build the matching version on the owner without leaving generated code. A full chain MUST replace its tail with a version keyed by the join of the tail and the observed facts.
**Applies to**: AArch64
**Verification**: Test

The chain keeps up to three versions, from most specific to most general. A join keeps each complete field on which the keys agree.

### A64A-FALL-4 Events

**Rule**: An x87 instruction raising pending #MF MUST deliver it through the interpreter at the faulting instruction. Generated exceptions MUST carry the committed state to the owner.
**Applies to**: AArch64
**Verification**: Test

| Event |
|---|
| An x87 instruction that raises #MF from a pending unmasked exception |
| A host fault or a precise checked-access refusal |
| A misaligned aligned access or an invalid LDMXCSR value (#GP) |
| A division that raises #DE |
| An unmasked SSE exception with the committed MXCSR status (#XM) |
| INT 0x2e without a host handler |
| A core semantic gap carrying the decoded operation and EIP. This terminal event returns the unsupported-instruction outcome. |

## Tier-1 placement

### A64A-PLACE-2 Guard cost

**Rule**: An entry guard MUST cost a bounded number of loads and compares. A static edge that proves the target key MUST enter its body. An empty-key unit MUST emit its original instruction sequence when diagnostics are off.
**Applies to**: AArch64
**Verification**: Test

### A64A-PLACE-4 Build location

**Rule**: A Tier-1 version miss MAY build on the owner. A rebuild MUST reuse the site's unit without reading or decoding guest code.
**Applies to**: AArch64
**Verification**: Test
