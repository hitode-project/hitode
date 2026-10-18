# adv-cpu: the x86-32 execution base

The runtime executes guest code with the Tier 0 predecoding interpreter and one platform engine: the Tier 2 Cranelift region JIT on x86_64, or the a64 translator on AArch64. They share `CpuState` and guest memory. Decoding uses iced-x86, and floating point uses Berkeley SoftFloat 3e (BSD-3-Clause). Unicorn is a dev-dependency used only as a differential reference; production `adv-run` never links it.

The rules are in [execution.md](../../docs/spec/execution.md) and [a64.md](../../docs/spec/a64.md). This page describes the design behind them.

## Crates

- `adv-cpu-core` holds the engine-neutral state, memory, interpreter, floating point and the `Backend` and `Engine` interfaces.
- `adv-cpu-x64` holds the Cranelift engine: GIR, compilation, the executor, row kernels, linking and the code cache. It builds only on x86_64.
- `adv-cpu-a64` holds the AArch64 translator.
- `adv-cpu` is the facade. It picks the engine by target architecture and re-exports it, and it holds the integration tests and examples. Engine choice is a platform fork, not a runtime switch or feature; x64-specific execution assertions are skipped on AArch64.

The x64 engine is frozen and receives only correctness fixes. The rest of this page describes Tier 0 and x64 unless it names a64.

## State and execution

- `CpuState` holds the GPRs, EFLAGS, EIP, FS base, segment selectors and GDTR, the raw 80-bit x87 registers with tag, TOP, control and status, MXCSR, XMM and the deferred MMX alias writes. `ExecutionContext` copies this state plus restartable executor progress. It holds no memory, devices or host continuations. `NativeContext` is the Tier 2 calling context.
- The fuel budget counts work units: a native basic block, an interpreted instruction or a completed REP element. It is not virtual time, and Tier 0 units and Tier 2 blocks cannot be added together.
- M:1 scheduling, waits and the clock belong to the machine layer. The CPU provides progress, exceptions, budget and park boundaries.
- `HostFeatures` describes what the host CPU offers. It is separate from the guest CPUID configuration, which is a host-independent virtual profile that the API feature queries also read.

## Tier 0

- The interpreter caches predecoded operations with their code page versions. It runs cold code, code waiting for compilation, single steps and forms the engines do not support. A form the interpreter itself lacks returns `UnsupportedInstruction` with its PC and decode kind.
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

## Tier 2 on x86_64

- GIR plans every region. Runs of supported integer instructions become SSA segments; other instructions and the specialized loop kernels become opaque operations that reuse the existing emitter. Both share `NativeContext`, lazy flags, memory access, linking and exception exits, and compile once in the background.
- Integer flags keep a lazy recipe, with CF held as its own SSA bit, so unused carry comparisons disappear. Budget, interrupt, fault and cross-region exits publish full EFLAGS. Region exits store the recipe in `CpuState::lazy_flags`, and `settle_flags` computes EFLAGS once on return and at helper entry.
- Automatic regions start at real hot entries and follow direct branches and sequential flow, up to 256 instructions and a 16 KiB span. Callees, existing or pending entries, stops and edges past the limits become link frontiers, so guest transfers are never cut. A new entry produces its own fragment.
- Regions link through a slot table with tail calls, and indirect targets use a bounded lookup. Regions, loop kernels and row trampolines share the context and memory two-pointer tail contract in `compile::contract`, and region boundaries publish `CpuState` by write set. `set_region_linking(false)` turns linking off.
- Native SHL, SHR and SAR cover 8, 16 and 32-bit registers with an immediate or CL count. Memory-operand shifts use the interpreter.
- Controlled x87 loads, stores, arithmetic, compares, exchanges and simple stack operations become native floating-point instructions, with the 80-bit registers still authoritative. Anything outside the exact binary64 subset, the precision and rounding guards, exceptions, mixed MMX regions or special memory uses the transactional helper. `ADV_JIT_X87_HELPER_ONLY=1` turns native emission off at compile time for A/B checks.
- Row batches run a proven counted inner loop across rows on a pool of host workers, while the owner stays inside its native call. The last write is merged in guest row order, so this is not parallel guest execution. `--row-workers` sets the pool size.

### Background compilation

