# Win32 personality

ID prefix: WIN32

This spec covers Win32 subsystems of the personality: images and loader state, shared objects, USER, GDI and other API boundaries. `adv-win32` and `adv-os` implement them, and native fixtures in `tools/fixtures/` compare them with Windows.

## Images and loader state

### WIN32-IMAGE-1 Atomic rebasing

**Rule**: Rebasing MUST validate the header and entire relocation table before modifying the image. Success MUST publish the actual ImageBase while retaining the parsed preferred base. Without relocations, absolute addresses MUST remain unchanged.
**Verification**: Test

### WIN32-IMAGE-2 Private image writes

**Rule**: The first real write to a private image page MUST retire its COPY state and invalidate changed code. Protection changes MUST NOT restore sharing. IAT binding MUST privatize its original section while relocation alone preserves COPY.
**Verification**: Test

### WIN32-IMAGE-3 View spans

**Rule**: Image and section views MUST account for their actual page spans. The remaining allocation-granule tail MUST query as free with `PAGE_NOACCESS`, but MUST NOT permit an independent reservation.
**Verification**: Test

### WIN32-IMAGE-4 Private copy protection

**Rule**: Private allocation with WRITECOPY MUST fail with `STATUS_INVALID_PAGE_PROTECTION`. Private protection with WRITECOPY MUST return `STATUS_INVALID_PARAMETER_4` and old protection 1. Win32 equivalents MUST report error 87.
**Verification**: Test

### WIN32-IMAGE-5 LDR publication

**Rule**: Load and memory lists MUST place NTDLL after the executable and before ordinary DLLs; initialization order MUST place NTDLL first. Later publication MUST preserve record addresses, reference counts, and other modules' relative order.
**Verification**: Test

### WIN32-IMAGE-6 Process DEP

**Rule**: The first executable image MUST fix process DEP from its NX_COMPAT flag under the OptIn system policy. Without that flag, readable pages MUST also execute while queries report guest protections. GetProcessDEPPolicy MUST report the decision.
**Verification**: Test

### WIN32-IMAGE-7 Process version

**Rule**: GetProcessVersion for the calling process MUST return the subsystem version in its mapped image header. Other process IDs MUST fail with error 87.
**Verification**: Test

### WIN32-IMAGE-8 Image sections

**Rule**: SEC_IMAGE views MUST lay out private copies with section page protections, relocating when the preferred base is taken. They MUST report MEM_IMAGE without binding imports or running code. Synthetic system DLL copies MUST reach the host exports.
**Verification**: Test

## Shared objects

### WIN32-OBJECT-1 RichEdit lifetime

**Rule**: RichEdit class registration MUST follow the shared module reference lifetime and reuse USER text state and A/W conversion. Extended rich-text, drawing, streaming, and OLE requests MUST fail explicitly.
**Verification**: Test

### WIN32-OBJECT-2 SRW identity

**Rule**: Win32 and NT SRW operations MUST share the guest lock word and existing wait owner. SRW locks MUST be nonrecursive and MUST NOT promise FIFO or fairness.
**Verification**: Test

### WIN32-OBJECT-3 Condition variables

**Rule**: Condition waits MUST atomically register and release their CS or SRW lock. Notification or timeout MUST reacquire through the original lock front end. Wakeups MUST NOT accumulate.
**Verification**: Test

### WIN32-OBJECT-4 Large heaps

**Rule**: Heap, RTL, CRT, Global, and Local allocation MUST share storage. Large blocks MUST start zeroed without ZERO_MEMORY, retire their mapping on release, and zero an enlarged moving-realloc tail. Moving realloc MUST preserve prior bytes, and blocks up to 16 KiB MUST NOT expose bytes another size freed.
**Verification**: Test

### WIN32-OBJECT-5 Persistent storage

**Rule**: Game writes MUST use a private overlay and registry persistence. Installation-directory writes MUST reach the mapped Program Files folder directly; a launched game's own folder MUST retain overlay semantics.
**Verification**: Test

### WIN32-OBJECT-6 Task memory

**Rule**: Shell and OLE MUST share process IMalloc and task allocation ownership. Filesystem folder selection MUST return bounded opaque PIDLs through a real modal window and BFFM callbacks.
**Verification**: Test

### WIN32-OBJECT-7 HGLOBAL streams

