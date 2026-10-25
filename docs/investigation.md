# Investigation

This page says where local run output goes and which existing tool to reach for first. Each tool documents its options and output in its docstring or header comment. [building.md](building.md) covers building and tests.

## Local output

Runs and analysis stay under the ignored `target/` folder, one folder per topic and run:

```
target/investigations/<topic>/<run>/
target/scratch/<agent>/          # one-off scripts and intermediate files
```

- Each run folder records the full command line and environment, the SHA-256 of the binary under test and the SHA-256 of the entry EXE: `play_session.py` writes them to `session.json`, and other runs keep a `cmd.txt`.
- Run a copy of the runner from the run or topic folder's `bin/`, with the FFmpeg libraries of a dynamic media build, so a rebuild elsewhere cannot replace it between or during runs. `play_session.py --pin-runner` does this.
- Give each run its own `--data-dir`. Copy saves or settings from a seed folder with `play_session.py --seed-data`. Runs never share an overlay or write to the game folder.
- Record the OS, host page size, engine and build with each result. Evidence from QEMU, cross builds, real hardware and self-written fixtures is not interchangeable.
- Refer to games only through `$ADV_SAMPLES`. Traces, screenshots, dumps, disassembly, guest code bytes and guest instruction addresses stay local. Only statistics and conclusions go into the repository.
- An analysis needed a second time becomes a tool under `tools/`, with a docstring describing its use, input and output, and a test.

## Tools by symptom

`cargo run -p adv-runtime --features gpu --example host_call_inventory -- --check docs/host-call-boundary.tsv` checks the generated registry boundary table, including aliases and COM methods. `--write` refreshes it, as does `python3 tools/check.py --fix`. `cargo test -p adv-runtime --features gpu --example host_call_inventory` tests coverage and conservative admission. Unproven ordinary effects and Windows kernel entry remain explicitly marked as not excluded; names alone never authorize acceleration.


### Startup and compatibility

| Symptom | First | Then |
|---|---|---|
| A user report or a run report | `tools/survey/triage_report.py <report.json>`: gaps by DLL, where each is registered, and the next command | For an anonymous report it prints the discovery rerun with the report's code page; the run report's `missing_apis` then lists every gap at once. |
| Where an API is implemented | `tools/survey/where.py <dll!api> ...` | Registration status from `host-call-boundary.tsv`, source lines, the table handler, cited specs and tests. |
| Planning a new game's work | `tools/survey/gap.py <game folder>` (needs `pefile`) | Static imports no registered DLL provides. An estimate only: implement from real calls in `missing_apis`. |
| Startup hangs, error box, early exit | `sample_matrix.py --only <id> --api-mode discovery --trace`, then the first failure in the report | `trace_hresult.py` summarizes failed HRESULTs. `--warm-from <cold run>` reuses the JIT profiles of a matching cold run. |
| Smoke test after a personality change | `sample_matrix.py --quick` over `tools/performance/startup-smoke.json`, then `matrix_gate.py --require-frames --frame-tolerance 1` | One game per engine for 10 seconds. A run with zero frames or a new structural warning is a regression. |
| First divergence in a new game, zero frames for no known reason | Native capture and `api_diff.py` in [tools/compat](../tools/compat/README.md), aligned on the main thread | Without child processes or output buffers the result is partial. |
| Wrong behavior after some interaction | Reproduce up to that step with a `play_session.py` schedule | `--trace` keeps `api.trace`. The control command `memory ADDR LEN` reads guest memory. |
| Wrong picture | A `checkpoint` or `capture` | `--gpu-software` compares adapters, and `trace_hresult.py` shows rendering failures. |
| Win32 behavior differs from Windows | A self-written fixture in `tools/fixtures/` run natively, under the interpreter and under the JIT; record its native result once on Windows with `tools/fixtures/record_native.py`, and `native_reference` in `adv-runtime` then compares every host and backend with it. To learn the expected behavior first, read Wine's conformance test for the API (`dlls/<dll>/tests/*.c`): `ok()` states what Windows returns, and `broken()` marks results that differ between Windows versions, often the Windows 7 behavior the default profile follows | Each fixture's header comment states what it checks and how to build it. `personality_contract.py` runs the shared personality contracts. |
| Backend hangs, or a run needs a CPU comparison | `backend_bisect` in `adv-runtime`, with `ADV_BISECT_EXE` set | Each backend runs with a fixed runner copy, its own data folder and an outer deadline. A timeout proves nothing about correctness. |

