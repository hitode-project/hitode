# Android execution acceptance tools

Validation ownership (2026-10-06): actual game performance is tested exclusively
by the user in headed mode. Agent runs cover ARM compatibility and correctness;
headless samples are limited to necessary functional or numerical diagnostics.
Existing timing and cold/warm tools do not authorize agent-run game performance
matrices, and headless measurements do not establish real gameplay performance.

These Windows-host tools build and run the execution layer without game assets.
Set `ANDROID_HOME` and `ANDROID_NDK_HOME` to the installed SDK and NDK; the tested
toolchain is NDK r28c, cargo-ndk 4.1.2, Rust 1.98.1 and JDK 21. The Unicorn test
oracle additionally uses the existing MSYS shell at `C:/msys64/usr/bin/sh.exe`.
Test-only Unicorn never enters an application's dependency graph.

## CPU tests in the adb shell

```powershell
./tools/android/run-cpu-tests.ps1 -Serial '<device-serial>'
./tools/android/run-cpu-tests.ps1 -Serial '<device-serial>' -TestName @() -UnitFilter virtual_memory::tests
```

The default integration targets are `execution_contract`, `host_interrupts`,
`loop_versioning` and `address_space`. Pass `-TestName` to select other existing
targets. Tests run serially with a bounded device timeout; `-AllTests` and
`-NoFailFast` retain all target results before returning failure. Results include
executable hashes and device page size. Use `-Output` for the task's evidence
directory. The runner cleans up its uniquely named device directory in finally.

Shared devices must reuse the existing PATH adb server on port 5037; never start, stop, or replace it.
Serialize device tasks using the user's atomic directory lock; wait before retrying infrastructure errors.
When `-Serial` is omitted the runner requires exactly one authorized device.

`-AllTests` builds all integration targets and unfiltered unit tests, continues after recording failures, and ultimately returns failure.
Complete targets with zero tests after platform exclusions are skipped; filters matching zero tests fail.
Device test directories are cleaned in finally. Devices use a64; Cranelift builds only on x86_64.
TMPDIR and test targets use the run's unique device directory and are cleaned in finally.
Callers acquire shared devices through the coordinator's `Request-Device` and call `Release-Device` in finally; the CPU runner does not schedule devices.
`-BuildOnly`/`-WhatIf` do not acquire devices.
First cross-build with `-BuildOnly`, then use `-SkipBuild` for multiple ELF configurations within the same isolated target directory.
Every run verifies unique artifacts and records all SHA-256 hashes. Rebuild after source changes; this option must not bypass updates.
Push unique Cargo filenames in batches. Idempotent queries/pushes/chmod/cleanup retry after 30 seconds on connection failure.
Tests retry only after confirmed connection failure before execution.
`-DeviceBudgetSeconds` defaults to 1100 seconds; exhaustion saves completed targets and cleans up without classifying infrastructure errors as test failures.

Replay accepts `--compile-threads 1..8`, corresponding to experimental `ADV_COMPILE_THREADS`; leaving it unset preserves the thread budget.
`ADV_ARTIFACT_ROOT` can direct replay and summary outputs to the task's local temporary directory; the default is ignored `target/`.
`--record-device-state` records battery temperature, screen, foreground window, and shell CPU ranges after asset verification, before guest startup, and after completion.
Callers arrange cooling and shared locks; raw state stays local.
`--cpu-mask <hex-mask>` restricts guest and compiler-thread affinity because adb-shell and foreground-app CPU availability can differ.
Summaries bind this mask and the report's actual available CPU count.

The default isolated build cache is `target/android-a0`. cargo-ndk consumes
Cargo's artifact JSON, so the runner requires one executable hash per selected
test in this directory. It rejects ambiguity; use a fresh `-TargetDir` when
mixing build configurations rather than guessing which binary to execute.

`arm64-v8a.toolchain.cmake` supplies the ABI and QEMU configuration needed by
the reference engine's CMake build. `adv-unicorn/build.rs` supplies Android's
pthread stub and compiler-rt link paths.

## A/B device acceptance

Run from repository-root PowerShell after setting `ANDROID_HOME` and `ANDROID_NDK_HOME` and installing the `aarch64-linux-android` Rust target and cargo-ndk.
Retain the default test selection above; these explicit presets build the unit-test ELF once, then run A and B.

