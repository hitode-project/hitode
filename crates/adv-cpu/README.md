# adv-cpu: the x86-32 execution base

The runtime executes guest code on AArch64 hosts with the Tier 0 predecoding interpreter and the a64 translator. They share `CpuState` and guest memory. Decoding uses iced-x86, and floating point uses Berkeley SoftFloat 3e (BSD-3-Clause). Unicorn is a dev-dependency used only as a differential reference; production `adv-run` never links it.

The rules are in [execution.md](../../docs/spec/execution.md) and [a64.md](../../docs/spec/a64.md). This page describes the design behind them.

## Crates

- `adv-cpu-core` holds the engine-neutral state, memory, interpreter, floating point and the `Backend` and `Engine` interfaces.
- `adv-cpu-a64` holds the AArch64 translator.
- `adv-cpu` re-exports the AArch64 engine and holds the integration tests and examples.

## State and execution

- `CpuState` holds the GPRs, EFLAGS, EIP, FS base, segment selectors and GDTR, the raw 80-bit x87 registers with tag, TOP, control and status, MXCSR, XMM and the deferred MMX alias writes. `ExecutionContext` copies this state plus restartable executor progress. It holds no memory, devices or host continuations.
- The fuel budget counts work units: a native basic block, an interpreted instruction or a completed REP element. It is not virtual time, and Tier 0 units and native blocks cannot be added together.
- M:1 scheduling, waits and the clock belong to the machine layer. The CPU provides progress, exceptions, budget and park boundaries.
- `HostFeatures` describes what the host CPU offers. It is separate from the guest CPUID configuration, which is a host-independent virtual profile that the API feature queries also read.

## Tier 0

- The interpreter caches predecoded operations with their code page versions. It runs cold code, single steps and forms the engines do not support. A form the interpreter itself lacks returns `UnsupportedInstruction` with its PC and decode kind.
- Instructions validate their accesses, stage their changes and then commit, so a fault keeps the completed prefix and a restartable current instruction.
- A predecode window caches up to 64 instructions or 512 bytes. Direct and indirect branches, short loops and local calls and returns run inside it by byte offset, with at most 256 units per call. Decoding passes a return or back edge only when its local forward target is known. Transfers out of the window, REP, native entries and owner operations exit; shared code never loops inside a window.
- The first fetch on an ordinary code page reads up to 15 bytes at once, keeping cross-page and missing-page dependencies and the exact failure position. Shared code is still fetched byte by byte.
- Classified register, single-page integer memory and stack operations validate first and then commit canonical state directly. Complex, shared and cross-page forms commit through a per-instruction transaction. Every instruction keeps the budget, TF, stop and code-write exits.
- Common SSE and MMX moves and packed integer operations cache operands and use fixed-width functions. MMX and x87 aliases stay consistent at commits and window exits. Other floating-point forms run as transactions.
- `run_chunk` commits at most 256 REP elements at a time. ECX, ESI and EDI record partial progress, and CMPS and SCAS keep the original EFLAGS for a fault. Within one ordinary page, REP MOVS and STOS copy or fill in batches while still charging each element; overlap, aliases, shared pages, code writes, cross-page elements and TF fall back to per-element transactions.
- TF single steps run in the interpreter. Divide errors, quotient overflow, UD2 and invalid encodings keep the fault PC; software INT commits the next PC.
- x87 arithmetic keeps the 80-bit representation, PC and RC, exception flags and stack semantics. SSE honors MXCSR rounding, DAZ, FTZ and exceptions. Unmasked exceptions never commit the destination early, and a pending #MF forces interpretation. FXSAVE and FXRSTOR in 32-bit form touch only the first 288 bytes.
- Basic floating point uses SoftFloat. Transcendental functions compute in binary128 and round to extended80 by the guest RC; small arguments match native x87, and large-argument trigonometry is not promised bit-exact on every CPU. FPREM and FPREM1 compute exact remainders by integer division, with C2 partial reduction and quotient bits.
- Coverage includes common 8, 16 and 32-bit integer forms, conditional execution, stack and flags, near calls, CRT bit and string operations, user-mode LOCK read-modify-write, common MMX, SSE and SSE2, and x87. Under M:1 a read-modify-write is atomic; this is not a promise of parallel guest CPUs. Arbitrary segmentation, privileged state and far transfers are not supported.

## AArch64 translation

The a64 engine executes first visits and translates hot traces directly to AArch64.
Guest registers live in fixed host registers, and guest memory uses a guarded
window with recovery side tables. Its design is described in
[`adv-cpu-a64`](../adv-cpu-a64/README.md).

```sh
cargo test --release -p adv-cpu
cargo run --release -p adv-cpu --example engine_bench
```
