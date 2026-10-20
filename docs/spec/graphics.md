# DirectDraw and programmable graphics

ID prefix: GFX

This spec covers the DirectDraw and Direct3D 9 API front ends: capabilities, shaders, vertex input, samplers, buffers, caches, numerical behavior and Effects. `adv-dx` and the GPU renderer in `adv-device` implement them.

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
**Verification**: Test

## D3D9 frame recording

### GFX-REC-1 Ordered recording and flush boundaries

**Rule**: D3D9 MUST validate recorded commands synchronously and flush before Present, readback, GPU waits, Reset, release, or recording limits. Busy continuations MUST resume the unexecuted suffix without replaying successful commands. Execution-time device failure MUST persist until Reset and be reported by Present or TestCooperativeLevel.
**Verification**: Test

Recorded programmable draws retain raw streams once, after consuming neutral inputs for bounds. Binding dependencies are cached until binding changes or resource destruction. Content writes resolve a slot once and index its current/base versions directly; retired slots can be reused.

Flush limits are 4096 commands or 64 MiB of upload and vertex data. Readback boundaries include GetRenderTargetData, GetFrontBufferData, GPU LockRect and GetDC; unsupported capture retains its ABI failure.

### GFX-REC-2 Complete content identities and repeated frames

**Rule**: Content identities MUST include every output-affecting command input, exact float bits, and sampled subresource versions. Elision MUST require unchanged predicted contents for every written resource, unchanged presentation parameters, and no intervening presentation from another front. Elided frames MUST preserve guest completion counters, return values, and pacing while issuing no GPU commands or publications.
**Verification**: Test

Full clears replace previous dependencies; partial clears, draws and copies retain them. Upload admission assigns a fresh identity. A partial flush or a private presentation without the shared output prevents elision of that frame. DISCARD presentation may rotate independently reconstructible targets; destination-dependent frames preserve writable contents using an immutable GPU snapshot.
Clear identities MUST include only the selected value and extent of each written color, depth or stencil component. Draws MUST read only the enabled depth/stencil tests.

### GFX-REC-3 Asynchronous pixel verification

**Rule**: `ADV_VERIFY_FRAMES=1` MUST execute full frames and asynchronously compare unchanged pixels with the previous readback. Mismatches MUST warn and increment Diagnostics. Diagnostics MUST expose command counts, flush reasons, eliminated frames, verification mismatches, full/clipped executions, and accumulated redraw fractions.
**Verification**: Test

### GFX-REC-4 Conservative damage and bounds

**Rule**: Each target subresource MUST compare ordered command keys and bounds, including sampled content and retained destination dependencies. Changed, added, or removed entries MUST damage both old and new bounds. Damage MUST retain at most four rectangles and conservatively merge overflow without promoting bounded damage to the whole target based on coverage alone.
**Verification**: Test

Overflow merges into one bounding box. Bounds use decoded XYZRHW or CPU world/view/projection positions, or the scalar vertex shader interpreter for at most 64 float-input vertices. Perspective division and viewport mapping precede one-pixel raster margins, point-size expansion, and viewport/guest-scissor intersection. Nonpositive w, nonfinite values, excessive vertices, unsupported inputs or instructions use full-target bounds. Clear and copy use destination rectangles; CPU uploads use the whole target. Size/format changes and Reset invalidate history. Reading an intermediate target before a later write, or sharing an attachment across target allocations, conservatively forces full execution.
Clears and copies MUST compare their written region independently of its old destination values, including partial clears of padded depth/stencil attachments. Attachment histories MUST use their own extent; execution clips MUST remain inside the logical color view. Depth/stencil changes MUST contribute to required execution and tested draws, but MUST NOT directly enlarge published color damage.

Intermediate color damage MUST propagate through proven axis-aligned copies and texture sampling, including scale, reversal and filter footprints; histories MUST resolve producer changes before consumers and accumulate allocation age separately. Local sampling proofs require constant positive homogeneous w and two coordinate planes per axis. Sampling uses clamped addressing and the base level without mip filtering. Fragment coordinates MUST preserve direct varyings, optionally moved through registers. Dependent or projected reads, derivatives, control flow, centroid inputs and unsupported geometry MUST retain full consuming-draw bounds.

