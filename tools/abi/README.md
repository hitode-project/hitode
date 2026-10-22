# x86 ABI metadata

Win32 declarations come from Microsoft's [Win32Metadata](https://github.com/microsoft/win32metadata), fixed by version and SHA-256 in [source.json](../../data/abi/source.json).
Metadata and tooling are MIT licensed; SDK headers are not imported into this repository.
The offline generator uses Microsoft's `System.Reflection.Metadata` shipped with PowerShell 7.
No SDK, .NET SDK, metadata reader, WinMD, or network access is needed to build or run the Rust workspace.

Regenerate with PowerShell 7:

```powershell
pwsh -NoProfile -File tools/abi/generate.ps1 -Download
pwsh -NoProfile -File tools/abi/generate.ps1 -Download -Check
# A previously downloaded, exact WinMD can replace -Download:
pwsh -NoProfile -File tools/abi/generate.ps1 -Winmd path/to/Windows.Win32.winmd -Check
```

## Fixed core system exports

`system-exports.tsv` is separate from signatures and pins x86 Windows 10.0.26100.9549 NTDLL, Kernel32, and KernelBase export metadata.
The DLLs contain 2523, 1670, and 2037 names, plus 82 API-set name projections referenced by their forwarders.
`system-exports-source.json` records source versions and SHA-256 hashes; the catalog contains no native implementations, addresses, data values, or new ABI declarations.

Each code export has an executable entry in its synthetic MEM_IMAGE. Registered API front ends provide behavior.
Entries without verified ABIs fail before returning in every mode; arguments, stack cleanup, and successful results are never guessed.
Unknown names outside the catalog fail lookup; optional-capability queries for other DLLs retain their rules.
Thirteen non-executable-section data exports are marked `DataUnsupported`, preserving names and native ordinals but exposing zero EAT slots and failing lookup.
This avoids giving guests instruction addresses or invented data layouts.

PE files retain native forwarder strings.
Referenced API-sets map to core DLLs using namespace hash prefixes and the calling module.
This prevents cycles through Kernel32's default host mapping.
HeapAlloc and EnterCriticalSection therefore share actual entry addresses with NTDLL's RtlAllocateHeap and RtlEnterCriticalSection respectively.
KernelBase APIs can reuse registered Kernel32 API behavior; their entries still belong to their own image, without assuming equal cross-module addresses.
This export-presence policy does not imply complete implementation of that Windows version; the personality independently maintains compatibility version reports.

Regeneration reads metadata only; explicitly supply the three PE32 files' directory and native `apisetschema.dll`:

```powershell
python tools/abi/system_exports.py --system-dir <x86-system-directory> --api-set-schema <apisetschema.dll>
python -m unittest discover -s tools/abi -p test_system_exports.py
```

SDK 10.0.26100.0 `ntdll.lib` corroborates the five registered RTL critical-section entries' one-argument x86 stdcall convention: `_RtlInitializeCriticalSection@4`, `_RtlDeleteCriticalSection@4`, `_RtlEnterCriticalSection@4`, `_RtlTryEnterCriticalSection@4`, and `_RtlLeaveCriticalSection@4`.
The library's SHA-256 is `167fdee306171ad6bcaadb9471c6dcbf3159dab9e36ff7cab40c44e1ed8cbed2`.
These entries reuse critical-section state.
RTL Enter/Leave return NTSTATUS_SUCCESS on success, including guest fast paths.

## Signature-table projection

`signatures.tsv` records names, P/Invoke calling conventions, parameter types and x86 stack widths for the DLL scope.
It distinguishes parameter count from stack words: `SetFilePointerEx` has four parameters and five words.
Architecture attributes are filtered before type projection.
Conflicting signatures stop generation.
`structs.rs` contains the initial generated Pod structures, plus size, alignment and field-offset assertions, including the by-value `BLENDFUNCTION` and gradient mesh records.
The supplemental binary64 validation accepts the existing `pow` and `_hypot` declarations with two double-width x86 arguments.
`com.tsv` flattens inherited COM slots, including guest `this`, for DDraw, DSound, DInput, D3D9 and DirectShow.
`constants.tsv` preserves the referenced enum and flag values.
The package and WinMD remain in `target/tmp/abi-metadata` only.

