# Wine userland

ID prefix: WINE

This spec covers the Wine userland stack. `adv-wineserver` runs Wine's server inside the host process. `adv-nt` takes the place of Wine's `ntdll.so` under Wine's own Windows DLLs. `adv-unix` runs Wine's other Unix libraries, such as `win32u.so`, as guest code. [README.md](README.md) explains the format.

## Sources

### WINE-SRC-1 Unmodified Wine

**Rule**: `vendor/wine` MUST stay byte-identical to the Wine release it vendors. Host adaptation MUST live in `adv-wineserver`'s shim, host and main sources, or in `adv-nt`.
**Verification**: Review — no check compares the tree with the release archive.

### WINE-SRC-2 Generated protocol

**Rule**: Every server request layout `adv-nt` sends MUST come from the vendored protocol headers through `tools/wine/protocol.py`. Hand-written request structures MUST NOT exist.
**Verification**: Check — `tools/check.py` runs `tools/wine/protocol.py --check`.

### WINE-SRC-3 System call numbers

**Rule**: System call numbers MUST come from the vendored `ntsyscalls.h` through `tools/wine/syscalls.py`.
**Verification**: Check — `tools/check.py` runs `tools/wine/syscalls.py --check`.

## Server

### WINE-SRV-1 In-process server

**Rule**: The server MUST run inside the host process and MUST return to the runtime whenever it would wait. Its clock MUST be the clock the runtime gives it.
**Verification**: Test

### WINE-SRV-2 No host-global effects

**Rule**: Starting or stepping the server MUST NOT change the host process's working directory. The server MUST NOT create files outside the prefix and `Config::server_dir`.
**Verification**: Test

### WINE-SRV-3 Socket path length

**Rule**: A guest process MUST connect to the server when the server directory path is longer than `sun_path` holds.
**Verification**: Test

## System calls

### WINE-SYS-1 No invented success

**Rule**: A system call case `adv-nt` does not handle MUST stop the machine with a message naming it. It MUST NOT return a success status for work it did not do.
**Verification**: Review — the rule concerns every unhandled branch, which no test enumerates.

### WINE-SYS-2 Standard handles

**Rule**: Each host descriptor the caller gives for standard input, output or error MUST become a server handle in the process parameters. A missing descriptor MUST leave that handle zero.
**Verification**: Test

### WINE-SYS-3 Section views

**Rule**: A read-only or read-write view of a data section MUST share the file's pages with every other mapping, the server's included. A view MUST start at the requested offset, which MUST be a multiple of 64 KiB.
**Verification**: Test

### WINE-SYS-4 One process per machine

**Rule**: `NtCreateUserProcess` MUST fail with `STATUS_NOT_SUPPORTED`. Calls on the current process MUST accept its server handles as well as its pseudo-handle.
**Verification**: Test

win32u creates the process's own desktop window, as Wine does when explorer cannot start.

### WINE-SYS-6 Display driver

**Rule**: Process start MUST set `GraphicsDriver` of the null-GUID display device to the driver the embedder names, as explorer would. With `null`, windows MUST be created and draw, showing nowhere.
**Verification**: Test

### WINE-SYS-5 Host names and listings

**Rule**: `\??\unix` MUST name the host's root as a drive names its link. Directory queries MUST list matching entries in name order, preceded by `.` and `..`, and resume until restarted. Listings MUST expose Wine's hashed DOS aliases for names that need them.
**Verification**: Test

### WINE-SYS-7 Instruction coherence

**Rule**: For the current process, `NtFlushInstructionCache` MUST return success after prior guest writes become visible to instruction execution.
**Verification**: Test

### WINE-SYS-8 Same mapped files

**Rule**: `NtAreMappedFilesTheSame` MUST succeed for two addresses in one view, and otherwise MUST ask the server whether two image views map one file. A private allocation MUST give `STATUS_CONFLICTING_ADDRESSES`.
**Verification**: Test

### WINE-SYS-9 Unix stack alignment

**Rule**: A call from the host into Unix-side ELF code MUST start its arguments on a 16-byte boundary, as Wine's system call dispatcher does. Calls into Windows code MUST keep their exact stack.
**Verification**: Test

### WINE-SYS-10 Main thread stack

**Rule**: The main thread's stack MUST be as large as the stack reserve of the process's image, and no smaller than 1 MiB.
**Verification**: Test

### WINE-SYS-11 NT file names

**Rule**: NT file names MUST reject dot components and duplicate separators inside paths. A relative file name MUST resolve through its directory handle.
**Verification**: Test

## Unix libraries

### WINE-UNIX-1 Guest ELF libraries

**Rule**: A builtin module's Unix library MUST load as guest code when the module first asks for its function table. It MUST be relocated and initialized before the table is returned, and load at most once.
**Verification**: Test

### WINE-UNIX-2 Binding

