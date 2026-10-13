import type { Game } from "./types";

// Fake save data and host actions for the manage screen until the Rust side
// provides them.

export interface Backup {
  id: string;
  /** Display time, e.g. "今天 21:14". */
  when: string;
  kind: "自动" | "手动";
  size: string;
}

export interface SaveInfo {
  path: string;
  backups: Backup[];
  autoBackup: boolean;
  keep: number;
}

export const KEEP_CHOICES = [3, 5, 10] as const;

export function fakeSaveInfo(game: Game): SaveInfo {
  const backups: Backup[] = [
    { id: "a", when: "今天 21:14", kind: "自动", size: "1.2 MB" },
    { id: "b", when: "昨天 23:02", kind: "自动", size: "1.2 MB" },
    { id: "c", when: "10 月 3 日", kind: "手动", size: "1.1 MB" },
  ];
  return { path: `[启动器数据]/saves/${game.title}`, backups, autoBackup: true, keep: 5 };
}

/** Pretends to make a backup of the current saves. */
export function backupNow(index: number): Backup {
  return { id: `n${index}`, when: "刚刚", kind: "手动", size: "1.2 MB" };
}

/** Pretends to restore a backup over the current saves. */
export function restoreBackup(game: Game, backup: Backup): void {
  void game;
  void backup;
}

/** Pretends to reveal a folder in the system file manager. */
export function revealFolder(path: string): void {
  void path;
}

/** Pretends to run the host's import / export save dialogs. */
export function importSaves(game: Game): void {
  void game;
}

export function exportSaves(game: Game): void {
  void game;
}
