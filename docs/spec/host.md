# Host shells

ID prefix: HOST

This spec covers the host shells. It includes launcher overlay input, the game view, sessions, macOS lifecycle and packaging and host persistence. The Android player has its own spec, [android.md](android.md). `adv-shell` implements the shared parts, and `apps/adv-launcher` and `apps/adv-run` are the entry points. Devices, fonts and media have their own specs.

## Overlay input

### HOST-OVL-1 Overlay message

**Rule**: The overlay MUST publish its input state through `set_overlay_input` as regions, `modal`, `wantsKeyboard` and `visible`. Regions MUST be rectangles in webview logical pixels with a top-left origin. Messages with unknown fields MUST be rejected, and empty regions MUST be dropped.
**Applies to**: Launcher
**Verification**: Test

### HOST-OVL-2 Press routing

**Rule**: A press MUST go to the webview when it falls inside a region or the overlay is modal. Every other press MUST go to the game. Region edges MUST be half-open.
**Verification**: Test

### HOST-OVL-3 Press capture

**Rule**: A press MUST keep its target until its release, even if a modal opens under it. A repeated press without a release MUST be routed again. A release without a known press MUST be routed by the regions.
**Verification**: Test

### HOST-OVL-4 Multiple pointers

**Rule**: Each pointer MUST be routed on its own.
**Applies to**: Hosts other than Android, which routes whole gestures under AND-APP-12
**Verification**: Test

### HOST-OVL-5 Keyboard

**Rule**: Keyboard input MUST go to the webview when `wantsKeyboard` is set and to the game otherwise.
**Verification**: Test

### HOST-OVL-6 Exclusive outside the game

**Rule**: `OverlayState` MUST start exclusive, so the webview owns all input until a game view is shown. It MUST notify the native view only when the input state changes.
**Applies to**: Launcher
**Verification**: Test

### HOST-OVL-7 Pause without Esc

**Rule**: The overlay MUST NOT intercept Esc, because games use it. On macOS the application menu item Pause (⌘.) MUST send a `pause` event to the webview.
**Applies to**: Launcher
**Verification**: Review — the menu item lives in the Tauri setup, which no test runs.

### HOST-OVL-8 Game title bar

**Rule**: On macOS the game screen MUST make the window and title bar opaque and show the game's title above the game. Its pause button MUST send the same `pause` event, and the overlay MUST NOT float one over the game. Leaving the game screen MUST restore the transparent window and title bar.
**Applies to**: Launcher, macOS
**Verification**: Review — AppKit title bars need a headed window, which no test runs.

### HOST-OVL-9 Hidden overlay

**Rule**: On macOS the webview MUST be hidden over a running game while `visible` is false, and shown otherwise. The host MUST show it before emitting `pause` and when the game ends. Changes published before that `pause` MUST NOT hide it again.
**Applies to**: Launcher, macOS
**Verification**: Test

`visible` is true while the overlay has a region, a modal layer or other content such as the FPS readout. A hidden WKWebView leaves the window's composition and runs no animation frames, so it cannot publish the change that would show it again.

## Game view

### HOST-VIEW-1 Drawable size and letterbox

**Rule**: The native frame size MUST use physical pixels: content points multiplied by `backingScaleFactor` on macOS. Native GPU presentation MUST default to preserving aspect ratio and black bars, using sharp-bilinear sampling unless the host selects an upscaler. Pointer positions MUST map back through the same letterbox.
**Verification**: Test

### HOST-VIEW-2 Retained output

**Rule**: Composing a frame MUST keep the previous output until commit. Retained pixels MUST survive reuse of the caller's buffer and invalid frames. A repaint without a new guest frame MUST show the retained pixels.
**Verification**: Test

### HOST-VIEW-3 One composition path

**Rule**: Software frames, GPU surfaces and video MUST share one background composition path. Composition MUST preserve encoded colors, write opaque alpha and keep the letterbox black.
**Verification**: Test

### HOST-VIEW-4 Surface configuration off the UI thread

