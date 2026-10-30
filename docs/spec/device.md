# Device services and optical media

ID prefix: DEV

This spec covers the neutral device services behind the API front ends: frames, depth attachments, texture updates, icon raster output, audio, fonts and optical media. `adv-device` and `adv-os` implement them.

## Frames

### DEV-FRAME-1 Shared presentation

**Rule**: DirectDraw and D3D9 MUST use neutral surfaces and drawing services under the same device interface. The mailbox MUST publish complete frames and let the host select the latest completed frame.
**Verification**: Test

### DEV-FRAME-2 Observable work

**Rule**: Discarding a host presentation frame MUST NOT skip guest-visible drawing, locking, or readback.
**Verification**: Test

### DEV-FRAME-3 Exact software damage

**Rule**: Software presentation MUST take clipped destination damage. Same-encoding SRCCOPY MUST compare and copy only changed rows, bounding their damage, while other paths MAY damage the clipped destination. Empty damage MUST update its counter without adding pending damage or producing a frame of its own.
**Verification**: Test

### DEV-FRAME-4 Incremental complete slots

**Rule**: Each mailbox slot MUST retain its last stored serial and complete normalized pixels. The store MUST retain at most sixteen frame-serial/damage entries and copy their union since that slot's serial. Missing history or changed source/layout MUST rebuild the full frame.
**Verification**: Test

### DEV-FRAME-5 Consumer damage accumulation

**Rule**: Each consumer MUST accumulate history since its last converted serial, preserve undamaged BGRA pixels, and fully convert when history is missing. Damage MUST contain at most four rectangles, merging overflow into a bounding box. Bottom-up storage and indexed palettes MUST preserve exact pixels.
**Verification**: Test

### DEV-FRAME-6 Full-frame transitions

**Rule**: Writable DirectDraw Unlock MUST damage its locked rectangle, while read-only Unlock MUST NOT publish. Unrestricted locks, palette changes, storage flips, and size/format changes MUST damage the whole surface. Video composition and removal MUST rebuild the full base before unchanged composition.
**Verification**: Test

### DEV-FRAME-7 Software reference comparison

**Rule**: `ADV_VERIFY_SOFTWARE_FRAMES` MUST compare incremental software storage and conversion with full-frame references without changing displayed pixels.
**Verification**: Test

### DEV-FRAME-8 Software frame boundaries

**Rule**: With a mailbox, software presents MUST accumulate four-rectangle damage, publishing one serial/history entry per flush. Writer waits/message retrieval MUST flush, and any wait/present MUST flush after 50 ms. Readback, capture, layout/palette/video changes, flips and exit MUST flush first, with immediate headless presents and reason-specific counters.
**Verification**: Test

### DEV-FRAME-12 Unchanged software redraws

**Rule**: Software redraws matching the last publication MUST NOT publish, copy snapshot storage, or wake presentation. This comparison MUST include damage smaller than a content-diff tile.
**Verification**: Test

### DEV-FRAME-11 Deleted frame sources

**Rule**: Deleting the source of a pending software frame MUST discard that frame.
**Verification**: Test

### DEV-FRAME-10 Content-diff publication damage

**Rule**: A published software frame MUST shrink declared damage to 64x16 tiles whose pixels differ from the previous publication. A rectangle with more than three quarters changed MUST keep its declared bounds. The frame cap MUST measure declared damage.
**Verification**: Test

Comparing against the last publication keeps every slot's damage history complete, so incremental slots and consumers stay exact. Packed formats keep their declared damage.

## Depth attachments

### DEV-DEPTH-1 Independent depth

**Rule**: Depth/stencil surfaces MAY exceed color dimensions and MUST retain identity and contents across color-target switches. Drawing and clearing MUST preserve depth/stencil outside the color region.
**Verification**: Test

### DEV-DEPTH-2 Attachment copies

**Rule**: Padded color attachments MUST copy the original mip region in and out within one submission. Invalid smaller or foreign-context depth attachments MUST fail before encoding.
**Verification**: Test

### DEV-DEPTH-3 Scratch bound

**Rule**: Each command producer MUST retain at most one color attachment scratch image, bounded to 64 MiB.
**Verification**: Review — attachment tests check reuse, but do not attempt the 64 MiB admission boundary.

