import { t } from "./i18n.svelte";
import { untrack } from "svelte";
import { androidStorage } from "./android-access.svelte";
import { inTauri, onHostEvent } from "./bridge";
import { device } from "./device.svelte";
import {
  importRuntimeComponent,
  installRuntimeComponent,
  libraryInspect,
  pauseGame,
  pickComponentFile,
  startGame,
  stopGame,
  type GameFinished,
  type RunSummary,
} from "./host";

/**
 * A script started from the run page. It runs there, its console shown, until
 * it shows a frame or a dialog; then the game screen takes over.
 */
export type ScriptRun = {
  gameId: string;
  file: string;
  /** What it wrote to its console so far. */
  output: string;
  shown: boolean;
  result?: { exitCode: number; entries: number } | { error: string };
};
import { explainJit, jit } from "./jit.svelte";
import { library } from "./library.svelte";
import { nav } from "./nav.svelte";
import { prefs } from "./prefs.svelte";
import {
  componentForDll,
  exitedWithoutFrame,
  missingRuntimeComponent,
  type RuntimeComponent,
} from "./redist";

/**
 * The states of the launch screen; a game that fails to run comes back here as
 * "failed". iOS waits for a JIT request in progress ("jit-wait") and otherwise
 * recommends JIT once per session ("jit"): a game started without it runs
 * interpreted to its end.
 */
export type LaunchState = "prepare" | "failed" | "unsupported" | "jit" | "jit-wait";

/** The user chose to play without JIT this session. */
let jitDeclined = false;

const UNSUPPORTED_PLATFORM = "此平台暂不支持启动";

const NO_GAME = "no game is running";

/** Calls a pause / stop command, ignoring that the game has already ended. */
function quiet(command: Promise<void>, name: string): Promise<void> {
  return command.catch((error: unknown) => {
    if (!String(error).includes(NO_GAME)) console.error(`${name} failed`, error);
  });
}

/**
 * Pausing the running game: the pause menu and a report opened during play
 * each hold a pause while they are open. Holds are counted and the host is
 * told once per change, after the current update, so moving from the pause
 * menu to the report does not resume the game in between.
 */
class Pauser {
  #holds = 0;
  #sent = false;
  #queued = false;

  /** Pauses until the returned release is called. */
  hold(): () => void {
    this.#holds += 1;
    this.#schedule();
    let released = false;
    return () => {
      if (released) return;
      released = true;
      this.#holds -= 1;
      this.#schedule();
    };
  }

  /** A new or ended game starts unpaused on the host. */
  reset(): void {
    this.#sent = false;
    this.#schedule();
  }

  #schedule(): void {
    if (this.#queued) return;
    this.#queued = true;
    queueMicrotask(() => {
      this.#queued = false;
      const paused = this.#holds > 0;
      if (paused === this.#sent || !inTauri) return;
      this.#sent = paused;
      void quiet(pauseGame(paused), "pause_game");
    });
  }
}

export const pauser = new Pauser();

/**
 * The running game and what the last run of each game left behind: the run
 * summary (the report's failure and frame timing) and, for a run that failed,
 * the error the launch screen shows once. Errors are only kept for this
 * session; summaries fall back to the library's last-run record.
 */
class Runs {
  summaries = $state<Record<string, RunSummary | null>>({});
  /** The pause menu to restore when returning from an in-game report. */
  pauseAfterReport: string | null = null;
  takePauseAfterReport(id: string): boolean {
    if (this.pauseAfterReport !== id) return false;
    this.pauseAfterReport = null;
    return true;
  }

  /** Launch errors not yet shown, by game id. */
  errors = $state<Record<string, string>>({});
  /** Registry helper files discovered after a silent zero-frame exit, by game id. */
  registryHints = $state<Record<string, string[]>>({});
  /** The game the host is running (start_game returned and it has not finished). */
  running = $state<string | null>(null);
  /** Games the user asked to stop: their end is not a failure. */
  stopping = $state<Record<string, boolean>>({});
  /** start_game calls that have not answered yet, by game id. */
  #starting: Record<string, Promise<void>> = {};
  /** Optional one-shot executable override for the next launch of `gameId`. */
  #pendingExe: Record<string, string> = {};
  /** The last script started from the run page. */
  script = $state<ScriptRun | null>(null);

