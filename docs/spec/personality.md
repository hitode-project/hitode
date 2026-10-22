# Personality

ID prefix: PERS

This spec covers the Windows identity the guest sees. It includes implementation levels, exports and diagnostics, observable NT and RTL boundaries, the version profile, native services and ABI ownership. `adv-abi`, `adv-os` and `adv-win32` implement it. [win32.md](win32.md) covers individual Win32 subsystems.

## Scope and acceptance

### PERS-SCOPE-1 Implementation levels

**Rule**: API front ends MUST provide observable behavior, shared state, or an explicit API failure. They MUST NOT fabricate successful objects, devices, decoded output, or authentication.
**Verification**: Review — the three implementation levels require reviewing each API against its observable effects.

### PERS-SCOPE-3 Shared ownership

**Rule**: Win32, NT, RTL, CRT, and COM front ends MUST reuse existing state owners for memory, handles, files, modules, threads, and synchronization. DirectX reference helpers MAY share guest counters with host COM helpers while retaining one Objects owner until public and internal references vanish. Controlling aliases, guest patches and rejected aggregation MUST retain their behavior, with original handlers owning zero and overflow edges.
**Verification**: Review — subsystem tests cover individual bridges, not the ownership path of every API front end.

### PERS-SCOPE-4 Exhaustion

**Rule**: Resource exhaustion MUST return failure without publishing partial resources or modifying outputs that the API preserves.
**Verification**: Test

### PERS-SCOPE-5 Capability diagnostics

**Rule**: Deliberate rejection of unsupported behavior MUST report the API and reason through `adv_abi::warn_unavailable`. Ordinary missing files and timeouts MUST NOT become capability gaps.
**Verification**: Review — no test distinguishes ordinary failures from capability rejections across every front end.

### PERS-SCOPE-6 Compatibility claims

**Rule**: Zero stub calls or successful return codes MUST NOT establish structural compatibility. Structural failures in live or retired children MUST disqualify the process family.
**Verification**: Test

### PERS-SCOPE-8 Excluded targets

**Rule**: The personality MUST NOT advertise x64 guests, D3D11 engines, OpenGL engines, or standalone installer compatibility as supported targets.
**Verification**: Review — no unified test validates the advertised compatibility scope.

### PERS-SCOPE-10 Input state ownership

**Rule**: Synchronous keyboard queries MUST use message-queue state, while asynchronous queries MUST use physical input state. Host windows MUST provide presentation and input without replacing guest USER semantics.
**Verification**: Test

### PERS-SCOPE-11 Encoding boundaries

**Rule**: API front ends MUST preserve explicit ACP and charset selection and MUST NOT silently truncate conversion sources.
**Verification**: Review — charset tests cover selected APIs, but no test checks source completeness across all conversion front ends.

## Input delivery

### PERS-INPUT-1 Wheel routing

**Rule**: Host wheel messages MUST target the valid keyboard focus independently of mouse capture, retaining screen coordinates. DefWindowProcA/W MUST synchronously forward unhandled WM_MOUSEWHEEL to the parent and return its result, stopping at a window without a parent.
**Verification**: Test

### PERS-INPUT-2 Legacy device polling

**Rule**: Legacy keyboard/mouse devices MUST expose IDirectInputDevice2A/W with the SDK's 27-slot layout and shared base-interface identity, state, and references. Poll MUST return DIERR_NOTACQUIRED before acquisition, DIERR_INPUTLOST on foreground loss, and DI_NOEFFECT for acquired event-driven devices without consuming input.
**Verification**: Test

## Exports and diagnostics

### PERS-EXPORT-1 Fixed export structure

**Rule**: Synthetic system modules MUST expose real PE exports and patchable guest entries. Unknown names MUST remain absent.
**Verification**: Test

### PERS-EXPORT-2 Discovery classification

