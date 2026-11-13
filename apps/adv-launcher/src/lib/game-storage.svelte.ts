import { call, preview } from "./bridge";
import { PREVIEW_IMPORTS } from "./fake";
import { t } from "./i18n.svelte";
import { library } from "./library.svelte";
import { storageSize } from "./storage";

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

export interface RemovableCopy {
  copy: GameCopy;
  sharedWith: number;
}

/** Confirmation subtitle and action label describing whether an internal Hitode copy exists. */
export function removeConfirmText(info: RemovableCopy | null, deleteCopy: boolean) {
  const canDeleteCopy = !!info && info.sharedWith === 0;
  const targetCopy = canDeleteCopy && deleteCopy ? info.copy : null;
  const subtitle = !info
    ? t("游戏文件不会被删除")
    : info.sharedWith > 0
      ? t("游戏文件不会被删除。内部副本（{0}）正与库中其他 {1} 个游戏共用。", storageSize(info.copy.bytes), info.sharedWith)
      : t("Hitode 内部副本占用 {0}，仅移除不会删除它。", storageSize(info.copy.bytes));
  const action = targetCopy
    ? t("移除并删除副本（{0}）", storageSize(targetCopy.bytes))
    : t("移除");
  return { canDeleteCopy, targetCopy, subtitle, action };
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

  /** Returns this game's Hitode internal copy when present, and how many other library games share it. */
  async removableCopy(gameId: string): Promise<RemovableCopy | null> {
    const location = await this.location(gameId).catch(() => null);
    if (!location || location.kind !== "copy" || !location.copy) return null;
    const sharedWith = this.linked(location.copy).filter((item) => item.id !== gameId).length;
    return { copy: location.copy, sharedWith };
  }

  /** Removes a library entry and, when requested, deletes its exclusive internal copy first. */
  async removeGame(gameId: string, copy?: GameCopy | null) {
    if (copy) await this.delete(copy);
    await library.remove(gameId);
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