**Rule**: An undefined symbol MUST bind to another loaded library first, then to ntdll.dll's export of that name, then to a host function. Unix libraries call every function as cdecl, so a stdcall export MUST bind through a trampoline.
**Verification**: Test

An ntdll.dll export with no known convention stops the load. A weak symbol nothing provides binds to zero.

### WINE-UNIX-3 win32k calls and callbacks

**Rule**: System calls from 0x1000 MUST run the function win32u registered with its arguments. `KeUserModeCallback` MUST run `KiUserCallbackDispatcher` on the same stack, and `NtCallbackReturn` MUST end that call with its result. It MUST restore the exception list the callback found.
**Verification**: Test

### WINE-UNIX-4 Shared libraries and files

**Rule**: `dlopen` of a bare name MUST look next to the Unix libraries already loaded. A Unix library MUST open host files only for reading, and map them as shared read-only views. `ntdll_get_data_dir` MUST name `share/wine` of that installation.
**Verification**: Test

FreeType is such a library: win32u opens it by name and draws TrueType text through it.

## Threads

### WINE-THR-1 One scheduling authority

**Rule**: Every guest thread MUST be a machine context, and every guest wait MUST block in the machine. The host thread MUST NOT block on a server pipe.
**Verification**: Test

### WINE-THR-2 Wakeups and APCs

**Rule**: A server wakeup MUST reach a waiting thread's context after every server run. A user APC queued to an alertable waiter MUST run on that thread before its wait returns.
**Verification**: Test

### WINE-THR-3 Suspension

**Rule**: Suspending a thread MUST stop its context before the caller next runs. Resuming it to a zero count MUST let it run.
**Verification**: Test

### WINE-THR-4 Termination

**Rule**: Terminating another thread, or a process's other threads before it exits, MUST end their contexts where they stand, with no detach work. Waiters MUST see each thread signaled with the given exit code.
**Verification**: Test

### WINE-THR-5 Debug register contexts

**Rule**: NT thread contexts MUST retain and return x86 debug registers. These registers MUST NOT trigger hardware breakpoints.
**Verification**: Test

## Time

### WINE-TIME-1 Shared clock page

**Rule**: The clocks in KUSER_SHARED_DATA MUST be refreshed as each system call enters and before each context runs. A guest that polls the page without a system call MUST see time pass.
**Verification**: Test

## Host DLL images

### WINE-DLL-1 Loader-owned entries

**Rule**: Host DLL exports MUST point inside an i386 PE32 image that Wine's loader maps. Entries MUST use their registered calling conventions and machine bindings without storing host addresses, at any load base.
**Verification**: Test

### WINE-DLL-2 Ordinary import calls

**Rule**: Host DLL import calls MUST read the image's IAT slot at call time and enter its target through `Outcome::call` and a continuation. Guest changes to the slot or target entry MUST intercept the call. Host code that a COM method or another path outside the image's entries enters MUST bind imports with the loader's image base.
**Verification**: Test

### WINE-DLL-3 Prefix installation

**Rule**: Host DLL installation MUST replace the prefix's system32 entry with a PE file carrying Wine's builtin signature. Installation MUST NOT modify the Wine distribution or follow an existing file symlink when writing.
**Verification**: Test

## Graphics

### WINE-GFX-1 DirectDraw memory DCs

**Rule**: On the Wine userland, a DirectDraw surface's DC MUST come from `D3DKMTCreateDCFromMemory` over the surface's own pixels, size, pitch and format. The call MUST go through the DirectDraw image's IAT. ReleaseDC MUST call `GdiFlush` and then `D3DKMTDestroyDCFromMemory` the same way.
**Verification**: Test

### WINE-GFX-2 Presented GDI drawing

**Rule**: What Wine's GDI draws into a DirectDraw surface MUST reach the presenter when the surface is blitted to the primary surface.
**Verification**: Test

### WINE-GFX-3 Window association

**Rule**: Wine MUST load our user driver by the display device's `GraphicsDriver` name. Its DllMain MUST hand win32u a function table, leaving the rest to Wine's null driver. Window geometry that DirectDraw uses MUST come from the window rectangles win32u reports to the driver.
**Verification**: Test

### WINE-GFX-4 Window surfaces

**Rule**: The driver MUST give each window except the desktop a window surface over pixels it owns, wrapped by `NtGdiDdDDICreateDCFromMemory`. A flush of a visible window MUST present the window's part of those pixels without copying them.
**Verification**: Test

### WINE-GFX-5 In-process desktop

**Rule**: When win32u starts explorer for its desktop, the process MUST NOT start. The calling thread MUST instead create the desktop and message windows as explorer does. It MUST then show the desktop over the virtual screen.
**Verification**: Test

### WINE-GFX-6 Direct3D 9 on Wine

