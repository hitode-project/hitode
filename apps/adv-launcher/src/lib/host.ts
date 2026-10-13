import { call } from "./bridge";

export type Codepage = 932 | 936;
export interface PlaySession { ended: number; seconds: number }
export interface LibraryGame {
  id: string; title: string; folder: string; exe: string; codepage: Codepage;
  addedAt: number; lastPlayed: number | null; playSeconds: number;
  favorite: boolean; nsfw: boolean; sessions: PlaySession[];
  offline: "disk" | "missing" | null;
}
export interface ExeCandidate {
  path: string; size: number; arch: "x86" | "x64" | "other"; codepage: Codepage;
}
export interface FolderInspection {
  folder: string; title: string; exes: ExeCandidate[];
  suggested: number | null; inLibrary: string | null;
}
export interface ScanFound {
  folder: string; relative: string; depth: number; title: string;
  exe: ExeCandidate | null; setup: boolean; inLibrary: string | null;
}
export interface GamePatch {
  title?: string; codepage?: Codepage; favorite?: boolean; nsfw?: boolean;
  folder?: string; exe?: string;
}
export interface RunSummary {
  failure: {
    kind: "api" | "cpu" | "runtime";
    api: string | null;
    cpu_fault: string | null;
    after: "lt_1m" | "1m_10m" | "10m_1h" | "gt_1h";
  } | null;
  perf: {
    frame_ms_p50: number;
    frame_ms_p95: number;
    frame_ms_p99: number;
  } | null;
}
export type GameFinished =
  | { gameId: string; exitCode: number; summary: RunSummary }
  | { gameId: string; error: string };

export function libraryPickFolder(): Promise<string | null> {
  return call("library_pick_folder", {}, null);
}
export function libraryList(): Promise<LibraryGame[]> {
  return call("library_list", {}, []);
}
export function libraryInspect(folder: string): Promise<FolderInspection | null> {
  return call("library_inspect", { folder }, null);
}
export function libraryScan(root: string, depth: number): Promise<ScanFound[]> {
  return call("library_scan", { root, depth }, []);
}
export function libraryAdd(folder: string, exe: string, codepage: Codepage, title: string): Promise<LibraryGame | null> {
  return call("library_add", { folder, exe, codepage, title }, null);
}
export function libraryUpdate(id: string, patch: GamePatch): Promise<LibraryGame | null> {
  return call("library_update", { id, patch }, null);
}
export function libraryRemove(id: string): Promise<LibraryGame | null> {
  return call("library_remove", { id }, null);
}
export function libraryRestore(game: LibraryGame, index: number): Promise<void> {
  return call("library_restore", { game, index }, undefined);
}
export function startGame(id: string): Promise<void> {
  return call("start_game", { id }, undefined);
}
