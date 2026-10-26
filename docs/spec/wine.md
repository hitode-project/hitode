# Wine userland

ID prefix: WINE
ID prefix: SRV
ID prefix: NT
ID prefix: NAT
ID prefix: CODEC

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

## Server channels

### SRV-CHAN-1 Memory channels

**Rule**: Request, reply and wait pipe bytes MUST travel through shared memory without channel read or write system calls.
Registered descriptors MUST retain pipe read, close and readiness behavior across descriptor transfers and reuse.
Wake delivery MUST visit only threads whose wait queues received bytes.
**Verification**: Test

## Server event loop

### SRV-EVENT-1 Incremental server events

**Rule**: The server MUST use kqueue on Apple targets and epoll on Android, keeping channel registrations outside the kernel.
Event waits MUST merge queue readiness with kernel events using zero kernel timeouts and yield to the runtime when nothing is ready.
Channel registrations MUST preserve event data, enablement and close behavior.
**Verification**: Test

### SRV-EVENT-2 Resume only for server input

**Rule**: Waiting servers MUST resume only for changed channel input, an expired deadline, kernel readiness, or an immediate continuation.
Kernel readiness MUST wake the session and gate zero-timeout kernel queries without delaying ready descriptors.
Descriptors that remain ready MUST remain observable on later steps.
**Verification**: Test
**Basis**: Wine 11 `server/fd.c`; WINE-THR-1

## Server synchronization

### SRV-SYNC-1 In-process object operations

**Rule**: Synchronization operations MUST preserve Wine 11's object state, access checks, return values, and fallback behavior.
**Verification**: Test
**Basis**: Wine 11 `dlls/ntdll/unix/sync.c`; Linux `Documentation/userspace-api/ntsync.rst`

### SRV-SYNC-2 Atomic machine waits

**Rule**: Pending device waits MUST park guest contexts in the machine. Wait-all MUST acquire every object atomically. Pulsing an event MUST satisfy eligible registered waiters before resetting it.
**Verification**: Test
**Basis**: WINE-THR-1, WINE-THR-2

### SRV-SYNC-3 Descriptor ownership

**Rule**: Device objects MUST survive descriptor transfer and pending waits until their last reference closes. Closing a handle MUST invalidate its synchronization cache entry. Other descriptors and paths MUST retain libc behavior.
**Verification**: Test

### SRV-SYNC-4 Every event signaller

**Rule**: Every host path that sets or resets an event, including file I/O completion, MUST use its in-process sync first. The server asserts on event operations for those objects.
**Verification**: Test
**Basis**: Wine 11 `server/event.c`

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
Search expressions MUST recognize the NT DOS wildcard tokens `<`, `>` and `"` alongside `*` and `?`.
Directory query `BOOLEAN` arguments MUST use only the low byte of their stack slot.
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
DOS device paths without a host drive MUST resolve through the server's symbolic links.
**Verification**: Test

### WINE-SYS-12 Indexed path lookups

**Rule**: Case-insensitive directory misses without possible hashed aliases MUST avoid listings. Indexed lookups MUST preserve case folding, alias rules, and entry precedence. Indexes MUST revalidate directory inode and nanosecond modification time, evict least recently used directories beyond 256, and invalidate directories for NT mutations.
**Verification**: Test
**Basis**: Wine 11.0 `find_file_in_dir` and `get_dir_case_sensitivity`.

### WINE-SYS-13 Registry value cache

**Rule**: Cached registry reads MUST preserve information classes, buffer negotiation and value contents. Registry mutations and handle retirement through typed or raw requests MUST invalidate caches across the process family. Caches MUST retain at most 256 complete values per process, each at most 4096 bytes.
**Verification**: Test

## NT files

NT services follow Wine 11.0's argument checks, server requests and status codes.

### NT-FILE-1 File descriptor operations

**Rule**: File flushing, deletion, mailslot creation, and scatter/gather I/O MUST follow Wine 11's checks, statuses, server requests, and side effects.
**Verification**: Test

### NT-FILE-2 File waits and cancellation

**Rule**: File locks, directory notifications, and I/O cancellation MUST follow Wine 11. Their waits MUST use the machine's scheduler.
**Verification**: Test

