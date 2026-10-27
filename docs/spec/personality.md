# Personality

ID prefix: PERS

This spec covers the Windows identity the guest sees. It includes implementation levels, exports and diagnostics, the code page profile and ABI ownership. `adv-abi` and `adv-os` implement it. [win32.md](win32.md) covers individual Win32 subsystems.

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

### PERS-SCOPE-8 Excluded targets

**Rule**: The personality MUST NOT advertise x64 guests, D3D11 engines, OpenGL engines, or standalone installer compatibility as supported targets.
**Verification**: Review — no unified test validates the advertised compatibility scope.

### PERS-SCOPE-10 Input state ownership

**Rule**: Synchronous keyboard queries MUST use message-queue state, while asynchronous queries MUST use physical input state. Host windows MUST provide presentation and input without replacing guest USER semantics.
**Verification**: Test

### PERS-SCOPE-11 Encoding boundaries

**Rule**: API front ends MUST preserve explicit ACP and charset selection and MUST NOT silently truncate conversion sources.
**Verification**: Review — charset tests cover selected APIs, but no test checks source completeness across all conversion front ends.

## Exports and diagnostics

### PERS-EXPORT-1 Fixed export structure

**Rule**: Synthetic system modules MUST expose real PE exports and patchable guest entries. Unknown names MUST remain absent.
**Verification**: Test

### PERS-EXPORT-2 Discovery classification

**Rule**: Dynamic discovery MUST expose only implemented or reviewed exports, except under `ADV_PERSONALITY_STRICT`. Forwarders MUST use their final target classification. Exports introduced after Windows 7 MUST remain undiscoverable in both API modes.
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

### PERS-EXPORT-10 Mode contracts

**Rule**: An implementation MAY declare, per scalar parameter, the values it handles as Windows 7 does. A survey MUST report each constant argument outside such a contract, marking calls that discard the result. It MUST list constant arguments to parameters without a contract.
**Verification**: Test

## Windows identity and native services

### PERS-PROFILE-7 ANSI code page selection

**Rule**: Without an explicit code page, the ANSI code page MUST be 936 for a Simplified Chinese executable and 932 otherwise. Markers are a name token `chs`, `cn`, `gb` or `zhcn`, a kana-free name with a GBK-only character, or GB2312 data text outnumbering Shift_JIS threefold. An explicit code page MUST replace the detected one before guest code runs.
**Verification**: Test

## ABI ownership

### PERS-ABI-1 Single signature source

**Rule**: API signatures MUST come from `data/abi` and explicit checked aliases. API front ends MUST NOT create conflicting declarations or calling conventions.
**Verification**: Test

Typed adapters MAY borrow their complete readable argument span for decoding. They MUST preserve scalar and aggregate bits, stack-word rounding, page permissions and the original failing argument when a complete span cannot be borrowed. Zero-argument calls MUST NOT read argument memory.

### PERS-ABI-2 Layer ownership

**Rule**: PE parsing MUST belong to `adv-pe`, loading policy to `adv-os`, and platform API behavior to its personality front end. Device implementations MUST NOT depend on personality crates.
**Verification**: Check — Cargo dependencies and `tools/layering/check_layers.py` enforce the layer direction.
