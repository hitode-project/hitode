# DirectDraw and programmable graphics

ID prefix: GFX

This spec covers shared CPU compositing and the DirectDraw and Direct3D 9 API front ends. It includes capabilities, shaders, vertex input, samplers, buffers, caches and numerical behavior. `adv-dx` and the GPU renderer in `adv-device` implement them.

## DirectDraw

### GFX-DDRAW-1 Lock and clipper state

**Rule**: WRITEONLY Lock and Unlock MUST retain shared busy-state and write-version semantics. Primary Blt MUST refresh a window clipper's geometry after window movement without requiring reattachment.
**Verification**: Test

### GFX-DDRAW-2 Device identity

**Rule**: COM activation and DirectDraw exports MUST share the device owner. Activated devices MUST require Initialize and describe only the virtual adapter.
**Verification**: Test

### GFX-DDRAW-3 Focus window

**Rule**: SETFOCUSWINDOW MUST retain the device's focus HWND.
**Verification**: Review — no dedicated test asserts the stored focus-window identity.

### GFX-DDRAW-4 Source color keys

**Rule**: SetColorKey MUST store one inclusive raw range per surface and key kind. Blt and BltFast with source keys MUST skip source pixels in range, comparing only color bits of 32-bit pixels.
**Verification**: Test

## Native programmable execution

### GFX-METAL-1 Apple programmable draws

**Rule**: Apple ARM programmable draws MUST use native Metal on the wgpu command queue, preserving the reference pixels exactly. Backend switches MUST submit preceding work before dependent operations, while initialization of allocations absent from the batch MAY precede it at its submission boundary. `ADV_GPU_BACKEND=wgpu` MUST select the reference until R3-B removes the migration switch.
**Verification**: Test

| Concern | Contract |
|---|---|
| Resources | wgpu owns or wraps textures; display targets can use IOSurface storage. Software publications use no-copy Metal uploads. Bounded clears use native drawing and its shared prebuild/persistent pipeline cache. Texture and padded-attachment copies use native blits while Metal is active; first-use wgpu initialization for allocations absent from the batch is queued before it at the existing submission boundary. Texture uploads, fixed drawing and readback retain wgpu paths. |
| Uploads | A shared 64 MiB ring holds vertices, indices and constants; ordered completion flags retire intervals, and insufficient space returns Busy before encoding or uploading. |
| Shaders | Naga uses wgpu-hal 30 Metal options; MSL is cached by shader content, texture visibility, vertex layout and point topology variant. Pipeline states retain the existing 64-bit key and capacity limit. Native recipes use the shared background prebuild, demand result and persistent cache (GFX-CACHE-8, GFX-CACHE-10). |
| Batches | Matching attachments reuse the encoder; adjacent identical constants reuse their ring range. |
| Observations | GPU reports expose Metal draws, backend switches partitioned into partial clears, copies, uploads, fixed draws, readbacks and other transitions (including entry to Metal), ring bytes written and ring Busy returns. |

Acquiring a submission lease MUST NOT flush queued drawing or change backends. Native frames MUST retain their producer's completion and let direct presentation subscribe to it without an extra queue submission. Actual reference-backend work MUST preserve queue order and invalidate stale native completion tokens.

## GPU render passes

### GFX-PASS-1 Deferred full clears

**Rule**: Full-target color clears MUST become load-action clears in the next compatible draw pass. Partial and padded-target clears MUST preserve bounded drawing semantics.
Full-target clips MUST preserve full-clear load actions for equal-sized attachments.
Pending clears MUST resolve before target changes, copies, readbacks, uploads, presentation, frame completion, or external queue submissions.
Retained resources MUST keep pending clears valid after callers release their target handles. Backend changes MUST preserve command order.
**Verification**: Test

### GFX-PASS-2 Optional depth and stencil

