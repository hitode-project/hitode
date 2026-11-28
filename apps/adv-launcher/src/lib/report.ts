/** The /3 wire format mirrors adv_report::Report, including field order. */
export const REPORT_SCHEMA = "hitode.report/3";
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
export type StopReason = "live_snapshot" | "guest_exit" | "guest_failure" | "host_error" | "panic" | "user_close" | "user_force_close";
export type FailureKind = "api" | "nt_unsupported" | "missing_dll" | "cpu" | "host_error" | "host_panic" | "runtime";
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
export type Scaling = "fit" | "integer" | "stretch" | "custom";
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
  /** iOS: the address space the launch reservation saw. */
  address_space?: ReportAddressSpace;
  graphics: ReportGraphics;
  settings: ReportSettings;
}
/** Host addresses as `0x` hex; `mapped` only when no window fit. */
export interface ReportAddressSpace {
  max: string;
  window?: string;
  mapped?: string[];
}
export interface ReportGame {
  exe_sha256: string;
  exe_size: number;
  codepage: number;
  timestamp: number;
  image_size: number;
  file_version?: string;
  arch?: "x86" | "x64" | "other";
  subsystem?: "gui" | "cui" | "other";
  sections?: number;
  overlay_bytes?: number;
  system_imports?: string[];
}
export interface ReportTitle { vndb: string; name: string; edition: string }
export interface ReportRun {
  jit: boolean;
  stop: StopReason;
  exit_code?: string;
  processes: number;
  failed_is_child?: boolean;
  dialogs_shown?: number;
  present_size?: [number, number];
}
export interface ReportHostPanic {
  thread: string;
  location?: string;
  reason?: string;
  translating?: string;
}
export interface ReportFailure {
  kind: FailureKind;
  api?: string | null;
  source?: string;
  op?: string;
  code?: string;
  dlls?: string[];
  cpu_fault?: string | null;
  fault_insn?: string;
  stack?: string[];
  panic?: ReportHostPanic;
  after: FailureAfter;
}
export interface ReportThreadSnapshot {
  tid: number;
  state: string;
  blocked_api?: string;
  stack?: string[];
}
export interface ReportBoundaryStat {
  kind: string;
  name: string;
  calls: number;
  host_ms: number;
}
export interface ReportHang {
  exec_thread_responsive: boolean;
  idle_wait_pct?: number;
  threads?: ReportThreadSnapshot[];
  top_boundaries?: ReportBoundaryStat[];
}
export interface ReportDiagnostics {
  warnings?: { api: string; reason: string; count: number }[];
  rejected_probes?: { api: string; detail: string; count: number }[];
  wine_errors?: { level: string; channel: string; func: string; count: number }[];
  hooked_exports?: { api: string; form: string }[];
  exceptions_seen?: { code: string; count: number; last_pc?: string }[];
  graphics?: { fronts?: string[]; d3d9_draws?: number; d3d9_readback_kb?: number };
  audio_video?: { audio_fronts?: string[]; video_fronts?: string[]; dsound_create_failures?: Record<string, number> };
  fonts?: { missing_standard?: string[]; substitutions?: number; charsets?: number[]; gdi_codepages?: number[] };
  windows?: { top_level: number; visible: number; dialogs?: number };
  host_panics?: ReportHostPanic[];
}
export interface ReportCpuShare {
  translated_pct: number;
  apply_pct: number;
  interpret_pct: number;
  interpreter_pct: number;
  rep_pct: number;
}
export interface ReportPerf {
  frame_ms_p50: number;
  frame_ms_p95: number;
  frame_ms_p99: number;
  long_frames_50ms?: number;
  long_frames_250ms?: number;
  cpu_share?: ReportCpuShare;
  top_cliffs?: { name: string; count: number }[];
  peak_heap_mb?: number;
}
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
  run?: ReportRun;
  failure?: ReportFailure;
  hang?: ReportHang;
  diagnostics?: ReportDiagnostics;
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
  run: ReportRun | null;
  failure: ReportFailure | null;
  hang: ReportHang | null;
  diagnostics: ReportDiagnostics | null;
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
  if (input.run) report.run = input.run;
  if (input.failure) report.failure = input.failure;
  if (input.hang) report.hang = input.hang;
  if (input.diagnostics) report.diagnostics = input.diagnostics;
  if (input.perf) report.perf = input.perf;
  if (input.apiGaps) report.api_gaps = input.apiGaps;
  if (input.timeline) report.timeline = input.timeline;
  return report;
}

