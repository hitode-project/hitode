# a64 floating point

ID prefix: A64F

This spec covers how `adv-cpu-a64` runs x87, MMX and SSE instructions. The rest of the engine is in [a64.md](a64.md), and [README.md](README.md) explains the format.

## Floating point
### A64F-FP-1 x87 write-through

**Rule**: Outside runs under A64F-FP-7, the x87 stack, TOP, tags, status word and control word MUST live only in `CpuState`. A natively executed x87 instruction MUST write back only after every check passes.
**Applies to**: AArch64
**Verification**: Test

### A64F-FP-2 Native x87 forms and fallback

**Rule**: Only forms that `fast::classify` accepts MAY run natively. Pending exceptions and MMX state MUST call the interpreter helper in place. Except for FXAM classification, empty or full slots, nonzero denormals and unsupported range/precision MUST call the interpreter helper in place.
**Applies to**: AArch64
**Verification**: Test

### A64F-FP-3 x87 arithmetic

**Rule**: Native arithmetic MAY accept zeros and normals with normal results, under any rounding control and precision 24, 53 or 64. Results MUST round once from the exact value without narrowing the exponent range, and inexact ones MUST require PE masked. PE and C1 MUST follow that rounding, and compares MUST order equal doubles that lost bits by their exact significands.
**Applies to**: AArch64
**Verification**: Test

### A64F-FP-4 MMX

**Rule**: Except under A64-TR-5, MMX writes MUST publish at exits, control edges, helpers, leaf calls, checked accesses, x87 instructions and cache drops. Boundaries and faults MUST publish dirty MMX and x87 aliases (exponent 0xffff), using site masks and signal contexts. The first MMX instruction of a unit MUST check pending x87 exceptions, then clear TOP and tags.
**Applies to**: AArch64
**Verification**: Test

### A64F-FP-5 SSE

**Rule**: Native SSE floating point MAY run only under default MXCSR, MUST NOT write FPCR, and MAY access FPSR only in its slow path. The fast path MUST have PE set or an exact operation, no denormal operand, and values excluding other exceptions, or take the slow path. Exceptions other than denormal operands and inexact, and unaligned `movaps`-class accesses, MUST leave for the interpreter before committing guest state.
**Applies to**: AArch64
**Verification**: Test

### A64F-FP-6 SSE compares

**Rule**: A compare MUST select its predicate from the low three bits of the immediate, with QNaN exception behavior following the predicate. SNaN operands MUST go to the interpreter. A scalar compare MUST read only the low lane and keep the upper bits of the destination.
**Applies to**: AArch64
**Verification**: Test

### A64F-FP-7 x87 runs

**Rule**: A run MAY span x87 instructions, integer instructions that neither observe x87 state nor fault, and forward edges to merges reached only from the run. It MAY keep stack values in v24–v31 as exact binary64 pairs, renaming copies, with arithmetic specialized to the translation-time control word behind a guard. It MUST check relied-on tags at entry and fetched values, and MUST publish at exits, helpers and faults as before the faulting instruction.
**Applies to**: AArch64
**Verification**: Test

Withdrawn: none