**Rule**: Dynamic discovery MUST expose only implemented or reviewed exports, except under `ADV_PERSONALITY_STRICT`. Forwarders MUST use their final target classification. Exports introduced after Windows 7 MUST remain undiscoverable in both API modes.
**Verification**: Test

### PERS-EXPORT-3 Review admission

**Rule**: A reviewed export MUST have a callable signature and registered failure. Implementing it MUST remove its review entry.
**Verification**: Test

### PERS-EXPORT-4 Reviewed failures

**Rule**: Reviewed exports MUST return their registered failure in both API modes and set LastError to 120. They MUST count reviewed calls without counting stub calls.
**Verification**: Test

### PERS-EXPORT-5 Diagnostic stubs

**Rule**: Strict mode MUST stop at an unimplemented diagnostic stub. Discovery mode MUST record the call and return its registered failure, else the failure below for its return type. Player runs MUST default to discovery, and returns with no derivable failure MUST stop.
**Verification**: Test

| Return type | Failure |
|---|---|
| void, BOOL, counts, handles, pointers, strings | 0 |
| HRESULT, NTSTATUS, WIN32_ERROR, GDI+ Status, errno_t | Their not-implemented code |
| SOCKET, COLORREF, WAIT_EVENT, untyped Winsock int | All bits set |
| Untyped winmm result, except device counts | MMSYSERR_NOTSUPPORTED |
| 64-bit, floating and aggregate | None |

### PERS-EXPORT-6 Data exports

**Rule**: Data exports MUST have real layout providers. Function thunks MUST NOT substitute for data, and unsupported data exports MUST NOT bind.
**Verification**: Test

### PERS-EXPORT-7 Guest entry precedence

**Rule**: Guest modifications to export entries MUST remain discoverable and executable regardless of the original implementation classification.
**Verification**: Test

### PERS-EXPORT-8 Bounded gap reporting

**Rule**: Personality diagnostics MUST distinguish rejected probes, reviewed calls, stub calls, and structural mismatches. Rejected names MUST be bounded to 256 entries with explicit dropped counts.
**Verification**: Review — the complete personality report and its 256-name boundary lack a dedicated end-to-end test.

### PERS-EXPORT-9 Static survey

**Rule**: A survey MUST list, before guest code runs, every static import, constant-named module load or lookup and constant COM class the runtime cannot satisfy. It MUST resolve game modules before host modules, as the loader does. It MUST count lookup call sites whose name or class is not a constant.
**Verification**: Test

## Observable API boundaries

### PERS-BRIDGE-1 Observed boundaries

**Rule**: NT and RTL bridges MUST resolve the live guest entry. Only unchanged entries MAY use the host fast path. Unsupported observable boundary preservation MUST emit a capability warning.
**Verification**: Test

### PERS-BRIDGE-2 File and heap identity

**Rule**: File, enumeration, and heap front ends MUST preserve shared identities and propagate live NT or RTL hook results.
**Verification**: Test

### PERS-BRIDGE-3 Module and thread boundaries

**Rule**: Loading, module queries, reference changes, and thread creation MUST honor live NT or LDR hooks and their changed parameters or failures.
**Verification**: Test

### PERS-BRIDGE-4 Synchronization boundaries

**Rule**: Event, mutant, semaphore, and wait front ends MUST share object state with NT services. Hook changes MUST affect actual acquisition, release, or failure.
**Verification**: Test

### PERS-BRIDGE-5 Exception boundaries

**Rule**: Software exceptions MUST pass through live `RtlRaiseException`, `NtRaiseException`, `KiUserExceptionDispatcher`, and `NtContinue` boundaries. Hook parameter changes MUST reach subsequent handlers.
**Verification**: Review — available Rust tests cover individual boundaries, not the complete modified-parameter chain described by the native fixture.

### PERS-BRIDGE-6 Unsupported native behavior

**Rule**: Unsupported synchronization security, file-time modification, asynchronous file paths, and NtContinue alert testing MUST fail explicitly rather than imply support through exported names.
**Verification**: Review — existing boundary tests do not enumerate all rejected security, asynchronous-file, and alert-testing combinations.

