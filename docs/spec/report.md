# Compatibility report

ID prefix: REPORT

This spec covers the anonymous compatibility report, schema `hitode.report/1`, that the launcher saves and later submits. `adv-report` defines the format, and the launcher assembles it in `apps/adv-launcher/src/lib/report.ts` and `src-tauri/src/report.rs`. Diagnostic detail stays in the local `--report` output, which this spec does not cover.

> Warning: a report leaves the user's machine. Every rule here keeps private data out of it.

## Format

### REPORT-FMT-1 Strict schema

**Rule**: A report MUST carry the schema `hitode.report/1`. Deserializing a report with unknown fields or another schema MUST fail. The launcher MUST validate a report this way before saving or submitting it.
**Verification**: Test

### REPORT-FMT-2 Shared wire format

**Rule**: The field names, order and values of `adv_report::Report` MUST match the types in `apps/adv-launcher/src/lib/report.ts`.
**Verification**: Test

### REPORT-FMT-3 The note

**Rule**: The user's note MUST be the only free text in a report, and it MUST NOT exceed 2000 characters.
**Verification**: Test

## Identity

### REPORT-ID-1 Game identity

**Rule**: A report MUST identify the game only by the SHA-256 and size of its main executable and by the guest code page. File names, paths and titles MUST NOT appear.
**Verification**: Test

### REPORT-ID-2 Deduplication key

**Rule**: The dedupe key MUST be derived from a per-installation secret and the executable hash. It MUST be stable for one game on one installation and reveal nothing else.
**Verification**: Test

### REPORT-ID-3 Installation secret

**Rule**: The installation secret MUST be random, created on first use and stored only in the app data folder. It MUST NOT appear in any report or leave the machine.
**Applies to**: Launcher
**Verification**: Review — the secret is read only by `installation_secret` in the launcher.

## Failure and performance

### REPORT-FAIL-1 Failure categories

**Rule**: A failure MUST carry only its kind (api, cpu or runtime), an unimplemented API name, a CPU fault kind and address, and a time bucket. Runtime messages, host paths and guest text MUST NOT enter the report.
**Verification**: Test

### REPORT-FAIL-2 Time buckets

**Rule**: The time until a failure MUST be reported only as one of the buckets `lt_1m`, `1m_10m`, `10m_1h` and `gt_1h`.
**Verification**: Test

### REPORT-FAIL-3 Host errors

**Rule**: A run that ends with a host-side error MUST still yield a summary, classified by REPORT-FAIL-1 from the error text and its elapsed time. Such a summary MUST NOT carry frame timing.
**Verification**: Test

### REPORT-GAP-1 API gaps

**Rule**: A summary MUST list the unimplemented APIs the run called, and those a loaded module imports but did not call. Each list MUST hold at most 256 sorted, distinct `module!name` entries in printable ASCII. Other entries MUST be dropped, and a report carrying one MUST fail validation.
**Verification**: Test

### REPORT-PERF-1 Frame time percentiles

**Rule**: Performance data MUST be the 50th, 95th and 99th percentiles of frame time by nearest rank, rounded to 0.1 ms. Individual frame times MUST NOT be reported.
**Verification**: Test

## Saving

### REPORT-SAVE-1 Saved file

**Rule**: The launcher MUST save a report to the user's download folder as `adv-report-YYYY-MM-DD.json`, adding `-N` when that name exists. Saving MUST NOT overwrite an existing file.
**Applies to**: Launcher
**Verification**: Review — `save_report` opens files with `create_new`, and no test covers the download folder.

Withdrawn: none
