import { t } from "./i18n.svelte";
import { call, inTauri } from "./bridge";
import {
  addToLibrary,
  defaultInstallTarget,
  encChoices,
  fakeInspection,
  fakeScan,
  FAKE_SCAN_ROOT,
  pickInstallFolder,
  takeInstallSource,
  type Enc,
} from "./fake-import";
import { libraryAdd, libraryInspect, libraryPickFolder, libraryScan, type Codepage, type ExeCandidate, type FolderInspection, type LibraryGame, type Novel, type ScanFound } from "./host";
import { library } from "./library.svelte";
import { nav } from "./nav.svelte";

// UI state of the three adding flows, shared by the desktop and phone pages.
// Construct each class during component initialisation: they own effects.
// Folder picking, inspection, scanning and adding go to the host (library_*
// commands); in a plain browser (`pnpm dev`) they use fake results instead.

const CODEPAGES: Record<Enc, Codepage> = { ja: 932, zh: 936 };
export const encOfCodepage = (codepage: number): Enc => (codepage === 936 ? "zh" : "ja");

/** Where the flow is: the folder picker is open, the host is reading, or a result. */
export type Phase = "picking" | "reading" | "ready" | "error";

async function pickFolder(): Promise<string | null> {
  return inTauri ? libraryPickFolder() : FAKE_SCAN_ROOT;
}

function sizeLabel(bytes: number): string {
  return bytes >= 1048576 ? `${(bytes / 1048576).toFixed(1)} MB` : `${Math.max(1, Math.round(bytes / 1024))} KB`;
}

/* ---------- Folder scan ---------- */

export const SCAN_DEPTHS = [1, 2, 3] as const;

export function depthLabel(n: number): string {
  return t("{0} 层子文件夹", n);
}

export const SCAN_ENC_OPTIONS: { value: Enc; label: string }[] = [
  { value: "ja", get label() { return t("日文"); } },
  { value: "zh", get label() { return t("简中"); } },
];

/** A scan result as the pages list it. */
export interface ScanRow {
  /** The game folder; unique within one scan. */
  id: string;
  name: string;
  /** Path relative to the scanned folder. */
  path: string;
  exe: string;
  inLib: boolean;
  noExe: boolean;
  /** Only an installer was found. */
  setup: boolean;
  found: ScanFound;
}

function scanRow(f: ScanFound): ScanRow {
  return {
    id: f.folder,
    name: f.title,
    path: f.relative || ".",
    exe: f.exe?.path ?? "",
    inLib: f.inLibrary !== null,
    noExe: f.exe === null && !f.setup,
    setup: f.setup,
    found: f,
  };
}

/** Results that cannot be ticked: already added, no main program, or an installer. */
export function scanLocked(g: ScanRow): boolean {
  return g.inLib || g.noExe || g.setup;
}

export function scanNote(g: ScanRow): string {
  return g.inLib ? t("已在库中") : t("没找到程序");
}

export class ScanPick {
  phase = $state<Phase>("picking");
  error = $state("");
  root = $state("");
  rows = $state<ScanRow[]>([]);
  depth = $state(2);
  watch = $state(true);
  /** Results the user unticked. */
  off = $state<Record<string, boolean>>({});
  /** Encodings the user changed from the detected one. */
  enc = $state<Record<string, Enc>>({});
  /** The installer row whose "暂不支持安装" note is showing. */
  installNote = $state<string | null>(null);
  adding = $state(false);
  done = $state(false);

  readonly list = $derived(this.rows);
  readonly pickable = $derived(this.list.filter((g) => !scanLocked(g)));
  readonly chosen = $derived(this.pickable.filter((g) => !this.off[g.id]));
  readonly allOn = $derived(this.chosen.length === this.pickable.length);
  readonly addLabel = $derived(
    this.done ? t("已添加") : this.adding ? t("正在添加…") : this.chosen.length ? t("添加 {0} 个游戏", this.chosen.length) : t("添加"),
  );

  #began = false;
  #scan = 0;

  constructor() {
    // After the "已添加" confirmation, return to the library.
    $effect(() => {
      if (!this.done) return;
      const timer = setTimeout(() => nav.home(), 900);
      return () => clearTimeout(timer);
    });
  }