**Rule**: Draw passes MUST omit unused depth/stencil attachments unless clearing them or retaining an already attached depth/stencil image.
Fragment-depth outputs MUST retain bound depth attachments.
A draw requiring depth/stencil MUST reopen a color-only pass without adding a submission. Attached contents MUST retain Store actions across frames.
Metal and wgpu MUST share these rules and report passes, color loads, depth loads, and clears merged into draw passes.
**Verification**: Test

### GFX-PASS-3 Deferred depth and stencil clears

**Rule**: Depth and stencil clears MUST wait on their depth image until a pass attaches it. Whole-image clears MUST become that pass's load actions; partial clears MUST execute as bounded draws before the first draw that tests or writes depth or stencil.
A clear that cannot absorb a waiting one MUST execute the waiting one first. Deferring a clear MUST end a pass that already attaches the image. Depth images MUST NOT be observable except through attachment.
Queued clear regions MUST retain their admission-time clip; subsequent drawing clips MUST NOT narrow them when they are materialized.
Clipped clears MUST retain depth/stencil deferral independently of their color writes. Repeated equal clears inside a pending region MUST NOT materialize an otherwise unused attachment.
**Verification**: Test

## D3D9 frame recording

### GFX-REC-1 Ordered recording and flush boundaries

**Rule**: D3D9 MUST validate new recorded commands synchronously and flush before Present, readback, GPU waits, Reset, release, or recording limits. Busy continuations MUST resume the unexecuted suffix without replaying successful commands. Execution-time device failure MUST persist until Reset and be reported by Present or TestCooperativeLevel.
**Verification**: Test

Recorded programmable draws capture guest streams and neutral geometry once, before the guest can mutate or release their sources. Immutable vertices and register snapshots are shared with bounds and sampling analysis. Backend byte packing and point expansion occur only when surviving commands execute. Binding dependencies are cached until binding changes or resource destruction. Content writes resolve a slot once and index its current/base versions directly; retired slots can be reused.

Flush limits are 4096 commands or 64 MiB of upload and vertex data. Readback boundaries include GetRenderTargetData, GetFrontBufferData, GPU LockRect and GetDC; unsupported capture retains its ABI failure.

### GFX-REC-2 Complete content identities and repeated frames

**Rule**: Content identities MUST include every output-affecting command input, exact float bits, and sampled subresource versions. Elision MUST require unchanged predicted contents for every written resource, unchanged presentation parameters, and no intervening presentation from another front. Elided frames MUST preserve guest completion counters, return values, and pacing while issuing no GPU commands or publications.
**Verification**: Test

Full clears replace previous dependencies; partial clears, draws and copies retain them. Upload admission assigns a fresh identity. A partial flush or a private presentation without the shared output prevents elision of that frame. DISCARD presentation may rotate independently reconstructible targets; destination-dependent frames preserve writable contents using an immutable GPU snapshot.
Clear identities MUST include only the selected value and extent of each written color, depth or stencil component. Draws MUST read only the enabled depth/stencil tests.
Fixed-state draw keys MAY be cached between mutations, but every mutable state access MUST invalidate the cached key. Fixed-state categories retain shared immutable maps and complete content keys. Identical map insertions preserve that category's key; arbitrary mutable access invalidates it.
Fixed-state category reads MUST use dense SDK selector tables, and state-block membership and copying MUST use bounded slot bitmaps without per-member heap allocation.
Fixed-input draws MAY capture each owned vertex stream once per draw when the span reads no more bytes than separate vertex reads. Complete input identities MUST retain index order and exact vertex bytes, including repeated indices; later draws MUST observe buffer writes.
Owned input captures with identical complete inputs and decoding state MAY reuse immutable decoded geometry across adjacent frames. Each frame generation MUST retain at most 4096 entries and 64 MiB, and cached geometry MUST NOT retain GPU resources. Fixed programmable streams MAY reuse this complete capture identity instead of hashing expanded bytes again; arbitrary byte mutation and point expansion MUST discard that identity.

### GFX-REC-3 Asynchronous pixel verification