### NT-FILE-3 Device controls

**Rule**: Disk-family device controls MUST return Wine's results without host disk devices. Other device controls MUST reach the server.
**Verification**: Test

### NT-FILE-4 File stubs

**Rule**: `NtQueryEaFile`, `NtSetEaFile`, and `NtSetVolumeInformationFile` MUST preserve Wine 11's stub checks, statuses, and side effects.
**Verification**: Review — the probe omits Wine's stubs.

## NT security

### NT-SEC-1 Tokens and privileges

**Rule**: Token creation, opening, filtering, privilege adjustment, privilege checks, and default DACL updates MUST match Wine 11.0's server requests and results. Token users MUST remain guest identities independent of host names and users.
**Verification**: Test

### NT-SEC-2 Security descriptors and access

**Rule**: Access checks and object security queries and updates MUST preserve Wine 11.0's descriptor conversion, length checks, statuses, and server effects.
**Verification**: Test

### NT-SEC-3 Object identity and lifetime

**Rule**: Object comparison and reserve allocation MUST use Wine 11.0's server requests. Permanent and temporary objects MUST send `set_object_permanence` and retain the server's lifetime behavior.
**Verification**: Test

### NT-SEC-4 Security stubs

**Rule**: Security calls stubbed by Wine 11.0 MUST preserve its fixed statuses and output writes.
**Verification**: Review — the call list is compared with Wine's Unix security and server implementations.

## NT registry, processes and jobs

### NT-PROC-1 Registry hive operations

**Rule**: Registry rename, transacted opens, and hive operations MUST follow Wine's argument checks and server requests. Hive files MUST use the guest's file handles.
**Verification**: Test

### NT-PROC-2 Process and thread operations

**Rule**: Process enumeration, suspension, thread alerts, continuation, and APC calls MUST follow Wine's behavior within `WINE-SYS-4`. Suspension MUST update machine contexts.
**Verification**: Test

### NT-PROC-3 Jobs and completions

**Rule**: Job membership, information, termination, and completion calls MUST use Wine's checks and server requests. Terminating the current process's job MUST end the machine process.
**Verification**: Test

## NT system, memory and debugging

### NT-SYS-1 Extended section views

**Rule**: Extended section calls MUST follow Wine 11.0 parameter checks, shared mappings and placeholder replacement. Preserving an unmapped placeholder MUST retain its reservation.
**Verification**: Test

### NT-SYS-2 Memory services

**Rule**: Memory flush, lock, prefetch and WoW64 services MUST follow Wine 11.0 statuses, lengths and server requests. Process write barriers MUST order prior guest writes.
**Verification**: Test

### NT-SYS-3 Guest system state

**Rule**: Locale and execution-state updates MUST affect only guest state. License queries MUST read `\Registry\Machine\Software\Wine\LicenseInformation` with Wine's type and length reporting.
**Verification**: Test

### NT-SYS-4 Debug services

**Rule**: Debug attachment, continuation, object information and event waits MUST use Wine's server requests. Debug waits MUST block through machine scheduling.
**Verification**: Test

### NT-SYS-5 System stubs

**Rule**: Fixed system stubs MUST retain Wine 11.0 statuses and output writes. Syscall-table stubs MUST raise Wine's noncontinuable stub exception. Unsupported LDT updates MUST return `STATUS_NOT_IMPLEMENTED`.
**Verification**: Review — the implementations are compared with Wine's Unix sources and syscall stub table.

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

**Rule**: Except for host libraries, `dlopen` of a bare name MUST look next to the Unix libraries already loaded. A Unix library MUST open host files only for reading, and map them as shared read-only views. `ntdll_get_data_dir` MUST name `share/wine` of that installation.
**Verification**: Test

FreeType is a host library under WINE-UNIX-6.

### WINE-UNIX-5 Fixed-point formatting

**Rule**: Unix library printf-family functions MUST consume `%f` and `%F` arguments as i386 doubles, preserving flags, width, precision and following arguments. Bounded output MUST retain the full formatted length and terminate any nonempty buffer.
**Verification**: Test

### WINE-UNIX-6 Host FreeType

