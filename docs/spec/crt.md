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

### CRT-LIB-7 Heap ownership and resizing

**Rule**: CRT allocation fronts and size queries MUST share the existing heap owner and declared version exports. Reallocation failure MUST preserve the old block, and `_expand` MUST resize without moving. Zero-sized `realloc` MUST release a nonnull block.
**Verification**: Test

### CRT-LIB-8 Heap error contracts

**Rule**: Public allocation failure and product overflow MUST publish ENOMEM. NULL size-query and expansion inputs MUST use the runtime's parameter-validation profile. Zero-sized allocation requests MUST retain distinct live blocks that can be freed.
**Verification**: Test

### CRT-LIB-9 Aligned allocation views

**Rule**: Aligned CRT allocations MUST retain real backing blocks in the shared heap, with recoverable guest headers. The requested view plus offset MUST satisfy its alignment, and size queries MUST recover the requested extent. Resizing MUST preserve old contents on success and retain the original allocation on failure.
**Verification**: Test

### CRT-LIB-10 Aligned zeroing and validation

**Rule**: Aligned recalloc MUST zero newly added bytes and reject overflowing products with ENOMEM. Invalid inputs MUST follow the version's parameter-validation profile and native zero-alignment policy. Zero-sized resizing of a nonnull view MUST release its backing block before alignment validation.
**Verification**: Test

### CRT-LIB-11 Integer conversion boundaries

**Rule**: CRT integer conversions MUST use x86 result widths, saturate overflow with ERANGE, and preserve errno on successful conversion. End pointers MUST identify the first unconsumed unit, or the original pointer when no digit was converted. Narrow and wide fronts MUST share checked scanning while observing their runtime's locale owner.
**Verification**: Test

### CRT-LIB-12 Integer validation and access order

**Rule**: Integer conversions MUST initialize nonnull end outputs before validating source and radix. Invalid parameters MUST use the runtime's validation profile, with full-width zero results when execution continues. Prefix scanning MUST stop at the first unrecognized unit and avoid reading a suffix after overflow when no end position was requested.
**Verification**: Test

### CRT-LIB-13 Integer radix output

**Rule**: Integer radix output MUST use x86 input widths, lowercase digits, and a minus sign only for signed decimal inputs. Byte and UTF-16 fronts MUST share checked output, with capacities measured in code units. Secure fronts MUST clear the first unit before capacity/radix validation and report range failures through the runtime's parameter handler.
**Verification**: Test

### CRT-LIB-14 Set scans

**Rule**: Byte and UTF-16 set searches MUST share code-unit membership without locale conversion. Spans MUST count units, and pointer results MUST identify the first matching source unit. Scans MUST stop at matching or terminating units without reading inaccessible following units or imposing an artificial string-length bound.
**Verification**: Test

### CRT-LIB-15 Runtime diagnostic defaults

**Rule**: Default CRT abort MUST terminate through the Windows process owner with status three, without draining CRT exit callbacks. Runtime-error termination MUST use status 255 through that same owner. Debugger notifications MUST NOT fabricate a recipient or capability warning when no debugger is attached.
**Verification**: Test

### CRT-LIB-16 Signal ownership and delivery

**Rule**: CRT process signals MUST share module slots, while fault signals MUST retain independent per-thread exception entries. Delivery MUST reset the relevant user handler before invoking guest code, and ignored signals MUST return normally. SIGABRT aliases MUST share state, while explicit raise preserves the original callback number.
**Verification**: Test

### CRT-LIB-17 Signal context and filtering

**Rule**: Signal FP-code and exception-pointer cells MUST share the error-cell allocation and lifetime, expose callback context/reason and restore prior values on normal return. Filters MUST derive dispositions from native actions, retaining ten legacy fault entries and twelve UCRT entries including both SSE statuses. Nonlocal signal exits MUST abandon the CRT continuation without restoring prior cells or resuming termination.
**Verification**: Test

### CRT-LIB-18 Termination families

**Rule**: Cataloged CRT exit fronts MUST share module-owned callback arrays, with a separate UCRT quick-exit array and shared API-set aliases. Full cleanup MUST drain ordinary callbacks in LIFO order, accept registration during draining and close only that CRT's streams before returning or terminating. Quick exit MUST drain only quick callbacks, while `_exit`, `_Exit` and `_c_exit` MUST skip callbacks and flushing with `_c_exit` returning.
**Verification**: Test

### CRT-LIB-19 Initializer tables

**Rule**: Cataloged CRT initializer fronts MUST share checked ascending table traversal, skip NULL entries and preserve guest callback execution. `_initterm_e` MUST stop at the first nonzero result and return it, while `_initterm` MUST visit every entry regardless of callback return values.
**Verification**: Test

### CRT-LIB-20 In-place string edits

**Rule**: Cataloged ordinary byte and UTF-16 reversal and set fronts MUST share checked code-unit access, return the destination and preserve the original terminator. Bounded ordinary set operations MUST stop at their count without reading a following unit, including zero-count inputs.
**Verification**: Test

### CRT-LIB-21 Secure string set

**Rule**: Secure set fronts MUST validate the original terminator within capacity even after count exhaustion or NUL replacement. Unterminated inputs MUST preserve native incremental edits, clear the first unit and report EINVAL through the runtime's validation policy. Only bounded NULL/zero-capacity/zero-count requests MUST bypass input validation.
**Verification**: Test

Withdrawn: none
