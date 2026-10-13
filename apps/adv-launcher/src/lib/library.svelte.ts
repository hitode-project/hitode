import { inTauri, onHostEvent } from "./bridge";
import { fakeLibrary } from "./fake";
import { libraryInspect, libraryList, libraryPickFolder, libraryRemove, libraryRestore, libraryUpdate, type LibraryGame } from "./host";
import { minutesByDay } from "./stats";
import type { Game, LibraryFilter } from "./types";

const COVERS = 12;

/** A stable placeholder cover (cv1…cv12) until real cover art is wired. */
export function placeholderCover(id: string): string {
  let h = 2166136261;
  for (const ch of id) h = Math.imul(h ^ (ch.codePointAt(0) ?? 0), 16777619);
  return `cv${((h >>> 0) % COVERS) + 1}`;
}

/** The page model of a library entry; brand, year and engine have no source yet. */
export function toGame(g: LibraryGame, now: number): Game {
  return {
    id: g.id,
    title: g.title,
    brand: "",
    year: 0,
    cover: placeholderCover(g.id),
    engine: "",
    codepage: g.codepage,
    path: g.folder,
    exe: g.exe,
    lastPlayed: g.lastPlayed,
    playMinutes: Math.round(g.playSeconds / 60),
    favorite: g.favorite,
    nsfw: g.nsfw,
    offline: g.offline,
    daily: minutesByDay(g.daily, now, 0, 30),
    history: g.daily,
    lastSummary: g.lastRun?.summary ?? null,
    sessions30: g.sessions.length,
  };
}

/**
 * The game library as the launcher shows it. In Tauri it mirrors the Rust
 * library (library_* commands) and reloads after every game ends; in a plain
 * browser (`pnpm dev`) it shows fake data and changes stay in memory.
 */
class Library {
  games = $state<Game[]>(inTauri ? [] : fakeLibrary());
  /** Adult covers the user revealed this session; they blur again on restart. */
  revealed = $state<Record<string, boolean>>({});
  /** The time the play statistics are measured against; refreshed with the list. */
  now = $state(Date.now());
  /** A game the library screen should select when it opens next (after a game ends). */
  focus = $state<string | null>(null);

  /** Removed entries as the host returned them, for undo. */
  #removed: { entry: LibraryGame | null; game: Game; index: number } | null = null;

  constructor() {
    if (!inTauri) return;
    void this.refresh();
    onHostEvent("game_finished", () => void this.refresh());
  }

  async refresh(): Promise<void> {
    if (!inTauri) return;
    try {
      const list = await libraryList();
      this.now = Date.now();
      this.games = list.map((g) => toGame(g, this.now));
    } catch (error) {
      console.error("library_list failed", error);
    }
  }

  /** The game to select when the library screen opens; reading it clears it. */
  takeFocus(): string | null {
    const id = this.focus;
    this.focus = null;
    return id;
  }

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

  /** Puts an entry the host returned in place of the one with its id (or appends it). */
  put(entry: LibraryGame): Game {
    const game = toGame(entry, this.now);
    const index = this.games.findIndex((g) => g.id === entry.id);
    if (index < 0) this.games = [...this.games, game];
    else this.games[index] = game;
    return game;
  }

  /** Removes a game from the library (its files stay); the last removal can be undone. */
  async remove(id: string): Promise<void> {
    const index = this.games.findIndex((g) => g.id === id);
    const game = this.games[index];
    if (!game) return;
    let entry: LibraryGame | null = null;
    if (inTauri) {
      try {
        entry = await libraryRemove(id);
      } catch (error) {
        console.error("library_remove failed", error);
        return;
      }
    }
    this.#removed = { entry, game, index };
    this.games = this.games.filter((g) => g.id !== id);
  }

  /** Puts the last removed game back where it was. */
  async undoRemove(): Promise<void> {
    const removed = this.#removed;
    if (!removed) return;
    this.#removed = null;
    if (removed.entry) {
      try {
        await libraryRestore(removed.entry, removed.index);
      } catch (error) {
        console.error("library_restore failed", error);
        return;
      }
    }
    const games = [...this.games];
    games.splice(Math.min(removed.index, games.length), 0, removed.game);
    this.games = games;
  }

  /** 找回: points a missing game at the folder the user picks, keeping its main program if it is there. */
  async relocate(id: string): Promise<void> {
    const game = this.get(id);
    if (!game) return;
    if (!inTauri) {
      game.offline = null;
      return;
    }
    try {
      const folder = await libraryPickFolder();
      if (!folder) return;
      const inspection = await libraryInspect(folder);
      if (!inspection) return;
      const exes = inspection.exes.map((e) => e.path);
      // The same main program by name when the new folder has it, else the suggestion.
      const same = exes.find((e) => e.toLowerCase() === game.exe.toLowerCase());
      const suggested = inspection.suggested === null ? undefined : exes[inspection.suggested];
      const exe = same ?? suggested;
      if (!exe) return;
      const updated = await libraryUpdate(id, { folder: inspection.folder, exe });
      if (updated) this.put(updated);
      await this.refresh();
    } catch (error) {
      console.error("relocating the game failed", error);
    }
  }

  async toggleFavorite(id: string): Promise<void> {
    const game = this.get(id);
    if (game) await this.#update(game, { favorite: !game.favorite }, () => (game.favorite = !game.favorite));
  }

  /** 隐藏封面: blurs the cover in the library until revealed. */
  async toggleNsfw(id: string): Promise<void> {
    const game = this.get(id);
    if (game) await this.#update(game, { nsfw: !game.nsfw }, () => (game.nsfw = !game.nsfw));
  }

  async #update(game: Game, patch: { favorite?: boolean; nsfw?: boolean }, fake: () => void): Promise<void> {
    if (!inTauri) {
      fake();
      return;
    }
    try {
      const updated = await libraryUpdate(game.id, patch);
      if (updated) {
        // The list carries offline state; an update reply does not.
        this.put({ ...updated, offline: game.offline });
      }
    } catch (error) {
      console.error("library_update failed", error);
    }
  }
}

export const library = new Library();
