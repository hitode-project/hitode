# Machine and guest processes

ID prefix: PROC

This spec covers `adv-machine`, which schedules guest threads, owns time and dispatches callbacks, and the guest process family that `adv-runtime` runs. The family supports ordinary x86 `CreateProcessA/W` launch chains, including the suspended creation, remote import rewriting and resume that Microsoft Detours performs. [README.md](README.md) explains the format.

## Machine

### PROC-MACH-1 One execution owner

**Rule**: One execution owner MUST run all guest threads, and guest code MUST NOT run in parallel. Row helpers and background compile threads are not guest execution, and each MUST keep its own bounded budget.
**Verification**: Review — the structure of the machine enforces it.

### PROC-MACH-2 Waits and wakeups

**Rule**: Each context MUST have one owner at a time. Wait registration, signal consumption, timeouts and cancellation MUST go through the same machine state. A host with no events MUST sleep on `WakeEvent`, which keeps pending notifications so wakeups are not lost.
**Verification**: Test

### PROC-MACH-10 Signal-driven waits

**Rule**: Signaling MUST release satisfiable waiters in wait order per object. A wake pass MUST evaluate only changed objects and expired wait deadlines. Before the host idles, a full scan MUST release any wait an unsignaled state change satisfied.
**Verification**: Test

### PROC-MACH-3 Continuations

**Rule**: An API that needs a guest callback or a wait MUST save a continuation and resume it in the original thread context. Host object locks MUST NOT be held across guest execution or waits. Nonlocal unwinding MUST NOT be handled by relaxing continuation checks.
**Verification**: Test

### PROC-MACH-4 Direct host services

**Rule**: `Env::invoke_host_u32` MUST pass a validated argument snapshot to a registered implementation without running rewritable private thunks. Ordinary API front ends MUST NOT use it to bypass guest hooks.
**Verification**: Review — the rule concerns which callers use the interface.

### PROC-MACH-5 Urgent wakeups

**Rule**: Urgent wakeups such as audio and input MAY run before the normal turn. They MUST NOT starve normal workers or change the completion semantics of guest locks and calls.
**Verification**: Test

### PROC-MACH-6 Audio callbacks

**Rule**: The host audio callback MUST NOT run guest code, take blocking locks, allocate or perform I/O. UI and execution MUST exchange data only through bounded channels and the frame mailbox.
**Verification**: Review — the constraint covers what platform callbacks call.

### PROC-MACH-7 Context wait boundary

**Rule**: Before parking, each initial or resumed `Outcome::Block` MUST invoke the installed personality hook with its context, CPU, system and extension state. The hook MAY replace the wait with a terminal outcome while preserving ordinary wait and continuation semantics.
**Verification**: Test

## Virtual clock

### PROC-CLOCK-1 Time skip

**Rule**: The machine MUST own the virtual clock. Ctrl time skip MAY advance to a deadline only when no context can run and the wait allows it. It MUST NOT complete external I/O early, and the audio play cursor MUST follow device consumption.
**Verification**: Test

### PROC-CLOCK-2 No time skip in a family

**Rule**: Once a child process exists, the family MUST disable Ctrl time skip. A waiting process MUST NOT skip the runnable time of another process.
**Verification**: Review — no test holds Ctrl while a child process runs.

### PROC-CLOCK-3 Clock polling

**Rule**: A context that only rereads the clock MAY wait for the next millisecond on each read. The streak that proves it MUST exceed bounded per-item loops, such as a 256-key scan read twice.
**Verification**: Test

### PROC-CLOCK-4 Frame spin

**Rule**: With a host frame clock, a real-time polling round MAY wait for the first vsync after a frame, at most once per frame. Every ready context MUST have completed that frame, passed one ordinary round since, and run under 0.5 ms since its last round. Held keys or mouse buttons MUST keep 1 ms rounds, and events MUST still wake the host at once.
**Verification**: Test