**Rule**: `ADV_VERIFY_FRAMES=1` MUST execute full frames and asynchronously compare unchanged pixels with the previous readback. Mismatches MUST warn and increment Diagnostics. Diagnostics MUST expose command counts, flush reasons, eliminated frames, verification mismatches, full/clipped executions, and accumulated redraw fractions.
**Verification**: Test

### GFX-REC-4 Conservative damage and bounds

**Rule**: Each target subresource MUST accumulate recording-time differences, including sampled content and retained destination dependencies. Changed, added, or removed entries MUST damage both old and new bounds. Damage MUST retain at most four rectangles and conservatively merge overflow without promoting bounded damage to the whole target based on coverage alone.
**Verification**: Test

Overflow merges into one bounding box. Bounds use decoded XYZRHW or CPU world/view/projection positions, or the scalar vertex shader interpreter for at most 64 float-input vertices. Perspective division and viewport mapping precede one-pixel raster margins, point-size expansion, and viewport/guest-scissor intersection. Nonpositive w, nonfinite values, excessive vertices, unsupported inputs or instructions use full-target bounds. Clear and copy use destination rectangles; CPU uploads use the whole target. Size/format changes and Reset invalidate history. Reading an intermediate target before a later write, or sharing an attachment across target allocations, conservatively forces full execution.
Clears and copies MUST compare their written region independently of its old destination values, including partial clears of padded depth/stencil attachments. Attachment histories MUST use their own extent; execution clips MUST remain inside the logical color view. Depth/stencil changes MUST contribute to required execution and tested draws, but MUST NOT directly enlarge published color damage.

Intermediate color damage MUST propagate through proven axis-aligned copies and texture sampling, including scale, reversal and filter footprints; histories MUST resolve producer changes before consumers and accumulate allocation age separately. Local sampling proofs require constant positive homogeneous w and two coordinate planes per axis. Sampling uses clamped addressing and the base level without mip filtering. Fragment coordinates MUST preserve direct varyings, optionally moved through registers. Dependent or projected reads, derivatives, control flow, centroid inputs and unsupported geometry MUST retain full consuming-draw bounds.

### GFX-REC-5 Clipped execution and buffer age

**Rule**: Present MUST replay the complete recording once, loading attachments and intersecting writes with the redraw bounding box. Each allocation MUST accumulate damage since its last execution, using full redraw when older than the eight-frame history. Clip state MUST clear after execution, including Busy and failure paths.
**Verification**: Test

Unknown allocations redraw fully; nonrotating targets have age one. Clear and copy respect the clip. Early flushes conservatively invalidate damage history. DISCARD rotation requires an initial full overwrite so replay reconstructs stale slots exactly. Accumulating frames restore the last logical backbuffer when necessary and keep the snapshot path. Early reads restore that baseline too. Diagnostic verification executes full frames while still computing damage and reading the actual presented allocation. No presentation cadence or audio behavior changes.
Immutable snapshot presentation MUST preserve known color damage, just as rotating presentation does. Draws whose conservative bounds do not intersect the required execution region MUST be omitted before backend submission.

### GFX-REC-6 D3D9 unchanged calls

**Rule**: Binding setters and state-block Capture and Apply that change nothing MUST return D3D_OK before validation, reference transfers or command recording. Calls MUST take their full handler while recording, for invalid objects, and when SetTexture would restore managed residency or flush dirty levels.
**Verification**: Test

| Methods | Unchanged when |
|---|---|
| SetTexture, SetStreamSource, SetIndices, SetVertexShader, SetPixelShader, SetVertexDeclaration, SetFVF | The incoming object identity, and any offset, stride or FVF, equal the bound state. |
| StateBlock Capture, Apply | Every captured value equals the device state. |

### GFX-REC-7 Shader constant ranges and content hashing

