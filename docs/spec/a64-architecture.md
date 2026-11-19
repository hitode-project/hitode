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

## Placement

### A64A-PLACE-4 Build location

**Rule**: A rebuild MUST reuse the site's unit without reading or decoding guest code.
**Applies to**: AArch64
**Verification**: Test
