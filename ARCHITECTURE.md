# Architecture

This document is a map of Hitode: what the parts are, how they connect, and where to look next. It describes the code as it is. The rules live in [docs/spec/](docs/spec/README.md), and the design behind each part lives in the module docs and README of its crate. Terms are defined in [docs/glossary.md](docs/glossary.md).

## What Hitode does

Hitode runs the original x86-32 Windows executables and DLLs of classic ADV (visual novel) games on AArch64 macOS, iOS/iPadOS and Android hosts. The goals are to run many engines without per-game setup, to use little power, and to keep fan translation patches working.

The approach is borrowed from console emulators. The game's own code runs unchanged as the *guest*. Hitode provides what that code expects from its "hardware": Wine's Windows userland over an NT platform of its own, graphics, audio and video devices, time and threads. Where a library call or a hot leaf function has a proven-equivalent native implementation, Hitode may run that instead, as long as the guest cannot tell the difference.

The main targets are games that use GDI, DirectDraw, Direct3D 9, DirectSound and the classic video paths. x64 guests, Direct3D 11 and OpenGL are out of scope.

## Principles

These hold across the whole codebase:

- **Only what the guest can observe.** Hitode implements the system behavior that guest code can see. It does not rebuild the NT kernel or a Windows process.
- **Patches stay possible.** Patches rewrite import pointers, export entries, function prologues and COM vtables. Every fast path checks for such rewrites and falls back to the slow path when they happen. System DLLs, Wine's and the host-module images alike, exist in guest memory, so patches can find and change them like real ones.
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
| Assembly | `adv-runtime` | Wine sessions: wiring the Wine userland, the native device fronts and devices | API behavior |
| Device fronts | `adv-dx` | Parameters, observable behavior and object state of the DirectX COM fronts | Drawing, mixing, decoding |
| System services | `adv-os` | Heaps, virtual file system, COM activation, NLS tables and host key mapping for native fronts | |
| ABI | `adv-abi`, `adv-abi-macros` | Signatures, calling conventions, argument decoding, registration, stubs, API diagnostics, COM identities and vtables; native computation contracts, adaptation proposals and admission proofs | API behavior and native computation |
| NT platform | `adv-nt` | The NT services under Wine's PE modules in the Wine userland port: image mapping, process startup, the system call and Unix call dispatchers, the server protocol client, boundary counters, and the code identity of distribution modules with the native bodies it admits | Windows behavior above the NT interface |
| Wine server | `adv-wineserver` | Wine's server built in process for the Wine userland port ([#58](https://github.com/hitode-project/hitode/issues/58), [docs/spec/wine.md](docs/spec/wine.md)): handles, namespace, registry and timers, resumed by its caller with time from its caller | Waiting, time, host processes and signals |
| Wine drivers | `adv-drv` | Wine's driver modules on the Wine userland: the user driver, which places windows and gives them surfaces in the process surface store | The graphics fronts; host windows and output |
| Unix libraries | `adv-unix` | Wine's Unix libraries, such as `win32u.so`, as guest ELF code: the loader, the host side of `ntdll.so` they import, and the C library parts they use | Windows behavior; host libraries they would load |
| Machine | `adv-machine` | Guest contexts, scheduling, clocks, waits, callbacks, exception and trap dispatch | |
| Execution | `adv-cpu`, `adv-cpu-core`, `adv-cpu-a64` | Running x86 code: CPU state, guest address space, code validity, translation | Anything about Win32 |
| Devices | `adv-device` | Neutral surfaces and drawing, PCM mixing, fonts, audio and video decoding | Win32 handles and window semantics |
| Shared | `adv-sync`, `adv-pe`, `adv-report` | Channels and wakeups; PE parsing and generic image writing; the anonymous compatibility report format | Policy |
| Test only | `adv-unicorn`, `adv-samples` | A reference CPU, locating local samples | Anything shipped |

The dependency arrows, pointing at what is depended on:

