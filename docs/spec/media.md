# Audio and video API contracts

ID prefix: MEDIA

This spec covers the native audio and video API front ends: DirectSound storage, DirectShow graphs and Windows Media. `adv-dx` implements them, and FFmpeg decodes through `adv-device` when the `media` feature is built.

## Audio APIs

### MEDIA-AUDIO-3 Deferred DirectSound storage

**Rule**: LOCDEFER buffers MUST retain their flag and share software PCM storage without starting playback during resource acquisition. Fixed-location flags MUST NOT combine with deferred allocation. Hardware requests MUST fail, and empty effect chains MUST NOT create DSP objects.
**Verification**: Test

### MEDIA-AUDIO-4 Buffered lead observations

**Rule**: The mixer MUST observe buffered lead and missing PCM without changing samples, timelines, cursors, notifications, or scheduling. Callback observation MUST allocate nothing, acquire no locks, perform no system calls, and traverse each ring's coverage at most once. Execution MUST read the alarm epoch with one relaxed atomic load, and reports MUST include both lead metrics.
**Verification**: Test

| Observation | Definition |
|---|---|
| Ring lead | Contiguous written duration from the playback phase; only playing streaming voices |
| Owned lead | Unplayed accepted queue duration, excluding ended or paused voices and pending producer commands |
| Refill period | Producer timestamps carried by nonempty Written commands or accepted PCM blocks; EMA weight 1/4, initialized by the first interval |
| Valid period | At least four refills and a positive EMA; the first owned block counts as a refill |
| Low lead | Lead strictly below twice the valid period; one event per normal-to-low transition, rearmed upon recovery |
| Alarm epoch | Increment for each low-lead event and each callback increasing either missing-PCM counter |
| low_lead_events | Cumulative low-lead transitions |
| min_lead_ratio_permille | Lifetime minimum floor(1000 × lead / period); zero before any valid observation |

## Failure and completion

### MEDIA-FAIL-1 Builds without media support

**Rule**: Without the `media` feature, connecting a video source MUST return `VFW_E_UNSUPPORTED_STREAM` and report a capability gap. The graph MUST NOT publish an output pin for it. Undecodable input MUST fail the same way.
**Verification**: Test

### MEDIA-FAIL-2 Stream failure

**Rule**: A decode or seek failure after connection MUST send `EC_ERRORABORT` with the failing HRESULT and a warning. It MUST stop the stream and drop its pending PCM. It MUST NOT send a successful completion.
**Verification**: Test

### MEDIA-FAIL-3 Completion

**Rule**: Completion MUST come only from the actual end of stream or an explicit stop position, after pending PCM has played. Container duration MUST NOT end decoding.
**Verification**: Test

## Filter graphs

### MEDIA-GRAPH-1 Graph interfaces

**Rule**: Graph interfaces MUST share identity and state for connection, playback, events, seeking, position, volume, and event reception. The graph MUST hold one reference to its IObjectWithSite site, which GetSite queries, until replaced or released. System splitting and decoding MUST use the common media decoder.
**Verification**: Test

### MEDIA-GRAPH-2 File sources

**Rule**: AddSourceFilter MUST use ordinary guest file APIs and reject missing files or invalid arguments without publishing a source or touching its output. Its async reader MUST load undecodable contents, which rendering then rejects with `VFW_E_UNSUPPORTED_STREAM`. The WM ASF Reader MUST be this source, loaded through IFileSourceFilter and rejecting undecodable files, and its filter MUST keep the file sharing lock.
**Verification**: Test

### MEDIA-GRAPH-3 Render negotiation

**Rule**: Render MUST validate an unconnected output pin's graph ownership and direction before invoking guest Connect. It MUST prefer existing guest video renderers before the built-in renderer and connect audio output. Allocator refusal MUST disconnect and release the failed candidate before retrying.
**Verification**: Test

### MEDIA-GRAPH-4 Append-only RenderFile

**Rule**: IGraphBuilder and IMediaControl RenderFile MUST append through the common file-source and Render path without clearing prior graph members. Failure MUST preserve prior connections, and the graph MUST retain source locks. The reserved playlist parameter MUST be ignored.
**Verification**: Test

### MEDIA-GRAPH-5 RenderEx limits

**Rule**: RenderEx with zero flags MUST use normal Render semantics. Existing-renderer mode MAY add intermediate decoders but MUST NOT create renderers and MUST return `VFW_E_RENDERER_NOT_FOUND` when none are available. Other flags or nonnull context MUST return `E_INVALIDARG`.
**Verification**: Test