**Rule**: Translation MUST cache external register usage, excluding shader-local definitions. Fingerprints, copies and uploads MUST process only those ranges while preserving exact bits and uniform layout. Recording MUST retain complete xxh3-128 identities, and both backends MUST reserve aligned complete binding windows before writing.
**Verification**: Test

Usage includes implicit matrix rows, int/bool operands and legacy bump parameters. Relative float addressing covers all 256 float registers except local definitions. Constants, vertices, indices and pixels use 128-bit content keys. Insufficient storage wraps before the complete binding window exceeds the buffer. Packed wgpu uploads retain one queue write per buffer per batch. Immutable snapshots retain only used registers, reuse unchanged captures across writes to unused registers and serve CPU damage interpretation directly. Complete constant blocks are materialized only on demand; CPU interpretation agrees with complete constants.

### GFX-REC-8 Frontend work counters

**Rule**: Diagnostic counter builds MUST report retained bytes and lookups per draw, entries per successful StateBlock Apply, and dead frame/pass eliminations. Ordinary builds MUST compile out work instrumentation.
**Verification**: Test

The diagnostic Cargo feature is `adv-dx/d3d9-counters`. Retained payloads are counted per command, including shared snapshots; cumulative recorded bytes do not measure resident memory or memory traffic. Recording limits continue to use the logical upload size.

### GFX-REC-9 Dead target writes

**Rule**: Only operations whose every output is fully overwritten before any read MAY be eliminated, and a clear MUST drop exactly the components so overwritten. Partial flushes, uploads, partial overwrites, and preserved attachment aspects MUST retain their dependencies. Verification mode MUST execute the complete reference command stream.
**Verification**: Test

A pass is a contiguous group of writes to one target allocation. Backward
liveness tracks logical resource levels and attachment aspects. Dead frames use
GFX-REC-2; these and dead writes are the only whole-frame/operation omissions.
Damage clipping remains a conservative region optimization under GFX-REC-4/5.

### GFX-REC-10 Detached render-target readback

**Rule**: GetRenderTargetData into a plain system-memory surface MUST queue its copy in submission order and return before completion. LockRect, GetDC, surface export and another readback into that surface MUST wait for the copy first. Texture-level destinations and a busy readback lane MUST keep the blocking path.
**Verification**: Test

### GFX-REC-11 Differential frame recording

**Rule**: State writes MUST maintain history-independent 128-bit identities, preserving state-block membership and exact values. Eligible unchanged draws MUST reuse shared previous-frame entries, checking guest-writable bytes and realigning after insertions or removals. Reuse MUST preserve commands, conservative damage, and pixels, and unavailable histories and flush boundaries MUST retain full preparation.
**Verification**: Test

### GFX-PACE-1 Vsync-locked refresh boundaries

**Rule**: Present boundaries MUST fall on host vsyncs, and a late Present MUST wait for the next one without repaying missed frames. At display rates within 1% of a whole multiple of the guest rate, on-time boundaries MUST be exactly that many vsyncs apart. Other rates MUST snap the guest's rational cadence to the nearest vsync after now.
**Verification**: Test

## Capabilities

### GFX-CAP-1 Honest capabilities

**Rule**: Caps and creation results MUST describe executable guest capabilities. Unsupported shader or declaration combinations MUST fail explicitly rather than succeed without drawing.
**Verification**: Test

### GFX-CAP-2 Shader versions

**Rule**: SM2 MUST be reported by default. The SM3 switch MUST stay independent and default off. Software vertex-processing devices MUST report at most VS2 while the VS3 large constant space is unsupported.
**Verification**: Test

### GFX-CAP-3 Excluded resources

**Rule**: Shader Model 4+, D3D10/11, geometry and tessellation shaders, N-patches, and shader ProcessVertices MUST remain unsupported. Cube textures, volume textures, extra MRTs, and software-VP large constants MUST NOT be advertised as implemented.
**Verification**: Test

## Shaders and state

### GFX-SHADER-1 Bounded bytecode

