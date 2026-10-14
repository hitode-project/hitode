import { t } from "./i18n.svelte";
import { untrack } from "svelte";
import { inTauri, onHostEvent } from "./bridge";
import { pauseGame, startGame, stopGame, type GameFinished, type RunSummary } from "./host";
import { library } from "./library.svelte";
import { nav } from "./nav.svelte";

/** The states of the launch screen; a game that fails to run comes back here as "failed". */
export type LaunchState = "prepare" | "failed" | "unsupported";

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
  /** Launch errors not yet shown, by game id. */
  errors = $state<Record<string, string>>({});
  /** The game the host is running (start_game returned and it has not finished). */
  running = $state<string | null>(null);
  /** Games the user asked to stop: their end is not a failure. */
  stopping = $state<Record<string, boolean>>({});
  /** start_game calls that have not answered yet, by game id. */
  #starting: Record<string, Promise<void>> = {};

  constructor() {
    onHostEvent<GameFinished>("game_finished", (event) => this.#finished(event));
  }

  /** Starts `gameId` through the host. */
  start(gameId: string): Promise<void> {
    const started = startGame(gameId).then(() => {
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
   * Asks the game to quit; a second call, or `force`, ends it at once. A game
   * still starting is stopped once it has started. The end arrives as
   * game_finished.
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
    await quiet(stopGame(), "stop_game");
    if (force) await quiet(stopGame(), "stop_game");
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

  #finished(event: GameFinished): void {
    const id = event.gameId;
    const here = nav.route.name === "game" || nav.route.name === "launch";
    const showing = here && "gameId" in nav.route && nav.route.gameId === id;
    const stopped = this.stopping[id] === true;
    delete this.stopping[id];
    if (this.running === id) this.running = null;
    pauser.reset();
    this.summaries[id] = event.summary;
    if ("error" in event && !stopped) {
      this.errors[id] = event.error;
      if (showing) nav.replace({ name: "launch", gameId: id });
    } else {
      // Back to the game's detail in the library; a stop the user asked for
      // (even a forced one, which reports an error) is a normal end.
      library.focus = id;
      if (showing) nav.home();
    }
  }
}

export const runs = new Runs();

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
  #gameId: () => string;
  #alive = true;

  constructor(gameId: () => string) {
    this.#gameId = gameId;
    // A pending error (the game failed while running) is shown by the effect below.
    if (!(gameId() in runs.errors)) this.#start();
    $effect(() => {
      const id = this.#gameId();
      if (runs.errors[id] === undefined) return;
      const error = untrack(() => runs.takeError(id));
      if (error !== null) this.#fail("failed", error);
    });
    $effect(() => () => {
      this.#alive = false;
    });
  }

  #start(): void {
    const id = this.#gameId();
    this.view = "prepare";
    this.reason = "";
    const game = library.get(id);
    if (game?.offline) {
      this.#fail("unsupported", game.offline === "disk" ? t("所在磁盘未连接") : t("找不到文件夹"));
      return;
    }
    // Only builds with a native game view register start_game (macOS for now).
    if (!inTauri) {
      this.#fail("unsupported", UNSUPPORTED_PLATFORM);
      return;
    }
    runs.start(id).then(
      () => {
        // The game may already have ended or been cancelled, which leaves
        // this screen or fails it.
        if (this.#alive && this.view === "prepare" && !runs.stopping[id]) nav.replace({ name: "game", gameId: id });
      },
      (error: unknown) => {
        const message = String(error);
        // Builds without the command answer "Command start_game not found".
        if (/start_game.*not found/i.test(message)) this.#fail("unsupported", UNSUPPORTED_PLATFORM);
        else this.#fail("failed", message);
      },
    );
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
    this.#start();
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
