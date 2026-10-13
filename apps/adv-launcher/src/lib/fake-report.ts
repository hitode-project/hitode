import type { Report, ReportBase } from "./report";
import type { Game } from "./types";

// Stand-ins used only in a plain browser (`pnpm dev`), where there is no
// host: the report fields the host would identify, a submission (there is no
// report service yet) and revealing a saved file.

/** Deterministic 32-bit hash so every fake game gets stable fingerprints. */
function hash(text: string, seed: number): number {
  let h = 2166136261 ^ seed;
  for (const ch of text) h = Math.imul(h ^ (ch.codePointAt(0) ?? 0), 16777619);
  return h >>> 0;
}

/** What report_base would return for `game` on a Windows PC or an Android phone. */
export function fakeReportBase(game: Game, phone: boolean): ReportBase {
  const sha = Array.from({ length: 8 }, (_, i) => hash(game.id, i).toString(16).padStart(8, "0")).join("");
  return {
    dedupe_key: "q3V9sJk2" + hash(game.id, 7).toString(36).padStart(7, "0"),
    app: { version: "0.4.2", build: "a1b2c3d", channel: "preview" },
    platform: phone
      ? { os: "android", os_major: "15", arch: "aarch64", cpu_engine: "a64", graphics: "gpu" }
      : { os: "windows", os_major: "11", arch: "x86_64", cpu_engine: "x64", graphics: "gpu" },
    game: { exe_sha256: sha, exe_size: 600000 + (hash(game.id, 99) % 3000000), codepage: game.codepage },
  };
}

let attempts = 0;

/** Pretends to submit; the first attempt of the session fails so the retry flow is reachable. */
export function fakeSubmit(report: Report): Promise<"sent" | "error"> {
  void report;
  attempts += 1;
  const ok = attempts > 1;
  return new Promise((resolve) => setTimeout(() => resolve(ok ? "sent" : "error"), 1400));
}

/** The path save_report would answer with. */
export function fakeSavedPath(): string {
  return `[下载]/adv-report-${new Date().toISOString().slice(0, 10)}.json`;
}
