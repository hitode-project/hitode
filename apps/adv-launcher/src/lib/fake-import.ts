import type { FolderInspection, ScanFound } from "./host";
import { library } from "./library.svelte";
import type { Game } from "./types";

// Fake data and simulated host actions for adding games. Scanning and single
// imports use them only in a plain browser (`pnpm dev`); the installer and the
// phone storage access are not wired to the host yet and use them everywhere.

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

/**
 * `mojibake` is the Shift-JIS title read as GBK, which is what the wrong
 * choice shows; without it (nothing to decode it from) both show the title.
 */
export function encChoices(title: string, mojibake = title): EncChoice[] {
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

export const FAKE_SCAN_ROOT = "[所选文件夹]/ADV";

function exe(path: string, codepage: 932 | 936 = 932): ScanFound["exe"] {
  return { path, size: 3_400_000, arch: "x86", codepage, inLibrary: null };
}

const FAKE_FOUND: ScanFound[] = [
  { relative: "夏影のアルバム", depth: 1, title: "夏影のアルバム", exe: exe("Natsukage.exe"), setup: false, inLibrary: "g1" },
  { relative: "Lapis Soft/星屑レター", depth: 2, title: "星屑レター", exe: exe("Hoshikuzu.exe"), setup: false, inLibrary: null },
  { relative: "鈴音堂/硝子の街と七つの鐘 汉化版", depth: 2, title: "硝子の街と七つの鐘 汉化版", exe: exe("GlassBell_chs.exe", 936), setup: false, inLibrary: null },
  { relative: "旧作/2003/灯台守のいない島", depth: 3, title: "灯台守のいない島", exe: exe("Toudai.exe"), setup: false, inLibrary: null },
  { relative: "旧作/2010/雨音ステップ", depth: 3, title: "雨音ステップ", exe: null, setup: false, inLibrary: null },
  { relative: "旧作/2005/蒼の残響", depth: 3, title: "蒼の残響", exe: null, setup: true, inLibrary: null },
].map((f) => ({ ...f, folder: `${FAKE_SCAN_ROOT}/${f.relative}` }));

/** What library_scan would find under the fake root, `depth` levels down. */
export function fakeScan(depth: number): ScanFound[] {
  return FAKE_FOUND.filter((f) => f.depth <= depth);
}

/* ---------- Phone storage access ---------- */

let storageGranted = false;

/** Android: whether the launcher may already read game folders on shared storage. */
export function hasStoragePermission(): boolean {
  return storageGranted;
}

/** Android: simulates the system screen granting folder access; the user comes back granted. */
export function requestStoragePermission(): void {
  storageGranted = true;
}

/** Space a game takes when copied into the app's own storage instead. */
export const COPY_SIZE = "2.3 GB";

/** iOS: what the Files picker returns; part of the picked game is still in iCloud. */
export function pickFolderIos(): { name: string; inCloud: boolean } {
  return { name: "夏影のアルバム", inCloud: true };
}

/* ---------- Single game ---------- */

/** What library_inspect would return for a picked game folder. */
export function fakeInspection(): FolderInspection {
  return {
    folder: "[所选文件夹]/夏影のアルバム",
    title: "夏影のアルバム",
    exes: [
      { path: "Natsukage.exe", size: 3_400_000, arch: "x86", codepage: 932, inLibrary: null },
      { path: "Natsukage64.exe", size: 3_900_000, arch: "x64", codepage: 932, inLibrary: null },
      { path: "config.exe", size: 412_000, arch: "x86", codepage: 932, inLibrary: null },
    ],
    suggested: 0,
    inLibrary: null,
  };
}

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
    path: `${FAKE_SCAN_ROOT}/旧作/2005/蒼の残響`,
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
export function addToLibrary(title: string, enc: Enc, path: string, meta?: Meta): string {
  const existing = library.games.find((g) => g.title === title);
  if (existing) {
    if (existing.offline) existing.offline = null;
    return existing.id;
  }
  const game: Game = {
    id: `add-${library.games.length + 1}-${Date.now().toString(36)}`,
    title,
    brand: meta?.brand ?? "",
    year: meta?.year ?? 0,
    cover: meta?.cover ?? "cv1",
    engine: "",
    codepage: CODEPAGE[enc],
    path,
    exe: "Game.exe",
    lastPlayed: null,
    playMinutes: 0,
    favorite: false,
    nsfw: false,
    offline: null,
    daily: Array.from({ length: 30 }, () => 0),
    sessions30: 0,
    history: {},
    lastSummary: null,
  };
  library.games = [...library.games, game];
  return game.id;
}