**Rule**: HGLOBAL streams MUST share Global storage, while clones retain independent cursors. Final release MUST perform delete-on-release. Read, Write, Seek, SetSize, Stat, and CopyTo MUST use that storage and real guest interface calls.
**Verification**: Test

### WIN32-OBJECT-8 Overlay whiteouts

**Rule**: Deleting a read-only game file MUST record a persistent whiteout instead of failing. The file MUST stay hidden until the guest writes that name again. Directory lookups MUST be answered from an index that only VFS-made changes invalidate, so host changes made outside the runtime during a run stay invisible.
**Verification**: Test

### WIN32-OBJECT-9 Shared small heap blocks

**Rule**: Registered heaps MUST share validated headers and generation-tagged lists between guest and host calls, preserving requested sizes, zeroing, and live hooks. Guest operations MUST validate handles through a bounded registry, with locking and destruction preventing new operations and draining existing operations. HeapWalk and HeapValidate MUST account for busy and free blocks and reject corrupt lists without losing ownership.
**Verification**: Test

| Eligibility or limit | Required behavior |
|---|---|
| Process heap | Register before guest execution |
| Growable HeapCreate heap, options limited to NO_SERIALIZE and CREATE_ENABLE_EXECUTE | Register if a slot is available |
| Fixed maximum size or other creation options | Retain host allocation |
| Registry capacity | 16 heaps including the process heap, stored in its control page |
| Slab capacity | 128 slabs per registered heap, then ordinary host allocation |
| Slab replenishment | Start each size class at 16 KiB and double exhausted-class replenishments up to 1 MiB |
| Slab lookup hint | Validate its registered range; invalid or mismatching hints retain the full search |
| Destroyed registration | Remove before releasing arenas and initialize fresh metadata on address reuse |

## Windows and input

### WIN32-USER-1 Window properties

**Rule**: A/W properties MUST share case-insensitive atom identities and retain raw DWORD values, including zero. Enumeration MUST use a stable snapshot and real guest callbacks, preserve names across waits, and return the last callback result.
**Verification**: Test

### WIN32-USER-2 Property retirement

**Rule**: Window destruction MUST clean properties after NCDESTROY callbacks and release string-key atom references. Caller-supplied atoms MUST remain caller-owned. RemoveProp MUST NOT release an atom reference.
**Verification**: Test

### WIN32-USER-3 Popup completion

**Rule**: Popup projection MUST follow initialization callbacks, and selection MUST revalidate the live enabled menu tree. Flags MUST select command return or notification. Cancellation, destruction, and EndMenu MUST retire tracking without taking ownership of HMENU.
**Verification**: Test

### WIN32-USER-4 Popup limits

**Rule**: Popup tracking MUST reject recursion, cross-thread owners, and selectable bitmap or owner-draw entries. It MUST NOT expose a guest menu window or promise dynamic native submenu redraw.
**Verification**: Test

### WIN32-USER-5 Native menu projection

**Rule**: Windows and macOS menus MUST remain independent projections of guest menu state and use completion signals to end native tracking. Hidden operation MUST validate selections through the same guest menu contract.
**Verification**: Review — USER tests cover guest tracking, but no test jointly checks native menu ownership and hidden selection validation.

> Note: HOST-MENU-1 and HOST-MENU-3 in [host.md](host.md) cover the native menu side.

### WIN32-USER-6 Window placement

**Rule**: SetWindowPlacement MUST validate the complete structure and length before changing state, then use USER geometry, visibility, and activation callbacks. Normal windows MUST recover offscreen placement into the virtual work area; tool windows MUST preserve screen coordinates.
**Verification**: Test

### WIN32-USER-7 Placement state and limits

**Rule**: Placement MUST retain minimum position and restore-to-maximized state in the window owner. Asynchronous cross-thread placement and reversed or overflowing restore rectangles MUST fail explicitly.
**Verification**: Test

### WIN32-USER-8 Physical double clicks

**Rule**: Double clicks MUST require CS_DBLCLKS, matching window and button, and configured time and screen-distance bounds. Delivery MUST retain down/up/double/up ordering and physical key state. Focus loss or window destruction MUST clear candidates.
**Verification**: Test

### WIN32-USER-9 Double-click configuration

**Rule**: Double-click APIs and metrics MUST share guest USER configuration, with timing independent of execution speed. They MUST NOT change host settings or transform guest-posted mouse messages.
**Verification**: Test

