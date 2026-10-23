# Architecture

This document is a map of Hitode: what the parts are, how they connect, and where to look next. It describes the code as it is. The rules live in [docs/spec/](docs/spec/README.md), and the design behind each part lives in the module docs and README of its crate. Terms are defined in [docs/glossary.md](docs/glossary.md).

## What Hitode does

Hitode runs the original x86-32 Windows executables and DLLs of classic ADV (visual novel) games on ARM and x86-64 hosts. The goals are to run many engines without per-game setup, to use little power, and to keep fan translation patches working.

The approach is borrowed from console emulators. The game's own code runs unchanged as the *guest*. Hitode provides what that code expects from its "hardware": a subset of Windows, graphics, audio and video devices, time and threads. Where a library call or a hot leaf function has a proven-equivalent native implementation, Hitode may run that instead, as long as the guest cannot tell the difference.

The main targets are games that use GDI, DirectDraw, Direct3D 9, DirectSound and the classic video paths. x64 guests, Direct3D 11 and OpenGL are out of scope.

## Principles

These hold across the whole codebase:

- **Only what the guest can observe.** Hitode implements the system behavior that guest code can see. It does not rebuild the NT kernel or a Windows process.
- **Patches stay possible.** Patches rewrite import pointers, export entries, function prologues and COM vtables. Every fast path checks for such rewrites and falls back to the slow path when they happen. Synthetic system DLLs exist in guest memory, so patches can find and change them like real ones.
- **Correctness first.** The interpreter and the general compatibility layer are always correct. Faster paths must validate their inputs, code identity and invalidation conditions.
- **One owner per responsibility.** One implementation serves every API that needs it. A platform port adapts an existing implementation instead of adding a second one.
- **Platforms are equal.** macOS, iOS and Android share the a64 engine, and an optimization or experience fix lands on all three. Windows remains the reference for sampling and compatibility.
- **Default on or not merged.** A feature is either on by default or stays out of master. Diagnostic and test switches are the only exception.
- **Mature libraries first.** A new dependency must replace long-term maintenance or bring a direct compatibility gain. Test references such as Unicorn never enter the shipped runtime.

## Code map

The workspace is layered. Each layer may depend only on the layers below it, and `tools/layering/check_layers.py` enforces the rules.

| Layer | Crates | Responsible for | Not responsible for |
|---|---|---|---|
| Host shell | `adv-shell`, `apps/adv-run`, `apps/adv-launcher`, `apps/adv-jit-helper` | Host windows, input, audio output, platform lifecycle, reports | Compatibility logic |
| Assembly | `adv-runtime` | Loading and wiring the personality, machine and devices; scheduling a family of guest processes | API behavior |
| Personality | `adv-win32`, `adv-dx` | Parameters, observable behavior and object state of Win32, the CRT and DirectX COM | Drawing, mixing, decoding |
| System services | `adv-os` | Loader, synthetic PE images, PEB/TEB/LDR, virtual file system, registry, handles, COM activation and Windows allocators | |
| ABI | `adv-abi`, `adv-abi-macros` | Signatures, calling conventions, argument decoding, registration, stubs, API diagnostics, COM identities and vtables | API behavior |
| NT platform | `adv-nt` | The NT services under Wine's PE modules in the Wine userland port: image mapping, process startup, the system call and Unix call dispatchers, and the server protocol client | Windows behavior above the NT interface |
| Wine server | `adv-wineserver` | Wine's server built in process for the Wine userland port ([#58](https://github.com/hitode-project/hitode/issues/58), [docs/spec/wine.md](docs/spec/wine.md)): handles, namespace, registry and timers, resumed by its caller with time from its caller | Waiting, time, host processes and signals |
| Unix libraries | `adv-unix` | Wine's Unix libraries, such as `win32u.so`, as guest ELF code: the loader, the host side of `ntdll.so` they import, and the C library parts they use | Windows behavior; host libraries they would load |
| Machine | `adv-machine` | Guest contexts, scheduling, clocks, waits, callbacks, exception and trap dispatch | |
| Execution | `adv-cpu`, `adv-cpu-core`, `adv-cpu-x64`, `adv-cpu-a64` | Running x86 code: CPU state, guest address space, code validity, translation | Anything about Win32 |
| Devices | `adv-device` | Neutral surfaces and drawing, PCM mixing, fonts, audio and video decoding | Win32 handles and window semantics |
| Shared | `adv-sync`, `adv-pe`, `adv-report` | Channels and wakeups; PE parsing and generic image writing; the anonymous compatibility report format | Policy |
| Test only | `adv-unicorn`, `adv-samples` | A reference CPU, locating local samples | Anything shipped |