**Rule**: Wine's `libfreetype.so.6` requests MUST resolve to host FreeType 2.13.2 with the distribution's rendering defaults. Guest records and referenced buffers MUST use i386 layouts in guest heap memory and reflect completed host mutations. Glyph bytes, metrics and function results MUST match the distribution's i386 FreeType.
**Verification**: Test
**Basis**: Wine 11.0 `win32u/freetype.c` and FreeType 2.13.2 public headers.

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

## Native bodies

`adv-nt::identity` matches mapped distribution images and functions against code identity tables.
`adv-nt::native` binds bodies to the confirmed functions.

### NAT-ID-1 Code identity

**Rule**: A code identity table MUST describe a mapped image only when the hash of the image's headers equals the table's. A function MUST be bound to a native body only when its bytes, with base relocations masked, hash to the table's value.
**Verification**: Test

### NAT-ID-2 Unix code identity

**Rule**: Unix i386 ELF identity tables MUST include local function symbols and mask dwords patched by dynamic relocations. Unix modules MUST bind bodies only after relocation, with matching ELF headers, program headers, and function bytes. A declined PIC entry call MUST preserve its original return address.
**Verification**: Test

### NAT-BODY-1 Body semantics

**Rule**: A native body that accepts a call MUST leave the guest state Wine's function would leave. Registers and flags the calling convention does not keep are exempt. Otherwise it MUST decline before any side effect, and the call MUST run Wine's function from its entry with the caller's frame.
**Verification**: Test

### NAT-BODY-2 Performance counter body

**Rule**: The native body of `ntdll.dll!NtQueryPerformanceCounter` MUST use the system call's counter computation and machine clock. It MUST return success and write the counter and optional frequency as the system call does. A null counter or an unwritable output MUST make the body decline before any write.
**Verification**: Test

### NAT-BODY-3 DirectSound mixer

**Rule**: The DirectSound mixer body MUST preserve Wine's float samples, scratch data, buffer positions, frequency accumulators, states, shared lock words, and stopped result. It MUST decline before writes when locks would wait, notifications would fire, effects or 3D apply, conversions are unsupported, or required memory is inaccessible. Its field offsets MUST match the pinned module's DWARF.
**Verification**: Test

### NAT-BODY-4 DIB primitives

**Rule**: DIB bodies MUST preserve Wine's pixels, padding, overlap traversal, bit fields, palette selection, blend arithmetic, and interpolation rounding. Unsupported formats or inaccessible memory MUST decline before writes. Field offsets and data addresses MUST match the pinned Unix module's DWARF.
**Verification**: Test

### NAT-BODY-5 Code page bodies

**Rule**: PE code page bodies MUST use Wine's loaded conversion and normalization tables and preserve its output bytes, lengths, default-character indicators and errors. Their structure offsets and global addresses MUST match the pinned modules' DWARF. Cached-page lookups MUST decline while Wine's locale lock is held.
**Verification**: Test

### NAT-BODY-6 CRT bodies

**Rule**: Locale-independent PE string and memory bodies MUST preserve Wine's bytes, return values, bounds and overlap behavior. An access that would fault MUST decline before any write.
**Verification**: Test

### NAT-BODY-7 PE body memory access

**Rule**: PE string and conversion bodies MUST bound guest memory calls by pages and tables touched, rather than input units. Short calls fitting within one page of strings or eight KiB of conversion output MUST NOT allocate heap buffers.
**Verification**: Test

### NAT-BODY-8 D3DX pixel conversion

**Rule**: D3DX pixel bodies MUST preserve channel values, filter coordinates, color keys and padding with layouts matching the pinned module's DWARF. Unsupported formats or filters, overlapping buffers and inaccessible memory MUST decline before writes. Changed helper code or copy imports MUST retain guest execution.
**Verification**: Test

### NAT-BODY-9 PNG row bodies

**Rule**: PNG row bodies MUST preserve Wine's Sub, Up, Average and Paeth bytes and row metadata through the shared device kernels. Missing kernels, overlapping rows and inaccessible memory MUST retain guest execution before writes. Row metadata offsets MUST match the pinned module's DWARF.
**Verification**: Test

### NAT-BODY-10 Embedded zlib checksums

