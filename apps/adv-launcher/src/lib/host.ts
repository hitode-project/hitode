import { playback } from "./playback";
import { gameFilter } from "./upscaling.svelte";
import { call } from "./bridge";

export type Codepage = 932 | 936;
export interface PlaySession { ended: number; seconds: number }
export interface LastRun { ended: number; summary: RunSummary | null; error: string | null }
export interface Novel {
  id: string; title: string; alttitle: string | null; aliases: string[]; released: string | null;
  description: string | null; developers: { name: string }[];
  image: { thumbnail: string; sexual: number } | null; cover?: string | null;
}
export interface LibraryGame {
  vndb: Novel | null;
  id: string; title: string; folder: string; exe: string; codepage: Codepage;
  addedAt: number; lastPlayed: number | null; playSeconds: number;
  favorite: boolean; nsfw: boolean; sessions: PlaySession[];
  offline: "disk" | "missing" | null;
  daily: Record<string, number>; lastRun: LastRun | null;
}
export interface ExeCandidate {
  path: string; size: number; arch: "x86" | "x64" | "other"; codepage: Codepage;
  inLibrary: string | null;
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
  | { gameId: string; error: string; summary: RunSummary };

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
  return call("start_game", { id, filter: gameFilter(id), playback: playback(id) }, undefined);
}
/** Pause or resume the running game, e.g. while the pause menu or report page is open. */
export function pauseGame(paused: boolean): Promise<void> {
  return call("pause_game", { paused }, undefined);
}
export function stopGame(): Promise<void> {
  return call("stop_game", {}, undefined);
}
export function revealPath(path: string): Promise<void> {
  return call("reveal_path", { path }, undefined);
}
