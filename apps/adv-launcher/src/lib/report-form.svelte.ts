import { call } from "./bridge";
import { fakeReportBase, fakeSavedPath, fakeSubmit } from "./fake-report";
import { codepageLabel } from "./format";
import type { RunSummary } from "./host";
import { buildReport, PERF_ISSUES, type PerfIssue, type Report, type ReportBase, type ReportFailure, type ReportPerf, type Situation } from "./report";
import type { Game } from "./types";

// The report form shared by the desktop and phone report screens: what the
// user picked, the rows of "将发送的内容", the assembled report and the
// submit / save flow.

export type ReportView = "form" | "sending" | "sent" | "error" | "saved";
/** Rows the user may leave out of the report. */
export type Optional = "failure" | "perf" | "note";

export const SITUATIONS: [Situation, string][] = [
  ["fail", "无法启动"],
  ["crash", "中途出错"],
  ["perf", "能玩，但有性能问题"],
  ["ok", "能正常玩"],
];

export const ISSUE_LABELS: Record<PerfIssue, string> = {
  stutter: "卡顿",
  fast_forward: "快进时卡",
  first_launch: "首次启动慢",
  loading: "加载慢",
  audio_dropout: "声音断续",
  input_latency: "输入延迟",
};

/** What the report screen shows and sends besides the user's own choices. */
export interface ReportContext {
  base: ReportBase;
  /** Header line under the game title. */
  build: string;
  /** Read-only rows of "将发送的内容". */
  exeLabel: string;
  versionLabel: string;
  platformLabel: string;
  codepageLabel: string;
  failureLabel: string;
  /** From the last run this session; null when it recorded none. */
  failure: ReportFailure | null;
  perfLabel: string;
  perf: ReportPerf | null;
}

const OS_NAMES: Record<string, string> = { windows: "Windows", macos: "macOS", linux: "Linux", android: "Android", ios: "iOS" };
const NOT_RUN = "未运行过";

function failureLabel(summary: RunSummary | null | undefined): string {
  if (!summary) return NOT_RUN;
  const f = summary.failure;
  if (!f) return "没有记录到失败";
  if (f.kind === "api") return `未实现 · ${f.api ?? "未知接口"}`;
  if (f.kind === "cpu") return `CPU 异常 · ${f.cpu_fault ?? "未知"}`;
  return "运行时错误";
}

function perfLabel(summary: RunSummary | null | undefined): string {
  if (!summary) return NOT_RUN;
  const p = summary.perf;
  if (!p) return "没有帧时间记录";
  return `帧时间 p50 ${p.frame_ms_p50.toFixed(1)} ms · p95 ${p.frame_ms_p95.toFixed(1)} ms`;
}

/**
 * The report context of `game`: the host identifies the build, platform and
 * main program (report_base), and the failure and frame timing come from the
 * game's last run this session (`summary`; missing when it has not run).
 */
export async function loadReportContext(game: Game, summary: RunSummary | null | undefined, phone: boolean): Promise<ReportContext> {
  const base = await call<ReportBase>(
    "report_base",
    { exe: `${game.path}/${game.exe}`, codepage: game.codepage },
    fakeReportBase(game, phone),
  );
  const { app, platform } = base;
  const sha = base.game.exe_sha256;
  const platformName = `${OS_NAMES[platform.os] ?? platform.os} ${platform.os_major} · ${platform.arch === "aarch64" ? "arm64" : "x64"}`;
  return {
    base,
    build: `hitode ${app.version} · ${platformName}`,
    exeLabel: `sha256 ${sha.slice(0, 4)}…${sha.slice(-4)} · ${(base.game.exe_size / 1048576).toFixed(1)} MB`,
    versionLabel: `${app.version} (${app.build})`,
    platformLabel: platformName,
    codepageLabel: `${base.game.codepage} ${codepageLabel(base.game.codepage)}`,
    failureLabel: failureLabel(summary),
    failure: summary?.failure ?? null,
    perfLabel: perfLabel(summary),
    perf: summary?.perf ?? null,
  };
}