### WIN32-USER-10 Class state

**Rule**: A/W class operations MUST share one owner and class-owned menu strings. Replacing a procedure MUST preserve the old procedure's encoding. Unsupported cross-encoding procedure thunks MUST fail explicitly.
**Verification**: Test

### WIN32-USER-11 Timed messages

**Rule**: Same-thread SendMessageTimeout MUST reuse text conversion; cross-thread value messages MUST use the send queue. Cross-thread, cross-encoding text MUST fail without publishing storage that can expire before receipt.
**Verification**: Test

### WIN32-USER-12 Committed input

**Rule**: Committed text MUST reach keyboard focus as UTF-16 or packed ANSI `WM_IME_CHAR`, skipping unrepresentable characters. DefWindowProcA MUST split double-byte characters into lead and trail `WM_CHAR`; DefWindowProcW MUST preserve each unit.
**Verification**: Test

### WIN32-USER-13 IME boundaries

**Rule**: The personality MUST NOT fabricate IME windows or input contexts. Composition and `ImmGetCompositionString` MUST remain explicitly unavailable.
**Verification**: Test

### WIN32-USER-14 Owner-draw button projection

**Rule**: Native dialog adapters MUST project BS_OWNERDRAW as ordinary buttons using guest titles, preserving guest style, identity, input, and callbacks. They MUST report unavailable owner-draw appearance and MUST NOT fabricate images for unsupported image controls.
**Verification**: Review — no existing test checks both native button projection and the appearance capability diagnostic.

### WIN32-USER-15 Message box text encoding

**Rule**: MessageBoxA text and caption MUST use the ANSI code page unless that reading is not ordinary text while the other of 932 and 936 is. Translation patches can leave individual strings in the original encoding.
**Verification**: Test

### WIN32-USER-16 Host pointer reach

**Rule**: Host pointer input MUST reach every displayed point of the top-level window, even where it extends past the virtual display. A guest ClipCursor MUST still confine it, and guest SetCursorPos MUST stay on the display.
**Verification**: Test

### WIN32-USER-17 Status bars

**Rule**: CreateStatusWindow and the status bar class MUST keep parts, part text, text types and simple mode as Windows reports them. The bar MUST align to its parent's bottom or top edge unless CCS_NORESIZE or CCS_NOPARENTALIGN says otherwise. Hosts show the text only, and owner-drawn parts, icons and tips MUST warn.
**Verification**: Test

### WIN32-USER-18 List boxes

**Rule**: The ListBox class MUST keep items, item data, selection and top index, inserting in case-insensitive order under LBS_SORT. A host selection MUST update the guest selection and notify the parent with LBN_SELCHANGE only under LBS_NOTIFY. Multiple selection and string-less owner-drawn lists MUST warn.
**Verification**: Test

### WIN32-USER-19 Host activation

**Rule**: Host application activation MUST reach the active top-level window as WM_ACTIVATEAPP, WM_NCACTIVATE, then WM_ACTIVATE. Deactivation MUST use the reverse order.
**Verification**: Test

### WIN32-USER-20 Input inspection

**Rule**: The host input view MUST report the displayed top-level window and its thread's focus, active and capture windows. A host hit test at a point of the displayed window MUST name the window that ordinary mouse input there reaches.
**Verification**: Test

## Drawing and text

### WIN32-GDI-1 Virtual display

**Rule**: Basic and extended A/W display queries MUST describe one virtual primary display using complete SDK DEVMODE layouts. They MUST NOT expose host driver-private bytes.
**Verification**: Test

### WIN32-GDI-2 Regions

**Rule**: GDI regions MUST use bounded nonoverlapping rectangles under one object owner, scan-converting curves and polygons per row. Rotating or shearing ExtCreateRegion transforms and shaped window regions MUST fail. NULL SetWindowRgn restoration and absent-region queries MUST NOT fabricate shapes.
**Verification**: Test

### WIN32-GDI-3 Presentation canvas

**Rule**: A GDI canvas MAY belong to a root or visible presentation window. Hidden parented-window DCs and screen DCs with an existing presentation window MUST NOT resize or erase another window's pixels. Independent-window drawing and layered composition MUST fail explicitly.
**Verification**: Test

### WIN32-GDI-4 Parent changes