**Rule**: On macOS the surface MUST be configured only by background tasks, at startup or after a submitted pause. The UI thread MUST only read the size and schedule work.
**Applies to**: macOS
**Verification**: Review — the thread affinity of AppKit callbacks is not observable in a test.

### HOST-VIEW-5 Key translation

**Rule**: Physical keys MUST be delivered as US virtual keys with set 1 scan codes, and no two key positions MUST share a scan code. Key messages MUST follow the Win32 `lParam` layout.
**Verification**: Test

### HOST-VIEW-6 Embedded window sizing

**Rule**: A windowed game container MUST fit the content area to the game canvas aspect ratio when mounting or changing canvas dimensions. Removing a windowed game MUST restore the previous window frame. Full-screen presentation MUST retain aspect-preserving letterboxing.
**Applies to**: macOS
**Verification**: Test

### HOST-VIEW-7 Spatial upscaling

**Rule**: Selected upscalers MUST process the composed game frame on the host GPU without modifying guest pixels. Software, GPU and video inputs MUST share the same filter. Native-size and downscaled output MUST bypass super-resolution.
**Verification**: Test

Models whose upstream source declares FP16 compute in half precision when the GPU offers shader f16. Otherwise their passes run in single precision with the same f16-rounded weights.

Optimized kernels MUST retain the selected model, weights and arithmetic precision. Final 8-bit display channels MUST differ by at most 2/255 from the unfused model. Adjacent layers share quantized workgroup storage; no intermediate tensor precision is discarded. Devices offering half-precision 8×8 cooperative matrices use tiled RGB input convolutions for Anime4K and k7; their Metal backend uses SIMD-group matrices. CuNNy keeps its vector input convolution and 8-bit intermediate storage, fusing only the final intermediate pair. ArtCNN keeps its FP32 graph.

### HOST-VIEW-8 Upscaling preferences

**Rule**: The launcher MUST persist a global upscaling choice and per-game overrides, including an explicit off choice. Games without overrides MUST inherit the global choice. Changes during play MUST apply without restarting the guest.
**Applies to**: Launcher, macOS
**Verification**: Review — persistence and live switching require the launcher webview and a running native game session.

## Session

### HOST-VIEW-9 Compositor scaling

**Rule**: Without upscaling or composed video, the frame layer MUST occupy the viewport over a black base and scale its image. Exact integer scales MUST keep the game resolution under nearest sampling. Other scales MUST prescale by the integer part per axis and leave the rest to linear filtering.
**Applies to**: macOS, iOS
**Verification**: Test

Nearest sampling at integer scales and an integer nearest prescale followed by bilinear filtering both define sharp-bilinear, so the look does not change. An upscaler's doubled output is magnified by the layer when the viewport can hold it, and otherwise resampled to the viewport's size; both are linear, as before. With composed video, the drawable covers the whole view and the frame is composed into it as before.

### HOST-VIEW-10 Damage-limited upscaling

**Rule**: When a frame's damage since the last upscaled frame is known, the upscaler MUST recompute only pixels that damage reaches, matching a whole-frame run. Each pass MUST read only values computed in the same run or left valid by earlier runs. Rebuilds, composed video and unknown damage MUST run on the whole frame.
**Verification**: Test

Every layer is a 3×3 convolution, so a change reaches at most one input pixel per layer plus one for the final bilinear sample. Fused passes retain the sum of their layer radii. Intermediate textures are shared between tensors, so the last pass covers the full reach. Each earlier pass covers the next pass’s workgroup-rounded area plus its radius.

| Frame source | Damage since the last upscaled frame |
|---|---|
| Software | The snapshot's damage, when the previous consumed frame was upscaled |
| D3D9 rotating Present | Union of the recorded changes of each publication since the last upscaled one |
| D3D9 copy Present, another surface, more than eight publications, or a skipped frame | Unknown |

### HOST-VIEW-11 Shared display targets

