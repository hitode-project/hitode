import { library } from "./library.svelte";
import type { Game } from "./types";

// Fake data and simulated host actions for adding games (scan, single import,
// installer). Every call that will go to the Rust side lives in this file.

/** Guest text encoding offered when adding a game. */
export type Enc = "ja" | "zh";

const CODEPAGE: Record<Enc, number> = { ja: 932, zh: 936 };

/** The two encoding choices with a preview of the title as each would render it. */
export interface EncChoice {
  k: Enc;
  label: string;
  lang: string;
  preview: string;
}

/** `mojibake` is the Shift-JIS title read as GBK, which is what the wrong choice shows. */
export function encChoices(title: string, mojibake: string): EncChoice[] {
  return [
    { k: "ja", label: "日文", lang: "ja", preview: title },
    { k: "zh", label: "简体中文", lang: "zh-CN", preview: mojibake },
  ];
}

/** Online metadata the user can attach to a game. */
export interface Meta {
  title: string;
  brand: string;
  year: number;
  cover: string;
}

/* ---------- Folder scan ---------- */

export interface ScanFound {
  id: string;
  name: string;
  /** Path relative to the scanned folder. */
  path: string;
  exe: string;
  /** Subfolder depth below the scanned folder. */
  depth: number;
  meta: Meta;
  inLib?: boolean;
  enc?: Enc;
  noExe?: boolean;
  /** Only an installer was found; installing leads to the install flow. */
  setup?: boolean;
}

export const SCAN_ROOT = "[所选文件夹]/ADV";

/** Simulates scanning the folder the user picked, up to the deepest offered depth. */
export function scanFolder(): ScanFound[] {
  const m = (title: string, brand: string, year: number, cover: string): Meta => ({ title, brand, year, cover });
  return [
    { id: "a", name: "夏影のアルバム", path: "夏影のアルバム", exe: "Natsukage.exe", depth: 1, inLib: true, meta: m("夏影のアルバム", "月下工房", 2004, "cv1") },
    { id: "b", name: "星屑レター", path: "Lapis Soft/星屑レター", exe: "Hoshikuzu.exe", depth: 2, enc: "ja", meta: m("星屑レター", "Lapis Soft", 2007, "cv2") },
    { id: "c", name: "硝子の街と七つの鐘", path: "鈴音堂/硝子の街と七つの鐘 汉化版", exe: "GlassBell_chs.exe", depth: 2, enc: "zh", meta: m("硝子の街と七つの鐘", "鈴音堂", 2009, "cv4") },
    { id: "d", name: "灯台守のいない島", path: "旧作/2003/灯台守のいない島", exe: "Toudai.exe", depth: 3, enc: "ja", meta: m("灯台守のいない島", "白波ソフト", 2003, "cv8") },
    { id: "e", name: "雨音ステップ", path: "旧作/2010/雨音ステップ", exe: "", depth: 3, noExe: true, meta: m("雨音ステップ", "白波ソフト", 2010, "cv10") },
    { id: "f", name: "蒼の残響", path: "旧作/2005/蒼の残響", exe: "SETUP.EXE", depth: 3, setup: true, meta: m("蒼の残響", "鈴音堂", 2005, "cv7") },
  ];
}

/** Adds the picked scan results; returns how many were added. */
export function addScanned(picked: { found: ScanFound; enc: Enc }[]): number {
  for (const { found, enc } of picked) addToLibrary(found.meta, enc, `${SCAN_ROOT}/${found.path}`);
  return picked.length;
}

/* ---------- Single game ---------- */