### PROC-CLOCK-5 Productive polling

**Rule**: A real-time polling round MAY wait only after every ready context repeated earlier host-call sequences between its last four yields. A context doing new work between yields MUST run again without a host wait.
**Verification**: Test

### PROC-CLOCK-6 Timer ticks

**Rule**: In real time, timed waits and waitable timers MUST expire on the first whole guest millisecond at or after their deadline. Display-paced waits MUST keep exact deadlines. Polling rounds MUST end on the next whole millisecond.
**Verification**: Test

## Isolation

### PROC-ISO-1 Separate processes

**Rule**: Each guest process MUST have its own `Machine`, address space, heap, module table, PEB, TEB, handle table and threads. The same virtual address MAY hold different bytes in different processes.
**Verification**: Test

### PROC-ISO-2 No host processes

**Rule**: The runtime MUST NOT start host Windows child processes. APIs MUST NOT recognize game names or injected DLL names.
**Verification**: Review — this limits how the runtime is built.

### PROC-ISO-3 Cross-process requests

**Rule**: Cross-process operations MUST run only after the execution lease returns. A trap MUST NOT reenter another guest.
**Verification**: Review — the request queue of `adv-os` enforces the order.

## Creation

### PROC-CREATE-1 Publication gate

**Rule**: A child MUST be admitted to scheduling only after the parent call writes both handles and `PROCESS_INFORMATION`. If the creation is cancelled or fails, the child MUST be reclaimed without running its TLS callbacks, DllMain or entry point.
**Verification**: Test

### PROC-CREATE-2 Startup parameters

**Rule**: `CreateProcessA/W` MUST copy the command line, ANSI or Unicode environment block, working directory and code page. The PEB parameters, `GetCommandLine` and CRT startup MUST read the same content.
**Verification**: Test

### PROC-CREATE-3 Suspended creation and injection

**Rule**: Creation MUST map the executable and establish the main thread before resume, without calling guest TLS or DllMain. After resume, the loader MUST read the current import directory from the child's memory, including remote allocations.
**Verification**: Test

### PROC-CREATE-4 Suspend counts and thread context

**Rule**: `CREATE_SUSPENDED`, `SuspendThread` and `ResumeThread` MUST keep real counts. `GetThreadContext` and `SetThreadContext` MUST act on the saved state of a suspended thread. The first real schedule MUST run initialization, then apply the latest requested integer and control state.
**Verification**: Test

## Memory and handles

### PROC-MEM-1 Remote memory

**Rule**: `VirtualAllocEx`, `VirtualFreeEx`, `VirtualProtectEx`, `VirtualQueryEx`, `ReadProcessMemory` and `WriteProcessMemory` MUST act on the target process. Outputs MUST be validated in the caller's address space. Writes MUST trigger the existing code invalidation.
**Verification**: Test

### PROC-MEM-2 Failures stay contained

**Rule**: A failed operation MUST NOT change another process, leave a half-created object or skip code invalidation.
**Verification**: Test

### PROC-HANDLE-1 Process and thread handles

**Rule**: Process and thread handles MUST reference a completion signal and a stable identity, and closing one MUST NOT end the process. Process exit MUST publish the exit code, wake waiters and release the address space. Handles MUST keep the final state queryable.
**Verification**: Test

### PROC-HANDLE-2 Handle attributes

**Rule**: `HANDLE_FLAG_INHERIT` and `HANDLE_FLAG_PROTECT_FROM_CLOSE` MUST belong to each alias separately.
**Verification**: Test

### PROC-HANDLE-4 Wait rights

**Rule**: A wait on a process or thread handle MUST check the `SYNCHRONIZE` right.
**Verification**: Review — no test waits on a handle without `SYNCHRONIZE`.

### PROC-HANDLE-3 Handle types without cross-process sharing

**Rule**: Process and thread handles MAY be inherited and duplicated across processes. Creation MUST fail when an inheritable handle has a type that is not shared across processes, and MUST NOT drop it silently.
**Verification**: Review — no test marks a file or event handle inheritable.