**Rule**: Our Direct3D 9 MUST load as a PE image in place of Wine's. Its surface DCs MUST be memory DCs that call GDI through its own IAT. A presented back buffer MUST reach the presenter.
**Verification**: Test

### WINE-GFX-7 Frames published on waits

**Rule**: When a thread that drew a software frame waits, the frame MUST be published. A frame deferred to a UI's mailbox MUST NOT wait for a readback.
**Verification**: Test

### WINE-GFX-8 Disabled N-patches

**Rule**: Direct3D 9 MUST accept `SetNPatchMode(0)` without changing rendering state. Enabling N-patches MUST remain unavailable.
**Verification**: Test

## Input

### WINE-IN-1 Presented window coordinates

**Rule**: Host mouse points MUST map to screen coordinates through the last presented window. Window surfaces MUST use `Window::extent`; DirectDraw and Direct3D 9 MUST use `Window::client`.
**Verification**: Test

### WINE-IN-2 Hardware input

**Rule**: The user driver's `ProcessEvents` MUST deliver queued mouse and keyboard events through win32u as hardware input on the guest thread. Keyboard input MUST carry its virtual key, scan code, extended flag and release flag.
**Verification**: Test

### WINE-IN-3 Input wakes message waits

**Rule**: Each input thread MUST register its event pipe as Wine's queue fd. Queuing host input MUST run the server and deliver wake-ups even when every guest context waits. Input waits MUST NOT use periodic polling.
**Verification**: Test

## Native UI and control

### WINE-UI-1 Native popup presentation

**Rule**: Shown `#32768` windows MUST project their `MN_GETHMENU` items natively relative to the presented window's content, without presenting frames. Their completion tokens MUST become inactive when tracking ends.
**Verification**: Test

### WINE-UI-2 Popup input

**Rule**: Host popup choices MUST click Wine's item rectangles, opening ancestor submenus in order. Cancellation MUST end Wine's menu loop. Wine MUST retain `TPM_RETURNCMD`, `TPM_NONOTIFY`, and menu notification behavior.
**Verification**: Test

### WINE-UI-3 Frame control

**Rule**: Headless Wine control MUST support `await-frame` and `resume` through the shared frame gate. A matched or failed gate MUST freeze guest execution before acknowledgment.
**Verification**: Test

### WINE-UI-4 Memory inspection

**Rule**: Wine control `memory` MUST read the presented process's guest memory and report read failures.
**Verification**: Test

### WINE-UI-5 Metrics and reports

**Rule**: Wine control MUST support `reset-metrics` and `report`. Reset MUST restart frame observations, first-frame timing, and supported wait statistics. Reports MUST omit sections requiring unavailable stack data.
**Verification**: Test

## Audio

### WINE-AUD-1 Builtin render driver

**Rule**: `wineadvaudio.drv` MUST expose one render endpoint and no capture, MIDI or aux devices. Its Unix functions MUST use the ordinary Unix-call dispatcher.
**Verification**: Test

### WINE-AUD-2 Shared mixer

**Rule**: Wine render streams MUST play guest ring buffers through `adv-device`'s mixer. Padding and position MUST follow its play cursor and playback clock.
**Verification**: Test

### WINE-AUD-3 Period waits

**Rule**: A stream's timer loop MUST wait on the machine clock and signal its event each period. Releasing the stream MUST end its timer thread.
**Verification**: Test

### WINE-AUD-4 Session output

**Rule**: A session MUST own its audio control and expose its mixed PCM stream. Windowed shells MUST open host audio output. Headless shells MUST NOT open host audio output.
**Verification**: Test

## Sessions

### WINE-SES-1 Shared prefix

**Rule**: Every game MUST run on the player's one prefix, kept between runs, with drive C: inside it or where the embedder places it. Each start MUST relink system32 and the template's module copies to the Wine installation. Drive D: MUST be the program's directory, which the guest writes in place.
**Verification**: Test

### WINE-SES-2 Host wakes

**Rule**: While every guest context waits, a session MUST serve finished host work before it blocks. A host wake MUST end the block. The run loop MUST take the wake's interrupt before it drains host work.
**Verification**: Test

### WINE-SES-3 Provisioned prefix

**Rule**: A new prefix MUST start as a copy of the installation's template, `share/hitode/prefix`, and a session MUST NOT start without one. Each start MUST first run `wineboot.exe --restart` to its exit as its own process on the session's server. A process MUST start with its own variables, then those Wine's `add_registry_environment` adds.
**Verification**: Test

`tools/wine/prefix-template.sh` makes the template with standard Wine's `wineboot --init`. The restart creates the volatile keys and the known DLLs before the program starts. The template is a distribution artifact, made where standard Wine runs, and not part of the repository.

### WINE-SES-4 Fixed identity

