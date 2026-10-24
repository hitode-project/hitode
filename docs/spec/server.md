# Server transport and synchronization

ID prefix: SRV

This spec covers how guest processes reach the in-process Wine server: request, reply and wait channels, the server's event loop, and synchronization objects. WINE-SRV rules in [wine.md](wine.md) cover the server itself. [README.md](README.md) explains the format.

## Channels

### SRV-CHAN-1 Memory channels

**Rule**: Request, reply and wait pipe bytes MUST travel through shared memory without channel read or write system calls.
Registered descriptors MUST retain pipe read, close and readiness behavior across descriptor transfers and reuse.
Wake delivery MUST visit only threads whose wait queues received bytes.
**Verification**: Test
