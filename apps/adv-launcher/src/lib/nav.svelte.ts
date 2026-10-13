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
const PLAIN = ["stats", "import", "scan", "install", "settings"];
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
 */
class Navigator {
  route = $state<Route>(typeof location === "undefined" ? HOME : parseHash(location.hash));
  #history: Route[] = [];

  constructor() {
    $effect.root(() => {
      $effect(() => {
        const hash = routeHash(this.route);
        if (location.hash !== hash) history.replaceState(null, "", hash || location.pathname);
      });
    });
  }

  go(route: Route): void {
    this.#history.push(this.route);
    this.route = route;
  }

  /** Replaces the current screen without adding a back entry (e.g. launch → game). */
  replace(route: Route): void {
    this.route = route;
  }

  back(): void {
    this.route = this.#history.pop() ?? HOME;
  }

  /** Returns to the library and drops the back stack. */
  home(filter: LibraryFilter = "all"): void {
    this.#history = [];
    this.route = { name: "library", filter };
  }
}

export const nav = new Navigator();