**Rule**: SetParent MUST preserve local coordinates and CHILD/POPUP styles while updating the parent-child tree and window queries.
**Verification**: Review — no dedicated test checks the complete SetParent geometry, style, and query contract.

### WIN32-GDI-5 Partial DIB upload

**Rule**: SetDIBitsToDevice MUST upload supplied scan bands using shared 1/4/8/16/24/32-bit decoding, DC origins, and clipping. Unreadable input MUST leave pixels unchanged. Out-of-range top-down bands MUST fail explicitly.
**Verification**: Test

### WIN32-GDI-6 Indexed storage

**Rule**: Indexed 1/4/8-bit surfaces MUST retain actual bit order, DWORD stride, and independent palettes. Drawing and readback MUST share storage and preserve neighboring bits and padding. CreateCompatibleBitmap MUST use shared storage even with a draw backend installed.
**Verification**: Test

### WIN32-GDI-7 Monochrome coverage

**Rule**: Monochrome text MUST preserve coverage as index bits even when both palette entries contain identical RGB. Drawing MUST preserve the original palette.
**Verification**: Test

### WIN32-GDI-8 DIB palettes and readback

**Rule**: Indexed DIB input MUST interpret RGBQUAD tables or logical-palette WORD indices according to usage. GetDIBits MUST validate outputs before publishing pixels or palettes and preserve format-query boundaries. True-color input MUST ignore palette usage flags, while unsupported conversions and unknown usage MUST fail.
**Verification**: Test

### WIN32-GDI-9 Standard bitfields

**Rule**: Standard 32-bit RGB bitfields MUST share BGRA storage while preserving scan order and the high byte. Nonstandard masks MUST fail explicitly.
**Verification**: Test

### WIN32-GDI-10 Selected font metrics

**Rule**: Glyph widths, ABC spacing, and Unicode coverage MUST use the selected device font. Windows MUST preserve native fallback and bitmap-font outcomes; portable measurements MUST use outline advances, hinted bounds, and primary-face coverage.
**Verification**: Test

### WIN32-GDI-11 Character placement

**Rule**: Windows character placement MUST validate bounded output arrays and preserve native partial-result behavior. ANSI placement MUST require matching guest and host ACP for the default charset and retain explicit-charset conversion. Portable complex layout MUST fail explicitly.
**Verification**: Test

### WIN32-GDI-12 Encoded font metrics

**Rule**: ANSI widths and kerning MUST use the selected charset and device-provided default glyph. Wide outline metrics MUST preserve their own native short-buffer rules. Portable wide metrics MUST take first and last characters from the Unicode cmap, and break and default characters from OS/2 usFirstCharIndex.
**Verification**: Test

### WIN32-GDI-13 Indexed readback formats

**Rule**: GetDIBits MUST support same-depth indexed and 24/32-bit RGB readback. Indexed format queries MUST write only headers. PAL readback MUST return sequential WORD palette entries and map pixels to the current logical palette.
**Verification**: Test

### WIN32-GDI-15 Memory fonts

**Rule**: AddFontMemResourceEx MUST register a copy as a process-private, non-enumerable resource, report its face count, and return a distinct handle. RemoveFontMemResourceEx MUST release only that registration. Process cleanup MUST release remaining memory fonts.
**Verification**: Test

## API boundaries

### WIN32-API-1 Folder paths

**Rule**: Special-folder PIDLs MUST use mapped CSIDL paths and task allocation, with A/W path access and ILFree, CoTaskMemFree, or IMalloc release. Unsupported tokens and namespaces MUST fail. Recursive directory front ends MUST honor live NT create, query, and close boundaries.
**Verification**: Test

### WIN32-API-2 Absent controllers

**Rule**: XInput 9.1.0, 1.3, and 1.4 MUST report no configured controller without modifying output buffers. Named and supported ordinal entries MUST share identity.
**Verification**: Test

### WIN32-API-3 Directory notifications

**Rule**: FindFirstChangeNotification MUST return INVALID_HANDLE_VALUE and ERROR_NOT_SUPPORTED without creating a wait object. FindNextChangeNotification and FindCloseChangeNotification MUST fail with invalid-handle errors without consuming unrelated handles.
**Verification**: Review — no dedicated test checks notification failures and preservation of other handle kinds.

### WIN32-API-4 Thread flags

**Rule**: CreateThread and CreateRemoteThread MUST recognize CREATE_SUSPENDED and stack reservation while ignoring other Win32 creation bits. Converted NtCreateThreadEx flags MUST still receive independent validation.
**Verification**: Test

