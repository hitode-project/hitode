import { t } from "./i18n.svelte";
import { invoke } from "@tauri-apps/api/core";
import { call, inTauri } from "./bridge";
import { playback } from "./playback";
import { prefs } from "./prefs.svelte";
import { gameFilter } from "./upscaling.svelte";
import { fakeReportBase, fakeSavedPath, fakeSubmit } from "./fake-report";
import { codepageLabel } from "./format";
import type { RunSummary } from "./host";
import { buildReport, issueUrl, PERF_ISSUES, type PerfIssue, type Report, type ReportBase, type ReportApiGaps, type ReportFailure, type ReportPerf, type Situation, type ReportTitle, type ReportTimeline, type ReportSettings } from "./report";
import type { Game } from "./types";

// The report form shared by the desktop and phone report screens: what the
// user picked, the rows of "将发送的内容", the assembled report and the
// submit / save flow.

export type ReportView = "form" | "sending" | "sent" | "error" | "saved";
/** Rows the user may leave out of the report. */
export type Optional = "title" | "failure" | "perf" | "gaps" | "note";

export const SITUATIONS: [Situation, string][] = [
  ["no_start", "无法启动"], ["freeze", "卡死"], ["crash", "闪退"],
  ["stutter", "卡顿、掉帧"], ["graphics", "画面异常"], ["sound_video", "声音或视频异常"],
  ["input", "操作异常"], ["save", "存档读档异常"], ["heat", "发热、耗电异常"],
  ["other", "其他"], ["ok", "能正常玩"],
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
  title: ReportTitle | null;
  timeline: ReportTimeline | null;
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
  /** Null when the last run met no unimplemented API. */
  apiGaps: ReportApiGaps | null;
}

const OS_NAMES: Record<string, string> = { windows: "Windows", macos: "macOS", linux: "Linux", android: "Android", ios: "iOS" };
const NOT_RUN = "未运行过";

function failureLabel(summary: RunSummary | null | undefined): string {
  if (!summary) return t(NOT_RUN);
  const f = summary.failure;
  if (!f) return t("没有记录到失败");
  if (f.kind === "api") return t("未实现 · {0}", f.api ?? t("未知接口"));
  if (f.kind === "cpu") return t("CPU 异常 · {0}", f.cpu_fault ?? t("未知"));
  return t("运行时错误");
}

function perfLabel(summary: RunSummary | null | undefined): string {
  if (!summary) return t(NOT_RUN);
  const p = summary.perf;
  if (!p) return t("没有帧时间记录");
  return t("帧时间 p50 {0} ms · p95 {1} ms", p.frame_ms_p50.toFixed(1), p.frame_ms_p95.toFixed(1));
}

function gapsLabel(gaps: ReportApiGaps): string {
  return t("已调用 {0} · 仅导入 {1}", String(gaps.called.length), String(gaps.imported.length));
}

/**
 * The report context of `game`: the host identifies the build, platform and
 * main program (report_base), and the failure and frame timing come from the
 * game's last run this session (`summary`; missing when it has not run).
 */
