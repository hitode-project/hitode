# Configuration

This page lists every environment variable, `adv-run` option and behavior-selecting Cargo feature, with its default. A change that adds or removes one updates this page in the same commit. Path values and captured output stay local.

Kinds:

- **Feature** changes what the program can do. New features are on by default or not merged; turning one on needs benefit data and no growth in the known-failure list.
- **Comparison** selects an old or new implementation during a migration. Each names the point at which it is removed.
- **Diagnostic** only observes or checks.
- **Fixture** controls self-written tests and the conditions they run under.

## Launcher upscaling

The global filter offers Off, Anime4K, CuNNy, ArtCNN and k7_modernAnime; Off is the default.
This user-selected image treatment is an approved exception to the default-on feature rule.
Each game inherits the global choice unless it has an override, including an explicit Off choice.
Global preferences use `adv-launcher.prefs`; overrides use `adv-launcher.upscaling` in webview local storage.
The game overlay changes the override immediately. Filtering applies only when the physical viewport enlarges the game canvas.
The models are Anime4K Upscale Denoise L, CuNNy2 4x12 NVL, neutral ArtCNN C4F16 and k7_modernAnime FHD x2.
Each runs once at 2x; linear resampling fits the result to the viewport.
With upscaling off, native GPU presentation uses sharp-bilinear sampling: integer enlargement retains hard pixels, while fractional enlargement blends pixel boundaries.

## Launcher runtime and library settings

`adv-launcher.prefs` stores interface language (`auto`, Chinese, Japanese or English), playtime tracking (on), background pause (on), Ctrl behavior (hold), FPS display (off), volume (70%) and mute (off).
Per-game settings in `adv-launcher.game-settings` override proportional scaling with integer or stretch modes and Ctrl hold with toggle mode.
Font replacements, smoothing and up to four ISO images on D: through G: apply on the next launch; font and disc defaults follow the game and mount nothing.
Volume, mute, scaling and FPS controls in the game overlay apply immediately.
Snapshots include the independent game data directory and registry. Automatic backups default to on with five retained copies; retention choices are three, five or ten and exclude manual backups.

The import page lists VNDB candidates for the game and links only the one the user picks; the default is no link. Searching accepts names, `v` identifiers and VNDB visual novel links, ten candidates per page.
Searching automatically at import stays off until the user answers a one-time prompt on the first import; dismissing it keeps search manual. The choice lives in `adv-launcher.prefs` and the settings switch changes it. Only game names are sent.
Metadata and cached thumbnails persist in `library.json`; refreshing metadata preserves the independently editable display name.
Unlinking leaves the display name and game data intact and disables automatic reassociation for that entry.

## Environment variables

Header macros, placeholder names in script examples and names read only to clear old environments are not listed. Defaults come from the code that reads each variable.

