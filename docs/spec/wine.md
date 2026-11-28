# Wine userland

ID prefix: WINE
ID prefix: SRV
ID prefix: NT

This spec covers the Wine userland stack. `adv-wineserver` runs Wine's server inside the host process. `adv-nt` takes the place of Wine's `ntdll.so` under Wine's own Windows DLLs. `adv-unix` runs Wine's other Unix libraries, such as `win32u.so`, as guest code. [README.md](README.md) explains the format. Native bodies and static codec kernels are in [wine-native.md](wine-native.md).

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

### WINE-SRC-4 Userland patches

**Rule**: The Wine distribution MUST build from the pinned release source with only `tools/wine/patches` applied. kernel32's `VirtualProtect`, `VirtualQuery`, `FlushInstructionCache` and exception-filter thunks MUST NOT share a page with its other import thunks.
**Verification**: Test

### WINE-SRC-5 Hot-patch prologue

**Rule**: Every `WINAPI` and `WINAPIV` function of the i386 PE modules MUST start with `mov edi,edi; push ebp; mov ebp,esp`. A hook that copies an export's first five bytes MUST therefore copy whole instructions.
**Verification**: Test

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

### WINE-SRV-4 No executable host mappings

**Rule**: The server MUST NOT map host memory executable. A mapping it requests with execute access MUST be granted readable without it.
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
A further view of an image at its server-assigned dynamic base MUST retain that base in its headers and fixups.
An image that other memory displaces from that base MUST be relocated to its view's address before its code is bound.
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
A mask without wildcards MUST name at most one entry, found as an indexed lookup (WINE-SYS-12) on every volume. A listing MUST check the type of matching entries only, without a host lookup unless the entry is a link.
Directory query `BOOLEAN` arguments MUST use only the low byte of their stack slot.
**Verification**: Test

Win32 file searches (`FindFirstFileExW`, `FindNextFileW`) query the directory
record Windows queries, `FileBothDirectoryInformation`, through the current
guest import, and convert it to the record Wine reads. Inline hooks written
against Windows answer only that record; Wine's extended record gets an empty
success from them.

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
A name whose directory exists as written MUST resolve by looking up its last component alone.
**Verification**: Test
**Basis**: Wine 11.0 `find_file_in_dir` and `get_dir_case_sensitivity`.

### WINE-SYS-13 Registry value cache

**Rule**: Cached registry reads MUST preserve information classes, buffer negotiation and value contents. Registry mutations and handle retirement MUST invalidate caches across the process family. Caches MUST retain at most 256 complete values per process, each at most 4096 bytes.
**Verification**: Test

### WINE-SYS-14 Name probe cache

**Rule**: Attribute queries by name MUST repeat their last result until a revision ends, which opening and closing alone MUST NOT do. Server calls that create, truncate, remove, rename, link or chmod a name MUST end it, as MUST file writes, flushes, mappings and information changes. A probe overtaken by a revision MUST NOT be kept.
**Verification**: Test

Caches hold at most 16384 names per process. Host changes made outside the
process family may stay unseen until the revision ends.

### WINE-SYS-15 Directory listings for absent names

**Rule**: Path lookups MUST remember known directories, and list a directory once after 32 absent-name lookups. An ASCII name without `~` that matches no listed name MUST then resolve as missing without a host lookup. Server name changes, NT mutations of a directory and links the runtime changes in the prefix MUST end what is remembered.
**Verification**: Test

Listed names include every name the host may fold to an ASCII one. Directories
over 65536 names, or unreadable ones, keep host lookups. File writes and
metadata changes leave listings intact. Lookups remember a drive by its link, so
relinking the game drive for the next session ends them.

### WINE-SYS-16 Heap blocks shrink in place

**Rule**: A heap reallocation that does not grow a block MUST return the same block, as Windows 7 did. This includes blocks the low-fragmentation heap serves.
**Verification**: Test

> Note: Windows 10's low-fragmentation heap moves a block shrunk into another size class. Engines of the Windows 7 era keep using the pointer they shrank, and crash there.

### NT-SOCK-1 Wine socket I/O

