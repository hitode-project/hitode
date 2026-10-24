# Native bodies

ID prefix: NAT

This spec covers host bodies for functions of the pinned Wine distribution. `adv-nt::identity` matches mapped images and functions against code identity tables, and `adv-nt::native` binds bodies to the functions they confirm. [README.md](README.md) explains the format.

## Identity

### NAT-ID-1 Code identity

**Rule**: A code identity table MUST describe a mapped image only when the hash of the image's headers equals the table's. A function MUST be bound to a native body only when its bytes, with base relocations masked, hash to the table's value.
**Verification**: Test

## Bodies

### NAT-BODY-1 Body semantics

**Rule**: A native body that accepts a call MUST leave the guest state Wine's function would leave. Registers and flags the calling convention does not keep are exempt. Otherwise it MUST decline before any side effect, and the call MUST run Wine's function from its entry with the caller's frame.
**Verification**: Test

### NAT-BODY-2 Performance counter body

**Rule**: The native body of `ntdll.dll!NtQueryPerformanceCounter` MUST use the system call's counter computation and machine clock. It MUST return success and write the counter and optional frequency as the system call does. A null counter or an unwritable output MUST make the body decline before any write.
**Verification**: Test

### NAT-BODY-3 DirectSound mixer

**Rule**: The DirectSound mixer body MUST preserve Wine's float samples, scratch data, buffer positions, frequency accumulators, states, shared lock words, and stopped result. It MUST decline before writes when locks would wait, notifications would fire, effects or 3D apply, conversions are unsupported, or required memory is inaccessible. Its field offsets MUST match the pinned module's DWARF.
**Verification**: Test