The dependency arrows, pointing at what is depended on:

```text
adv-shell → adv-runtime → adv-win32 → adv-os → adv-abi → adv-machine → adv-cpu
adv-runtime → adv-dx → adv-abi / adv-device
adv-runtime → adv-unix → adv-nt (the Wine userland)
adv-runtime → adv-wineserver (a Wine session connects the server to adv-nt)
adv-dx → adv-os (transitional: fronts other than DirectDraw and D3D9)
adv-cpu → adv-cpu-x64 (x86_64) / adv-cpu-a64 (AArch64) → adv-cpu-core
adv-win32 / adv-dx / adv-shell → adv-device
adv-cpu-core / adv-machine / adv-device / adv-shell → adv-sync
adv-shell → adv-report;  adv-os → adv-pe;  adv-abi → adv-abi-macros / adv-pe
adv-run / adv-launcher → adv-shell
```

`adv-win32` and `adv-dx` do not depend on each other. DirectDraw and D3D9 use
`adv-abi` for COM identities, interface aliases and reference counts and
`adv-device` for surfaces and GPU work. The assembly supplies D3D9's private guest
allocator and both fronts' memory DC operations (`adv_dx::interop`), window geometry
and display state. DC operations complete through `Outcome` continuations so an
assembly can call guest Windows interfaces. The old stack adapts its heaps and
shared GDI state in `adv-runtime::graphics`; its COM activation stays in `adv-os`.
DirectDraw keeps its private surface arena. Other DirectX fronts retain a
transitional dependency on `adv-os` for heaps, activation, Automation, files and
input state.

For the Wine stack, `adv-abi::dll` writes host-module PE images through `adv-pe` and calls their imports through the machine. `adv-unix::builtin` installs those images in the prefix before process startup. `adv-runtime::wine` assembles the graphics fronts on that stack: our DirectDraw as such an image, with memory DCs from Wine's GDI.

## A frame, from start to screen

