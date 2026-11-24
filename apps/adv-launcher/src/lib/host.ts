import { playback } from "./playback";
import { gameFilter } from "./upscaling.svelte";
import { call } from "./bridge";
import { pickStorageFolder, takePreparedFolder, type FolderPickOptions } from "./storage";

export type Codepage = 932 | 936 | 950;
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
  /** Until its setup has run: the setup on the game's first disc. */
  setup?: string;
  /** The folder its setup installed into, behind `C:\Program Files`. */
  programFiles?: string;
}
export interface DiscInspection {
  label: string; bytes: number; programs: string[]; codepage: Codepage;
}
export interface ExeCandidate {
  path: string; size: number; arch: "x86" | "x64" | "other"; codepage: Codepage;
  inLibrary: string | null;
}
export interface FolderInspection {
  folder: string; title: string; exes: ExeCandidate[];
  suggested: number | null; inLibrary: string | null;
  registryFiles?: string[];
}
export interface RegistryImportResult {
  entries: number; normalizedWow64: number;
}
export interface ScanFound {
  folder: string; relative: string; depth: number; title: string;
  exe: ExeCandidate | null; exes: ExeCandidate[]; setup: boolean; inLibrary: string | null;
}
export interface GamePatch {
  title?: string; codepage?: Codepage; favorite?: boolean; nsfw?: boolean;
  folder?: string; exe?: string;
}
export interface RunSummary {
  graphics?: import("./report").ReportGraphics | null;
  settings?: import("./report").ReportSettings | null;
  timeline?: import("./report").ReportTimeline | null;
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
  /** Missing in summaries saved before API gaps were recorded. */
  api_gaps?: { called: string[]; imported: string[] } | null;
}
export type GameFinished =
  | { gameId: string; exitCode: number; summary: RunSummary; registry?: RegistryImportResult }
  | { gameId: string; error: string; summary: RunSummary };

export async function libraryPickFolder(options?: FolderPickOptions): Promise<string | null> {
  const prepared = takePreparedFolder();
  if (prepared) return prepared;
  const folder = await pickStorageFolder(options);
  if (folder !== undefined) return folder;
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
export function libraryInspectDisc(path: string): Promise<DiscInspection | null> {
  return call("library_inspect_disc", { path }, null);
}
export function libraryAddInstall(target: string | null, setup: string, codepage: Codepage, title: string): Promise<LibraryGame | null> {
  return call("library_add_install", { target, setup, codepage, title }, null);
}
/** Folders setups installed programs into, outside every game's folder. */
export function libraryInstalled(id: string): Promise<{ name: string; folder: string }[]> {
  return call("library_installed", { id }, []);
}
export function libraryUpdate(id: string, patch: GamePatch): Promise<LibraryGame | null> {
  return call("library_update", { id, patch }, null);
}
export function libraryRemove(id: string): Promise<LibraryGame | null> {
  return call("library_remove", { id }, null);
}
export function libraryReorder(ids: string[]): Promise<void> {
  return call("library_reorder", { ids }, undefined);
}
/** A light haptic tick for a step under the finger. */
export function hapticTick(): void {
  void call("haptic_tick", {}, undefined);
}
export function libraryRestore(game: LibraryGame, index: number): Promise<void> {
  return call("library_restore", { game, index }, undefined);
}
/** Sends a line to the running script, as Enter does. */
export function scriptInput(line: string): Promise<void> {
  return call("script_input", { line }, undefined);
}
export function startGame(id: string, exe?: string): Promise<void> {
  return call("start_game", { id, filter: gameFilter(id), playback: playback(id, exe) }, undefined);
}
/** Pause or resume the running game, e.g. while the pause menu or report page is open. */
export function pauseGame(paused: boolean): Promise<void> {
  return call("pause_game", { paused }, undefined);
}
/** Asks the running game to close, or with `force` ends it at once. */
export function stopGame(force = false): Promise<void> {
  return call("stop_game", { force }, undefined);
}
/** Opens drive C: of the Wine prefix, where games' other saved data is. */
export function revealDrive(): Promise<void> {
  return call("reveal_drive", {}, undefined);
}
/** Imports a `.reg` file or executes a registry restore `.bat`/`.cmd` script for a game. */
export function importRegistry(id: string, path: string, discs: string[]): Promise<RegistryImportResult | null> {
  return call("import_registry", { id, path, discs }, null);
}
export function revealPath(path: string): Promise<void> {
  return call("reveal_path", { path }, undefined);
}
