# Device services and optical media

ID prefix: DEV

This spec covers the neutral device services behind the API front ends: frames, depth attachments, texture updates, images, audio, fonts and optical media. `adv-device` and `adv-os` implement them.

## Frames

### DEV-FRAME-1 Shared presentation

**Rule**: GDI, DirectDraw, and D3D9 MUST use neutral surfaces and drawing services under the same device interface. The mailbox MUST publish complete frames and let the host select the latest completed frame.
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

### DEV-FRAME-11 Deleted frame sources

**Rule**: Deleting the source of a pending software frame MUST discard that frame.
**Verification**: Test

### DEV-FRAME-9 Software frame cap

**Rule**: With a host frame clock, a boundary frame covering a quarter of the surface may follow the last one within 1/60 s. It MAY then hold the writer's next empty PeekMessage until the vsync. Messages, timers and input MUST end the hold at once, and held keys or mouse buttons MUST prevent it.
**Verification**: Test

Flush reasons are writer wait, message retrieval, age expiry and immediate visibility. Pending age starts at the first write; unrelated waits do not end younger frames. Message boundaries apply even when no message is retrieved.

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

**Rule**: BGRA8 upload and shadow writeback MUST share an immutable snapshot. X8R8G8B8 MUST preserve original X bytes in the shadow while normalizing GPU alpha separately.
**Verification**: Test

### DEV-UPDATE-3 Completed buffer retention

**Rule**: Completed raw and converted upload buffers MUST each retain at most 8 MiB. Larger updates MUST remain usable without retaining their buffers.
**Verification**: Review — existing update tests exercise changing sizes and formats without asserting the completed-buffer retention limit.

### DEV-UPDATE-4 Dirty level expansion

**Rule**: Dirty regions MAY conservatively expand to matching complete mip levels. This expansion MUST NOT introduce GPU waits or readback.
**Verification**: Test

## Image services

### DEV-IMAGE-1 Image decoding

**Rule**: Device image services MUST decode PNG, BMP, and DDS including authored DXT mipmaps, and scale BGRA8 pixels. D3DX front ends MUST retain HRESULT, source-format, and filter-policy semantics. Default memory texture loading MUST publish a real complete mip chain through the shared D3D9 resource owner.
**Verification**: Test

### DEV-IMAGE-2 GDI+ lifetime

**Rule**: GDI+ Graphics MUST retain their images, cloned pixels MUST be independent, and borrowed scan0 changes and drawing MUST synchronize with guest storage. GDI bitmap writeback MUST preserve guest object ownership.
**Verification**: Test

### DEV-IMAGE-3 GDI+ platform boundary

**Rule**: GDI+ native pointers and private DCs MUST NOT enter guest memory. Missing device operations MUST return an explicit not-implemented result identifying the calling flat API.
**Verification**: Review — native pointer isolation and complete capability diagnostics require adapter review.

### DEV-IMAGE-4 GDI+ storage bounds

**Rule**: GDI+ images MUST be limited to 64 MiB each, with at most 256 MiB of independent front-end backing.
**Verification**: Review — no existing test reaches either GDI+ storage admission boundary.

### DEV-IMAGE-5 CPU bitmap rendering

