# Compatibility report

ID prefix: REPORT

This spec covers the compatibility report, schema `hitode.report/2`, that the launcher saves or opens in an issue. `adv-report` defines the format, and the launcher assembles it in `apps/adv-launcher/src/lib/report.ts` and `src-tauri/src/report.rs`. Diagnostic detail stays in the local `--report` output, which this spec does not cover.

> Warning: a report leaves the user's machine. Every rule here keeps private data out of it.

## Format

### REPORT-FMT-1 Strict schema

**Rule**: A report MUST carry the schema `hitode.report/2`. Deserializing a report with unknown fields or another schema MUST fail. The launcher MUST validate a report this way before saving or opening an issue.
**Verification**: Test

### REPORT-FMT-2 Shared wire format

**Rule**: The field names, order and values of `adv_report::Report` MUST match the types in `apps/adv-launcher/src/lib/report.ts`.
**Verification**: Test

### REPORT-FMT-3 Player text

**Rule**: The note and edition MUST be the only player-entered free text. The note MUST contain at most 2000 characters, and edition at most 80 characters.
**Verification**: Test

## Identity

### REPORT-ID-1 Game identity

**Rule**: Game identity MUST include executable SHA-256, file size, code page, PE timestamp and image size, plus file version when present. The title block MUST appear only when the player keeps its row. Game filenames and all paths MUST NOT appear.
**Verification**: Test

The title block holds the library match’s VNDB ID and name, plus the player’s edition. VNDB IDs match `v` followed by digits; names contain 1–160 characters without control characters. Only edition is editable in that row.

### REPORT-ID-2 Deduplication key

**Rule**: The dedupe key MUST be derived from a per-installation secret and the executable hash. It MUST be stable for one game on one installation and reveal nothing else.
**Verification**: Test

### REPORT-ID-3 Installation secret

**Rule**: The installation secret MUST be random, created on first use and stored only in the app data folder. It MUST NOT appear in any report or leave the machine.
**Applies to**: Launcher
**Verification**: Review — the secret is read only by `installation_secret` in the launcher.

## Failure and performance

### REPORT-FAIL-1 Failure categories

**Rule**: A failure MUST contain only its kind, API name or CPU fault site, and time bucket. Runtime messages, host paths and guest text MUST NOT enter the report.
**Verification**: Test

### REPORT-FAIL-2 Time buckets

**Rule**: The failure’s `after` field MUST use one of the buckets `lt_1m`, `1m_10m`, `10m_1h` and `gt_1h`.
**Verification**: Test

### REPORT-FAIL-3 Host errors

**Rule**: A host-side error MUST yield a summary classified by REPORT-FAIL-1 from terminal exception evidence and elapsed time. Errors without exception evidence MUST be runtime failures. Such summaries MUST NOT carry frame timing.
**Verification**: Test

### REPORT-GAP-1 API gaps

**Rule**: Each Wine stub raise MUST enter one session-owned set, shared by every process and the debugger. Reports MUST derive `called` and terminal API failures from this state, with `imported` empty on Wine. Lists MUST contain at most 256 sorted, distinct, printable ASCII `module!function` names.
**Verification**: Test

Both guest `EXCEPTION_WINE_STUB` records and host syscall stubs feed the same set. Report construction lowercases module names and drops invalid entries; report validation rejects invalid names. Legacy ABI stubs retain their existing text classification while they still produce that diagnostic.

### REPORT-PERF-1 Frame time percentiles

**Rule**: Performance data MUST be the 50th, 95th and 99th percentiles of frame time by nearest rank, rounded to 0.1 ms. Individual frame times MUST NOT be reported.
**Verification**: Test

### REPORT-SITE-1 Crash sites

**Rule**: CPU failures MUST name the faulting PC and a cheaply available return address as `module+0xoffset`. Modules MUST be labeled `exe`, their Wine distribution filename, or `game#` followed by eight source-file SHA-256 hex digits. Absolute code addresses MUST appear only outside mapped modules.
**Verification**: Test

Game modules are hashed only while constructing a failure report. When source bytes cannot be read, their label is `game` without a hash. Access addresses use the same formatter. A saved frame-pointer caller is included when readable inside the guest stack.

### REPORT-TIME-1 Run timeline

**Rule**: Timelines MUST record first window, frame, audio, video and input milestones once, in milliseconds since guest startup. Missing milestones MUST be absent. Timelines MUST include presented frame count, last frame time when present, and run duration.
**Verification**: Test

Window means a top-level window shown; audio means rendered non-silent audio; video means a published video frame; input means hardware input delivered to Wine. Frame timestamps come from the shell’s existing frame observations and survive diagnostic metric resets. `end` excludes report construction and file output.

### REPORT-PLAT-1 Platform and settings

**Rule**: Platform MUST include OS major version and hardware model identifier. Graphics MUST contain API, adapter name, vendor, device, driver and selected binding tier from the GPU device. Settings MUST record launcher choices that select host execution paths, without file paths or font names.
**Verification**: Test

Apple model identifiers use `hw.model` on macOS and `hw.machine` on iOS; Android uses `ro.product.model`. OS versions use `kern.osproductversion` on Apple and `ro.build.version.release` on Android. Metal uses its adapter model as device identity, never its registry ID; Vulkan uses vendor and device IDs. Tiers are `metal`, `bindless`, `push_descriptors` or `descriptor_sets`.

The summary builds its typed settings once from the end-of-run display mailbox, game view and options. It holds upscaler, scaling, fast-forward mode, custom-font file and override counts, font smoothing and disc count. Headless runs omit display and game-view settings. The report uses this snapshot and adds the frontend’s current touch mode. Reports without a run use current launcher settings.

Choices are `off|anime4k|cunny|artcnn|k7` for upscaling, `fit|integer|stretch` for scaling, `hold|toggle` for fast-forward, and `direct|trackpad` for touch mode.

### REPORT-ISSUE-1 Player symptoms

**Rule**: Report issues and the issue template MUST share the symptom enum. Performance details MUST appear only for `stutter`.
**Verification**: Test

Symptoms are `no_start`, `freeze`, `crash`, `stutter`, `graphics`, `sound_video`, `input`, `save`, `heat`, and `other`.

## Saving

### REPORT-SAVE-1 Saved file

**Rule**: The launcher MUST save a report to the user's download folder as `adv-report-YYYY-MM-DD.json`, adding `-N` when that name exists. Saving MUST NOT overwrite an existing file.
**Applies to**: Launcher
**Verification**: Review — `save_report` opens files with `create_new`, and no test covers the download folder.

### REPORT-SAVE-2 Issue delivery

**Rule**: Opening an issue MUST prefill its title and body from the validated report, including the symptoms and any included note. If the encoded URL exceeds 8000 characters, the launcher MUST save the full JSON under REPORT-SAVE-1 and keep the draft within the limit. The button label MUST remain the same for both cases.
**Verification**: Test

The launcher opens an editable issue draft through GitHub's `title` and `body` URL parameters. Short drafts include the JSON in the body. For longer drafts, the player attaches the saved file. The manual issue form is `game-issue.yml`.

On Android and iOS, the launcher MUST open the issue URL in a browser rather than handing it to the GitHub app.

### REPORT-SAVE-3 Mobile sharing

**Rule**: The launcher MUST let the user open the system share sheet for a saved report. Only a validated report file in the download directory MUST be passed to the sheet.
**Applies to**: Android, iOS
**Verification**: Review — the platform share sheet requires the mobile app.

Withdrawn: none