External `System.Guid` values project to 16 opaque bytes with four-byte alignment (`[u32; 4]` in the generated Rust).
Struct packing follows WinMD, and any padding is explicit so Pod records never expose uninitialized host bytes.
Large arrays use zeroed defaults; records exceeding 255 stack words are pointer-only and do not get a by-value `GuestArg` implementation.
`D3DADAPTER_IDENTIFIER9` is 1100 bytes with packing 4; the authored D3D9 capability fixture checks these values and field offsets against the installed x86 SDK in addition to the generated assertions.

## Selected legacy exports and DLL aliases

DbgHelp is projected in full; ImageHlp retains only the `MakeSureDirectoryPathExists` alias.
Both share an ANSI directory API front end; `SHCreateDirectoryExW` in the metadata provides the Unicode counterpart.

`data/abi/selected-exports.tsv` projects explicit legacy registrations and DLL aliases from the same pinned WinMD. The input names source and destination exports and expected x86 stack widths, never copied parameter or return layouts.
NTDLL stays outside the full DLL scope; its selected declarations coexist with existing checked SDK supplements and native registrations.
MPR and DbgHelp are projected in full, while Shfolder and ImageHlp retain selected aliases.
Missing source declarations, changed stdcall stack widths, duplicate selections and conflicting aliases stop generation.
`tools/abi/test-selected-exports.ps1` checks these failures, complete alias equivalence, and selected-only namespaces.

`RtlFormatCurrentUserKeyPath` is absent from the pinned WinMD. Its existing registration cannot be projected from that source by inventing a declaration.

## SDK declarations absent from WinMD

`D3DXCreateTexture` and `D3DXCheckTextureRequirements` use Microsoft Learn's `D3dx9tex.h` prototypes for `_29` and `_43`, with eight/seven x86 WINAPI arguments and HRESULT returns.
Correction and creation share D3D9 capabilities and texture ownership without projecting the entire legacy SDK. D3DX math registrations share SDK prototypes across `_29` and `_43`, including axis rotations and homogeneous, coordinate, and normal Vec2 transforms.

The generator derives missing Zw signatures from Nt declarations without duplicating parameter layouts.
Personality registration binds missing public Zw names to the same Nt implementations; discovery does not depend on installing Win7 numeric entries.
The personality explicitly registers catalog aliases with matching ABIs absent from the SDK, checking every existing declaration.

`D3DXSaveSurfaceToFileInMemory` and `D3DXSaveSurfaceToFileW` supplement `sdk-input.tsv` using Microsoft Learn's `D3dx9tex.h` declarations; existing tools generate their signatures.
Both take five x86 stdcall stack arguments and return HRESULT; filenames project as Unicode strings.
The generator accepts only these exact declarations, sourced in `sdk-source.json`; the runtime's complete API registration audit verifies their ABIs.
`sdk-com-input.tsv` also generates five `ID3DXBuffer` slots: three IUnknown methods, pointer-returning GetBufferPointer, and DWORD-returning GetBufferSize.
`selected-exports.tsv` projects only `DwmSetWindowAttribute` from the pinned WinMD without expanding DWM's DLL scope.
Three OpenMP compiler-runtime entries are absent from WinMD; audits verify their cdecl/variadic conventions separately against Wine declarations and repository-owned native fixtures.

`sdk-input.tsv` supplements `msvfw32.dll!ICInstall/ICRemove/DrawDibOpen/Close/Draw/MCIWndCreateA/W/MCIWndRegisterClass` from SDK 10.0.26100.0 `um/vfw.h`.
`sdk-source.json` records the header hash and official source.
`DrawDibOpen(void)` takes no arguments; `VFWAPI` is stdcall on x86, and HDRAWDIB is a 32-bit HANDLE alias.
Close and Draw take one and thirteen 32-bit arguments respectively and return BOOL; the generator accepts only those three exact DrawDib declarations.
They enter the same signature table; `#[win32]` implementations provide behavior.
Runtime execution does not require SDK headers.
`MCIWndCreateA/W` take four x86 arguments and return HWND; `MCIWndRegisterClass` takes none and returns BOOL. These three use `VFWAPIV` (`WINAPIV`, x86 cdecl), so their caller owns stack cleanup.