**Rule**: SM1–3 parsing MUST validate version-specific tokens, register bounds, comments, modifiers, relative addressing, predicates, and control flow without panicking on malformed input.
**Verification**: Test

### GFX-SHADER-2 Creation translation

**Rule**: Shader creation MUST resolve translation before drawing, including reuse of successful cached results. An untranslatable object MUST retain an explicit draw failure instead of producing a successful no-op.
**Verification**: Review — parser and binding tests do not cover creating an accepted but untranslatable object and then drawing it.

### GFX-SHADER-3 Objects and constants

**Rule**: Shader binding MUST validate stage and device ownership while retaining bound objects independently of public references. GetFunction and constant access MUST validate complete ranges and preserve stored bits, including BOOL words.
**Verification**: Test

### GFX-SHADER-4 State blocks and Reset

**Rule**: State blocks MUST preserve recorded stage membership without changing live state during recording. Reset MUST clear bindings, constants, and recording while preserving independently referenced shader objects.
**Verification**: Test

Capture and Apply track versions per state category and inspect values only in
changed categories. They copy only differing captured entries, preserving exact
float bits, uncaptured state and COM ownership. Capturing a complete state category shares its immutable map and content key. An unchanged Capture reuses its
snapshot storage; an unchanged Apply performs no state writes. Redundant fixed-state and constant setters preserve category revisions while recording still captures their membership. Matching Capture/Apply bypass binding preparation and reference transfers; changes outside the captured subset refresh its observed revisions. Applying only shader or buffer changes MUST preserve the fixed-state key cache.

## Vertex and raster input

### GFX-INPUT-1 Declarations

**Rule**: Programmable input MUST fetch all 17 supported D3DDECLTYPE formats from raw multistream bytes with D3D component defaults.
**Verification**: Test

### GFX-INPUT-2 Mixed pipelines

**Rule**: VS-only and PS-only draws MUST preserve the fixed stage's supported inputs and outputs. Programmable rasterization MUST apply D3D9 half-pixel, depth, fog, point-size, and clip-plane conventions.
**Verification**: Review — native shader and raster scripts cover these combinations, but no annotated test covers the full mixed-stage raster contract.

### GFX-INPUT-3 Pretransformed vertices

**Rule**: XYZRHW and POSITIONT draws MUST bypass the bound vertex shader while preserving its binding identity and continuing pixel shading. POSITIONT declarations MUST use offset-ordered stream 0 inputs. Programmable pretransformed draws MUST capture stream bytes and indices and map positions in the vertex stage, interpolating only the varyings the pixel stage reads.
**Verification**: Test

### GFX-INPUT-4 Fixed texture coordinates

**Rule**: FVF and equivalent declarations MUST preserve eight FLOAT1–4 texture-coordinate sets and D3D default components through mixed pixel shading.
**Verification**: Test

### GFX-INPUT-5 Fixed raster limits

**Rule**: Fixed drawing MUST reject active fog, user clipping, or unsupported point-size effects instead of silently ignoring them.
**Verification**: Test

### GFX-INPUT-6 Expanded upload bound

**Rule**: Expanded fixed-function vertex uploads MUST remain within 16 MiB.
**Verification**: Review — no test reaches the upload limit.

## Samplers

### GFX-SAMPLER-1 Addressing

**Rule**: Every sampler slot MUST preserve U/V/W address defaults, queries, and state-block values. Two-dimensional sampling MUST NOT consume W coordinates.
**Verification**: Test

### GFX-SAMPLER-2 Neutral values

**Rule**: Sampler state MUST preserve zero LOD bias including negative zero, anisotropy 1, and zero sRGB, element-index, and displacement-offset values. Setters, queries, and state blocks MUST agree, while unsupported nondefault effects MUST fail explicitly.
**Verification**: Test

### GFX-SAMPLER-3 Locked textures

**Rule**: Draws MUST sample a CPU-locked texture's last unlocked GPU contents instead of failing. DC leases and pending texture transfers MUST still reject sampling.
**Verification**: Test