### WIN32-API-5 Processor configuration

**Rule**: The hardware CPU registry key MUST share A/W and NT registry ownership, expose the virtual CPU profile and 1000 MHz, and remain volatile. Writes, deletions, child creation, and explicit write opens MUST fail; legacy and MAXIMUM_ALLOWED opens MUST be read-only.
**Verification**: Test

### WIN32-API-6 Ideal processor

**Rule**: Ideal-processor APIs MUST share thread identity and the single-CPU profile, accepting processor 0 and using 32 to query the prior value. Ex APIs MUST validate group fields, allow overlapping input/output, and preserve distinct query-output and setter-pointer failures. They MUST NOT bind host threads.
**Verification**: Test

### WIN32-API-7 Wide strings

**Rule**: Wide APIs MUST preserve UTF-16 units, including embedded nulls and raw font-name fields. Zero-capacity LoadStringW MUST return the original resource pointer. CharNextW MUST follow the fixed USER32 traversal table rather than Unicode grapheme boundaries.
**Verification**: Test

### WIN32-API-8 Absent consoles and devices

**Rule**: Console reads and code-page operations MUST fail without a console and MUST NOT change ACP. Wide waveOut, MIDI, and property-sheet entries MUST retain their explicit absent-device or unsupported behavior.
**Verification**: Test

### WIN32-API-9 Lexical shell paths

**Rule**: Shell path operations MUST remain lexical and independent of VFS current directories. ANSI paths MUST preserve DBCS boundaries, wide paths MUST preserve UTF-16 units, and wildcard matching MUST avoid recursive suffix expansion.
**Verification**: Test

### WIN32-API-10 Picker failure

**Rule**: Unavailable file dialogs MUST report initialization failure through thread-local CommDlgExtendedError rather than pretending user cancellation. Shell selection and item queries MUST NOT publish fabricated PIDLs or file information.
**Verification**: Test

### WIN32-API-11 Recursive Shell directories

**Rule**: Unicode `SHCreateDirectory` and `SHCreateDirectoryExA/W` MUST share recursive directory behavior. Intermediate files MUST produce path-not-found, while existing final files MUST produce already-exists.
**Verification**: Test

### WIN32-API-12 Linguistic comparison

**Rule**: Windows `lstrcmpA/W`, `lstrcmpiA/W`, and `CompareStringA/W/Ex` MUST compare with the explicit guest locale. ANSI comparison MUST distinguish locale encoding from `LOCALE_USE_CP_ACP`.
**Applies to**: Windows
**Verification**: Test

### WIN32-API-13 Portable comparison boundary

**Rule**: Portable comparison MUST support UTF-16 code-unit ordering and case folding. Unsupported linguistic flags and versioned sorting information MUST fail explicitly.
**Applies to**: Non-Windows hosts
**Verification**: Review — the native comparison fixture runs only on Windows and does not exercise portable flag rejection.

### WIN32-API-14 Legacy file handles

**Rule**: HFILE functions MUST follow CreateFile, ReadFile, WriteFile, SetFilePointer, and CloseHandle semantics and return HFILE_ERROR on failure. A zero-length `_lwrite` MUST truncate at the current position.
**Verification**: Test

### WIN32-API-17 Fixed DOS devices

**Rule**: DefineDosDeviceA/W MUST fail with ERROR_ACCESS_DENIED. They MUST NOT change logical drives or DOS-device queries.
**Verification**: Test

### WIN32-API-18 MinGW CRT imports

**Rule**: msvcrt `_stricmp`, `_strcmpi` and `_strnicmp` MUST fold only ASCII letters and return the byte difference. `__p__iob` MUST return one stable stream table per CRT module. `abort` and `_amsg_exit` MUST end the process with codes 3 and 255.
**Verification**: Test

### WIN32-API-19 CRT stdio read buffering
**Rule**: CRT streams MUST use 4096-byte read-ahead, preserve logical positions and EOF, and synchronize unread bytes before writes, seeks or flushes. Buffered reads and position queries MAY use EnvLeaf under A64-CTL-12 after complete destination checks. An overridden `NtReadFile` MUST retain unbuffered reads.
**Verification**: Test

### WIN32-INPUT-1 Mouse tracking