**Rule**: The NT platform MUST transfer Winsock stream and datagram payloads between guest buffers and server-owned sockets. It MUST preserve receive addresses, byte counts, readiness and asynchronous completion.
**Verification**: Test
**Basis**: Wine 11 `dlls/ntdll/unix/socket.c`.

The NT platform serves the Wine AFD send, receive and available-byte queries. The
server retains ownership of the sockets, readiness queues and completion state.

## NT files

NT services follow Wine 11.0's argument checks, server requests and status codes.

### NT-FILE-1 File descriptor operations

**Rule**: File flushing, deletion, mailslot creation and queries, and scatter/gather I/O MUST follow Wine 11's checks, statuses, server requests, and side effects.
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

### NT-FILE-5 Replaced files

**Rule**: A name that resolves to a replaced file's host identity MUST open its replacement, whatever path reaches it. Names reported to the guest MUST remain the original's.
**Verification**: Test

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

**Rule**: Process enumeration, suspension, thread alerts, continuation, and APC calls MUST follow Wine's behavior within `WINE-SYS-4`. Suspension MUST update machine contexts. An ended thread's stack MUST leave the process's allocations, so that later allocations of its range succeed.
**Verification**: Test

When importing extended x86 context registers, reserved x87 control-word bits
are ignored. The supported fields restore without changing the current reserved bits.

### NT-PROC-4 Thread creation at capacity

**Rule**: Live thread slots MUST fit inside the reserved TEB area. Exhausting those slots MUST return `STATUS_NO_MEMORY` without creating a server thread. Failed thread creation MUST release any thread resources it acquired.
**Verification**: Test

> Note: The macOS, iOS and Android launchers raise their soft descriptor limit to cover the server's thread pipes. The target is 1024, capped by the inherited hard limit.

### NT-PROC-3 Jobs and completions

**Rule**: Job membership, information, termination, and completion calls MUST use Wine's checks and server requests. Terminating the current process's job MUST end the machine process.
**Verification**: Test

## NT system, memory and debugging

### NT-SYS-1 Extended section views

**Rule**: Extended section calls MUST follow Wine 11.0 parameter checks, shared mappings and placeholder replacement. Preserving an unmapped placeholder MUST retain its reservation.
**Verification**: Test

### NT-SYS-2 Memory services

**Rule**: Memory flush, lock, prefetch and WoW64 services MUST follow Wine 11.0 statuses, lengths and server requests. Process write barriers MUST order prior guest writes.
An unconstrained private allocation MUST be able to use free user addresses outside the dynamic arena.
An allocation without a base MUST be placed first-fit bottom-up from the lowest user granule (0x10000), as on Windows; the main image's preferred range MUST be held from process creation until the image is mapped there, so early allocations fill around it. Thread stacks are placed bottom-up as well; the host's own reservations fill the dynamic range top-down, as Windows places system memory, so they never take the low addresses the guest expects.
Control-flow-guard protection bits MUST follow Windows. Allocation accepts `PAGE_TARGETS_INVALID` with an executable protection; with any other protection the status is `STATUS_INVALID_PARAMETER`. A protection change accepts `PAGE_TARGETS_NO_UPDATE` and `0x20000000` with an executable protection; with any other protection the status is `STATUS_INVALID_PARAMETER_4`. Accepted bits MUST NOT be stored in the page protection.
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

### NT-SYS-6 Exception stack exhaustion

**Rule**: Exception delivery MUST stop with a terminal guest stack overflow when its frame would cross the guard at the bottom of the thread stack. The failure MUST identify the exception that required the frame.
**Verification**: Review — the frame boundary is checked before any guest stack write.
**Basis**: Wine 11.0 `virtual_setup_exception` aborts when the exception frame reaches the last stack page.

### NT-SYS-7 CPU exception records

**Rule**: A CPU exception MUST reach the guest with the code, address, saved EIP and parameters that Windows x86 delivers.
**Verification**: Test — the probe's output is compared with its output on Windows 11 x86, on both engines.

BOUND out of range raises `STATUS_ARRAY_BOUNDS_EXCEEDED` at the instruction. User mode may use the gates for vectors 3, 4, 0x29, 0x2c, 0x2d and 0x2e. `INT n` through any other gate raises an access violation at the instruction, with parameters 0 and 0xffffffff. `INT 1` is therefore not a single step. `INT 4` raises `STATUS_INTEGER_OVERFLOW` one byte before the next instruction, where EIP stays. `INT 0x2c` raises `STATUS_ASSERTION_FAILURE` with the address and EIP at the instruction.

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