### GFX-REC-5 Clipped execution and buffer age

**Rule**: Present MUST replay the complete recording once, loading attachments and intersecting writes with the redraw bounding box. Each allocation MUST accumulate damage since its last execution, using full redraw when older than the eight-frame history. Clip state MUST clear after execution, including Busy and failure paths.
**Verification**: Test

Unknown allocations redraw fully; nonrotating targets have age one. Clear and copy respect the clip. Early flushes conservatively invalidate damage history. DISCARD rotation requires an initial full overwrite so replay reconstructs stale slots exactly. Accumulating frames restore the last logical backbuffer when necessary and keep the snapshot path. Early reads restore that baseline too. Diagnostic verification executes full frames while still computing damage and reading the actual presented allocation. No presentation cadence or audio behavior changes.
Immutable snapshot presentation MUST preserve known color damage, just as rotating presentation does. Draws whose conservative bounds do not intersect the required execution region MUST be omitted before backend submission.

### GFX-REC-6 D3D9 leaf admission

**Rule**: Only the methods below MAY enter EnvLeaf without blocking, guest callbacks or guest memory allocation and release. Draw and Clear admission MUST decline before side effects when recording might flush, wait or execute immediately. Declined calls MUST execute their ordinary adapter with identical state, commands and guest results.
**Verification**: Test
**Basis**: A64-CTL-12

| Methods | Admission |
|---|---|
| Set/GetRenderState, Set/GetTextureStageState, Set/GetSamplerState, Set/GetTransform, Set/GetViewport, Set/GetScissorRect | Synchronous state and guest memory access. |
| GetFVF, GetTexture, GetStreamSource, GetIndices, GetVertexDeclaration, GetVertexShader, GetPixelShader | Query state and retain existing COM identities. |
| Set/GetVertexShaderConstantF/I/B, Set/GetPixelShaderConstantF/I/B, BeginScene, EndScene | Synchronous constants and scene state. |
| SetTexture, SetStreamSource, SetIndices, SetVertexShader, SetPixelShader, SetVertexDeclaration, SetFVF, StateBlock Capture/Apply | Any replaced binding retains public ownership; unchanged bindings may have zero public references. SetTexture additionally requires no residency restoration or dirty-level flush. |
| DrawPrimitive, DrawIndexedPrimitive, DrawPrimitiveUP, DrawIndexedPrimitiveUP, Clear | No active flush; incoming command and conservative byte bound remain strictly below 4096 commands and 64 MiB. UP additionally requires empty bindings it would clear. |

Binding and state-block admission checks all releases before mutation. Calls that
could free guest storage or restore texture residency decline to the ordinary
path. Rebinding an unchanged live object avoids redundant validation and reference
transfers; state-block recording still captures unchanged setter membership.

### GFX-REC-7 Shader constant ranges and content hashing

**Rule**: Translation MUST cache external register usage, excluding shader-local definitions. Fingerprints, copies and uploads MUST process only those ranges while preserving exact bits and uniform layout. Recording MUST retain complete xxh3-128 identities, and both backends MUST reserve aligned complete binding windows before writing.
**Verification**: Test

Usage includes implicit matrix rows, int/bool operands and legacy bump parameters. Relative float addressing covers all 256 float registers except local definitions. Constants, vertices, indices and pixels use 128-bit content keys. Insufficient storage wraps before the complete binding window exceeds the buffer. Packed wgpu uploads retain one queue write per buffer per batch. CPU damage interpretation agrees with complete constants.

### GFX-REC-8 Frontend work counters

**Rule**: Diagnostic counter builds MUST report retained bytes and lookups per draw, entries per successful StateBlock Apply, and dead frame/pass eliminations. Ordinary builds MUST compile out work instrumentation.
**Verification**: Test

The diagnostic Cargo feature is `adv-dx/d3d9-counters`.

### GFX-REC-9 Dead target passes

**Rule**: Only complete target passes whose every output is fully overwritten before any read MAY be eliminated. Partial flushes, uploads, partial overwrites, and preserved attachment aspects MUST retain their dependencies. Verification mode MUST execute the complete reference command stream.
**Verification**: Test

A pass is a contiguous group of writes to one target allocation. Backward
liveness tracks logical resource levels and attachment aspects. Dead frames use
GFX-REC-2; these and dead passes are the only whole-frame/pass omissions.
Damage clipping remains a conservative region optimization under GFX-REC-4/5.