### MEDIA-GRAPH-6 Failed connection ownership

**Rule**: Total Render failure MUST roll back new connections, members, and references, restoring the source decoder for retry. Removal or termination MUST disconnect every connected pin of each member and its peer, then release readers.
**Verification**: Test

### MEDIA-GRAPH-7 Partial graphs and unsupported automation

**Rule**: Partial rendering MUST return `VFW_S_PARTIAL_RENDER` with a capability warning. Generic external-splitter readers and unsupported filter automation objects MUST fail explicitly.
**Verification**: Review — existing rollback tests do not cover partial-render warnings and both unsupported automation boundaries.

### MEDIA-GRAPH-8 Virtualized file contents

**Rule**: File sources MUST read the host descriptor only while the guest's read path keeps its mapped entry bytes. The read path is kernelbase ReadFile and ntdll NtReadFile. Once guest code hooks it, the contents MUST come through the guest's ReadFile, and the handle MUST close through the guest's CloseHandle.
**Verification**: Test

### MEDIA-GRAPH-9 Event timeouts

**Rule**: GetEvent MUST zero the event code before it waits, so a timeout returns `E_ABORT` with code zero.
**Verification**: Test

### MEDIA-GRAPH-10 Aggregation

**Rule**: An aggregated filter graph MUST be created only for IUnknown, and any other IID MUST return `E_NOINTERFACE` with a NULL output. Its inner IUnknown MUST keep its own count and answer IUnknown with itself. Every other interface MUST forward QueryInterface, AddRef and Release to the outer object.
**Verification**: Test

### MEDIA-GRAPH-11 Member names

**Rule**: AddFilter MUST give a nameless filter the graph counter's next free four-digit name, starting at `0001`. A taken name MUST get the counter appended and return `VFW_S_DUPLICATE_NAME`. IGraphVersion MUST change on every membership change.
**Verification**: Test

### MEDIA-GRAPH-12 Reference clock

**Rule**: The graph MUST hold one reference to its reference clock and give it to every member, including later ones. Removal and final release MUST clear a member's clock. Until a clock is set explicitly, leaving Stopped MUST choose the newest member's own clock, otherwise a system clock.
**Verification**: Test

### MEDIA-GRAPH-13 Automation and filter mapper

**Rule**: Automation interfaces MUST answer IDispatch through the quartz type library. IFilterMapper, IFilterMapper2 and IFilterMapper3 MUST come from an aggregated system filter mapper.
**Verification**: Review — unit tests cover GetTypeInfoCount only; the type library and the mapper come from Wine's quartz, which unit tests do not load.

### MEDIA-GRAPH-14 Source filter choice

**Rule**: AddSourceFilter MUST take the media type from `HKCR\Media Type\Extensions` when the file extension is registered. Otherwise the first `Media Type` entry whose byte patterns all match MUST decide the type and the source filter. A registered source other than the async reader MUST be created, loaded and added.
**Verification**: Test

### MEDIA-GRAPH-15 Graph seeking

**Rule**: The graph's IMediaSeeking MUST ask every member exposing IMediaSeeking in media time, newest first, with host playback answering as one more member. Durations and stops MUST take the largest answer, capabilities their intersection, and SetPositions MUST pause a running graph around the seek. Stop MUST re-seek every member to the kept position.
**Verification**: Test

### MEDIA-GRAPH-16 Intelligent connect

**Rule**: Connect and Render of a guest pin MUST try a direct connection, then graph members newest first, then the mapper's matching filters, added on demand. Recursion MUST stop at depth five, and a connection closing a loop MUST fail with VFW_E_CANNOT_CONNECT. A filter added for a failed attempt MUST be removed again.
**Verification**: Test

### MEDIA-STREAM-1 Pull delivery

**Rule**: IAsyncReader sources MUST connect through host filters and negotiate RGB32, RGB24, RGB565, RGB555, or PCM with guest renderers and host allocators. Guest sample delivery MUST run on a dedicated guest thread using graph time, with audio entering the shared mixer. Rewinding MUST restart decoding at the first packet of every container.
**Verification**: Test

### MEDIA-STREAM-2 Rejected push

**Rule**: System splitter and decoder input pins MUST reject Receive and ReceiveMultiple with `E_NOTIMPL` and a capability warning. ReceiveMultiple MUST zero a valid processed output and return `E_POINTER` for an invalid output.
**Verification**: Test

### MEDIA-STREAM-3 Audio lead and pause