**Rule**: Metal display targets MAY use IOSurface storage shared with a native layer. Layers MUST receive frames after GPU completion and retain them until the system compositor releases their surfaces. Direct presentation MUST preserve opaque RGB, color space, scaling and frame cadence.
**Applies to**: macOS
**Verification**: Test

Direct presentation requires Core Animation's `contentsOpaque` capability. Other presentations use the drawable compositor. Display callbacks reclaim retired surfaces; a temporary timer also drains pending retirements when the frame clock is idle or occluded.
### HOST-SES-1 Embedding

**Rule**: Embedded sessions MUST use the shared shell execution and background services. A session MUST NOT take over the host `NSApplication` delegate or the host window.
**Applies to**: macOS
**Verification**: Review — no test can start AppKit on the main thread.

### HOST-SES-12 Player execution lifetime

**Rule**: The player MUST run at most one Wine session on its persistent execution owner. Idle execution MUST block without polling. Bundled Wine files MUST be prepared off the UI thread before game startup, without selecting the built-in stack when unavailable.
Preparation MUST validate the compressed stream completely before publishing the installation's ready marker.
**Verification**: Test

### HOST-SES-2 Wakeups

**Rule**: Real input MUST wake guest execution. Repaints and telemetry MUST NOT wake it. The tick deadline MUST be armed only for a pending refresh of a visible view.
**Verification**: Test

### HOST-SES-3 Input capacity

**Rule**: Saturated telemetry MUST NOT consume the capacity reserved for input, and dropped telemetry MUST be counted. Menu events MUST coalesce and use the existing input channel.
**Verification**: Test

### HOST-SES-4 Posted text

**Rule**: `post_text` MUST deliver text through the session input channel and MUST return the count the guest accepted.
**Verification**: Test

### HOST-SES-5 Finish callback

**Rule**: The finish callback MUST run once, on the main thread, with `Finished { exit_code, summary }`. It MUST wait for session execution, background services, guest files, and registry finalization.
**Applies to**: macOS
**Verification**: Review — no test can run the AppKit main loop.

### HOST-SES-6 Launcher game lifecycle

**Rule**: `start_game` MUST mount the game view on the main thread and MUST create at most one session. When the game ends, the launcher MUST remove the view and emit `game_finished` with the exit code or error and the summary.
**Applies to**: Launcher
**Verification**: Review — `game.rs` drives AppKit and has no tests.

### HOST-SES-7 Launcher pause

**Rule**: A paused session MUST NOT run guest code, MUST freeze guest time and MUST suspend audio output until it resumes. A close request MUST resume the session first so the guest can answer. System sleep MUST still suspend only audio output.
**Applies to**: Launcher, macOS
**Verification**: Test

## Desktop game library

### HOST-SES-8 Presentation rate

**Rule**: `Present` and software frames MUST be presented at most once per 1/60 s on displays faster than 60 Hz, with at most 10% slack. Software frames rely on DEV-FRAME-8 boundaries for this cap. Slower displays MUST cap presentation at their own rate.
**Applies to**: macOS, Android
**Verification**: Test

### HOST-SES-10 Submit before refresh

**Rule**: Published frames MUST schedule upload, scaling and presentation outside guest execution as soon as the presentation cap permits. Display links MUST publish refresh timing without gating submission or running without pending content. Hosts without display links MUST retain deadline timers.
**Verification**: Test

### HOST-SES-11 Requested display cadence

**Rule**: Display rate requests MUST follow publication timestamps, accounting for mailbox drops and excluding repaints. Supported Apple display links and Android native surfaces MUST receive the estimated guest cadence, capped at 60 Hz. Android surfaces MAY replace AND-APP-10's initial 60 Hz request after measuring cadence.
**Verification**: Test

### HOST-LIB-1 Storage and removal

**Rule**: The version 1 library MUST use camelCase fields in `<app_data_dir>/library.json`, atomically replaced through `library.json.tmp`, and retain local-date `daily` totals permanently. Corrupt files MUST be renamed to `library.json.bad-<unix ms>` and logged, then startup MUST use an empty library. Removal MUST leave game files and saves intact, and old libraries lacking `daily` MUST backfill sessions once.
**Applies to**: Launcher, desktop and Android
**Verification**: Test

