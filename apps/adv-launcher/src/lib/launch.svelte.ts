import { nav } from "./nav.svelte";

/** The states of the launch screen; the game screen routes a crash here as "failed". */
export type LaunchState = "prepare" | "failed" | "unsupported";

// Fake outcomes until the Rust side reports them: g4 stops after preparing,
// g5 is a 64-bit program; every other game starts.
const FAILS = "g4";
const UNSUPPORTED = "g5";
export const UNSUPPORTED_REASON = "64 位程序";

/** What launching `gameId` leads to before any preparation runs. */
export function launchStart(gameId: string): LaunchState {
  return gameId === UNSUPPORTED ? "unsupported" : "prepare";
}

/**
 * The launch flow shared by the desktop and phone launch screens: fake
 * first-run preparation with steady progress, then the game screen or the
 * failure. Create it while a component initialises; the timer stops with it.
 */
export class LaunchFlow {
  view = $state<LaunchState>("prepare");
  pct = $state(0);
  /** Bumped by 再试一次 to restart the preparation. */
  #run = $state(0);
  #gameId: () => string;

  constructor(gameId: () => string, initial?: LaunchState) {
    this.#gameId = gameId;
    this.view = initial ?? launchStart(gameId());

    $effect(() => {
      if (this.view !== "prepare") return;
      void this.#run;
      const id = this.#gameId();
      let p = 0;
      let done = false;
      this.pct = 0;
      const finish = () => {
        if (done) return;
        if (id === FAILS) this.view = "failed";
        else nav.replace({ name: "game", gameId: id });
      };
      const timer = setInterval(() => {
        p = Math.min(100, p + 2 + ((p * 7) % 5));
        this.pct = p;
        if (p < 100) return;
        clearInterval(timer);
        setTimeout(finish, 400);
      }, 90);
      return () => {
        done = true;
        clearInterval(timer);
      };
    });
  }

  retry(): void {
    this.view = "prepare";
    this.#run += 1;
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
