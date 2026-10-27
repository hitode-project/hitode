import { locale } from "./i18n.svelte";
import type { Game } from "./types";

// Fake library for the browser preview (`pnpm dev`); in the app the library
// comes from the Rust side.

const DAY = 86_400_000;
const now = Date.now();

interface Seed {
  cover: string;
  engine: string;
  /** Days since last played; null if never started. */
  last: number | null;
  hours: number;
  sessions?: number;
  favorite?: boolean;
  nsfw?: boolean;
  offline?: Game["offline"];
  root?: string;
}

// Neutral sample entries: numbered titles, no brands, plain covers.
const seeds: Seed[] = [
  { cover: "cv1", engine: "KiriKiri 2", last: 1, hours: 12.5, sessions: 9, favorite: true },
  { cover: "cv2", engine: "NScripter", last: 3, hours: 31, sessions: 22, favorite: true },
  { cover: "cv3", engine: "SoftPAL", last: 7, hours: 8, sessions: 6 },
  { cover: "cv4", engine: "KiriKiri 2", last: 14, hours: 46, sessions: 31, favorite: true },
  { cover: "cv5", engine: "CatSystem2", last: 30, hours: 3, sessions: 2 },
  { cover: "cv6", engine: "Yu-RIS", last: null, hours: 0, nsfw: true },
  { cover: "cv7", engine: "SoftPAL", last: null, hours: 0 },
  { cover: "cv8", engine: "NScripter", last: null, hours: 0, offline: "disk", root: "/Volumes/外置硬盘/Games" },
  { cover: "cv9", engine: "KiriKiri 2", last: null, hours: 0 },
  { cover: "cv10", engine: "KiriKiri 2", last: null, hours: 0, offline: "missing", root: "~/ADV" },
  { cover: "cv11", engine: "KiriKiri Z", last: null, hours: 0 },
  { cover: "cv12", engine: "SoftPAL", last: null, hours: 0 },
];

function sampleTitle(n: number): string {
  const language = locale();
  return language === "zh" ? `示例游戏 ${n}` : language === "ja" ? `サンプルゲーム ${n}` : `Sample Game ${n}`;
}

/** Deterministic pseudo-random daily minutes ending on the last-played day. */
function daily(index: number, last: number | null): number[] {
  let seed = index * 7919 + 17;
  const rnd = () => {
    seed = (seed * 9301 + 49297) % 233280;
    return seed / 233280;
  };
  return Array.from({ length: 30 }, (_, day) => {
    const ago = 29 - day;
    if (last === null || ago < last) return 0;
    if (ago === last) return Math.round(60 + rnd() * 110);
    return rnd() < 0.4 ? Math.round(20 + rnd() * 150) : 0;
  });
}

export function fakeLibrary(): Game[] {
  return seeds.map((s, i) => fakeGame(s, i));
}

function fakeGame(s: Seed, i: number): Game {
  const days = daily(i, s.last);
  // The total covers at least the last 30 days.
  const recent = days.reduce((sum, minutes) => sum + minutes, 0);
  const title = sampleTitle(i + 1);
  return {
    id: `g${i + 1}`,
    title,
    brand: "",
    year: 0,
    cover: s.cover,
    engine: s.engine,
    codepage: 932,
    path: `${s.root ?? "~/Games"}/${title}`,
    exe: "Game.exe",
    lastPlayed: s.last === null ? null : now - s.last * DAY,
    playMinutes: Math.max(Math.round(s.hours * 60), recent),
    favorite: s.favorite ?? false,
    nsfw: s.nsfw ?? false,
    offline: s.offline ?? null,
    daily: days,
    sessions30: s.sessions ?? 0,
    history: {},
    lastSummary: null,
  };
}