### HOST-LIB-2 Executable candidates

**Rule**: Top-level `.exe` candidates, all listed in scan results, MUST use case-insensitive extensions, decreasing size, automatic code pages and Machine mappings `0x14c=x86`, `0x8664=x64`, otherwise `other`. Suggestion MUST select the largest x86 excluding stems starting with `setup`, `install`, `unins` or `config`, NSIS or mostly-appended-payload packages and console programs, or null. Candidate and scan `inLibrary` MUST match canonical folder and case-insensitive exe, while inspection-level `inLibrary` MUST match any entry in the folder.
**Applies to**: Launcher, desktop and Android
**Verification**: Test

### HOST-LIB-3 Launch and play sessions

**Rule**: `start_game` MUST resolve the id's executable, code page and independent `<app_data_dir>/games/<id>` directory. Completion MUST update `lastPlayed` and persist nullable `lastRun` with end time, summary and any error. The library MUST be saved before emitting `game_finished`, whose exit and error payloads MUST include `gameId`.
**Applies to**: Launcher, macOS
**Verification**: Test

### HOST-LIB-4 Stop game

**Rule**: `stop_game` MUST first request normal session closure, then force closure if another request remains pending. With no running session it MUST return `no game is running`. Completion MUST use the existing finish callback and `game_finished` event without quitting the launcher.
**Applies to**: Launcher, macOS
**Verification**: Review — the command runs on the AppKit main thread and requires a native session.

### HOST-LIB-5 Drive C:

**Rule**: Games MUST write their own folders in place, and every game MUST share one Wine prefix in the app data directory. The launcher MUST open its drive C:, which holds what games write elsewhere, in the system file manager. iOS MUST keep drive C: in the app's Documents, and Android MUST serve it through a documents provider.
**Applies to**: Launcher
**Verification**: Review — the file managers run on a device.

### HOST-LIB-6 Playtime tracking

**Rule**: With playtime tracking enabled, completion MUST accumulate `playSeconds`, retain 30-day sessions and split `daily` at local midnights. Disabled tracking MUST preserve existing statistics without adding sessions, duration or daily totals. Tracking preferences MUST persist across launcher restarts.
**Applies to**: Launcher, macOS
**Verification**: Test

### HOST-RUN-1 Presentation controls

**Rule**: Host volume MUST scale device output without modifying source samples. Fit, integer and stretch modes MUST use the same physical-pixel rectangle for presentation and pointer mapping. Integer mode and the command-line window MUST show each game pixel as the most whole device pixels that fit, else shrink proportionally.
**Applies to**: Launcher, macOS
**Verification**: Test

### HOST-RUN-2 Toggle fast-forward

**Rule**: Toggle mode MUST alternate guest Ctrl presses and releases on physical Ctrl presses, ignoring physical releases. Both Ctrl keys MUST toggle the same guest key. Losing focus or changing modes MUST release latched input.
**Applies to**: Launcher, macOS
**Verification**: Test

### HOST-VNDB-1 Metadata association

**Rule**: VNDB links MUST preserve game files, identifiers and display names unless the user explicitly edits the display name. Import MUST list candidates, default to no link and link only the work the user chooses. Links MUST resolve only visual novel identifiers on VNDB.
**Applies to**: Launcher, desktop
**Verification**: Test

### HOST-VNDB-2 Lookup consent

**Rule**: The launcher MUST NOT send an automatic VNDB search at import before the user allows it in a one-time prompt. Dismissing the prompt MUST leave search manual, and the prompt MUST say that only game names are sent.
**Applies to**: Launcher, desktop
**Verification**: Review — the launcher frontend has no test runner.

### HOST-UPDATE-1 Update check

**Rule**: The launcher MUST check for updates only on request from phone or sidebar Settings, both showing the installed version. The check MUST read only the latest hitode-project release and compare its numeric version. External links MUST open only VNDB and hitode-project pages.
**Applies to**: Launcher
**Verification**: Test

