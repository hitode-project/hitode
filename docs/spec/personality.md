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

**Rule**: Dynamic discovery MUST expose only implemented or reviewed exports, except under `ADV_PERSONALITY_STRICT`. Forwarders MUST use their final target classification.
**Verification**: Test

### PERS-EXPORT-3 Review admission

**Rule**: A reviewed export MUST have a callable signature and registered failure. Implementing it MUST remove its review entry.
**Verification**: Test

### PERS-EXPORT-4 Reviewed failures

**Rule**: Reviewed exports MUST return their registered failure in both API modes and set LastError to 120. They MUST count reviewed calls without counting stub calls.
**Verification**: Test

### PERS-EXPORT-5 Diagnostic stubs

**Rule**: Strict mode MUST stop at an unimplemented diagnostic stub. Discovery mode MUST return the registered failure and record the call. Missing failure contracts MUST NOT default to success.
**Verification**: Test

### PERS-EXPORT-6 Data exports

**Rule**: Data exports MUST have real layout providers. Function thunks MUST NOT substitute for data, and unsupported data exports MUST NOT bind.
**Verification**: Test

### PERS-EXPORT-7 Guest entry precedence

**Rule**: Guest modifications to export entries MUST remain discoverable and executable regardless of the original implementation classification.
**Verification**: Test

### PERS-EXPORT-8 Bounded gap reporting

**Rule**: Personality diagnostics MUST distinguish rejected probes, reviewed calls, stub calls, and structural mismatches. Rejected names MUST be bounded to 256 entries with explicit dropped counts.
**Verification**: Review — the complete personality report and its 256-name boundary lack a dedicated end-to-end test.

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

## Offline networking

### PERS-NET-1 Offline Internet sessions

**Rule**: Synchronous WinINet and WinHTTP initialization MUST create library-owned sessions independently of transport availability. Session queries and closure MUST validate library ownership. Offline URL retrieval MUST fail without creating requests or reporting capability gaps.
**Verification**: Test

### PERS-NET-2 Local network addresses

**Rule**: Winsock byte-order and numeric address conversions MUST operate without network I/O or requiring WSAStartup. Invalid text MUST preserve output, and failed pointer or family validation MUST set the documented Winsock error. Legacy Winsock DLL aliases MUST reuse initialization state and local arithmetic.
**Verification**: Test

## Filesystem identity

### PERS-FILE-1 Invariant filesystem names

**Rule**: Filesystem name matching MUST use the shared invariant UTF-16 case mapping on every host. Non-ASCII names MUST use the same directory index and invalidation rules as ASCII names.
**Verification**: Test

Withdrawn: PERS-SCOPE-2, PERS-SCOPE-7, PERS-SCOPE-9

## Cryptography

### PERS-CRYPTO-1 Streaming digests

**Rule**: CryptoAPI MD5, SHA-1, SHA-256, SHA-384 and SHA-512 MUST compute their standard digests across arbitrary input splits. Hash duplication MUST copy an independently writable stream. Successful hash-value retrieval MUST finalize that hash, while size queries and short buffers MUST preserve its stream.
**Verification**: Test
**Basis**: RFC 1321, FIPS 180-4, and Microsoft CryptGetHashParam documentation.

### PERS-CRYPTO-2 CSP ownership

**Rule**: CryptoAPI contexts and hashes MUST use typed aliases in the shared Objects owner. Context reference counts MUST control context lifetime. Invalid handles and failed input validation MUST preserve hash state and unrelated output bytes.
**Verification**: Test

### PERS-CRYPTO-3 Symmetric transforms

**Rule**: CryptoAPI AES-128/192/256, DES, triple DES, RC2 and RC4 MUST perform their specified symmetric transforms. Key duplication MUST preserve an independent cipher state. Size queries and rejected calls MUST NOT advance cipher state.
**Verification**: Test
**Basis**: FIPS 197, FIPS 46-3, RFC 2268 and RFC 6229.

### PERS-CRYPTO-4 HMAC and derived session keys

**Rule**: CryptoAPI HMAC MUST compute RFC 2104 with the selected digest and supplied session key. CryptDeriveKey MUST finalize its base hash and use the documented CSP expansion for AES and triple DES. Plaintext key blobs MUST encode and recover the owned key material and export permissions.
**Verification**: Test
**Basis**: RFC 2104, RFC 4231 and Microsoft CryptDeriveKey documentation.

### PERS-CRYPTO-5 Random output boundaries

**Rule**: Random APIs MUST fill exactly the requested output range. Zero-length random requests MUST succeed without dereferencing a null buffer. Invalid output ranges MUST preserve guest bytes.
**Verification**: Test

### PERS-CRYPTO-6 Secure random source

**Rule**: RtlGenRandom, CryptGenRandom and symmetric CryptGenKey MUST use the shared host OS entropy adapter. They MUST NOT substitute a deterministic generator when that source fails.
**Verification**: Review — platform source selection and cryptographic entropy require review beyond statistical output tests.

## Process status

### PERS-PROCESS-1 Process and module queries

