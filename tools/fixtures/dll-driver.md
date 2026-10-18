# DLL export driver

`fixture_dll_driver.exe` is an x86 C fixture without CRT dependencies.
It reads `dll-driver.json` from the working directory, loads the specified DLL, executes export calls, and writes `dll-driver.jsonl`.
The same EXE, DLL, and JSON run on Windows, the adv-run interpreter, and JIT.
This tool compares functions and measures module costs without engine-specific logic.

## Build and run

Use this directory's LLVM/Windows SDK import-library build process:

```powershell
./tools/fixtures/build-dll-driver.ps1
python tools/performance/dll_exports.py --runner target/release/adv-run.exe
```

The default script is a repository-owned probe covering three ABIs, ordinal exports, return-value references, structure pointers, and memory hashes.
Use `--variants native` for native validation only, or `--variants native jit` to compare JIT only.
Pass additional runner arguments with forms such as `--runner-arg=--sync-compilation`; keep configurations consistent across runs.
Do not override the tool's backend, overlay, report, strict-mode, or guest-codec settings.

Resolve real DLL directories only through `$ADV_SAMPLES`; keep scripts and input assets local:

```powershell
python tools/performance/dll_exports.py --runner target/release/adv-run.exe --script target/scratch/local-module.json --sample-dir engine/module --output target/investigations/dll-exports/module-run
```

`--sample-dir` names a directory relative to `$ADV_SAMPLES`, containing the DLL, dependencies, and data.
The tool copies that directory into each run's isolated `image/` and freezes the entry EXE, adv-run, and runner dependency DLLs.
Adv-run writes to an isolated `data/overlay/`. Output must use a new directory under `target/investigations/`.
Each run's `cmd.txt` records commands, ADV environment variables, and EXE/DLL/runner/script SHA-256 hashes.
`summary.json` contains only call counts, comparable scalar results, memory hashes, timing statistics, and validation summaries.
It contains no pointer addresses, memory bytes, or disassembly. Execution images and overlays are local inputs/artifacts, never repository content.

## Script format (version 1)

Top-level fields are `version`, `dll`, optional `buffers`, and required `calls`.
DLL and buffer-file paths are relative to the working directory; JSON Unicode characters/escapes are supported, with paths converted from UTF-8 to UTF-16.
Unknown or duplicate fields, out-of-range values, and wrong types are rejected. JSON numbers must be integers; hexadecimal words require strings.

Buffer entries:

| Field | Meaning |
|---|---|
| `id`, `size` | Unique name and byte count; VirtualAlloc storage lives throughout the call sequence |
| `fill` | Initial fill byte, default 0 |
| `hex` | Contiguous hexadecimal bytes, such as `"01020304"`, replacing bytes from the start |
| `words` | Little-endian DWORD array, replacing bytes from the start |
| `file` | Local input file, replacing bytes from the start; must fit in the buffer |

Choose at most one of `hex`/`words`/`file`; remaining bytes retain `fill`.
Limits are 256 buffers, 64 MiB per buffer, and 256 MiB total.

Call entries:

| Field | Meaning |
|---|---|
| `id` | Unique call name; later steps can reference its last return value |
| `export` | Exact export name, including decoration, or integer ordinal 1–65535 |
| `abi` | Explicit `stdcall`, `cdecl`, or `thiscall` |
| `args` | Raw stack words in declaration order, at most 64; omitted means no arguments |
| `this` | ECX value for thiscall; forbidden for other ABIs |
| `iterations` | Positive timed-loop count, default 1 |
| `warmup` | Calls before timing, default 0; warmup changes state without automatic reset |
| `return` | `u32` (default), `u64`, `pointer`, or `void` |
| `expect` | Optional expected final EAX; mismatch exits with 2 |
| `writes` | Buffer DWORD writes before calls, allowing embedded pointers; performed before resolving this/args |
| `hashes` | Buffer ranges observed after calls; omitted means all buffers, empty array means no memory observation |

`writes` runs once before warmup, using
`{"buffer":"state","offset":4,"words":[{"buffer":"pixels"},16]}`.
A `hashes` entry uses `{"buffer":"pixels","offset":16,"size":224}`;
`offset` defaults to 0, and `size` defaults to the remaining bytes.
For structures containing native pointers, explicitly select stable fields to avoid treating process-specific addresses as semantic differences.

Words can be JSON integers (unsigned DWORD or signed i32), hexadecimal strings, or these references:

| Expression | Value |
|---|---|
| `{"buffer":"name","offset":4}` | Buffer pointer plus byte offset |
| `{"read":"name","offset":4}` | Little-endian DWORD read from the buffer |
| `{"result":"create","word":"eax","offset":4}` | Earlier return word plus offset; word defaults to eax and also accepts edx |

Manually split 64-bit integers, floating-point values, and structures into stack words according to the target ABI; thiscall's `this` is separate from `args`.
`u32` compares EAX; `u64` compares EDX:EAX. `pointer` reports/compares only nullness, retaining the actual pointer internally for later references.
`void` does not compare registers. Scripts must initialize and release resources according to function lifecycles and arrange arguments according to the DLL's contract.
The driver checks ABI stack-cleanup differences, restores the stack, and reports errors.

## Comparison and timing boundaries

Each step emits begin, end, and result; loading, warmup, and hashing are outside the timing window.
Result includes the last ABI-defined scalar return value and FNV-1a 64-bit memory hashes.
Completion emits complete; errors emit error and exit with 2. Failure to write results exits with 3.
Comparison first checks every step's counts, return types/values, and hashes. Any mismatch leaves the module ratio empty and exits with 1.

Native timing uses QPC. Adv-run's QPC is a clamped guest clock, retained only for diagnostics and excluded from ratios.
The Python tool polls JSONL begin/end markers on the host and times them with `perf_counter_ns`, including loop-control and call-trampoline costs.
Observations include file-write boundaries and polling error. Polling defaults to 1 ms; results also record the actual maximum interval.
Both windows must reach `--min-ms` (default 20 ms).
The guest's host-observed window must also reach ten times that run's maximum observation interval before a function ratio is reported.
Native QPC windows are independent of guest polling intervals.
Otherwise, `interval_too_short` indicates that script N should increase; the tool never automatically reruns, changes N, or extrapolates short windows.
The module ratio compares total durations of calls meeting the threshold; `measured_calls` states the included count.

For stateful modules such as E-mote, first use small N to verify initialization/execution/release returns and stable outputs, then select sufficiently long deterministic loops.
Equal final hashes do not prove matching intermediate states, complete memory, or full-game behavior.
This tool does not replace headed frame-rate, input-latency, or power acceptance.

## Tests

```powershell
$env:ADV_DLL_DRIVER_RUNNER = (Resolve-Path target/release/adv-run.exe).Path
python -m unittest discover -s tools/performance -p test_dll_exports.py -v
```

Python-only tests verify comparison thresholds, clock selection, protocol completeness, and sample-path constraints.
Windows tests invoke the build script, then verify real x86 ABIs, file/hexadecimal initialization, pointer offsets, and invalid scripts.
With `ADV_DLL_DRIVER_RUNNER`, tests run native, interpreter, and JIT variants, checking equal export returns and memory hashes plus actual JIT compiled-code execution.
All test artifacts use isolated directories under `target/scratch/`.
