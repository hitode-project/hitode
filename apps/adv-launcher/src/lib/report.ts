/** The /2 wire format mirrors adv_report::Report, including field order. */
export const REPORT_SCHEMA = "hitode.report/2";
export type Outcome = "ok" | "issues" | "failed";

/** Order and labels also define the issue template's symptom dropdown. */
export const ISSUES = {
  no_start: "Does not start / 无法启动",
  freeze: "Freezes / 卡死（画面不动、没有响应）",
  crash: "Crashes or quits / 闪退",
  stutter: "Stutters or runs slowly / 卡顿、掉帧",
  graphics: "Graphics wrong / 画面异常",
  sound_video: "Sound or video wrong / 声音或视频异常",
  input: "Input wrong / 操作异常",
  save: "Saving or loading wrong / 存档读档异常",
  heat: "Heat or battery drain / 发热、耗电异常",
  other: "Other / 其他",
} as const;
export type Issue = keyof typeof ISSUES;
export const PERF_ISSUES = ["stutter", "fast_forward", "first_launch", "loading", "audio_dropout", "input_latency"] as const;
export type PerfIssue = (typeof PERF_ISSUES)[number];
export type FailureAfter = "lt_1m" | "1m_10m" | "10m_1h" | "gt_1h";
export interface ReportApp { version: string; build: string; channel: string }
export interface ReportGraphics {
  api: "metal" | "vulkan";
  name: string;
  vendor: string;
  device: string;
  driver: string;
  tier: string;
}
export type Upscaler = "off" | "anime4k" | "cunny" | "artcnn" | "k7";
export type Scaling = "fit" | "integer" | "stretch";
export type FastForward = "hold" | "toggle";
export type TouchMode = "direct" | "trackpad";
export interface ReportSettings {
  upscaler?: Upscaler;
  scaling?: Scaling;
  fast_forward?: FastForward;
  touch_mode?: TouchMode;
  font_files: number;
  font_overrides: number;
  font_smoothing: boolean | null;
  discs: number;
}
export interface ReportPlatform {
  os: string;
  os_major: string;
  model: string;
  /** iOS: whether the signature raises the memory limit. */
  increased_memory_limit?: boolean;
  graphics: ReportGraphics;
  settings: ReportSettings;
}
export interface ReportGame {
  exe_sha256: string;
  exe_size: number;
  codepage: number;
  timestamp: number;
  image_size: number;
  file_version?: string;
}
export interface ReportTitle { vndb: string; name: string; edition: string }
export interface ReportFailure {
  kind: "api" | "cpu" | "runtime";
  api: string | null;
  cpu_fault: string | null;
  after: FailureAfter;
}
export interface ReportPerf { frame_ms_p50: number; frame_ms_p95: number; frame_ms_p99: number }
export interface ReportApiGaps { called: string[]; imported: string[] }
export interface ReportTimeline {
  window?: number;
  frame?: number;
  audio?: number;
  video?: number;
  input?: number;
  frames: number;
  last_frame?: number;
  end: number;
}
export interface Report {
  schema: typeof REPORT_SCHEMA;
  dedupe_key: string;
  app: ReportApp;
  platform: ReportPlatform;
  game: ReportGame;
  title?: ReportTitle;
  outcome: Outcome;
  issues: Issue[];
  perf_issues?: PerfIssue[];
  note?: string;
  failure?: ReportFailure;
  perf?: ReportPerf;
  api_gaps?: ReportApiGaps;
  timeline?: ReportTimeline;
}
export type Situation = Issue | "ok";
export type ReportBase = Pick<Report, "dedupe_key" | "app" | "platform" | "game">;
export interface ReportInput {
  situation: Situation;
  title: ReportTitle | null;
  keepTitle: boolean;
  perfIssues: PerfIssue[];
  note: string | null;
  failure: ReportFailure | null;
  perf: ReportPerf | null;
  apiGaps: ReportApiGaps | null;
  timeline: ReportTimeline | null;
}
export function situationOutcome(situation: Situation): Outcome {
  if (situation === "ok") return "ok";
  return ["no_start", "freeze", "crash"].includes(situation) ? "failed" : "issues";
}
/** Assemble in wire order; optional rows are omitted before delivery. */
export function buildReport(base: ReportBase, input: ReportInput): Report {
  const { situation } = input;
  const report: Report = {
    schema: REPORT_SCHEMA,
    dedupe_key: base.dedupe_key,
    app: base.app,
    platform: base.platform,
    game: base.game,
    ...(input.keepTitle && input.title ? { title: input.title } : {}),
    outcome: situationOutcome(situation),
    issues: situation === "ok" ? [] : [situation],
  };
  if (situation === "stutter") report.perf_issues = PERF_ISSUES.filter((key) => input.perfIssues.includes(key));
  if (input.note) report.note = input.note;
  if (input.failure) report.failure = input.failure;
  if (input.perf) report.perf = input.perf;
  if (input.apiGaps) report.api_gaps = input.apiGaps;
  if (input.timeline) report.timeline = input.timeline;
  return report;
}

/** Use GitHub's editable issue body: form dropdowns do not accept URL prefills. */
export function issueDraft(report: Report, json: string): { url: string; attachReport: boolean } {
  const symptoms = report.issues.map((issue) => ISSUES[issue]).join(", ") || "Runs normally / 能正常玩";
  const platform = { macos: "macOS", ios: "iOS / iPadOS", android: "Android" }[report.platform.os];
  const title = report.title ? `${report.title.name}: ${symptoms}` : symptoms;
  const page = "https://github.com/hitode-project/hitode/issues/new?";
  const issue = (details: string, attachment: string) => {
    const fields = [
      ["Game / 游戏", report.title ? `${report.title.name} (https://vndb.org/${report.title.vndb})` : ""],
      ["Patch / 补丁", report.title?.edition ?? ""],
      ["Platform / 平台", platform ?? report.platform.os],
      ["Device and Hitode version / 设备与 Hitode 版本", `${report.platform.model} · Hitode ${report.app.version} (${report.app.build})`],
      ["Symptom / 现象", symptoms],
      ["What happened / 发生了什么", details],
      ["Report / 报告", attachment],
    ];
    const body = fields.map(([label, value]) => `### ${label}\n\n${value}`).join("\n\n");
    return `${page}${new URLSearchParams({ title, body })}`;
  };
  const note = report.note ?? "";
  const limit = 8000;
  const full = issue(note, `\`\`\`json\n${json}\n\`\`\``);
  if (full.length <= limit) return { url: full, attachReport: false };

  const attachment = "Attach the saved report JSON. / 请附上已保存的报告 JSON。";
  const withAttachment = issue(note, attachment);
  if (withAttachment.length <= limit) return { url: withAttachment, attachReport: true };

  const points = Array.from(note);
  const suffix = "…\n\nFull description in the saved report. / 完整描述见已保存的报告。";
  const shortened = (n: number) => issue(points.slice(0, n).join("") + suffix, attachment);
  let low = 0;
  let high = points.length;
  while (low < high) {
    const middle = Math.ceil((low + high) / 2);
    if (shortened(middle).length <= limit) low = middle;
    else high = middle - 1;
  }
  return { url: shortened(low), attachReport: true };
}
