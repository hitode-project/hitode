import { saveReportFile, submitReport, type ReportContext } from "./fake-report";
import { buildReport, PERF_ISSUES, type PerfIssue, type Report, type Situation } from "./report";

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

  #ctx: () => ReportContext | null;

  constructor(ctx: () => ReportContext | null) {
    this.#ctx = ctx;
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
      failure: keepFailure ? { ...ctx.failure, after: situation === "fail" ? "lt_1m" : "1m_10m" } : null,
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
    this.view = await submitReport(report);
  }

  async save(): Promise<void> {
    const report = this.report;
    if (!report || this.sending) return;
    this.savedPath = await saveReportFile(report);
    this.view = "saved";
  }
}
