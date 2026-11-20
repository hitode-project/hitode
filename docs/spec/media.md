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

**Rule**: AddSourceFilter MUST use ordinary guest file APIs and reject missing files or invalid arguments without publishing a source or touching its output. Its async reader MUST load undecodable contents, which rendering then rejects with `VFW_E_UNSUPPORTED_STREAM`. The WM ASF Reader MUST be this source, loading through IFileSourceFilter before or after graph join, rejecting undecodable files and keeping the file lock.
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

**Rule**: GetEvent MUST zero the event code before it waits, so a timeout returns `E_ABORT` with code zero. GetEvent finding no queued event MUST reset the event handle.
**Verification**: Test

### MEDIA-GRAPH-10 Aggregation

**Rule**: An aggregated filter graph or host filter MUST be created only for IUnknown, and any other IID MUST return `E_NOINTERFACE` with a NULL output. Its inner IUnknown MUST keep its own count and answer IUnknown with itself. Except for DMO wrappers, every other interface MUST forward QueryInterface, AddRef and Release to the outer object.
**Verification**: Test

### MEDIA-GRAPH-11 Member names

**Rule**: AddFilter MUST give a nameless filter the graph counter's next free four-digit name, starting at `0001`. A taken name MUST get the counter appended and return `VFW_S_DUPLICATE_NAME`. IGraphVersion MUST change on every membership change.
**Verification**: Test

### MEDIA-GRAPH-12 Reference clock

**Rule**: The graph MUST hold one reference to its reference clock and give it to every member, including later ones. Removal and final release MUST clear a member's clock. Until a clock is set explicitly, leaving Stopped MUST choose the earliest added member's own clock, otherwise a system clock.
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

### MEDIA-GRAPH-17 Control delegation

**Rule**: Without a host renderer, the graph's IBasicAudio, IBasicVideo2 and IVideoWindow MUST forward each call to the newest guest member exposing that interface. With no such member, IBasicAudio MUST return E_NOTIMPL and the video interfaces E_NOINTERFACE.
**Verification**: Test

### MEDIA-GRAPH-18 Graph state

**Rule**: Graph transitions MUST return the first member result that is not S_OK, and Stop from Running MUST pause every member first. GetState MUST poll every member and report VFW_S_STATE_INTERMEDIATE until all reach the graph state or the timeout passes. Stop MUST return only after every host stream thread has left the downstream filters.
**Verification**: Test

Withdrawn: MEDIA-GRAPH-19

### MEDIA-GRAPH-20 Asynchronous run

**Rule**: When the pause of IMediaControl::Run from Stopped returns S_FALSE, Run MUST return S_FALSE and report Running unless a member's GetState(0) gives VFW_S_CANT_CUE. A worker MUST then run the members once all have paused unless Pause or Stop comes first, and streams MUST stay paused until then. Otherwise, and for IMediaFilter::Run from any state, the graph MUST run every member at once.
**Verification**: Test

### MEDIA-GRAPH-21 Event notification

**Rule**: Default handling MUST consume only EC_COMPLETE and EC_REPAINT, and every other event MUST be queued. While AM_MEDIAEVENT_NONOTIFY is set, events MUST be dropped, and setting it MUST discard the queue and reset the event handle.
**Verification**: Test

### MEDIA-GRAPH-22 Completion

**Rule**: When every renderer has completed, the graph MUST queue EC_COMPLETE, or set only the event handle while notification is disabled. Completion MUST end WaitForCompletion and leave the position at the stop members reported when the run started. Starting a run MUST remove queued EC_COMPLETE events.
**Verification**: Test

### MEDIA-GRAPH-23 Member removal

**Rule**: RemoveFilter MUST disconnect each connected pin's peer and then the pin before the filter leaves the graph. The first failing Disconnect MUST be returned, and the filter MUST stay a member.
**Verification**: Test

### MEDIA-GRAPH-24 Direct connection