```powershell
# Without SDK, NDK, or devices: print build arguments, device commands, and child-environment cleanup.
./tools/android/run-cpu-tests.ps1 -ExecutionExperiments -WhatIf
./tools/android/test-run-cpu-tests.ps1

# Cross-check the library; set the NDK target C compiler/archiver in the PowerShell child process.
$ndkBin = Join-Path $env:ANDROID_NDK_HOME 'toolchains/llvm/prebuilt/windows-x86_64/bin'
$env:CC_aarch64_linux_android = Join-Path $ndkBin 'aarch64-linux-android29-clang.cmd'
$env:AR_aarch64_linux_android = Join-Path $ndkBin 'llvm-ar.exe'
cargo check --target aarch64-linux-android -p adv-cpu --lib

# Cross-build/link only, including Unicorn; no adb invocation or device acceptance.
./tools/android/run-cpu-tests.ps1 -ExecutionExperiments -BuildOnly -Output target/investigations/android-ready/build

# Complete device acceptance; omit -Serial only when exactly one device is authorized.
./tools/android/run-cpu-tests.ps1 -ExecutionExperiments -Serial '<device-serial>' -Output target/investigations/android-ready/device
```

Commands for individual checks follow. `-UnitFilter` also accepts multiple filters; each runs in a separate process with its own log.

```powershell
# A: real protection windows, mapping transactions, mixed pages, and five parent-controlled SIGSEGV child fixtures.
./tools/android/run-cpu-tests.ps1 -Serial '<device-serial>' -TestName @() -UnitFilter virtual_memory -Output target/investigations/android-ready/A
# B: scalar fault recovery, prefixes/continuations, and twelve parent-controlled recovery/original-handler-chain child fixtures.
./tools/android/run-cpu-tests.ps1 -Serial '<device-serial>' -TestName @() -UnitFilter fault_recovery -Output target/investigations/android-ready/B
```

The device unit-test ELF launches child fixtures directly through `current_exe`, without the host Cargo runner or individual adb launches.
A's `fault_child` stays ignored; only its parent supplies `--exact --ignored` and `ADV_CPU_DIRECT_PROBE`, then strictly verifies SIGSEGV.
B's parent sets `ADV_CONTROLLED_FAULT_CASE` and checks successful exits individually.
The runner clears both variables and Linux/Android Cargo runner variables at top level, preventing parent bypass or recursive host-tool launches.
Device timeout covers the entire process group without `--foreground`. Do not add `--ignored` to ordinary acceptance runs.
Like the direct-memory/PCM backends, B's SIGBUS fixture uses Android's `memfd_create` syscall, avoiding the libc symbol requiring API 30; builds target API 29.

`build.json` records cross-linked artifacts; `device.json` records device, API, and actual page size.
`results.json` records each filter's exit code, pass count, and ELF SHA-256; `cmd.txt` records commands, environment, and runner/ELF hashes without guest EXEs.
Zero passes, nonzero exits, and missing or ambiguous ELFs fail. Raw logs stay local.
Reusing `-Output` overwrites matching log names; use distinct run directories for repeated acceptance.

