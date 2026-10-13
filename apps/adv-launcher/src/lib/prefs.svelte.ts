/** Launcher-wide preferences; kept in localStorage until the Rust side stores settings. */
export type Theme = "system" | "light" | "dark";
export type Accent = "blue" | "green" | "graphite";
export type Language = "auto" | "zh" | "ja" | "en";
/** How the game frame fills the window; per-game settings may also stretch. */
export type Scale = "fit" | "int";
export type Filter = "smooth" | "sharp";
/** Whether the fast-forward key works while held or toggles. */
export type FastForward = "hold" | "toggle";

export interface Prefs {
  theme: Theme;
  accent: Accent;
  /** Blur covers of games marked adult until revealed. */
  blurAdult: boolean;
  language: Language;
  scale: Scale;
  filter: Filter;
  fastForward: FastForward;
  pauseInBackground: boolean;
  showFps: boolean;
  /** Fetch titles and covers from VNDB. */
  fetchVndb: boolean;
  trackPlayTime: boolean;
}

const KEY = "adv-launcher.prefs";
const defaults: Prefs = {
  theme: "system",
  accent: "blue",
  blurAdult: true,
  language: "auto",
  scale: "fit",
  filter: "smooth",
  fastForward: "hold",
  pauseInBackground: true,
  showFps: false,
  fetchVndb: true,
  trackPlayTime: true,
};

function load(): Prefs {
  try {
    const raw = localStorage.getItem(KEY);
    if (raw) return { ...defaults, ...(JSON.parse(raw) as Partial<Prefs>) };
  } catch {
    // Storage may be unavailable; fall back to defaults.
  }
  return { ...defaults };
}

export const prefs = $state<Prefs>(load());

export function savePrefs(): void {
  try {
    localStorage.setItem(KEY, JSON.stringify(prefs));
  } catch {
    // Not persisted this session.
  }
}