## Texture updates

### DEV-UPDATE-1 Transactional texture updates

**Rule**: UpdateTexture MUST validate and snapshot matching mip levels before submission. Busy or failed updates MUST retain reservations and restore dirty marks for retry. Completion MUST update guest shadows.
**Verification**: Test

### DEV-UPDATE-2 Snapshot semantics

**Rule**: BGRA8 and A8 upload and shadow writeback MUST share an immutable snapshot; A8 texels MUST expand to black BGRA on the GPU. X8R8G8B8 MUST preserve original X bytes in the shadow while normalizing GPU alpha separately.
**Verification**: Test

### DEV-UPDATE-3 Completed buffer retention

**Rule**: Completed raw and converted upload buffers MUST each retain at most 8 MiB. Larger updates MUST remain usable without retaining their buffers.
**Verification**: Review — existing update tests exercise changing sizes and formats without asserting the completed-buffer retention limit.

### DEV-UPDATE-4 Dirty rectangle updates

**Rule**: Each matched mip level MUST upload and write back only its dirty rectangle, preserving texels outside it; clean levels MUST be skipped. Updates MUST NOT introduce GPU waits or readback.
**Verification**: Test

## Image services

### DEV-IMAGE-15 Icon and image list raster output

**Rule**: Icon and image list drawing MUST apply packed AND/XOR masks or premultiplied source-over through shared surfaces, mappings and clipping. Icon extraction MUST preserve independent pixels and monochrome double-height mask layout. Invalid destination storage MUST preserve pixels and damage versions.
**Verification**: Test
**Basis**: [DrawIconEx](https://learn.microsoft.com/windows/win32/api/winuser/nf-winuser-drawiconex), [ICONINFO](https://learn.microsoft.com/windows/win32/api/winuser/ns-winuser-iconinfo), Microsoft ImageList_Draw and ImageList_GetIcon contracts.

## Audio ownership

### DEV-AUDIO-1 Shared playback

**Rule**: DirectSound and waveOut MUST share mixing and playback time. Stable PCM views, generations, and retirement acknowledgments MUST prevent reads from retired or reused storage.
**Verification**: Test

### DEV-AUDIO-2 Control delivery

**Rule**: Audio command queues MUST be bounded without losing stop or release requests. Notifications MUST return through machine-owned events.
Device reconnection MUST retain refill notifications crossed while reanchoring a playing PCM ring.
**Verification**: Test

### DEV-AUDIO-3 Vorbis callbacks

**Rule**: Host Vorbis implementations MUST read packets through the guest callback contract and preserve seek, lifetime, and PCM semantics.
**Verification**: Test

### DEV-AUDIO-4 Codec admission

**Rule**: Static codec kernels MUST require matching signatures, calling conventions, and bounded inputs. Native acceleration MUST preserve library-level numerical tolerances and MUST NOT authorize arbitrary function replacement.
**Verification**: Test

### DEV-CODEC-1 Integer byte kernels

**Rule**: PNG row and LZ77 kernels MUST implement the byte semantics below without guest state, allocation, callbacks or I/O. Invalid sizes or distances MUST be rejected before writing.
**Verification**: Test

| Computation | Byte semantics |
|---|---|
| PNG Sub/Up/Average/Paeth | In-place modulo-256 output; zero left and upper-left before the first pixel |
| PNG Average | Widen the sum before division |
| PNG Paeth | Break ties in left, above, upper-left order |
| LZ77 | Ordered reads include newly written history for overlapping matches |

### DEV-VIDEO-1 Native packet decoding

**Rule**: Whole-packet Theora replacements MAY own opaque decoder state after complete library-family admission.
They MUST preserve output pixels, return values, counters, seek controls, lifecycle and callback order.
Guest stream I/O and presentation clocks MUST remain guest-controlled.
Recognition MUST validate complete function/table fingerprints and relocation relationships, independent of game addresses.
Callback continuations MUST publish completed rows before returning control to guest code.
Workers MAY process disjoint native planes. They MUST finish before guest observation and MUST NOT access guest CPU state or memory.
Architectural state MUST follow the admitted library ABI, without instruction-level execution accounting inside native calls.
Changed library code MUST NOT execute against stale native or guest decoder state.
Unmatched libraries MUST execute their guest implementations.
**Verification**: Test

### DEV-AUDIO-5 Owned PCM timing

**Rule**: Owned PCM clock conversion MUST use guest and device readings from the same synchronization point. Decode duration MUST NOT shift a continuous audio stream's presentation timestamps. Queue-depth metrics MUST measure total queued duration per stream.
**Verification**: Test

## Fonts

### DEV-FONT-1 Font selection

**Rule**: Font services MUST preserve guest-font priority and code-page rules unless the host explicitly selects a replacement family. Host replacements MUST apply consistently to metrics and drawing, with caches kept below observable API entry points and dropped on any change. Cubic outlines requested as quadratic MUST use uniform midpoint quadratics within 1/16 pixel, using at most 16 pieces per cubic.
**Verification**: Test

### DEV-FONT-2 Font resource budgets

**Rule**: Guest font resources MUST remain within 64 MiB. System and controlled font files MUST individually remain within 128 MiB, with 128 MiB aggregate budgets for their respective stores.
**Verification**: Review — resource tests cover guest admission failure but do not test both system and controlled-font 128 MiB boundaries.

### DEV-FONT-3 Legacy family names

**Rule**: Portable matching and enumeration MUST accept a face's legacy family name in every language it provides, as GDI does. Enumeration MUST report that name in the guest UI language when present, otherwise in English. Faces that cannot be measured MUST be left out rather than failing the enumeration.
**Verification**: Test

### DEV-FONT-4 Missing family fallback

**Rule**: Portable enumeration of a named family that no font provides MUST report the face drawing would substitute, under the requested name. Hosts MUST record a diagnostic when this happens. A full enumeration MUST likewise report each standard Windows family the host lacks, named in the guest UI language when Windows localizes it.
**Verification**: Test

### DEV-FONT-5 Full family enumeration

**Rule**: Enumerating without a family name MUST report each family once, preferring its regular style, and once per character set for extended enumeration. Host-private families, whose names start with a period, and unnamed faces MUST be left out.
**Verification**: Test

## Optical media

### DEV-DISC-1 Mount configuration

**Rule**: Startup configuration MAY mount multiple ISO images on distinct drive letters. Mounts MUST NOT replace the system drive or the game-root drive.
**Verification**: Test

### DEV-DISC-2 Installation and authentication

**Rule**: Mounting MUST NOT install the game, change host registry state, or manufacture authentication results. Guest authentication MUST use actual medium contents, and host image paths MUST NOT become guest paths.
**Verification**: Review — these isolation and authorization boundaries require inspecting all mount-to-guest integration paths.

### DEV-DISC-3 Inherited lifetime

**Rule**: Children MUST inherit the same read-only medium owner. Open files and mappings MUST retain the medium after the parent exits.
**Verification**: Test

### DEV-DISC-4 Supported image data

**Rule**: Optical media MUST support ISO9660 and Joliet with 2048-byte logical sectors, including noncontiguous multi-extent files. Reads MUST honor file bounds and independent cursors.
**Verification**: Test

### DEV-DISC-5 Bounded image access

**Rule**: Optical access MUST reuse the shared ISO parser without unpacking images or loading an entire image into memory. Section mappings MUST read pages on demand.
**Verification**: Test

### DEV-DISC-6 Volume identity

**Rule**: A/W drive, DOS-device, volume, and capacity queries MUST use shared medium metadata. CDFS serials MUST use the selected descriptor's byte-column checksum, and volume flags and name limits MUST follow MS-FSA CDFS definitions.
**Verification**: Test

### DEV-DISC-7 Read-only files

**Rule**: Optical reads, seeks, enumeration, attributes, copying, INI access, and mapping MUST reuse VFS and section ownership. Writes and medium modifications MUST fail without entering the game overlay.
**Verification**: Test

### DEV-DISC-8 Unsupported media operations

**Rule**: UDF, BIN/CUE, multisession audio, raw device IOCTLs, CD audio, and physical-medium information MUST NOT be inferred from ISO data support. Runtime eject and media-change notifications MUST remain unavailable.
**Verification**: Review — no test enumerates all unsupported formats and device operations or runtime media-change requests.

Withdrawn: DEV-CODEC-2