The host owns text services under WINE-UNIX-6.

### WINE-UNIX-5 Fixed-point formatting

**Rule**: Unix library printf-family functions MUST consume `%f` and `%F` arguments as i386 doubles, preserving flags, width, precision and following arguments. Bounded output MUST retain the full formatted length and terminate any nonempty buffer.
**Verification**: Test

### WINE-UNIX-6 Host font frontend

**Rule**: Host font services MUST own font data, text layout, text composition, and DirectWrite glyphs. Wine's font module and FreeType paths MUST remain dormant. Every face MUST appear as TrueType to the guest.
**Verification**: Test

Native font initialization fills win32u's ANSI, OEM and UTF-8 code page tables from the PEB, as Wine's does. It also updates `FontAssoc\Associated Charset` for the ANSI code page so DC font code pages match Windows.

### WINE-UNIX-7 DirectWrite glyph bitmaps

**Rule**: DirectWrite MUST keep glyph bitmaps of different rendering modes separate. DirectWrite MUST round glyph bitmap origins vertically to the nearest pixel, with half pixels rounded toward positive infinity.
**Verification**: Test

The pinned Wine source carries both through `tools/wine/patches`.

### WINE-UNIX-8 Text records

**Rule**: While text records are on, the host font frontend and the DIB copy bodies MUST keep exactly the records listed below, in one sequence. While records are off, they MUST NOT keep any. Records beyond capacity MUST be counted as dropped.
**Verification**: Test

- Each drawn line, before composition: its thread, DC, font, clip, target DIB, and each glyph's code and device origin. The target is also found beneath a forwarding driver, such as a window's. Glyph-index text keeps its indices.
- Each GetGlyphOutline or DirectWrite glyph bitmap that writes glyph bytes: its character or glyph index, font and written guest range.
- Each completed copy_rect, blend_rects or convert_to whose source holds text: both DIBs, the destination rectangle and the source map. Its destination then holds text too.

A DIB is identified by its bits, stride, depth and rectangle origin, so every record locates its pixels in guest memory; a source holds text when its bytes overlap bytes that do. Each record's font is a session key, and `glyph_characters` maps a glyph index in that font back to its characters. Measurement keeps no record, and neither does a copy between DIBs that never held text. Text extraction reads these where characters lose their identity; the session takes them through `take_text_records` and maps window surfaces through `window_planes`.

## Threads

### WINE-THR-1 One scheduling authority

**Rule**: Every guest thread MUST be a machine context, and every guest wait MUST block in the machine. The host thread MUST NOT block on a server pipe.
**Verification**: Test

### WINE-THR-2 Wakeups and APCs

**Rule**: A server wakeup MUST reach a waiting thread's context after every server run. A user APC queued to an alertable waiter MUST run on that thread before its wait returns.
**Verification**: Test

### WINE-THR-3 Suspension

**Rule**: Suspending a thread MUST stop its context before the caller next runs. Resuming it to a zero count MUST let it run. A suspend wait MUST NOT consume or lose a concurrent server select wake-up.
**Verification**: Test

### WINE-THR-4 Termination

**Rule**: Terminating another thread, or a process's other threads before it exits, MUST end their contexts where they stand, with no detach work. Waiters MUST see each thread signaled with the given exit code. Termination MUST remove pending suspension and saved signal state before host polling.
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

**Rule**: Installed host DLL exports MUST point inside an i386 PE32 image that Wine's loader maps. Entries MUST use their registered calling conventions and machine bindings without storing host addresses, at any load base.
**Verification**: Test

### WINE-DLL-4 Replaced module resources

**Rule**: A host DLL that takes the place of a Wine module's file MUST carry that module's resources, such as type libraries and version information, unchanged.
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

**Rule**: The driver MUST give each window except the desktop a window surface over pixels it owns, wrapped by `NtGdiDdDDICreateDCFromMemory`. A flush of the base window MUST present its content in those pixels without copying them; other windows follow WINE-GFX-10.
**Verification**: Test

