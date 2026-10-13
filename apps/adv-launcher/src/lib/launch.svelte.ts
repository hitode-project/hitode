import { untrack } from "svelte";
import { inTauri, onHostEvent } from "./bridge";
import { startGame, type GameFinished, type RunSummary } from "./host";
import { library } from "./library.svelte";
import { nav } from "./nav.svelte";

/** The states of the launch screen; a game that fails to run comes back here as "failed". */
export type LaunchState = "prepare" | "failed" | "unsupported";

const UNSUPPORTED_PLATFORM = "此平台暂不支持启动";

/**
 * What the last run of each game left behind, for this session: the run
 * summary of a game that exited (the report's failure and frame timing), or
 * the error of one that could not run (shown once by the launch screen).
 */
class Runs {
  summaries = $state<Record<string, RunSummary | null>>({});
  /** Launch errors not yet shown, by game id. */
  errors = $state<Record<string, string>>({});

  constructor() {
    onHostEvent<GameFinished>("game_finished", (event) => this.#finished(event));
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
    if ("error" in event) {
      this.summaries[id] = null;
      this.errors[id] = event.error;
      if (showing) nav.replace({ name: "launch", gameId: id });
    } else {
      this.summaries[id] = event.summary;
      // Back to the game's detail in the library.
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
      this.#fail("unsupported", game.offline === "disk" ? "所在磁盘未连接" : "找不到文件夹");
      return;
    }
    // Only builds with a native game view register start_game (macOS for now).
    if (!inTauri) {
      this.#fail("unsupported", UNSUPPORTED_PLATFORM);
      return;
    }
    startGame(id).then(
      () => {
        // The game may already have ended, which leaves this screen or fails it.
        if (this.#alive && this.view === "prepare") nav.replace({ name: "game", gameId: id });
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