Authored fixtures and generators for one area:

| Symptom | First | Then |
|---|---|---|
| Android APK page compatibility | `tools/android/check-apk.py <apk>` | Standard-library ELF LOAD and stored ZIP data alignment checks for 16 KiB pages. Tests: `python -m unittest discover -s tools/android -p test_check_apk.py`. |
| Upscaling shader regeneration | `tools/upscaling/generate.py <slangc>` | Pinned upstream shaders → WGSL, quantized layer fusion, cooperative matrix RGB convolutions and texture lifetimes; no game inputs. Tests: `python -m unittest discover -s tools/upscaling`. |
| Recursive Shell directories | [`shell_directories.c`](../tools/fixtures/shell_directories.c), `build-shell-directories.ps1` | Authored local paths and scalar output; Unicode/ANSI recursion, existing paths and errors. Test: `cargo test -p adv-runtime --features gpu --test native_reference`. |
| D3DX empty textures | [`d3dx9-texture-create.c`](../tools/fixtures/d3dx9-texture-create.c), `test-d3dx9-texture-create.ps1` | Authored dimensions, formats and usage; scalar correction, mip and lifetime comparisons. Test: `cargo test -p adv-runtime --test d3dx_texture --features gpu`. |
| D3DX axis rotations and Vec2 | [`d3dx9-math2.c`](../tools/fixtures/d3dx9-math2.c), `build-d3d9.ps1 -Name d3dx9-math2` | Authored matrices, overlapping outputs and special values; native scalar comparison with 2e-6 tolerance. Test: `cargo test -p adv-runtime --test d3dx_math2`. |
| Linguistic comparison | [`nls-compare.c`](../tools/fixtures/nls-compare.c), `build-nls-compare.ps1` | Authored strings and locale flags; A/W/Ex and lstrcmp scalar results. Test: `cargo test -p adv-runtime --features gpu --test native_reference`. |
| Mouse tracking | [`mouse-track.c`](../tools/fixtures/mouse-track.c), `build-mouse-track.ps1` | Hidden owned windows without host cursor movement; query, cancellation and leave results. Test: `cargo test -p adv-runtime --features gpu --test native_reference`. |
| D3D9 texture-target copies | [`d3d9-texture-blit.c`](../tools/fixtures/d3d9-texture-blit.c), `test-d3d9-texture-blit.ps1` | Authored RGBX/ARGB images; scalar and pixel comparisons for rectangles, container identity and render state. |
| Multiline DrawText | [`drawtext-lines.c`](../tools/fixtures/drawtext-lines.c), `build-drawtext-lines.ps1` | Authored text and offscreen DIBs; RGB hashes, line heights, clipping and preserved rectangles. Test: `cargo test -p adv-runtime --features gpu --test native_reference`. |

### CPU and JIT

