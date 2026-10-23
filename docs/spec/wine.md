# Wine userland

ID prefix: WINE

This spec covers the Wine userland stack. `adv-wineserver` runs Wine's server inside the host process. `adv-nt` takes the place of Wine's `ntdll.so` under Wine's own Windows DLLs. [README.md](README.md) explains the format.

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
