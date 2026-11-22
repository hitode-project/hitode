import { call, preview } from "./bridge";
import { PREVIEW_IMPORTS } from "./fake";
import { DATA_DIR, removeFileFromSetups, setupUsesFile } from "./fake-settings";
import { t } from "./i18n.svelte";
import { library } from "./library.svelte";
import { storageSize } from "./storage";

export interface GameCopy {
  id: string;
  directory: string;
  name: string;
  bytes: number;
}

export interface ImportedFile {
  id: string;
  name: string;
  kind: "disc" | "font" | "other";
  path: string;
  bytes: number;
}

export interface StorageSummary {
  dataDir: string;
  gameFilesBytes: number;
  envBytes: number;
  cacheBytes: number;
}

export interface GameLocation {
  kind: "copy" | "documents" | "external";
  displayPath: string;
  canReveal: boolean;
  copy: GameCopy | null;
  cacheBytes: number;
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

const MB = 1_048_576;
const GB = 1_073_741_824;

class GameStorage {
  folders = $state<GameCopy[]>(preview ? [
    { id: "star-island", directory: `${PREVIEW_IMPORTS}/star-island`, name: "Star Island", bytes: 3.4 * GB },
    { id: "rainy-bookshop", directory: `${PREVIEW_IMPORTS}/rainy-bookshop`, name: "Rainy Bookshop", bytes: 2.1 * GB },
    { id: "old-import", directory: `${PREVIEW_IMPORTS}/old-import`, name: "Old Import", bytes: 1.7 * GB },
  ] : []);

  files = $state<ImportedFile[]>(preview ? [
    { id: "file-1", name: "Bonus_Disc2.iso", kind: "disc", path: `${DATA_DIR}/files/file-1/Bonus_Disc2.iso`, bytes: 680 * MB },
  ] : []);

  summary = $state<StorageSummary | null>(preview ? {
    dataDir: DATA_DIR,
    gameFilesBytes: (3.4 + 2.1 + 1.7) * GB + 680 * MB,
    envBytes: 248 * MB,
    cacheBytes: 148 * MB,
  } : null);

  measured = $state(preview);
  #refreshing: Promise<void> | null = null;
  #previewGameCache = new Map<string, number>();

  get total() {
    return this.folders.reduce((sum, folder) => sum + folder.bytes, 0)
      + this.files.reduce((sum, file) => sum + file.bytes, 0);
  }

  get orphanFolders() {
    return this.folders.filter((folder) => this.linked(folder).length === 0);
  }

  get orphanFiles() {
    return this.files.filter((file) => this.linkedFile(file).length === 0);
  }

  get orphanBytes() {
    return this.orphanFolders.reduce((sum, folder) => sum + folder.bytes, 0)
      + this.orphanFiles.reduce((sum, file) => sum + file.bytes, 0);
  }

  #syncPreviewSummary() {
    if (!preview || !this.summary) return;
    this.summary = {
      ...this.summary,
      gameFilesBytes: this.total,
    };
  }

  async refreshSummary() {
    if (preview) return;
    const summary = await call<StorageSummary | null>("storage_summary", {}, null);
    if (summary) this.summary = summary;
  }

  async refresh() {
    if (preview) return;
    this.#refreshing ??= Promise.all([
      call<GameCopy[]>("list_game_copies", {}, []),
      call<ImportedFile[]>("list_imported_files", {}, []),
      call<StorageSummary | null>("storage_summary", {}, null),
    ]).then(([copies, files, summary]) => {
      this.folders = copies;
      this.files = files;
      if (summary) this.summary = summary;
      this.measured = true;
    }).finally(() => { this.#refreshing = null; });
    return this.#refreshing;
  }

  async location(id: string): Promise<GameLocation | null> {
    if (!preview) return call<GameLocation | null>("game_file_location", { gameId: id }, null);
    const game = library.get(id);
    if (!game) return null;
    const copy = this.folders.find((folder) => this.contains(folder, game.path)) ?? null;
    const cacheBytes = this.#previewGameCache.get(id) ?? 42 * MB;
    return {
      kind: game.path.startsWith(PREVIEW_IMPORTS + "/") ? "copy" : "external",
      displayPath: game.path,
      canReveal: !game.offline,
      copy,
      cacheBytes,
    };
  }

  contains(folder: GameCopy, path: string) {
    return path === folder.directory || path.startsWith(folder.directory + "/");
  }

  linked(folder: GameCopy) {
    return library.games.filter((game) => !game.offline && this.contains(folder, game.path));
  }

  linkedFile(file: ImportedFile) {
    return library.games.filter((game) => setupUsesFile(game.id, file.path));
  }

  filesForGame(gameId: string) {
    return this.files.filter((file) => setupUsesFile(gameId, file.path));
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

  async clearCache() {
    if (preview) {
      await new Promise((resolve) => setTimeout(resolve, 300));
      if (this.summary) this.summary = { ...this.summary, cacheBytes: 0 };
      this.#previewGameCache.clear();
      return;
    }
    await call("clear_game_cache", {}, null);
    await this.refreshSummary();
  }

  async clearGameCache(gameId: string) {
    if (preview) {
      const prev = this.#previewGameCache.get(gameId) ?? 42 * MB;
      this.#previewGameCache.set(gameId, 0);
      if (this.summary) {
        this.summary = { ...this.summary, cacheBytes: Math.max(0, this.summary.cacheBytes - prev) };
      }
      return;
    }
    await call("clear_single_game_cache", { gameId }, null);
    await this.refreshSummary();
  }

  async delete(folder: GameCopy) {
    if (preview) {
      for (const game of this.linked(folder)) game.offline = "missing";
      this.folders = this.folders.filter((item) => item.id !== folder.id);
      this.#syncPreviewSummary();
    } else {
      await call("delete_game_copy", { id: folder.id }, undefined);
      this.folders = this.folders.filter((item) => item.id !== folder.id);
      await Promise.all([library.refresh(), this.refreshSummary()]);
    }
  }

  async deleteFile(file: ImportedFile) {
    if (preview) {
      this.files = this.files.filter((item) => item.id !== file.id);
      removeFileFromSetups(file.path);
      this.#syncPreviewSummary();
    } else {
      await call("delete_imported_file", { id: file.id }, undefined);
      this.files = this.files.filter((item) => item.id !== file.id);
      removeFileFromSetups(file.path);
      await this.refreshSummary();
    }
  }

  async deleteOrphans() {
    const copies = [...this.orphanFolders];
    const files = [...this.orphanFiles];
    for (const copy of copies) await this.delete(copy);
    for (const file of files) await this.deleteFile(file);
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