References: [Android ABI](https://developer.android.com/ndk/guides/abis),
[Android 16 KiB page requirements](https://developer.android.com/guide/practices/page-sizes).

Run the complete command on ARM64 devices with each actual page size.
Simulating 16 KiB groups on 4 KiB devices or aligning ELFs does not establish real 16 KiB acceptance.
These tests run in **adb-shell native processes**; B verifies an owned original-handler chain without loading ART.
Cross-builds, dry runs, emulators, and QEMU are not physical-device results.

## Shared PCM and portable fonts

```powershell
./tools/android/run-cpu-tests.ps1 -Serial '<device-serial>' -TestName shared_audio -UnitFilter shared_memory
cargo ndk -t arm64-v8a --platform 29 test -p adv-device --release --test text_portable --no-run --target-dir target/android-a0
```

Run the resulting `text_portable-*` ELF in an owned adb-shell directory with a
bounded timeout. Its embedded font fixture exercises glyph geometry, raster
packing, metrics, resources, transforms and pixel formats without proprietary
fonts. The optional system-font case also exercises Japanese Unicode/DBCS output
when a Japanese face exists.
These checks do not open a DAC.

## Fixed game input and frame capture

Stage B adds `adv-scene-v1` schedules alongside the existing timed lists. See
the B report for tested scope. A scene
contains ordered `command`, `delay_ms`, or `checkpoint` steps. Checkpoints use:

```json
{"checkpoint":"chapter","await":{"frame_fnv1a64":"a6968230ebfbf74d","timeout_ms":30000,"hold_key":17},"reset_metrics":true}
```

`await-frame` arms before holding the key and resuming. Matching a complete raw
BGRA frame releases the held key and freezes the guest before ACK/report/capture.
Timeout also releases and freezes, but permanently fails the run. Optional
`probes` (`x`, `y`, `bgra`) only prefilter the full hash. PNG exports force alpha
to 255; raw frames may have zero or mixed alpha, so neither raw hashes nor probe
alpha can be inferred from an exported PNG alone. Use a frozen report as truth.
`current:true` can accept an already-present setup screen, but cannot hold a key.

Scene runs require `--font-profile DEVICE_PATH --font-manifest LOCAL_JSON`.
The manifest has `profile:{path,sha256}` and `files:[{path,sha256,bytes}]`.
All deployed assets, seed files, fonts, codepage and scene steps are bound to the
reference; reused device assets must each match a local counterpart. Local files
not deployed are excluded. No game, save or font files are checked into the repo.

`--record-scene-reference OUTPUT/reference.json` records observations only.
Inspect frozen images and assertions, then approve the reference independently;
use `--scene-reference` in a separate run to validate it. `--cold` removes only
the new overlay's JIT archive; `--warm-profile FILE` installs a specific archive.
Both retain the same save seed. The resulting archive is pulled into the output.
On arm64 devices the engine is a64, which writes neither archive, so cold and
warm runs coincide and no archive is expected as evidence.

`--capture-pcm` records bounded DirectSound Play snapshots and excludes the run
from performance comparisons. Compare them with `codec-pcm-compare.py --reference
RUN_A --candidate RUN_B --trace audio.log --max-error N --output RESULT.json`.
Choose the threshold from the codec contract; static Verify and dynamic Native
are different validation paths. This is source PCM, not mixer or DAC output.
Use `summarize-baseline.py RUN... --output RESULT.json --table TABLE.md` only for
approved, otherwise identical cold/warm runs without capture or font diagnostics.
It retains per-run percentiles and reports compile, wait and gate costs separately.

```powershell
cargo ndk -t arm64-v8a --platform 29 build -p adv-run --release --target-dir target/android-a0
python tools/android/replay-sample.py --adb "$env:ANDROID_HOME/platform-tools/adb.exe" --serial '<device-serial>' --binary target/android-a0/aarch64-linux-android/release/adv-run --game-dir '<authorized-local-game-directory>' --exe-relative nana_chs.exe --codepage 936 --schedule tools/performance/scenarios/nana-play.json --output target/arm-samples/nana
```

Use `--adb-port` if needed. The script copies assets into a fresh device directory
and uses a separate write overlay. Subsequent runs can reuse the printed/session
`remote_game_directory` via `--remote-game-dir`; it is never deleted or modified
by the tool. Local output must be under ignored `target/`. Each input waits for
the actual control acknowledgement; checkpoints retrieve JSON and PNG. Strict
API eligibility, nonzero frames, failures, executable hashes and remote exit
status are checked. Scene progress and text layout still require inspecting PNGs.
Device execution has an independent timeout even if the adb client disconnects.

For Majiro, use the Japanese executable, CP932, `--no-tiering`, a copied v10 seed
via `--seed-data`, and `tools/performance/scenarios/majiro-v10-portable.json`.
The portable schedule preserves load/settings/short Ctrl/advance operations with
named observations; it is a timed replay, not the historical seven-hash gated
throughput benchmark. Remove `*.fcd` only from the copied seed to verify new font
rasterization instead of replaying a Windows glyph cache. Keep a manifest of the
original and modified seed. No game data or font files belong in Git.

`--font-diagnostics` records requested/resolved font, source and size in
`stderr.log`; use it for layout acceptance, not timing comparisons. `ADV_FONT_DIR`
can supplement platform fonts, while `Text::with_font_directories` isolates font
discovery for API tests. Guest-added fonts take precedence for matching families.
Android font substitutions need their own layout reference; Windows GDI frame
hashes are not automatically portable. The seven-color sample's existing 165 s
script may reach a choice around 90 s, so later Ctrl receipts alone do not prove
continued story progress.
