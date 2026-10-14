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
import { inTauri } from "./bridge";
import { libraryInspect, type Codepage, type ExeCandidate } from "./host";
import { library } from "./library.svelte";
import { prefs, savePrefs, type Accent, type Filter, type Prefs } from "./prefs.svelte";
import { filterOverride, gameFilter, setGameFilter } from "./upscaling.svelte";
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
  readonly #initial: Game;
  setup: GameSetup;
  /** The folder's x86 programs in Tauri; read once when the form opens. */
  #candidates = $state<ExeCandidate[]>([]);
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

  /** Main program choices; in Tauri the current one stays listed even if it is gone. */
  readonly exes: string[];
  readonly exe: string;

  constructor(game: Game) {
    this.#initial = game;
    this.setup = $state(loadGameSetup(game.id, game.codepage));
    this.encPreview = $derived(codepagePreview(this.game.title, this.setup.nativeCodepage, this.game.codepage));
    this.exes = $derived.by(() => {
      if (!inTauri) return this.setup.exes;
      const paths = this.#candidates.map((e) => e.path);
      const current = this.game.exe.toLowerCase();
      return paths.some((p) => p.toLowerCase() === current) ? paths : [this.game.exe, ...paths];
    });
    this.exe = $derived(inTauri ? this.game.exe : this.setup.exe);
    $effect(() => saveGameSetup(this.game.id, $state.snapshot(this.setup)));
    if (inTauri) void this.#inspect();
  }

  /** The library's current entry; host updates replace it. */
  get game(): Game {
    return library.get(this.#initial.id) ?? this.#initial;
  }

  async #inspect(): Promise<void> {
    try {
      const found = await libraryInspect(this.game.path);
      this.#candidates = found?.exes.filter((e) => e.arch === "x86") ?? [];
    } catch (error) {
      console.error("library_inspect failed", error);
    }
  }

  /** Runs another program of the folder, with the code page detected for it. */
  setExe(exe: string): void {
    if (!inTauri) {
      this.setup.exe = exe;
      return;
    }
    const codepage = this.#candidates.find((e) => e.path === exe)?.codepage;
    void library.setRun(this.game.id, codepage ? { exe, codepage } : { exe });
  }

  setCodepage(codepage: Codepage): void {
    if (inTauri) void library.setRun(this.game.id, { codepage });
    else this.game.codepage = codepage;
  }

  value<K extends OverrideKey>(key: K): GameValues[K] {
    if (key === "filter") return gameFilter(this.game.id) as GameValues[K];
    return this.setup.overrides[key] ?? this.global[key];
  }

  overridden(key: OverrideKey): boolean {
    if (key === "filter") return filterOverride(this.game.id) !== undefined;
    return this.value(key) !== this.global[key];
  }

  pick<K extends OverrideKey>(key: K, v: GameValues[K]): void {
    if (key === "filter") {
      setGameFilter(this.game.id, v as Filter);
      return;
    }
    if (v === this.global[key]) delete this.setup.overrides[key];
    else this.setup.overrides[key] = v;
  }

  reset(key: OverrideKey): void {
    if (key === "filter") {
      setGameFilter(this.game.id, "inherit");
      return;
    }
    this.pick(key, this.global[key]);
  }

  resetAll(): void {
    this.setup.overrides = {};
    setGameFilter(this.game.id, "inherit");
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
