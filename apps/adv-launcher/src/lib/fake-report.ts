import { FAKE_NOW } from "./fake";
import { codepageLabel } from "./format";
import type { Report, ReportBase, ReportFailure, ReportPerf } from "./report";
import type { Game } from "./types";

// Fake report context and host actions until the launcher reads them from the
// Rust side (adv-report) and submits through it.

/** The failure the fake launch shows for the game that fails (see Launch.svelte). */
export const FAKE_FAILURE_LABEL = "未实现 · Direct3D 9 体积纹理";
const FAKE_FAILURE: Omit<ReportFailure, "after"> = {
  kind: "api",
  api: "d3d9.dll!IDirect3DDevice9::CreateVolumeTexture",
  cpu_fault: null,
};
const FAKE_PERF: ReportPerf = { frame_ms_p50: 16.8, frame_ms_p95: 41.2, frame_ms_p99: 88.5 };

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
  failure: Omit<ReportFailure, "after">;
  perfLabel: string;
  perf: ReportPerf;
}

/** Deterministic 32-bit hash so every fake game gets stable fingerprints. */
function hash(text: string, seed: number): number {
  let h = 2166136261 ^ seed;
  for (const ch of text) h = Math.imul(h ^ (ch.codePointAt(0) ?? 0), 16777619);
  return h >>> 0;
}

function fakeSha(id: string): string {
  if (id === "g1") return "3f9a0d6e8b47c1f25a6e93d0b7c48f1e2d5a9b06c3e71f48a2b6d90e5f13c21e";
  return Array.from({ length: 8 }, (_, i) => hash(id, i).toString(16).padStart(8, "0")).join("");
}

function sizeLabel(bytes: number): string {
  return `${(bytes / 1048576).toFixed(1)} MB`;
}

export function fakeReportContext(game: Game): ReportContext {
  const sha = fakeSha(game.id);
  const size = game.id === "g1" ? 1887437 : 600000 + (hash(game.id, 99) % 3000000);
  return {
    base: {
      dedupe_key: "q3V9sJk2" + hash(game.id, 7).toString(36).padStart(7, "0"),
      app: { version: "0.4.2", build: "a1b2c3d", channel: "preview" },
      platform: { os: "windows", os_major: "11", arch: "x86_64", cpu_engine: "x64", graphics: "gpu" },
      game: { exe_sha256: sha, exe_size: size, codepage: game.codepage },
    },
    build: "adv-host 0.4.2 · Windows 11 x64",
    exeLabel: `sha256 ${sha.slice(0, 4)}…${sha.slice(-4)} · ${sizeLabel(size)}`,
    versionLabel: "0.4.2 (a1b2c3d)",
    platformLabel: "Windows 11 · x64",
    codepageLabel: `${game.codepage} ${codepageLabel(game.codepage)}`,
    failureLabel: FAKE_FAILURE_LABEL,
    failure: FAKE_FAILURE,
    perfLabel: "帧时间 p50 16.8 ms · p95 41.2 ms",
    perf: FAKE_PERF,
  };
}

let attempts = 0;

/** Pretends to submit; the first attempt of the session fails so the retry flow is reachable. */
export function submitReport(report: Report): Promise<"sent" | "error"> {
  void report;
  attempts += 1;
  const ok = attempts > 1;
  return new Promise((resolve) => setTimeout(() => resolve(ok ? "sent" : "error"), 1400));
}

/** Pretends to save the report JSON; resolves to the displayed path. */
export function saveReportFile(report: Report): Promise<string> {
  void report;
  const day = new Date(FAKE_NOW).toISOString().slice(0, 10);
  return Promise.resolve(`[下载]/adv-report-${day}.json`);
}

/** Pretends to reveal a saved file in the system file manager. */
export function revealFile(path: string): void {
  void path;
}
