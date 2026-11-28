import { t } from "./i18n.svelte";
import { call, inTauri } from "./bridge";
import {
  addToLibrary,
  CODEPAGES,
  encChoices,
  fakeInspection,
  fakeScan,
  FAKE_SCAN_ROOT,
  type Enc,
} from "./fake-import";
import { loadGameSetup, saveGameSetup } from "./fake-settings";
import { libraryAdd, libraryAddInstall, libraryInspect, libraryInspectDisc, libraryInstalled, libraryPickFolder, libraryScan, libraryUpdate, type Codepage, type ExeCandidate, type FolderInspection, type LibraryGame, type Novel, type ScanFound } from "./host";
import { library } from "./library.svelte";
import { nav } from "./nav.svelte";
import { copyExternalFolder, storageSize, takeCopyableUri, type FolderPickOptions } from "./storage";

// UI state of the three adding flows, shared by the desktop and phone pages.
// Construct each class during component initialisation: they own effects.
// Folder picking, inspection, scanning and adding go to the host (library_*
// commands); in a plain browser (`pnpm dev`) they use fake results instead.

export const encOfCodepage = (codepage: number): Enc => (codepage === 950 ? "zh-TW" : codepage === 936 ? "zh" : "ja");

/** Where the flow is: the folder picker is open, the host is reading, or a result. */
export type Phase = "picking" | "reading" | "ready" | "error";

async function pickFolder(options: FolderPickOptions): Promise<string | null> {
  return inTauri ? libraryPickFolder(options) : FAKE_SCAN_ROOT;
}

function copyOptions(signal: AbortSignal, update: (status: string) => void): FolderPickOptions {
  let total = 0;
  return {
    signal,
    onSize: (bytes) => { total = bytes; update(storageSize(bytes)); },
    onProgress: (progress) => update(`${storageSize(total)} · ${total ? Math.min(100, Math.floor(progress.bytes * 100 / total)) : 100}%`),
  };
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
  { value: "zh-TW", get label() { return t("繁中"); } },
];

/** A scan result as the pages list it. */
export interface ScanRow {
  /** The game folder; unique within one scan. */
  id: string;
  name: string;
  /** Path relative to the scanned folder. */
  path: string;
  exe: string;
  /** Programs the user can pick instead of the suggested one. */
  exes: string[];
  inLib: boolean;
  noExe: boolean;
  /** No program was suggested; the row offers the others, unticked. */
  unsure: boolean;
  /** Only an installer was found. */
  setup: boolean;
  found: ScanFound;
}

