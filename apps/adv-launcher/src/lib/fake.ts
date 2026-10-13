import type { Game } from "./types";

// Fake library for the browser preview (`pnpm dev`); in the app the library
// comes from the Rust side.

const DAY = 86_400_000;
const now = Date.now();

interface Seed {
  title: string;
  brand: string;
  year: number;
  cover: string;
  engine: string;
  /** Days since last played; null if never started. */
  last: number | null;
  hours: number;
  sessions?: number;
  favorite?: boolean;
  nsfw?: boolean;
  offline?: Game["offline"];
  path?: string;
}

const seeds: Seed[] = [
  { title: "夏影のアルバム", brand: "月下工房", year: 2004, cover: "cv1", engine: "KiriKiri 2", last: 1, hours: 12.5, sessions: 9, favorite: true },
  { title: "星屑レター", brand: "Lapis Soft", year: 2007, cover: "cv2", engine: "NScripter", last: 3, hours: 31, sessions: 22, favorite: true },
  { title: "硝子の街と七つの鐘", brand: "鈴音堂", year: 2009, cover: "cv4", engine: "SoftPAL", last: 7, hours: 8, sessions: 6 },
  { title: "千年桜の約束", brand: "ひだまり舎", year: 2008, cover: "cv9", engine: "KiriKiri 2", last: 14, hours: 46, sessions: 31, favorite: true },
  { title: "ほうかごエトランゼ", brand: "Lapis Soft", year: 2011, cover: "cv5", engine: "CatSystem2", last: 30, hours: 3, sessions: 2 },
  { title: "雨音ステップ", brand: "白波ソフト", year: 2010, cover: "cv10", engine: "Yu-RIS", last: null, hours: 0, nsfw: true },
  { title: "蒼の残響", brand: "鈴音堂", year: 2005, cover: "cv7", engine: "SoftPAL", last: null, hours: 0 },
  { title: "灯台守のいない島", brand: "白波ソフト", year: 2003, cover: "cv8", engine: "NScripter", last: null, hours: 0, offline: "disk", path: "/Volumes/外置硬盘/Games/灯台守のいない島" },
  { title: "雪解けのプレリュード", brand: "月下工房", year: 2006, cover: "cv6", engine: "KiriKiri 2", last: null, hours: 0 },
  { title: "白露の庭", brand: "ひだまり舎", year: 2002, cover: "cv3", engine: "KiriKiri 2", last: null, hours: 0, offline: "missing", path: "~/ADV/白露の庭" },
  { title: "夜明けのリフレイン", brand: "月下工房", year: 2012, cover: "cv11", engine: "KiriKiri Z", last: null, hours: 0 },
  { title: "紅葉坂ダイアリー", brand: "鈴音堂", year: 2001, cover: "cv12", engine: "SoftPAL", last: null, hours: 0 },
];

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
  return {
    id: `g${i + 1}`,
    title: s.title,
    brand: s.brand,
    year: s.year,
    cover: s.cover,
    engine: s.engine,
    codepage: 932,
    path: s.path ?? `~/Games/${s.title}`,
    exe: "Game.exe",
    lastPlayed: s.last === null ? null : now - s.last * DAY,
    playMinutes: Math.max(Math.round(s.hours * 60), recent),
    favorite: s.favorite ?? false,
    nsfw: s.nsfw ?? false,
    offline: s.offline ?? null,
    daily: days,
    sessions30: s.sessions ?? 0,
  };
}