**Rule**: CPU GDI+ devices MUST provide owned bitmap storage, clear, image drawing with retained compositing modes, filled paths and filled primitives through the shared device interface.
**Verification**: Test
**Basis**: [GDI+ Graphics](https://learn.microsoft.com/windows/win32/gdiplus/-gdiplus-class-graphics).

### DEV-IMAGE-6 GDI+ value queries

**Rule**: Image queries MUST report actual dimensions, bounds, pixels and bitmap type. Path and string-format clones MUST retain independent state and reject invalid receiver kinds.
**Verification**: Test

### DEV-IMAGE-7 GDI+ path pixel centers

**Rule**: Aliased paths MUST test pixel centers using their fill rule. Antialiased path coverage MUST honor integer and half-integer pixel-center modes.
**Verification**: Test
**Basis**: [PixelOffsetMode](https://learn.microsoft.com/windows/win32/api/gdiplusenums/ne-gdiplusenums-pixeloffsetmode).

### DEV-IMAGE-8 GDI+ affine matrices

**Rule**: Matrices MUST retain IEEE nonfinite coefficients and compose row-vector affine transforms in the requested prepend or append order. Failed inversion and overflowing point transformations MUST preserve input state.
**Verification**: Test
**Basis**: [MatrixOrder](https://learn.microsoft.com/windows/win32/api/gdiplusenums/ne-gdiplusenums-matrixorder).

### DEV-IMAGE-9 GDI+ solid brush state

**Rule**: Solid brush color changes MUST affect subsequent drawing. Cloned brushes MUST retain independent colors, and color access MUST reject non-solid receivers.
**Verification**: Test

### DEV-IMAGE-10 GDI+ world transforms

**Rule**: Graphics world matrices MUST transform path and image drawing. Setting and querying world matrices MUST copy their values without sharing subsequent mutations. Singular world transforms MUST fail without changing graphics state.
**Verification**: Test

### DEV-IMAGE-11 GDI+ string layout settings

**Rule**: String formats MUST preserve independent digit substitution, alignment, trimming, hotkey, tab and measurable-range settings when cloned. Generic formats MUST report native default and typographic settings. Setting more than 32 character ranges MUST return ValueOverflow without changing the previous ranges.
**Verification**: Test
**Basis**: [StringFormat](https://learn.microsoft.com/windows/win32/api/gdiplusstringformat/nl-gdiplusstringformat-stringformat).

### DEV-IMAGE-12 Image list storage

**Rule**: Image list duplication MUST copy independent pixels, alpha modes, masks, dimensions and background color. Image removal MUST compact later indexes, and icon size changes MUST remove every image. Count expansion MUST preserve existing images, and truncation MUST retire removed storage.
**Verification**: Test
**Basis**: [ImageList_Duplicate](https://learn.microsoft.com/windows/win32/api/commctrl/nf-commctrl-imagelist_duplicate), [ImageList_SetIconSize](https://learn.microsoft.com/windows/win32/api/commctrl/nf-commctrl-imagelist_seticonsize).

### DEV-IMAGE-13 GDI+ path geometry

**Rule**: Path arrays MUST preserve cubic control points, figure boundaries, closure flags and markers through cloning and affine transformations. Cardinal curves MUST preserve tension, neighboring-point tangents and closed joins in cubic control points that affect shared raster output. Zero-area rectangles MUST add no points, while zero-area ellipses MUST retain thirteen cubic points.
**Verification**: Test
**Basis**: [GraphicsPath](https://learn.microsoft.com/windows/win32/api/gdipluspath/nl-gdipluspath-graphicspath).

### DEV-IMAGE-14 Image list bitmap inputs

**Rule**: Bitmap strips MUST append independent RGBA and mask snapshots through shared GDI bitmap storage and palettes. Replacement MUST preserve list dimensions and all other images. Color-key insertion MUST derive transparency from the specified or upper-left palette color without consuming source handles.
**Verification**: Test
**Basis**: Microsoft ImageList_Add, ImageList_AddMasked and ImageList_Replace contracts.

### DEV-IMAGE-15 Icon and image list raster output

**Rule**: Icon and image list drawing MUST apply packed AND/XOR masks or premultiplied source-over through shared surfaces, mappings and clipping. Icon extraction MUST preserve independent pixels and monochrome double-height mask layout. Invalid destination storage MUST preserve pixels and damage versions.
**Verification**: Test
**Basis**: [DrawIconEx](https://learn.microsoft.com/windows/win32/api/winuser/nf-winuser-drawiconex), [ICONINFO](https://learn.microsoft.com/windows/win32/api/winuser/ns-winuser-iconinfo), Microsoft ImageList_Draw and ImageList_GetIcon contracts.

### DEV-IMAGE-16 GDI+ compositing and graphics properties

**Rule**: Graphics compositing MUST select source-over blending or source-copy replacement without changing pixels outside primitive coverage. Smoothing getters MUST report normalized aliases, while text hint getters MUST report the retained device setting. Compositing MUST quantize premultiplied color and alpha channels before conversion to straight-alpha image pixels.
**Verification**: Test
**Basis**: Microsoft CompositingMode and SmoothingMode contracts and native-backed System.Drawing GraphicsTests pixel expectations.

## Audio ownership

### DEV-AUDIO-1 Shared playback

**Rule**: DirectSound and waveOut MUST share mixing and playback time. Stable PCM views, generations, and retirement acknowledgments MUST prevent reads from retired or reused storage.
**Verification**: Test

### DEV-AUDIO-2 Control delivery

**Rule**: Audio command queues MUST be bounded without losing stop or release requests. Notifications MUST return through machine-owned events.
**Verification**: Test

### DEV-AUDIO-3 Vorbis callbacks

**Rule**: Host Vorbis implementations MUST read packets through the guest callback contract and preserve seek, lifetime, and PCM semantics.
**Verification**: Test

### DEV-AUDIO-4 Codec admission

**Rule**: Static codec kernels MUST require matching signatures, calling conventions, and bounded inputs. Native acceleration MUST preserve library-level numerical tolerances and MUST NOT authorize arbitrary function replacement.
**Verification**: Test

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

**Rule**: Font services MUST preserve guest-font priority and code-page rules unless the host explicitly selects a replacement family. Host replacements MUST apply consistently to metrics and drawing, and changes MUST invalidate cached fonts and glyphs. Caching MUST remain below observable API entry points.
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

Withdrawn: none