  constructor() {
    onHostEvent<GameFinished>("game_finished", (event) => this.#finished(event));
    onHostEvent<[string, string]>("script_output", ([id, text]) => {
      if (this.script?.gameId === id) this.script.output = text;
    });
    onHostEvent<string>("game_shown", (id) => {
      if (this.script?.gameId !== id || this.script.result) return;
      this.script.shown = true;
      nav.go({ name: "game", gameId: id });
    });
  }

  /** Runs a script of `gameId` behind the run page. */
  runScript(gameId: string, file: string): void {
    this.script = { gameId, file, output: "", shown: false };
    this.#pendingExe[gameId] = file;
    this.start(gameId).catch((error: unknown) => {
      if (this.script?.gameId === gameId) this.script.result = { error: launchError(String(error)) };
    });
  }

  /** Launches a specific executable in `gameId`'s environment without changing its main exe. */
  launchExe(gameId: string, exe: string): void {
    this.#pendingExe[gameId] = exe;
    nav.go({ name: "launch", gameId });
  }

  /** Starts `gameId` through the host. */
  start(gameId: string): Promise<void> {
    const exe = this.#pendingExe[gameId];
    delete this.#pendingExe[gameId];
    const started = startGame(gameId, exe).then(() => {
      this.running = gameId;
      pauser.reset();
    });
    this.#starting[gameId] = started;
    const settle = () => {
      if (this.#starting[gameId] === started) delete this.#starting[gameId];
    };
    started.then(settle, settle);
    return started;
  }

  /**
   * Asks the game to quit, or with `force` ends it at once. A game still
   * starting is stopped once it has started. The end arrives as game_finished.
   */
  async stop(gameId: string, force = false): Promise<void> {
    this.stopping[gameId] = true;
    const starting = this.#starting[gameId];
    if (starting) {
      try {
        await starting;
      } catch {
        // It never started: nothing to stop.
        delete this.stopping[gameId];
        return;
      }
    }
    await quiet(stopGame(force), "stop_game");
  }

  /** The game stays after a stop request it declined; the next stop asks again. */
  keep(gameId: string): void {
    delete this.stopping[gameId];
  }

  /** The summary of the game's last run: this session's, else the library's record. */
  summaryOf(gameId: string): RunSummary | null {
    if (gameId in this.summaries) return this.summaries[gameId] ?? null;
    return library.get(gameId)?.lastSummary ?? null;
  }

  /** The launch error waiting for `gameId`'s launch screen, if any; reading it clears it. */
  takeError(gameId: string): string | null {
    const error = this.errors[gameId] ?? null;
    delete this.errors[gameId];
    return error;
  }

  /** Registry files waiting for `gameId`'s launch screen after a zero-frame exit. */
  takeRegistryHint(gameId: string): string[] | null {
    const files = this.registryHints[gameId] ?? null;
    delete this.registryHints[gameId];
    return files;
  }

  #finished(event: GameFinished): void {
    const id = event.gameId;
    const here = nav.route.name === "game" || nav.route.name === "launch";
    const showing = here && "gameId" in nav.route && nav.route.gameId === id;
    const stopped = this.stopping[id] === true;
    delete this.stopping[id];
    if (this.pauseAfterReport === id) this.pauseAfterReport = null;
    if (this.running === id) this.running = null;
    pauser.reset();
    this.summaries[id] = event.summary;
    const script = this.script?.gameId === id && !this.script.result ? this.script : null;
    if (script) {
      script.result = "error" in event
        ? { error: stopped ? t("已停止") : event.error }
        : { exitCode: event.exitCode, entries: event.registry?.entries ?? 0 };
      // A script that never showed anything ends on the run page.
      if (!script.shown) return;
    }
    const game = library.get(id);
    if ("error" in event && !stopped) {
      this.errors[id] = event.error;
      if (showing) nav.replace({ name: "launch", gameId: id });
    } else if (game?.setup) {
      // A setup that ran picks what it installed next.
      if (showing) nav.replace({ name: "install", gameId: id });
    } else if (
      showing &&
      inTauri &&
      game &&
      exitedWithoutFrame(event, stopped, !!game.setup, !!script)
    ) {
      void libraryInspect(game.path)
        .then((found) => {
          const stillShowing =
            (nav.route.name === "game" || nav.route.name === "launch") &&
            "gameId" in nav.route &&
            nav.route.gameId === id &&
            this.running !== id;
          if (!stillShowing) return;
          const files = found?.registryFiles ?? [];
          if (files.length > 0) {
            this.registryHints[id] = files;
            nav.replace({ name: "launch", gameId: id });
          } else {
            library.focus = id;
            nav.home();
          }
        })
        .catch(() => {
          library.focus = id;
          if (showing) nav.home();
        });
    } else {
      // Back to the game's detail in the library; a stop the user asked for
      // (even a forced one, which reports an error) is a normal end.
      library.focus = id;
      if (showing) nav.home();
    }
  }
}

export const runs = new Runs();

/** Words a host launch error that names its cause for the player. */
function launchError(message: string): string {
  const missing = /^missing_dll:(.+)$/.exec(message);
  if (missing?.[1]) {
    const component = componentForDll(missing[1]);
    if (component) return t("需要 {0} 运行库（缺少 {1}）", component.name, component.dll);
    return t(MISSING_DLL, missing[1]);
  }
  if (message.includes("no 4 GiB-aligned guest window")) {
    return explainJit(jit.status?.increasedMemoryLimit ? "noGuestWindowExtended" : "noGuestWindow");
  }
  return message;
}

const MISSING_DLL = "缺少 {0}：这是 Hitode 不提供的 Windows 组件。如果这个程序是启动器或正版验证程序，请改为启动游戏本体。";

/**
 * The launch flow shared by the desktop and phone launch screens: start the
 * game through the host, then the game screen or the failure. The host does
 * not report preparation progress, so the screen shows an indeterminate bar.
 * Create it while a component initialises.
 */
export class LaunchFlow {
  view = $state<LaunchState>("prepare");
  /** Why the game stopped or cannot start. */
  reason = $state("");
  recovery = $state<"missing" | "access" | "permission" | "component" | "registry" | null>(null);
  missingComponent = $state<RuntimeComponent | null>(null);
  registryFiles = $state<string[]>([]);
  installError = $state("");
  recovering = $state(false);
  #gameId: () => string;
  #alive = true;

