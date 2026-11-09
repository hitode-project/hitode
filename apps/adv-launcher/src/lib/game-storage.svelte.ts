import { call, preview } from "./bridge";
import { PREVIEW_IMPORTS } from "./fake";
import { library } from "./library.svelte";

export interface GameCopy {
  id: string;
  directory: string;
  name: string;
  bytes: number;
}

export interface GameLocation {
  kind: "copy" | "documents" | "external";
  displayPath: string;
  canReveal: boolean;
  copy: GameCopy | null;
}

export interface ExportedCopy {
  label: string;
  path: string | null;
}

const GB = 1_073_741_824;

class GameStorage {
  folders = $state<GameCopy[]>(preview ? [
    { id: "star-island", directory: `${PREVIEW_IMPORTS}/star-island`, name: "Star Island", bytes: 3.4 * GB },
    { id: "rainy-bookshop", directory: `${PREVIEW_IMPORTS}/rainy-bookshop`, name: "Rainy Bookshop", bytes: 2.1 * GB },
    { id: "old-import", directory: `${PREVIEW_IMPORTS}/old-import`, name: "Old Import", bytes: 1.7 * GB },
  ] : []);

  measured = $state(preview);
  #refreshing: Promise<void> | null = null;

  get total() { return this.folders.reduce((sum, folder) => sum + folder.bytes, 0); }

  async refresh() {
    if (preview) return;
    this.#refreshing ??= call<GameCopy[]>("list_game_copies", {}, []).then((copies) => {
      this.folders = copies;
      this.measured = true;
    }).finally(() => { this.#refreshing = null; });
    return this.#refreshing;
  }

  async location(id: string): Promise<GameLocation | null> {
    if (!preview) return call<GameLocation | null>("game_file_location", { gameId: id }, null);
    const game = library.get(id);
    if (!game) return null;
    const copy = this.folders.find((folder) => this.contains(folder, game.path)) ?? null;
    return { kind: game.path.startsWith(PREVIEW_IMPORTS + "/") ? "copy" : "external", displayPath: game.path, canReveal: !game.offline, copy };
  }

  contains(folder: GameCopy, path: string) {
    return path === folder.directory || path.startsWith(folder.directory + "/");
  }

  linked(folder: GameCopy) {
    return library.games.filter((game) => !game.offline && this.contains(folder, game.path));
  }

  async reveal(gameId: string | null, folder?: GameCopy) {
    await call("reveal_game_files", { gameId, copyId: gameId ? null : folder?.id ?? null }, undefined);
  }

  async delete(folder: GameCopy) {
    if (preview) {
      for (const game of this.linked(folder)) game.offline = "missing";
      this.folders = this.folders.filter((item) => item.id !== folder.id);
    } else {
      await call("delete_game_copy", { id: folder.id }, undefined);
      this.folders = this.folders.filter((item) => item.id !== folder.id);
      await library.refresh();
    }
  }

  async export(folder: GameCopy): Promise<ExportedCopy | null> {
    return call<ExportedCopy | null>("export_game_copy", { id: folder.id }, null);
  }

  async moveLinks(folder: GameCopy, target: string) {
    if (preview) {
      for (const game of this.linked(folder)) game.path = target + game.path.slice(folder.directory.length);
    } else {
      await call("use_game_copy", { id: folder.id, destination: target }, undefined);
      await library.refresh();
    }
  }
}

export const gameStorage = new GameStorage();