1. **Launch.** A front end (the `adv-run` command line, the Tauri launcher or the Android app) creates an `adv-shell` session for the game executable, a data directory and options such as the code page.
2. **Load.** `adv-runtime` asks the `adv-os` loader to map the executable and its DLLs into the guest address space. System DLLs are synthetic images with real export tables, so imports resolve as on Windows.
3. **Run.** `adv-machine` schedules guest threads on one execution owner, and `adv-cpu` runs their code. Code runs first in the interpreter; hot code is translated to host machine code.
4. **Call the system.** A call into a system DLL reaches a trap. `adv-abi` decodes the arguments by the API's signature and calls its front end in `adv-win32` or `adv-dx`. If the API needs a guest callback or has to wait, the machine saves a continuation and resumes it later in the same thread's context.
5. **Draw.** GDI, DirectDraw and Direct3D 9 front ends draw through the neutral surfaces of `adv-device`, with software and GPU backends behind one interface. Apple ARM programmable draws use native Metal with wgpu-owned textures; other GPU work uses wgpu ([GFX-METAL-1](docs/spec/graphics.md#gfx-metal-1-apple-programmable-draws)). A finished frame goes into a mailbox.
6. **Present.** The shell takes the newest finished frame and presents it through wgpu (Metal on macOS). Dropping a host frame never skips a draw, lock or readback that the guest can observe.
7. **Sound and input.** DirectSound and waveOut share one mixer and playback clock, and the host audio callback only reads mixed PCM. Host input events become USER messages in the guest's queues.

## Execution

The `adv-cpu` facade picks one engine per target architecture at compile time:

- **Tier 0** is a predecoding interpreter shared by both engines. It runs cold code, single steps and every instruction without a native form, and it is the oracle for differential tests.
- **a64** runs on every AArch64 host. An entry runs once in a first-visit executor, and a second visit translates a trace. Guest registers live in fixed host registers, and guest memory is accessed directly through a guarded 4 GiB window. Faults recover from side tables. See [docs/spec/a64.md](docs/spec/a64.md).
- **x64** runs on x86-64 hosts. It compiles regions with Cranelift in the background, with GIR as the only Tier 2 front end. It is frozen and only receives correctness fixes. See [docs/spec/execution.md](docs/spec/execution.md).

The main performance targets are ARM macOS and modern Android ARM64. Windows on x86-64 remains the reference platform for sampling and compatibility work.

## Machine

One execution owner runs all guest threads; there is no parallel guest execution. Waits, signals, timeouts and cancellation go through the same machine state. API handlers that need a guest callback save a continuation instead of blocking the host.

The machine owns the virtual clock. Holding Ctrl to skip may advance time to the next deadline only when no context can run, and never completes external I/O early. Several guest processes can run inside one host process, each with its own address space, machine and handles. The rules are in [docs/spec/processes.md](docs/spec/processes.md).

## Personality and compatibility

The personality is Windows 7 SP1. Version APIs and the PEB come from one profile.

Each export is *implemented* (real behavior), *reviewed* (a deliberate, documented failure) or *hidden* (not discoverable). Player runs default to discovery mode, which records a call to a missing stub and returns a failure derived from its signature; strict mode, used for acceptance, stops there. Running with no stub calls does not prove compatibility, so structural warnings are counted separately. The rules are in [docs/spec/personality.md](docs/spec/personality.md), [docs/spec/win32.md](docs/spec/win32.md), [docs/spec/crt.md](docs/spec/crt.md) and [docs/spec/system.md](docs/spec/system.md).

The virtual file system keeps the game's own files apart from its writes, and the registry persists per game. An installer writes into a folder that the launcher manages. Read-only ISO 9660 and Joliet images can be attached as CD drives.

API signatures and metadata come from `data/abi/`; `tools/abi/` regenerates them, and `adv-abi` registers each front end from them.

## Devices

- **Graphics:** GDI, DirectDraw and Direct3D 9 share neutral surfaces and drawing services. Direct3D 9 shaders are translated within the limits in [docs/spec/graphics.md](docs/spec/graphics.md). Device services follow [docs/spec/device.md](docs/spec/device.md).
- **Audio:** one mixer resamples with rubato and decodes with Symphonia. Shared PCM buffers are read through stable views, so a device never reads storage that the guest has freed.
- **Video:** DirectShow, MCI and wmvcore decode through FFmpeg when built with the `media` feature. See [docs/spec/media.md](docs/spec/media.md).
- **Fonts:** Windows GDI on Windows, and fontdb, skrifa and zeno elsewhere. Fonts that the guest supplies take priority.

## Front ends

- **`adv-run`** is the command-line runner used for development and testing, with or without a window.
- **`apps/adv-launcher`** is the player front end, built with Tauri and Svelte. It shows the game library, and draws its overlay as a transparent webview on top of the native game view. The overlay publishes the regions it wants input for, and the shell routes each pointer or key to the overlay or to the game.
- **`apps/adv-jit-helper`** runs in the iOS player's embedded extension. It uses idevice to connect to the paired local device, mount its developer disk image and prepare the player's JIT pool as a separate debugger process (IOS-JIT-2). It has no runtime crate dependencies.
- On Android, the same launcher runs in a Tauri Android app. Its webview sits above a SurfaceView that adv-shell presents into ([docs/spec/android.md](docs/spec/android.md)).

On macOS, guest menus appear in the native menu bar and guest dialogs become child windows of the game window. Quitting first asks the game to close and waits for it; a second quit forces the stop. Sleep releases the audio stream, and wake rebuilds it. After a run, the launcher can save an anonymous compatibility report whose format `adv-report` defines ([docs/spec/report.md](docs/spec/report.md)). The host contracts are in [docs/spec/host.md](docs/spec/host.md).

## Testing

- CPU behavior is checked with self-written instruction and fault fixtures, differential runs against the interpreter, and Unicorn as a test-only reference.
- Windows behavior is checked by running the same self-written program natively on Windows and under Hitode, then comparing the results.
- `tools/check.py` runs before every commit. It checks formatting, privacy, layering, the specs and the launcher front end.
- Rules in `docs/spec/` name their verification, and tests cite the rules they check with `// spec:` markers.

Game assets, screenshots, traces and anything taken from a game stay local and never enter the repository.

## Where to look next

| To find | Look in |
|---|---|
| Rules a subsystem must follow | [docs/spec/](docs/spec/README.md) |
| Why something is the way it is | The module docs (`//!`) and crate READMEs, then the commit history |
| Environment variables, command-line options and features | [docs/configuration.md](docs/configuration.md) |
| Building, testing and packaging | [docs/building.md](docs/building.md) |
| Tools for debugging a game | [docs/investigation.md](docs/investigation.md) |
| Host shells and platforms | [docs/spec/host.md](docs/spec/host.md), [docs/spec/android.md](docs/spec/android.md), [docs/building.md](docs/building.md) |