  get registryFile(): string | null {
    return this.registryFiles[0] ?? null;
  }

  constructor(gameId: () => string) {
    this.#gameId = gameId;
    // A pending error or zero-frame registry hint is shown by the effects below.
    if (!(gameId() in runs.errors) && !(gameId() in runs.registryHints)) void this.#start();
    $effect(() => {
      const id = this.#gameId();
      if (runs.errors[id] === undefined) return;
      const error = untrack(() => runs.takeError(id));
      if (error !== null) this.#failFromMessage(error);
    });
    $effect(() => {
      const id = this.#gameId();
      if (runs.registryHints[id] === undefined) return;
      const files = untrack(() => runs.takeRegistryHint(id));
      if (files && files.length > 0) {
        this.recovery = "registry";
        this.registryFiles = files;
        this.installError = "";
        this.#fail("failed", t("游戏未显示画面就退出了"));
      }
    });
    $effect(() => () => {
      this.#alive = false;
    });
  }

  async #start(): Promise<void> {
    const id = this.#gameId();
    this.view = "prepare";
    this.reason = "";
    this.recovery = null;
    this.missingComponent = null;
    this.registryFiles = [];
    this.installError = "";
    if (jit.active && !jitDeclined) {
      if (!jit.status) await jit.refresh();
      if (jit.busy) {
        this.view = "jit-wait";
        await jit.settled();
        if (!this.#alive || this.view !== "jit-wait") return;
      }
      if (jit.status && !jit.status.ready && !prefs.hideJitWarnings) {
        this.view = "jit";
        return;
      }
      this.view = "prepare";
    }
    const game = library.get(id);
    if (game && androidStorage.blocked(game.path, game.offline)) {
      this.recovery = "permission";
      this.#fail("unsupported", t("需要文件访问权限"));
      return;
    }
    if (game?.offline) {
      this.#fail("unsupported", game.offline === "disk" ? t("所在磁盘未连接") : t("找不到文件夹"));
      if (game.offline === "missing") this.recovery = "missing";
      return;
    }
    // Only builds with a native game view register start_game (macOS for now).
    if (!inTauri) {
      this.#fail("unsupported", t(UNSUPPORTED_PLATFORM));
      return;
    }
    runs.start(id).then(
      () => {
        // The game may already have ended or been cancelled, which leaves
        // this screen or fails it.
        if (this.#alive && this.view === "prepare" && !runs.stopping[id]) nav.replace({ name: "game", gameId: id });
      },
      (error: unknown) => {
        if (!this.#alive || this.view !== "prepare") return;
        const message = String(error);
        // Builds without the command answer "Command start_game not found".
        if (/start_game.*not found/i.test(message)) this.#fail("unsupported", t(UNSUPPORTED_PLATFORM));
        else if (message === "launch_missing_file" || message === "launch_folder_access") {
          if (device.platform === "android" && androidStorage.supported && !androidStorage.granted) {
            this.recovery = "permission";
            this.#fail("failed", t("需要文件访问权限"));
          } else {
            const missing = message === "launch_missing_file";
            this.recovery = missing ? "missing" : "access";
            this.#fail("failed", t(missing ? "找不到游戏文件" : "无法访问游戏文件夹"));
          }
        } else this.#failFromMessage(message);
      },
    );
  }

  #failFromMessage(message: string): void {
    const component = missingRuntimeComponent(message);
    if (component) {
      this.recovery = "component";
      this.missingComponent = component;
      this.installError = "";
      this.#fail("failed", t("需要 {0} 运行库（缺少 {1}）", component.name, component.dll));
      return;
    }
    this.#fail("failed", launchError(message));
    if (!/^missing_dll:/.test(message)) void this.#discoverRegistryFiles();
  }

  async #discoverRegistryFiles(): Promise<void> {
    if (!inTauri) return;
    const game = library.get(this.#gameId());
    if (!game || game.setup) return;
    try {
      const found = await libraryInspect(game.path);
      if (this.#alive && this.view === "failed" && this.recovery === null) {
        this.registryFiles = found?.registryFiles ?? [];
      }
    } catch {
      // Optional recovery hint; ignore inspection errors.
    }
  }

  #fail(view: LaunchState, reason: string): void {
    this.view = view;
    this.reason = reason;
  }