### GFX-SAMPLER-4 Staged uploads

**Rule**: Unlocked 32-bit BGRA pixels MUST be copied from guest memory once, straight into GPU staging memory, and reach the texture by a GPU copy. Staging buffers MUST return to a bounded per-device pool after the GPU has read them.
**Verification**: Test

## Bounded buffers

### GFX-BUFFER-1 Constant layout

**Rule**: Each stage's uniform block MUST contain 256 float4, 16 int4, 16 Boolean-vector slots, and bump state within 4864 bytes. Draw parameters MUST occupy at most 640 bytes.
**Verification**: Review — the fixed layouts match these sizes, but no size assertion or test pins both byte limits.

### GFX-BUFFER-2 Upload bounds

**Rule**: Uniforms MUST use 256-byte alignment in a shared fixed/programmable batch of at most 32 draws. Batches MUST flush before storage reuse, external writes, readback, frame publication, producer changes, and oversized draws, which MUST submit immediately. Buffers MUST receive at most one queue write per batch, and adjacent identical constants MUST reuse their offset.
**Verification**: Test

### GFX-BUFFER-3 Version limits

**Rule**: Pixel shader register access MUST obey its shader version even when the neutral constant block has greater capacity.
**Verification**: Test

## Caches

### GFX-CACHE-1 Pipeline variants

**Rule**: Pipeline keys MUST include immutable program identity, texture visibility, vertex layout, and blend, depth, and raster state. Program construction MUST assign an identity retained by clones. The pipeline cache MUST retain at most 512 entries with FIFO eviction.
**Verification**: Test — identity and FIFO eviction are covered, but complete key-component coverage also requires review.

### GFX-CACHE-2 WGSL cache validity

**Rule**: Disk WGSL cache keys MUST include bytecode SHA-256 and translator ABI. Disk reads MUST validate checksums and WGSL with naga, regenerating corrupt or incompatible entries.
**Verification**: Test

### GFX-CACHE-3 Disk cache bounds

**Rule**: WGSL disk caching MUST enforce a total byte limit and atomic replacement. Vulkan cache blobs MUST be adapter-keyed, bounded to 32 MiB, and SHA-256 checked, with incompatible driver data discarded.
**Verification**: Test — byte and partition budgets, corruption, native driver-size boundaries and incompatible-header fallback.

### GFX-CACHE-4 Cache location

**Rule**: Disk caches MUST write only under an explicitly selected host cache directory. Backends without native pipeline caching MUST retain portable WGSL caching.
**Verification**: Review — no existing test checks absence of filesystem writes without configuration across all backends.

### GFX-CACHE-5 Repeated shader creation

**Rule**: Identical bytecode MAY reuse successful translations while preserving independent shader objects and exact GetFunction bytes. Each process translation cache MUST retain at most 256 entries, with at most 8 MiB of bytecode, WGSL, and sampler payloads. Translation and assembled-program caches MUST evict oldest entries individually instead of clearing unrelated entries.
**Verification**: Test

### GFX-CACHE-6 Texture bindings

**Rule**: Texture binding caches MUST key layout identity, each bound view identity, and sampler state. They MUST retain at most 1024 entries with LRU eviction. Expired view lifetimes MUST invalidate cached bindings.
**Verification**: Test

### GFX-CACHE-7 Background compilation

**Rule**: Shader translation and module preparation MUST start at resource creation on a separately bounded, lower-priority worker. Demand MUST take queued work or join its existing compilation, preserving identical source and results. The worker MUST retain at most 64 queued jobs and 32 MiB, leaving work demand-driven if priority reduction fails.
On Apple hosts, foreground waits MUST temporarily donate priority to the compiler and release that donation after the final waiter resumes.
Binding changes MUST NOT start speculative module preparation for transient shader pairs. Shader and declaration creation retain their preparation jobs; actual draws and persisted pipeline recipes supply complete combinations.
**Verification**: Test — queue takeover, running-work joins and priority-donation lifetime; platform priority selection also requires review.