`quartz.dll!AMGetErrorTextA` supplements a three-argument DWORD/WINAPI declaration from the same SDK's `um/errors.h` and x86 `quartz.lib`.
`sdk-source.json` records source hashes.
The generator accepts only the exact A signature with HRESULT/LPSTR/DWORD arguments.

The legacy desktop `EnableEUDC` BOOL declaration is supplemented from Microsoft Learn and corroborated by the SDK's x86 `gdi32.lib` `_EnableEUDC@4` import.
The generator accepts only that exact one-BOOL signature.
The guest frontend fails EUDC font linking explicitly; tests never alter the native desktop's EUDC state.

`sdk-com-input.tsv` supplies the 14 x86 slots of `IDxDiagProvider/IDxDiagContainer`, which are also absent from the pinned WinMD. `sdk-com-source.json` records the installed `um/dxdiag.h` header hash.
These declarations merge into the same `com.tsv`; the generator rejects duplicate, reordered or incomplete slots.

The system-service scope in `modules.txt` additionally projects PSAPI, MPR, MSI, NetAPI32, WinHTTP, UrlMon, IP Helper, SetupAPI, DbgHelp, UserEnv, Crypt32 and RPC. The pinned WinMD attributes canonical XInput declarations to 1.4 and Winsock functions to Ws2_32.
Selected legacy XInput and Wsock32 exports reuse those complete declarations; Wsock32 names are matched against the existing SDK ordinal-import catalog.
Signatures describe ABI availability, not implemented behavior; coverage continues to report unresolved implementations.

The closure's XInput 1.3 ordinals 2, 3 and 5 alias GetState, SetState and Enable.
Their selected declarations reuse the corresponding named WinMD ABI and their mutable guest entries.
Ordinal identities were verified from Microsoft's [June 2010 redistributable](https://www.microsoft.com/en-us/download/details.aspx?id=8109), `APR2007_xinput_x86.cab`, XInput 1.3 version 9.18.944.0.
The redistributable SHA-256 is `053f76dcbb28802e23341b6a787e3b0791c0fa5c8d4d011b1044172dbf89c73b`; the x86 DLL SHA-256 is `8d540d484ea41e374fd0107d55d253f87ded4ce780d515d8fd59bbe8c98970a7`.
Only export metadata was read; binaries and instruction bytes remain outside the repository.

For the closure's two undocumented version cases, the maintainer permits SDK gates and published Win7 export metadata as supplemental sources.
FlsGetValue2 is gated by `_WIN32_WINNT_WIN10` in the SDK's `fibersapi.h`; its precise later introduction is not inferred from that lower bound.
AppPolicyGetProcessTerminationMethod appears in the SDK's AppModel policy declarations and is absent from the published Win7 Kernel32 export catalog; its exact minimum remains undocumented.
The cited Win7 catalog is build 6.1.7100.0, rather than an authored SP1 native test; this comparison does not claim SP1 differential verification.
The post-Win7 rows pin the SDK header revision, and these names remain unregistered and unavailable to guest discovery.
SetHandleCount remains implemented: the Win7 export catalog contains it despite Learn's modern minimum-client label, and Learn describes its Win32 behavior as having no effect.

## CRT export catalog