export const IMPORT_CANDIDATE = {
  title: "夏影のアルバム",
  mojibake: "壞塭偺傾儖僶儉",
  folder: "[所选文件夹]",
  size: "2.1 GB",
  exes: [
    { name: "Natsukage.exe", size: "3.4 MB" },
    { name: "config.exe", size: "412 KB" },
  ],
  /** Metadata matches found by folder name. */
  matches: [
    { title: "夏影のアルバム", brand: "月下工房", year: 2004, cover: "cv1" },
    { title: "夏影のアルバム ～遠い日の約束～", brand: "月下工房", year: 2006, cover: "cv11" },
    { title: "夏影 -Summer Shade-", brand: "Lapis Soft", year: 2012, cover: "cv6" },
  ] satisfies Meta[],
  /** Results of a manual title search. */
  results: [
    { title: "夏影のアルバム ファンディスク", brand: "月下工房", year: 2005, cover: "cv11" },
    { title: "夏影のアルバム 体験版", brand: "月下工房", year: 2004, cover: "cv6" },
  ] satisfies Meta[],
  /** What a pasted VNDB link resolves to. */
  linked: { title: "夏影のアルバム", brand: "月下工房", year: 2004, cover: "cv1" } satisfies Meta,
};

/* ---------- Installer / disc image ---------- */

export interface InstallSource {
  kind: "disc" | "setup";
  /** Volume label or folder name shown in the source card. */
  label: string;
  /** Where the source was picked from. */
  path: string;
  detail: string;
  setup: string;
  /** Drive letter the disc stays mounted as while playing. */
  drive: string;
  meta: Meta;
  mojibake: string;
  installedSize: string;
  exes: string[];
  /** Files the installer window shows while copying. */
  files: string[];
}

const SOURCES: Record<string, InstallSource> = {
  disc: {
    kind: "disc",
    label: "NATSUKAGE_DISC1",
    path: "[下载]/NATSUKAGE_DISC1.iso",
    detail: "光盘镜像 · 612 MB",
    setup: "SETUP.EXE",
    drive: "D:",
    meta: { title: "夏影のアルバム", brand: "月下工房", year: 2004, cover: "cv1" },
    mojibake: "壞塭偺傾儖僶儉",
    installedSize: "1.4 GB",
    exes: ["Natsukage.exe", "config.exe"],
    files: ["data\\scenario.pak", "data\\graphic.pak", "data\\voice.pak", "movie\\op.mpg"],
  },
  f: {
    kind: "setup",
    label: "蒼の残響",
    path: `${SCAN_ROOT}/旧作/2005/蒼の残響`,
    detail: "安装程序 · 584 MB",
    setup: "SETUP.EXE",
    drive: "",
    meta: { title: "蒼の残響", brand: "鈴音堂", year: 2005, cover: "cv7" },
    mojibake: "憮偺巆嬁",
    installedSize: "820 MB",
    exes: ["Aonozankyou.exe", "config.exe"],
    files: ["data.xp3", "bgm.xp3", "voice.xp3", "video.xp3"],
  },
};

let pendingSource = "disc";

/** Chooses what the next install screen installs (a scan result, or the default disc). */
export function chooseInstallSource(id: string): void {
  pendingSource = id in SOURCES ? id : "disc";
}

/** The source for an install screen being opened; resets to the default disc. */
export function takeInstallSource(): InstallSource {
  const source = SOURCES[pendingSource] ?? (SOURCES["disc"] as InstallSource);
  pendingSource = "disc";
  return source;
}

export function defaultInstallTarget(title: string): string {
  return `[所选文件夹]/ADV/${title}`;
}

/** Simulates the folder picker for the install location. */
export function pickInstallFolder(current: string, title: string): string {
  const other = `[外置硬盘]/Games/${title}`;
  return current === other ? defaultInstallTarget(title) : other;
}

/* ---------- Library ---------- */

/**
 * Simulates adding a game to the library and returns its id. A game already in
 * the fake library (same title) is reused, and found again if it was offline.
 */
export function addToLibrary(meta: Meta, enc: Enc, path: string): string {
  const existing = library.games.find((g) => g.title === meta.title);
  if (existing) {
    if (existing.offline) library.relocate(existing.id);
    return existing.id;
  }
  const game: Game = {
    id: `add-${library.games.length + 1}-${Date.now().toString(36)}`,
    title: meta.title,
    brand: meta.brand,
    year: meta.year,
    cover: meta.cover,
    engine: "",
    codepage: CODEPAGE[enc],
    path,
    lastPlayed: null,
    playMinutes: 0,
    favorite: false,
    nsfw: false,
    offline: null,
    daily: Array.from({ length: 30 }, () => 0),
    sessions30: 0,
  };
  library.games = [...library.games, game];
  return game.id;
}