| Symptom | First | Then |
|---|---|---|
| Suspected CPU semantics | The same path with `--backend interpreter`; when the interpreter cannot reach the failing point in reasonable time, `ADV_JIT_VERIFY=1` (or `strict`) with the JIT | Bisect with `--no-region-linking`, `--sync-compilation`, `--no-tiering` and `--compile speed`, then write a Unicorn differential test. |
| Suspicious x87 results | `ADV_JIT_X87_HELPER_ONLY=1` | A result that differs from the helper means native emission is wrong. |
| Local instruction or return stack problems in the interpreter | `--backend interpreter` with the `ADV_INTERP_TRACE_*` variables below | Combine with the `StackTop` and `Candidate` fields of raw interrupts. |
| Synthetic Tier 0 baseline | `cargo run -p adv-cpu-core --release --example tier0_baseline -- --seconds 2` | Authored integer/string/x87/MMX/indirect-call loops, retired-instruction checks and JSON throughput; no game samples. |
| CPU suite and strict judgement | `acceptance.py [--strict] <logs>` in `tools/cpu` | On AArch64, `arm64-check.sh <ref>` runs the suite once and judges it both ways. |
| Execution cliffs and tier verification counts | `tools/survey/cliffs.py <report.json> ...` | Groups plain and verify runs and uses only each run's final report. |
| IR growth and compile service time | `ADV_JIT_IR_COUNTS=1`, then `tools/survey/compile_ir.py <stderr.log>` | Counts are anonymous. Byte categories are not a cost model for single IR operations. |
| x64 region entry checks and cold compile cost | `host_region_bench` example in `adv-cpu` | `ADV_REGION_BENCH_COMPILE=1` adds Fast and Speed compile percentiles. |
| AArch64 execution cost | `engine_bench` example in `adv-cpu` | Compares the interpreter and the platform engine through `Backend` only, in ns per guest instruction. |
| Poll word and interrupt latency | `safepoint_bench` example in `adv-cpu` | `ADV_SAFEPOINT_BENCH_TIER` selects Tier 0 or 2. |
| Which observed code static discovery covers | `tools/performance/static_coverage.py` | Labeled wall-clock sample coverage, not a complete PC set. |
| Native instruction rate for frame budget estimates | `native_instruction_rate` example in `adv-cpu` | Feed its JSON to `frame_budget.py --native-rate`. |

### Performance

| Symptom | First | Then |
|---|---|---|
| Uneven animation intervals, slow Ctrl skip, long first start | `frame_budget.py run --platform android\|windows --binary <runner> --output <run>` | Summarizes presentation runs, load boundaries and Ctrl commits per second. `--no-code-cache` is for troubleshooting only. `--frame-budget` reads the clock twice per host call, so leave it off when timing startup. |
| Slow, stuttering or slow skipping in a game | The attribution workflow below | |
| Two builds A/B | `compare_runtime.py` on a fixed frame prefix, gated on frame hashes | Compatibility regressions use `sample_matrix.py` and `matrix_gate.py`. |
| Heap allocation cost | `heap_service` example in `adv-os` | `large` and `metadata` select other workloads. |
| Short name comparison, Global and Vorbis HLE cost | `hot_hle` example in `adv-win32`, with its own `--data-dir` | Includes guest execution and cold compilation, so it is not pure dispatch cost. |
| Slow D3D9 system memory texture upload | `benchmark_update_texture` in `adv-dx`, run with `--ignored` | Semantic tests sit beside it. |
| Wait precision | `wait_precision` example in `adv-sync`, `hle_wait_precision` example in `adv-win32` | `--no-time-skip` checks the Ctrl policy. |
| AArch64 host CPU attribution | `ADV_PERF_MAP_DIR=<run>` with simpleperf, then `tools/performance/arm64_hotspots.py` | Off by default and anonymous. Without kernel access, kernel time is not measured. |
| Android CPU suite and execution experiments | `tools/android/run-cpu-tests.ps1 -AllTests` or `-ExecutionExperiments` | Use `-WhatIf` or `-BuildOnly` without a device. |
| Android scene summaries | `tools/android/summarize-baseline.py` | Checks that scene, binary and configuration match. |

### Devices and Windows ABI

| Symptom | First | Then |
|---|---|---|
| Sound | `ADV_AUDIO_TRACE=<file>`, with `ADV_AUDIO_DUMP_DIR=<dir>` | `play_session.py --capture-audio` and `analyze_audio_capture.py` |
| Text and fonts | `ADV_TEXT_DIAGNOSTICS=1` | `ADV_FONT_PROFILE` |
| mmio and RIFF reads | `ADV_MMIO_TRACE=<file>` | |
| Cost of one DLL, function-level differential | `tools/performance/dll_exports.py` with an x86 C fixture and a JSON script | [The driver protocol](../tools/fixtures/dll-driver.md) |
| Fixed core exports, ordinals and API set forwarders | `tools/abi/system_exports.py` | Reads PE export and namespace metadata only. |
| SDK ordinal import identity | `tools/abi/import_ordinals.py --check` | |

### Traces

- `--trace` writes one line per call: `ctx  frame  module!name  args  ret`, separated by tabs. It changes timing, so it is never used for timing.
- A process or thread exit that does not return adds a `# terminal` comment with the original binding, exit code, entry stack and resume point.
- `# cpu_fault` records a real CPU fault before personality exception handling. It does not mean the fault went unhandled. Interrupts use `# raw_interrupt`. Addresses stay out of shared reports.

