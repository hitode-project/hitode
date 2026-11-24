import type { Codepage, FolderInspection, ScanFound } from "./host";
import { library } from "./library.svelte";
import type { Game } from "./types";

// Fake data and simulated host actions for adding games. Scanning and single
// imports and phone storage use them only in a plain browser (`pnpm dev`).
// The installer flow uses simulated actions.

/** Guest text encoding offered when adding a game. */
export type Enc = "ja" | "zh" | "zh-TW";

export const CODEPAGES: Record<Enc, Codepage> = { ja: 932, zh: 936, "zh-TW": 950 };

/** An encoding choice with a title preview. */
export interface EncChoice {
  k: Enc;
  label: string;
  lang: string;
  preview: string;
}

/** Simulated installs can supply a GBK preview of the Shift-JIS title. */
export function encChoices(title: string, mojibake = title): EncChoice[] {
  return [
    { k: "ja", label: "日文", lang: "ja", preview: title },
    { k: "zh", label: "简体中文", lang: "zh-CN", preview: mojibake },
    { k: "zh-TW", label: "繁体中文", lang: "zh-TW", preview: title },
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

function exe(path: string, codepage: 932 | 936 = 932): NonNullable<ScanFound["exe"]> {
  return { path, size: 3_400_000, arch: "x86", codepage, inLibrary: null };
}

const FAKE_FOUND: ScanFound[] = [
  { relative: "夏影のアルバム", depth: 1, title: "夏影のアルバム", exe: exe("Natsukage.exe"), setup: false, inLibrary: "g1" },
  { relative: "Lapis Soft/星屑レター", depth: 2, title: "星屑レター", exe: exe("Hoshikuzu.exe"), setup: false, inLibrary: null },
  { relative: "鈴音堂/硝子の街と七つの鐘 汉化版", depth: 2, title: "硝子の街と七つの鐘 汉化版", exe: exe("GlassBell_chs.exe", 936), setup: false, inLibrary: null },
  { relative: "旧作/2003/灯台守のいない島", depth: 3, title: "灯台守のいない島", exe: exe("Toudai.exe"), setup: false, inLibrary: null },
  { relative: "旧作/2010/雨音ステップ", depth: 3, title: "雨音ステップ", exe: null, setup: false, inLibrary: null },
  { relative: "旧作/2005/蒼の残響", depth: 3, title: "蒼の残響", exe: null, setup: true, inLibrary: null },
].map((f) => ({
  ...f,
  folder: `${FAKE_SCAN_ROOT}/${f.relative}`,
  exes: f.exe ? [f.exe, ...(f.depth === 2 ? [exe("Config.exe"), exe("Launcher.exe")] : [])] : [],
}));

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
    codepage: CODEPAGES[enc],
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
