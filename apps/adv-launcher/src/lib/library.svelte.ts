import { fakeLibrary, FAKE_NOW } from "./fake";
import type { Game, LibraryFilter } from "./types";

/** The game library as the launcher shows it; fake data for now. */
class Library {
  games = $state<Game[]>(fakeLibrary());
  /** Adult covers the user revealed this session; they blur again on restart. */
  revealed = $state<Record<string, boolean>>({});
  readonly now = FAKE_NOW;

  get(id: string): Game | undefined {
    return this.games.find((g) => g.id === id);
  }

  filtered(filter: LibraryFilter, query: string): Game[] {
    const q = query.trim().toLowerCase();
    const list = this.games.filter((g) => {
      if (filter === "recent" && g.lastPlayed === null) return false;
      if (filter === "favorite" && !g.favorite) return false;
      if (filter === "unstarted" && g.lastPlayed !== null) return false;
      return !q || g.title.toLowerCase().includes(q) || g.brand.toLowerCase().includes(q);
    });
    // Most recently played first, then never-started games in library order.
    return list.sort((a, b) => (b.lastPlayed ?? -1) - (a.lastPlayed ?? -1));
  }

  count(filter: LibraryFilter): number {
    return this.filtered(filter, "").length;
  }

  remove(id: string): void {
    this.games = this.games.filter((g) => g.id !== id);
  }

  /** Pretends the user pointed a missing game at its new folder. */
  relocate(id: string): void {
    const game = this.get(id);
    if (game) game.offline = null;
  }

  toggleFavorite(id: string): void {
    const game = this.get(id);
    if (game) game.favorite = !game.favorite;
  }
}

export const library = new Library();
