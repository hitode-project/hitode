import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { buildReport, ISSUES, issueDraft, REPORT_SCHEMA, type ReportBase, type ReportInput } from "./report.ts";

const base: ReportBase = {
  dedupe_key: "0123456789abcdef",
  app: { version: "0.1.0", build: "8c048ed6", channel: "preview" },
  platform: {
    os: "macos", os_major: "26", model: "Example1,1",
    graphics: { api: "metal", name: "Example GPU", vendor: "apple", device: "Example GPU", driver: "26", tier: "metal" },
    settings: { upscaler: "off", scaling: "fit", fast_forward: "hold", touch_mode: "direct", font_files: 0, font_overrides: 0, font_smoothing: null, discs: 0 },
  },
  game: { exe_sha256: "ab".repeat(32), exe_size: 1887437, codepage: 932, timestamp: 123, image_size: 8192 },
};
const input: ReportInput = {
  situation: "crash", title: { vndb: "v123", name: "Example", edition: "1.2 patch" }, keepTitle: true,
  perfIssues: [], note: null,
  failure: { kind: "api", api: "d3d9.dll!IDirect3DDevice9::CreateVolumeTexture", cpu_fault: null, after: "1m_10m" },
  perf: { frame_ms_p50: 16.8, frame_ms_p95: 41.2, frame_ms_p99: 88.5 },
  apiGaps: { called: ["gdiplus.dll!GdipDrawCurve"], imported: ["ole32.dll!CoCreateGuid"] }, timeline: null,
};
// spec: REPORT-FMT-2
test("report uses /2 and Rust wire order", { timeout: 1000 }, () => {
  const report = buildReport(base, { ...input, keepTitle: false, situation: "no_start" });
  assert.equal(report.schema, REPORT_SCHEMA);
  assert.deepEqual(Object.keys(report), ["schema", "dedupe_key", "app", "platform", "game", "outcome", "issues", "failure", "perf", "api_gaps"]);
  assert.deepEqual(JSON.parse(JSON.stringify(report)), report);
  assert.equal(report.outcome, "failed");
  assert.deepEqual(report.issues, ["no_start"]);
});
// spec: REPORT-ID-1
test("form omits the title block when its row is unchecked", { timeout: 1000 }, () => {
  assert.deepEqual(buildReport(base, input).title, input.title);
  const report = buildReport(base, { ...input, keepTitle: false });
  assert.equal("title" in report, false);
  const url = new URL(issueDraft(report, JSON.stringify(report, null, 2)).url);
  assert.equal(url.searchParams.get("title"), ISSUES.crash);
  assert.ok(!url.searchParams.get("body")!.includes("vndb.org"));
  assert.ok(!url.searchParams.get("body")!.includes("1.2 patch"));
});
test("stutter retains only performance detail; never-run data stays absent", { timeout: 1000 }, () => {
  const report = buildReport(base, { ...input, situation: "stutter", perfIssues: ["loading", "stutter"], timeline: { frame: 10, frames: 4, last_frame: 70, end: 80 } });
  assert.deepEqual(report.perf_issues, ["stutter", "loading"]);
  assert.equal(report.timeline?.frame, 10);
  assert.equal("perf_issues" in buildReport(base, input), false);
  assert.deepEqual(buildReport(base, { ...input, situation: "ok" }).issues, []);
});
// spec: REPORT-ISSUE-1
test("manual issue template and launcher drafts share symptom labels", { timeout: 1000 }, () => {
  const template = readFileSync(new URL("../../../../.github/ISSUE_TEMPLATE/game-issue.yml", import.meta.url), "utf8");
  const options = template.split("id: symptom")[1].split("validations:")[0].split("options:\n")[1].trim().split("\n").map((line) => line.trim().slice(2));
  assert.deepEqual(options, Object.values(ISSUES));
  for (const situation of Object.keys(ISSUES) as (keyof typeof ISSUES)[]) {
    const note = "推进到第二句\n再点一次就闪退（A + B & C）";
    const report = buildReport(base, { ...input, situation, note });
    const draft = issueDraft(report, JSON.stringify(report, null, 2));
    const url = new URL(draft.url);
    const body = url.searchParams.get("body")!;
    assert.equal(draft.attachReport, false);
    assert.equal(url.searchParams.get("title"), `Example: ${ISSUES[situation]}`);
    assert.equal(url.searchParams.has("template"), false);
    assert.ok(body.includes(`### Symptom / 现象\n\n${ISSUES[situation]}\n\n`));
    assert.ok(body.includes(`### What happened / 发生了什么\n\n${note}\n\n`));
    assert.ok(body.includes("Example (https://vndb.org/v123)"));
    assert.ok(body.includes("1.2 patch"));
    assert.ok(body.includes("### Platform / 平台\n\nmacOS\n\n"));
    assert.ok(body.includes("Example1,1 · Hitode 0.1.0 (8c048ed6)"));
    assert.deepEqual(JSON.parse(body.split("\n```json\n")[1].split("\n```")[0]), report);
  }
});

// spec: REPORT-SAVE-2
test("issue URL keeps reports through 8000 encoded characters and omits larger reports", { timeout: 1000 }, () => {
  const report = buildReport(base, { ...input, note: "A note to keep in the issue" });
  const json = JSON.stringify(report, null, 2);
  const room = 8000 - issueDraft(report, json).url.length;
  const exact = issueDraft(report, json + " ".repeat(room));
  assert.equal(exact.url.length, 8000);
  assert.equal(exact.attachReport, false);
  const oversized = issueDraft(report, json + " ".repeat(room + 1));
  const body = new URL(oversized.url).searchParams.get("body")!;
  assert.equal(oversized.attachReport, true);
  assert.ok(!body.includes("```json"));
  assert.ok(body.includes(report.note!));
  assert.ok(body.includes(ISSUES.crash));
  const unicode = buildReport(base, { ...input, note: "𠮷".repeat(2000) });
  const shortened = issueDraft(unicode, JSON.stringify(unicode, null, 2));
  assert.equal(shortened.attachReport, true);
  assert.ok(shortened.url.length <= 8000);
  const preview = new URL(shortened.url).searchParams.get("body")!;
  assert.ok(preview.includes("𠮷…"));
  assert.ok(!preview.includes("�"));
});
