import { flushSync } from "svelte";
import type { LibraryFilter } from "./types";

/** Every screen of the launcher; the game screen hosts the in-game overlay. */
export type Route =
  | { name: "library"; filter: LibraryFilter }
  | { name: "stats" }
  | { name: "import" }
  | { name: "scan" }
  | { name: "install" }
  | { name: "settings" }
  | { name: "game-settings"; gameId: string }
  | { name: "report"; gameId: string }
  | { name: "launch"; gameId: string }
  | { name: "game"; gameId: string };

const HOME: Route = { name: "library", filter: "all" };
const FILTERS = ["all", "recent", "favorite", "unstarted"];
const PLAIN = ["stats", "import", "scan", "settings"];
const PER_GAME = ["game-settings", "report", "launch", "game"];

/** The screen a location hash names: `#/settings`, `#/library/recent`, `#/launch/g1`. */
export function parseHash(hash: string): Route {
  const [name = "", arg = ""] = hash.replace(/^#\/?/, "").split("/").map(decodeURIComponent);
  if (name === "library" && FILTERS.includes(arg)) return { name, filter: arg as LibraryFilter };
  if (PLAIN.includes(name)) return { name } as Route;
  if (PER_GAME.includes(name) && arg) return { name, gameId: arg } as Route;
  return HOME;
}

export function routeHash(route: Route): string {
  if (route.name === "library") return route.filter === "all" ? "" : `#/library/${route.filter}`;
  return "gameId" in route ? `#/${route.name}/${encodeURIComponent(route.gameId)}` : `#/${route.name}`;
}

/** How a screen change moves: a push, a pop, or a crossfade between peers. */
type Motion = "forward" | "back" | "fade";

/**
 * Screen navigation with a back stack; no router library. The location hash
 * mirrors the current screen so any screen can be opened directly while
 * developing; the back stack lives here, not in browser history.
 *
 * The system back gesture (Android back, a browser's back button) arrives as
 * `popstate`. While there is somewhere to go back to, or a screen registered
 * with `onBack`, one guard entry sits on top of the browser history so the
 * gesture is caught here instead of leaving the page.
 *
 * Screen changes animate through the View Transitions API, styled per
 * platform in app.css. The game screen changes at once: it is transparent
 * over the native game view.
 */
class Navigator {
  route = $state<Route>(typeof location === "undefined" ? HOME : parseHash(location.hash));
  #history: Route[] = [];
  #handlers: (() => boolean)[] = [];
  #guarded = false;
  #ignorePop = false;
  /** The latest screen asked for; `route` follows once a transition captures the old one. */
  #target: Route = this.route;

  constructor() {
    $effect.root(() => {
      $effect(() => this.#writeHash(routeHash(this.route)));
    });
    if (typeof window !== "undefined") window.addEventListener("popstate", () => this.#popped());
  }

  go(route: Route): void {
    this.#history.push(this.#target);
    this.#show(route, route.name === this.#target.name ? "fade" : "forward");
    this.#sync();
  }

  /** Replaces the current screen without adding a back entry (e.g. launch → game). */
  replace(route: Route): void {
    this.#show(route, "fade");
  }

  back(): void {
    this.#show(this.#history.pop() ?? HOME, "back");
    this.#sync();
  }

  /** Returns to the library and drops the back stack. */
  home(filter: LibraryFilter = "all"): void {
    this.#history = [];
    this.#show({ name: "library", filter }, this.#target.name === "library" ? "fade" : "back");
    this.#sync();
  }

  #show(to: Route, motion: Motion): void {
    const from = this.#target;
    this.#target = to;
    if (typeof document === "undefined" || !document.startViewTransition || from.name === "game" || to.name === "game") {
      this.route = to;
      return;
    }
    document.documentElement.dataset.motion = motion;
    // Changes asked for before the old screen is captured land together.
    document.startViewTransition(() => {
      this.route = this.#target;
      flushSync();
    });
  }

  /**
   * Lets the current screen handle the system back gesture first; `handler`
   * returns true when it consumed the gesture. Returns the unregister function.
   */
  onBack(handler: () => boolean): () => void {
    this.#handlers.push(handler);
    this.#sync();
    return () => {
      this.#handlers = this.#handlers.filter((h) => h !== handler);
      this.#sync();
    };
  }

  #writeHash(hash: string): void {
    if (location.hash !== hash) history.replaceState(history.state, "", hash || location.pathname);
  }

  #sync(): void {
    if (typeof history === "undefined") return;
    const wanted = this.#history.length > 0 || this.#handlers.length > 0;
    if (wanted && !this.#guarded) {
      history.pushState({ advBackGuard: true }, "", location.href);
      this.#guarded = true;
    } else if (!wanted && this.#guarded) {
      this.#guarded = false;
      this.#ignorePop = true;
      history.back();
    }
  }

  #popped(): void {
    if (this.#ignorePop) {
      this.#ignorePop = false;
      this.#writeHash(routeHash(this.#target));
      return;
    }
    // The guard entry was consumed by the gesture.
    this.#guarded = false;
    const handler = this.#handlers.at(-1);
    if (!handler?.() && this.#history.length > 0) this.#show(this.#history.pop() ?? HOME, "back");
    this.#sync();
    this.#writeHash(routeHash(this.#target));
  }
}

export const nav = new Navigator();