**Rule**: Connect and ConnectDirect MUST call Connect on the first pin as given, without reordering the pair. ConnectDirect MUST fail with VFW_E_CIRCULAR_GRAPH when the connection would close a loop.
**Verification**: Test

### MEDIA-GRAPH-25 Transition order

**Rule**: Each Stop, Pause and Run MUST first sort all members by their pins' connections, downstream before upstream, starting from the newest member. Host members MUST take their places by their own pins. Members MUST then change state in that order.
**Verification**: Test

### MEDIA-GRAPH-26 Rendered file splitters

**Rule**: RenderFile's splitter outputs MUST carry the media types their stream headers give, as IAsyncReader splitters' do. Each output MUST try the graph's members before a host decoder. A guest input MUST receive the file's demuxed packets.
**Verification**: Test — header types; members-first rendering and packet delivery also require review.

### MEDIA-STREAM-1 Pull delivery

**Rule**: IAsyncReader sources MUST connect through host filters and negotiate RGB32, RGB24, RGB565, RGB555, or PCM with guest renderers and host allocators. Guest delivery MUST use graph time on dedicated guest threads, one per guest audio renderer, feeding it paused or running with EndOfStream after its samples. Host-rendered audio MUST enter the shared mixer, and rewinding MUST restart decoding at the first packet of every container.
**Verification**: Test

### MEDIA-STREAM-2 Rejected push

**Rule**: System splitter input pins MUST reject Receive and ReceiveMultiple with `E_NOTIMPL` and a capability warning. ReceiveMultiple MUST zero a valid processed output and return `E_POINTER` for an invalid output.
**Verification**: Test

### MEDIA-STREAM-3 Audio lead and pause

**Rule**: Audio-only streams MUST decode about one second ahead, matching the quartz DirectSound renderer's buffer. A paused graph MUST pause its queued PCM, and resuming MUST shift that PCM by the paused time. Pausing from Stopped or Running MUST present the first picture at or after the position, which the stream then holds.
**Verification**: Test

### MEDIA-STREAM-4 Decoding off the execution thread

**Rule**: Each movie source MUST decode on a host thread of its own and answer every request exactly as decoding on the caller's thread would. Between requests it MAY decode and convert the next frames ahead.
**Verification**: Test

### MEDIA-STREAM-5 MPEG media types

**Rule**: Guest-created MPEG filters MUST enumerate and validate Windows' MPEG stream and decoded output formats, while Render's splitter MUST accept any decodable container without demuxing it. Splitter audio types MUST preserve MPEG frame header fields and offer payload, elementary payload, and MP3 formats when applicable. Media negotiation MUST validate explicit output formats and prioritize downstream types for wildcard requests.
**Verification**: Test

### MEDIA-STREAM-6 Filter pin ownership

**Rule**: MPEG pins MUST count public references on their owning filter while retaining their own IUnknown identity. Pin enumerators MUST retain their filter and snapshot its pins until Reset. Pin enumerators MUST use the argument and cursor results below.
**Verification**: Test

| Operation | Result |
|---|---|
| Next of multiple pins with valid output and no fetched count | `E_INVALIDARG`, unchanged position |
| Skip beyond the remaining pins | `S_FALSE`, unchanged position |

### MEDIA-STREAM-7 MPEG decoder delivery

**Rule**: MPEG decoders MUST retain notified input allocators and negotiate output allocators with the downstream input. Output allocators MUST commit during Pause or Run and decommit during Stop. Decoder inputs MUST decode pushed samples, forward stream events, reject stopped delivery, and return `S_FALSE` during flushing.
**Verification**: Test

### MEDIA-STREAM-8 MPEG splitter streams

**Rule**: Direct MPEG splitter outputs MUST deliver compressed packets and end-of-stream while paused, using the selected segment positions. Splitter output seeking MUST expose media time, capability mask `0x37`, duration, positions, and a positive playback rate. IAMStreamSelect MUST describe each output's type, group, name, and pin identity.
**Verification**: Test