## Scheduling and failure

### PROC-SCHED-1 Single process and family scheduling

**Rule**: Before a child exists, `step()` MUST call the unsliced `Machine::run()`, and `step_for()` MUST use the caller's whole slice. Afterwards, the family MUST share one settled clock and rotate processes in short slices.
**Verification**: Test

### PROC-SCHED-2 Stable final results

**Rule**: Exit and failure results MUST stay the same on later steps.
**Verification**: Test

### PROC-SCHED-3 Loader exit

**Rule**: The exit of a launcher process MUST NOT end the session while a child lives. Display, input and reporting MUST move to the remaining child.
**Verification**: Test

### PROC-SCHED-4 Child failures

**Rule**: An exception in a child MUST end only that child, and the parent MUST be able to read its final state. The native exception status MUST be kept. Missing emulator capabilities MUST report `STATUS_NOT_SUPPORTED`, not an access violation.
**Verification**: Test

### PROC-SCHED-5 Family reports

**Rule**: The report MUST list each process with its PID, parent PID, image and final state. Missing APIs and acceptance eligibility MUST cover the whole family, including children that have exited.
**Verification**: Test

## Devices

### PROC-DEV-1 Foreground frames

**Rule**: A foreground switch MUST move the single frame mailbox producer to the new process. Frame sequence numbers MUST keep increasing across the switch.
**Verification**: Test

### PROC-DEV-2 Shared devices

**Rule**: Processes MUST share the host GPU device and queue, and each process MUST own its API front-end resources and caches. Audio source IDs MUST be partitioned by PID, and an exiting process MUST revoke only its own sources.
**Verification**: Review — no single test runs GPU and audio work in two processes.

## Limits and scope

### PROC-LIMIT-1 Bounded family

**Rule**: A family MUST allow at most 8 live processes, 128 processes in total and 64 pending requests. One remote transfer MUST NOT exceed 64 MiB, an environment block 2 MiB, and inherited aliases 1024. Exceeding a limit MUST fail without publishing a new machine.
**Verification**: Test

### PROC-LIMIT-2 Real-time scheduler only

**Rule**: Several processes MUST run only through the real-time scheduler of `Runtime`. Single-machine tools on virtual time MUST return not supported when the guest creates a child.
**Verification**: Review — the tools are outside the runtime tests.

### PROC-SCOPE-1 Unsupported creation features

**Rule**: Debug launches, extended startup attributes, non-null security descriptors, explicit standard handle redirection and debug registers MUST fail explicitly. 64-bit guests and cross-bitness helpers are not supported.
**Verification**: Review — each is a refusal path without a fixture.

Withdrawn: none

### PROC-MACH-8 Guest reference calls

**Rule**: DirectX reference calls MAY execute in guest vtables using shared words confined to the execution owner. Public and internal references MUST share one lifetime while preserving original edge handlers and the following conditions.
**Verification**: Test

| Aspect | Requirement |
|---|---|
| Storage | Shared words MUST survive guest unmapping. |
| Admission | Enrollment MUST require one public reference and the default reference limit. Allocation failure MUST preserve ordinary handlers. |
| Internal owners | A zero-public-reference object MUST retain internal bindings until their final release. |
| Audio | Guest audio references MUST mark the context through the private TEB extension at 0xffc, conservatively protecting it from cadence stretching for its lifetime. |
| Unsupported backends | Backends without shared-word support MUST retain ordinary handlers. |

### PROC-MACH-9 Host boundary accounting

**Rule**: Reports MUST expose non-leaf totals and completed guest and leaf fractions per API. Diagnostics MUST distinguish entries, same-binding fallbacks and linked fallbacks while preserving registers, flags and guest patches. Disabled guest bodies and fallback transfers MUST execute no diagnostic counter instructions.
**Verification**: Test
