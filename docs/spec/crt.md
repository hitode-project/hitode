# C runtime

ID prefix: CRT

This spec covers the Microsoft C runtimes the guest links: every msvcrt and msvcr version, ucrtbase and its API-set contracts. `adv-win32` implements them through one shared implementation per function. [personality.md](personality.md) covers export structure and CRT process identity cells.

## Shared runtime functions

### CRT-LIB-1 String version exports

**Rule**: Cataloged CRT byte and UTF-16 string exports MUST select one shared implementation through generated ABI declarations. Counts MUST measure bytes or UTF-16 code units respectively. Bounded operations MUST NOT read beyond their count or a string terminator.
**Verification**: Test

### CRT-LIB-2 String writes and searches

**Rule**: String writes MUST report inaccessible destination memory instead of successful partial copies. Bounded copy MUST pad after NUL, while bounded concatenation MUST append exactly one terminator. Searches MUST preserve terminator matches and return the first matching substring.
**Verification**: Test

### CRT-LIB-3 Multibyte code-page ownership

**Rule**: CRT multibyte code-page controls MUST share their runtime's locale owner and default to the guest ACP. Public API-set calls MUST use UCRT state without changing another CRT or Win32 ACP. Single-byte and UTF-8 modes MUST report false byte-lead classifications.
**Verification**: Test

### CRT-LIB-4 Multibyte traversal boundaries

**Rule**: Multibyte traversal MUST distinguish character and byte limits, preserve pair boundaries, and exclude terminal lead bytes from counts. Bounded copies MUST pad after NUL and suppress incomplete final pairs without reading beyond their count. Modern argument checks MUST use the runtime's invalid-parameter handler, and inaccessible destination writes MUST fail.
**Verification**: Test

### CRT-LIB-5 Process identity and delay

**Rule**: CRT process identity MUST match the existing Windows process owner. CRT sleep MUST use the existing Windows scheduler, converting a zero duration to one millisecond. Every version and public API-set alias MUST preserve these semantics.
**Verification**: Test

### CRT-LIB-6 Legacy numeric and command-line data

**Rule**: Legacy `_HUGE` data MUST be an eight-byte double initialized to positive infinity. Cataloged `_adjust_fdiv` cells MUST start at zero for the conforming virtual processor. CRT command-line pointer cells MUST reference the Win32 command-line owner while retaining independent, writable module storage.
**Verification**: Test

Withdrawn: none
