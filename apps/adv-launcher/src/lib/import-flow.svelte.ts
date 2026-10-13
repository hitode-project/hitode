import {
  addScanned,
  addToLibrary,
  chooseInstallSource,
  defaultInstallTarget,
  encChoices,
  IMPORT_CANDIDATE,
  pickInstallFolder,
  scanFolder,
  takeInstallSource,
  type Enc,
  type Meta,
  type ScanFound,
} from "./fake-import";
import { nav } from "./nav.svelte";

// UI state of the three adding flows, shared by the desktop and phone pages.
// Construct each class during component initialisation: they own effects.

/* ---------- Folder scan ---------- */

export const SCAN_DEPTHS = [0, 1, 2, 3, 4] as const;

export function depthLabel(n: number): string {
  return n === 0 ? "只看所选文件夹" : `${n} 层子文件夹`;
}

export const SCAN_ENC_OPTIONS: { value: Enc; label: string }[] = [
  { value: "ja", label: "日文" },
  { value: "zh", label: "简中" },
];

/** Results that cannot be ticked: already added, no main program, or an installer. */
export function scanLocked(g: ScanFound): boolean {
  return !!g.inLib || !!g.noExe || !!g.setup;
}

export function scanNote(g: ScanFound): string {
  return g.inLib ? "已在游戏库" : "未找到主程序";
}

export class ScanPick {
  readonly found = scanFolder();
  depth = $state(2);
  watch = $state(true);
  /** Results the user unticked. */
  off = $state<Record<string, boolean>>({});
  /** Encodings the user changed from the detected one. */
  enc = $state<Record<string, Enc>>({});
  done = $state(false);

  readonly list = $derived(this.found.filter((g) => g.depth <= this.depth));
  readonly pickable = $derived(this.list.filter((g) => !scanLocked(g)));
  readonly chosen = $derived(this.pickable.filter((g) => !this.off[g.id]));
  readonly allOn = $derived(this.chosen.length === this.pickable.length);
  readonly addLabel = $derived(
    this.done ? "已添加" : this.chosen.length ? `添加 ${this.chosen.length} 个游戏` : "添加",
  );

  constructor() {
    // After the "已添加" confirmation, return to the library.
    $effect(() => {
      if (!this.done) return;
      const timer = setTimeout(() => nav.home(), 900);
      return () => clearTimeout(timer);
    });
  }

  isOn(g: ScanFound): boolean {
    return !scanLocked(g) && !this.off[g.id];
  }

  encOf(g: ScanFound): Enc {
    return this.enc[g.id] ?? g.enc ?? "ja";
  }

  setDepth(n: number): void {
    this.depth = n;
    this.done = false;
  }

  toggle(g: ScanFound): void {
    this.off[g.id] = !this.off[g.id];
    this.done = false;
  }

  setEnc(g: ScanFound, v: Enc): void {
    this.enc[g.id] = v;
    this.done = false;
  }

  toggleAll(): void {
    const next = { ...this.off };
    for (const g of this.pickable) next[g.id] = this.allOn;
    this.off = next;
    this.done = false;
  }

  install(g: ScanFound): void {
    chooseInstallSource(g.id);
    nav.go({ name: "install" });
  }

  add(): void {
    if (this.done) return;
    addScanned(this.chosen.map((g) => ({ found: g, enc: this.encOf(g) })));
    this.done = true;
  }
}

/* ---------- Single game ---------- */

const VNDB = /(vndb\.org\/|^)v\d+/;

export class ImportPick {
  readonly game = IMPORT_CANDIDATE;
  readonly encs = encChoices(IMPORT_CANDIDATE.title, IMPORT_CANDIDATE.mojibake);
  /** `match` indexes the matches, then the manual search / link row, then "no online data". */
  readonly LINK = IMPORT_CANDIDATE.matches.length;
  readonly NONE = IMPORT_CANDIDATE.matches.length + 1;

  exe = $state(0);
  enc = $state<Enc>("ja");
  match = $state(0);
  link = $state("");
  res = $state(0);
  /** Library id once added; the button then starts the game. */
  addedId = $state<string | null>(null);

