# Wine native bodies

ID prefix: NAT
ID prefix: CODEC

This spec covers host code that runs in place of identified Wine and guest functions on the Wine stack: native bodies and static codec kernels. [wine.md](wine.md) covers the rest of the stack, and [README.md](README.md) explains the format.

## Native bodies

`adv-nt::identity` matches mapped distribution images and functions against code identity tables.
`adv-nt::native` binds bodies to the confirmed functions.

### NAT-ID-1 Code identity

**Rule**: A code identity table MUST describe a mapped image only when the hash of the image's headers equals the table's. A function MUST be bound to a native body only when its bytes, with base relocations masked, hash to the table's value. Native bodies MUST find Wine data by the symbol names the table lists, never by pinned addresses.
**Verification**: Test

A family of bodies whose data symbols the table lacks keeps Wine's code.

### NAT-ID-2 Unix code identity

**Rule**: Unix i386 ELF identity tables MUST include local function symbols and mask dwords patched by dynamic relocations. Unix modules MUST bind bodies only after relocation, with matching ELF headers, program headers, and function bytes. A declined PIC entry call MUST preserve its original return address.
**Verification**: Test

### NAT-BODY-1 Body semantics

**Rule**: A body bound to a Windows API MUST leave the state the function leaves on Windows, including results, output bytes, errors and the last-error value. Other bodies MUST leave the state their bound guest code leaves. Registers and flags the calling convention does not keep are exempt.
**Verification**: Test

Windows behavior is measured on Windows 11 with the ACP and locale under test.

Required host text services follow WINE-UNIX-6 in interpreted and translated execution.

### NAT-BODY-2 Performance counter body

**Rule**: Bodies of `kernelbase.dll!QueryPerformanceCounter`, `ntdll.dll!RtlQueryPerformanceCounter` and `ntdll.dll!NtQueryPerformanceCounter` MUST use the system call's counter computation and machine clock. They MUST write the counter and return what Windows returns. Inaccessible outputs MUST produce the status or exception Windows produces.
**Verification**: Test

### NAT-BODY-3 DirectSound mixer

**Rule**: The DirectSound mixer body MUST preserve Wine's float samples, scratch data, buffer positions, frequency accumulators, states, shared lock words and stopped result. It MUST cover every format, channel count, resampling ratio, 3D setting and floating-point control the function accepts. It MAY return to guest code under A64-CALL-6 only when a lock would wait or an effect object runs.
**Verification**: Test

Field offsets match the pinned module's DWARF. The mixer runs as a host-call body, so notifications use machine services.

### NAT-BODY-4 DIB primitives

**Rule**: DIB bodies MUST preserve Wine's pixels, padding, overlap traversal, bit fields, palette selection, blend arithmetic and interpolation rounding. They MUST cover every format, palette depth, brush and floating-point control their primitive accepts. Field offsets MUST match the pinned Unix module's DWARF.
**Verification**: Test

After copy_rect, blend_rects or convert_to completes, it also keeps a text record while records are on (WINE-UNIX-8); recording rereads inputs the body has read and never changes its result.

### NAT-BODY-6 CRT bodies

**Rule**: Locale-independent CRT string and memory bodies MUST return the results and write the bytes the Windows CRT does, for every length and overlap. Inaccessible memory MUST follow A64-CALL-4.
**Verification**: Test

### NAT-BODY-16 Complete bodies

**Rule**: A body MUST complete every call its function accepts, for all argument values, code pages, locales, formats, overlaps and floating-point controls. It MAY return to guest code only under A64-CALL-6. Faults, including division by zero, MUST raise the exception the function raises.
**Verification**: Test

### NAT-BODY-17 Body memory access

**Rule**: Bodies MUST reach guest memory through one access type, which on AArch64 reads the window and class table under A64-CALL-4. Other hosts MUST give that type the same results through `Memory`. Bodies MUST read and write guest bytes in place, copying only where their function's semantics need a snapshot.
**Verification**: Test

### NAT-BODY-18 Direct and service bodies

**Rule**: Hot functions that complete without waits, thread switches or guest calls MUST bind as direct bodies under A64-CALL-2. Service bodies MAY change host state, such as server objects, queues and file positions, but MUST NOT read guest registers or schedule threads. Each binding MUST declare its calling convention, argument bytes and result registers.
**Verification**: Test

| Group | Service functions |
|---|---|
| Time | `QueryPerformanceCounter`, `timeGetTime` |
| Input | `GetKeyState`, `GetAsyncKeyState`, `GetKeyboardState`, `GetCursorPos` |
| Queues | `PeekMessageA` and `PeekMessageW` finding no message, `GetQueueStatus`, `GetInputState` |
| Objects | Waits with a zero timeout, `SetEvent`, `ResetEvent`, `ReleaseSemaphore`, `ReleaseMutex` |
| Files | File position queries and changes, `GetFileSize`, `GetFileSizeEx` |

Windows empty A/W peeks preserve last error and MSG, dispatch pending sent messages despite filters, and skip GETMESSAGE/FOREGROUNDIDLE hooks. Measured WoW64 empty peeks hand off about once per 100 calls, including PM_NOYIELD; the body requests that completed-call handoff from its owner. Driver jobs, dirty surfaces, internal messages and matching messages or timers requiring server processing retain the full path. The ANSI wrapper keeps its pending character conversion state and reaches the shared empty-peek body through NtUserPeekMessage. The full USER path advances pending server activity before Wine reads its shared queue bits. File cursors belong to server fd ownership, including inherited and duplicate handles whose clients have not admitted a descriptor.

### NAT-BODY-8 D3DX pixel conversion

