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

/**
 * Screen navigation with a back stack; no router library. The location hash
 * mirrors the current screen so any screen can be opened directly while
 * developing; the back stack lives here, not in browser history.
 *
 * The system back gesture (Android back, a browser's back button) arrives as
 * `popstate`. While there is somewhere to go back to, or a screen registered
 * with `onBack`, one guard entry sits on top of the browser history so the
 * gesture is caught here instead of leaving the page.
 */
class Navigator {
  route = $state<Route>(typeof location === "undefined" ? HOME : parseHash(location.hash));
  #history: Route[] = [];
  #handlers: (() => boolean)[] = [];
  #guarded = false;
  #ignorePop = false;

  constructor() {
    $effect.root(() => {
      $effect(() => this.#writeHash(routeHash(this.route)));
    });
    if (typeof window !== "undefined") window.addEventListener("popstate", () => this.#popped());
  }

  go(route: Route): void {
    this.#history.push(this.route);
    this.route = route;
    this.#sync();
  }

  /** Replaces the current screen without adding a back entry (e.g. launch → game). */
  replace(route: Route): void {
    this.route = route;
  }

  back(): void {
    this.route = this.#history.pop() ?? HOME;
    this.#sync();
  }

  /** Returns to the library and drops the back stack. */
  home(filter: LibraryFilter = "all"): void {
    this.#history = [];
    this.route = { name: "library", filter };
    this.#sync();
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
      this.#writeHash(routeHash(this.route));
      return;
    }
    // The guard entry was consumed by the gesture.
    this.#guarded = false;
    const handler = this.#handlers.at(-1);
    if (!handler?.() && this.#history.length > 0) this.route = this.#history.pop() ?? HOME;
    this.#sync();
    this.#writeHash(routeHash(this.route));
  }
}

export const nav = new Navigator();