WinMD lists no C runtime exports, so `crt-exports.tsv` names what each x86 CRT DLL exports: msvcrt, msvcrt20/40, msvcr70 through msvcr120, ucrtbase, vcruntime140, msvcp60 and the UCRT API-set contracts.
Names and the data flag come from the public-domain mingw-w64-crt module definitions, preprocessed for i386; `crt-exports-source.json` pins the commit and file hashes.
The catalog states existence only, never a convention or implementation promise; `tools/survey/coverage.py` compares it and `signatures.tsv` against registered APIs.
`closure.tsv` lists the functions sample game PEs import or look up by name; `tools/survey/closure.py` regenerates it from local game folders, and `coverage.py --closure` reports it.
`post-win7.tsv` lists catalogued functions introduced after Windows 7, with their minimum client and source; they stay unimplemented, so discovery hides them as on Windows 7, and coverage counts them as excluded.

Use the pinned WinMD's `SupportedOSPlatform` attribute, falling back to Microsoft Learn's minimum supported client when absent.
Keep exclusions sorted by DLL and export name; merge concurrent additions by their union and sort again.
The signature catalog remains complete: excluding discovery does not erase ABI evidence or turn missing behavior into a registered failure.

Multi-version desktop requirements need their earliest applicable client, rather than the first modern client in the list.
The pinned metadata incorrectly marks 262 MSI names as `windows8.0` and 96 older Winsock names as `windows8.1`.
Learn explicitly retains older clients for [MsiGetFileHash](https://learn.microsoft.com/en-us/windows/win32/api/msi/nf-msi-msigetfilehashw) and [WSAStartup](https://learn.microsoft.com/en-us/windows/win32/api/winsock2/nf-winsock2-wsastartup); these functions remain in scope.
Likewise, AddDllDirectory, RemoveDllDirectory and SetDefaultDllDirectories remain in scope because their requirements include Windows 7 with KB2533623.
XInput availability is DLL-specific: the [platform requirements](https://learn.microsoft.com/en-us/windows/win32/api/xinput/nf-xinput-xinputgetstate) exclude XInput 1.4 while retaining the legacy SDK and 9.1.0 versions.

```sh
python tools/abi/crt_exports.py --mingw <mingw-w64-checkout> --check
python -m unittest discover -s tools/abi -p test_crt_exports.py
```

Byte/UTF-16 set scans share unsigned code-unit membership and checked page reads
across all cataloged versions, without locale conversion or a fixed length cap.

## SDK CRT declarations

WinMD omits CRT exports.
`data/abi/crt-input.tsv` supplies supported x86 scalar and pointer declarations for CRT functions and data storage.
SDK declarations come from 10.0.26100.0 float.h, stdlib.h, errno.h, process.h, corecrt_startup.h, corecrt_wstdlib.h, corecrt_malloc.h, corecrt_wstring.h and signal.h.
Secure byte-copy declarations use the same SDK's `corecrt_memcpy_s.h`.
Scalar C++ allocation declarations use MSVC Build Tools 14.44.35207 `vcruntime_new.h`.
[crt-source.json](../../data/abi/crt-source.json) records their SHA-256 values and official documentation; no SDK header is copied.
Metadata.cs merges this input into the same sorted `signatures.tsv` and rejects conflicting rows.
The macro and runtime consume that one result through the existing signature resolver; neither has an extra CRT table or parser.
Empty parameter lists retain an empty sixth TSV column, so its final separator is intentional.

The process/error declarations additionally cover cataloged exports in all ten legacy CRT DLLs, UCRT and their API sets.
`msvcrt::versions` selects those entries by both export presence and the generated declaration, using one implementation per function family.
List-form exec/spawn entries use a variadic mandatory prefix; their signed x86 `intptr_t` result is 32 bits.
Version profiles select legacy errno validation or modern invalid-parameter callbacks.
Existing module/thread error cells remain the only errno/DOS-error owner, including API-set calls into UCRT. The virtual environment has no command processor, so valid subprocess requests fail with ENOENT and publish no handles or streams.
A `system(NULL)` query returns zero without changing the error cells.
`test-crt-input.ps1` verifies regeneration and rejection of changed process-list and binary-math ABI contracts.

CRT runtime callback declarations additionally project terminate, unexpected, translator and purecall aliases, including VC14's `__std_terminate`.
Terminate, unexpected and translator handlers use the existing CRT/thread owner; purecall handlers use CRT-global state.
VC14's standard terminate entry uses UCRT's slot.
Returning terminate handlers take the shared public abort path; returning unexpected handlers enter terminate.
Default abort/runtime-error exits use the Windows process owner with statuses 3/255,
bypassing CRT exit callbacks. Debugger hooks have no recipient in this profile.
Signal/raise and legacy exception filters share CRT module/thread signal slots;
FP-code and exception-pointer cells extend the existing errno allocation. Native
handler reset, context restoration and UCRT validation differences remain explicit.
Their callbacks execute as guest code, preserving explicit exits.

The x86 internal SEH helpers use explicit ABI rows: EH2/EH3 take four cdecl arguments, EH4 common takes six, and local/global helpers retain caller cleanup.
EH4 scope tables decode with the supplied cookie and invoke the supplied ECX security-check callback.
Scope filters and finally funclets receive the guest establisher's EBP. Accepted filters call the live public `RtlUnwind` entry before local cleanup and resume through a machine checkpoint that validates stack and continuation ancestry.
No host exception substitutes for guest SEH dispatch.
`crt_cpp_runtime` and `crt_seh` exercise these paths in both execution modes.

C++ throw declarations preserve the documented two-pointer stdcall ABI, while frame handlers use four cdecl pointers and the internal x86 FuncInfo-in-EAX contract.
Frame handlers interpret VC6/VC7/VC8+ absolute-pointer metadata, match qualified catchable types and PMD base adjustments, invoke guest copy constructors and unwind funclets, and retain exception objects across rethrows.
Catch completion runs the object's destructor before a checked nonlocal continuation.
Asynchronous frames invoke the installed thread translator with original Windows exception pointers.
Active C++ local-unwind scopes supply uncaught queries, including UCRT queries of VC14 cleanup; constructing or delivering a throw record does not alter that count.
Rethrow records retain the compiler's null object/ThrowInfo marker.
These internal contracts were checked against published Microsoft x86 runtimes; `crt-source.json` records the binaries' versions, hashes and observations.
`crt_cpp_eh` exercises real Windows delivery, nested/outer catches, cleanup, translator throws, object lifetimes and termination in both execution modes.

Byte/UTF-16 length, comparison, search and non-secure copying declarations now project cataloged versions through the same metadata-driven export table.
Their shared kernel checks every guest read/write, counts UTF-16 code units, stops bounded reads at their count, pads bounded copies after NUL, and terminates bounded concatenation.
Substring search uses one linear prefix table and returns the first match.
Invalid destination writes fail instead of reporting a successful partial copy.
`crt_strings` checks all CRT versions and public API-set projections, including page-end input and read-only output, in interpreter and JIT.

`_setjmp`, `_setjmp3` and `longjmp` now share version selection and the MSVC sixteen-DWORD x86 jump-buffer layout.
The variadic `_setjmp3` declaration retains only its mandatory buffer/count prefix.
Global unwinding calls live `RtlUnwind` before a compiler-provided local callback or the default SEH scope cleanup.
`__CxxLongjmpUnwind` takes one jump-buffer pointer with stdcall cleanup and reads saved FuncInfo from UnwindData and the saved target state.
Jump restoration uses the same checked stack-directed continuation protocol as catch entry, preserving saved nonvolatile registers and the zero-to-one result rule.
Guest buffer writes are checked.
C++ and SEH fixtures cover cross-frame cleanup before the saved continuation resumes in both execution modes.
The x86 `_EH_prolog` entry installs the EAX-supplied handler and preserves the compiler frame layout without an ordinary stack argument.
`__CxxQueryExceptionSize` returns its sixteen-byte registration buffer size; `_NLG_Return2` preserves incoming registers and flags at the debugger return boundary.
These internal contracts use the pinned Microsoft x86 runtime observations described above.

`python tools/abi/crt_exclusions.py` generates the exact CRT-task exclusions in `crt-exclusions.tsv`: the private API set, ConcRT namespace and legacy VC20/40 iostream families, including their data exports.
`--check` detects stale entries.
This scope manifest neither registers behavior nor changes export discovery; `tools/survey/coverage.py` continues to report the unfiltered catalogs.

Declarations include floating controls, error cells, exit arrays and initializer
ranges. Integer conversions share checked narrow/UTF-16 scanning, x86-width
saturation, radix formatting and module/thread errno, including EDX:EAX returns and native
end-pointer validation order. Legacy startup entries use Learn prototypes where SDK
headers omit them. Their guest argv/environment arrays consume Win32 strings and
process environment, with per-CRT guest-heap snapshots released on process detach.
Glob expansion requests containing wildcards explicitly fail rather than report
unexpanded arguments as success. Public `malloc/calloc/realloc/free/_msize/_expand`
fronts in every cataloged runtime and public heap API set reuse the existing RTL
process heap. Allocation failure and multiplication overflow publish ENOMEM;
zero-byte malloc/calloc retain distinct one-byte blocks. A nonnull zero-sized
realloc releases its block, while `_expand` retains one live byte and uses the
RTL in-place flag. Failed resizing preserves the old block. NULL `_msize/_expand`
inputs use the runtime's parameter-validation profile. Tests check every alias.
Aligned malloc/free/size/realloc/recalloc fronts and their offset variants retain
the native x86 guest header pointing into the same process heap. Resizing keeps
the old backing until allocation and copying succeed; recalloc zeros the added
extent. VC80/VC90's native zero-alignment acceptance is a version parameter;
UCRT and the other profiles require the documented nonzero power of two. Native
aligned size queries validate NULL and require the original alignment/offset.
Tests check all versions and zeroing, release and error contracts in both backends.
Legacy scalar `operator new/delete` use that same heap, including distinct live
zero-byte allocations. The throwing `new` overload explicitly stops on allocation
failure because guest `std::bad_alloc`/new-handler dispatch is not implemented;
it never returns a fake NULL result. No array, nothrow, sized or aligned overload
is advertised by this slice.
`memcpy_s/memmove_s` reuse guest range checks, the common memory copy, errno and
invalid-parameter callback state. Both accept zero count without pointer access.
On parameter errors, `memcpy_s` clears the declared destination capacity and
`memmove_s` leaves it unchanged. Only `memmove_s` defines overlapping-copy behavior.
Legacy `_onexit`/`atexit` and UCRT `_crt_atexit` share the existing guest exit-array
executor with per-CRT ownership. `_cexit` drains it, `exit` drains before Win32
process termination, and `_exit` bypasses the callbacks. Direct `ExitProcess` and
raw PE-entry returns do not synthesize a C-main return. Pending arrays and headers
are released at the existing process-detach boundary. UCRT `_onexit`/`atexit`
are static shims and are not advertised as native DLL exports.
Cataloged legacy `_acmdln`/`_wcmdln` rows describe pointer **data**, as declared in
`corecrt_startup.h` and [Microsoft's command-line variable documentation](https://learn.microsoft.com/en-us/cpp/c-runtime-library/acmdln-tcmdln-wcmdln).
Cataloged `_adjust_fdiv` compatibility cells are also data, with a zero
initial value; its data-section export was checked in the installed Microsoft
x86 MSVCRT PE rather than represented by a function thunk. Storage rows use
convention `data`, a scalar/pointer cell width and no parameters. The generator
rejects malformed data declarations; the function macro rejects them as callable
signatures. Registry assembly initializes pointer descriptors from the single
Win32 command-line owner before module loading. Synthetic PE files and mapped
modules publish the same eight-byte-aligned RW/NX cells; aliases share symbol
identity, while distinct symbols with equal initial bytes retain separate cells.
Static IATs and dynamic lookups refer to those cells. Getter calls never reset
guest writes. Explicit code-page configuration refreshes the existing ANSI buffer
only when the code page changes and propagates write failures to the caller.
These bindings use normal Rust behavior functions over the guest FpuEnvironment,
not host floating-control instructions. Modern `__control87_2` is not invented
for the system MSVCRT profile, whose native export is absent on the oracle host.
SDK prototypes establish ABI; authored MSVCRT/UCRT comparisons establish the
selected behavior contract, not every historic VC runtime DLL revision.

An API has one behavior function:

```rust
#[adv_abi::win32(kernel32, name = "QueryPerformanceCounter", level = implement)]
fn query(cx: &mut adv_abi::Env<'_>, out: adv_abi::GuestPtr<i64>)
    -> Result<i32, adv_cpu::backend::MemFault>
{
    out.write(cx.cpu, 123)?;
    Ok(1)
}
// The macro generates query_ABI. The personality's registration list uses:
// registry.register(query_ABI);
```

The macro verifies the metadata parameter count, guest value kind, stack width, calling convention and return kind.
`Outcome` returns remain available for guest callbacks and scheduler waits.
Variadics and unsupported aggregate returns require a dedicated projection; they cannot silently pass through the macro.
By-value Pod structures are decoded as stack copies.
Guest memory faults stop the call instead of decoding unreadable arguments as zero.
`GuestPtr` enforces guest permissions.

Strict mode stops on the first generated stub.
Discovery records every stub in `ApiDiagnostics`, returns `E_NOTIMPL` for metadata `HRESULT` results or a reviewed entry from `failures.tsv`, and sets thread-local LastError to 120.
Unknown return contracts and unsupported signatures stop even in discovery: metadata describes an ABI, not a universally safe failure result.
Outputs are left untouched, and discovery results never count as compatibility acceptance.

Entries reviewed in `data/abi/dynamic_exports.tsv` are not stubs.
In both modes they return their registered failure, set LastError to 120, and are counted in `ApiDiagnostics::reviewed_calls` without affecting acceptance eligibility, so the guest sees the same result whichever mode reports it.
A review must name an unimplemented declaration with a registered failure; `personality_invariants` in adv-runtime enforces that.

Legacy registration remains available.
`Registry::signature_conflicts()` reports known metadata conflicts without changing legacy behavior; `missing_signatures()` reports signatures absent from the generated table, including remaining CRT and non-SDK exports.
`coverage()` separates the three declared levels from unclassified legacy APIs.
`exports()` supplies the synthetic PE writer with the union of implementations and generated exports.
This framework changes host binding only: all calls still execute the guest thunk, preserving guest entry hooks, IAT rewrites and vtables.

## Legacy coverage audit

`cargo test -p adv-runtime --test abi_metadata -- --nocapture` invokes the same pure registration function used by production.
It audits both Guest/Native codec policies and the diagnostic wsprintf override, without loading an image or opening an audio device.
Every known signature conflict must be zero.
New unclassified metadata omissions fail the audit instead of disappearing into an exception list.
To refresh the complete classified list, set `ADV_ABI_AUDIT_PATH` to the **absolute** path of `data/abi/legacy-signatures.tsv` before running the test.

WinMD does not encode export ordinals.
The internal entries are outside the public Windows ABI. `GetEnvironmentStringsA` is a legacy alias of the metadata's `GetEnvironmentStrings`; the Interlocked compiler intrinsics are absent from its P/Invoke methods.
These omissions do not suppress any known metadata conflict.

## SDK ordinal imports

WinMD does not describe import ordinals.
`import_ordinals.py` reads the official x86 Windows SDK **short import records**, not DLL implementations, following [Microsoft's import-library format](https://learn.microsoft.com/en-us/windows/win32/debug/pe-format#import-library-format).
`import-ordinals.tsv` stores 865 named function ordinal aliases from SDK 10.0.26100.0; its header records input-library SHA-256 hashes.
Only ordinal imports are projected; name-import hints are not export ordinals, and data imports are excluded.
XInput 1.4 comes from SDK `xinput.lib`, whose library name differs from the DLL stem.

```powershell
python tools/abi/import_ordinals.py --sdk-dir 'C:\Program Files (x86)\Windows Kits\10\Lib\10.0.26100.0\um\x86' --check
```

An SDK ordinal and its name share the same mutable thunk entry.
Explicit legacy ordinal handlers take precedence.
A table entry adds no implementation or guessed signature: if neither a known signature nor implementation exists, calling it still stops.
The self-authored `api_stubs.c` fixture covers the real SDK's static `d2d1.dll!#1` import, strict stopping, discovery chaining and correct failure returns.

The CRT export audit removes three nonexistent handwritten fronts: VC80/VC90 `time` and the public string API-set's `memchr`.
Microsoft x86 PE export tables verify their absence; `crt-source.json` records versions, sources and hashes.
The genuine `_time32`/`_time64` and UCRT/VCRuntime `memchr` entries remain separate exports.
No matching CRT P/Invoke declarations exist in the pinned WinMD, so its SupportedOSPlatform attributes supply no CRT exclusions.
Internal helpers are not classified as post-Windows-7 merely from a compiler release date.

Multibyte pointer movement, byte/character counts, contextual classification and bounded copies now share one kernel across cataloged CRT versions.
`_setmbcp` and `_getmbcp` use the existing module locale owner, independently of LC_CTYPE and Win32 ACP; public API-set fronts use UCRT state.
Version profiles select legacy UTF-8 rejection and modern invalid-parameter handling.
The lexical path splitter uses this same multibyte setting in both ordinary and secure fronts.
Byte limits never read a partial pair's trailing byte; bounded copies clear incomplete final pairs and pad after NUL. `crt_mbcs` checks CP932/936, module isolation, API sets, invalid callbacks and page boundaries in interpreter/JIT. Explicit locale handles and thread-locale configuration are separate work.

Cataloged CRT process identity and delay fronts use SDK process.h/stdlib.h cdecl declarations and the live GetCurrentProcessId/Sleep entries.
`_sleep(0)` requests one millisecond, matching the pinned VC90 x86 entry; suspension uses the Windows scheduler and clock.
`crt_process` checks all runtime projections and the public runtime API set in interpreter and JIT.

`crt-kind-overrides.tsv` corrects import-library `DATA` markers that mingw uses for replacement math/fenv, FP-reset and timezone functions.
SDK declarations and Microsoft x86 PE sections establish these as code; `_HUGE`, `_iob` and genuine data remain data.
The export generator applies evidence only to names already present in its source, records the evidence hash, and rejects attempts to invent an export.
`crt-source.json` records the declaration and native-section provenance.

Legacy data cells now use the same version/catalog selection as code.
Each cataloged `_HUGE` is a real writable eight-byte double initialized to positive infinity; `_adjust_fdiv` is a zero-initialized int for the conforming virtual processor.
All legacy command-line cells point to the existing Win32 string owner while retaining independent guest-writable pointer storage.
`crt_data` checks static IATs and real dynamic lookup on all three CPU backends.
The ABI generator accepts double data only as data and rejects function reinterpretation.

Cataloged exit and initializer exports now select the shared runtime table across every CRT version.
Ordinary and quick-exit callbacks retain separate guest arrays under the existing module owner, heap and lock; API sets share UCRT state.
Full cleanup closes that CRT's streams after callbacks; fast cleanup preserves them.
`crt_exit` checks version projections, reentrant LIFO draining, quick-array isolation, initializer stopping and stream ownership on interpreter and JIT.

In-place reversal and set exports share checked byte/UTF-16 kernels across cataloged CRT versions, including the verified UCRT `_o_` wrappers.
Secure set fronts preserve the native incremental writes, original-suffix validation and EINVAL order; counts and capacities measure code units.
`crt_strings` covers all projections, surrogate units, error callbacks, zero counts and page boundaries in interpreter/JIT.

CRT remove, unlink and rename fronts now call the live Windows mutation entries through one versioned adapter.
UCRT narrow paths convert with the existing filename codec and retain per-argument RTL buffers until completion; legacy fronts retain their A/W calls.
DOS error translation preserves the pinned first-match table, fallback ranges and UCRT Unicode-error difference.
`crt_file_mutation` checks every projection, CP932/936 overlay effects, provider failures, buffer cleanup and allocation/validation ordering in interpreter/JIT.
