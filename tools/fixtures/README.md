# Repository-owned fixtures

## PE fixtures

DLL export driver: [usage and JSON protocol](dll-driver.md).
Build with `./tools/fixtures/build-dll-driver.ps1`; `tools/performance/dll_exports.py` compares scalar results, memory hashes, and loop costs for identical scripts.
It runs on native Windows, the adv-run interpreter, and JIT.

The patch compatibility fixture is built separately:

```powershell
./tools/fixtures/build-patch.ps1
cargo test -p adv-runtime --test patch_compat
```

`fixture_patch_compat.exe` first runs under native 32-bit Windows to produce
`patch-compat.log`; the integration test then compares the entire log with JIT,
the shared-state interpreter, and a dev-only Unicorn backend running the same
PE. It exercises a real PE IAT rewrite,
unmodified upstream MinHook v1.3.4 hooking `GetModuleHandleA`, its original
trampoline with one-argument stdcall cleanup, byte-for-byte hook removal,
`DirectDrawCreateEx` followed by an `IDirectDraw7::QueryInterface` vtable-slot
rewrite that calls the original method, and dynamically loaded DllMain rewriting
a previously executed function from return value 17 to 33. The JIT run asserts
translated-code invalidation, and page-query checks verify writable transitions
and restoration rather than assuming successful `VirtualProtect` calls.

The fixture uses an `asInvoker` manifest, creates no window, and presents no
frame. Sources and the pinned MinHook/HDE license are in
[`vendor/minhook/UPSTREAM.md`](vendor/minhook/UPSTREAM.md). The dependency is
used only for fixture builds. No game files or external binary patches are used.

A second runtime test moves an entire API thunk into an unrelated guest page,
rewrites the original API entry to a guest replacement, and verifies that only
execution of the relocated trap calls the original handler. It checks stdcall
arguments, ESP/EBP/EDI preservation, nested guest callbacks through the return
pad, blocked-call resumption, and cdecl x87 return values on all three executors.
Unicorn is injected through the runtime's generic backend in tests and examples;
it is not a production CLI backend or a JIT fallback.

Build on Windows with LLVM and the Windows SDK import libraries:

```powershell
./tools/fixtures/build.ps1
```

Only C/assembly/DEF sources are checked in. Objects, EXEs, DLLs, import libraries,
reports and PNGs are written under `target/fixtures`. The build needs no CRT and
does not run the compiled EXE directly on Windows.

`fixture_window.exe` statically imports `fixture_probe.dll` and dynamically loads
`fixture_dynamic.dll`. All three request base `0x00400000`, forcing DLL rebasing.
The fixture checks relocated data pointers, name and ordinal exports, TLS
callbacks before DllMain, initialized static TLS, and a new thread's independent
TLS template. A thread sleeps and signals an event before the main thread opens a
window. The main thread draws a BGRA DIB using GDI and dispatches keyboard/mouse
messages. It checks that guest TranslateMessage produces exactly one character
for a queued host keydown. Its timer callback sleeps, resumes and paints a completion mark. The
program destroys the guest window and exits with code zero after injected input.

```powershell
cargo run --release -p adv-run -- target/fixtures/fixture_window.exe --self-test-input --width 320 --height 240 --seconds 10 --report target/fixtures/window.json --screenshot target/fixtures/window.png
cargo run --release -p adv-run -- target/fixtures/fixture_window.exe --backend interpreter --headless --self-test-input --width 320 --height 240 --seconds 10
cargo test --release -p adv-run --test fixtures
```

The visible window displays a color gradient and three small status squares.
The squares turn white for keyboard input, left mouse input and resumed timer
callback. The generated screenshot is deterministic. `--hidden` exercises the
same native window callback, painting and input pipeline without showing a
window. `--self-test-input` uses PostMessageW and the real host message pump in windowed mode and the
guest message queue in headless mode.