**Bounded interpreter observation.** With `--backend interpreter` on either CPU engine, set `ADV_INTERP_TRACE_DIR` to an existing folder under `target` and `ADV_INTERP_TRACE_RANGES` to comma-separated half-open `<start>..<end>` PC ranges.

- `ADV_INTERP_TRACE_MAX` defaults to 4096 (1 to 100000), and `ADV_INTERP_TRACE_STACK_WORDS` to 4 (0 to 8). `ADV_INTERP_TRACE_START` delays observation until a PC.
- Each executor writes `interpreter-*.jsonl` with the opcode name, integer registers and flags before and after, stack words, the step and the stop reason. It holds no code bytes.
- `ADV_INTERP_TRACE_WATCH_DWORD` watches one address. The recorded PCs narrow down the writing block; they do not name the exact instruction.
- `ADV_INTERP_TRACE_OPERANDS=1` adds explicit register, immediate and memory operand values of up to 8 bytes.
- Observation never changes guest memory or exception behavior. Output stays local.

**Tier verification.** `ADV_JIT_VERIFY=1` interprets the first native entries before running them natively and counts the comparisons in the report's `tier_verification`; `strict` stops at the first mismatch. The x64 engine checks one unlinked block after each publication. The a64 engine checks the first runs from each entry PC by their retired instruction count. Mismatch reports carry guest addresses and stay local.

## Live sessions: the debugger

For a stall, freeze or stutter the user sees, attach to the session they are playing instead of replaying it. Start `adv-run --debug`, or the launcher or `adv-run` with `ADV_DEBUG=1`. The session serves a socket under the temp folder's `hitode-debug/` and prints its path.

```
python3 tools/debug/hitode_debug.py list               # sessions, newest first
python3 tools/debug/hitode_debug.py [--pid PID]        # prompt; or append one command
```

- **At once, even while the guest is wedged:** `info` (build, platform, media and GPU features), `timeline` (one row per presented frame with execution-thread busy time), `samples` (the in-process sampler, 500 Hz by default, one minute kept; Apple arm64 hosts), `profile off|HZ`, `mark`, `marks`, `snaps` and `pull`.
- **At the next guest boundary, or a timeout that says the execution thread is stuck:** `threads` (state, waits, wait objects, parked API, caller and scanned return addresses), `stack`, `calls` (the last 256 host calls), `gaps` (unavailable and missing APIs, stubs, message boxes, failed lookups), `modules`, `mem`, `dis`, `slow [reset]` (helper and interpreter runs by EIP) and `report`.
- **Shaped by the client:** `tl` lists frame gaps and whether the thread was busy or waiting; `prof` splits samples into slices (`--range A:B` narrows to one stall) and names the hottest guest units (module+offset), guest instructions with the part of their code (body, cold, x87-entry, x87-publish, x87-wide for integer x87 arithmetic, fp-status for the SSE slow path through FPSR, host below them, or a shared stub such as `stub:lookup`) and host functions. A snapshot also lists its hottest instructions with their form and host code size by part, and `prof` and `analyze` group samples by instruction family, so scattered hot instructions add up by mechanism.
- **Analysis:** `analyze` snapshots now and, for each frame gap, gives its sample shares, the hottest guest regions (nearby units of one module merged), host functions called from them, slow paths near them and a one-line verdict: waiting, generated code, overhead parts of generated code (stubs, x87 entry or publication), host calls from guest code, or translation. `watch` pulls and analyzes every new snapshot until the session ends and appends the reports to `target/investigations/debug/<pid>-analysis.txt`. Run it in the background while the user plays.
- **Snapshots:** **Alt+Shift+F12 (Option+Shift+F12 on a Mac)** in the game window marks the moment and keeps a snapshot of the last 30 seconds in `<data-dir>/debug/`; `snap` takes one on request. A frame gap of 250 ms or more that the execution thread spent mostly in guest steps takes one by itself 2 s later (up to 32 per session); waiting gaps do not, since engines that present only on change leave them on every still screen. A snapshot holds the raw samples, slow paths since the previous snapshot and the runner's image and base, so `--snapshot FILE tl|prof|analyze` works after the session has ended. The client saves snapshots under `target/investigations/debug/`; they hold game text and stay local.