### MEDIA-STREAM-9 Streams without a clock

**Rule**: After SetSyncSource(NULL), a running video-only stream MUST present each picture without waiting for its time. GetCurrentPosition MUST report 0 while such a graph runs and the stop once it completes.
**Verification**: Test

## Windows Media decoder wrappers

### MEDIA-DMO-1 Decoder admission

**Rule**: DMO wrappers MUST publish input/output pins only after Init, using a built-in Windows Media decoder or an aggregated registered media object.
Wine decoder capability probes MUST succeed for available codecs before stream sequence headers are supplied; decoder opening MUST wait for the first sample.
**Verification**: Test

### MEDIA-DMO-2 Push ownership and preroll

**Rule**: Built-in DMO decoders MUST accept only host samples, while registered media objects MUST receive buffers wrapping upstream samples. Decoded video MUST follow graph time with one paused preroll frame. Decoded audio MUST reach connected guest renderers during preroll and playback, or use the shared mixer for host renderers.
**Verification**: Test

### MEDIA-DMO-3 End-of-stream ordering

**Rule**: DMO EOS MUST drain the decoder before downstream forwarding. Audio-only completion MUST wait for queued PCM playback.
**Verification**: Review — no existing DMO test observes decoder drain, downstream EOS, and final audible PCM ordering.

### MEDIA-DMO-4 Upstream seeking and control

**Rule**: Connected DMO graphs MUST derive duration from upstream IMediaSeeking and forward seeks to that source. Pause and Run MUST invoke the upstream source last, and Stop MUST invoke it first.
**Verification**: Test

### MEDIA-DMO-5 Wrapper identity and state

**Rule**: DMO wrappers MUST validate aggregation requests but retain their own controlling identity, exposing additional interfaces through the aggregated media object. Leaving Stopped MUST allocate streaming resources and commit connected output allocators, preserving Stopped when allocation fails. Stop MUST wait for active reception, decommit outputs, flush the media object, and free streaming resources.
**Verification**: Test

### MEDIA-DMO-6 Media object streaming

**Rule**: Registered DMOs MUST negotiate types through IMediaObject, including allocator sizes, alignment, and dynamic output types. Reception MUST translate timestamps and flags, drain incomplete connected outputs, and propagate downstream failures. EOS MUST drain once before flushing and forwarding, and flushing MUST reject samples until EndFlush resets EOS and flushes the media object.
**Verification**: Test

## Synchronous Windows Media reader

### MEDIA-WMV-1 Compressed samples

**Rule**: WMCreateSyncReader MUST deliver compressed samples with timestamps and clean-point flags, and reconstruct missing decoded WMA Pro timestamps by counting samples. Without Wine assembly, requests for decompressed output MUST return `E_NOTIMPL`. Builds without media support MUST fail reader creation.
**Verification**: Test

### MEDIA-WMV-2 Input and allocation

**Rule**: Synchronous readers MUST limit complete guest IStream input to 512 MiB. Stream and output allocators MUST remain separate, receive zero flags and timestamps, and own compressed and decoded buffers respectively. Once used, both allocator slots for that stream MUST reject changes, and allocation failure MUST return `NS_E_NO_MORE_SAMPLES`.
**Verification**: Test

### MEDIA-WMV-3 Output properties

**Rule**: Synchronous readers MUST number audio outputs before video and return independent output-property snapshots. Output formats MUST default to PCM audio and RGB24 video, with full source and target rectangles. Format negotiation MUST reject incompatible major types, unsupported video layouts, changed video dimensions, and audio channel expansion with Windows HRESULTs.
**Verification**: Test

### MEDIA-WMV-4 Reader arguments