### WINE-GFX-5 In-process desktop

**Rule**: When win32u starts explorer for its desktop, the process MUST NOT start. The calling thread MUST instead create the desktop and message windows as explorer does. It MUST then show the desktop over the virtual screen without activating it, so the first window the process activates receives `WM_ACTIVATEAPP`.
**Verification**: Test

### WINE-GFX-6 Direct3D 9 on Wine

**Rule**: Our Direct3D 9 MUST load as a PE image in place of Wine's. Its surface DCs MUST be memory DCs that call GDI through its own IAT. A presented back buffer MUST reach the presenter.
Child processes MUST defer native GPU presentation to the session's consumer before drawing.
**Verification**: Test

A swap chain supplying the presented base retains its presentation pacing even
when its guest device window is hidden.

### WINE-GFX-7 Frames published on waits

**Rule**: When a thread that drew a software frame waits, the frame MUST be published. A frame deferred to a UI's mailbox MUST NOT wait for a readback.
**Verification**: Test

### WINE-GFX-8 Disabled N-patches

**Rule**: Direct3D 9 MUST accept `SetNPatchMode(0)` without changing rendering state. Enabling N-patches MUST remain unavailable.
**Verification**: Test

### WINE-GFX-9 Display geometry

**Rule**: The driver MUST report one 2560x1440 monitor, so win32u never clamps a framed 1920x1080 game window. The mode list MUST include it. Its work area MUST leave out a 40-pixel taskbar at the bottom.
**Verification**: Test

### WINE-GFX-10 Window planes

**Rule**: The largest shown top-level window MUST be the base, whose frames come from its front while one presents it, else from its surface. Other shown windows in front of it MUST show over it in z-order, each its own layer at a whole scale near native windows'. A plane that appears, moves, changes or goes away MUST show the base beneath again without a guest draw.
**Verification**: Test

Owners and show order do not choose the base. Toolkits own every window by one hidden application window, and a startup window may show first. The current base wins a tie. A window becomes the base once it has been drawn; hiding or minimizing the base hands it to the next largest window at once. A modal window disables the windows it stands over, as `DialogBox` and toolkit modal forms do. While a shown window is disabled, only a disabled window can be the base. A settings form larger than its main window thus stays a plane. A front is a Direct3D 9 device or a DirectDraw object. It owns the base from the moment it submits a frame for that window; geometry queries leave ownership unchanged. Direct3D 9 retains its frame across window surface flushes. DirectDraw shares window content with GDI as defined by WINE-GFX-11. A successful native present supersedes older software work still waiting to publish. A repeated present publishes when it carries a change to the planes. Natively projected and minimized windows are neither base nor planes. A reparented layered child window stays a plane directly above its root ancestor. Its position is parent-relative, and its premultiplied alpha blends transparently. A plane is a window's whole surface at its screen position relative to the base window's content. Frames carry the planes beside the base, never inside it. The host centers each on the game point it covers, at the whole scale nearest its own, with nearest sampling. Resizing the host moves a plane without scaling it. Captures draw the planes at their guest positions. A frame carrying only changed planes presents again while the guest presents nothing. A plane outside the base window's content is moved into it with `SetWindowPos`, as a window manager keeps windows on screen.

### WINE-GFX-11 DirectDraw and GDI content handoff

**Rule**: A fresh base-window GDI flush overlapping window content MUST replace a shared DirectDraw frame. Geometry-only recomposition, empty dirt, noncontent dirt and other windows MUST NOT transfer that ownership. A subsequent DirectDraw frame MUST reclaim shared content without changing Direct3D 9 ownership rules.
**Verification**: Test
**Basis**: DirectDraw and GDI interoperability on the display surface.

Exclusive-mode DirectDraw frames retain non-shared base ownership and display mode geometry across window surface flushes until the device leaves exclusive mode.

## Input

### WINE-IN-1 Presented window coordinates

**Rule**: A host point on a plane's layer MUST reach that plane's window at the pixel shown, even when the base window is disabled. A press MUST keep its window until release. Other host points MUST reach the enabled base window, and scripted points the window under them, through the last presented content's origin (WINE-GFX-10).
**Verification**: Test