| Name | Kind | Default |
|---|---|---|
| `ADV_ABI_AUDIT_PATH` | Fixture | Unset: run the parent test |
| `ADV_ANDROID_SHELL_BUILD` | Fixture | Build-time label `unrecorded` |
| `ADV_API_DIFF_BUILD` | Fixture | Unset: skip the local build check |
| `ADV_API_DIFF_CHILD` | Fixture | Unset; the fixture passes it |
| `ADV_ARTIFACT_ROOT` | Fixture | The repository `target` |
| `ADV_AUDIO_CAPTURE` | Fixture | No output; used only by `audio_transition_probe` |
| `ADV_AUDIO_CAPTURE_CALLBACK` | Diagnostic | No output |
| `ADV_AUDIO_DUMP_CHUNKS` | Diagnostic | Off; on when set |
| `ADV_AUDIO_DUMP_DIR` | Diagnostic | No output; needs `ADV_AUDIO_TRACE` |
| `ADV_AUDIO_MUTE_DEVICE` | Diagnostic | Off; `1` mutes the physical output |
| `ADV_AUDIO_TRACE` | Diagnostic | No output |
| `ADV_BENCH_CPU` | Fixture | `0` (Windows synthetic memory benchmark) |
| `ADV_BISECT_EXE` | Fixture | None; required for the explicit backend comparison, with the game chosen through `ADV_SAMPLES` |
| `ADV_BISECT_BACKENDS` | Fixture | `interpreter,unicorn`; `jit` optional |
| `ADV_BISECT_SECONDS` | Fixture | `10` (1 to 120), the outer deadline per backend |
| `ADV_BISECT_CODEPAGE` | Fixture | The runtime default code page |
| `ADV_BISECT_WORKER`, `ADV_BISECT_DATA`, `ADV_BISECT_BACKEND`, `ADV_BISECT_HASH_ITEM` | Fixture | Unset; child protocol set by the parent test |
| `ADV_CODESIGN_IDENTITY` | Feature | Unset: `tools/macos/package.py` signs ad hoc |
| `ADV_COMPILE_THREADS` | Diagnostic | Automatic 1 to 3, up to 4 on desktops with many cores; override 1 to 8 |
| `ADV_CONTROLLED_FAULT_CASE` | Fixture | Unset; set by the parent test |
| `ADV_CPU_DIRECT_PROBE` | Fixture | Unset; set by the parent test |
| `ADV_DEBUG` | Diagnostic | Off; `1` serves the interactive debugger, as `adv-run --debug` |
| `ADV_DETOURS_SOURCE` | Fixture | Unset: no local Detours checkout |
| `ADV_DEVICE_HELPER` | Fixture | Unset; device tools require it when needed |
| `ADV_DEVICE_TASK` | Fixture | `bud` (frame budget tool) |
| `ADV_DLL_DRIVER_RUNNER` | Fixture | Unset: skip the runner check |
| `ADV_ENGINE_BENCH_ENGINES` | Fixture | Both the interpreter and the platform engine |
| `ADV_ENGINE_BENCH_SCALE` | Fixture | `100` percent of the default iterations (1 to 1000) |
| `ADV_ENGINE_BENCH_WORKLOADS` | Fixture | All workloads |
| `ADV_ERROR_MODE_NATIVE_TEST_CHILD` | Fixture | Unset; set by the parent test |
| `ADV_FIXTURE_ENV_83F2` | Fixture | Set and cleared by the fixture |
| `ADV_FIXTURE_OTHER_83F2` | Fixture | Deliberately undefined variable read by the fixture |
| `ADV_FONT_DIR` | Feature | System font folders only |
| `ADV_FONT_PROFILE` | Fixture | No controlled font profile |
| `ADV_GPU_BACKEND` | Migration comparison | Unset: native Metal programmable draws on Apple ARM; `wgpu` forces the reference path. Remove when R3-B completes. |
| `ADV_GIR_CANDIDATES` | Fixture | None; required by the explicit corpus test |
| `ADV_VERIFY_SOFTWARE_FRAMES` | Diagnostic | Off; any value compares incremental software storage and conversion against full-frame references |
| `ADV_GIR_CAPTURE` | Diagnostic | No output; needs the `gir-capture` feature |
| `ADV_GIR_CORPUS` | Fixture | None; required by the explicit corpus test |
| `ADV_GIR_FAULT_CHILD` | Fixture | Unset; set by the parent test |
| `ADV_GIR_LIFT` | Diagnostic | Off; needs the `gir-lift` feature |
| `ADV_GIR_LIFT_CORPUS` | Fixture | No private corpus; self-written regions only |
| `ADV_GIR_NATIVE` | Diagnostic | No output |
| `ADV_GIR_OUTPUT` | Fixture | None; required by the explicit corpus test |
| `ADV_HOST_FEATURES` | Diagnostic | Every detected feature; can only turn features off |
| `ADV_HOST_FEATURES_REPORT` | Diagnostic | Off |
| `ADV_INTERP_PROFILE` | Diagnostic | No output |
| `ADV_INTERP_TRACE_DIR` | Diagnostic | No output |
| `ADV_INTERP_TRACE_MAX` | Diagnostic | `4096`, at most 100000 |
| `ADV_INTERP_TRACE_OPERANDS` | Diagnostic | Off |
| `ADV_INTERP_TRACE_RANGES` | Diagnostic | No ranges; set with `ADV_INTERP_TRACE_DIR` |
| `ADV_INTERP_TRACE_START` | Diagnostic | No delay; when set, observation starts at that PC |
| `ADV_INTERP_TRACE_WATCH_DWORD` | Diagnostic | None; watches one DWORD at interpreter dispatch boundaries |
| `ADV_INTERP_TRACE_STACK_WORDS` | Diagnostic | `4` (0 to 8) |
| `ADV_IOS_JIT_AT_LAUNCH` | Fixture | Unset; `enable`, `ddiStatus` or `downloadDdi` runs that JIT helper task at launch |
| `ADV_JIT_DUMP_DIR` | Diagnostic | No output |
| `ADV_JIT_DUMP_REGIONS` | Diagnostic | All regions; needs `ADV_JIT_DUMP_DIR` |
| `ADV_JIT_IR_COUNTS` | Diagnostic | Off |
| `ADV_JIT_MAP_DIR` | Diagnostic | No output |
| `ADV_JIT_VERIFY` | Diagnostic | Off; `1` or `strict` |
| `ADV_JIT_X87_HELPER_ONLY` | Diagnostic | Off; guarded native x87 emission allowed |
| `ADV_JIT_X87_SINGLE_ONLY` | Diagnostic | Off; resident x87 runs allowed |
| `ADV_LOOP_COVERAGE` | Diagnostic | Off |
| `ADV_MMIO_TRACE` | Diagnostic | No output |
| `ADV_NATIVE_CACHE_CHILD` | Fixture | Unset; the parent test passes a local synthetic archive |
| `ADV_NATIVE_CACHE_REJECT_BINARY` | Fixture | Unset; checks that another host binary is rejected |
| `ADV_PERF_MAP_DIR` | Diagnostic | No output |
| `ADV_PERSONALITY_STRICT` | Comparison | Off: only implemented and reviewed exports are discoverable. Removed when the dynamic export policy comparison ends |
| `ADV_PRINTF_DIAG_PATH` | Diagnostic | No output |
| `ADV_PRINTF_DIAG_WORDS` | Diagnostic | Empty filter; needs `ADV_PRINTF_DIAG_PATH` |
| `ADV_PRIVACY_TERMS` | Diagnostic | The local `.privacy-terms`, if present |
| `ADV_PRIVATE_ENV` | Fixture | Data in a self-written guest environment block only |
| `ADV_REGION_BENCH_CATEGORY` | Fixture | All categories |
| `ADV_REGION_BENCH_COMPILE` | Fixture | Off |
| `ADV_REGION_BENCH_COMPILE_REPEATS` | Fixture | `9` |
| `ADV_REGION_BENCH_EXPORT_CORPUS` | Fixture | No output |
| `ADV_REGION_BENCH_LOOPS` | Fixture | `10000` |
| `ADV_REGION_BENCH_PRIVATE_LIMIT` | Fixture | `3` |
| `ADV_REGION_BENCH_REPEATS` | Fixture | `4` |
| `ADV_REGION_BENCH_SAMPLES` | Fixture | `7` |
| `ADV_REGION_BENCH_TIER0_ONLY` | Fixture | Off |
| `ADV_SAFEPOINT_BENCH_LOOPS` | Fixture | `200000` |
| `ADV_SAFEPOINT_BENCH_SAMPLES` | Fixture | `7` |
| `ADV_SAFEPOINT_BENCH_TIER` | Fixture | `2` |
| `ADV_SAMPLES` | Fixture | Unset: tests that need games skip |
| `ADV_SECURE_COPY_ORACLE_CHILD` | Fixture | Unset; set by the parent test |
| `ADV_SHELL_DPI_TEST_CHILD` | Fixture | Unset; set by the parent test |
| `ADV_SHELL_LOG_TEST_CHILD` | Fixture | Unset; set by the parent test |
| `ADV_STARTUP_API_MODE` | Fixture | The runtime default mode |
| `ADV_STARTUP_GPU` | Fixture | Off |
| `ADV_TEST_AUTHORED_EFFECT_FXO` | Fixture | None; skips the comparison with locally compiled output |
| `ADV_TEST_FFMPEG` | Fixture | `ffmpeg` |
| `ADV_TEST_GPU_BACKEND` | Fixture | Automatic; `vulkan` or `gles` |
| `ADV_TEST_ISO` | Fixture | None; a read-only image must be given |
| `ADV_TEXT_DIAGNOSTICS` | Diagnostic | Off |
| `ADV_TIER1_CENSUS` | Diagnostic | Off; census of x87 exact-path forms |
| `ADV_VERIFY_FRAMES` | Diagnostic | Off; `1` executes every D3D9 frame and asynchronously compares pixels outside computed damage with the previous readback |