- `JitCpu` sends automatic hot promotion to the shared two-level background service in `adv-sync`. Queued, compiling and unclaimed results share 8 permits. A worker sees only the region bytes (at most 16 KiB plus 14 trailing bytes) and a page version snapshot, never live guest memory.
- Only the execution owner, outside native calls, checks the request ticket, page versions, permissions and mapping before publishing code and updating slots. Stale results are dropped, and failed compilations stay in the interpreter with bounded diagnostics.
- `WorkPolicy::responsive(units)` splits Fast jobs by decode work; the default is unlimited. Speed jobs over budget keep the Fast code. Manual `add_region` stays synchronous and outside the budget.
- Production regions compile Fast first, and long-lived hot spots are promoted to Speed. A bare `Executor` compiles synchronously unless `set_async_compilation(true)` is called.
- `set_compile_threads(n)` uses n dedicated threads. The shell keeps one core for guest execution and one for audio and presentation, then uses at least 1 compile thread, at most 3 on Android or with fewer than 8 cores, and at most 4 otherwise.
- The report's `compilation_schedule` gives Fast and Speed histograms of `deferred_us`, `queue_us`, `service_us`, `publication_us` and `age_us`, with no per-request addresses or code bytes.

### Code caches and validity

- The in-memory code cache holds at most 16,384 regions and 128 MiB of generated code by default (`set_code_cache_limits`). The byte count excludes paging, constants and snapshots, so it is not an RSS limit. The owner evicts by recent use when no native call is active.
- `jit-warm.bin` lists region identities worth compiling again; the next start queues them after the code mapping matches, at most 2,048 entries within 60 s. It holds no machine code.
- The CLI keeps persistent machine code in `jit-code.bin` under `--data-dir`: at most 128 MiB and 4,096 entries, evicted by recent use and replaced atomically on exit. The key covers code and page dependencies, the main EXE and module identity and base, compile options, the GIR and semantic version, the host binary and the compiler, ABI and ISA. A miss or mismatch compiles normally, and a hit rebinds symbols, relocations and recovery tables before publication through the owner.
- The loader seeds entries, TLS callbacks, exports and relocations of the main image and non-system DLLs, and background discovery follows direct calls and branches. Image snapshots total at most 32 MiB, with at most 512 candidates and 32,768 decoded instructions per image. Candidates use idle compile slots and are published only when execution reaches them with matching inputs.
- `--no-code-cache` turns off the persistent cache, warm profile and early compilation, for troubleshooting only. The CPU API is `set_persistent_code_cache` and `save_persistent_code_cache`.
- Code writes, unloads and protection changes update dependencies. The owner revokes links, publishes new entries and frees old code when no native call is active. Published code revalidates only its decoded instructions; cross-page instructions and non-executable tail pages keep negative dependencies.

### Memory

- `address_space()` reserves the full 4 GiB and commits by page. Guest pages are 4 KiB on any host page size, and a guest page keeps its meaning when another page in the same 16 KiB host page changes.
- Production access is checked, with loop proofs removing repeated checks where they hold. On the serial checked path, initialized private COPY pages with no outside aliases allow ordinary reads from an immutable permission snapshot; the first write still returns to the owner to retire COPY.
- On Linux and Android, committing a fully free host page uses `MADV_DONTNEED` for on-demand zero pages and clears explicitly when that fails.
- Only registered, exact host fault sites are handled. Handlers do not allocate, take ordinary locks or schedule guests. Recovery restores canonical state and completes at most one checked access; it never replays a committed prefix.

## Engine interface

The shell controls execution only through `adv_cpu::Engine`: `configure` passes the compile policy, threads, output folder and cache switches once; `report_fields` adds engine-specific report fields; and `persist` saves the warm profile and code cache on exit. An engine ignores options it lacks, so the warm profile, background compilation and `--no-code-cache` do nothing on AArch64.

1. `Executor::new(base, len)` serves small test windows, and `Executor::address_space()` serves processes.
2. `map`, `unmap` and `protect` manage pages, and `write` loads or patches code. Set `state`, then call `run(stop, budget)`.
3. `run_with_traps` delivers raw interrupts, CPU and memory to `ExecutionTrap`, or returns as the handler asks. `JitCpu` owns an `Executor` and implements `backend::Backend`; APIs, callbacks and waits are scheduled only by `adv-machine` and `adv-abi`.
4. `Stats` counts native and interpreted execution, compile submissions, completions and stale drops, code invalidations and raw traps. Background `compile_time` overlaps guest execution and is not main-thread blocking time.

`Runtime<B>::with_cpu` lets tests inject another backend while the loader, devices and APIs stay shared. Production `adv-run` accepts only `--backend jit|interpreter`.

```sh
cargo test --release -p adv-cpu
cargo run --release -p adv-cpu --example engine_bench
cargo run --release -p adv-cpu --example region_link_bench -- 7 200000
cargo run --release -p adv-cpu --example topology_bench -- 31 200000
```

Tests use self-written pixel and control-flow kernels, independent integer formulas, Unicorn single-step comparisons, native x87 rounding results, cross-page atomic commits, context, budget and code-version races, and a real MinHook fixture. Cross-compiling for ARM and measuring on ARM hardware are separate checks.
