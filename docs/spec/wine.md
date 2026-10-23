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

**Rule**: `NtCreateUserProcess` MUST fail with `STATUS_NOT_SUPPORTED`. win32u then creates the process's own desktop window, as Wine does when explorer cannot start.
**Verification**: Test

### WINE-SYS-6 Display driver

**Rule**: Process start MUST set `GraphicsDriver` of the null-GUID display device to the driver the embedder names, as explorer would. With `null`, windows MUST be created and draw, showing nowhere.
**Verification**: Test

### WINE-SYS-5 Host names and listings

**Rule**: `\??\unix` MUST name the host's root as a drive names its link. A directory listing MUST give `.`, `..`, then the entries matching the mask in name order. Each query MUST continue where the last stopped, until a restart.
**Verification**: Test

Entries carry no 8.3 names, as on a volume that does not make them.

### WINE-SYS-7 Instruction coherence

**Rule**: For the current process, `NtFlushInstructionCache` MUST return success after prior guest writes become visible to instruction execution.
**Verification**: Test

### WINE-SYS-8 Same mapped files

**Rule**: `NtAreMappedFilesTheSame` MUST succeed for two addresses in one view, and otherwise MUST ask the server whether two image views map one file. A private allocation MUST give `STATUS_CONFLICTING_ADDRESSES`.
**Verification**: Test

### WINE-SYS-9 Unix stack alignment

**Rule**: A call from the host into Unix-side ELF code MUST start its arguments on a 16-byte boundary, as Wine's system call dispatcher does. Calls into Windows code MUST keep their exact stack.
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

**Rule**: System calls from 0x1000 MUST run the function win32u registered with its arguments. `KeUserModeCallback` MUST run `KiUserCallbackDispatcher` on the same stack, and `NtCallbackReturn` MUST end that call with its result.
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

**Rule**: Suspending another thread MUST stop its context before the system call returns, and resuming it to a zero count MUST let it run. A thread suspending itself is not supported.
**Verification**: Test

### WINE-THR-4 Termination

**Rule**: Terminating another thread MUST end its context where it stands, with no detach work. Its waiters MUST see the thread signaled with the given exit code.
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

## Sessions

### WINE-SES-1 Kept prefix

**Rule**: A session MUST keep its prefix between runs. Each start MUST relink system32 to the Wine installation and drive D: to the program's directory.
**Verification**: Test

### WINE-SES-2 Host wakes

**Rule**: While every guest context waits, a session MUST serve finished host work before it blocks. A host wake MUST end the block. The run loop MUST take the wake's interrupt before it drains host work.
**Verification**: Test