### GFX-PACE-1 Vsync-locked refresh boundaries

**Rule**: When the host display rate is within 1% of a whole multiple of the guest refresh rate, Present boundaries MUST fall on host vsyncs. An on-time boundary MUST follow the previous one by exactly that multiple of vsyncs. A late Present MUST wait for the next vsync without repaying missed frames, and other rates MUST keep machine-clock boundaries.
**Verification**: Test

### GFX-PACE-2 Unpaced presents beyond the display rate

**Rule**: An IMMEDIATE or occluded Present MUST count as a whole frame for the DEV-FRAME-9 frame cap. Fast-forward MUST bypass that cap.
**Verification**: Test

The cap holds only the presenting context's next empty PeekMessage, so Present itself still returns at once.

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
float bits, uncaptured state and COM ownership. An unchanged Capture reuses its
snapshot storage; an unchanged Apply performs no state writes.

## Vertex and raster input

### GFX-INPUT-1 Declarations

**Rule**: Programmable input MUST fetch all 17 supported D3DDECLTYPE formats from raw multistream bytes with D3D component defaults.
**Verification**: Test

### GFX-INPUT-2 Mixed pipelines

**Rule**: VS-only and PS-only draws MUST preserve the fixed stage's supported inputs and outputs. Programmable rasterization MUST apply D3D9 half-pixel, depth, fog, point-size, and clip-plane conventions.
**Verification**: Review — native shader and raster scripts cover these combinations, but no annotated test covers the full mixed-stage raster contract.

### GFX-INPUT-3 Pretransformed vertices

**Rule**: XYZRHW and POSITIONT draws MUST bypass the bound vertex shader while preserving its binding identity and continuing pixel shading. POSITIONT declarations MUST use offset-ordered stream 0 inputs.
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

**Rule**: Identical bytecode MAY reuse successful translations while preserving independent shader objects and exact GetFunction bytes. Each process translation cache MUST retain at most 256 entries, with at most 8 MiB of bytecode, WGSL, and sampler payloads.
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

## Effects

### GFX-EFFECT-1 Binary Effect

**Rule**: Precompiled FXO Effects MUST preserve parameter, technique, pass, constant-table, preshader, and pass-state semantics. Effect input parsing MUST remain bounded on truncated or malformed data.
**Verification**: Test

### GFX-EFFECT-2 Effect boundaries

**Rule**: HLSL source compilation, unsupported FXLC expressions, state managers, parameter blocks, and pools MUST fail explicitly. Version declarations MUST NOT hide these limits behind successful no-ops.
**Verification**: Review — binary and preshader tests do not enumerate every unsupported Effect interface and source-compilation entry.

## D3DX and surface copies

### GFX-D3DX-1 Empty texture correction

**Rule**: `_29` and `_43` texture requirement checks and creation MUST correct dimensions, mip counts, and compatible integer formats using actual D3D9 capabilities. Optional requirement outputs MUST remain unchanged on correction failure. Created textures MUST share D3D9 COM identity, mip storage, locks, and release behavior.
**Verification**: Test

### GFX-D3DX-2 Matrix and Vec2 operations

**Rule**: `_29` and `_43` math entries MUST share axis rotations and Vec2 transforms, reading inputs before writing overlapping outputs. Normals MUST ignore translation; coordinate transforms MUST divide by homogeneous W and return NaN for zero W.
**Verification**: Test

### GFX-COPY-1 Texture destinations and RGBX

**Rule**: `StretchRect` into render-target texture surfaces MUST retain container ownership until asynchronous completion. RGBX-to-ARGB copies MUST produce opaque alpha and preserve pixels outside the destination rectangle. Copies MUST NOT consume or modify guest drawing state.
**Verification**: Review — the native texture-blit script checks pixels and identity, but no annotated test covers asynchronous container retention.

### GFX-STATE-1 Neutral fixed state

**Rule**: DITHERENABLE zero and COLORVERTEX defaults, setters, and queries MUST agree. Enabling dithering MUST fail explicitly.
**Verification**: Review — the native texture-blit script checks state roundtrips, but no annotated Rust test covers both states.

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