```text
adv-shell → adv-runtime → adv-dx → adv-abi → adv-machine → adv-cpu
adv-dx → adv-device;  adv-os → adv-abi
adv-runtime → adv-drv → adv-unix → adv-nt (the Wine userland)
adv-drv → adv-device
adv-runtime → adv-wineserver (a Wine session connects the server to adv-nt)
adv-nt → adv-wineserver (native queue key state)
adv-dx / adv-runtime / adv-shell → adv-os (transitional: services of the native fronts)
adv-cpu → adv-cpu-a64 → adv-cpu-core
adv-shell → adv-device
adv-cpu-core / adv-machine / adv-device / adv-shell → adv-sync
adv-shell → adv-report;  adv-os → adv-pe;  adv-abi → adv-abi-macros / adv-pe
adv-run / adv-launcher → adv-shell
```

DirectDraw and D3D9 use
`adv-abi` for COM identities, interface aliases and reference counts and
`adv-device` for surfaces and GPU work. The assembly supplies D3D9's private guest
allocator and both fronts' memory DC operations (`adv_dx::interop`), window geometry
and display state. DC operations complete through `Outcome` continuations so an
assembly can call guest Windows interfaces. DirectDraw keeps its private surface arena. Other DirectX fronts retain a
transitional dependency on `adv-os` for heaps, activation, Automation, files and
input state.

For the Wine stack, `adv-abi::dll` writes host-module PE images through `adv-pe` and calls their imports through the machine. `adv-unix::builtin` installs those images in the prefix before process startup. `adv-runtime::wine` assembles the graphics fronts on that stack: our DirectDraw as such an image, with memory DCs from Wine's GDI. `adv-drv` holds the driver modules win32u loads, such as the user driver whose window reports give the fronts their geometry.

## A frame, from start to screen