  /**
   * 取消: ends the game once it has started and goes back to its detail. It is
   * forced: a quit prompt from a game the user never saw could not be answered.
   */
  cancel(): void {
    const id = this.#gameId();
    void runs.stop(id, true);
    library.focus = id;
    nav.home();
  }

  retry(): void {
    void this.#start();
  }

  async recoverFolder(): Promise<void> {
    this.recovering = true;
    try {
      const recovered = this.recovery === "permission"
        ? await androidStorage.request()
        : await library.relocate(this.#gameId());
      if (recovered && this.#alive) this.retry();
    } finally {
      this.recovering = false;
    }
  }

  /** Downloads and installs the missing redistributable component, then launches. */
  async installComponent(): Promise<void> {
    if (!this.missingComponent || this.recovering) return;
    this.recovering = true;
    this.installError = "";
    try {
      await installRuntimeComponent(this.missingComponent.id);
      if (this.#alive) this.retry();
    } catch (error) {
      if (this.#alive) this.installError = String(error);
    } finally {
      this.recovering = false;
    }
  }

  /** Imports the missing redistributable component from a local installer or 32-bit DLL, then launches. */
  async importComponent(): Promise<void> {
    if (!this.missingComponent || this.recovering) return;
    this.installError = "";
    let path: string | null;
    try {
      path = await pickComponentFile();
    } catch (error) {
      if (this.#alive) this.installError = String(error);
      return;
    }
    if (!path || !this.#alive) return;
    this.recovering = true;
    try {
      await importRuntimeComponent(this.missingComponent.id, path);
      if (this.#alive) this.retry();
    } catch (error) {
      if (this.#alive) this.installError = String(error);
    } finally {
      this.recovering = false;
    }
  }

  /** Opens the game's scripts and registry page in place of the launch screen. */
  openRunPage(): void {
    nav.replace({ name: "game-run", gameId: this.#gameId() });
  }

  /** Enables JIT from the recommendation, then starts the game once it is ready. */
  async enableJit(): Promise<void> {
    this.view = "jit-wait";
    await jit.enable();
    await jit.settled();
    if (this.#alive && this.view === "jit-wait") void this.#start();
  }

  /** 仍然开始: plays interpreted, without asking again this session. */
  withoutJit(): void {
    jitDeclined = true;
    void this.#start();
  }

  /** Replaces the launch screen so leaving the report does not relaunch the game. */
  report(): void {
    nav.replace({ name: "report", gameId: this.#gameId() });
  }

  /** 更换主程序: the game's settings, in place of the launch screen. */
  changeExe(): void {
    nav.replace({ name: "game-settings", gameId: this.#gameId() });
  }
}