Every presenter's frames carry only that content, while the host shows caption, frame and menu bar natively.

### WINE-IN-2 Hardware input

**Rule**: The user driver's `ProcessEvents` MUST deliver queued mouse and keyboard events through win32u as hardware input on the guest thread. Keyboard input MUST carry its virtual key, scan code, extended flag and release flag.
**Verification**: Test

### WINE-IN-3 Input wakes message waits

**Rule**: Each input thread MUST register its event pipe as Wine's queue fd. Queuing host input MUST run the server and deliver wake-ups even when every guest context waits. Input waits MUST NOT use periodic polling.
**Verification**: Test

### WINE-IN-4 DirectInput action map device filtering

**Rule**: `IDirectInputDevice8::SetActionMap` MUST assign each device object's application data only from actions mapped to that device without `DIA_APPNOMAP`. Actions mapped to another device or left unmapped MUST NOT overwrite matching object IDs on the target device.
**Verification**: Test

The pinned Wine source carries this through `tools/wine/patches`.

## Native UI and control

### WINE-UI-1 Native popup presentation

**Rule**: Shown `#32768` windows MUST project their `MN_GETHMENU` items natively over their owner's plane layer (WINE-GFX-10) or the presented content, without presenting frames. Text dropdown lists MUST project natively without replacing the owner's frame. Completion tokens MUST become inactive when tracking ends.
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

### WINE-UI-8 Effective guest speed

**Rule**: Diagnostic retirement MUST distinguish translated code, operation helpers, interpret calls, and interpreter execution, with interpreted REP elements counted separately. The debugger MUST join matching counter intervals and execution-thread samples into per-form shares, retirement, nanoseconds per instruction, effective MIPS, and hottest guest units. Busy-gap analysis MUST include this report, separating checked accesses, host calls, memory slow paths, and waiting.
**Verification**: Test

### WINE-UI-7 Native dialog presentation

**Rule**: A shown top-level window MUST be read before its first flush. A dialog the host projects, such as a message box, MUST NOT present frames. Its icon MUST NOT show as an unavailable control.
**Verification**: Test

### WINE-UI-10 Native dialog controls

**Rule**: A top-level dialog MUST show natively only while every shown control in it has a native kind from the list below. Otherwise the whole dialog MUST stay its own painting as a plane (WINE-GFX-10). A control MUST show only while its parents show.
**Verification**: Test

- Buttons, labels, frames, edits, combo boxes, list boxes, trackbars, status bars and progress bars.
- Read-only text of many lines, read whole up to 65536 UTF-16 units.
- List views that keep their strings.
- Picture statics, shown by their bitmap's pixels in the snapshot that first shows them.
- Child dialogs, which show their controls and nothing themselves.

> Note: Setup wizards are property sheets. Their pages are child dialogs, a license is a read-only rich edit, and banners are picture statics. A tree, a tab strip or an owner-drawn control has no native kind, so its dialog keeps Wine's painting.

### WINE-UI-11 Panic reports

**Rule**: A panic on any thread of a session MUST leave a report in the session's debug folder, whether or not the debugger runs. The report MUST name the thread, the panic and the guest unit being translated, if any. Reporting MUST cost nothing until a panic.
**Verification**: Test

> Note: A worker that recovers from a panic, such as a failed compile, leaves the game running. Without the report, only stderr shows it.

### WINE-UI-9 Detached menu bars

**Rule**: A menu bar moved from the presented window MUST follow its root or submenu handles to its new window. The new window MUST remain the command target when hidden. A detached bar with at most one row of content MUST NOT present a window plane.
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

**Rule**: MIDI output MUST support capability queries, open/close, prepared long messages, short messages, reset and stereo volume. Successful open, close and long-message submission MUST deliver the corresponding WinMM callbacks. Failed requests MUST NOT create an output handle or deliver a completion callback, and unpreparing an unprepared header MUST succeed.
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

## Sessions

### WINE-SES-10 Sequential sessions

**Rule**: Sequential sessions on one host MUST share their prefix server while owning separate guest machines and mount sources. Ending a session MUST release every guest process and save registry changes before another session starts. Failed startup and forced stops MUST leave the host reusable.
**Verification**: Test