  /** Opens the folder picker once; cancelling it leaves the screen. */
  async begin(): Promise<void> {
    if (this.#began) return;
    this.#began = true;
    try {
      const root = await pickFolder();
      if (!root) {
        nav.back();
        return;
      }
      this.root = root;
      await this.#run();
    } catch (error) {
      this.#fail(error);
    }
  }

  async #run(): Promise<void> {
    const run = ++this.#scan;
    this.phase = "reading";
    try {
      const found = inTauri ? await libraryScan(this.root, this.depth) : fakeScan(this.depth);
      if (run !== this.#scan) return;
      this.rows = found.map(scanRow);
      this.phase = "ready";
    } catch (error) {
      if (run === this.#scan) this.#fail(error);
    }
  }

  #fail(error: unknown): void {
    this.error = String(error);
    this.phase = "error";
  }

  isOn(g: ScanRow): boolean {
    return !scanLocked(g) && !this.off[g.id];
  }

  encOf(g: ScanRow): Enc {
    return this.enc[g.id] ?? encOfCodepage(g.found.exe?.codepage ?? 932);
  }

  setDepth(n: number): void {
    if (n === this.depth) return;
    this.depth = n;
    this.done = false;
    if (this.root) void this.#run();
  }

  toggle(g: ScanRow): void {
    this.off[g.id] = !this.off[g.id];
    this.done = false;
  }

  setEnc(g: ScanRow, v: Enc): void {
    this.enc[g.id] = v;
    this.done = false;
  }

  toggleAll(): void {
    const next = { ...this.off };
    for (const g of this.pickable) next[g.id] = this.allOn;
    this.off = next;
    this.done = false;
  }

  /** Installing is not wired to the host yet. */
  install(g: ScanRow): void {
    this.installNote = g.id;
  }

  /** Adds the ticked games one by one; a failure stops and is shown. */
  async add(): Promise<void> {
    if (this.done || this.adding) return;
    this.adding = true;
    try {
      for (const g of this.chosen) {
        const exe = g.found.exe;
        if (!exe) continue;
        const codepage = CODEPAGES[this.encOf(g)];
        if (inTauri) {
          const added = await libraryAdd(g.found.folder, exe.path, codepage, g.name);
          if (added) library.put(added);
        } else {
          addToLibrary(g.name, this.encOf(g), g.found.folder);
        }
      }
      await library.refresh();
      this.done = true;
    } catch (error) {
      this.#fail(error);
    } finally {
      this.adding = false;
    }
  }
}

/* ---------- Single game ---------- */

/** A main program candidate as the import page lists it. */
export interface ExeRow {
  name: string;
  size: string;
  /** 64-bit programs cannot run. */
  disabled: boolean;
  candidate: ExeCandidate;
}

export class ImportPick {
  phase = $state<Phase>("picking");
  error = $state("");
  inspection = $state<FolderInspection | null>(null);
  exe = $state(-1);
  enc = $state<Enc>("ja");
  title = $state("");
  /** The VNDB work the user chose; none links nothing. */
  vndb = $state<Novel | null>(null);
  /** Why linking the chosen work failed after the game was added. */
  linkError = $state("");
  adding = $state(false);
  /** Library id once added; the button then starts the game. */
  addedId = $state<string | null>(null);

  readonly exes = $derived<ExeRow[]>(
    (this.inspection?.exes ?? []).map((e) => ({
      name: e.path,
      size: sizeLabel(e.size),
      disabled: e.arch === "x64",
      candidate: e,
    })),
  );
  /** The library entry of the chosen program, if it was added before. */
  readonly chosenInLibrary = $derived(this.exes[this.exe]?.candidate.inLibrary ?? null);
  /** No program that can run: the "没找到程序" state. */
  readonly noExe = $derived(this.phase === "ready" && !this.exes.some((e) => !e.disabled));
  readonly encs = $derived(encChoices(this.title || (this.inspection?.title ?? "")));
  readonly canAdd = $derived(this.phase === "ready" && !this.noExe && this.exe >= 0 && this.title.trim() !== "" && !this.adding);

  constructor() {
    void this.pick();
  }

  /** Opens the folder picker; cancelling it the first time leaves the screen. */
  async pick(): Promise<void> {
    const first = this.inspection === null;
    this.phase = "picking";
    try {
      const folder = await pickFolder();
      if (!folder) {
        if (first) nav.back();
        else this.phase = "ready";
        return;
      }
      this.phase = "reading";
      const inspection = inTauri ? await libraryInspect(folder) : fakeInspection();
      if (!inspection) throw new Error(t("读不到这个文件夹"));
      this.#show(inspection);
    } catch (error) {
      this.error = String(error);
      this.phase = "error";
    }
  }

  #show(inspection: FolderInspection): void {
    this.inspection = inspection;
    this.title = inspection.title;
    this.vndb = null;
    this.addedId = null;
    const usable = inspection.exes.findIndex((e) => e.arch !== "x64");
    const suggested = inspection.suggested;
    this.exe = suggested !== null && inspection.exes[suggested]?.arch !== "x64" ? suggested : usable;
    this.enc = encOfCodepage(inspection.exes[this.exe]?.codepage ?? 932);
    this.phase = "ready";
  }

  /** Any change after adding asks to add again with the new choices. */
  #edit(apply: () => void): void {
    apply();
    this.addedId = null;
  }

  /** Choosing a program also takes its detected encoding. */
  pickExe(i: number): void {
    const row = this.exes[i];
    if (!row || row.disabled) return;
    this.#edit(() => {
      this.exe = i;
      this.enc = encOfCodepage(row.candidate.codepage);
    });
  }
  pickEnc(v: Enc): void {
    this.#edit(() => (this.enc = v));
  }
  setTitle(v: string): void {
    this.#edit(() => (this.title = v));
  }
  setVndb(v: Novel | null): void {
    this.#edit(() => (this.vndb = v));
  }

  /** Adds to the library; once added, starts the game instead. A program already added opens its entry. */
  async add(): Promise<void> {
    if (this.addedId) {
      nav.replace({ name: "launch", gameId: this.addedId });
      return;
    }
    if (this.chosenInLibrary !== null) {
      library.focus = this.chosenInLibrary;
      nav.home();
      return;
    }
    const inspection = this.inspection;
    const exe = this.exes[this.exe];
    if (!this.canAdd || !inspection || !exe) return;
    this.adding = true;
    this.linkError = "";
    try {
      const title = this.title.trim();
      if (inTauri) {
        const added = await libraryAdd(inspection.folder, exe.name, CODEPAGES[this.enc], title);
        if (added) this.addedId = library.put(added).id;
        // The game stays added if its metadata cannot be fetched.
        if (added && this.vndb) {
          try {
            const linked = await call<LibraryGame | null>("vndb_apply", { id: added.id, vn: this.vndb.id, title }, null);
            if (linked) library.put(linked);
          } catch (error) {
            this.linkError = String(error);
          }
        }
      } else {
        this.addedId = addToLibrary(title, this.enc, inspection.folder);
      }
    } catch (error) {
      this.error = String(error);
      this.phase = "error";
    } finally {
      this.adding = false;
    }
  }
}

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
    this.addedId = addToLibrary(this.source.meta.title, this.enc, this.target, this.source.meta);
  }
}
