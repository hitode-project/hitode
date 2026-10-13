import type { Game } from "./types";

// Play statistics as pure functions of the library. The host keeps play
// sessions for the last 30 days only (`Game.daily`); older days are empty in
// the app and faked below for the browser preview.

export type StatsRange = "week" | "month" | "year";

export const STATS_RANGES: { value: StatsRange; label: string }[] = [
  { value: "week", label: "周" },
  { value: "month", label: "月" },
  { value: "year", label: "年" },
];

/** One game's minutes per calendar day, oldest first; the last entry is today. */
export interface PlayLog {
  game: Game;
  days: number[];
}

/** One chart column. */
export interface StatsBucket {
  /** Full label for the readout and screen readers: "10月12日 周一", "3月". */
  label: string;
  /** Axis label: "10/12", "周一", "3月". */
  short: string;
  hours: number;
}

export interface StatsRankRow {
  /** Null for the folded "其他 N 部" row. */
  game: Game | null;
  title: string;
  hours: number;
  /** Bar length relative to the top game, 0–100. */
  width: number;
}

export interface StatsScale {
  max: number;
  step: number;
  ticks: { value: number; label: string }[];
}

export interface RangeStats {
  heroLabel: string;
  totalHours: number;
  delta: string;
  chartTitle: string;
  buckets: StatsBucket[];
  scale: StatsScale;
  /** Label every n-th column, counted back from the newest. */
  labelEvery: number;
  rank: StatsRankRow[];
}

const DAY = 86_400_000;
const WEEKDAYS = ["日", "一", "二", "三", "四", "五", "六"];
/** Rows the ranking shows before folding the rest into one. */
const RANK_LIMIT = 5;

/** Calendar date `ago` days before `today` (local time, safe across DST). */
function dayBefore(today: Date, ago: number): Date {
  return new Date(today.getFullYear(), today.getMonth(), today.getDate() - ago);
}

const sum = (list: number[]) => list.reduce((a, b) => a + b, 0);

/** Sum of `days` over the `count` days ending `endAgo` days before today. */
function sumWindow(days: number[], endAgo: number, count: number): number {
  const end = days.length - endAgo;
  return sum(days.slice(Math.max(0, end - count), Math.max(0, end)));
}

/** Minutes in each month of `year` up to and including `lastMonth`; dates after `until` are excluded. */
function monthly(days: number[], today: Date, year: number, lastMonth: number, until: Date): number[] {
  const out = Array.from({ length: lastMonth + 1 }, () => 0);
  for (let i = 0; i < days.length; i++) {
    const minutes = days[i] ?? 0;
    if (!minutes) continue;
    const d = dayBefore(today, days.length - 1 - i);
    if (d.getFullYear() !== year || d > until || d.getMonth() > lastMonth) continue;
    out[d.getMonth()] = (out[d.getMonth()] ?? 0) + minutes;
  }
  return out;
}

/** A y-axis with at most five steps of a round size covering `max` hours. */
export function niceScale(max: number): StatsScale {
  const steps = [0.5, 1, 2, 5, 10, 20, 25, 50, 100, 200, 250, 500, 1000];
  const step = steps.find((s) => s * 5 >= max) ?? Math.ceil(max / 5000) * 1000;
  const top = Math.max(step, Math.ceil(max / step - 1e-9) * step);
  const ticks = [];
  for (let v = 0; v <= top + 1e-9; v += step) ticks.push({ value: v, label: step < 1 ? v.toFixed(1) : String(Math.round(v)) });
  return { max: top, step, ticks };
}

/** Per-game hours in the period, ranked; the tail folds into "其他 N 部". */
function rank(entries: { game: Game; minutes: number }[]): StatsRankRow[] {
  const played = entries.filter((e) => e.minutes > 0).sort((a, b) => b.minutes - a.minutes);
  const shown = played.length > RANK_LIMIT + 1 ? played.slice(0, RANK_LIMIT) : played;
  const rest = played.slice(shown.length);
  const top = (shown[0]?.minutes ?? 0) / 60;
  const row = (game: Game | null, title: string, minutes: number): StatsRankRow => {
    const hours = minutes / 60;
    return { game, title, hours, width: top > 0 ? Math.min(100, (hours / top) * 100) : 0 };
  };
  const rows = shown.map((e) => row(e.game, e.game.title, e.minutes));
  if (rest.length) rows.push(row(null, `其他 ${rest.length} 部`, sum(rest.map((e) => e.minutes))));
  return rows;
}

function compare(diffHours: number, period: string): string {
  if (Math.abs(diffHours) < 0.05) return `与${period}持平`;
  return `比${period}${diffHours > 0 ? "多" : "少"} ${Math.abs(diffHours).toFixed(1)} 小时`;
}