Wine sessions use the same debugger in headless and windowed runs. `info` names
which stack is running. `modules` and `threads` list the session's processes;
`mem`, `dis`, `stack`, `calls` and `slow` inspect the presented process. `calls`
records NT/Unix and host-module boundaries, not calls between Wine PE modules.
`gaps` reports that the built-in stack's API diagnostics are unavailable.
Addresses in the distribution's modules are named `module!function+offset`
from the code identity tables when the mapped image's headers match them.
Timeline execution counters cover the process family, including exited children.
The report's `wine_boundaries` counts the family's system calls (NT and win32k
tables), Unix calls by library and code, and server requests by name, each with
the host time of its synchronous part; a system call's time includes the
requests it sends, and guest continuations and waits are not counted.
Samples lack guest process identity, so Wine profile summaries keep raw guest
addresses and omit process-specific instruction forms and code sizes.

## Reproducing and driving

**Control commands.** `--control-stdin` takes one command per line:

- Input: `click X Y`, `move X Y`, `mouse-down X Y`, `mouse-up X Y`, `right-mouse-down X Y`, `right-mouse-up X Y`, `key VK`, `key-down VK`, `key-up VK` and `text STRING`.
- Menus and dialogs: `menus`, `menu ID`, `dialogs`, `dialog ID`, `dialog-track ID VALUE` and `dialog-close`.
- App activation: `activate 0` and `activate 1`, as a frontend reports switching away and back.
- State: `windows` (presented frames, the displayed top-level window, focus, active and capture windows, and each top-level window's class and text) and `hit X Y` (the window a click there reaches, or `null`). Poll them to wait for a screen instead of sleeping a fixed time; `play_session.py` steps do this with `wait`.
- Records: `capture PATH`, `report PATH` and `memory ADDR LEN`.
- Measurement and flow: `reset-metrics`, `await-frame HASH TIMEOUT_MS ...`, `resume` and `close`.

Coordinates are guest canvas coordinates, so they change with the guest resolution and the game's window or full screen setting.

**Replay with `play_session.py` and a JSON schedule.**

- `at` is seconds since process start. `checkpoint` saves the picture and report, and `reset_metrics` starts a new measurement window.
- Reusable scenarios without paths go into `tools/performance/scenarios/`. Do not write a private driver script.
- To compare headed performance, use `play_session.py --mode hidden --audio --mute-audio` without capture or trace. This keeps the interactive compile budget and the real audio device path. Headless runs have a different background compile budget.
- To sample at the same time, wait until the schedule passes `reset-metrics`, then start a sampler on the same PID.

Headless runs check mechanisms and gains quickly. Fix the binary, configuration, code page, seed and input, and prefer short A/B runs. A headless result states its conditions and does not stand for headed frame rate, input latency or power use; the user judges those on the target platform.

## Performance work

- **Targets.** ARM macOS, iOS and modern Android share the a64 engine and get the same optimizations. The x86_64 engine is frozen and only gets correctness fixes; do not change the a64 contract in [docs/spec/a64.md](spec/a64.md) without a maintainer.
- **Priorities.** First, transitions and animations run at the game's own pace without stutter or runs of dropped frames. Second, Ctrl skipping advances the story fast; dropped frames there do not matter. Third, the first start may be slower, but not much.
- **Baselines.** Normal play has no audio dropouts, a click shows on screen within 100 ms at p95, and waiting for input costs almost no CPU. Frames that are slow natively do not count, load pauses are judged on their own, and steady state comes before one-time costs.
- **Spend where it shows.** Never trade away what a player perceives: stutter, dropped frames, slow loads, audio glitches. Shrink what a player cannot see, such as hashing, copies, host calls, wakeups, memory bandwidth and diagnostics, and move what must stay to moments nobody notices. When unsure, keep the experience and prove the saving with counts.
- **Mechanism first.** Read the code path that costs, then run one measurement that tells hypotheses apart; prefer shared fixes over patching functions one by one. CPU translation, memory, scheduling, codecs and presentation can change together, so current boundaries are not limits.
- **Correctness first.** A missing or wrong implementation is never explained away as slowness. A game limited only by emulation speed may ship with that boundary recorded.

## Performance attribution

1. **Start from a native baseline.** Use the same EXE, the same configuration and the same point in the game. Compare how long it takes to reach a point or how much the story advances per unit of time, not only frame counts.
2. **Compare function shares with native.** A function near zero natively but high under Hitode usually means the guest took another path, such as a different device capability. Shares close to native but a much slower whole means a general cost in generated code.
3. **For scattered hot spots, look for shared costs** such as memory checks, register spills, region entry and exit, REP and helper round trips. Do not patch functions one by one.
4. **`--profile` disables region linking** and creates entries that normally do not exist. Use it only to count entries.
5. **Sampling reads the wall-clock RIP of one thread.** All guest threads run on one host thread, while a native game spreads work such as audio decoding over several. Sampling suspends the thread, so read results as shares.

```
# native baseline: CPU per thread, EIP of the busiest thread by module and function
python tools/performance/native_sample.py sample <run> --name <game.exe> --seconds 20 [--threads 2] [--while-key ctrl]
python tools/performance/native_sample.py report <run>
# runner: region table jit-<pid>.tsv and instruction table insn-<pid>.tsv
ADV_JIT_MAP_DIR=<run> <bin>/adv-run <exe> ... --report <run>/report.json
# sampling: pc-samples.tsv, sample-modules.json, optional region code and host stacks
python tools/performance/pc_sample.py <run> --pid <pid> --seconds 20 [--dump-regions <hex,...>] [--stack]
python tools/performance/attribute_pc.py <run>
python tools/performance/native_hotspots.py <run> <guest_start>   # machine code and samples of a hot region
python tools/performance/native_hotspots.py <run> --summary       # generated code cost by part and category
python tools/performance/stack_report.py <run> [--focus <name>]   # host call stacks
# macOS, also the launcher: one-second slices with guest regions (a64 writes jit-<pid>.tsv only)
ADV_JIT_MAP_DIR=<run> <launcher or adv-run> ...
python3 tools/performance/macos_sample.py record <run> --pid <pid> --seconds 30
python3 tools/performance/macos_sample.py report <run> [--from <ms> --to <ms>]
python tools/performance/compare_loop_profiles.py <base> <cand>
# before running: what a module will ask of the CPU
python tools/survey/module_profile.py <module> [--json <out>]
```

- `native_sample.py --while-key ctrl` samples only while the key is held, to measure skipping. Count work that a native game spreads over threads against the single Hitode thread.
- `native_hotspots.py --summary` gives host instructions per guest instruction, split by check, body, pure and slow, and the share of stack accesses. Untagged code counts toward the next tag, so the check share is a lower bound.
- JIT code has no unwind information, so host stacks stop at native regions. They answer who calls a host function, not which guest code ran.
- `ADV_JIT_DUMP_DIR` with `ADV_JIT_DUMP_REGIONS=<hex,...>` dumps the VCode of chosen regions.
- `--schedule-profile <run>/schedule.tsv` with `analyze_schedule.py` covers scheduling, locks and long frames. `--host-profile <csv>` gives API service time.
- `ADV_LOOP_COVERAGE=1` with `--report` writes `*.loops.tsv` for `loop_coverage.py`. It ranks by instruction weight, not time.
- `ADV_INTERP_PROFILE=<file>` profiles the interpreter.
- On macOS, `play_session.py` writes cumulative process CPU time and memory to each checkpoint's `*.process.json`. Subtract CPU time and `wall_seconds` between checkpoints for whole-process core usage; 1.0 means one fully occupied core.
- Checkpoint reports of `--frame-budget` and `--host-profile` carry cumulative `host_services_csv`. Subtract two checkpoints for the service time and count of a fixed segment; it includes handlers and continuations and excludes guest callbacks and waits.
- In code cache reports, `load_ns` covers reading, checking and parsing the archive, and `restore_ns` covers relocation and publication after a hit. Neither is pure disk I/O.

A finding goes into the commit or pull request that acts on it, with the baseline configuration, the sample count and window, and the hypotheses ruled out.
