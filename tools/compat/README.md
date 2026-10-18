# New-game API triage

On Windows x64, `native_capture.exe` creates a suspended x86 game and remotely calls `LoadLibraryW` to load the logging DLL.
It then calls the exported initializer and resumes the main thread.
`api_diff.py` aligns local native and Hitode traces, reporting the first non-ignored divergence candidate, call numbers, context, and subsequent summaries.
A divergence candidate does not establish the cause of zero-frame execution.

## Build and capture

Install LLVM `clang-cl`/`lld-link`, the Windows SDK (default 10.0.26100.0), and Visual Studio x86 static CRT libraries, which provide large-stack-frame probes.
The build uses vendored MinHook verified by SHA-256. It downloads no dependencies and does not link MinHook into the runtime.
The generator reads `data/abi/signatures.tsv`; generated files and `coverage.json` stay in the build directory.

Set `ADV_API_DIFF_RUN` to the user's local artifact directory. `ADV_SAMPLES` is the read-only sample root.
Replace the placeholders below with sample-relative paths; never store local sample paths in the repository.

```powershell
$run = $env:ADV_API_DIFF_RUN
if (!$run) { throw 'Set ADV_API_DIFF_RUN before capturing' }
New-Item -ItemType Directory -Force -Path $run | Out-Null
$env:TEMP = $run
$env:TMP = $run
$env:PYTHONDONTWRITEBYTECODE = '1'
& tools/compat/build-native.ps1 -OutputDirectory "$run/build"

# Copy the entire game directory for native writes; never run against the source.
$source = Join-Path $env:ADV_SAMPLES '<sample-relative-directory>'
Copy-Item -LiteralPath $source -Destination "$run/native-game" -Recurse
& "$run/build/native_capture.exe" --dll "$run/build/native_trace.dll" `
    --output "$run/native.trace" --seconds 30 --max 100000 `
    --exclude '*!GetTickCount;*!QueryPerformanceCounter;*!timeGetTime' `
    --cwd "$run/native-game" -- "$run/native-game/<entry.exe>"

# Freeze the runner and its required DLLs into $run/bin before this command.
& "$run/bin/adv-run.exe" (Join-Path $source '<entry.exe>') `
    --headless --audio --codepage 936 --api-mode strict --seconds 30 `
    --data-dir "$run/overlay" --trace "$run/adv.trace" --report "$run/report.json"
python tools/compat/api_diff.py "$run/native.trace" "$run/adv.trace" --output "$run/diff.json"
```

The build script accepts `-LlvmBin`, `-WindowsSdk`, `-SdkVersion`, and `-VcRuntimeDirectory`.
Launcher `--include`/`--exclude` accept semicolon-separated, case-sensitive globs, such as `kernel32.dll!*;user32.dll!CreateWindow*`.
The working directory defaults to the entry point's directory. `--seconds` accepts 1–3600 and defaults to 20.
`--max` defaults to 50000; zero disables the record limit. This limit counts calls entering capture; unfinished calls can lack completion rows.
Expiration closes the launcher's job and reclaims its process family without terminating other session processes.
Forced termination can lose records from the final flush interval.

Each run directory keeps `cmd.txt` with complete commands, environment variables, and SHA-256 hashes of the frozen runner and entry EXE.
Also record the native capture DLL's hash. Leave original game directories untouched; Hitode uses a fresh overlay.
Retain required evidence and frozen binaries, then remove only this task's build cache.

## Logs and comparison

Ordinary rows contain Tab-separated `thread  0  module!name  args  ret  sequence`; the first five columns match Hitode.
The first column is the logical thread number (main thread: 1); the last is that thread's entry-call sequence number.
Rows remain ordered by completion. There is no cross-thread ordering; equal sequence numbers on different threads do not imply simultaneity.
Native frame numbers are always zero and are excluded from comparison.

Signatures cover kernel32, user32, gdi32, advapi32, winmm, ole32, and registered ntdll interfaces.
Exports are hooked in advance, capturing both static imports and later `GetProcAddress` calls.
Variadic and aggregate-return signatures are excluded. Missing exports, shared alias entry points, and unhookable functions produce comments.
`# hook` lists installed hooks. Only adv calls within that set are compared; other calls count as `outside_native_coverage`, not equivalence.