**Rule**: Adler-32 bodies MUST preserve the pinned zlib's seeded checksum, short-input arithmetic and null-buffer initialization. Inaccessible input MUST retain guest execution.
**Verification**: Test

### NAT-BODY-11 DEFLATE fast batches

**Rule**: Native DEFLATE batches MUST preserve Wine's output, consumed input, bit reservoir, stream pointers and block-end state. Rejected batches MUST retain guest execution before guest writes. Stream and window layouts MUST match the pinned module's DWARF.
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
Child processes MUST defer native GPU presentation to the session's consumer before drawing.
**Verification**: Test

### WINE-GFX-7 Frames published on waits

**Rule**: When a thread that drew a software frame waits, the frame MUST be published. A frame deferred to a UI's mailbox MUST NOT wait for a readback.
**Verification**: Test

### WINE-GFX-8 Disabled N-patches

**Rule**: Direct3D 9 MUST accept `SetNPatchMode(0)` without changing rendering state. Enabling N-patches MUST remain unavailable.
**Verification**: Test

### WINE-GFX-9 Display geometry

**Rule**: The driver MUST report one 2560x1440 monitor, so win32u never clamps a framed 1920x1080 game window. The mode list MUST include it. Its work area MUST leave out a 40-pixel taskbar at the bottom.
**Verification**: Test

## Input

### WINE-IN-1 Presented window coordinates

**Rule**: Host mouse points MUST map to screen coordinates through the origin of the last presented window's content. Every presenter's frames carry only that content; the host shows caption, frame and menu bar natively.
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

**Rule**: Wine control MUST support `reset-metrics` and `report`. Reset MUST restart frame observations, first-frame timing, and supported wait statistics. Reports MUST include actual stop reasons, full guest exit codes, and failures, omitting sections requiring unavailable stack data.
**Verification**: Test

### WINE-UI-6 Live debugger

**Rule**: Wine sessions MUST serve the shared debugger when requested, including while paused or frame-frozen. Boundary reads MUST inspect the presented process without running guest code. Unsupported API diagnostics MUST be explicit.
**Verification**: Test

### WINE-UI-7 Native dialog presentation

**Rule**: A shown top-level window MUST be read before its first flush. A dialog the host projects, such as a message box, MUST NOT present frames. Its icon MUST NOT show as an unavailable control.
**Verification**: Test

## Audio

### WINE-AUD-1 Builtin render driver

**Rule**: `wineadvaudio.drv` MUST expose one render endpoint, one software MIDI output, and no capture, MIDI input or aux devices. Its Unix functions MUST use the ordinary Unix-call dispatcher.
**Verification**: Test

### WINE-AUD-2 Shared mixer

**Rule**: Wine render streams MUST play guest ring buffers through `adv-device`'s mixer. Padding and position MUST follow its play cursor and playback clock.
**Verification**: Test

### WINE-AUD-3 Period waits

**Rule**: A stream's timer loop MUST wait on the machine clock and signal its event each period. Releasing the stream MUST end its timer thread. The default device period MUST be 20 ms, so clients that queue three periods hold 60 ms across host callback bursts.
**Verification**: Test

### WINE-AUD-4 Session output

**Rule**: A session MUST own its audio control and expose its mixed PCM stream. Windowed shells MUST open host audio output. Headless shells MUST NOT open host audio output.
**Verification**: Test

### WINE-AUD-5 MIDI output messages

**Rule**: MIDI output MUST support capability queries, open/close, prepared long messages, short messages, reset and stereo volume. Successful open, close and long-message submission MUST deliver the corresponding WinMM callbacks. Failed requests MUST NOT create an output handle or deliver a completion callback.
**Verification**: Test

### WINE-AUD-6 MIDI synthesis

**Rule**: MIDI notes and controllers MUST produce PCM through the shared mixer with a bundled General MIDI bank. Wine MUST retain stream timing, pause/resume and MCI sequencing. Closing MIDI output MUST remove its queued PCM.
**Verification**: Test

### WINE-AUD-7 Native DirectSound

**Rule**: `dsound.dll` MUST be the built-in stack's DirectSound front, with Wine's export names and ordinals. It MUST mix on the host through the session's audio control, and capture MUST report no driver. Position notifications MUST be signalled on device time, bounding the session's idle wait.
**Verification**: Test