## Menus and dialogs

### HOST-MENU-1 Native menu projection

**Rule**: Native menu titles MUST drop mnemonics and the accelerator column. A native menu MUST preserve the window content size and its specific guest target. A hidden popup MUST NOT detach the menu bar.
**Verification**: Test

### HOST-MENU-2 Native dialog projection

**Rule**: A native dialog MUST project guest state without feeding its own changes back, and MUST keep the guest control identity.
**Verification**: Test

### HOST-MENU-3 macOS placement

**Rule**: Guest top-level menus MUST be inserted after the application menu and removed when the session ends. A dialog MUST be a child window of the game view's window.
**Applies to**: macOS
**Verification**: Review — the main menu and child windows need a running AppKit application.

## macOS lifecycle

### HOST-LIFE-1 Close and quit

**Rule**: Closing the window or the first quit MUST post `WM_CLOSE` and wait for the guest without a timeout. `applicationShouldTerminate:` MUST return `NSTerminateLater` and reply only after the session finishes.
**Applies to**: macOS
**Verification**: Review — `NSApplication` termination cannot run in a test.

### HOST-LIFE-2 Forced quit

**Rule**: Only a second quit request MAY abort guest execution. A forced quit MUST still wait for execution thread cleanup.
**Applies to**: macOS
**Verification**: Review — `NSApplication` termination cannot run in a test.

### HOST-LIFE-3 Log out and power off

**Rule**: The workspace power-off notification MUST post `WM_QUERYENDSESSION` and then `WM_ENDSESSION`. The host MUST NOT promise that the guest finishes, because the system MAY end the process.
**Applies to**: macOS
**Verification**: Review — workspace notifications cannot be sent in a test.

### HOST-LIFE-4 Activation and focus

**Rule**: Application activation MUST post `WM_ACTIVATEAPP`. Losing key status MUST lose guest focus unless key moved to a guest dialog. Minimizing and restoring MUST post `WM_SIZE`, and a guest minimizing the displayed window MUST minimize the host window, also on Windows.
**Applies to**: macOS
**Verification**: Review — AppKit notifications cannot be sent in a test.

### HOST-LIFE-5 Background

**Rule**: Hiding, minimizing and occlusion MUST stop only presentation. The host MUST NOT pause the guest in the background. The App Nap activity MUST cover the player window lifetime and MUST allow system sleep.
**Applies to**: macOS
**Verification**: Review — occlusion and App Nap cannot be driven in a test.

### HOST-LIFE-6 Sleep

**Rule**: The host MUST release the audio stream before system sleep and rebuild it after wake. The default device MUST be compared by its CoreAudio identifier.
**Applies to**: macOS
**Verification**: Review — system sleep cannot be driven in a test.

### HOST-LIFE-7 Launcher quit

**Rule**: The launcher MUST replace the predefined Quit item with one that calls `AppHandle::exit`, because tao does not intercept `terminate:`. While a game runs, Quit and the window's close button MUST end only the game, and a second use while it closes MUST force it. After a game starts, the launcher MUST handle the Apple quit event, which quits once the game has closed.
**Applies to**: Launcher, macOS
**Verification**: Review — the Tauri menu and Apple events cannot run in a test.

## Persistence

### HOST-SAVE-1 Registry file

**Rule**: The host MUST write the registry to a temporary file, sync it and atomically replace the old file. A failed write MUST report the error and keep the old content.
**Verification**: Test

### HOST-SAVE-2 Guest file semantics

**Rule**: Guest saves MUST keep Windows per-write file semantics. The host MUST NOT add transactions to guest files or truncate the guest exit path.
**Verification**: Review — the rule forbids host behavior that no single test can enumerate.

### HOST-SAVE-3 Explicit flushes