Integers use hexadecimal; BOOLEAN and narrow return values are truncated to their actual widths.
Handles retain values; ordinary pointers become `ptr`/`null`.
LPARAM/WPARAM/LRESULT payloads and window/class extra-word slots can contain pointers; they are categorized conservatively, so embedded integers cannot be checked.
A/W strings use `s8:` hexadecimal bytes and `s16:` UTF-16 units, capped at 128 units; unreadable or longer strings are marked.
Arguments are captured at entry; output buffers are not expanded.
The capture DLL excludes itself and Windows system modules loaded during initialization, prevents reentry with TLS, and preserves LastError.
A locked shared buffer flushes at 64 KiB; a background worker flushes every 250 ms. Capture changes timing and cannot support performance comparisons.

The comparator maps handles and TLS/FLS slots to first-observed identities, allowing different numbers introduced by intervening calls.
Closing starts a new lifetime. Pointers and strings become categories; nondeterministic times, process/thread identities, and path lengths are removed.
Null, failure, and insufficient-buffer states remain. Nt/Zw user-mode names merge according to the
[Windows user-mode Nt/Zw contract](https://learn.microsoft.com/en-us/windows-hardware/drivers/kernel/using-nt-and-zw-versions-of-the-native-system-services-routines).
Public dynamic symbols can be compared using native strings and adv `# resolve` comments; arbitrary string contents never enter reports.

Threads pair by first appearance within each process, with the main thread first.
Adv's initial section without a `# process` prefix joins the default initial PID, preventing the parent from splitting into two processes.
Other generators can use `--adv-root-pid <hex>`.
Async workers might not correspond; select explicitly with `--native-thread 1:2 --adv-thread 1:3`. Both selectors are required together.

```powershell
# Focus on one API family using existing traces; no recapture is needed.
python tools/compat/api_diff.py "$run/native.trace" "$run/adv.trace" `
    --include 'kernel32.dll!CreateFile*' --include 'kernel32.dll!ReadFile' `
    --native-thread 1:1 --adv-thread 1:1 --context 3 --limit 10 --output "$run/files.json"

# Only ignore calls after their noise has been explained.
python tools/compat/api_diff.py "$run/native.trace" "$run/adv.trace" `
    --ignore '*!RtlEqualUnicodeString' --ignore '*!NtDelayExecution' `
    --window 24 --context 3 --limit 10 --output "$run/reviewed.json"
```

`--ignore-file` accepts one glob per line and `#` comments.
`--no-default-ignore` disables default exclusions for time, identities, diagnostics, and runtime-internal window procedures.
Alignment uses bounded lookahead; long polling sequences remain insertions/deletions unless explicitly ignored.
`value` identifies argument/return differences; `native_only`, `adv_only`, and `order` identify ordering candidates.
`capture_tail` means unequal capture lengths, not a missing call.
Reports include every group count and the first `--limit` contexts; call numbers include filtered rows.
`unobserved` marks string contents, pointed-to data, and unknown signatures, preventing unobserved content from counting as verified.

## Limitations, privacy, and validation

The tools support x86/WoW64 within one Windows session.
Before a suspended process has a module list, remote threads use the matching-bitness KnownDll layout; cross-session and cross-bitness operation are unsupported.
Initialization runs outside `DllMain`; remote threads first undergo Windows DLL/TLS initialization, so capture omits part of native startup.
System modules loaded during initialization can add noise; preloading observed modules changes enumeration results.
COM vtables, unregistered ABIs, variadics, aggregate returns, and output structures are not captured.

Children receive only `# child_untraced`, including standard and Internal/AsUser creation functions with PROCESS_INFORMATION signatures.
They remain in the job without follow-up injection. Process families and unmatched threads require partial-comparison labels.
Dynamically generated code, private exports, shared alias entries, exceptions, and non-returning NT calls can escape capture.
ExitProcess/ExitThread produce termination comments. Handle pairing, repeated functions, and calls without string contents can be ambiguous; output requires review.

Traces, strings, logs, game copies, screenshots, dumps, overlays, generated sources, and binaries stay local; never commit or upload them.
JSON contexts contain only API names, normalized scalars/categories, and anonymous identifiers.
They exclude raw pointers, paths, arbitrary strings, process/thread IDs, and guest instruction locations; public API symbols from metadata are allowed.
If a game rejects injection, record that result and choose another sample without modifying protection, DRM, or packing logic.

```powershell
$env:ADV_API_DIFF_BUILD = "$run/build"
python -m unittest discover -s tools/compat -p 'test_*.py' -v
python tools/check.py
```