  readonly linkOk = $derived(VNDB.test(this.link.trim()));
  readonly linkId = $derived(/v(\d+)/.exec(this.link)?.[1] ?? "");
  readonly searchOn = $derived(this.link.trim() !== "" && !this.linkOk);

  /** Any change after adding asks to add again with the new choices. */
  #edit(apply: () => void): void {
    apply();
    this.addedId = null;
  }

  pickExe(i: number): void {
    this.#edit(() => (this.exe = i));
  }
  pickEnc(v: Enc): void {
    this.#edit(() => (this.enc = v));
  }
  pickMatch(i: number): void {
    this.#edit(() => (this.match = i));
  }
  setLink(v: string): void {
    this.#edit(() => (this.link = v));
  }
  pickRes(i: number): void {
    this.#edit(() => (this.res = i));
  }

  chosenMeta(): Meta {
    const game = this.game;
    const none: Meta = { title: game.title, brand: "", year: 0, cover: "" };
    if (this.match < this.LINK) return game.matches[this.match] ?? none;
    if (this.match === this.LINK && this.linkOk) return game.linked;
    if (this.match === this.LINK && this.searchOn) return game.results[this.res] ?? none;
    return none;
  }

  /** Adds to the library; once added, starts the game instead. */
  add(): void {
    if (this.addedId) {
      nav.replace({ name: "launch", gameId: this.addedId });
      return;
    }
    this.addedId = addToLibrary(this.chosenMeta(), this.enc, this.game.folder);
  }
}

export const metaLine = (m: Meta): string => `${m.brand} · ${m.year}`;

/* ---------- Installer / disc image ---------- */

export type InstallStep = "识别" | "安装中" | "完成";

export class InstallFlow {
  readonly source = takeInstallSource();
  readonly title = this.source.meta.title;
  readonly disc = this.source.kind === "disc";
  readonly encs = encChoices(this.title, this.source.mojibake);

  step = $state<InstallStep>("识别");
  keep = $state(true);
  target = $state(defaultInstallTarget(this.title));
  pct = $state(0);
  exe = $state(this.source.exes[0] ?? "");
  enc = $state<Enc>("ja");
  /** Library id once added; the button then starts the game. */
  addedId = $state<string | null>(null);

  readonly finished = $derived(this.pct >= 100);
  readonly file = $derived(
    this.source.files[Math.min(this.source.files.length - 1, Math.floor(this.pct / 26))] ?? "",
  );
  readonly discLine = $derived(this.keep ? `${this.source.drive} ${this.source.label}` : "不保留");

  #timer: ReturnType<typeof setInterval> | undefined;

  constructor() {
    $effect(() => () => this.#stop());
  }

  #stop(): void {
    clearInterval(this.#timer);
    this.#timer = undefined;
  }

  /** Simulated installer run: the guest SETUP.EXE copying files. */
  start(): void {
    this.#stop();
    this.pct = 0;
    this.addedId = null;
    this.step = "安装中";
    this.#timer = setInterval(() => {
      this.pct = Math.min(100, this.pct + 3);
      if (this.pct >= 100) this.#stop();
    }, 110);
  }

  cancelRun(): void {
    this.#stop();
    this.step = "识别";
  }

  /** Clicking the installer window presses its default button. */
  tapFrame(): void {
    if (!this.finished) {
      this.#stop();
      this.pct = 100;
    } else {
      this.step = "完成";
    }
  }

  changeTarget(): void {
    this.target = pickInstallFolder(this.target, this.title);
  }

  pickExe(v: string): void {
    this.exe = v;
    this.addedId = null;
  }

  pickEnc(v: Enc): void {
    this.enc = v;
    this.addedId = null;
  }

  add(): void {
    if (this.addedId) {
      nav.replace({ name: "launch", gameId: this.addedId });
      return;
    }
    this.addedId = addToLibrary(this.source.meta, this.enc, this.target);
  }
}