/** Everything the statistics screen shows for one range. */
export function rangeStats(logs: PlayLog[], range: StatsRange, today: Date): RangeStats {
  const len = Math.max(0, ...logs.map((l) => l.days.length));
  // Align every log on today, padding shorter ones with older zero days.
  const aligned = logs.map((l) => ({ game: l.game, days: [...Array<number>(len - l.days.length).fill(0), ...l.days] }));
  const total = Array.from({ length: len }, (_, i) => sum(aligned.map((l) => l.days[i] ?? 0)));

  if (range === "year") {
    const year = today.getFullYear();
    const month = today.getMonth();
    const minutes = monthly(total, today, year, month, today);
    const sameDayLastYear = new Date(year - 1, month, today.getDate());
    const lastYear = sum(monthly(total, today, year - 1, month, sameDayLastYear));
    const buckets = minutes.map((m, i) => ({ label: `${i + 1}月`, short: `${i + 1}月`, hours: m / 60 }));
    return {
      heroLabel: "今年",
      totalHours: sum(minutes) / 60,
      delta: lastYear > 0 ? `去年同期 ${(lastYear / 60).toFixed(1)} 小时` : "去年同期没有游玩记录",
      chartTitle: "每月游玩时长",
      buckets,
      scale: niceScale(Math.max(0, ...buckets.map((b) => b.hours))),
      labelEvery: 1,
      rank: rank(aligned.map((l) => ({ game: l.game, minutes: sum(monthly(l.days, today, year, month, today)) }))),
    };
  }

  const count = range === "week" ? 7 : 30;
  const buckets = total.slice(-count).map((m, i, list) => {
    const d = dayBefore(today, list.length - 1 - i);
    const weekday = `周${WEEKDAYS[d.getDay()] ?? ""}`;
    return {
      label: `${d.getMonth() + 1}月${d.getDate()}日 ${weekday}`,
      short: range === "week" ? weekday : `${d.getMonth() + 1}/${d.getDate()}`,
      hours: m / 60,
    };
  });
  const current = sumWindow(total, 0, count) / 60;
  const previous = sumWindow(total, count, count) / 60;
  return {
    heroLabel: `近 ${count} 天`,
    totalHours: current,
    delta: compare(current - previous, `前 ${count} 天`),
    chartTitle: "每日游玩时长",
    buckets,
    scale: niceScale(Math.max(0, ...buckets.map((b) => b.hours))),
    labelEvery: range === "week" ? 1 : 7,
    rank: rank(aligned.map((l) => ({ game: l.game, minutes: sumWindow(l.days, 0, count) }))),
  };
}

/** The column to show first: the newest one with any play, else the newest. */
export function defaultBucket(buckets: StatsBucket[]): number {
  for (let i = buckets.length - 1; i >= 0; i--) if ((buckets[i]?.hours ?? 0) > 0) return i;
  return buckets.length - 1;
}

/** Play logs for the library: older days (from `older`) followed by `Game.daily`. */
export function playLogs(games: Game[], now: number, older: (game: Game, now: number) => number[] = noOlderDays): PlayLog[] {
  return games.map((game) => ({ game, days: [...older(game, now), ...game.daily] }));
}

// ---- History before the 30-day window ------------------------------------

/** Days of fake history before `Game.daily`: back past January 1 of last year. */
const HISTORY_DAYS = 700;

/** No play recorded before the 30-day window: what the host knows. */
export function noOlderDays(): number[] {
  return Array.from({ length: HISTORY_DAYS }, () => 0);
}

/**
 * Deterministic fake minutes per day before the 30-day window: `playMinutes`
 * more, at about the window's pace (40 minutes a day on average), over the
 * days leading up to the last-played day or the window, whichever is older.
 */
export function fakeOlderDays(game: Game, now: number): number[] {
  const out = Array.from({ length: HISTORY_DAYS }, () => 0);
  if (game.lastPlayed === null || game.playMinutes <= 0) return out;

  let seed = [...game.id].reduce((a, c) => a * 31 + c.charCodeAt(0), 7) % 233280;
  const rnd = () => {
    seed = (seed * 9301 + 49297) % 233280;
    return seed / 233280;
  };
  const days = out.length;
  const recent = game.daily.length;
  const lastAgo = Math.max(recent, Math.round((now - game.lastPlayed) / DAY));
  /** Index in `out` of the day `ago` days before today. */
  const at = (ago: number) => days - 1 - (ago - recent);
  const span = Math.max(3, Math.round(game.playMinutes / 40));
  const weights = out.map(() => 0);
  for (let k = 0; k < span && at(lastAgo + k) >= 0; k++) {
    if (k === 0 || rnd() < 0.45) weights[at(lastAgo + k)] = 0.3 + rnd();
  }
  const total = sum(weights);
  let left = game.playMinutes;
  weights.forEach((w, i) => {
    if (!w) return;
    const minutes = Math.min(left, Math.round((w / total) * game.playMinutes));
    out[i] = minutes;
    left -= minutes;
  });
  // Rounding leftovers go to the last-played day.
  if (left > 0 && at(lastAgo) >= 0) out[at(lastAgo)] = (out[at(lastAgo)] ?? 0) + left;
  return out;
}
