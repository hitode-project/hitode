# Win32 personality

ID prefix: WIN32

This spec covers the Win32 state the shared crates still own: image rebasing, heaps, storage, message text and font metrics. `adv-pe`, `adv-os` and `adv-device` implement them.

## Images

### WIN32-IMAGE-1 Atomic rebasing

**Rule**: Rebasing MUST validate the header and entire relocation table before modifying the image. Success MUST publish the actual ImageBase while retaining the parsed preferred base. Without relocations, absolute addresses MUST remain unchanged.
**Verification**: Test

## Shared objects

### WIN32-OBJECT-4 Large heaps

**Rule**: Heap, RTL, CRT, Global, and Local allocation MUST share storage. Large blocks MUST start zeroed without ZERO_MEMORY, retire their mapping on release, and zero an enlarged moving-realloc tail. Moving realloc MUST preserve prior bytes, and blocks up to 16 KiB MUST NOT expose bytes another size freed.
**Verification**: Test

### WIN32-OBJECT-5 Persistent storage

**Rule**: Game writes MUST use a private overlay and registry persistence. Installation-directory writes MUST reach the mapped Program Files folder directly; a launched game's own folder MUST retain overlay semantics.
**Verification**: Test

### WIN32-OBJECT-8 Overlay whiteouts

**Rule**: Deleting read-only game files MUST persist whiteouts, hiding names until rewritten. Non-Windows Win32 file renames over read-only layers MUST preserve originals and live owners, persistently hide old names, and preserve visibility on failure. Directory lookups MUST be answered from an index that only VFS-made changes invalidate, so host changes made outside the runtime during a run stay invisible.
**Verification**: Test

## Windows and input

### WIN32-USER-15 Message box text encoding

**Rule**: MessageBoxA text and caption MUST use the ANSI code page unless that reading is not ordinary text while the other of 932 and 936 is. Translation patches can leave individual strings in the original encoding.
**Verification**: Test

## Drawing and text

### WIN32-GDI-7 Monochrome coverage

**Rule**: Monochrome text MUST preserve coverage as index bits even when both palette entries contain identical RGB. Drawing MUST preserve the original palette.
**Verification**: Test

### WIN32-GDI-10 Selected font metrics

**Rule**: Glyph widths, ABC spacing, Unicode coverage and legacy/Point32 A/W text extents MUST share selected device fonts and validation of counted text. Windows MUST preserve native fallback and bitmap-font outcomes; portable measurements MUST use outline advances, hinted bounds, and primary-face coverage.
**Verification**: Test

### WIN32-GDI-12 Encoded font metrics

**Rule**: ANSI widths and kerning MUST use the selected charset and device-provided default glyph. Wide outline metrics MUST preserve their own native short-buffer rules. Portable wide metrics MUST take first and last characters from the Unicode cmap, and break and default characters from OS/2 usFirstCharIndex.
**Verification**: Test

### WIN32-GDI-11 Character placement

**Rule**: Character placement MUST validate bounded output arrays and preserve partial-result behavior. ANSI placement MUST use the guest ACP and retain explicit-charset conversion. Placement arrays MUST use the same realized fonts as measurement and drawing.
**Verification**: Test

### WIN32-GDI-14 Paragraph drawing

**Rule**: `DrawTextA/W` MUST handle CR, LF, CRLF, external leading, and bottom clipping while preserving the input RECT outside `DT_CALCRECT`. Wrapping, tabs, prefixes, ellipses, and alignment MUST share selected-font advances, the guest ACP, and DC clipping.
**Verification**: Test

### WIN32-GDI-15 Memory fonts

**Rule**: AddFontMemResourceEx MUST register a copy as a process-private, non-enumerable resource, report its face count, and return a distinct handle. RemoveFontMemResourceEx MUST release only that registration. Process cleanup MUST release remaining memory fonts.
**Verification**: Test

Withdrawn: WIN32-IMAGE-5
