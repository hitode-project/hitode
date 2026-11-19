# a64 engine architecture

ID prefix: A64A

Instruction-level contracts are in [a64.md](a64.md). The shared execution contract is in [execution.md](execution.md).

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