The `adv-dx/d3d9-counters` Cargo feature adds a D3D9 work report: retained
recording bytes and lookup operations per draw, changed entries per StateBlock
Apply, and eliminated dead frames/passes. It is diagnostic only. Builds without
the feature compile out the instrumentation. Lookup counts cover resource
identity resolutions and current/base content-version table probes in recording;
they exclude backend work and damage-history bookkeeping.

## adv-run options

`Options::parse_args` in `adv-shell` is authoritative; `adv-run --help` prints the same options. The library interface and the Android app configure themselves separately. Options that pick values of one capability share a row.

| Option | Kind | Default |
|---|---|---|
| `--exe` | Feature | Required; also accepted as a positional argument |
| `--backend` | Diagnostic | `jit` (Tier 0 with the platform engine); `interpreter` optional |
| `--row-workers` | Feature | Automatic, at most 3 helpers; `0` runs rows inline |
| `--no-row-batching` | Diagnostic | Absent: proven batches enabled |
| `--api-mode` | Diagnostic | `discovery`: unimplemented APIs return their derived failure and are reported; `strict` stops at the first, for acceptance |
| `--no-time-skip` | Diagnostic | Absent: Ctrl time skip allowed when its conditions hold |
| `--compile` | Diagnostic | `fast`; `speed` optional |
| `--no-tiering` | Diagnostic | Absent: Fast to Speed promotion allowed |
| `--no-region-linking` | Diagnostic | Absent: region linking enabled |
| `--sync-compilation` | Diagnostic | Absent: background compilation |
| `--codepage` | Feature | `auto`: 936 for marked or mostly GB2312 executables, otherwise 932; `932` and `936` override |
| `--headless` | Fixture | Off: a host window is shown |
| `--codec` | Diagnostic | `native`; `guest` for comparison |
| `--codec-kernels` | Diagnostic | `native`; `guest` or `verify` compare Vorbis kernels and admitted Theora decoders |
| `--codec-signatures` | Feature | Vorbis x87/SSE signatures; external records use `---`. Theora family fingerprints are bundled independently |
| `--audio` | Feature | Off; headless runs simulate the device. To become default or be removed with the normal-play audio baseline |
| `--hidden` | Fixture | Off |
| `--gpu-software` | Diagnostic | Off: the adapter is chosen automatically |
| `--headless-gpu` | Fixture | Off; needs `--headless`, and a `gpu` build except on macOS |
| `--d3d9-sm3` | Feature | Off. To become default or be removed at capability acceptance |
| `--d3d9-shader-cache` | Host path override | Defaults to `--data-dir/artifacts/gpu`; stores bounded WGSL, portable pipeline recipes and supported native driver caches |
| `--width` | Feature | `640` |
| `--height` | Feature | `480` |
| `--frames` | Fixture | No frame limit |
| `--seconds` | Fixture | No wall-clock limit |
| `--data-dir` | Feature | `target/adv-data/<exe>` |
| `--no-code-cache` | Diagnostic | Absent: persistent machine code and budgeted ahead-of-time compilation; for troubleshooting only |
| `--cdrom` | Feature | None; repeat to mount read-only ISO images |
| `--install-dir` | Feature | None: `C:\Program Files` lives in the data folder |
| `--report` | Diagnostic | No output |
| `--screenshot` | Diagnostic | No output |
| `--trace` | Diagnostic | No output |
| `--host-profile` | Diagnostic | No output |
| `--survey` | Diagnostic | No output |
| `--schedule-profile` | Diagnostic | No output |
| `--auto-dialog-ms` | Fixture | No automatic dialog choice |
| `--self-test-input` | Fixture | Off |
| `--profile` | Diagnostic | Off; turning it on disables region linking |
| `--frame-budget` | Diagnostic | Off; headless with one guest worker only |
| `--control-stdin` | Diagnostic | Off |
| `--help`, `-h` | Diagnostic | Help not shown |