`fixture_wait.exe` blocks on an unsignalled event indefinitely. It checks that a
headless shell still respects `--seconds` when there is no guest deadline.
`fixture_close.exe` loops forever in its guest close handler. A hidden host
window receives `WM_CLOSE`; the shell must stop the CPU after its two-second
grace rather than waiting for the longer general execution limit.
`fixture_control.exe` draws a small surface then waits for input indefinitely.
`fixture_compute.exe` adds a worker that never calls a host API or waits. The
same control regression checks input, drawing and closing while that worker
continues computing, and a separate deadline test stops it by host wall time.
`fixture_slice.exe` runs two finite CPU workers, checking exactly 2,000,000
volatile increments each and their final computed values. Unsliced runtime and
the CLI's resumable slices must both produce natural exit zero on JIT and the
interpreter.
The explicit `--control-stdin` entry accepts one command per line: `click X Y`,
`key VK` (decimal or `0xNN`), `capture PATH`, `report PATH`, `reset-metrics`, and `close`.
Coordinates are guest client coordinates; paths use the remaining line and may
contain spaces. Commands acknowledge on stdout as `control ok ...` or
`control error ...`. Click/key ACKs confirm queuing. A guest slice runs between
commands; guest processing may require more slices, so use report/capture to
observe completion. EOF leaves the guest running.
`reset-metrics` starts a new performance observation window at the current guest
frame. Use it after entering the desired scene, then request a report after a
measurement interval. It excludes the first partial frame interval; repeated
presentations of the existing frame do not count as new frames. The report's
`performance` section covers this window, while top-level runtime counters remain
cumulative. Guest time, execution, and input are unaffected.

`fixture_audio.exe` uses guest DirectSound COM calls to fill a mono PCM16 buffer
with a low-amplitude waveform, play it for a second, inspect its moving cursor,
stop and release it. An uncooperative CPU worker forces PCM pumping to run at
soft-slice boundaries. `--audio --headless` validates the model without opening a
sound device. The separately ignored device test checks real output callbacks
and non-silent sample counts; it emits a short quiet tone:

```powershell
cargo test -p adv-run --test fixtures native_audio_output_consumes_non_silent_guest_pcm -- --ignored --exact
```

`native_audio_unlock_probe.exe` runs directly on Windows against its DirectSound
implementation, outside the ADV guest runtime. It records HRESULTs for correct,
output-parameter-address, unrelated-memory and null Unlock pointers, repeated
Unlock, Lock after failed Unlock, and playback while holding a valid Lock. Its
brief playback uses zero PCM, so it emits no tone. Run it directly after building:

```powershell
.\target\fixtures\native_audio_unlock_probe.exe
```

Nonzero guest exit codes identify the failed check: 10–15 concern DLL loading,
relocation and TLS; 16–17 concern thread/event synchronization; 18–21 concern
window/GDI creation or drawing; 22 means the guest closed before both input types;
23 means character translation produced a duplicate or incorrect character.

## Optical media

[`optical.iso`](../../crates/adv-os/tests/fixtures/optical.iso) is an authored 28-sector ISO9660/Joliet test image, with no game
data. It is shared by the VFS and Win32 regression tests.

- Volume label: `OPTICAL_TEST`; logical sector size: 2048 bytes.
- Primary and Joliet descriptors: sectors 16 and 17; terminator: sector 18.
- Primary root: sector 20; Joliet root: sector 21; `Sub`: sector 22.
- Primary little/big endian root path tables: sectors 23 and 24.
- `ReadMe.txt`: sector 25, `hello world` (11 bytes).
- `Sub/日本.txt`: sector 26, `abcde` (5 bytes).
- `Sub/split.bin`: sectors 26 and 27, `abcde world` (5 + 6 bytes).
- Records use October 7, 2026, 12:34:56 UTC+8; identifiers use `;1` versions.

The primary namespace exposes `README.TXT`; the Joliet namespace exposes mixed
case and Unicode names. File extents deliberately share bytes to verify that
the VFS identities belong to file records, rather than only to physical extents.
