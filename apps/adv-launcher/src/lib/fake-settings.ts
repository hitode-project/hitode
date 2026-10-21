import { call } from "./bridge";
import type { FastForward, Filter } from "./prefs.svelte";

// Persisted per-game settings and browser-only library maintenance fixtures.

export const APP_VERSION = "0.1.2";
export const DATA_DIR = "~/Library/Application Support/org.hitode.launcher";

export interface LibraryRoot {
  id: string;
  path: string;
  /** Folder levels searched for games; null for a disk that is not connected. */
  depth: number | null;
}

export const fakeRoots: LibraryRoot[] = [
  { id: "games", path: "~/Games", depth: 2 },
  { id: "adv", path: "~/ADV", depth: 1 },
  { id: "ext", path: "/Volumes/外置硬盘/Games", depth: null },
];

export interface SentReport {
  id: string;
  gameId: string;
  date: string;
  kind: string;
}

export const fakeReports: SentReport[] = [
  { id: "r1", gameId: "g1", date: "10 月 3 日", kind: "无法启动" },
  { id: "r2", gameId: "g2", date: "9 月 21 日", kind: "性能问题" },
];

/** What every report sends; mirrors the report screen's list. */
export const reportContents: { label: string; note: string }[] = [
  { label: "程序指纹", note: "主程序的 sha256 与大小" },
  { label: "版本", note: "hitode 版本与构建号" },
  { label: "平台", note: "系统与处理器架构" },
  { label: "编码", note: "游戏使用的代码页" },
  { label: "失败信息", note: "无法启动或中途出错时" },
  { label: "性能", note: "选择性能问题时的帧时间" },
  { label: "备注", note: "你填写的补充说明" },
];

export const SAVES_GB = 0.128;
export const CACHE_GB = 2.1;

/** Per-game settings that can override a global value. */
export type GameScale = "fit" | "int" | "stretch";
export type GlyphSmoothing = "game" | "on" | "off";
/** "auto", "sans", "serif", or "font:<name>" for a local or imported font. */
export type FontChoice = string;

export interface GameValues {
  scale: GameScale;
  filter: Filter;
  smooth: GlyphSmoothing;
  ff: FastForward;
  gothic: FontChoice;
  mincho: FontChoice;
}

export type OverrideKey = keyof GameValues;

/** Global defaults with no control on the settings screen. */
export const GAME_DEFAULTS = { smooth: "game", gothic: "auto", mincho: "auto" } as const;

export interface GameSetup {
  /** Code page the game's own strings are stored in. */
  nativeCodepage: number;
  exes: string[];
  exe: string;
  discs: string[];
  fontFiles: string[];
  overrides: Partial<GameValues>;
}

const exes: Record<string, string[]> = {
  g1: ["Natsukage.exe", "config.exe"],
  g2: ["Hoshikuzu.exe", "nsconfig.exe"],
  g3: ["Garasu.exe", "Setup.exe"],
  g4: ["Sennen.exe", "config.exe"],
  g5: ["Etranger.exe", "config.exe"],
  g6: ["Amaoto.exe", "Config.exe"],
  g7: ["Aonozankyo.exe", "Setup.exe"],
  g8: ["Toudaimori.exe", "nsconfig.exe"],
  g9: ["Yukidoke.exe", "config.exe"],
  g10: ["Hakuro.exe", "config.exe"],
  g11: ["Yoake.exe", "config.exe"],
  g12: ["Momiji.exe", "Setup.exe"],
};

// Persisted separately from the library, keyed by its stable game id.
const SETUP_KEY = "adv-launcher.game-settings";
function savedSetups(): [string, GameSetup][] {
  try { return Object.entries(JSON.parse(localStorage.getItem(SETUP_KEY) ?? "{}")); }
  catch { return []; }
}
const setups = new Map<string, GameSetup>(savedSetups());

/** The game's run setup; `codepage` is its current code page, taken as native on first load. */
export function loadGameSetup(gameId: string, codepage: number): GameSetup {
  const saved = setups.get(gameId);
  if (saved) return structuredClone(saved);
  const list = exes[gameId] ?? ["Game.exe"];
  return { nativeCodepage: codepage, exes: list, exe: list[0] ?? "Game.exe", discs: [], fontFiles: [], overrides: {} };
}

export function saveGameSetup(gameId: string, setup: GameSetup): void {
  setups.set(gameId, structuredClone(setup));
  localStorage.setItem(SETUP_KEY, JSON.stringify(Object.fromEntries(setups)));
}

/** Host action: drop cached thumbnails and shader caches. */
export function clearCache(): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, 400));
}

const tables = new Map<string, Map<string, number[]>>();

/** Character → bytes for a double-byte legacy encoding the platform can decode. */
function encodeTable(label: string): Map<string, number[]> {
  const cached = tables.get(label);
  if (cached) return cached;
  const table = new Map<string, number[]>();
  const decoder = new TextDecoder(label);
  for (let b = 0x80; b <= 0xff; b++) {
    const ch = decoder.decode(new Uint8Array([b]));
    if (ch.length === 1 && ch !== "�" && !table.has(ch)) table.set(ch, [b]);
  }
  // One decode per lead byte: every pair followed by a newline, split afterwards.
  for (let lead = 0x81; lead <= 0xfe; lead++) {
    const buf: number[] = [];
    for (let trail = 0x40; trail <= 0xfe; trail++) buf.push(lead, trail, 0x0a);
    decoder.decode(new Uint8Array(buf)).split("\n").forEach((ch, i) => {
      if (ch.length === 1 && ch !== "�" && !table.has(ch)) table.set(ch, [lead, 0x40 + i]);
    });
  }
  tables.set(label, table);
  return table;
}

const labels: Record<number, string> = { 932: "shift_jis", 936: "gbk", 950: "big5", 949: "euc-kr" };

/** How `text`, stored in code page `from`, reads when the game runs with code page `to`. */
export function codepagePreview(text: string, from: number, to: number): string {
  const src = labels[from];
  const dst = labels[to];
  if (from === to || !src || !dst) return text;
  try {
    const table = encodeTable(src);
    const bytes: number[] = [];
    for (const ch of text) {
      const code = ch.codePointAt(0) ?? 0;
      bytes.push(...(code < 0x80 ? [code] : (table.get(ch) ?? [0x3f])));
    }
    return new TextDecoder(dst).decode(new Uint8Array(bytes));
  } catch {
    return text;
  }
}

/** Host action: reveal the game's folder in Finder or Explorer. */
export function revealFolder(path: string): Promise<void> {
  return call("reveal_path", { path }, undefined);
}