## Windows identity and native services

### PERS-PROFILE-1 Consistent Windows identity

**Rule**: The default profile MUST identify Windows 7 SP1, build 7601, with `PRODUCT_ULTIMATE`. PEB version fields, version APIs, suite information, and product information MUST use the selected profile.
**Verification**: Test

### PERS-PROFILE-2 Mutable PEB versions

**Rule**: Version APIs backed by PEB fields MUST reflect guest changes to those fields. Suite masks and product type MUST remain derived from the selected profile.
**Verification**: Review — profile tests change configuration but do not mutate the installed PEB and compare all affected APIs.

### PERS-PROFILE-3 Unmodeled identity channels

**Rule**: System version registry values, synthetic DLL version resources, and `KUSER_SHARED_DATA` MUST NOT be fabricated without a profile-backed implementation.
**Verification**: Test

### PERS-PROFILE-4 Native service ABI

**Rule**: For profile 6.1.7601, native x86 `INT 2E` MUST use EAX as service number and EDX as argument pointer. Arguments MUST be validated and copied before dispatch. Ordinary returns MUST replace only EAX.
**Verification**: Test

### PERS-PROFILE-5 Native service failures

**Rule**: Unimplemented catalog services MUST return `STATUS_NOT_IMPLEMENTED`. Unknown services, GUI selectors, and other profiles MUST return `STATUS_INVALID_SYSTEM_SERVICE`. Unreadable arguments MUST return `STATUS_ACCESS_VIOLATION`, while zero-argument services MUST ignore EDX.
**Verification**: Test

### PERS-PROFILE-6 Native service identity

**Rule**: Implemented Nt/Zw pairs MUST share entries and state. Relocated entries MUST remain callable, and native services MUST preserve waits, APCs, and explicit context restoration.
**Verification**: Test

### PERS-PROFILE-7 ANSI code page selection

**Rule**: Without an explicit code page, the ANSI code page MUST be 936 for a Simplified Chinese executable and 932 otherwise. Markers are a name token `chs`, `cn`, `gb` or `zhcn`, a kana-free name with a GBK-only character, or GB2312 data text outnumbering Shift_JIS threefold. An explicit code page MUST replace the detected one before guest code runs.
**Verification**: Test

### PERS-PROFILE-8 CRT subprocess environment

**Rule**: CRT `system(NULL)` queries MUST report no command processor without changing error cells. Valid `system`, `_spawn*`, `_exec*`, and `_popen` calls MUST fail with ENOENT in this environment. Ordinary subprocess failures MUST NOT emit capability warnings or publish process handles or streams.
**Verification**: Test

### PERS-PROFILE-9 CRT error cells

**Rule**: CRT error pointers and getters/setters MUST share the existing module/thread cells. UCRT API-set calls MUST use UCRT's cells. Other CRT modules and threads MUST retain independent cells.
**Verification**: Test

## ABI ownership

### PERS-ABI-1 Single signature source

**Rule**: API signatures MUST come from `data/abi` and explicit checked aliases. API front ends MUST NOT create conflicting declarations or calling conventions.
**Verification**: Test

### PERS-ABI-2 Layer ownership

**Rule**: PE parsing MUST belong to `adv-pe`, loading policy to `adv-os`, and platform API behavior to its personality front end. Device implementations MUST NOT depend on personality crates.
**Verification**: Check — Cargo dependencies and `tools/layering/check_layers.py` enforce the layer direction.

### PERS-ABI-3 Nt and Zw registration

**Rule**: Implemented Nt/Zw pairs MUST share binding identity before native numeric service installation. Missing Zw declarations MUST derive from their matching Nt declarations.
**Verification**: Test

Withdrawn: PERS-SCOPE-2, PERS-SCOPE-7, PERS-SCOPE-9