### WINE-SES-1 Shared prefix

**Rule**: Every game MUST run on the player's one prefix, kept between runs, with drive C: inside it or where the embedder places it. A host MUST provision system32, the template's module links and its own host modules on the drive C: a session uses. The program directory MUST keep its path on drive C: and otherwise use D: or the first unmounted letter after it, with writes in place.
Process startup MUST register the prefix's drive letters in the server namespace.
**Verification**: Test

Wine modules whose graphics backend Hitode does not serve (such as `dxgi.dll`) stay omitted from system32; refreshing an older prefix removes stale symlinks while leaving regular files intact.

### WINE-SES-2 Host wakes

**Rule**: While every guest context waits, a session MUST serve finished host work before it blocks. A host wake MUST end the block. The run loop MUST take the wake's interrupt before it drains host work.
UI projection refreshes MUST wake execution only when they queue new guest work.
A front end input wake MUST return control to the front end at the next session loop boundary.
**Verification**: Test

### WINE-SES-3 Provisioned prefix

**Rule**: A host MUST provision a new prefix from the installation's template, `share/hitode/prefix`, before starting its server. A host MUST run `wineboot.exe --restart` successfully once on its server before discovering game fonts and starting games. A process MUST start with its own variables, then those Wine's `add_registry_environment` adds.
**Verification**: Test

`tools/wine/prefix-template.sh` makes the template with standard Wine's `wineboot --init`. The restart creates the volatile keys and known DLLs for the server's lifetime. RunOnce and pending rename operations run at that boot boundary. The template is a distribution artifact, made where standard Wine runs, and not part of the repository.

The transient boot machine uses Tier-1 translation. Its policy is independent
of the application's resident optimization and computation policy.

### WINE-SES-4 Fixed identity

**Rule**: What a guest learns about its user and machine MUST NOT come from the host: the user MUST be `Player` and the computer `HITODE`. The firmware tables MUST name the virtual machine and the emulated processor, with no serial numbers or UUID. Name resolution through `getaddrinfo` MUST fail.
Processor counts and affinity MUST describe eight guest cores in one group, independently of the execution owner's host CPU.
**Verification**: Test

### WINE-SES-5 Regional text

**Rule**: Session locales MUST follow the ANSI code page: ja-JP for 932, zh-CN for 936, zh-TW for 950. Text MUST use portable matching and regional replacements without overriding installed families. Font discovery MUST be shared across sessions and scan each directory once per process.
**Verification**: Test

### WINE-SES-6 Installed system subdirectories

**Rule**: The prefix template MUST retain Wine-installed subdirectories of `system32`, including their data and module links.
**Verification**: Test

### WINE-SES-7 Execution settings

**Rule**: Both shells MUST apply the same engine policy and diagnostic settings. Wine MUST honor frame limits, shader capabilities, shader cache location, and physical audio enablement.
**Verification**: Test

### WINE-SES-8 Host font choices

**Rule**: Explicit host font choices MUST override matching font-creation requests without changing caller buffers, including when the requested family is installed. Font files, substitutions, and smoothing MUST apply before rendering and follow child processes. Session font resources, substitutions, and smoothing MUST NOT affect later sessions.
**Verification**: Test

### WINE-SES-9 Mounted file ownership

**Rule**: Wine sessions MUST apply disc image mounts and installation-directory mappings before process startup. Files, directories, sections, children, and volume queries MUST retain the same mounted backing. Reused prefixes MUST NOT retain removed host mappings.
**Verification**: Test

### WINE-SES-11 Registry restoration

**Rule**: Registry restoration MUST persist `.reg` imports and `.bat` scripts on the shared prefix with the game's drive layout and code page. Imports and scripts MUST map `WOW6432Node` keys to 32-bit `SOFTWARE`. A script MUST run as a guest program (WINE-SES-12), as the list below requires.
**Verification**: Test

- `cmd.exe` runs it from its own path, reading a copy without its elevation and pauses.
- Its console MAY be piped: it then reads the host's lines and writes its output, both in the game's code page.
- What it wrote is settled once the session ends.

### WINE-SES-12 Guest programs

