# Server transport and synchronization

ID prefix: SRV

This spec covers how guest processes reach the in-process Wine server: request, reply and wait channels, the server's event loop, and synchronization objects. WINE-SRV rules in [wine.md](wine.md) cover the server itself. [README.md](README.md) explains the format.

## Channels

### SRV-CHAN-1 Memory channels

**Rule**: Request, reply and wait pipe bytes MUST travel through shared memory without channel read or write system calls.
Registered descriptors MUST retain pipe read, close and readiness behavior across descriptor transfers and reuse.
Wake delivery MUST visit only threads whose wait queues received bytes.
**Verification**: Test

## Event loop

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

## Synchronization

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