## Numerical comparisons

### GFX-NUM-1 Floating-point semantics

**Rule**: Shader arithmetic MUST use f32 and preserve D3D9 reference behavior for special values, including zero multiplication, reciprocal zero, and logarithm zero.
**Verification**: Test

## Surface copies

### GFX-COPY-1 Texture destinations and RGBX

**Rule**: Texture surfaces used by `StretchRect` MUST retain their container ownership until asynchronous completion. RGBX-to-ARGB copies MUST produce opaque alpha and preserve pixels outside the destination rectangle. Copies MUST NOT consume or modify guest drawing state.
**Verification**: Test

### GFX-COPY-2 Lockable render targets

**Rule**: Standalone non-multisampled render targets created with Lockable MUST support CPU locks through ordered GPU readback and writable unlock uploads. Targets created without Lockable MUST reject CPU locks. Read-only unlocks MUST preserve GPU contents.
**Verification**: Test
**Basis**: Microsoft Direct3D 9 `IDirect3DDevice9::CreateRenderTarget` and `IDirect3DSurface9::LockRect`.

CPU-only surfaces and managed texture-level locks preserve pending recorded commands; only GPU-backed locks require a readback barrier.

### GFX-FIXED-1 Lighting and materials

**Rule**: Fixed vertex processing MUST apply directional, point and spot lighting, material sources, ambient, emissive and enabled specular contributions using D3D9 equations. Light and material setters, queries and state blocks MUST preserve their values, with arbitrary light indices and at most eight active lights per draw. `MultiplyTransform` MUST premultiply the stored transform by the supplied matrix.
**Verification**: Test
**Basis**: Microsoft Direct3D 9 mathematics of lighting and `IDirect3DDevice9::MultiplyTransform`.

### GFX-FIXED-2 Fog, texture factor and shading

**Rule**: Fixed draws MUST implement D3D9 fog and point scaling as exceptions to GFX-INPUT-5, plus texture factor, shading, blend factors, depth bias and sRGB. Fog and specular MUST follow texture combining without changing the combined alpha. Fixed pipeline variants MUST preserve existing unlit pipeline recipes and cache identities.
**Verification**: Test
**Basis**: Microsoft Direct3D 9 fog formulas, texture arguments and shading modes.

### GFX-STATE-1 Neutral fixed state

**Rule**: Defaults, setters, and queries MUST agree for DITHERENABLE, COLORVERTEX, WRAP0–WRAP7, and AMBIENT. Enabling dithering or texture-coordinate wrapping MUST fail explicitly. AMBIENT MUST accept and preserve every DWORD value.
**Verification**: Test

Withdrawn: GFX-NUM-2, GFX-NUM-3, GFX-NUM-4, GFX-NUM-5

### GFX-CACHE-8 Persistent pipeline identity

**Rule**: Actual pipeline recipes MUST persist per game, partitioned by backend, driver identity and target format. Recipes MUST include shader content, texture visibility, vertex layouts and immutable render state. Missing, corrupt or incompatible archives MUST fall back to identical demand compilation.
**Verification**: Test — warm restart, content keys, corruption and version/driver rejection.

### GFX-CACHE-9 Recipe budgets

**Rule**: Versioned, checksum-validated archives MUST retain at most 512 recipes and 8 MiB per partition. Oldest recipes and driver partitions MUST be evicted, retaining at most four partitions.
**Verification**: Test

### GFX-CACHE-10 Prebuilding and counters

**Rule**: Prebuilding MUST share one result with demand and yield at pipeline boundaries during foreground compilation. Reports MUST count foreground waits and nanoseconds, prebuild hits and misses, persisted recipes and discarded archives.
**Verification**: Test — shared prebuild/demand results and warm restart; report assembly also requires review.