**Rule**: A session MUST be able to start a guest program in place of its game, as the list below requires. What the program installs on drive C:, and the registry and shell links it writes, MUST hold in the next session. Children it stages and waits for MUST run to their end.
**Verification**: Test

- It runs on the game's prefix, discs and program folder, or with no program folder, as a disc's setup does.
- It starts in its image's directory unless another is given.
- A host image in the program folder or on drive C: keeps that path; any other takes the next free drive letter.
- Asked for output, it receives standard output and error, with empty input.

> Note: Explorer starts a program in its own directory, where setups expect their files. A game installed on drive C: keeps the path its setup wrote into the registry and its shortcuts.

> Note: The launcher installs from a disc image this way. A library entry stands for the install, and its first run starts the disc's setup with `C:\Program Files` in the chosen folder. The program the player then picks there becomes the game and keeps that `C:\Program Files`.

### WINE-SES-13 Disc changes

**Rule**: A running session MUST be able to put another disc image in a mounted drive. Paths opened afterwards, and volume queries on the drive, MUST reach the new disc. Files opened before MUST keep reading the disc they were opened on.
**Verification**: Test

> Note: Setups on several discs ask for the next one in the same drive and check its label.

### WINE-SES-14 Installed programs

**Rule**: A host MUST list the programs whose uninstall entries on the prefix name a folder on drive C:, with that folder on the host. The folder MUST come from `InstallLocation`, else `DisplayIcon`, else `UninstallString`. Drive C: itself, its Windows folder and `C:\Program Files` MUST NOT be listed as a program's folder.
**Verification**: Test

> Note: A setup told to install outside the chosen folder leaves its game elsewhere on drive C:, and its uninstall entry says where.

### WINE-SES-15 Root folders in the chosen folder

**Rule**: With a chosen install folder, a new folder at the root of drive C: MUST lie in that folder. It MUST keep its `C:\` path in later sessions, for opens and for the names handles report. Wine's folders and names the prefix already holds at that root MUST stay in the prefix.
**Verification**: Test

> Note: Some setups install to a fixed folder at the root of the Windows drive instead of Program Files.

## Conformance

### WINE-TEST-1 Wine's own tests

**Rule**: Listed Wine conformance tests that pass on standard Wine MUST pass here with no failures. Each runs as its own program without arguments.
**Verification**: Test


## Native video

### WINE-MEDIA-8 Native DirectShow graphs

**Rule**: Wine's `quartz.dll` MUST activate DirectShow classes, including VMR-7, through `adv_dx::dshow`, with the DMO wrapper via `qasf.dll`. Native file graphs MUST decode through `adv_device` and mix audio through the session's native DirectSound service. Host video renderers MUST accept pushed uncompressed RGB frames over `IMemInputPin`.
**Verification**: Test

Windowless VMR-7 (`CLSID_VideoMixingRenderer`) converts pushed uncompressed RGB frames to presentation frames.

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

Each retained media module MUST match the active Wine distribution and MUST only be rewritten when its contents differ.

## Native WinMM

### WINE-AUD-8 Native wave output

**Rule**: Native PCM and float wave output MUST consume queued headers on device time through the session mixer. Pausing, restarting, resetting and breaking loops MUST preserve Wine 11's header flags, ordering and positions. The wave mapper's preferred device MUST be the one device counted.
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

**Rule**: MIDI streams MUST sequence short, long, tempo and callback events on host time through the Wine stack's synthesizer. Pause, restart, stop and properties MUST retain Wine 11's results, and position MUST report SMPTE time under an SMPTE division. Session idle deadlines MUST include native audio work.
**Verification**: Test
**Basis**: Wine 11 `dlls/winmm/winmm.c`; Wine winmm `midi` test, which passes on Windows.

### WINE-AUD-13 Native DirectMusic synthesizer

**Rule**: DirectMusic software synthesis MUST run through `adv-device::midi::Synth` and the session mixer on host time. Prefixes MUST install a default General MIDI collection when absent without replacing an existing file. Structured MIDI messages, SysEx messages and volume properties MUST control synthesized PCM output.
**Verification**: Test
**Basis**: Wine 11.0 `dmsynth.dll`, `dmusic.dll` and `dmloader.dll`.

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

Withdrawn: WINE-TEST-2
