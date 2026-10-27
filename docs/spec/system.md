# System libraries

ID prefix: SYS

This spec covers system identity outside the core subsystems: invariant filesystem names. `adv-os` implements it. [personality.md](personality.md) covers the shared Windows identity and [win32.md](win32.md) the core subsystems.

## Filesystem identity

### SYS-FILE-1 Invariant filesystem names

**Rule**: Filesystem name matching MUST use the shared invariant UTF-16 case mapping on every host. Non-ASCII names MUST use the same directory index and invalidation rules as ASCII names.
**Verification**: Test