**Rule**: `GetStreamSelected` and compressed-sample mode methods MUST return `E_INVALIDARG` for invalid stream numbers, and nonexistent profile stream numbers MUST return `NS_E_NO_STREAM`. Invalid selections MUST preserve the previous selection, and end-of-stream MUST preserve timestamps while reporting an explicitly requested stream number. Reading any stream MUST require an output number or stream number destination.
**Verification**: Test

### MEDIA-WMV-5 Header metadata

**Rule**: ASF duration MUST come from the header's play duration minus preroll, and compressed ASF video samples MUST report one-millisecond durations. Missing attributes MUST leave destinations unchanged, and `WM/VideoFrameRate` MUST return `ASF_E_NOTFOUND` when absent. Insufficient attribute buffers MUST update the required size without changing the attribute type or value.
**Verification**: Test

### MEDIA-WMV-6 Reader settings and identities

**Rule**: Synchronous readers MUST expose profile, header, language-list, packet-size, playlist-burn, and timecode interface identities. Output settings MUST follow Windows defaults and applicability for audio and video, retain assigned values, and return `E_UNEXPECTED` before opening.
**Verification**: Test

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

**Rule**: Host video windows other than VMR windows, fullscreen activation, and unsupported input-message forwarding MUST fail explicitly without altering embedded geometry.
**Verification**: Test

### MEDIA-VIDEO-7 Native video planes

**Rule**: With GPU rendering, decoded host pictures MUST use four reusable presentable buffers per stream: NV12 on Apple, NV21 on Android. Display MUST transfer each picture's presentable lease at most once. Guest pixel requests MUST convert on demand using the stream matrix and range.
**Verification**: Test

### MEDIA-VIDEO-8 VMR configuration and interfaces

**Rule**: VMR rendering mode MUST default to windowed and lock after its first explicit setting. Interfaces MUST follow the selected mode, and stream-count queries MUST fail before mixing is enabled.
**Verification**: Test

### MEDIA-VIDEO-9 VMR pins and enumeration

**Rule**: Mixing MUST publish the requested numbered input pins. VMR pins MUST share their filter's reference count, and pin enumerators MUST retain their filter. Enumerators MUST observe added pins after Reset.
**Verification**: Test

### MEDIA-VIDEO-10 VMR bitmap negotiation

**Rule**: VMR connection MUST interpret supported bitmap depths independently of subtype labels, while rejecting paletted RGB subtypes. QueryAccept MUST accept a null subtype with a video information format. A sample without a stop time MUST retain its start timestamp.
**Verification**: Test

### MEDIA-VIDEO-11 VMR readiness and paused samples

**Rule**: Connected VMR transitions MUST return S_FALSE before receiving a sample, and paused GetState MUST report an intermediate state. Paused Receive MUST publish and hold the first sample until Run, Stop, or flushing, including samples marked preroll. Flush completion MUST require a new sample, while end of stream MUST complete readiness.
**Verification**: Test

### MEDIA-VIDEO-12 VMR guest windows

**Rule**: Windowed VMR MUST own a guest USER window and honor its caption, styles, owner, geometry, visibility, and message forwarding. Graph startup MUST show windowed VMR windows only when AutoShow is enabled. Windowed and windowless rendering MUST use the existing video provider.
**Verification**: Test

### MEDIA-VIDEO-13 VMR video outputs

**Rule**: VMR basic-video getters MUST validate required outputs before checking connection. VMR snapshots MUST retain the last RGB32 image across playback states and preserve a supplied size when copying pixels.
**Verification**: Test

### MEDIA-VIDEO-14 Renderless VMR ownership

**Rule**: Renderless VMR MUST retain the guest allocator-presenter and DirectDraw device and allocate surfaces during allocator configuration. Samples MUST populate those surfaces and invoke PrepareSurface followed by PresentImage with their timestamps. Renderless VMR MUST NOT publish frames through the host video provider.
**Verification**: Test

Withdrawn: MEDIA-VIDEO-15