**Rule**: D3DX pixel bodies MUST preserve channel values, filter coordinates, color keys and padding with layouts matching the pinned module's DWARF. They MUST cover every format, filter and overlap the function accepts. Changed helper code or copy imports MUST leave the function unbound.
**Verification**: Test

D3DX checks an admitted CRT entry against the exact relocated bytes saved at binding. It watches those bytes and the import slots together.

### NAT-BODY-9 PNG row bodies

**Rule**: PNG row bodies MUST preserve Wine's Sub, Up, Average and Paeth bytes and row metadata for every pixel depth and row overlap. Every host MUST provide each filter, scalar where no device kernel exists. Row metadata offsets MUST match the pinned module's DWARF.
**Verification**: Test

### NAT-BODY-10 Embedded zlib checksums

**Rule**: Adler-32 bodies MUST preserve the pinned zlib's seeded checksum, short-input arithmetic and null-buffer initialization.
**Verification**: Test

### NAT-BODY-11 DEFLATE fast batches

**Rule**: Native DEFLATE batches MUST preserve Wine's output, consumed input, bit reservoir, stream pointers and block-end state. They MUST cover every stream the function accepts, including output overlapping its inputs. Stream and window layouts MUST match the pinned module's DWARF.
**Verification**: Test

### NAT-BODY-12 Queue key state

**Rule**: The `win32u.dll!NtUserGetKeyState` body MUST return Wine's synchronized queue key state, masked and sign-extended, to the guest. A thread without a queue MAY return to guest code under A64-CALL-6, without creating a queue or leaving server errors or references.
**Verification**: Test

### NAT-BODY-13 Statically linked helpers

**Rule**: Runtime helpers that game images link statically MUST bind only where an executable section holds their exact bytes, each copy under its own thunk. Borland's division bodies MUST return EDX:EAX, pop the divisor, zero ECX, and raise `EXCEPTION_INT_DIVIDE_BY_ZERO` under A64-CALL-4 for a zero divisor. Its copies MUST set EAX, ECX, EDX and overlapping bytes as its code does.
**Verification**: Test

### NAT-BODY-14 Engine pixel routines

**Rule**: Engine pixel bodies MUST write the pixels the routine writes, bit for bit, even where its pair paths round unlike its single-pixel path. They MUST keep every register the routine keeps. Overlapping sources and destinations MUST be read and written in the routine's order.
**Verification**: Test

### NAT-BODY-15 Native host work

**Rule**: A body's host work MUST scale with bytes passed and returned, plus a constant. Tables derived from immutable data MUST be built at most once per process and reached without hashing per call. Bodies MUST NOT read clocks, allocate heap memory or take host locks per call, except where their function does.
**Verification**: Test

### NAT-BODY-19 Host features

**Rule**: Bodies MAY use host instruction set features detected at startup, such as NEON table lookups, CRC32, PMULL and dot products. The variant MUST be chosen when the function binds, without per-call feature checks. Every variant MUST produce identical results.
**Verification**: Test

### NAT-BODY-20 Wine string comparison data

**Rule**: Comparison bodies MUST read Wine's loaded sort, locale and code page data in place and return Wine's results and last error. They MUST NOT load, copy or correct NLS data.
**Verification**: Test

## Static codec kernels

Static Vorbis, Theora, and Ogg bindings run on the Wine stack.
Device decoding follows [device.md](device.md).

### CODEC-MAP-1 Image binding

**Rule**: Image scans MUST run exactly once per non-distribution mapping in native or verify mode and MUST be skipped for all other mappings.
Admission MUST use reviewed codec signatures over executable ranges and resolve each layout reference to a displacement in the matched code.
Unmapping MUST retire bindings and their decoder state.
**Verification**: Test

### CODEC-CALL-1 Vorbis transforms

**Rule**: Native Vorbis transforms MUST retain the built-in stack's admission, ABI, table, buffer, and floating-environment checks. Declined calls MUST preserve guest memory and resume the original function through its trampoline. Verify mode MUST publish guest results and preserve guest floating state.
**Verification**: Test

### CODEC-CALL-2 Theora packets

**Rule**: Theora bindings MUST retain complete library-family admission and preserve pixels, state, guest allocation lifetime, return values, and callbacks. Calls without admitted decoder state MUST resume guest code before changing guest memory. Verify mode MUST compare native results while publishing guest results.
**Verification**: Test

### CODEC-CALL-3 Packet run-ahead

**Rule**: A predictor MUST read only complete packets from a stream paired by an observed packet output and decoder input.
File prediction MUST follow accepted pages linked to contiguous ranges of observed file reads.
Workers MUST use positioned reads and validate Ogg page CRCs before staging file packets of the paired stream.
Each decoder MUST stage at most three packets. Each file predictor MUST bound its page buffer and release it with the decoder.
Workers MUST stage results in host storage without changing committed decoder state.
The owner MUST match packet bytes, metadata, and decoding controls before committing a staged result.
Seeks, resets, callable controls, and clear MUST invalidate staged results.
Verify mode MUST compare staged output and state with synchronous decoding.
**Verification**: Test

### CODEC-MODULE-1 Shipped Vorbisfile modules

**Rule**: Wine MUST bind admitted shipped `vorbisfile.dll` exports to host thunks while preserving names, ordinals, original code, and loader lifetime. Admission MUST retain the provider's ordinal-layout, static-import, delay-import, and dynamic-symbol checks through Wine's file namespace. Guest mode and rejected modules MUST retain guest execution.
**Verification**: Test
**Basis**: DEV-AUDIO-3

Withdrawn: NAT-BODY-5, NAT-BODY-7