export interface ReportRow {
  id: string;
  label: string;
  value: string;
  /** Free text (the note) rather than a monospace value. */
  txt?: boolean;
  locked: boolean;
  optional?: Optional;
  /** The user left this row out. */
  removed: boolean;
}

export class ReportForm {
  view = $state<ReportView>("form");
  situation = $state<Situation>("fail");
  issues = $state<Partial<Record<PerfIssue, boolean>>>({});
  note = $state("");
  off = $state<Record<Optional, boolean>>({ failure: false, perf: false, note: false });
  rawOpen = $state(false);
  savedPath = $state("");
  /** Null while loading. */
  context = $state<ReportContext | null>(null);
  /** Why the context could not be loaded (e.g. the main program is gone). */
  loadError = $state("");

  #ctx = (): ReportContext | null => this.context;

  /** Create during component initialisation: loads the context of the game `game()` returns. */
  constructor(game: () => Game | undefined, summary: () => RunSummary | null | undefined, phone: boolean) {
    $effect(() => {
      const g = game();
      const s = summary();
      this.context = null;
      this.loadError = "";
      if (!g) return;
      let live = true;
      loadReportContext(g, s, phone).then(
        (ctx) => {
          if (live) this.context = ctx;
        },
        (error: unknown) => {
          if (live) this.loadError = String(error);
        },
      );
      return () => {
        live = false;
      };
    });
  }

  readonly isPerf = $derived(this.situation === "perf");
  readonly hasFail = $derived(this.situation === "fail" || this.situation === "crash");
  readonly trimmed = $derived(this.note.trim());
  readonly sending = $derived(this.view === "sending");

  readonly rows = $derived.by((): ReportRow[] => {
    const ctx = this.#ctx();
    if (!ctx) return [];
    const list: Omit<ReportRow, "removed">[] = [
      { id: "exe", label: "程序指纹", value: ctx.exeLabel, locked: true },
      { id: "ver", label: "版本", value: ctx.versionLabel, locked: true },
      { id: "plat", label: "平台", value: ctx.platformLabel, locked: true },
      { id: "cp", label: "编码", value: ctx.codepageLabel, locked: true },
    ];
    // A launch failure is the point of the report, so it cannot be left out.
    if (this.hasFail)
      list.push({ id: "fail", label: "失败信息", value: ctx.failureLabel, locked: this.situation === "fail", optional: "failure" });
    if (this.isPerf) list.push({ id: "perf", label: "性能", value: ctx.perfLabel, locked: false, optional: "perf" });
    if (this.trimmed) list.push({ id: "note", label: "备注", value: this.trimmed, txt: true, locked: false, optional: "note" });
    return list.map((r) => ({ ...r, removed: !r.locked && r.optional !== undefined && this.off[r.optional] }));
  });

  readonly report = $derived.by((): Report | null => {
    const ctx = this.#ctx();
    if (!ctx) return null;
    const { situation, hasFail, isPerf, trimmed, off } = this;
    const keepFailure = hasFail && (situation === "fail" || !off.failure);
    return buildReport(ctx.base, {
      situation,
      perfIssues: PERF_ISSUES.filter((k) => this.issues[k]),
      note: trimmed && !off.note ? trimmed : null,
      failure: keepFailure ? ctx.failure : null,
      perf: isPerf && !off.perf ? ctx.perf : null,
    });
  });

  readonly raw = $derived(this.report ? JSON.stringify(this.report, null, 2) : "");

  toggleIssue(key: PerfIssue): void {
    this.issues[key] = !this.issues[key];
  }

  async submit(): Promise<void> {
    const report = this.report;
    if (!report || this.sending) return;
    this.view = "sending";
    // There is no report service yet: only the browser preview offers sending.
    this.view = await fakeSubmit(report);
  }

  async save(): Promise<void> {
    const report = this.report;
    if (!report || this.sending) return;
    try {
      this.savedPath = await call<string>("save_report", { report }, fakeSavedPath());
      this.view = "saved";
    } catch (error) {
      console.error("save_report failed", error);
    }
  }
}
