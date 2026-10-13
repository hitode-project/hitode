import {
  CACHE_GB,
  GAME_DEFAULTS,
  SAVES_GB,
  clearCache,
  codepagePreview,
  fakeReports,
  fakeRoots,
  loadGameSetup,
  pickDiscImage,
  pickFont,
  saveGameSetup,
  type GameSetup,
  type GameValues,
  type LibraryRoot,
  type OverrideKey,
} from "./fake-settings";
import { library } from "./library.svelte";
import { prefs, savePrefs, type Accent, type Prefs } from "./prefs.svelte";
import type { Game } from "./types";

// State shared by the desktop and phone settings screens.

export function setPref<K extends keyof Prefs>(key: K, value: Prefs[K]): void {
  prefs[key] = value;
  savePrefs();
}

export const ACCENTS: { value: Accent; label: string; color: string }[] = [
  { value: "blue", label: "蓝", color: "#3767D6" },
  { value: "green", label: "绿", color: "#2D7556" },
  { value: "graphite", label: "石墨", color: "#3F3F44" },
];

const gb = (v: number) => `${v.toFixed(1)} GB`;

/** Library folders, storage and sent reports; kept for the session. */
class GeneralSettings {
  roots = $state<LibraryRoot[]>(fakeRoots.map((r) => ({ ...r })));
  cache = $state<"full" | "clearing" | "cleared">("full");
  withdrawn = $state<Record<string, boolean>>({});
  keyReset = $state(false);

  readonly cacheGb = $derived(this.cache === "cleared" ? 0 : CACHE_GB);
  readonly totalGb = $derived(SAVES_GB + this.cacheGb);
  readonly totalLabel = $derived(gb(this.totalGb));
  readonly cacheLabel = $derived(this.cache === "cleared" ? "0 MB" : gb(CACHE_GB));
  readonly clearLabel = $derived(this.cache === "cleared" ? "已清除" : this.cache === "clearing" ? "正在清除…" : "清除");
  /** Widths of the saves and cache segments of the usage bar, in percent. */
  readonly savesPct = $derived(Math.max(2, (SAVES_GB / this.totalGb) * 100));
  readonly cachePct = $derived((this.cacheGb / this.totalGb) * 100);
  readonly reports = $derived(
    fakeReports.map((r) => ({ ...r, title: library.get(r.gameId)?.title ?? "已移除的游戏" })),
  );

  rootMeta(root: LibraryRoot): string {
    const count = library.games.filter((g) => g.path.startsWith(root.path + "/")).length;
    return root.depth === null ? `磁盘未连接 · ${count} 个游戏` : `自动发现 · ${root.depth} 层 · ${count} 个游戏`;
  }

  removeRoot(id: string): void {
    this.roots = this.roots.filter((r) => r.id !== id);
  }

  async clearCache(): Promise<void> {
    this.cache = "clearing";
    await clearCache();
    this.cache = "cleared";
  }
}

export const general = new GeneralSettings();

type FontKey = "gothic" | "mincho";

/**
 * One game's settings form: run setup and per-game overrides of global values.
 * Create it during component setup; edits are saved for the session as they happen.
 */
export class GameForm {
  readonly game: Game;
  setup: GameSetup;
  /** Local or imported fonts picked this session, offered in both font menus. */
  picked = $state<string[]>([]);

  static readonly LETTERS = ["D:", "E:", "F:", "G:"];
  static readonly FONTS: { key: FontKey; name: string }[] = [
    { key: "gothic", name: "ＭＳ ゴシック" },
    { key: "mincho", name: "ＭＳ 明朝" },
  ];

  /** Global values the per-game settings fall back to. */
  readonly global = $derived<GameValues>({
    scale: prefs.scale,
    filter: prefs.filter,
    ff: prefs.fastForward,
    ...GAME_DEFAULTS,
  });
  readonly anyOverridden = $derived((Object.keys(this.global) as OverrideKey[]).some((k) => this.overridden(k)));
  /** The game's title as it reads with the current code page. */
  readonly encPreview: string;
  readonly extraFonts = $derived([
    ...new Set([
      ...this.picked,
      ...GameForm.FONTS.map((f) => this.value(f.key))
        .filter((v) => v.startsWith("font:"))
        .map((v) => v.slice(5)),
    ]),
  ]);
  readonly smoothClass = $derived(this.value("smooth") === "on" ? "sm-on" : this.value("smooth") === "off" ? "sm-off" : "");

  constructor(game: Game) {
    this.game = game;
    this.setup = $state(loadGameSetup(game.id, game.codepage));
    this.encPreview = $derived(codepagePreview(this.game.title, this.setup.nativeCodepage, this.game.codepage));
    $effect(() => saveGameSetup(this.game.id, $state.snapshot(this.setup)));
  }

  value<K extends OverrideKey>(key: K): GameValues[K] {
    return this.setup.overrides[key] ?? this.global[key];
  }

  overridden(key: OverrideKey): boolean {
    return this.value(key) !== this.global[key];
  }

  pick<K extends OverrideKey>(key: K, v: GameValues[K]): void {
    if (v === this.global[key]) delete this.setup.overrides[key];
    else this.setup.overrides[key] = v;
  }

  reset(key: OverrideKey): void {
    this.pick(key, this.global[key]);
  }

  resetAll(): void {
    this.setup.overrides = {};
  }

  get discsFull(): boolean {
    return this.setup.discs.length >= GameForm.LETTERS.length;
  }

  addDisc(): void {
    if (!this.discsFull) this.setup.discs.push(pickDiscImage(this.setup.exe, this.setup.discs));
  }

  removeDisc(index: number): void {
    this.setup.discs.splice(index, 1);
  }

  /** Applies a font menu choice; "local" and "import" open the host picker first. */
  async changeFont(key: FontKey, select: HTMLSelectElement): Promise<void> {
    const next = select.value;
    if (next !== "local" && next !== "import") {
      this.pick(key, next);
      return;
    }
    // Keep showing the current choice while the host dialog is open.
    select.value = this.value(key);
    const family = await pickFont(next);
    if (!family) return;
    if (!this.picked.includes(family)) this.picked.push(family);
    this.pick(key, `font:${family}`);
  }

  /** CSS font stack previewing a font choice. */
  static fontFamily(choice: string): string {
    if (choice === "serif") return "var(--font-serif-ja)";
    if (choice.startsWith("font:")) return `"${choice.slice(5)}", var(--font-ja)`;
    return "var(--font-ja)";
  }
}