## Media

### WINE-MEDIA-1 Host media seam

**Rule**: Wine's unchanged PE media modules MUST use host tables implementing Wine 11's winegstreamer and winedmo Unix interfaces over `adv-device::media`. Muxer creation MUST fail when no encoder exists.
**Verification**: Test

### WINE-MEDIA-2 Decoder waits

**Rule**: Media input and sample waits MUST block guest contexts through machine objects. Host decoder notifications MUST wake the session without execution-thread blocking or timer polling.
**Verification**: Test

### WINE-MEDIA-3 Demand-driven decoding

**Rule**: Decoding MUST run on host workers with at most a few decoded frames queued per stream. Disabled, paused and unread streams MUST NOT drive decoding.
**Verification**: Test

### WINE-MEDIA-4 Hardware decoding

**Rule**: Apple hosts MUST use VideoToolbox for supported streams and fall back to software per stream. Apple FFmpeg profiles MUST include the matching hardware accelerators. Android MAY decode in software.
**Verification**: Review — hardware availability and fallback require platform runs.

### WINE-MEDIA-5 Guest frame writes

**Rule**: Each delivered frame MUST be written once into guest memory. Conversion and scaling MUST write directly into the requested guest buffer at its negotiated format and stride.
**Verification**: Review — memory ownership and conversion destinations require source inspection.

### WINE-MEDIA-6 Native pixel negotiation

**Rule**: Parser negotiation MUST report native YUV before RGB wherever Wine lets the consumer choose. Guest code MUST NOT perform per-pixel conversion.
**Verification**: Test

### WINE-MEDIA-7 Reproducible media distribution

**Rule**: The Wine recipe MUST build winegstreamer's PE module with the distribution's flags, install it, strip debug information, and register its classes. Adding the module MUST NOT change other distribution binaries.
**Verification**: Review — distribution checksums and prefix regeneration are packaging checks.

## Static codec kernels

Static Vorbis and Theora bindings run on the Wine stack.
Device decoding follows [device.md](device.md).

### CODEC-MAP-1 Image binding

**Rule**: Non-distribution images MUST be scanned once per mapping, using executable ranges and the reviewed codec signatures. Distribution images and guest mode MUST NOT be scanned. Unmapping MUST retire bindings and their decoder state.
**Verification**: Test

### CODEC-CALL-1 Vorbis transforms

**Rule**: Native Vorbis transforms MUST retain the built-in stack's admission, ABI, table, buffer, and floating-environment checks. Declined calls MUST preserve guest memory and resume the original function through its trampoline. Verify mode MUST publish guest results and preserve guest floating state.
**Verification**: Test

### CODEC-CALL-2 Theora packets

**Rule**: Theora bindings MUST retain complete library-family admission and preserve pixels, state, guest allocation lifetime, return values, and callbacks. Calls without admitted decoder state MUST resume guest code before changing guest memory. Verify mode MUST compare native results while publishing guest results.
**Verification**: Test

## Sessions

### WINE-SES-10 Sequential sessions

**Rule**: Sequential sessions on one host MUST share their prefix server while owning separate guest machines and mount sources. Ending a session MUST release every guest process and save registry changes before another session starts. Failed startup and forced stops MUST leave the host reusable.
**Verification**: Test

### WINE-SES-1 Shared prefix

**Rule**: Every game MUST run on the player's one prefix, kept between runs, with drive C: inside it or where the embedder places it. A host MUST provision system32 and the template's module links before first using a drive C:. The program directory MUST use D: or the first unmounted letter after it, with writes in place.
Process startup MUST register the prefix's drive letters in the server namespace.
**Verification**: Test

### WINE-SES-2 Host wakes

**Rule**: While every guest context waits, a session MUST serve finished host work before it blocks. A host wake MUST end the block. The run loop MUST take the wake's interrupt before it drains host work.
UI projection refreshes MUST wake execution only when they queue new guest work.
**Verification**: Test

### WINE-SES-3 Provisioned prefix