**Rule**: On macOS, `FlushFileBuffers` and `NtFlushBuffersFile` MUST request `F_FULLFSYNC`. A same-volume rename with `MOVEFILE_WRITE_THROUGH` MUST sync the parent directory and then request `F_FULLFSYNC`. Other writes MUST NOT flush the drive cache.
**Applies to**: macOS
**Verification**: Review — no test can observe the drive cache flush.

### HOST-SAVE-4 MoveFileEx durability

**Rule**: With `MOVEFILE_WRITE_THROUGH`, a cross-volume move MUST flush the copy before deleting the source. A failed flush or a denied delete MUST keep the source.
**Verification**: Test

### HOST-SAVE-5 Copied installer state

**Rule**: A game folder holding an empty Silky's Plus `<exe>_inst.xml` template MUST get the installer's per-user state on first run. The filled file MUST name the guest game root and executable, and the `save` template MUST be copied beside it unless portable saves serve it.
Installer state MUST use the active stack's backing directory for the guest C drive.
**Verification**: Test

## macOS packaging

### HOST-PKG-1 FFmpeg build

**Rule**: The macOS release MUST link FFmpeg statically from source whose SHA-256 matches `tools/macos/ffmpeg.json`. The build MUST enable only the listed demuxers, decoders and parsers. Encoders, muxers, network, external library autodetection and GPL or nonfree components MUST be disabled.
**Applies to**: macOS
**Verification**: Test

### HOST-PKG-2 Bundle contents

**Rule**: The bundle MUST NOT contain dylibs outside the system. The minimum system version MUST come from the Mach-O deployment version of the runner.
**Applies to**: macOS
**Verification**: Test

### HOST-PKG-3 Code signing

**Rule**: The main program MUST be signed with Hardened Runtime and only the `com.apple.security.cs.allow-jit` entitlement. Library validation MUST stay enabled. An ad hoc signature MUST NOT be submitted for notarization.
**Applies to**: macOS
**Verification**: Test

### HOST-PKG-4 Licenses and sources

**Rule**: The bundle MUST include the project license, the FFmpeg LGPL and the license of every dependency, with source locations and checksums. A crate without its own license file MUST take the license from the exact published revision of its repository.
**Applies to**: macOS
**Verification**: Test

### HOST-PKG-5 Credentials

**Rule**: The Developer ID identity and the notarization profile MUST come only from the local environment and keychain. They MUST NOT enter the repository.
**Applies to**: macOS
**Verification**: Review — the privacy scan cannot recognize every credential.

### HOST-PKG-6 Published build paths

**Rule**: Shipped files MUST NOT contain developer home, checkout or temporary build paths, including native helpers and archive members.
**Verification**: Test

Withdrawn: HOST-AND-1, HOST-AND-2, HOST-AND-3, HOST-AND-4, HOST-AND-5, HOST-AND-6, HOST-AND-7, HOST-AND-8, HOST-AND-9, HOST-AND-10, HOST-AND-11, HOST-SES-9

### HOST-DAMAGE-1 Software texture uploads

**Rule**: The software GPU presenter MUST transfer only damaged rectangles with matching origins and row pitches, without comparing complete frames. New or resized textures MUST receive complete images, while repaint MUST preserve existing textures. Metal uploads MUST retain immutable, page-owned publication storage through completion and decode it identically to CPU snapshots.
**Verification**: Test

Metal uses no-copy buffers for publications without video overlays: BGRA samples buffer-backed textures directly, and other formats use integer GPU decoding. Other presentations reuse the consumer's BGRA snapshot.

### HOST-DAMAGE-2 Software work counters

**Rule**: Software frame counters MUST expose copied, CPU-converted, CPU-uploaded and GPU-transferred bytes, published frames and empty damage. Byte rates MUST use elapsed real time and exclude diagnostic reference work.
**Verification**: Test

### HOST-DAMAGE-3 Sample shared snapshots

**Rule**: Metal BGRA software presentation without video overlays MUST sample an immutable buffer-backed texture directly from snapshot storage. The texture MUST retain that storage through GPU use while guest writes and mailbox reuse continue independently.
**Verification**: Test