The default-off features above conflict with the default-on rule; each must become default or be removed.

Wine sessions apply the shared engine and diagnostic settings, shader capabilities and cache location,
font files, family substitutions and smoothing, frame and time limits, and audio output selection.
`--report` writes the final stop reason and full guest exit code even on a guest failure;
control snapshots retain `running` until execution stops. CPU, host and scheduling profiles use
the same writers as the built-in stack. Ctrl acceleration uses the shared live key policy for
eligible delays and stays disabled once a child process has been created.

## Cargo features

No feature selects execution behavior: the engine follows the target architecture. Platform and device build capabilities such as `gpu` and `media` are described in [building.md](building.md).

Host service CSV diagnostics also count successful guest bodies, ordinary fallbacks and leaf calls per API, with guest/leaf fractions. Guest probes are installed only while host profiling, frame timing or frame-budget diagnostics are active; disabled bodies and fallback jumps contain no counter instructions. The final report always derives non-leaf host calls from existing total and leaf counts.

### AArch64 compact window

`ADV_A64_COMPACT_WINDOW=1` runs the iOS guest window layout on other hosts: guests get 2 GiB, the always-accessible alias sits in the window's upper half, and all processes of a family share one window, mapping their memory into it in turn. iOS always uses it. It is read once, before the first window is created.

### AArch64 prepared code pool

`ADV_A64_CODE_POOL_MIB` sets the fixed host-process code pool in MiB (default 512; 128 on iOS, where the JIT debugger prepares every page). It is read once before the first translated guest runs; each process leases 64 MiB. On iOS, a short pool first halves the lease down to 16 MiB; full exhaustion or an invalid/unavailable pool falls back to Tier 0 with a capability warning; other hosts map overflow caches separately. Hosts may call `adv_cpu_a64::prepare_code_pool()` before creating machines to prepare executable memory explicitly. This does not change the x86_64 engine.

Tier 0 baseline reports include `tier0_instructions` and `tier0_instructions_per_second` when `--profile` or `--frame-budget` enables exact retirement counting. Otherwise these fields are null. Rates use total host wall time, including waits, and do not treat REP elements as separately retired instructions. The standalone `adv-cpu-core` example `tier0_baseline` prints synthetic-case JSON lines; `--seconds N` sets each case duration and `--iterations N` sets loop work per run.