**Rule**: `TrackMouseEvent` and `_TrackMouseEvent` MUST share per-thread requests, queries, and selective cancellation. Guest pointer movement and message deadlines MUST deliver one-shot hover/leave notifications. Hover SPI settings MUST remain guest-owned, and window destruction MUST clear tracking.
**Verification**: Test

### WIN32-GDI-14 Paragraph drawing

**Rule**: Top-left `DrawTextA/W` MUST handle CR, LF, CRLF, external leading, and bottom clipping while preserving the input RECT. Single-line, literal ampersand, `DT_NOPREFIX`, and `DT_NOCLIP` behavior MUST share the selected font, guest ACP, and DC clipping.
**Verification**: Test

### WIN32-API-15 Hot query leaf calls

**Rule**: The listed queries MAY use EnvLeaf under A64-CTL-12, preserving results, memory, LastError, input observations and ClockPoll. Clock leaves MUST decline required waits before clock or poll changes, accounting for host-call increments after successful leaves. Buffer checks MUST precede side effects and reject invalid ranges by declining.
**Applies to**: AArch64
**Verification**: Test

Clock: `timeGetTime`, `GetTickCount`, QPC/QPF, `GetLocalTime`, `GetSystemTime(AsFileTime)`; keyboard: `GetKeyState`, `GetAsyncKeyState`, `GetKeyboardState`; character: `IsDBCSLeadByte(Ex)`, `CharNextA`. Additional leaves cover synchronous USER window/focus/cursor/metric queries and host lstr length/copy/locale comparisons. Output queries MUST decline invalid complete ranges before reading clocks or modifying outputs. String leaves MUST retain host caught-fault and locale behavior, with invalid `lstrlenA/W` returning zero.

D3DX9 leaves: `D3DXMatrixMultiply`, `D3DXMatrixTranspose`, `D3DXMatrixScaling`, `D3DXMatrixTranslation`, `D3DXMatrixRotationYawPitchRoll`, `D3DXMatrixRotationX/Y/Z`, `D3DXMatrixPerspectiveFovLH`, `D3DXMatrixLookAtLH`, `D3DXVec2Transform(Coord/Normal)`, `D3DXVec3Transform(Coord)`, `D3DXVec4Transform`, `D3DXVec3CatmullRom`, `D3DXVec3Project`, in d3dx9_29/43. Matrix-stack leaves: `QueryInterface`, `AddRef`, `GetTop`, `LoadIdentity`, `LoadMatrix`, `MultMatrix(Local)`, `RotateAxis(Local)`, `RotateYawPitchRoll(Local)`, `Scale(Local)`, `Translate(Local)`. Math leaves MUST decline failed input/output ranges before writing. `Push`, `Pop` and `Release` retain ordinary mapping-capable handlers.

CRT `_errno` leaves MUST decline before allocating a missing module/thread cell. `RegQueryValueExA/W` leaves MUST decline before loading the registry from disk. Resolved registry queries MUST preserve ordinary error returns and output writes. `GetFileAttributesA/W` leaves MUST decline live NT attribute providers and system-image paths before side effects.

### WIN32-API-16 Guest atomic operations

**Rule**: InterlockedIncrement, InterlockedDecrement, InterlockedExchange, InterlockedExchangeAdd and InterlockedCompareExchange MUST use LOCK-prefixed guest operations with 32-bit wrapping and native old/new return values. They MUST preserve LastError, callee-saved registers and native access-fault delivery, including when diagnostic guest routing is disabled. Critical-section guest entries MUST retain the ordinary contention, ownership and wake paths under PROC-MACH-2/3.
**Verification**: Test

CRT guest entries: `memcpy`, `memmove`, `memset`, `memcpy_s`, `memmove_s`, wherever registered in msvcrt, msvcr80, msvcr90, ucrtbase, vcruntime140 and the CRT string API set. Copy entries MUST preserve overlapping bytes in both directions. Secure-copy argument errors MUST enter the original host handler before destination writes. Guest memory faults MUST use ordinary guest access-violation delivery. Scalar `operator new/delete` in msvcrt/msvcr80/msvcr90 MUST reuse the shared heap listings, with rare cases entering their original CRT handlers.

Withdrawn: WIN32-EXC-1, WIN32-EXC-2, WIN32-EXC-3, WIN32-EXC-4, WIN32-EXC-5, WIN32-EXC-6, WIN32-EXC-7