**Rule**: A host MUST provision a new prefix from the installation's template, `share/hitode/prefix`, before starting its server. A host MUST run `wineboot.exe --restart` successfully once on its server before discovering game fonts and starting games. A process MUST start with its own variables, then those Wine's `add_registry_environment` adds.
**Verification**: Test

`tools/wine/prefix-template.sh` makes the template with standard Wine's `wineboot --init`. The restart creates the volatile keys and known DLLs for the server's lifetime. RunOnce and pending rename operations run at that boot boundary. The template is a distribution artifact, made where standard Wine runs, and not part of the repository.

### WINE-SES-4 Fixed identity

**Rule**: What a guest learns about its user and machine MUST NOT come from the host: the user MUST be `Player` and the computer `HITODE`. The firmware tables MUST name the virtual machine and the emulated processor, with no serial numbers or UUID. Name resolution through `getaddrinfo` MUST fail.
**Verification**: Test

### WINE-SES-5 Regional text

**Rule**: A session's system and user locales MUST follow its ANSI code page: ja-JP for 932, zh-CN for 936. Wine MUST load fonts discovered by the portable backend, including nested directories. Missing standard families MUST use its regional matching order, require the declared character set, and prefer actual TrueType outlines without overriding installed families.
**Verification**: Test

macOS simplified Chinese matching prefers Heiti SC before Songti SC. Songti's
Black and Bold faces also declare the Regular selection flag; Wine can choose
Black for a normal-weight request to that family.

### WINE-SES-6 Installed system subdirectories

**Rule**: The prefix template MUST retain Wine-installed subdirectories of `system32`, including their data and module links.
**Verification**: Test

### WINE-SES-7 Execution settings

**Rule**: Both shells MUST apply the same engine policy and diagnostic settings. Wine MUST honor frame limits, shader capabilities, shader cache location, and physical audio enablement.
**Verification**: Test

### WINE-SES-8 Host font choices

**Rule**: Explicit host font choices MUST override matching font-creation requests without changing caller buffers, including when the requested family is installed. Font files, substitutions, and smoothing MUST apply before rendering and follow child processes. Session font links, replacements, substitutions, and smoothing MUST NOT affect later sessions.
**Verification**: Test

### WINE-SES-9 Mounted file ownership

**Rule**: Wine sessions MUST apply ISO mounts and installation-directory mappings before process startup. Files, directories, sections, children, and volume queries MUST retain the same mounted backing. Reused prefixes MUST NOT retain removed host mappings.
**Verification**: Test

## Conformance

### WINE-TEST-1 Wine's own tests

**Rule**: Listed Wine conformance tests that pass on standard Wine MUST pass here with no failures. Each runs as its own program without arguments.
**Verification**: Test

### WINE-TEST-2 Stack switch comparison

**Rule**: Stack comparisons MUST run sequentially with the same executable, duration, headless GPU, audio, and automatic-dialog settings. The default gate MUST reject Wine CPU per frame exceeding 105% of the old stack's, or frame counts below 90%. Unsuccessful runs, missing summaries, and zero-frame measurements MUST NOT pass.
**Verification**: Test

## Native video

### WINE-MEDIA-8 Native DirectShow graphs

**Rule**: Wine's `quartz.dll` MUST activate implemented DirectShow classes through `adv_dx::dshow`. The DMO wrapper MUST activate through `qasf.dll`. Native file graphs MUST decode through `adv_device` and mix audio through the session's native DirectSound service.
**Verification**: Test

### WINE-MEDIA-9 Native synchronous reader

**Rule**: Wine's `WMCreateSyncReader` MUST use `adv_dx::wmvcore` for filename opening, compressed samples and decoded outputs. File reads and decoding MUST NOT block the execution owner.
**Verification**: Test

### WINE-MEDIA-10 Wine video geometry

**Rule**: Native video layers MUST use the driver's Wine HWND geometry and inherited visibility. Presented pixels MUST occupy the owner's position within the presented canvas.
**Verification**: Test

### WINE-MEDIA-11 Host playback service

**Rule**: Native renderer graphs MUST progress on host time without a guest streaming thread. The session MUST service video deadlines while guest threads wait. Completion MUST follow decoded EOF and queued audio playback.
**Verification**: Test

### WINE-MEDIA-12 Wine media fallback