function scanRow(f: ScanFound): ScanRow {
  const exe = f.exe ?? f.exes.find((e) => e.arch === "x86") ?? null;
  return {
    id: f.folder,
    name: f.title,
    path: f.relative || ".",
    exe: exe?.path ?? "",
    exes: f.exes.filter((e) => e.arch === "x86").map((e) => e.path),
    inLib: f.inLibrary !== null,
    noExe: exe === null && !f.setup,
    unsure: f.exe === null && exe !== null,
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
  copyStatus = $state("");
  root = $state("");
  rows = $state<ScanRow[]>([]);
  depth = $state(2);
  watch = $state(true);
  /** Results the user unticked. */
  off = $state<Record<string, boolean>>({});
  /** Encodings the user changed from the detected one. */
  enc = $state<Record<string, Enc>>({});
  /** Main programs the user picked over the suggested one. */
  exe = $state<Record<string, string>>({});
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
  #picker = new AbortController();

  constructor() {
    $effect(() => () => this.#picker.abort());
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
      const root = await pickFolder(copyOptions(this.#picker.signal, (status) => (this.copyStatus = status)));
      takeCopyableUri();
      if (this.#picker.signal.aborted) return;
      this.copyStatus = "";
      if (!root) {
        nav.back();
        return;
      }
      this.root = root;
      await this.#run();
    } catch (error) {
      if (!this.#picker.signal.aborted) this.#fail(error);
    }
  }

  async #run(): Promise<void> {
    const run = ++this.#scan;
    this.phase = "reading";
    try {
      const found = inTauri ? await libraryScan(this.root, this.depth) : fakeScan(this.depth);
      if (run !== this.#scan) return;
      this.rows = found.map(scanRow);
      this.off = Object.fromEntries(this.rows.filter((g) => g.unsure).map((g) => [g.id, true]));
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

  exeOf(g: ScanRow): ExeCandidate | null {
    const picked = this.exe[g.id];
    return g.found.exes.find((e) => e.path === (picked ?? g.exe)) ?? null;
  }

  encOf(g: ScanRow): Enc {
    return this.enc[g.id] ?? encOfCodepage(this.exeOf(g)?.codepage ?? 932);
  }

  setExe(g: ScanRow, path: string): void {
    this.exe[g.id] = path;
    this.done = false;
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
        const exe = this.exeOf(g);
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
  copyStatus = $state("");
  copyableUri = $state<string | null>(null);
  #picker = new AbortController();
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
  readonly canCopy = $derived(this.copyableUri !== null && this.phase === "ready" && !this.addedId && !this.adding);

  constructor() {
    $effect(() => () => this.#picker.abort());
    void this.pick();
  }

  /** Opens the folder picker; cancelling it the first time leaves the screen. */
  async pick(): Promise<void> {
    this.#picker.abort();
    this.#picker = new AbortController();
    const controller = this.#picker;
    this.copyStatus = "";
    const first = this.inspection === null;
    this.phase = "picking";
    try {
      const folder = await pickFolder(copyOptions(controller.signal, (status) => (this.copyStatus = status)));
      const copyable = takeCopyableUri();
      if (controller.signal.aborted) return;
      this.copyStatus = "";
      if (!folder) {
        if (first) nav.back();
        else this.phase = "ready";
        return;
      }
      this.phase = "reading";
      const inspection = inTauri ? await libraryInspect(folder) : fakeInspection();
      if (!inspection) throw new Error(t("读不到这个文件夹"));
      this.#show(inspection, copyable);
    } catch (error) {
      if (controller.signal.aborted) return;
      this.error = String(error);
      this.phase = "error";
    }
  }

  /** Copies an in-place external folder into Hitode's private storage and re-inspects it. */
  async copyToHitode(): Promise<void> {
    const uri = this.copyableUri;
    if (!uri || !this.canCopy) return;
    this.#picker.abort();
    this.#picker = new AbortController();
    const controller = this.#picker;
    this.copyStatus = "";
    try {
      const folder = await copyExternalFolder(uri, {
        ...copyOptions(controller.signal, (status) => (this.copyStatus = status)),
        onStart: () => {
          this.phase = "picking";
        },
      });
      if (controller.signal.aborted) return;
      this.copyStatus = "";
      if (!folder) {
        this.phase = "ready";
        return;
      }
      this.phase = "reading";
      const inspection = inTauri ? await libraryInspect(folder) : fakeInspection();
      if (!inspection) throw new Error(t("读不到这个文件夹"));
      const previousExe = this.exes[this.exe]?.name.toLowerCase();
      const customTitle = this.title.trim() !== "" && this.title !== this.inspection?.title ? this.title : null;
      const previousEnc = this.enc;
      const previousVndb = this.vndb;
      this.#show(inspection, null);
      if (customTitle !== null) this.title = customTitle;
      const sameExe = previousExe ? inspection.exes.findIndex((e) => e.arch !== "x64" && e.path.toLowerCase() === previousExe) : -1;
      if (sameExe >= 0) this.exe = sameExe;
      this.enc = previousEnc;
      this.vndb = previousVndb;
    } catch (error) {
      if (controller.signal.aborted) return;
      this.error = String(error);
      this.phase = "error";
    }
  }

  #show(inspection: FolderInspection, copyable: string | null): void {
    this.inspection = inspection;
    this.copyableUri = copyable;
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

/** The disc image the next install screen installs from, or a picker error to show there. */
let pendingDisc: string | null = null;
let pendingError: string | null = null;

export function chooseInstallDisc(path: string): void {
  pendingDisc = path;
  pendingError = null;
}

export function chooseInstallError(message: string): void {
  pendingDisc = null;
  pendingError = message;
}

/** Where an install is: reading the disc, ready to run its setup, or choosing the installed program. */
export type InstallStep = "读取" | "识别" | "错误" | "完成";

/**
 * Installing a game from a disc image: the disc's setup runs as the game
 * (WINE-SES-12) with `C:\Program Files` in the chosen folder, then the player
 * picks what it installed. A library entry stands for the install from the
 * start, so the setup runs and ends like any game; `gameId` reopens the
 * screen at its last step.
 */
export class InstallFlow {
  step = $state<InstallStep>("读取");
  error = $state("");
  disc = $state("");
  label = $state("");
  detail = $state("");
  programs = $state<string[]>([]);
  setup = $state("");
  title = $state("");
  codepage = $state<Codepage>(932);
  /** The chosen install folder; the library's own folder when null. */
  target = $state<string | null>(null);
  keep = $state(true);
  /** Further discs of a setup on several, mounted after the first and put in drive D: from the pause menu. */
  more = $state<string[]>([]);
  addDiscError = $state("");
  /**
   * What the setup installed: in the install folder, or in a folder its
   * uninstall entry names when it was told to install elsewhere.
   */
  exes = $state<{ value: string; label: string; codepage: Codepage; folder: string; exe: string }[]>([]);
  exe = $state("");
  enc = $state<Enc>("ja");
  readonly encs = $derived(encChoices(this.title));
  readonly #gameId: string | null;

  constructor(gameId: string | null = null) {
    this.#gameId = gameId;
    if (gameId) void this.#installed(gameId);
    else void this.#read();
  }

  async #read(): Promise<void> {
    const pickError = pendingError;
    const disc = pendingDisc;
    pendingError = null;
    pendingDisc = null;
    if (pickError) return this.#fail(pickError);
    if (!disc) return this.#fail(t("没有选择光盘镜像"));
    this.disc = disc;
    try {
      const found = await libraryInspectDisc(disc);
      if (!found) return this.#fail(t("无法读取光盘镜像"));
      this.label = found.label;
      this.title = found.label;
      this.detail = t("光盘镜像 · {0}", sizeLabel(found.bytes));
      this.programs = found.programs;
      this.setup = found.programs[0] ?? "";
      this.codepage = found.codepage;
      this.enc = encOfCodepage(found.codepage);
      this.step = this.setup ? "识别" : "错误";
      if (!this.setup) this.error = t("光盘上没有可运行的程序");
    } catch (error) {
      this.#fail(String(error));
    }
  }

  async #installed(gameId: string): Promise<void> {
    const game = library.get(gameId);
    if (!game) return this.#fail(t("找不到这个游戏"));
    this.title = game.title;
    this.target = game.path;
    this.enc = encOfCodepage(game.codepage);
    try {
      const found = await libraryScan(game.path, 3);
      const exes = found.flatMap((f) =>
        f.exes.map((x) => {
          const path = [f.relative, x.path].filter(Boolean).join("/");
          return { value: path, label: path, codepage: x.codepage, folder: game.path, exe: path };
        }),
      );
      for (const program of await libraryInstalled(gameId)) {
        for (const f of await libraryScan(program.folder, 1)) {
          for (const x of f.exes) {
            const exe = [f.relative, x.path].filter(Boolean).join("/");
            const value = `${program.folder}/${exe}`;
            exes.push({ value, label: `${program.name} · ${exe}`, codepage: x.codepage, folder: program.folder, exe });
          }
        }
      }
      this.exes = exes;
      const suggested = found.find((f) => f.exe);
      this.exe = suggested?.exe ? [suggested.relative, suggested.exe.path].filter(Boolean).join("/") : (exes[0]?.value ?? "");
      this.step = "完成";
      if (!exes.length) this.error = t("安装位置里没有找到程序");
    } catch (error) {
      this.#fail(String(error));
    }
  }

  #fail(message: string): void {
    this.error = message;
    this.step = "错误";
  }

  /** Up to four discs mount at once (D: to G:). */
  get canAddDisc(): boolean {
    return this.more.length < 3;
  }

  async addDisc(): Promise<void> {
    this.addDiscError = "";
    try {
      const path = await call<string | null>("pick_game_file", { kind: "disc" }, null);
      if (path && path !== this.disc && !this.more.includes(path)) this.more = [...this.more, path];
    } catch (error) {
      this.addDiscError = String(error);
    }
  }

  async changeTarget(): Promise<void> {
    const folder = await libraryPickFolder();
    if (folder) this.target = folder;
  }

  /** Adds the install to the library and runs the setup as its game. */
  async start(): Promise<void> {
    const game = await libraryAddInstall(this.target, this.setup, CODEPAGES[this.enc], this.title.trim() || this.label);
    if (!game) return this.#fail(t("无法添加到游戏库"));
    const entry = library.put(game);
    const setup = loadGameSetup(entry.id, game.codepage);
    saveGameSetup(entry.id, { ...setup, discs: [this.disc, ...this.more] });
    nav.replace({ name: "launch", gameId: entry.id });
  }

  pickExe(value: string): void {
    this.exe = value;
    const found = this.exes.find((x) => x.value === value);
    if (found) this.enc = encOfCodepage(found.codepage);
  }

  pickEnc(value: Enc): void {
    this.enc = value;
  }

  /** The installed program becomes the game; the disc stays only if asked. */
  async add(): Promise<void> {
    const id = this.#gameId;
    const chosen = this.exes.find((x) => x.value === this.exe);
    if (!id || !chosen) return;
    const game = library.get(id);
    if (game && chosen.folder !== game.path) {
      // Installed elsewhere: the game's folder moves to where it went.
      const moved = await libraryUpdate(id, { folder: chosen.folder, exe: chosen.exe, codepage: CODEPAGES[this.enc] });
      if (moved) library.put(moved);
    } else {
      await library.setRun(id, { exe: chosen.exe, codepage: CODEPAGES[this.enc] });
    }
    if (!this.keep) {
      const setup = loadGameSetup(id, CODEPAGES[this.enc]);
      saveGameSetup(id, { ...setup, discs: [] });
    }
    nav.replace({ name: "launch", gameId: id });
  }
}