export async function loadReportContext(game: Game, summary: RunSummary | null | undefined, phone: boolean): Promise<ReportContext> {
  const played = playback(game.id);
  const recorded = summary?.settings;
  const settings: ReportSettings = recorded
    ? {
        ...(recorded.upscaler === undefined ? {} : { upscaler: recorded.upscaler }),
        ...(recorded.scaling === undefined ? {} : { scaling: recorded.scaling }),
        ...(recorded.fast_forward === undefined ? {} : { fast_forward: recorded.fast_forward }),
        touch_mode: prefs.touchMode, font_files: recorded.font_files, font_overrides: recorded.font_overrides,
        font_smoothing: recorded.font_smoothing, discs: recorded.discs,
      }
    : {
        upscaler: gameFilter(game.id), scaling: played.scale === "int" ? "integer" : played.scale,
        fast_forward: played.toggle ? "toggle" : "hold", touch_mode: prefs.touchMode,
        font_files: played.fontFiles.length, font_overrides: Object.keys(played.fontFamilies).length,
        font_smoothing: played.smoothing, discs: played.discs.length,
      };
  const base = await call<ReportBase>(
    "report_base",
    { exe: `${game.path}/${game.exe}`, codepage: game.codepage, settings, graphics: summary?.graphics ?? null },
    fakeReportBase(game, phone),
  );
  base.platform.settings = settings;
  const { app, platform } = base;
  const sha = base.game.exe_sha256;
  const platformName = `${OS_NAMES[platform.os] ?? platform.os} ${platform.os_major} · ${platform.model}`;
  return {
    base,
    title: game.vndb ? { vndb: game.vndb.id, name: game.vndb.title, edition: base.game.file_version ?? "" } : null,
    timeline: summary?.timeline ?? null,
    build: `hitode ${app.version} · ${platformName}`,
    exeLabel: `sha256 ${sha.slice(0, 4)}…${sha.slice(-4)} · ${(base.game.exe_size / 1048576).toFixed(1)} MB`,
    versionLabel: `${app.version} (${app.build})`,
    platformLabel: platformName,
    codepageLabel: `${base.game.codepage} ${codepageLabel(base.game.codepage)}`,
    failureLabel: failureLabel(summary),
    failure: summary?.failure ?? null,
    perfLabel: perfLabel(summary),
    perf: summary?.perf ?? null,
    apiGaps: summary?.api_gaps ?? null,
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
  situation = $state<Situation>("no_start");
  edition = $state("");
  deliveryError = $state("");
  attachReport = $state(false);
  issues = $state<Partial<Record<PerfIssue, boolean>>>({});
  note = $state("");
  off = $state<Record<Optional, boolean>>({ title: false, failure: false, perf: false, gaps: false, note: false });
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
          if (live) { this.context = ctx; this.edition = ctx.title?.edition ?? ""; }
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

  readonly isPerf = $derived(this.situation === "stutter");
  readonly hasFail = $derived(["no_start", "freeze", "crash"].includes(this.situation));
  readonly trimmed = $derived(this.note.trim());
  readonly sending = $derived(this.view === "sending");

  readonly rows = $derived.by((): ReportRow[] => {
    const ctx = this.#ctx();
    if (!ctx) return [];
    const list: Omit<ReportRow, "removed">[] = [
      { id: "exe", label: t("程序指纹"), value: ctx.exeLabel, locked: true },
      { id: "ver", label: t("版本"), value: ctx.versionLabel, locked: true },
      { id: "plat", label: t("平台"), value: ctx.platformLabel, locked: true },
      { id: "cp", label: t("编码"), value: ctx.codepageLabel, locked: true },
    ];
    if (ctx.title) list.push({ id: "title", label: t("作品与版本"), value: ctx.title.name, txt: true, locked: false, optional: "title" });
    // A launch failure is the point of the report, so it cannot be left out.
    if (this.hasFail)
      list.push({ id: "fail", label: t("失败信息"), value: ctx.failureLabel, locked: this.situation === "no_start", optional: "failure" });
    if (this.isPerf) list.push({ id: "perf", label: t("性能"), value: ctx.perfLabel, locked: false, optional: "perf" });
    // Gaps the game survived matter in every situation.
    if (ctx.apiGaps) list.push({ id: "gaps", label: t("未实现接口"), value: gapsLabel(ctx.apiGaps), locked: false, optional: "gaps" });
    if (this.trimmed) list.push({ id: "note", label: t("备注"), value: this.trimmed, txt: true, locked: false, optional: "note" });
    return list.map((r) => ({ ...r, removed: !r.locked && r.optional !== undefined && this.off[r.optional] }));
  });

  readonly report = $derived.by((): Report | null => {
    const ctx = this.#ctx();
    if (!ctx) return null;
    const { situation, hasFail, isPerf, trimmed, off } = this;
    const keepFailure = hasFail && (situation === "no_start" || !off.failure);
    return buildReport(ctx.base, {
      situation,
      title: ctx.title ? { ...ctx.title, edition: this.edition.trim() } : null,
      keepTitle: !off.title,
      timeline: ctx.timeline,
      perfIssues: PERF_ISSUES.filter((k) => this.issues[k]),
      note: trimmed && !off.note ? trimmed : null,
      failure: keepFailure ? ctx.failure : null,
      perf: isPerf && !off.perf ? ctx.perf : null,
      apiGaps: off.gaps ? null : ctx.apiGaps,
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

  async openIssue(): Promise<void> {
    const report = this.report;
    if (!report || this.sending) return;
    this.deliveryError = "";
    this.attachReport = false;
    try {
      const json = await call<string>("report_json", { report }, JSON.stringify(report, null, 2));
      const url = issueUrl(report, json);
      if (!new URL(url).searchParams.has("report")) {
        if (!(await this.save())) return;
        this.attachReport = true;
      }
      if (inTauri) await invoke("plugin:opener|open_url", { url });
      else window.open(url, "_blank", "noopener");
    } catch {
      this.view = "form";
      this.deliveryError = t("没能打开问题页，请再试一次");
    }
  }

  async save(): Promise<boolean> {
    const report = this.report;
    if (!report || this.sending) return false;
    try {
      this.savedPath = await call<string>("save_report", { report }, fakeSavedPath());
      this.view = "saved";
      return true;
    } catch (error) {
      console.error("save_report failed", error);
      return false;
    }
  }
}