**Rule**: Native media images MUST preserve Wine's export names and ordinals. Unimplemented exports and quartz classes MUST delegate to Wine's retained original modules. External audio-renderer activation MUST retain Wine's push-sample support.
**Verification**: Test

## Native WinMM

### WINE-AUD-8 Native wave output

**Rule**: Native PCM and float wave output MUST consume queued headers on device time through the session mixer. Pausing, restarting, resetting and breaking loops MUST preserve Wine 11's header flags, ordering and positions.
**Verification**: Test
**Basis**: Wine 11 `dlls/winmm/waveform.c`.

### WINE-AUD-9 Callback delivery

**Rule**: Header completion MUST progress while all guest threads wait. Function completions MUST run on a guest worker. Window, thread and event callbacks MUST retain Wine's arguments and delivery order.
**Verification**: Test
**Basis**: Wine 11 `DriverCallback` and `WINMM_NotifyClient`.

### WINE-AUD-10 Named sounds

**Rule**: Memory, resource, file and alias sounds MUST play through native wave output. Synchronous calls MUST wait through the machine. Asynchronous looping and stopping MUST run on host time.
**Verification**: Test
**Basis**: Wine 11 `dlls/winmm/playsound.c`.

### WINE-AUD-11 Remaining Wine exports

**Rule**: The WinMM image MUST retain Wine 11's export names and ordinals. Unimplemented exports MUST call the original Wine module, loadable under a second name. Wine MCI wave playback MUST reach native wave output.
`timeGetTime` MUST use the machine clock and its shared clock-poll admission.
**Verification**: Test
**Basis**: Wine 11 `dlls/winmm/winmm.spec`.

### WINE-AUD-12 Native MIDI streams

**Rule**: MIDI streams MUST sequence short, long, tempo and callback events on host time through the Wine stack's synthesizer. Pause, restart, stop, properties and position MUST retain Wine 11's results. Session idle deadlines MUST include native audio work.
**Verification**: Test
**Basis**: Wine 11 `dlls/winmm/winmm.c`.

## Native XAudio2

### WINE-XAUDIO-1 Native XAudio2 graph

**Rule**: XAudio2 2.7, 2.8 and 2.9 voices MUST mix through the session's `AudioControl` on device time. Source sends through submix voices MUST apply voice volumes, channel volumes and output matrices. Send filters MUST process samples before accumulation into destination voices.
**Verification**: Test
**Basis**: Wine 11.0 `xaudio2_7` and FAudio.

### WINE-XAUDIO-2 Versioned activation

**Rule**: XAudio2 2.7 MUST activate through its normal and debug COM classes. XAudio2 2.8 and 2.9 MUST activate through `XAudio2Create`. Earlier XAudio2 modules MUST remain Wine's.
**Verification**: Test

### WINE-XAUDIO-3 Callback delivery

**Rule**: Engine and voice callbacks MUST execute on one guest worker in Wine's order, with submitted buffer contexts. Late callbacks MUST NOT stall device consumption.
**Verification**: Test

### WINE-XAUDIO-4 Native volume meter

**Rule**: A volume meter attached to a native voice MUST report that voice's played peak and RMS levels.
**Verification**: Test

### WINE-XAUDIO-5 Foreign effect compatibility

**Rule**: An unsupported effect chain MUST select Wine's original engine before audio flows. Forwarded voices MUST preserve the engine's graph and submitted buffers.
**Verification**: Test

### WINE-XAUDIO-6 Source sequencing

**Rule**: Native sources MUST honor play ranges, loops, frequency ratios, start, stop, flush, discontinuity and loop exit. Operation sets MUST apply together when committed.
**Verification**: Test

### WINE-XAUDIO-7 Native reverb

**Rule**: Supported audio reverb MUST process on the host using Wine 11.0's FAudio DSP and parameters. Effects MUST precede voice gain. Unsupported reverb layouts MUST NOT enter the native DSP.
**Verification**: Test

### WINE-XAUDIO-8 Source formats

**Rule**: Native sources MUST accept PCM, IEEE float, Microsoft ADPCM and xWMA buffers. Compressed decoding MUST use host code.
**Verification**: Test