**Rule**: PSAPI and K32 aliases MUST query the existing guest process, Loader and section owners. Module queries MUST enforce process query and VM-read rights, including local real handles. Reported paths MUST use owned LDR names or section paths and the same device mapping as QueryDosDevice.
**Verification**: Test

## Detached console

### PERS-CONSOLE-1 Absent console handles

**Rule**: Console input, output and buffer configuration MUST fail with ERROR_INVALID_HANDLE when no console is attached. Redirected standard streams MUST NOT constitute console buffers. Ordinary detached-console failures MUST NOT report capability gaps.
**Verification**: Test

### PERS-CONSOLE-2 Process control handlers

**Rule**: SetConsoleCtrlHandler MUST maintain the process handler table and CTRL+C ignore attribute independently of console attachment. Removing an absent handler MUST fail. FreeConsole MUST succeed while detached and reset process control handlers.
**Verification**: Test
**Basis**: Microsoft FreeConsole and SetConsoleCtrlHandler documentation.

### PERS-CONSOLE-3 Console creation capability

**Rule**: AllocConsole MUST fail and report a capability gap when no host console transport is provided. It MUST NOT publish a console window or valid console handles.
**Verification**: Test

## Installer discovery

### PERS-INSTALL-1 Empty installed state

**Rule**: MSI discovery MUST report the profile's empty installed product and component sets without capability warnings. Queries MUST distinguish unknown products, unknown components and malformed identifiers. Unknown-item failures MUST preserve output buffers and direct-status APIs MUST preserve LastError.
**Verification**: Test

### PERS-INSTALL-2 Installer file hashes

**Rule**: MsiGetFileHash MUST compute the real MD5 of guest file bytes through the existing VFS and coherent file readers. A/W paths MUST share that implementation. Invalid hash structures and failed reads MUST preserve hash output.
**Verification**: Test
**Basis**: Microsoft MsiGetFileHash documentation and RFC 1321.

### PERS-NET-3 Offline provider routing

**Rule**: MPR connection and external-resource requests MUST fail ordinarily in the offline profile without publishing redirects. Connected and remembered-resource enumerators MUST be real Objects aliases over the profile's empty resource lists. Provider names and current-user queries MUST use local configuration independently of transport availability.
**Verification**: Test

### PERS-NET-4 Provider extended errors

**Rule**: WNetSetLastError and WNetGetLastError MUST share extended error codes and UTF-16 strings per guest thread. A/W conversion MUST use the existing process codec. Extended-error queries MUST truncate strings with terminators without replacing another thread's information.
**Verification**: Test
**Basis**: Microsoft WNetSetLastError, WNetGetLastError and WNetGetUser documentation.

### PERS-NET-5 Provider capability refusal

**Rule**: Network-provider capability/version queries MUST report a capability gap when provider DLL dispatch is unavailable. They MUST NOT fabricate successful provider structures.
**Verification**: Test

### PERS-CRYPTO-7 Binary text conversions

**Rule**: CryptBinaryToString and CryptStringToBinary MUST transform the actual input bytes, including Base64 headers and hexadecimal encodings. Automatic detection MUST report the selected format and header skip count. A/W variants MUST share ASCII format semantics without applying an ANSI code page.
**Verification**: Test
**Basis**: Microsoft CryptBinaryToString and CryptStringToBinary documentation; RFC 4648.

### PERS-CRYPTO-8 Conversion output boundaries

**Rule**: Binary conversion size queries MUST report the complete required output size without writing output buffers. Short buffers MUST report ERROR_MORE_DATA and the required size. Conversions MUST validate guest output permissions and preserve LastError on success.
**Verification**: Test
**Basis**: Microsoft CryptBinaryToString and CryptStringToBinary documentation.

### PERS-SYNC-1 Signal and wait ownership

**Rule**: SignalObjectAndWait MUST signal an existing event, release an owned mutex or increment a semaphore before waiting through the shared scheduler. Invalid wait handles MUST leave the signal object unchanged. Alertable waits MUST dispatch queued APCs and report WAIT_IO_COMPLETION.
**Verification**: Test
**Basis**: Microsoft SignalObjectAndWait documentation.

### PERS-PROFILEFILE-1 File-backed INI mutations

**Rule**: Profile queries and writers MUST share the INI parser, VFS layout and coherent file owners. Section replacement MUST preserve unrelated sections. Writers MUST retain UTF-16LE files and encode other files through the guest ANSI code page.
**Verification**: Test
**Basis**: Microsoft GetPrivateProfileSection, WritePrivateProfileSection and WritePrivateProfileString documentation.

### PERS-PROFILEFILE-2 Checksummed profile structures

**Rule**: Profile structure writers MUST store the actual bytes with their checksum. Readers MUST validate size and checksum before writing guest output. Invalid data and inaccessible outputs MUST preserve output bytes.
**Verification**: Test
**Basis**: Microsoft GetPrivateProfileStruct and WritePrivateProfileStruct documentation.

### PERS-PROFILEFILE-3 Native profile writes

**Rule**: Modified native file entries MUST control profile opens, writes, truncation and closes. Failed writes MUST NOT truncate the file. Temporary native handles and guest scratch MUST be released after synchronous completion.
**Verification**: Test