1. **Launch.** A front end (the `adv-run` command line, the Tauri launcher or the Android app) creates an `adv-shell` session for the game executable, a data directory and options such as the code page.
2. **Load.** `adv-runtime::wine` starts the program through `adv-nt` and Wine's PE loader, using the bundled Windows modules and native device fronts.
3. **Run.** `adv-machine` schedules guest threads on one execution owner, and `adv-cpu` runs their code. Code runs first in the interpreter; hot code is translated to host machine code.
4. **Call the system.** Wine implements Windows behavior above the NT and Unix boundaries served by `adv-nt` and `adv-unix`. Native device fronts use `adv-abi`. Guest callbacks and waits save continuations that resume in the same thread's context.
5. **Draw.** GDI, DirectDraw and Direct3D 9 front ends draw through the neutral surfaces of `adv-device`, with software and GPU backends behind one interface. Apple ARM programmable draws use native Metal with wgpu-owned textures; other GPU work uses wgpu ([GFX-METAL-1](docs/spec/graphics.md#gfx-metal-1-apple-programmable-draws)). A finished frame goes into a mailbox.
6. **Present.** The shell takes the newest finished frame and presents it through wgpu (Metal on macOS). Dropping a host frame never skips a draw, lock or readback that the guest can observe.
7. **Sound and input.** DirectSound and waveOut share one mixer and playback clock, and the host audio callback only reads mixed PCM. Host input events become USER messages in the guest's queues.

## A call, from guest to body

A guest call to a function that has a native body, such as `memcpy` or `SetEvent`, takes the steps below. Each step names its code and the rules it follows. `tools/docs/check_docs.py` fails a commit when a named file or symbol no longer exists, or a cited rule is withdrawn, so the change that moves a step also updates its text.

1. **Bind at load.** The loader publishes imports through the thunk table. When `adv-nt` maps a distribution module, each body whose function bytes match the identity table binds the function's entry. The entry then leads to a thunk, and a direct body is also recorded under its entry. Runtime services bind a body to their own thunk, and the copied entries of host-module images carry that binding. Runtime helpers that game images link statically bind wherever their bytes appear.
   Code: [`ThunkTable`](crates/adv-machine/src/thunk.rs), [`bind`](crates/adv-nt/src/native.rs), [`bind_one`](crates/adv-nt/src/native.rs), [`intact`](crates/adv-nt/src/identity.rs), [`Sys::direct_bodies`](crates/adv-machine/src/lib.rs), [`Sys::bind_thunk_body`](crates/adv-machine/src/lib.rs), [`section::bind_builtin_bodies`](crates/adv-nt/src/section.rs), [`linked::bind`](crates/adv-nt/src/native/linked.rs).
   Rules: A64-CTL-14, NAT-BODY-13, NAT-BODY-18.
2. **Translate the call.** A trace resolves `call [slot]` and `jmp [slot]` when it is translated, and watches the slot and the entry bytes. A call reaching an intact entry with a direct body becomes an `Op::Body` site. Other transfers to a redirected entry go to its thunk.
   Code: [`Trace::build`](crates/adv-cpu-a64/src/translate/trace.rs), [`Redirect`](crates/adv-cpu-a64/src/translate/trace.rs), [`Bound`](crates/adv-cpu-a64/src/translate/trace.rs), [`Op::Body`](crates/adv-cpu-a64/src/translate/decode.rs), [`TrapHandler::direct_body`](crates/adv-cpu-core/src/backend/mod.rs).
   Rules: A64-CALL-1, A64-CTL-11, A64-CTL-14.
3. **Run the body on the window.** The site forwards profitable adjacent argument pushes for inline expansions, loads other declared stack, register, floating-control and bound constant inputs into w1–w7, points x0 at the body context, and calls the body as an AAPCS64 leaf, saving only unpublished caller-saved XMM values. Constant memory calls up to 64 bytes expand inline; blocked or crossing spans take the body. Omitted argument stores are restored before an out-of-line body runs, so ordinary bodies need no stack-alias exclusion. The body checks each 4 KiB page's class once, then reads and writes the guest window directly. The site writes the declared result registers and pops the arguments.
   Code: [`Translator::body`](crates/adv-cpu-a64/src/translate/ops.rs), [`Translator::inline_body`](crates/adv-cpu-a64/src/translate/body.rs), [`body`](crates/adv-cpu-core/src/body.rs) (the macro), [`BodyContext`](crates/adv-cpu-core/src/body.rs), [`DirectBody`](crates/adv-cpu-core/src/body.rs), [`Window`](crates/adv-cpu-core/src/body.rs).
   Rules: A64-CALL-2, A64-CALL-3, A64-CALL-4, A64-CALL-8, NAT-BODY-17.
4. **Leave for the full path.** A page the window may not take, such as a guard, file, shared or watched page, stops the body with `Stop::Slow` before it writes anything. Unmasked pixel arithmetic also takes this path, so a trap retains the writes that precede it. A call site then exits to interpret the call, and an entry site dispatches to the thunk.
   Code: [`Stop`](crates/adv-cpu-core/src/body.rs), [`Translator::body`](crates/adv-cpu-a64/src/translate/ops.rs).
   Rules: A64-CALL-4, A64-CALL-5.
5. **Run the full path.** The interpreter takes the entry's redirect to the thunk, and the machine runs its handler. A direct body runs again over `GuestMem`, which completes every access as a guest instruction would. A body that is not yet direct runs here with the handler's environment.
   Code: [`run_inner`](crates/adv-cpu-a64/src/executor.rs), [`Dispatch::on_host_call`](crates/adv-machine/src/lib.rs), [`run`](crates/adv-nt/src/native.rs), [`run_body`](crates/adv-nt/src/native.rs), [`run_full`](crates/adv-nt/src/native.rs), [`GuestMem`](crates/adv-cpu-core/src/body.rs).
   Rules: A64-CALL-5, NAT-BODY-16, NAT-BODY-17.
6. **Return, raise or run guest code.** A result returns as the function's `ret` would. A fault, including division by zero, raises the guest exception as if the entry's first instruction faulted. A body whose function would wait, call guest code or request the server runs the function's own code once, through a trampoline. A completed body may also ask to yield: generated code commits the return, and the machine hands off at the continuation.
   Code: [`Outcome::Raise`](crates/adv-machine/src/outcome.rs), [`apply`](crates/adv-machine/src/lib.rs), [`exception`](crates/adv-machine/src/lib.rs), [`resume_original`](crates/adv-nt/src/native.rs), [`trampoline`](crates/adv-nt/src/native.rs).
   Rules: A64-CALL-4, A64-CALL-6.
7. **Reach host state.** Service bodies, such as event signals and file positions, change host state through the `Services` the body context carries. That state lives in storage the machine also reaches only through shared handles, so no exclusive borrow aliases it. The server fd retains its shared file cursor until its last owner retires, including inherited handles without a client descriptor cache.
   Code: [`Services`](crates/adv-cpu-core/src/body.rs), [`TrapHandler::body_services`](crates/adv-cpu-core/src/backend/mod.rs), [`services::bodies`](crates/adv-nt/src/native/services.rs).
   Rules: NAT-BODY-18.
8. **Call without a body.** System calls (`int 0x2e`) and other thunks call their handler through the call stub, which synchronizes guest state. With diagnostics off, the call reads no clock and records nothing.
   Code: [`call`](crates/adv-cpu-a64/src/native.rs), [`Dispatch::on_host_call`](crates/adv-machine/src/lib.rs).
   Rules: A64-CTL-12, EXEC-CALL-1, EXEC-CALL-2.

Wine's eight kernelbase string comparison bodies port Wine 11's collation code
and read its loaded sort, locale and code page tables in place. ANSI sources
decode on demand, and comparison levels stream weights through stack buffers.
Byte identity and independent ordinary prefixes avoid conversion and weight work.
Locale cache publication, code page loading and owned locale locks stay in Wine.
The bodies' `Input::Args` names the argument block and `Input::Teb` carries FS
base through both translated calls and the full path (NAT-BODY-20).

## Core crates, by module

The four crates a guest call crosses, one row per role. The rules stay in the specs; a row says where to look. `tools/docs/check_docs.py` fails a commit when a top-level module of these crates is missing here, so a new or moved module updates its row.

### adv-cpu-a64

| Modules | Responsible for | Not responsible for |
|---|---|---|
| [`translate`](crates/adv-cpu-a64/src/translate/mod.rs) | Translation units: trace selection and analysis, switch tables, and the JIT back end that lowers a unit to AArch64 code, including body call sites (A64-TR-3, A64-TR-7) | Executable memory; running units |
| [`native`](crates/adv-cpu-a64/src/native.rs) | Running translated code: dispatch and unit lookup, link patching, safepoints, host calls from translated code and the entry to fault recovery | Choosing guest instructions |
| [`executor`](crates/adv-cpu-a64/src/executor.rs) | The engine's execution owner: first visits, when an entry is translated (A64-TR-2), exits, traps, fuel and deadlines, interpreter-only runs | Code generation |
| [`cpu`](crates/adv-cpu-a64/src/cpu.rs) | `Backend` and `Engine` over the executor, as the machine layer drives them | Scheduling |
| [`runtime`](crates/adv-cpu-a64/src/runtime.rs) | The execution thread's runtime context and the stubs that move guest state into and out of the fixed host registers (A64-REG-1) |  |
| [`cache`](crates/adv-cpu-a64/src/cache.rs) | Executable storage leased from the process code pool, never writable and executable through one mapping (A64-CTL-1, A64-CTL-2) | What code goes into it |
| [`asm`](crates/adv-cpu-a64/src/asm.rs), [`encode`](crates/adv-cpu-a64/src/encode.rs) | An instruction buffer with labels; `const fn` AArch64 encoders checked against clang vectors | Instruction selection |
| [`jit_map`](crates/adv-cpu-a64/src/jit_map.rs) | Opt-in maps of placed units for samplers | Changing translation |

### adv-cpu-core

| Modules | Responsible for | Not responsible for |
|---|---|---|
| [`backend`](crates/adv-cpu-core/src/backend/mod.rs) | The guest CPU interface the machine drives: `Backend::run`, `TrapHandler` and its body and service hooks | Scheduling; anything about Win32 |
| [`interpreter`](crates/adv-cpu-core/src/interpreter.rs) | Tier 0: predecoded integer execution with restartable faults, also the oracle for differential tests | x87 and SSE arithmetic |
| [`floating`](crates/adv-cpu-core/src/floating.rs), [`state`](crates/adv-cpu-core/src/state.rs) | Exact, transactional x87, SSE and packed-integer execution; executor-independent floating state |  |
| [`integer`](crates/adv-cpu-core/src/integer.rs), [`strings`](crates/adv-cpu-core/src/strings.rs) | Integer operand and flag semantics shared by execution and preparation-time analysis; guarded REP copies, fills, comparisons and scans |  |
| [`architecture`](crates/adv-cpu-core/src/architecture.rs) | Guest architectural state | OS context layouts, which `adv-nt::context` owns |
| [`memory`](crates/adv-cpu-core/src/memory.rs) | The guest window: page permissions and classes, watched code, dirty ranges and host ranges | Address-space policy, which `adv-machine::vm` owns |
| [`virtual_memory`](crates/adv-cpu-core/src/virtual_memory.rs), [`dual_view`](crates/adv-cpu-core/src/dual_view.rs), [`shared_memory`](crates/adv-cpu-core/src/shared_memory.rs) | Stable host reservations and coherent aliased views for code, devices and shared guest storage |  |
| [`body`](crates/adv-cpu-core/src/body.rs) | Direct bodies: the `body!` macro, `BodyContext`, and the `Window` and `GuestMem` accesses shared by both paths (A64-CALL-2, NAT-BODY-17) | Which functions have bodies, which `adv-nt::native` decides |
| [`trap`](crates/adv-cpu-core/src/trap.rs), [`file_memory`](crates/adv-cpu-core/src/file_memory.rs) | The raw trap boundary shared by native and interpreted code; native accesses to file-backed and lazy pages |  |
| [`fault_recovery`](crates/adv-cpu-core/src/fault_recovery.rs), [`fault_recipe`](crates/adv-cpu-core/src/fault_recipe.rs), [`execution_abi`](crates/adv-cpu-core/src/execution_abi.rs) | Exact scalar recovery from host faults in native code, and the state contract it restores |  |
| [`code_pool`](crates/adv-cpu-core/src/code_pool.rs), [`code_image`](crates/adv-cpu-core/src/code_image.rs) | Leases in the prepared executable pool; the image metadata the loader hands to the engine | Code generation |
| [`native_region`](crates/adv-cpu-core/src/native_region.rs) | Lowering data for an admitted byte region | Proving the region, which `adv-abi::native` does |
| [`engine`](crates/adv-cpu-core/src/engine.rs), [`host_features`](crates/adv-cpu-core/src/host_features.rs), [`cpu_profile`](crates/adv-cpu-core/src/cpu_profile.rs) | Engine options and reports for shells; the host's ARM64 features; the fixed virtual processor identity that guests see | Exposing host features to guests |
| [`timestamp`](crates/adv-cpu-core/src/timestamp.rs) | A 1 GHz guest reference counter | The guest clock, which `adv-machine::clock` owns |
| [`cliffs`](crates/adv-cpu-core/src/cliffs.rs), [`frame_counters`](crates/adv-cpu-core/src/frame_counters.rs), [`instruction_trace`](crates/adv-cpu-core/src/instruction_trace.rs), [`perf_map`](crates/adv-cpu-core/src/perf_map.rs), [`tier_verify`](crates/adv-cpu-core/src/tier_verify.rs) | Diagnostics: slow-path counters, frame snapshots, interpreter traces, sampler maps and opt-in tier verification against the interpreter | Changing execution |

### adv-machine

| Modules | Responsible for | Not responsible for |
|---|---|---|
| [`Machine`](crates/adv-machine/src/lib.rs), [`Sys`](crates/adv-machine/src/lib.rs) | The crate root: host-call dispatch, direct-body binding and the delivery of a handler's outcome |  |
| [`thunk`](crates/adv-machine/src/thunk.rs), [`patch`](crates/adv-machine/src/patch.rs) | Host function binding, the hookable thunk page, and detours of guest functions | Signatures, which `adv-abi` owns |
| [`outcome`](crates/adv-machine/src/outcome.rs), [`invocation`](crates/adv-machine/src/invocation.rs), [`leaf`](crates/adv-machine/src/leaf.rs), [`service`](crates/adv-machine/src/service.rs) | What a handler asks for next, as continuations that never block the host; invoking existing bindings; memory-only handlers; work that bodies record for the machine |  |
| [`sched`](crates/adv-machine/src/sched.rs), [`scheduling`](crates/adv-machine/src/scheduling.rs), [`wake_queue`](crates/adv-machine/src/wake_queue.rs) | Guest contexts and the cooperative scheduler, host-side allowances and wake preference |  |
| [`clock`](crates/adv-machine/src/clock.rs), [`frame_spin`](crates/adv-machine/src/frame_spin.rs), [`poll_progress`](crates/adv-machine/src/poll_progress.rs) | The single guest time source; frame spin and productive polling (PROC-CLOCK-4, PROC-CLOCK-5) | Host presentation timing |
| [`exception`](crates/adv-machine/src/exception.rs) | Delivery of precise CPU stops through the continuation stack | Exception records and handler search, which `adv-nt::exception` owns |
| [`vm`](crates/adv-machine/src/vm.rs), [`layout`](crates/adv-machine/src/layout.rs), [`guest_words`](crates/adv-machine/src/guest_words.rs) | Guest address-space bookkeeping and the layout of host-owned regions; words shared with authored guest code | Mapping pages, which `adv-cpu-core::memory` does |
| [`process`](crates/adv-machine/src/process.rs), [`objects`](crates/adv-machine/src/objects.rs), [`ext`](crates/adv-machine/src/ext.rs) | The process facts the machine needs, owned payloads and alias namespaces, and per-subsystem state by type | Image loading; API handle semantics |
| [`trace`](crates/adv-machine/src/trace.rs), [`stats`](crates/adv-machine/src/stats.rs), [`frame_budget`](crates/adv-machine/src/frame_budget.rs), [`schedule_trace`](crates/adv-machine/src/schedule_trace.rs), [`wait_diagnostics`](crates/adv-machine/src/wait_diagnostics.rs) | Diagnostics: the host-call trace, cost accounting, frame budgets, and scheduler and wait observations |  |

### adv-nt

| Modules | Responsible for | Not responsible for |
|---|---|---|
| [`dispatch`](crates/adv-nt/src/dispatch.rs), [`syscalls`](crates/adv-nt/src/syscalls.rs), [`unix`](crates/adv-nt/src/unix.rs), [`unixlib`](crates/adv-nt/src/unixlib.rs), [`win32k`](crates/adv-nt/src/win32k.rs) | Where ntdll.dll leaves the guest: system calls, Unix calls, builtin Unix libraries, win32k calls and user-mode callbacks | What `win32u.so` does with its calls |
| [`native`](crates/adv-nt/src/native.rs), [`identity`](crates/adv-nt/src/identity.rs) | Native bodies of distribution functions, and the code identity that admits them | Guest memory access inside bodies, which `adv-cpu-core::body` owns |
| [`process`](crates/adv-nt/src/process.rs), [`thread`](crates/adv-nt/src/thread.rs), [`params`](crates/adv-nt/src/params.rs), [`environment`](crates/adv-nt/src/environment.rs), [`apc`](crates/adv-nt/src/apc.rs) | Process startup and exit, threads, the parameters block, the initial environment and system APCs |  |
| [`memory`](crates/adv-nt/src/memory.rs), [`section`](crates/adv-nt/src/section.rs), [`image`](crates/adv-nt/src/image.rs) | Virtual memory services, sections and image views |  |
| [`exception`](crates/adv-nt/src/exception.rs), [`context`](crates/adv-nt/src/context.rs) | CPU exceptions, delivered as Wine's i386 signal handler delivers them, and the i386 CONTEXT record |  |
| [`server`](crates/adv-nt/src/server.rs), [`protocol`](crates/adv-nt/src/protocol.rs) | The wineserver client and its generated request layouts | The server, which `adv-wineserver` is |
| [`file`](crates/adv-nt/src/file.rs), [`directory`](crates/adv-nt/src/directory.rs), [`path`](crates/adv-nt/src/path.rs), [`object`](crates/adv-nt/src/object.rs) | File system calls, directory listings, NT names to host paths, and object attributes |  |
| [`sync`](crates/adv-nt/src/sync.rs), [`ntsync`](crates/adv-nt/src/ntsync.rs) | Waits and synchronization objects, over an in-process ntsync device | Parking, which the machine does |
| [`registry`](crates/adv-nt/src/registry.rs), [`token`](crates/adv-nt/src/token.rs), [`atom`](crates/adv-nt/src/atom.rs) | Calls on the registry, tokens and atoms that the server holds |  |
| [`information`](crates/adv-nt/src/information.rs), [`system`](crates/adv-nt/src/system.rs), [`kuser`](crates/adv-nt/src/kuser.rs), [`timezone`](crates/adv-nt/src/timezone.rs), [`nls`](crates/adv-nt/src/nls.rs), [`debug`](crates/adv-nt/src/debug.rs) | Process, thread and system information, KUSER_SHARED_DATA, the time zone, NLS sections and Wine's debug channels |  |
| [`status`](crates/adv-nt/src/status.rs), [`counters`](crates/adv-nt/src/counters.rs) | NTSTATUS values and boundary counters |  |

## Execution

The `adv-cpu` facade exposes the AArch64 engine:

- **Tier 0** is a predecoding interpreter shared with the translator. It runs cold code, single steps and every instruction without a native form, and it is the oracle for differential tests.
- **a64** runs on every AArch64 host. An entry runs once in a first-visit executor, and a second visit translates a trace. Guest registers live in fixed host registers, and guest memory is accessed directly through a guarded 4 GiB window. Faults recover from side tables. See [docs/spec/a64.md](docs/spec/a64.md).

The supported product hosts are macOS on Apple silicon, iOS/iPadOS and Android on AArch64.

## Machine

One execution owner runs all guest threads; there is no parallel guest execution. Waits, signals, timeouts and cancellation go through the same machine state. API handlers that need a guest callback save a continuation instead of blocking the host.

The machine owns the virtual clock. Holding Ctrl to skip may advance time to the next deadline only when no context can run, and never completes external I/O early. Several guest processes can run inside one host process, each with its own address space, machine and handles. The rules are in [docs/spec/processes.md](docs/spec/processes.md).

## Personality and compatibility

Wine provides the Windows personality. Hitode serves the device fronts DirectDraw, Direct3D 9, DirectSound, DirectShow, Windows Media and XAudio2 natively, as host-module images whose exports follow the native contracts; every other module is Wine's own code. The rules are in [docs/spec/wine.md](docs/spec/wine.md), [docs/spec/graphics.md](docs/spec/graphics.md) and [docs/spec/media.md](docs/spec/media.md).

Games share one Wine prefix, whose registry keeps their settings, and run in place from their own folders. Read-only ISO 9660 and Joliet images can be attached as CD drives through `adv-nt`'s read-only volume interface.
Sparse identity files retain wineserver handle and section ownership; data and lazy
mapping pages come directly from the shared ISO reader. Installation directories are
NT path redirects, so the prefix's original Program Files directory remains intact.

API signatures and metadata come from `data/abi/`; `tools/abi/` regenerates them, and `adv-abi` registers each front end from them.

## Devices

`adv-abi::native` separates `ReferenceContract`, `CandidateBinding` and
`VerifiedBinding`. Preparation reads owned, immutable image snapshots with
module-relative dependencies. The analyzer only proposes mappings; the
independent verifier decodes their complete code through `adv-cpu-core::integer`
and checks a supported recurrence before issuing a binding. Typed expressions
retain memory versions and complete state for each exit; entry conditions are
parameterized obligations for a publication bridge. Its forward-byte rule covers
LZ77 and PNG Sub/Up/Average bodies, plus straight-line Paeth predictors with fixed
dword frame slots. Indexed loops and register allocation are inferred. Unsupported
recurrences remain guest work. Wine's `adv-drv::codec` supplies cold admission for
the four PNG filters and long forward LZ77 copies through `adv-machine::NativeAdmission`.
The A64 front end offers complete
hot loop bodies; only verified bindings lower to engine-neutral native plans.
The back end specializes mappings, calls `adv-device::codec_kernel` on directly
accessible guest buffers and restores exact state through a checked scalar
tail. The ordinary translation cache owns these bridges and revokes them on
code changes, new stops and unmapping. No per-call analysis or identity hashing
is performed. A fixed scalar argument and separately checked read spans preserve
prefix history for row segments. Fixed-frame guards preserve invariant inputs
and the scalar tail's complete scratch effects. Paeth forms with internal
branches retain guest execution. These LZ77 copy bridges preserve overlapping
history; surrounding symbol decoding, window wrap and stream state remain guest work.

Wine's pinned libpng row entries use direct scalar filters on every host, including
branch-based Paeth and overlapping rows. D3DX9 versions 24–43 share direct pixel
conversion over checked guest spans, including floating, indexed and compressed
formats. The pinned Wine conversion kernels handle channel arithmetic, filters
and block codecs; disjoint 32-bit rows use a byte shuffle. Admission finishes
after the loader resolves copy imports, on the first entry, and watches the
helper code and import targets with the function.
Wine's embedded zlib copies share direct Adler-32 and `inflate_fast` bodies.
The decoder reads input, code tables and ring history in place, so output can
alias those inputs. It publishes zlib's stream and bit state, including error
messages and block ends; error-message addresses resolve when the module binds.

- **Graphics:** GDI, DirectDraw and Direct3D 9 share neutral surfaces and drawing services. Direct3D 9 shaders are translated within the limits in [docs/spec/graphics.md](docs/spec/graphics.md). Device services follow [docs/spec/device.md](docs/spec/device.md).
- **Audio:** one mixer resamples with rubato and decodes with Symphonia. Wine MIDI output synthesizes through OxiSynth with the bundled General MIDI bank. Shared PCM buffers are read through stable views, so a device never reads storage that the guest has freed.
- **Video:** DirectShow, MCI and wmvcore decode through FFmpeg when built with the `media` feature. See [docs/spec/media.md](docs/spec/media.md).
- **Fonts:** `adv-runtime::wine::fonts` owns GDI data, layout, DIB composition and DirectWrite glyph services over `adv-device::text` (fontdb, skrifa and zeno). Required native bodies serve interpreted and translated calls; Wine's font backend stays dormant. Fonts that the guest supplies take priority.

## Front ends

- **`adv-run`** is the command-line runner used for development and testing, with or without a window.
- **`apps/adv-launcher`** is the player front end, built with Tauri and Svelte. It shows the game library, and draws its overlay as a transparent webview on top of the native game view. The overlay publishes the regions it wants input for, and the shell routes each pointer or key to the overlay or to the game.
- **`apps/adv-jit-helper`** runs in the iOS player's embedded extension. It uses idevice to connect to the paired local device, mount its developer disk image and prepare the player's JIT pool as a separate debugger process (IOS-JIT-2). It has no runtime crate dependencies.
- On Android, the same launcher runs in a Tauri Android app. Its webview sits above a SurfaceView that adv-shell presents into ([docs/spec/android.md](docs/spec/android.md)).

The player uses Wine on macOS, iOS and Android. `adv-shell::Execution` owns one
execution thread and `adv-runtime::wine::session::Host` keeps the prefix server
between games. Each game has its own session, machines, devices and mount sources;
teardown saves the registry and releases every client before the finish callback.
The host provisions and boots the prefix once per server, then installs game
fonts without loading them in wineboot. System font discovery is reused between
sessions; each session applies its own Text resources and font overrides.
The execution owner blocks between games. The launcher prepares the embedded Wine
distribution off the UI thread; native views and input keep the same shell channels.

On macOS, guest menus appear in the native menu bar and guest dialogs become child windows of the game window. Quitting first asks the game to close and waits for it; a second quit forces the stop. Sleep releases the audio stream, and wake rebuilds it. After a run, the launcher can save an anonymous compatibility report whose format `adv-report` defines ([docs/spec/report.md](docs/spec/report.md)). The host contracts are in [docs/spec/host.md](docs/spec/host.md).

## Testing

- CPU behavior is checked with self-written instruction and fault fixtures, differential runs against the interpreter, and Unicorn as a test-only reference.
- Windows behavior is checked in Wine sessions: self-written probes and Wine's own conformance tests run on the Wine stack and compare their results with the native contract.
- `tools/check.py` runs before every commit. It checks formatting, privacy, layering, the specs, the code, rules and core-crate modules this guide names, and the launcher front end.
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