/** Compact Markdown triage summary embedded when full JSON exceeds the URL limit. */
export function triageSummary(report: Report): string {
  const lines: string[] = [];
  if (report.run) {
    const bits = [`stop=${report.run.stop}`, `jit=${report.run.jit}`, `procs=${report.run.processes}`];
    if (report.run.exit_code) bits.push(`exit=${report.run.exit_code}`);
    if (report.run.dialogs_shown) bits.push(`dialogs=${report.run.dialogs_shown}`);
    lines.push(`- Run: ${bits.join(", ")}`);
  }
  if (report.timeline) {
    lines.push(`- Timeline: frames=${report.timeline.frames}, end=${report.timeline.end}ms`);
  }
  if (report.failure) {
    const f = report.failure;
    const bits = [`kind=${f.kind}`];
    if (f.api) bits.push(`api=${f.api}`);
    if (f.source) bits.push(`source=${f.source}`);
    if (f.op) bits.push(`op=${f.op}`);
    if (f.code) bits.push(`code=${f.code}`);
    if (f.dlls?.length) bits.push(`dlls=${f.dlls.join(",")}`);
    if (f.cpu_fault) bits.push(`fault=${f.cpu_fault}`);
    if (f.fault_insn) bits.push(`insn=${f.fault_insn}`);
    if (f.panic) bits.push(`panic=${f.panic.location ?? f.panic.thread}`);
    lines.push(`- Failure: ${bits.join(" ")}`);
    if (f.stack?.length) lines.push(`- Stack: ${f.stack.slice(0, 6).join(" <- ")}`);
  }
  if (report.hang) {
    const bits = [`responsive=${report.hang.exec_thread_responsive}`];
    if (report.hang.idle_wait_pct !== undefined) bits.push(`idle=${report.hang.idle_wait_pct}%`);
    const top = report.hang.threads?.[0];
    if (top) {
      bits.push(`t0=${top.state}${top.blocked_api ? `@${top.blocked_api}` : ""}`);
    }
    lines.push(`- Hang: ${bits.join(", ")}`);
  }
  if (report.diagnostics) {
    const d = report.diagnostics;
    if (d.rejected_probes?.length) {
      lines.push(`- Probes: ${d.rejected_probes.slice(0, 3).map((p) => `${p.api}(${p.detail})x${p.count}`).join("; ")}`);
    }
    if (d.warnings?.length) {
      lines.push(`- Warnings: ${d.warnings.slice(0, 3).map((w) => `${w.api}: ${w.reason}`).join("; ")}`);
    }
    if (d.hooked_exports?.length) {
      lines.push(`- Hooks: ${d.hooked_exports.slice(0, 4).map((h) => `${h.api}(${h.form})`).join(", ")}`);
    }
    if (d.wine_errors?.length) {
      lines.push(`- Wine: ${d.wine_errors.slice(0, 3).map((e) => `${e.level}:${e.channel}:${e.func}x${e.count}`).join(", ")}`);
    }
    if (d.fonts?.missing_standard?.length) {
      lines.push(`- Missing fonts: ${d.fonts.missing_standard.join(", ")}`);
    }
  }
  if (report.perf) {
    const bits = [`p50=${report.perf.frame_ms_p50}ms`, `p95=${report.perf.frame_ms_p95}ms`];
    if (report.perf.cpu_share) bits.push(`jit=${report.perf.cpu_share.translated_pct}%`);
    const topCliff = report.perf.top_cliffs?.[0];
    if (topCliff) bits.push(`cliff=${topCliff.name}`);
    lines.push(`- Perf: ${bits.join(", ")}`);
  }
  return lines.join("\n");
}

function compactJson(json: string): string | null {
  try {
    return JSON.stringify(JSON.parse(json));
  } catch {
    return null;
  }
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

  const compact = compactJson(json);
  if (compact && compact.length < json.length) {
    const dense = issue(note, `\`\`\`json\n${compact}\n\`\`\``);
    if (dense.length <= limit) return { url: dense, attachReport: false };
  }

  const baseAttachment = "Paste the copied report JSON here or attach the saved file. / 在此直接粘贴已复制的报告 JSON，或附上已保存的文件。";
  const summary = triageSummary(report);
  const richAttachment = summary ? `${summary}\n\n${baseAttachment}` : baseAttachment;
  const withSummary = issue(note, richAttachment);
  if (withSummary.length <= limit) return { url: withSummary, attachReport: true };

  const withAttachment = issue(note, baseAttachment);
  if (withAttachment.length <= limit) return { url: withAttachment, attachReport: true };

  const points = Array.from(note);
  const suffix = "…\n\nFull description in the saved report. / 完整描述见已保存的报告。";
  const shortened = (n: number) => issue(points.slice(0, n).join("") + suffix, baseAttachment);
  let low = 0;
  let high = points.length;
  while (low < high) {
    const middle = Math.ceil((low + high) / 2);
    if (shortened(middle).length <= limit) low = middle;
    else high = middle - 1;
  }
  return { url: shortened(low), attachReport: true };
}
