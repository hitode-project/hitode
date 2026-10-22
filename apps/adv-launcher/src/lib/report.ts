/**
 * The anonymous compatibility report, mirroring the JSON the launcher sends
 * (schema "hitode.report/1"). Field names and order match the wire format so
 * the value can later come straight from the Rust adv-report crate.
 */

export const REPORT_SCHEMA = "hitode.report/1";

export type Outcome = "ok" | "issues" | "failed";
export type Issue = "crash" | "performance";

export const PERF_ISSUES = ["stutter", "fast_forward", "first_launch", "loading", "audio_dropout", "input_latency"] as const;
export type PerfIssue = (typeof PERF_ISSUES)[number];

/** How long the game ran before it failed. */
export type FailureAfter = "lt_1m" | "1m_10m" | "10m_1h" | "gt_1h";

export interface ReportApp {
  version: string;
  build: string;
  channel: string;
}

export interface ReportPlatform {
  /** "windows", "macos", "linux", "android", "ios". */
  os: string;
  os_major: string;
  arch: "x86_64" | "aarch64";
  cpu_engine: "x64" | "a64";
  graphics: string;
}

export interface ReportGame {
  exe_sha256: string;
  exe_size: number;
  /** Guest ANSI code page. */
  codepage: number;
}

export interface ReportFailure {
  /** "runtime": any other runtime failure; its message stays local. */
  kind: "api" | "cpu" | "runtime";
  /** The unimplemented or failing API, e.g. "d3d9.dll!IDirect3DDevice9::CreateVolumeTexture". */
  api: string | null;
  cpu_fault: string | null;
  after: FailureAfter;
}

export interface ReportPerf {
  frame_ms_p50: number;
  frame_ms_p95: number;
  frame_ms_p99: number;
}

/** Unimplemented APIs the run met, as sorted "module!name" lists. */
export interface ReportApiGaps {
  /** Called by the game; the runtime returned their failure and kept playing. */
  called: string[];
  /** Imported by a loaded module but not called. */
  imported: string[];
}

export interface Report {
  schema: typeof REPORT_SCHEMA;
  dedupe_key: string;
  app: ReportApp;
  platform: ReportPlatform;
  game: ReportGame;
  outcome: Outcome;
  issues: Issue[];
  /** Present only when the outcome is "issues" with a performance problem. */
  perf_issues?: PerfIssue[];
  note?: string;
  failure?: ReportFailure;
  perf?: ReportPerf;
  api_gaps?: ReportApiGaps;
}

/** What the user picked in the report form. */
export type Situation = "fail" | "crash" | "perf" | "ok";

/** The fields that identify the build, platform and game; never removable. */
export type ReportBase = Pick<Report, "dedupe_key" | "app" | "platform" | "game">;

export interface ReportInput {
  situation: Situation;
  perfIssues: PerfIssue[];
  /** Null when empty or removed by the user. */
  note: string | null;
  failure: ReportFailure | null;
  perf: ReportPerf | null;
  apiGaps: ReportApiGaps | null;
}

export function situationOutcome(situation: Situation): Outcome {
  if (situation === "ok") return "ok";
  return situation === "perf" ? "issues" : "failed";
}

/** Assembles the report in wire order. */
export function buildReport(base: ReportBase, input: ReportInput): Report {
  const { situation } = input;
  const report: Report = {
    schema: REPORT_SCHEMA,
    dedupe_key: base.dedupe_key,
    app: base.app,
    platform: base.platform,
    game: base.game,
    outcome: situationOutcome(situation),
    issues: situation === "crash" ? ["crash"] : situation === "perf" ? ["performance"] : [],
  };
  if (situation === "perf") report.perf_issues = PERF_ISSUES.filter((k) => input.perfIssues.includes(k));
  if (input.note) report.note = input.note;
  if (input.failure) report.failure = input.failure;
  if (input.perf) report.perf = input.perf;
  if (input.apiGaps) report.api_gaps = input.apiGaps;
  return report;
}
