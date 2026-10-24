# NT system calls

ID prefix: NT

This spec covers the NT system calls `adv-nt` ports from Wine 11.0's `dlls/ntdll/unix` beyond the core rules in [wine.md](wine.md). Each call follows Wine's argument checks, server requests and status codes. [README.md](README.md) explains the format.

## Files

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

## Security

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

## Registry, processes and jobs

### NT-PROC-1 Registry hive operations

**Rule**: Registry rename, transacted opens, and hive operations MUST follow Wine's argument checks and server requests. Hive files MUST use the guest's file handles.
**Verification**: Test

### NT-PROC-2 Process and thread operations

**Rule**: Process enumeration, suspension, thread alerts, continuation, and APC calls MUST follow Wine's behavior within `WINE-SYS-4`. Suspension MUST update machine contexts.
**Verification**: Test

### NT-PROC-3 Jobs and completions

**Rule**: Job membership, information, termination, and completion calls MUST use Wine's checks and server requests. Terminating the current process's job MUST end the machine process.
**Verification**: Test

## System, memory and debugging

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