**Rule**: Audio-only streams MUST decode about one second ahead, matching the quartz DirectSound renderer's buffer. A paused graph MUST pause its queued PCM, and resuming MUST shift that PCM by the paused time.
**Verification**: Test

### MEDIA-STREAM-4 Decoding off the execution thread

**Rule**: Each movie source MUST decode on a host thread of its own and answer every request exactly as decoding on the caller's thread would. Between requests it MAY decode and convert the next frames ahead.
**Verification**: Test

## Windows Media decoder wrappers

### MEDIA-DMO-1 Decoder admission

**Rule**: DMO wrappers MUST accept only supported WMV/WMA decoder identities and publish input/output pins only after Init.
Wine decoder capability probes MUST succeed for available codecs before stream sequence headers are supplied; decoder opening MUST wait for the first sample.
**Verification**: Test

### MEDIA-DMO-2 Push ownership and preroll

**Rule**: The DMO wrapper MUST be the only push-accepting host filter and accept only host samples from its allocator. Decoded video MUST follow graph time with one paused preroll frame, while decoded audio MUST use the shared mixer.
**Verification**: Review — DMO tests cover identity and pin publication but not pushed sample ownership, scheduling, and paused preroll.

### MEDIA-DMO-3 End-of-stream ordering

**Rule**: DMO EOS MUST drain the decoder before downstream forwarding. Audio-only completion MUST wait for queued PCM playback.
**Verification**: Review — no existing DMO test observes decoder drain, downstream EOS, and final audible PCM ordering.

### MEDIA-DMO-4 Upstream seeking and control

**Rule**: Connected DMO graphs MUST derive duration from upstream IMediaSeeking and forward seeks to that source. Pause and Run MUST invoke the upstream source last, and Stop MUST invoke it first.
**Verification**: Test

## Synchronous Windows Media reader

### MEDIA-WMV-1 Compressed samples

**Rule**: WMCreateSyncReader MUST deliver compressed samples with timestamps and clean-point flags, and reconstruct missing decoded WMA Pro timestamps by counting samples. Without Wine assembly, requests for decompressed output MUST return `E_NOTIMPL`. Builds without media support MUST fail reader creation.
**Verification**: Test

### MEDIA-WMV-2 Input and allocation

**Rule**: Synchronous readers MUST limit complete guest IStream input to 512 MiB. When supplied, guest IWMReaderAllocatorEx MUST own the buffer receiving each delivered sample.
**Verification**: Review — packet tests use prepared demuxed data and do not exercise full IStream admission or guest sample allocation.

## Embedded video

### MEDIA-VIDEO-1 Window state

**Rule**: Connected built-in video renderers MUST preserve guest Owner, child styles, position, visibility, AutoShow, and cursor state. Invalid inputs MUST leave window state unchanged.
**Verification**: Test

### MEDIA-VIDEO-2 Geometry and outputs

**Rule**: Video interfaces MUST preserve independent source cropping and destination rectangles, border color, native dimensions, and actual frame time. All outputs MUST be validated before any write.
**Verification**: Test

### MEDIA-VIDEO-3 Snapshots and interface failure

**Rule**: Paused snapshots MUST return cropped RGB32 DIBs with checked capacity. Missing renderers MUST return `E_NOINTERFACE`, and disconnected renderers MUST return `VFW_E_NOT_CONNECTED`.
**Verification**: Test

### MEDIA-VIDEO-4 Independent composition

**Rule**: Video MUST update through the bounded frame mailbox independently of game Present while retaining an immutable game frame. GPU composition MUST avoid game-frame readback, and software rendering and explicit capture MUST use the same geometry.
**Verification**: Test

### MEDIA-VIDEO-5 Layer retirement

**Rule**: Owner hiding or destruction, explicit hide or detach, and graph release MUST remove video and restore the retained game frame. Paused service MUST recheck geometry. Stop MUST retain the final image until hidden or detached.
**Verification**: Review — existing composition and window tests do not exercise every graph-driven owner and Stop transition through the presentation service.

### MEDIA-VIDEO-6 Window limits

**Rule**: Standalone host video windows, fullscreen activation, and input-message forwarding MUST fail explicitly without altering embedded geometry.
**Verification**: Test

### MEDIA-VIDEO-7 Native video planes

**Rule**: With GPU rendering, decoded host pictures MUST use four reusable presentable buffers per stream: NV12 on Apple, NV21 on Android. Display MUST transfer each picture's presentable lease at most once. Guest pixel requests MUST convert on demand using the stream matrix and range.
**Verification**: Test