**Rule**: What a guest learns about its user and machine MUST NOT come from the host: the user MUST be `Player` and the computer `HITODE`. The firmware tables MUST name the virtual machine and the emulated processor, with no serial numbers or UUID. Name resolution through `getaddrinfo` MUST fail.
**Verification**: Test

### WINE-SES-5 Regional text

**Rule**: A session's system and user locales MUST be those of the game's ANSI code page: ja-JP for 932, zh-CN for 936. Win32u MUST load the host's system fonts in place, and the prefix MUST replace the CJK faces Windows ships with host families.
**Verification**: Review — which host fonts a game shows is seen on its own screens.

### WINE-SES-6 Installed system subdirectories

**Rule**: The prefix template MUST retain Wine-installed subdirectories of `system32`, including their data and module links.
**Verification**: Test

## Conformance

### WINE-TEST-1 Wine's own tests

**Rule**: Listed Wine conformance tests that pass on standard Wine MUST pass here with no failures. Each runs as its own program without arguments.
**Verification**: Test

## Registry, process and job calls

### WINE-NT-301 Registry hive operations

**Rule**: Registry rename, transacted opens, and hive operations MUST follow Wine's argument checks and server requests. Hive files MUST use the guest's file handles.
**Verification**: Test

### WINE-NT-302 Process and thread operations

**Rule**: Process enumeration, suspension, thread alerts, continuation, and APC calls MUST follow Wine's behavior within `WINE-SYS-4`. Suspension MUST update machine contexts.
**Verification**: Test

### WINE-NT-303 Jobs and completions

**Rule**: Job membership, information, termination, and completion calls MUST use Wine's checks and server requests. Terminating the current process's job MUST end the machine process.
**Verification**: Test
## File calls

### WINE-NT-101 File descriptor operations

**Rule**: File flushing, deletion, mailslot creation, and scatter/gather I/O MUST follow Wine 11's checks, statuses, server requests, and side effects.
**Verification**: Test

### WINE-NT-102 File waits and cancellation

**Rule**: File locks, directory notifications, and I/O cancellation MUST follow Wine 11. Their waits MUST use the machine's scheduler.
**Verification**: Test

### WINE-NT-103 Device controls

**Rule**: Disk-family device controls MUST return Wine's results without host disk devices. Other device controls MUST reach the server.
**Verification**: Test

### WINE-NT-104 File stubs

**Rule**: `NtQueryEaFile`, `NtSetEaFile`, and `NtSetVolumeInformationFile` MUST preserve Wine 11's stub checks, statuses, and side effects.
**Verification**: Review — the package probe omits Wine's stubs.
## Token and object security calls

### WINE-NT-201 Tokens and privileges

**Rule**: Token creation, opening, filtering, privilege adjustment, privilege checks, and default DACL updates MUST match Wine 11.0's server requests and results. Token users MUST remain guest identities independent of host names and users.
**Verification**: Test

### WINE-NT-202 Security descriptors and access

**Rule**: Access checks and object security queries and updates MUST preserve Wine 11.0's descriptor conversion, length checks, statuses, and server effects.
**Verification**: Test

### WINE-NT-203 Object identity and lifetime

**Rule**: Object comparison and reserve allocation MUST use Wine 11.0's server requests. Permanent and temporary objects MUST send `set_object_permanence` and retain the server's lifetime behavior.
**Verification**: Test

### WINE-NT-204 Security stubs

**Rule**: Security calls stubbed by Wine 11.0 MUST preserve its fixed statuses and output writes.
**Verification**: Review — the call list is compared with Wine's Unix security and server implementations.
## System, memory and debug calls

### WINE-NT-401 Extended section views

**Rule**: Extended section calls MUST follow Wine 11.0 parameter checks, shared mappings and placeholder replacement. Preserving an unmapped placeholder MUST retain its reservation.
**Verification**: Test

### WINE-NT-402 Memory services

**Rule**: Memory flush, lock, prefetch and WoW64 services MUST follow Wine 11.0 statuses, lengths and server requests. Process write barriers MUST order prior guest writes.
**Verification**: Test

### WINE-NT-403 Guest system state

**Rule**: Locale and execution-state updates MUST affect only guest state. License queries MUST read `\Registry\Machine\Software\Wine\LicenseInformation` with Wine's type and length reporting.
**Verification**: Test

### WINE-NT-404 Debug services

**Rule**: Debug attachment, continuation, object information and event waits MUST use Wine's server requests. Debug waits MUST block through machine scheduling.
**Verification**: Test

### WINE-NT-405 System stubs

**Rule**: M-system fixed stubs MUST retain Wine 11.0 statuses and output writes. Syscall-table stubs MUST raise Wine's noncontinuable stub exception. Unsupported LDT updates MUST return `STATUS_NOT_IMPLEMENTED`.
**Verification**: Review — the implementations are compared with Wine's Unix sources and syscall stub table.
